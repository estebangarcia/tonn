//! AI session discovery and management for Nexterm.
//!
//! Scans `~/.claude/projects/` for Claude Code session JSONL files,
//! parses them into [`AiSession`] structs, and watches for changes.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tracing::{debug, warn};

// ── Constants ────────────────────────────────────────────────────────────────

const SUMMARY_MAX_CHARS: usize = 100;
const SUMMARY_ELLIPSIS: &str = "…";
const JSONL_EXTENSION: &str = "jsonl";
const CLAUDE_PROJECTS_DIR: &str = ".claude/projects";
const PATH_SEPARATOR: char = '-';

// ── Types ────────────────────────────────────────────────────────────────────

/// Which AI tool produced the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiTool {
    ClaudeCode,
}

/// How recently a session file must have been modified to be considered "active."
const ACTIVE_SESSION_RECENCY_SECS: u64 = 60;

/// A parsed AI coding session.
#[derive(Debug, Clone)]
pub struct AiSession {
    pub id: String,
    pub parent_id: Option<String>,
    pub file_path: PathBuf,
    pub project_dir: PathBuf,
    pub project_name: String,
    pub summary: String,
    pub message_count: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: Option<String>,
    pub tool: AiTool,
}

impl AiSession {
    /// Check if this session's file was recently modified (likely active).
    pub fn is_recently_active(&self) -> bool {
        std::fs::metadata(&self.file_path)
            .and_then(|m| m.modified())
            .map(|modified| {
                modified.elapsed().map_or(false, |elapsed| {
                    elapsed.as_secs() < ACTIVE_SESSION_RECENCY_SECS
                })
            })
            .unwrap_or(false)
    }
}

/// A tree of sessions for a single project.
#[derive(Debug, Clone)]
pub struct SessionTree {
    pub project_name: String,
    pub project_dir: PathBuf,
    pub roots: Vec<SessionNode>,
    /// Most recent `updated_at` across all sessions in this tree.
    pub last_activity: DateTime<Utc>,
}

/// A node in the session tree — a session with its child forks/continuations.
#[derive(Debug, Clone)]
pub struct SessionNode {
    pub session: AiSession,
    pub children: Vec<SessionNode>,
}

/// A flattened entry from a session tree, carrying depth info for rendering.
#[derive(Debug, Clone)]
pub struct FlatSessionEntry {
    pub session: AiSession,
    /// 0 = root, 1 = child, 2 = grandchild, etc.
    pub depth: usize,
    /// For tree-line rendering (`└─` vs `├─`).
    pub is_last_child: bool,
    pub has_children: bool,
}

impl SessionNode {
    /// Flatten this node and its children into a list with depth info.
    pub fn flatten(&self, depth: usize, is_last: bool, out: &mut Vec<FlatSessionEntry>) {
        out.push(FlatSessionEntry {
            session: self.session.clone(),
            depth,
            is_last_child: is_last,
            has_children: !self.children.is_empty(),
        });
        for (i, child) in self.children.iter().enumerate() {
            child.flatten(depth + 1, i == self.children.len() - 1, out);
        }
    }
}

// ── JSONL Parsing ────────────────────────────────────────────────────────────

/// Extract text content from a `message.content` value.
///
/// Handles both forms:
/// - String: `"content": "hello"`
/// - Array:  `"content": [{"type": "text", "text": "hello"}, ...]`
fn extract_text_content(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        return Some(text.to_string());
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Truncate a string to `SUMMARY_MAX_CHARS`, appending ellipsis if truncated.
fn truncate_summary(text: &str) -> String {
    let trimmed = text.trim().replace('\n', " ");
    if trimmed.len() <= SUMMARY_MAX_CHARS {
        trimmed
    } else {
        let boundary = trimmed
            .char_indices()
            .nth(SUMMARY_MAX_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(trimmed.len());
        format!("{}{SUMMARY_ELLIPSIS}", &trimmed[..boundary])
    }
}

/// Decode an encoded directory name back to a path.
///
/// Claude Code encodes `/Users/me/project` as `-Users-me-project`.
/// This is lossy — real hyphens in directory names also become separators.
fn decode_project_path(encoded: &str) -> PathBuf {
    // Replace leading `-` with `/`, then all remaining `-` with `/`
    let decoded = encoded.replacen(PATH_SEPARATOR, "/", 1);
    let decoded = decoded.replace(PATH_SEPARATOR, "/");
    PathBuf::from(decoded)
}

/// Extract the human-readable project name from a project directory path.
///
/// Uses the last component of the path (e.g. `/Users/me/my-project` → `my-project`).
fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Parse a single Claude Code session JSONL file.
///
/// Streams line-by-line with `BufReader` to handle large files efficiently.
fn parse_session_file(path: &Path) -> Option<AiSession> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let session_id = path.file_stem()?.to_str()?.to_string();

    let mut summary: Option<String> = None;
    let mut model: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut parent_id: Option<String> = None;
    let mut first_timestamp: Option<DateTime<Utc>> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;
    let mut message_count: usize = 0;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                debug!(path = %path.display(), "skipping malformed JSONL line");
                continue;
            }
        };

        let msg_type = entry.get("type").and_then(|t| t.as_str());

        // Track timestamps from every line that has one
        if let Some(ts_str) = entry.get("timestamp").and_then(|t| t.as_str()) {
            if let Ok(ts) = ts_str.parse::<DateTime<Utc>>() {
                if first_timestamp.is_none() {
                    first_timestamp = Some(ts);
                }
                last_timestamp = Some(ts);
            }
        }

        // Extract parent session ID from forkedFrom or parentSessionId
        if parent_id.is_none() {
            // Claude Code uses forkedFrom: { sessionId: "...", messageUuid: "..." }
            if let Some(forked) = entry.get("forkedFrom") {
                if let Some(pid) = forked.get("sessionId").and_then(|v| v.as_str()) {
                    parent_id = Some(pid.to_string());
                }
            }
            // Also check for direct parentSessionId (legacy/future format)
            if parent_id.is_none() {
                if let Some(pid) = entry.get("parentSessionId").and_then(|v| v.as_str()) {
                    parent_id = Some(pid.to_string());
                }
            }
        }

        // Extract cwd from the first message that has it
        if cwd.is_none() {
            if let Some(c) = entry.get("cwd").and_then(|v| v.as_str()) {
                cwd = Some(c.to_string());
            }
        }

        match msg_type {
            Some("user") => {
                message_count += 1;
                if summary.is_none() {
                    if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
                        if let Some(text) = extract_text_content(content) {
                            summary = Some(truncate_summary(&text));
                        }
                    }
                }
            }
            Some("assistant") => {
                message_count += 1;
                if model.is_none() {
                    if let Some(m) = entry
                        .get("message")
                        .and_then(|msg| msg.get("model"))
                        .and_then(|v| v.as_str())
                    {
                        model = Some(m.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    // We need at least one timestamp
    let created_at = first_timestamp?;
    let updated_at = last_timestamp.unwrap_or(created_at);

    // Determine project directory
    let project_dir = if let Some(ref c) = cwd {
        PathBuf::from(c)
    } else {
        // Fall back to decoding the parent directory name
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(decode_project_path)
            .unwrap_or_default()
    };

    let project_name = project_name_from_path(&project_dir);

    Some(AiSession {
        id: session_id,
        parent_id,
        file_path: path.to_path_buf(),
        project_dir,
        project_name,
        summary: summary.unwrap_or_default(),
        message_count,
        created_at,
        updated_at,
        model,
        tool: AiTool::ClaudeCode,
    })
}

// ── Tree Building ───────────────────────────────────────────────────────────

/// Build a forest of `SessionNode` from a flat list, linking parents to children.
fn build_tree(sessions: Vec<AiSession>) -> Vec<SessionNode> {
    let known_ids: std::collections::HashSet<&str> =
        sessions.iter().map(|s| s.id.as_str()).collect();

    let mut children_map: HashMap<String, Vec<AiSession>> = HashMap::new();
    let mut roots: Vec<AiSession> = Vec::new();

    for session in &sessions {
        if let Some(ref pid) = session.parent_id {
            if known_ids.contains(pid.as_str()) {
                children_map
                    .entry(pid.clone())
                    .or_default()
                    .push(session.clone());
            } else {
                // Parent not found — treat as root (orphaned child)
                roots.push(session.clone());
            }
        } else {
            roots.push(session.clone());
        }
    }

    fn build_node(
        session: AiSession,
        children_map: &mut HashMap<String, Vec<AiSession>>,
    ) -> SessionNode {
        let mut children: Vec<SessionNode> = children_map
            .remove(&session.id)
            .unwrap_or_default()
            .into_iter()
            .map(|child| build_node(child, children_map))
            .collect();
        children.sort_by(|a, b| b.session.updated_at.cmp(&a.session.updated_at));
        SessionNode { session, children }
    }

    roots.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    roots
        .into_iter()
        .map(|s| build_node(s, &mut children_map))
        .collect()
}

// ── Session Manager ──────────────────────────────────────────────────────────

/// Thread-safe manager for discovered AI sessions.
pub struct SessionManager {
    sessions: DashMap<String, AiSession>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }

    /// Resolve the Claude projects directory (`~/.claude/projects/`).
    fn projects_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|home| home.join(CLAUDE_PROJECTS_DIR))
    }

    /// Scan `~/.claude/projects/` for all session JSONL files.
    pub fn scan(&self) {
        let projects_dir = match Self::projects_dir() {
            Some(d) if d.is_dir() => d,
            _ => {
                warn!("claude projects directory not found");
                return;
            }
        };

        let entries = match fs::read_dir(&projects_dir) {
            Ok(e) => e,
            Err(err) => {
                warn!(%err, "failed to read claude projects directory");
                return;
            }
        };

        for project_entry in entries.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }

            let files = match fs::read_dir(&project_path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            for file_entry in files.flatten() {
                let file_path = file_entry.path();
                if file_path.extension().and_then(|e| e.to_str()) != Some(JSONL_EXTENSION) {
                    continue;
                }

                if let Some(session) = parse_session_file(&file_path) {
                    self.sessions.insert(session.id.clone(), session);
                }
            }
        }

        debug!(count = self.sessions.len(), "session scan complete");
    }

    /// Return all sessions sorted by `updated_at` (most recent first).
    pub fn all_sessions_sorted(&self) -> Vec<AiSession> {
        let mut sessions: Vec<AiSession> =
            self.sessions.iter().map(|entry| entry.value().clone()).collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sessions
    }

    /// Search sessions by query string (case-insensitive).
    ///
    /// Matches against `project_name`, `summary`, and `id`.
    pub fn search(&self, query: &str) -> Vec<AiSession> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<AiSession> = self
            .sessions
            .iter()
            .filter(|entry| {
                let session = entry.value();
                session.project_name.to_lowercase().contains(&query_lower)
                    || session.summary.to_lowercase().contains(&query_lower)
                    || session.id.to_lowercase().contains(&query_lower)
            })
            .map(|entry| entry.value().clone())
            .collect();
        results.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        results
    }

    /// Get a specific session by ID.
    pub fn get(&self, session_id: &str) -> Option<AiSession> {
        self.sessions.get(session_id).map(|entry| entry.value().clone())
    }

    /// Number of tracked sessions.
    pub fn count(&self) -> usize {
        self.sessions.len()
    }

    /// Build session trees grouped by project with parent-child relationships.
    /// Returns trees sorted by most recent activity (most recent first).
    pub fn session_trees(&self) -> Vec<SessionTree> {
        let sessions: Vec<AiSession> =
            self.sessions.iter().map(|entry| entry.value().clone()).collect();

        // Group sessions by project_dir
        let mut by_project: HashMap<PathBuf, Vec<AiSession>> = HashMap::new();
        for session in sessions {
            by_project
                .entry(session.project_dir.clone())
                .or_default()
                .push(session);
        }

        let mut trees: Vec<SessionTree> = by_project
            .into_iter()
            .map(|(project_dir, group)| {
                let project_name = project_name_from_path(&project_dir);
                let last_activity = group
                    .iter()
                    .map(|s| s.updated_at)
                    .max()
                    .unwrap_or_else(Utc::now);
                let roots = build_tree(group);
                SessionTree {
                    project_name,
                    project_dir,
                    roots,
                    last_activity,
                }
            })
            .collect();

        trees.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        trees
    }

    /// Watch `~/.claude/projects/` for JSONL file changes.
    ///
    /// Blocks the calling thread. On `Create` or `Modify` events for `.jsonl`
    /// files, the affected session is re-parsed and the map is updated.
    pub fn start_watcher(self: &Arc<Self>) -> notify::Result<()> {
        let projects_dir = Self::projects_dir().ok_or_else(|| {
            notify::Error::generic("could not resolve claude projects directory")
        })?;

        let manager = Arc::clone(self);

        let mut watcher =
            notify::recommended_watcher(move |result: Result<Event, notify::Error>| {
                let event = match result {
                    Ok(e) => e,
                    Err(err) => {
                        warn!(%err, "file watcher error");
                        return;
                    }
                };

                let dominated = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_)
                );

                if !dominated {
                    return;
                }

                for path in &event.paths {
                    if path.extension().and_then(|e| e.to_str()) != Some(JSONL_EXTENSION) {
                        continue;
                    }

                    debug!(path = %path.display(), "session file changed, re-parsing");
                    if let Some(session) = parse_session_file(path) {
                        manager.sessions.insert(session.id.clone(), session);
                    }
                }
            })?;

        watcher.watch(&projects_dir, RecursiveMode::Recursive)?;

        // Park the thread — the watcher must stay alive.
        std::thread::park();

        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal JSONL fixture in a temp file.
    fn write_fixture(dir: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(format!("{session_id}.{JSONL_EXTENSION}"));
        let mut file = File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        path
    }

    // ── extract_text_content ─────────────────────────────────────────────

    #[test]
    fn text_content_from_string() {
        let val = serde_json::json!("hello world");
        assert_eq!(extract_text_content(&val).unwrap(), "hello world");
    }

    #[test]
    fn text_content_from_array() {
        let val = serde_json::json!([
            {"type": "tool_use", "id": "abc"},
            {"type": "text", "text": "Build a server"}
        ]);
        assert_eq!(extract_text_content(&val).unwrap(), "Build a server");
    }

    #[test]
    fn text_content_from_empty_array() {
        let val = serde_json::json!([]);
        assert!(extract_text_content(&val).is_none());
    }

    #[test]
    fn text_content_from_null() {
        let val = serde_json::Value::Null;
        assert!(extract_text_content(&val).is_none());
    }

    // ── truncate_summary ─────────────────────────────────────────────────

    #[test]
    fn truncation_short_text() {
        let text = "short message";
        assert_eq!(truncate_summary(text), "short message");
    }

    #[test]
    fn truncation_long_text() {
        let text = "a".repeat(150);
        let result = truncate_summary(&text);
        assert!(result.len() > SUMMARY_MAX_CHARS);
        assert!(result.ends_with(SUMMARY_ELLIPSIS));
    }

    #[test]
    fn truncation_strips_newlines() {
        let text = "line one\nline two";
        assert_eq!(truncate_summary(text), "line one line two");
    }

    // ── decode_project_path ──────────────────────────────────────────────

    #[test]
    fn decode_path_basic() {
        let decoded = decode_project_path("-Users-me-project");
        assert_eq!(decoded, PathBuf::from("/Users/me/project"));
    }

    #[test]
    fn decode_path_deep() {
        let decoded = decode_project_path("-Users-me-workspace-my-app");
        assert_eq!(decoded, PathBuf::from("/Users/me/workspace/my/app"));
    }

    // ── parse_session_file ───────────────────────────────────────────────

    #[test]
    fn parse_minimal_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            dir.path(),
            "abc-123",
            &[
                r#"{"type":"user","message":{"role":"user","content":"Create a web server"},"timestamp":"2026-01-15T10:00:00Z","cwd":"/Users/me/myproject","sessionId":"abc-123"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-4-6","content":[{"type":"text","text":"Sure!"}]},"timestamp":"2026-01-15T10:00:05Z","sessionId":"abc-123"}"#,
            ],
        );

        let session = parse_session_file(&path).unwrap();
        assert_eq!(session.id, "abc-123");
        assert_eq!(session.summary, "Create a web server");
        assert_eq!(session.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(session.message_count, 2);
        assert_eq!(session.project_dir, PathBuf::from("/Users/me/myproject"));
        assert_eq!(session.project_name, "myproject");
        assert_eq!(session.tool, AiTool::ClaudeCode);
        assert!(session.updated_at > session.created_at);
    }

    #[test]
    fn parse_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            dir.path(),
            "broken-session",
            &[
                "NOT VALID JSON",
                r#"{"type":"user","message":{"role":"user","content":"hello"},"timestamp":"2026-02-01T12:00:00Z","cwd":"/tmp"}"#,
                "{also broken",
            ],
        );

        let session = parse_session_file(&path).unwrap();
        assert_eq!(session.summary, "hello");
        assert_eq!(session.message_count, 1);
    }

    #[test]
    fn parse_content_as_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            dir.path(),
            "array-content",
            &[
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fix the bug in main.rs"}]},"timestamp":"2026-03-01T08:00:00Z","cwd":"/project"}"#,
            ],
        );

        let session = parse_session_file(&path).unwrap();
        assert_eq!(session.summary, "Fix the bug in main.rs");
    }

    #[test]
    fn parse_falls_back_to_decoded_path() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate the parent directory name encoding
        let project_dir = dir.path().join("-Users-me-cool-project");
        fs::create_dir_all(&project_dir).unwrap();
        let path = write_fixture(
            &project_dir,
            "sess-1",
            &[
                r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-04-01T00:00:00Z"}"#,
            ],
        );

        let session = parse_session_file(&path).unwrap();
        assert_eq!(session.project_dir, PathBuf::from("/Users/me/cool/project"));
    }

    #[test]
    fn parse_empty_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), "empty", &[]);
        assert!(parse_session_file(&path).is_none());
    }

    // ── SessionManager ───────────────────────────────────────────────────

    fn make_session(id: &str, project: &str, summary: &str, updated: &str) -> AiSession {
        make_session_with_parent(id, project, summary, updated, None)
    }

    fn make_session_with_parent(
        id: &str,
        project: &str,
        summary: &str,
        updated: &str,
        parent_id: Option<&str>,
    ) -> AiSession {
        AiSession {
            id: id.to_string(),
            parent_id: parent_id.map(|s| s.to_string()),
            file_path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            project_dir: PathBuf::from(format!("/projects/{project}")),
            project_name: project.to_string(),
            summary: summary.to_string(),
            message_count: 1,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: updated.parse().unwrap(),
            model: None,
            tool: AiTool::ClaudeCode,
        }
    }

    #[test]
    fn search_matches_project_name() {
        let mgr = SessionManager::new();
        mgr.sessions
            .insert("s1".into(), make_session("s1", "web-app", "init", "2026-01-01T00:00:00Z"));
        mgr.sessions.insert(
            "s2".into(),
            make_session("s2", "cli-tool", "start", "2026-01-02T00:00:00Z"),
        );

        let results = mgr.search("web");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "s1");
    }

    #[test]
    fn search_matches_summary() {
        let mgr = SessionManager::new();
        mgr.sessions.insert(
            "s1".into(),
            make_session("s1", "proj", "fix database migration", "2026-01-01T00:00:00Z"),
        );

        let results = mgr.search("database");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_is_case_insensitive() {
        let mgr = SessionManager::new();
        mgr.sessions.insert(
            "s1".into(),
            make_session("s1", "MyProject", "Hello", "2026-01-01T00:00:00Z"),
        );

        assert_eq!(mgr.search("myproject").len(), 1);
        assert_eq!(mgr.search("HELLO").len(), 1);
    }

    #[test]
    fn all_sessions_sorted_by_updated_at() {
        let mgr = SessionManager::new();
        mgr.sessions.insert(
            "old".into(),
            make_session("old", "a", "old", "2026-01-01T00:00:00Z"),
        );
        mgr.sessions.insert(
            "mid".into(),
            make_session("mid", "b", "mid", "2026-06-15T00:00:00Z"),
        );
        mgr.sessions.insert(
            "new".into(),
            make_session("new", "c", "new", "2026-12-31T00:00:00Z"),
        );

        let sorted = mgr.all_sessions_sorted();
        assert_eq!(sorted[0].id, "new");
        assert_eq!(sorted[1].id, "mid");
        assert_eq!(sorted[2].id, "old");
    }

    #[test]
    fn get_and_count() {
        let mgr = SessionManager::new();
        mgr.sessions.insert(
            "s1".into(),
            make_session("s1", "p", "x", "2026-01-01T00:00:00Z"),
        );

        assert_eq!(mgr.count(), 1);
        assert!(mgr.get("s1").is_some());
        assert!(mgr.get("s999").is_none());
    }

    // ── parent_id parsing ───────────────────────────────────────────────

    #[test]
    fn parse_parent_id_forked_from() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            dir.path(),
            "child-sess",
            &[
                r#"{"type":"user","message":{"role":"user","content":"continue"},"timestamp":"2026-01-15T10:00:00Z","cwd":"/project","forkedFrom":{"sessionId":"parent-sess","messageUuid":"msg-123"}}"#,
            ],
        );

        let session = parse_session_file(&path).unwrap();
        assert_eq!(session.parent_id.as_deref(), Some("parent-sess"));
    }

    #[test]
    fn parse_no_parent_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            dir.path(),
            "root-sess",
            &[
                r#"{"type":"user","message":{"role":"user","content":"start"},"timestamp":"2026-01-15T10:00:00Z","cwd":"/project"}"#,
            ],
        );

        let session = parse_session_file(&path).unwrap();
        assert!(session.parent_id.is_none());
    }

    // ── build_tree ──────────────────────────────────────────────────────

    #[test]
    fn build_tree_simple() {
        let parent = make_session("A", "proj", "root", "2026-01-02T00:00:00Z");
        let child = make_session_with_parent("B", "proj", "child", "2026-01-03T00:00:00Z", Some("A"));

        let nodes = build_tree(vec![parent, child]);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].session.id, "A");
        assert_eq!(nodes[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].session.id, "B");
    }

    #[test]
    fn build_tree_orphan() {
        let orphan =
            make_session_with_parent("B", "proj", "orphan", "2026-01-02T00:00:00Z", Some("missing"));

        let nodes = build_tree(vec![orphan]);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].session.id, "B");
        assert!(nodes[0].children.is_empty());
    }

    #[test]
    fn build_tree_chain() {
        let a = make_session("A", "proj", "first", "2026-01-01T00:00:00Z");
        let b = make_session_with_parent("B", "proj", "second", "2026-01-02T00:00:00Z", Some("A"));
        let c = make_session_with_parent("C", "proj", "third", "2026-01-03T00:00:00Z", Some("B"));

        let nodes = build_tree(vec![a, b, c]);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].session.id, "A");
        assert_eq!(nodes[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].session.id, "B");
        assert_eq!(nodes[0].children[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].children[0].session.id, "C");
    }

    #[test]
    fn build_tree_fork() {
        let a = make_session("A", "proj", "root", "2026-01-01T00:00:00Z");
        let b = make_session_with_parent("B", "proj", "fork-1", "2026-01-02T00:00:00Z", Some("A"));
        let c = make_session_with_parent("C", "proj", "fork-2", "2026-01-03T00:00:00Z", Some("A"));

        let nodes = build_tree(vec![a, b, c]);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].session.id, "A");
        assert_eq!(nodes[0].children.len(), 2);
        // Children sorted by updated_at desc: C (Jan 3) before B (Jan 2)
        assert_eq!(nodes[0].children[0].session.id, "C");
        assert_eq!(nodes[0].children[1].session.id, "B");
    }

    #[test]
    fn session_trees_sorted() {
        let mgr = SessionManager::new();
        // Project alpha — older
        mgr.sessions.insert(
            "a1".into(),
            make_session("a1", "alpha", "old", "2026-01-01T00:00:00Z"),
        );
        // Project beta — newer
        mgr.sessions.insert(
            "b1".into(),
            make_session("b1", "beta", "new", "2026-06-01T00:00:00Z"),
        );

        let trees = mgr.session_trees();
        assert_eq!(trees.len(), 2);
        assert_eq!(trees[0].project_name, "beta");
        assert_eq!(trees[1].project_name, "alpha");
    }

    // ── flatten ─────────────────────────────────────────────────────────

    #[test]
    fn flatten_tree() {
        let a = make_session("A", "proj", "root", "2026-01-01T00:00:00Z");
        let b = make_session_with_parent("B", "proj", "child-1", "2026-01-03T00:00:00Z", Some("A"));
        let c = make_session_with_parent("C", "proj", "child-2", "2026-01-02T00:00:00Z", Some("A"));

        let nodes = build_tree(vec![a, b, c]);
        let mut flat = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            node.flatten(0, i == nodes.len() - 1, &mut flat);
        }

        assert_eq!(flat.len(), 3);

        // Root
        assert_eq!(flat[0].session.id, "A");
        assert_eq!(flat[0].depth, 0);
        assert!(flat[0].has_children);

        // First child (B, most recent) at depth 1
        assert_eq!(flat[1].session.id, "B");
        assert_eq!(flat[1].depth, 1);
        assert!(!flat[1].is_last_child);

        // Second child (C, older) at depth 1, is last
        assert_eq!(flat[2].session.id, "C");
        assert_eq!(flat[2].depth, 1);
        assert!(flat[2].is_last_child);
    }
}

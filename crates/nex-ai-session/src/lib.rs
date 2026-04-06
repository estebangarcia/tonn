//! AI session discovery and management for Nexterm.
//!
//! Provides a trait-based provider pattern (`AiToolProvider`) so that multiple
//! AI tools (Claude Code, Cursor, etc.) can be plugged into a single
//! `SessionManager`.  The built-in `ClaudeCodeProvider` scans
//! `~/.claude/projects/` for session JSONL files.

pub mod claude_code;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tracing::{debug, warn};

pub use claude_code::ClaudeCodeProvider;

// ── Constants ────────────────────────────────────────────────────────────────

/// How recently a session file must have been modified to be considered "active."
const ACTIVE_SESSION_RECENCY_SECS: u64 = 60;

// ── Types ────────────────────────────────────────────────────────────────────

/// Which AI tool produced the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiTool {
    ClaudeCode,
}

impl fmt::Display for AiTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiTool::ClaudeCode => write!(f, "Claude Code"),
        }
    }
}

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

// ── Provider Trait ───────────────────────────────────────────────────────────

/// Trait for AI tool providers that can discover and parse sessions.
pub trait AiToolProvider: Send + Sync {
    /// Human-readable name of this provider.
    fn name(&self) -> &str;

    /// The `AiTool` variant this provider produces.
    fn tool(&self) -> AiTool;

    /// Scan the filesystem and return all discovered sessions.
    fn scan(&self) -> Vec<AiSession>;

    /// Directories that should be watched for file changes.
    fn watch_paths(&self) -> Vec<PathBuf>;

    /// Parse a single file into a session, if applicable.
    fn parse_file(&self, path: &Path) -> Option<AiSession>;

    /// Build a shell command to resume the given session.
    fn resume_command(&self, session: &AiSession) -> String;
}

// ── Shared Utilities ────────────────────────────────────────────────────────

/// Extract the human-readable project name from a project directory path.
///
/// Uses the last component of the path (e.g. `/Users/me/my-project` → `my-project`).
pub(crate) fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
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
    providers: Vec<Box<dyn AiToolProvider>>,
    sessions: DashMap<String, AiSession>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    /// Create a new `SessionManager` with the `ClaudeCodeProvider` registered.
    pub fn new() -> Self {
        let providers: Vec<Box<dyn AiToolProvider>> =
            vec![Box::new(ClaudeCodeProvider::new())];
        Self {
            providers,
            sessions: DashMap::new(),
        }
    }

    /// Register an additional AI tool provider.
    pub fn register(&mut self, provider: Box<dyn AiToolProvider>) {
        self.providers.push(provider);
    }

    /// Scan all registered providers for sessions.
    pub fn scan(&self) {
        for provider in &self.providers {
            let discovered = provider.scan();
            for session in discovered {
                self.sessions.insert(session.id.clone(), session);
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

    /// Names of all registered providers.
    pub fn provider_names(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.name().to_string()).collect()
    }

    /// Build a resume command for the given session by delegating to its provider.
    pub fn resume_command(&self, session: &AiSession) -> String {
        for provider in &self.providers {
            if provider.tool() == session.tool {
                return provider.resume_command(session);
            }
        }
        // Fallback — should not happen if providers are registered correctly.
        String::new()
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

    /// Watch all registered providers' paths for file changes.
    ///
    /// Blocks the calling thread. On `Create` or `Modify` events for matching
    /// files, the affected session is re-parsed via the appropriate provider
    /// and the map is updated.
    pub fn start_watcher(self: &Arc<Self>) -> notify::Result<()> {
        let watch_paths: Vec<PathBuf> = self
            .providers
            .iter()
            .flat_map(|p| p.watch_paths())
            .collect();

        if watch_paths.is_empty() {
            return Err(notify::Error::generic(
                "no watch paths from any registered provider",
            ));
        }

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
                    debug!(path = %path.display(), "session file changed, re-parsing");
                    for provider in &manager.providers {
                        if let Some(session) = provider.parse_file(path) {
                            manager.sessions.insert(session.id.clone(), session);
                            break;
                        }
                    }
                }
            })?;

        for path in &watch_paths {
            if path.is_dir() {
                watcher.watch(path, RecursiveMode::Recursive)?;
            }
        }

        // Park the thread — the watcher must stay alive.
        std::thread::park();

        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── AiTool Display ──────────────────────────────────────────────────

    #[test]
    fn ai_tool_display() {
        assert_eq!(format!("{}", AiTool::ClaudeCode), "Claude Code");
    }
}

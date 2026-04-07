//! Claude Code provider — discovers and parses Claude Code session JSONL files.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use tracing::debug;

use crate::{project_name_from_path, AiSession, AiTool, AiToolProvider};

// ── Constants ────────────────────────────────────────────────────────────────

const SUMMARY_MAX_CHARS: usize = 100;
const SUMMARY_ELLIPSIS: &str = "…";
const JSONL_EXTENSION: &str = "jsonl";
const CLAUDE_PROJECTS_DIR: &str = ".claude/projects";
const PATH_SEPARATOR: char = '-';

const CLAUDE_CLI_LOCAL_BIN: &str = ".local/bin/claude";
const CLAUDE_CLI_DOT_CLAUDE_BIN: &str = ".claude/bin/claude";
const CLAUDE_CLI_USR_LOCAL: &str = "/usr/local/bin/claude";
const CLAUDE_CLI_HOMEBREW: &str = "/opt/homebrew/bin/claude";
const CLAUDE_CLI_FALLBACK: &str = "claude";


// ── ClaudeCodeProvider ──────────────────────────────────────────────────────

/// Provider that discovers AI sessions from Claude Code's JSONL project files.
pub struct ClaudeCodeProvider {
    base_dir: PathBuf,
}

impl ClaudeCodeProvider {
    pub fn new() -> Self {
        let base_dir = dirs::home_dir()
            .unwrap_or_default()
            .join(CLAUDE_PROJECTS_DIR);
        Self { base_dir }
    }
}

impl Default for ClaudeCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AiToolProvider for ClaudeCodeProvider {
    fn name(&self) -> &str {
        "Claude Code"
    }

    fn tool(&self) -> AiTool {
        AiTool::ClaudeCode
    }

    fn scan(&self) -> Vec<AiSession> {
        let mut sessions = Vec::new();

        if !self.base_dir.is_dir() {
            tracing::warn!("claude projects directory not found");
            return sessions;
        }

        let entries = match fs::read_dir(&self.base_dir) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(%err, "failed to read claude projects directory");
                return sessions;
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

                if let Some(session) = self.parse_file(&file_path) {
                    sessions.push(session);
                }
            }
        }

        sessions
    }

    fn watch_paths(&self) -> Vec<PathBuf> {
        vec![self.base_dir.clone()]
    }

    fn parse_file(&self, path: &Path) -> Option<AiSession> {
        parse_session_file(path)
    }

    fn resume_command(&self, session: &AiSession) -> String {
        let claude = find_claude_cli();
        format!(
            "cd {} && {} --resume {}",
            session.project_dir.display(),
            claude.display(),
            session.id,
        )
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
                if item.get("type").and_then(|t| t.as_str()) == Some("text")
                    && let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        return Some(text.to_string());
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
    let decoded = encoded.replacen(PATH_SEPARATOR, "/", 1);
    let decoded = decoded.replace(PATH_SEPARATOR, "/");
    PathBuf::from(decoded)
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
        if let Some(ts_str) = entry.get("timestamp").and_then(|t| t.as_str())
            && let Ok(ts) = ts_str.parse::<DateTime<Utc>>() {
                if first_timestamp.is_none() {
                    first_timestamp = Some(ts);
                }
                last_timestamp = Some(ts);
            }

        // Extract parent session ID from forkedFrom or parentSessionId
        if parent_id.is_none() {
            if let Some(forked) = entry.get("forkedFrom")
                && let Some(pid) = forked.get("sessionId").and_then(|v| v.as_str()) {
                    parent_id = Some(pid.to_string());
                }
            if parent_id.is_none()
                && let Some(pid) = entry.get("parentSessionId").and_then(|v| v.as_str()) {
                    parent_id = Some(pid.to_string());
                }
        }

        // Extract cwd from the first message that has it
        if cwd.is_none()
            && let Some(c) = entry.get("cwd").and_then(|v| v.as_str()) {
                cwd = Some(c.to_string());
            }

        match msg_type {
            Some("user") => {
                message_count += 1;
                if summary.is_none()
                    && let Some(content) = entry.get("message").and_then(|m| m.get("content"))
                        && let Some(text) = extract_text_content(content) {
                            summary = Some(truncate_summary(&text));
                        }
            }
            Some("assistant") => {
                message_count += 1;
                if model.is_none()
                    && let Some(m) = entry
                        .get("message")
                        .and_then(|msg| msg.get("model"))
                        .and_then(|v| v.as_str())
                    {
                        model = Some(m.to_string());
                    }
            }
            _ => {}
        }
    }

    let created_at = first_timestamp?;
    let updated_at = last_timestamp.unwrap_or(created_at);

    let project_dir = if let Some(ref c) = cwd {
        PathBuf::from(c)
    } else {
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

// ── CLI Discovery ───────────────────────────────────────────────────────────

/// Find the Claude CLI binary, checking common install locations.
fn find_claude_cli() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    [
        home.join(CLAUDE_CLI_LOCAL_BIN),
        home.join(CLAUDE_CLI_DOT_CLAUDE_BIN),
        PathBuf::from(CLAUDE_CLI_USR_LOCAL),
        PathBuf::from(CLAUDE_CLI_HOMEBREW),
    ]
    .into_iter()
    .find(|p| p.exists())
    .unwrap_or_else(|| PathBuf::from(CLAUDE_CLI_FALLBACK))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const TEST_JSONL_EXTENSION: &str = JSONL_EXTENSION;

    /// Build a minimal JSONL fixture in a temp file.
    fn write_fixture(dir: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(format!("{session_id}.{TEST_JSONL_EXTENSION}"));
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
}

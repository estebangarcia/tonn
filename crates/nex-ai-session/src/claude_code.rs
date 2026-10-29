//! Claude Code provider — discovers and parses Claude Code session JSONL files.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{project_name_from_path, AiSession, AiTool, AiToolProvider};

// ── Constants ────────────────────────────────────────────────────────────────

const SUMMARY_MAX_CHARS: usize = 100;
const SUMMARY_ELLIPSIS: &str = "…";
const JSONL_EXTENSION: &str = "jsonl";
const CLAUDE_PROJECTS_DIR: &str = ".claude/projects";
const PATH_SEPARATOR: char = '-';

/// How much of a transcript's start and end is read for its metadata (title,
/// cwd, model, timestamps) — the window the VS Code extension uses too.
/// Transcripts reach tens of MB; parsing every line as JSON took ~40 ms for a
/// 28 MB file and ~0.5 s for a full scan.
const TRANSCRIPT_WINDOW_BYTES: u64 = 64 * 1024;
/// A renamed session's title, stored next to the transcript:
/// `<project>/<session-id>/custom-title.json` → `{"customTitle": "…"}`.
const CUSTOM_TITLE_SIDECAR: &str = "custom-title.json";
/// Lines counted as messages (top-level `type`; JSON-string content escapes
/// its quotes, so these only match real entries).
const MESSAGE_MARKERS: [&str; 2] = [r#""type":"user""#, r#""type":"assistant""#];
const INTERRUPTED_MARKER: &str = "[Request interrupted by user";
const COMMAND_NAME_TAGS: (&str, &str) = ("<command-name>", "</command-name>");
const BASH_INPUT_TAGS: (&str, &str) = ("<bash-input>", "</bash-input>");
const BASH_INPUT_PREFIX: &str = "! ";

const CLAUDE_CLI_LOCAL_BIN: &str = ".local/bin/claude";
const CLAUDE_CLI_DOT_CLAUDE_BIN: &str = ".claude/bin/claude";
const CLAUDE_CLI_USR_LOCAL: &str = "/usr/local/bin/claude";
const CLAUDE_CLI_HOMEBREW: &str = "/opt/homebrew/bin/claude";
const CLAUDE_CLI_FALLBACK: &str = "claude";


// ── ClaudeCodeProvider ──────────────────────────────────────────────────────

/// Provider that discovers AI sessions from Claude Code's JSONL project files.
pub struct ClaudeCodeProvider {
    base_dir: PathBuf,
    /// `base_dir` plus, if different, its canonical path. The scan walks
    /// `base_dir` as configured, but the macOS watcher canonicalizes its
    /// roots and reports resolved paths — through a symlinked `~/.claude`
    /// (or home) the two differ, and both must be recognised.
    roots: Vec<PathBuf>,
}

impl ClaudeCodeProvider {
    pub fn new() -> Self {
        let base_dir = dirs::home_dir()
            .unwrap_or_default()
            .join(CLAUDE_PROJECTS_DIR);
        Self::with_base_dir(base_dir)
    }

    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        let mut roots = vec![base_dir.clone()];
        if let Ok(canonical) = base_dir.canonicalize()
            && canonical != base_dir
        {
            roots.push(canonical);
        }
        Self { base_dir, roots }
    }

    /// Whether `path` is a session transcript: `<base>/<project>/<id>.jsonl`.
    /// Pure path check (no I/O). The projects tree also holds per-session
    /// directories, `memory/` notes and nested subagent transcripts, which
    /// must not be parsed as sessions.
    fn is_session_file(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some(JSONL_EXTENSION)
            && path
                .parent()
                .and_then(Path::parent)
                .is_some_and(|projects| self.roots.iter().any(|root| root == projects))
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
                if let Some(session) = self.parse_file(&file_entry.path()) {
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
        if !self.is_session_file(path) {
            return None;
        }
        parse_session_file(path)
    }

    /// A transcript maps to itself; a title sidecar
    /// (`<project>/<id>/custom-title.json`) to its transcript
    /// (`<project>/<id>.jsonl`), so renames refresh the title.
    fn session_file_for(&self, changed: &Path) -> Option<PathBuf> {
        if self.is_session_file(changed) {
            return Some(changed.to_path_buf());
        }
        if changed.file_name()?.to_str()? != CUSTOM_TITLE_SIDECAR {
            return None;
        }
        let session_dir = changed.parent()?;
        let transcript = session_dir
            .parent()?
            .join(format!("{}.{JSONL_EXTENSION}", session_dir.file_name()?.to_str()?));
        self.is_session_file(&transcript).then_some(transcript)
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

/// One line for the session list: control characters (newlines, tabs) become
/// spaces, and the text is cut to `SUMMARY_MAX_CHARS` characters with an
/// ellipsis.
fn truncate_summary(text: &str) -> String {
    let flattened: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    match flattened.char_indices().nth(SUMMARY_MAX_CHARS) {
        Some((boundary, _)) => format!("{}{SUMMARY_ELLIPSIS}", flattened[..boundary].trim_end()),
        None => flattened,
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
/// Only the first and last `TRANSCRIPT_WINDOW_BYTES` are parsed as JSON (the
/// title, cwd, model and timestamps live there); the message count comes
/// from a plain text scan of the whole file.
fn parse_session_file(path: &Path) -> Option<AiSession> {
    let session_id = path.file_stem()?.to_str()?.to_string();
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();

    let head = parse_lines(&read_head(&mut file, len)?);
    let tail_entries = match read_tail(&mut file, len)? {
        Some(window) => parse_lines(&window),
        None => Vec::new(),
    };
    // A small transcript fits in the head window: then it's also the tail.
    let tail: &[Value] = if len > TRANSCRIPT_WINDOW_BYTES { &tail_entries } else { &head };

    // A subagent ("sidechain") transcript isn't a session of its own.
    if head.first().and_then(|e| e.get("isSidechain")).and_then(Value::as_bool) == Some(true) {
        return None;
    }

    // Fallbacks for timestamps outside the windows (e.g. a transcript that
    // opens with large untimestamped records): scan forward for the first
    // one; for the last, use the file's modification time.
    let created_at = head
        .iter()
        .find_map(timestamp)
        .or_else(|| first_timestamp_in_file(&mut file))?;
    let updated_at = tail
        .iter()
        .rev()
        .find_map(timestamp)
        .or_else(|| file.metadata().and_then(|m| m.modified()).ok().map(DateTime::<Utc>::from))
        .unwrap_or(created_at)
        .max(created_at);
    let cwd = head.iter().find_map(|e| str_field(e, "cwd"));
    let parent_id = head.iter().find_map(|e| {
        e.get("forkedFrom")
            .and_then(|f| f.get("sessionId"))
            .and_then(Value::as_str)
            .or_else(|| str_field(e, "parentSessionId"))
            .map(str::to_string)
    });
    let model = [head.as_slice(), tail]
        .into_iter()
        .find_map(|entries| entries.iter().find_map(assistant_model));

    let custom_title_file = path.parent()?.join(&session_id).join(CUSTOM_TITLE_SIDECAR);
    let summary = session_title(&head, tail, read_custom_title(&custom_title_file))
        .map(|title| truncate_summary(&title))
        .unwrap_or_default();

    let project_dir = match cwd {
        Some(c) => PathBuf::from(c),
        None => path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(decode_project_path)
            .unwrap_or_default(),
    };
    let project_name = project_name_from_path(&project_dir);

    Some(AiSession {
        id: session_id,
        parent_id,
        file_path: path.to_path_buf(),
        project_dir,
        project_name,
        summary,
        message_count: count_messages(&mut file),
        created_at,
        updated_at,
        model: model.map(str::to_string),
        tool: AiTool::ClaudeCode,
    })
}

/// The session's list title, in the VS Code extension's order of preference:
/// a custom title (from `/rename`), the AI-generated title (`ai-title`, newest
/// first), the last prompt, a legacy summary, then the first real prompt.
fn session_title(head: &[Value], tail: &[Value], sidecar_title: Option<String>) -> Option<String> {
    let latest = |entries: &[Value], entry_type: &str, key: &str| {
        entries
            .iter()
            .rev()
            .filter(|e| str_field(e, "type") == Some(entry_type))
            .find_map(|e| str_field(e, key).filter(|v| !v.trim().is_empty()))
            .map(str::to_string)
    };
    latest(tail, "custom-title", "customTitle")
        .or(sidecar_title)
        .or_else(|| latest(head, "custom-title", "customTitle"))
        .or_else(|| latest(tail, "ai-title", "aiTitle"))
        .or_else(|| latest(head, "ai-title", "aiTitle"))
        .or_else(|| latest(tail, "last-prompt", "lastPrompt"))
        .or_else(|| latest(tail, "summary", "summary"))
        .or_else(|| first_prompt(head))
}

/// The first prompt the user actually typed. Skips meta and compaction
/// entries, tool results, and text that starts with injected context —
/// `<ide_opened_file>`, `<ide_selection>`, `<system-reminder>`, … — or an
/// interruption notice. Shell inputs read `! cmd`; if only slash commands
/// were sent, the first command's name is used.
fn first_prompt(entries: &[Value]) -> Option<String> {
    let mut command_fallback = None;
    for entry in entries {
        let is_flag = |key| entry.get(key).and_then(Value::as_bool) == Some(true);
        if str_field(entry, "type") != Some("user") || is_flag("isMeta") || is_flag("isCompactSummary") {
            continue;
        }
        let texts: Vec<&str> = match entry.get("message").and_then(|m| m.get("content")) {
            Some(Value::String(text)) => vec![text],
            Some(Value::Array(blocks)) => {
                if blocks.iter().any(|b| str_field(b, "type") == Some("tool_result")) {
                    continue;
                }
                blocks
                    .iter()
                    .filter(|b| str_field(b, "type") == Some("text"))
                    .filter_map(|b| str_field(b, "text"))
                    .collect()
            }
            _ => continue,
        };
        for text in texts {
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if let Some(command) = between(text, COMMAND_NAME_TAGS) {
                command_fallback.get_or_insert_with(|| command.to_string());
                continue;
            }
            if let Some(input) = between(text, BASH_INPUT_TAGS) {
                return Some(format!("{BASH_INPUT_PREFIX}{}", input.trim()));
            }
            if !is_injected_context(text) {
                return Some(text.to_string());
            }
        }
    }
    command_fallback
}

/// Whether text starts with a tag (`<ide_opened_file>`, `<system-reminder …>`)
/// or an interruption notice — context added around a prompt, not the prompt.
fn is_injected_context(text: &str) -> bool {
    if text.starts_with(INTERRUPTED_MARKER) {
        return true;
    }
    let Some(rest) = text.strip_prefix('<') else { return false };
    let mut chars = rest.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return false;
    }
    chars
        .find(|&c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .is_some_and(|c| c.is_whitespace() || c == '>')
}

/// The text between the first `open` tag and the following `close` tag.
fn between<'a>(text: &'a str, (open, close): (&str, &str)) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let len = text[start..].find(close)?;
    Some(&text[start..start + len])
}

fn str_field<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

fn timestamp(entry: &Value) -> Option<DateTime<Utc>> {
    str_field(entry, "timestamp")?.parse().ok()
}

fn assistant_model(entry: &Value) -> Option<&str> {
    if str_field(entry, "type") != Some("assistant") {
        return None;
    }
    entry.get("message").and_then(|m| str_field(m, "model"))
}

/// A renamed session's title from its sidecar file, control characters removed.
fn read_custom_title(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    let title: String = str_field(&value, "customTitle")?.chars().filter(|c| !c.is_control()).collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// The transcript's first `TRANSCRIPT_WINDOW_BYTES`, extended to the end of
/// the record the window cuts into — so even an opening record larger than
/// the window (e.g. a pasted log as the first prompt) is read whole.
fn read_head(file: &mut File, len: u64) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut reader = BufReader::new(file);
    let mut head = Vec::with_capacity(len.min(TRANSCRIPT_WINDOW_BYTES) as usize);
    (&mut reader).take(TRANSCRIPT_WINDOW_BYTES).read_to_end(&mut head).ok()?;
    if head.last().is_some_and(|&b| b != b'\n') {
        reader.read_until(b'\n', &mut head).ok()?;
    }
    Some(head)
}

/// The transcript's last `TRANSCRIPT_WINDOW_BYTES`, extended back to the start
/// of the record the window cuts into — so a final record larger than the
/// window is read whole. `None` when the head already covers the file.
fn read_tail(file: &mut File, len: u64) -> Option<Option<Vec<u8>>> {
    if len <= TRANSCRIPT_WINDOW_BYTES {
        return Some(None);
    }
    let start = record_start_at_or_before(file, len - TRANSCRIPT_WINDOW_BYTES)?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::with_capacity((len - start) as usize);
    file.take(len - start).read_to_end(&mut tail).ok()?;
    Some(Some(tail))
}

/// Offset of the start of the record containing byte `pos`: just after the
/// nearest preceding newline, or 0. Searches backwards a window at a time.
fn record_start_at_or_before(file: &mut File, pos: u64) -> Option<u64> {
    let mut end = pos;
    let mut chunk = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(TRANSCRIPT_WINDOW_BYTES);
        file.seek(SeekFrom::Start(start)).ok()?;
        chunk.clear();
        (&mut *file).take(end - start).read_to_end(&mut chunk).ok()?;
        if let Some(newline) = chunk.iter().rposition(|&b| b == b'\n') {
            return Some(start + newline as u64 + 1);
        }
        end = start;
    }
    Some(0)
}

/// Parse a window's JSONL records (windows start and end on record
/// boundaries); blank and unparseable lines — e.g. a record still being
/// written — are skipped.
fn parse_lines(window: &[u8]) -> Vec<Value> {
    window
        .split(|&b| b == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

/// The first record timestamp anywhere in the file (fallback when the head
/// window has none).
fn first_timestamp_in_file(file: &mut File) -> Option<DateTime<Utc>> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).ok()? > 0 {
        if let Ok(entry) = serde_json::from_slice::<Value>(&line)
            && let Some(found) = timestamp(&entry)
        {
            return Some(found);
        }
        line.clear();
    }
    None
}

/// Count user/assistant entries over the whole file with a plain text scan —
/// no JSON parsing.
fn count_messages(file: &mut File) -> usize {
    if file.seek(SeekFrom::Start(0)).is_err() {
        return 0;
    }
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut count = 0;
    while reader.read_until(b'\n', &mut line).is_ok_and(|read| read > 0) {
        if let Ok(text) = std::str::from_utf8(&line)
            && MESSAGE_MARKERS.iter().any(|marker| text.contains(marker))
        {
            count += 1;
        }
        line.clear();
    }
    count
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

    #[test]
    fn only_project_level_transcripts_are_session_files() {
        let base = PathBuf::from("/home/u/.claude/projects");
        let provider = ClaudeCodeProvider::with_base_dir(base.clone());
        let project = base.join("-home-u-repo");
        assert!(provider.is_session_file(&project.join("abc.jsonl")));
        // Per-session directories, nested subagent transcripts and memory notes.
        assert!(!provider.is_session_file(&project.join("abc")));
        assert!(!provider.is_session_file(&project.join("abc/subagents/agent-1.jsonl")));
        assert!(!provider.is_session_file(&project.join("memory/notes.md")));
        assert!(!provider.is_session_file(&base.join("stray.jsonl")));
    }

    #[cfg(unix)]
    #[test]
    fn session_files_are_recognised_through_a_symlinked_root() {
        // The watcher reports resolved paths; the scan uses the configured
        // (symlinked) root. Both spellings of a transcript must be accepted.
        let dir = tempfile::tempdir().unwrap();
        let real_root = dir.path().join("real-projects");
        std::fs::create_dir_all(&real_root).unwrap();
        let linked_root = dir.path().join("linked-projects");
        std::os::unix::fs::symlink(&real_root, &linked_root).unwrap();

        let provider = ClaudeCodeProvider::with_base_dir(linked_root.clone());
        assert!(provider.is_session_file(&linked_root.join("proj/abc.jsonl")));
        let resolved_root = real_root.canonicalize().unwrap();
        assert!(provider.is_session_file(&resolved_root.join("proj/abc.jsonl")));
        assert!(!provider.is_session_file(&resolved_root.join("proj/abc/sub.jsonl")));
    }

    #[test]
    fn unreadable_session_path_returns_promptly() {
        // A directory named like a transcript: open() succeeds but every
        // read() fails (EISDIR). Skipping read errors used to spin forever at
        // 100% CPU on the file watcher thread.
        let base = tempfile::tempdir().unwrap();
        let dir_named_like_a_session = base.path().join("project").join("abc.jsonl");
        std::fs::create_dir_all(&dir_named_like_a_session).unwrap();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done_tx.send(parse_session_file(&dir_named_like_a_session).is_none());
        });
        let returned_none = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("parse_session_file must not spin on read errors");
        assert!(returned_none);
    }

    /// Build a minimal JSONL fixture in a temp file.
    fn write_fixture(dir: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(format!("{session_id}.{TEST_JSONL_EXTENSION}"));
        let mut file = File::create(&path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        path
    }

    // ── first_prompt / session_title ─────────────────────────────────────

    fn user(content: serde_json::Value) -> Value {
        serde_json::json!({"type": "user", "message": {"role": "user", "content": content}})
    }

    #[test]
    fn first_prompt_from_string_or_text_blocks() {
        assert_eq!(first_prompt(&[user(serde_json::json!("hello world"))]).as_deref(), Some("hello world"));
        let blocks = serde_json::json!([
            {"type": "tool_use", "id": "abc"},
            {"type": "text", "text": "Build a server"}
        ]);
        assert_eq!(first_prompt(&[user(blocks)]).as_deref(), Some("Build a server"));
        assert_eq!(first_prompt(&[user(serde_json::json!([]))]), None);
        assert_eq!(first_prompt(&[user(Value::Null)]), None);
    }

    #[test]
    fn first_prompt_skips_injected_context() {
        // What a VS Code session's first message looks like.
        let blocks = serde_json::json!([
            {"type": "text", "text": "<ide_opened_file>The user opened the file /a/b.rs</ide_opened_file>"},
            {"type": "text", "text": "<system-reminder>be nice</system-reminder>"},
            {"type": "text", "text": "Fix the scroll bug"}
        ]);
        let mut meta = user(serde_json::json!("Caveat: generated by a command"));
        meta["isMeta"] = Value::Bool(true);
        let tool_result = user(serde_json::json!([{"type": "tool_result", "content": "x"}]));
        let interrupted = user(serde_json::json!("[Request interrupted by user]"));
        let entries = [meta, tool_result, interrupted, user(blocks)];
        assert_eq!(first_prompt(&entries).as_deref(), Some("Fix the scroll bug"));
    }

    #[test]
    fn first_prompt_shell_input_and_command_fallback() {
        let bash = user(serde_json::json!("<bash-input> git status </bash-input>"));
        assert_eq!(first_prompt(&[bash]).as_deref(), Some("! git status"));
        let command = user(serde_json::json!("<command-name>/init</command-name><command-message>init</command-message>"));
        assert_eq!(first_prompt(&[command]).as_deref(), Some("/init"));
    }

    #[test]
    fn injected_context_detection() {
        assert!(is_injected_context("<ide_selection>The user selected"));
        assert!(is_injected_context("<system-reminder>\nx"));
        assert!(is_injected_context("[Request interrupted by user for tool use]"));
        assert!(!is_injected_context("<3 this terminal"), "not a tag");
        assert!(!is_injected_context("Use <Box> here"), "tag not at the start");
    }

    #[test]
    fn title_preference_order() {
        let ai = |t: &str| serde_json::json!({"type": "ai-title", "aiTitle": t});
        let custom = serde_json::json!({"type": "custom-title", "customTitle": "My rename"});
        let last = serde_json::json!({"type": "last-prompt", "lastPrompt": "latest ask"});
        let prompt = user(serde_json::json!("first ask"));

        let head = [prompt.clone(), ai("Old title")];
        let tail = [ai("New title"), last.clone()];
        assert_eq!(session_title(&head, &tail, None).as_deref(), Some("New title"), "newest AI title");
        assert_eq!(session_title(&head, &[custom], None).as_deref(), Some("My rename"));
        assert_eq!(session_title(&head, &tail, Some("Sidecar".into())).as_deref(), Some("Sidecar"));
        assert_eq!(session_title(std::slice::from_ref(&prompt), &[last], None).as_deref(), Some("latest ask"));
        assert_eq!(session_title(&[prompt], &[], None).as_deref(), Some("first ask"));
    }

    #[test]
    fn large_transcript_reads_title_and_end_time_from_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = vec![
            r#"{"type":"user","message":{"role":"user","content":"<ide_opened_file>x</ide_opened_file>"},"timestamp":"2026-01-15T10:00:00Z","cwd":"/Users/me/proj"}"#.to_string(),
        ];
        // ~150 KB of assistant turns: bigger than the head + tail windows.
        let filler = "z".repeat(100);
        for _ in 0..1_000 {
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"model":"claude-x","content":[{{"type":"text","text":"{filler}"}}]}},"timestamp":"2026-01-15T10:30:00Z"}}"#
            ));
        }
        lines.push(r#"{"type":"ai-title","aiTitle":"Fix the scroll bug"}"#.to_string());
        lines.push(r#"{"type":"user","message":{"role":"user","content":"thanks"},"timestamp":"2026-01-15T11:00:00Z"}"#.to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let path = write_fixture(dir.path(), "big", &refs);
        assert!(fs::metadata(&path).unwrap().len() > 2 * TRANSCRIPT_WINDOW_BYTES);

        let session = parse_session_file(&path).unwrap();
        assert_eq!(session.summary, "Fix the scroll bug");
        assert_eq!(session.message_count, 1_002, "counted over the whole file");
        assert_eq!(session.updated_at, "2026-01-15T11:00:00Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(session.model.as_deref(), Some("claude-x"));
        assert_eq!(session.project_dir, PathBuf::from("/Users/me/proj"));
    }

    /// A record whose message text is bigger than a metadata window.
    fn oversized_record(entry_type: &str, timestamp: &str) -> String {
        let text = "x".repeat(70_000);
        format!(r#"{{"type":"{entry_type}","message":{{"role":"{entry_type}","content":"{text}"}},"timestamp":"{timestamp}"}}"#)
    }

    #[test]
    fn opening_record_larger_than_the_window_is_read_whole() {
        let dir = tempfile::tempdir().unwrap();
        let first = oversized_record("user", "2026-01-15T10:00:00Z");
        let path = write_fixture(dir.path(), "big-first", &[&first, r#"{"type":"ai-title","aiTitle":"Pasted log triage"}"#]);
        let session = parse_session_file(&path).expect("session must not be hidden");
        assert_eq!(session.created_at, "2026-01-15T10:00:00Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(session.summary, "Pasted log triage");
    }

    #[test]
    fn final_record_larger_than_the_window_sets_the_update_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = vec![
            r#"{"type":"user","message":{"role":"user","content":"start"},"timestamp":"2026-01-15T10:00:00Z"}"#.to_string(),
        ];
        lines.push(oversized_record("assistant", "2026-01-15T12:00:00Z"));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let session = parse_session_file(&write_fixture(dir.path(), "big-last", &refs)).unwrap();
        assert_eq!(session.updated_at, "2026-01-15T12:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn creation_time_found_beyond_the_head_window() {
        // The head window holds only untimestamped records.
        let dir = tempfile::tempdir().unwrap();
        let snapshot = format!(r#"{{"type":"file-history-snapshot","snapshot":"{}"}}"#, "s".repeat(70_000));
        let path = write_fixture(
            dir.path(),
            "late-ts",
            &[
                r#"{"type":"ai-title","aiTitle":"Late timestamps"}"#,
                &snapshot,
                r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-01-15T10:00:00Z"}"#,
            ],
        );
        let session = parse_session_file(&path).expect("session must not be hidden");
        assert_eq!(session.created_at, "2026-01-15T10:00:00Z".parse::<DateTime<Utc>>().unwrap());
    }

    #[test]
    fn changed_paths_map_to_their_transcript() {
        let base = PathBuf::from("/home/u/.claude/projects");
        let provider = ClaudeCodeProvider::with_base_dir(base.clone());
        let project = base.join("-home-u-repo");
        let transcript = project.join("abc.jsonl");
        assert_eq!(provider.session_file_for(&transcript), Some(transcript.clone()));
        assert_eq!(provider.session_file_for(&project.join("abc").join(CUSTOM_TITLE_SIDECAR)), Some(transcript));
        assert_eq!(provider.session_file_for(&project.join("memory/notes.md")), None);
        assert_eq!(provider.session_file_for(&project.join("abc/subagents/agent-1.jsonl")), None);
    }

    #[test]
    fn renamed_session_uses_its_custom_title_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            dir.path(),
            "renamed",
            &[
                r#"{"type":"user","message":{"role":"user","content":"hi"},"timestamp":"2026-01-15T10:00:00Z"}"#,
                r#"{"type":"ai-title","aiTitle":"Generated title"}"#,
            ],
        );
        let sidecar_dir = dir.path().join("renamed");
        fs::create_dir_all(&sidecar_dir).unwrap();
        fs::write(sidecar_dir.join(CUSTOM_TITLE_SIDECAR), r#"{"customTitle":"Release prep\n"}"#).unwrap();
        assert_eq!(parse_session_file(&path).unwrap().summary, "Release prep");
    }

    #[test]
    fn sidechain_transcript_is_not_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(
            dir.path(),
            "agent",
            &[r#"{"isSidechain":true,"type":"user","message":{"role":"user","content":"sub task"},"timestamp":"2026-01-15T10:00:00Z"}"#],
        );
        assert!(parse_session_file(&path).is_none());
    }

    #[test]
    fn multibyte_title_under_the_limit_is_not_marked_truncated() {
        let title = "日本語のタイトル".repeat(5); // 40 chars, 120 bytes
        assert_eq!(truncate_summary(&title), title);
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

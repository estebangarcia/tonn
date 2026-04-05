use nex_common::OutputClass;
use regex::Regex;
use std::sync::LazyLock;

/// Number of leading lines inspected for classification heuristics.
const CLASSIFY_HEAD_LINES: usize = 20;

// ── Git diff ──────────────────────────────────────────────────────────────────
static RE_DIFF_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"diff --git").unwrap());
static RE_HUNK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^@@\s").unwrap());

// ── Compiler output ───────────────────────────────────────────────────────────
static RE_RUSTC_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"error\[E\d{4}\]").unwrap());
static RE_GCC_DIAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+:\d+:\s*(error|warning)").unwrap());

// ── Test results ──────────────────────────────────────────────────────────────
static RE_TEST_RESULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"test result:").unwrap());
static RE_TESTS_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Tests:").unwrap());
static RE_PASSED_FAILED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"passed.*failed").unwrap());

// ── Log output ────────────────────────────────────────────────────────────────
static RE_TIMESTAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap());
static RE_LOG_LEVEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(DEBUG|INFO|WARN|ERROR)\b").unwrap());

// ── ls directory ──────────────────────────────────────────────────────────────
static RE_LS_TOTAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^total\s+\d+").unwrap());

// ── Error message ─────────────────────────────────────────────────────────────
static RE_ERROR_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(Error|fatal|panic|Traceback)").unwrap());

// ── Grep result ───────────────────────────────────────────────────────────────
static RE_GREP_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\w+[./]\w+:\d+:").unwrap());
static RE_GREP_CMD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^rg\s|\bgrep\b|\brg\b)").unwrap());

// ── Git commands ──────────────────────────────────────────────────────────────
static RE_GIT_LOG_CMD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"git\s+log").unwrap());
static RE_GIT_STATUS_CMD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"git\s+status").unwrap());

/// Classify command output into a domain category.
///
/// Inspects the first [`CLASSIFY_HEAD_LINES`] of `stripped_output` plus the
/// originating `command` string, returning the best-matching [`OutputClass`].
pub fn classify(command: &str, stripped_output: &str) -> OutputClass {
    let head: Vec<&str> = stripped_output
        .lines()
        .take(CLASSIFY_HEAD_LINES)
        .collect();

    // 1. Git diff
    for line in &head {
        if RE_DIFF_HEADER.is_match(line) || RE_HUNK_HEADER.is_match(line) {
            return OutputClass::GitDiff;
        }
    }

    // 2. Compiler output
    for line in &head {
        if RE_RUSTC_ERROR.is_match(line) || RE_GCC_DIAG.is_match(line) {
            return OutputClass::CompileOutput;
        }
    }

    // 3. Test results
    for line in &head {
        if RE_TEST_RESULT.is_match(line)
            || RE_TESTS_HEADER.is_match(line)
            || RE_PASSED_FAILED.is_match(line)
        {
            return OutputClass::TestResult;
        }
    }

    // 4. Log output
    for line in &head {
        if RE_TIMESTAMP.is_match(line) || RE_LOG_LEVEL.is_match(line) {
            return OutputClass::LogOutput;
        }
    }

    // 5. ls directory
    for line in &head {
        if RE_LS_TOTAL.is_match(line) {
            return OutputClass::LsDirectory;
        }
    }

    // 6. JSON — first non-empty line starts with { or [
    if let Some(first) = head.iter().find(|l| !l.trim().is_empty()) {
        let trimmed = first.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return OutputClass::JsonOutput;
        }
    }

    // 7. Error message
    for line in &head {
        if RE_ERROR_START.is_match(line) {
            return OutputClass::ErrorMessage;
        }
    }

    // 8. Grep result (output pattern OR command name)
    let grep_by_output = head.iter().any(|l| RE_GREP_LINE.is_match(l));
    let grep_by_command = RE_GREP_CMD.is_match(command);
    if grep_by_output || grep_by_command {
        return OutputClass::GrepResult;
    }

    // 9. git log (command)
    if RE_GIT_LOG_CMD.is_match(command) {
        return OutputClass::GitLog;
    }

    // 10. git status (command)
    if RE_GIT_STATUS_CMD.is_match(command) {
        return OutputClass::GitStatus;
    }

    // 11. Fallback
    OutputClass::Plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_diff_header() {
        let out = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs";
        assert_eq!(classify("git diff", out), OutputClass::GitDiff);
    }

    #[test]
    fn test_git_diff_hunk() {
        let out = "@@ -1,3 +1,4 @@\n context\n+added";
        assert_eq!(classify("git diff HEAD~1", out), OutputClass::GitDiff);
    }

    #[test]
    fn test_compile_rustc_error() {
        let out = "error[E0308]: mismatched types\n --> src/main.rs:5:14";
        assert_eq!(classify("cargo build", out), OutputClass::CompileOutput);
    }

    #[test]
    fn test_compile_gcc_warning() {
        let out = "main.c:10:5: warning: unused variable 'x'";
        assert_eq!(classify("gcc main.c", out), OutputClass::CompileOutput);
    }

    #[test]
    fn test_test_result() {
        let out = "test result: ok. 42 passed; 0 failed; 0 ignored";
        assert_eq!(classify("cargo test", out), OutputClass::TestResult);
    }

    #[test]
    fn test_test_jest_style() {
        let out = "Tests: 3 passed, 1 failed\nTime: 2.4s";
        assert_eq!(classify("npm test", out), OutputClass::TestResult);
    }

    #[test]
    fn test_passed_failed_pattern() {
        let out = "5 passed 2 failed";
        assert_eq!(classify("pytest", out), OutputClass::TestResult);
    }

    #[test]
    fn test_log_output_timestamp() {
        let out = "2026-04-05T10:00:00Z INFO starting up\n2026-04-05T10:00:01Z DEBUG init done";
        assert_eq!(classify("journalctl", out), OutputClass::LogOutput);
    }

    #[test]
    fn test_log_output_level_keyword() {
        let out = "some prefix WARN something went wrong";
        assert_eq!(classify("tail -f app.log", out), OutputClass::LogOutput);
    }

    #[test]
    fn test_ls_directory() {
        let out = "total 48\ndrwxr-xr-x 5 user staff 160 Apr 5 10:00 .";
        assert_eq!(classify("ls -la", out), OutputClass::LsDirectory);
    }

    #[test]
    fn test_json_object() {
        let out = "{\"key\": \"value\"}";
        assert_eq!(classify("curl api.example.com", out), OutputClass::JsonOutput);
    }

    #[test]
    fn test_json_array() {
        let out = "[1, 2, 3]";
        assert_eq!(classify("echo json", out), OutputClass::JsonOutput);
    }

    #[test]
    fn test_error_message() {
        let out = "Traceback (most recent call last):\n  File \"main.py\"";
        assert_eq!(classify("python main.py", out), OutputClass::ErrorMessage);
    }

    #[test]
    fn test_grep_result_by_output() {
        let out = "src/main.rs:10: fn main() {\nsrc/lib.rs:1: pub mod foo;";
        assert_eq!(classify("find-stuff", out), OutputClass::GrepResult);
    }

    #[test]
    fn test_grep_result_by_command() {
        let out = "something\nwith no grep pattern";
        assert_eq!(classify("grep -rn TODO .", out), OutputClass::GrepResult);
    }

    #[test]
    fn test_rg_command_start() {
        assert_eq!(classify("rg pattern", "no matches"), OutputClass::GrepResult);
    }

    #[test]
    fn test_git_log_command() {
        let out = "commit abc123\nAuthor: dev\nDate: Mon Apr 5";
        assert_eq!(classify("git log --oneline", out), OutputClass::GitLog);
    }

    #[test]
    fn test_git_status_command() {
        let out = "On branch main\nnothing to commit";
        assert_eq!(classify("git status", out), OutputClass::GitStatus);
    }

    #[test]
    fn test_plain_fallback() {
        let out = "hello world\njust some text";
        assert_eq!(classify("echo hello", out), OutputClass::Plain);
    }

    #[test]
    fn test_empty_output() {
        assert_eq!(classify("true", ""), OutputClass::Plain);
    }

    #[test]
    fn test_priority_diff_over_error() {
        // "fatal" appears but diff header takes priority
        let out = "diff --git a/f b/f\nfatal: something";
        assert_eq!(classify("git diff", out), OutputClass::GitDiff);
    }

    #[test]
    fn test_priority_compile_over_grep() {
        // gcc diagnostic line also matches grep file:line pattern,
        // but compile has higher priority
        let out = "main.c:10:5: error: undeclared identifier";
        assert_eq!(classify("make", out), OutputClass::CompileOutput);
    }
}

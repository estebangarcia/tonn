use nex_common::{CompressedOutput, OutputClass};
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Maximum commit summary lines kept for git-log output.
const GIT_LOG_MAX_COMMITS: usize = 15;
/// Maximum key lines retained for test failures, compiler diagnostics, etc.
const MAX_KEY_LINES: usize = 20;
/// Maximum files shown in grep summaries.
const MAX_GREP_FILES: usize = 20;
/// Maximum matches shown per file in grep output.
const GREP_MATCHES_PER_FILE: usize = 3;
/// Threshold (in bytes) above which JSON parsing is skipped.
const JSON_MAX_BYTES: usize = 100_000;
/// Line count threshold below which plain output is kept verbatim.
const PLAIN_SHORT_THRESHOLD: usize = 50;
/// Lines kept from the head of long plain output.
const PLAIN_HEAD_LINES: usize = 10;
/// Lines kept from the tail of long plain output.
const PLAIN_TAIL_LINES: usize = 10;

// ── Regex patterns ────────────────────────────────────────────────────────────
static RE_DIFF_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"diff --git a/(.*) b/(.*)").unwrap());
static RE_DIFF_STAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+) files? changed(?:, (\d+) insertions?...)?(?:, (\d+) deletions?...)?")
        .unwrap()
});

static RE_PASSED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+) passed").unwrap());
static RE_FAILED_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+) failed").unwrap());
static RE_SKIPPED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+) (skipped|ignored)").unwrap());

static RE_COMPILE_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+:\d+:\s*error").unwrap());
static RE_COMPILE_WARNING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+:\d+:\s*warning").unwrap());
static RE_COMPILE_DIAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\S+:\d+:\d+:\s*(error|warning):.*").unwrap());

static RE_LOG_TIMESTAMP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}\S*\s*").unwrap());

static RE_GREP_FILE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^:]+):(\d+):(.*)$").unwrap());

/// Compute compression ratio from original text and compressed key lines.
fn compute_ratio(original: &str, key_lines: &[String]) -> f32 {
    let orig_tokens = original.len().max(1);
    let compressed_tokens: usize = key_lines.iter().map(|l| l.len()).sum();
    1.0 - (compressed_tokens as f32 / orig_tokens as f32)
}

/// Compress command output using the appropriate domain-specific strategy.
pub fn compress(class: &OutputClass, command: &str, stripped: &str) -> CompressedOutput {
    match class {
        OutputClass::GitDiff => compress_git_diff(stripped),
        OutputClass::TestResult => compress_test_result(stripped),
        OutputClass::CompileOutput => compress_compile(stripped),
        OutputClass::LogOutput => compress_log(stripped),
        OutputClass::GitLog => compress_git_log(stripped),
        OutputClass::GitStatus => compress_git_status(stripped),
        OutputClass::GrepResult => compress_grep(stripped),
        OutputClass::JsonOutput => compress_json(stripped),
        OutputClass::LsDirectory => compress_ls(stripped),
        OutputClass::ErrorMessage => compress_error(stripped),
        OutputClass::Plain | OutputClass::Interactive | OutputClass::Unknown => {
            compress_plain(command, stripped)
        }
    }
}

// ── Individual compressors ────────────────────────────────────────────────────

fn compress_git_diff(stripped: &str) -> CompressedOutput {
    let mut files: Vec<String> = Vec::new();
    let mut additions: usize = 0;
    let mut deletions: usize = 0;

    for line in stripped.lines() {
        if let Some(caps) = RE_DIFF_FILE.captures(line) {
            if let Some(name) = caps.get(2) {
                files.push(name.as_str().to_string());
            }
        } else if line.starts_with('+') && !line.starts_with("+++") {
            additions += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            deletions += 1;
        }
    }

    // Try to use the stat line if present
    if let Some(caps) = RE_DIFF_STAT.captures(stripped) {
        if let Some(ins) = caps.get(2)
            && let Ok(n) = ins.as_str().parse::<usize>() {
                additions = n;
            }
        if let Some(del) = caps.get(3)
            && let Ok(n) = del.as_str().parse::<usize>() {
                deletions = n;
            }
    }

    let summary = format!("diff: {} files, +{} -{}", files.len(), additions, deletions);
    let key_lines = files;
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_test_result(stripped: &str) -> CompressedOutput {
    let passed: usize = RE_PASSED
        .captures(stripped)
        .and_then(|c| c.get(1)?.as_str().parse().ok())
        .unwrap_or(0);
    let failed: usize = RE_FAILED_COUNT
        .captures(stripped)
        .and_then(|c| c.get(1)?.as_str().parse().ok())
        .unwrap_or(0);
    let skipped: usize = RE_SKIPPED
        .captures(stripped)
        .and_then(|c| c.get(1)?.as_str().parse().ok())
        .unwrap_or(0);

    let key_lines: Vec<String> = stripped
        .lines()
        .filter(|l| l.contains("FAIL") || l.contains("FAILED"))
        .take(MAX_KEY_LINES)
        .map(|l| l.to_string())
        .collect();

    let summary = format!("tests: {} passed, {} failed, {} skipped", passed, failed, skipped);
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_compile(stripped: &str) -> CompressedOutput {
    let mut errors: usize = 0;
    let mut warnings: usize = 0;
    let mut key_lines: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in stripped.lines() {
        if RE_COMPILE_ERROR.is_match(line) {
            errors += 1;
        }
        if RE_COMPILE_WARNING.is_match(line) {
            warnings += 1;
        }
        if let Some(m) = RE_COMPILE_DIAG.find(line) {
            let diag = m.as_str().to_string();
            if seen.insert(diag.clone()) && key_lines.len() < MAX_KEY_LINES {
                key_lines.push(diag);
            }
        }
    }

    let summary = format!("compile: {} errors, {} warnings", errors, warnings);
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_log(stripped: &str) -> CompressedOutput {
    let total_lines = stripped.lines().count();
    let mut groups: Vec<(String, usize)> = Vec::new();

    for line in stripped.lines() {
        let msg = RE_LOG_TIMESTAMP.replace(line, "").to_string();
        if let Some(last) = groups.last_mut()
            && last.0 == msg {
                last.1 += 1;
                continue;
            }
        groups.push((msg, 1));
    }

    let unique_count = groups.len();
    let key_lines: Vec<String> = groups
        .into_iter()
        .take(MAX_KEY_LINES)
        .map(|(msg, count)| {
            if count > 1 {
                format!("{} (×{})", msg, count)
            } else {
                msg
            }
        })
        .collect();

    let summary = format!("log: {} lines, {} unique messages", total_lines, unique_count);
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_git_log(stripped: &str) -> CompressedOutput {
    let key_lines: Vec<String> = stripped
        .lines()
        .take(GIT_LOG_MAX_COMMITS)
        .map(|l| l.to_string())
        .collect();

    let summary = format!("git log: {} commits shown", key_lines.len());
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_git_status(stripped: &str) -> CompressedOutput {
    let mut modified: usize = 0;
    let mut untracked: usize = 0;
    let mut staged: usize = 0;
    let mut key_lines: Vec<String> = Vec::new();

    for line in stripped.lines() {
        if line.len() < 2 {
            continue;
        }
        let prefix = line.get(..2).unwrap_or(line);
        let file = line.get(2..).unwrap_or("").trim();
        match prefix {
            " M" | "M " | "MM" => {
                modified += 1;
                key_lines.push(format!("modified: {}", file));
            }
            "A " | "AM" => {
                staged += 1;
                key_lines.push(format!("added: {}", file));
            }
            "??" => {
                untracked += 1;
                key_lines.push(format!("untracked: {}", file));
            }
            "D " | " D" => {
                key_lines.push(format!("deleted: {}", file));
            }
            _ => {
                // Other status codes (renamed, copied, etc.) count as staged
                if prefix.starts_with(|c: char| c.is_ascii_uppercase()) {
                    staged += 1;
                    key_lines.push(format!("{}: {}", prefix.trim(), file));
                }
            }
        }
    }

    let summary = format!(
        "git status: {} modified, {} untracked, {} staged",
        modified, untracked, staged
    );
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_grep(stripped: &str) -> CompressedOutput {
    let mut files: HashMap<String, Vec<String>> = HashMap::new();
    let mut file_order: Vec<String> = Vec::new();
    let mut total_matches: usize = 0;

    for line in stripped.lines() {
        if let Some(caps) = RE_GREP_FILE_LINE.captures(line) {
            let file = caps[1].to_string();
            total_matches += 1;
            let entry = files.entry(file.clone()).or_default();
            if entry.len() < GREP_MATCHES_PER_FILE {
                entry.push(line.to_string());
            }
            if !file_order.contains(&file) {
                file_order.push(file);
            }
        }
    }

    let file_count = file_order.len();
    let mut key_lines: Vec<String> = Vec::new();

    for file in file_order.into_iter().take(MAX_GREP_FILES) {
        let match_count = files.get(&file).map_or(0, |v| v.len());
        key_lines.push(format!("{}: ({} matches)", file, match_count));
        if let Some(matches) = files.get(&file) {
            for m in matches {
                key_lines.push(format!("  {}", m));
            }
        }
    }

    let summary = format!("grep: {} matches in {} files", total_matches, file_count);
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_json(stripped: &str) -> CompressedOutput {
    if stripped.len() > JSON_MAX_BYTES {
        return compress_plain("", stripped);
    }

    match serde_json::from_str::<serde_json::Value>(stripped) {
        Ok(serde_json::Value::Object(map)) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            let summary = format!("json: object with {} keys", keys.len());
            let ratio = compute_ratio(stripped, &keys);
            CompressedOutput {
                summary,
                key_lines: keys,
                compression_ratio: ratio,
            }
        }
        Ok(serde_json::Value::Array(arr)) => {
            let mut key_lines: Vec<String> = Vec::new();
            if let Some(first) = arr.first() {
                key_lines.push(format!("first element: {}", first));
            }
            let summary = format!("json: array of {} elements", arr.len());
            let ratio = compute_ratio(stripped, &key_lines);
            CompressedOutput {
                summary,
                key_lines,
                compression_ratio: ratio,
            }
        }
        _ => compress_plain("", stripped),
    }
}

fn compress_ls(stripped: &str) -> CompressedOutput {
    let lines: Vec<&str> = stripped.lines().collect();
    // Skip the "total N" header if present
    let entries: Vec<&str> = if lines.first().is_some_and(|l| l.starts_with("total ")) {
        lines[1..].to_vec()
    } else {
        lines.clone()
    };

    let count = entries.len();
    let mut ext_groups: HashMap<String, usize> = HashMap::new();

    for entry in &entries {
        // Grab the last whitespace-separated token as the filename
        if let Some(name) = entry.split_whitespace().last() {
            let ext = if let Some(pos) = name.rfind('.') {
                name[pos..].to_string()
            } else {
                "(no ext)".to_string()
            };
            *ext_groups.entry(ext).or_default() += 1;
        }
    }

    let key_lines: Vec<String> = ext_groups
        .iter()
        .map(|(ext, n)| format!("{}: {} files", ext, n))
        .collect();

    let summary = format!("ls: {} entries", count);
    let ratio = compute_ratio(stripped, &key_lines);

    CompressedOutput {
        summary,
        key_lines,
        compression_ratio: ratio,
    }
}

fn compress_error(stripped: &str) -> CompressedOutput {
    let first_line = stripped.lines().next().unwrap_or("").to_string();
    let key_lines: Vec<String> = stripped.lines().map(|l| l.to_string()).collect();

    CompressedOutput {
        summary: first_line,
        key_lines,
        compression_ratio: 0.0,
    }
}

fn compress_plain(_command: &str, stripped: &str) -> CompressedOutput {
    let lines: Vec<&str> = stripped.lines().collect();
    let total = lines.len();

    if total <= PLAIN_SHORT_THRESHOLD {
        let key_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        CompressedOutput {
            summary: format!("output: {} lines", total),
            key_lines,
            compression_ratio: 0.0,
        }
    } else {
        let mut key_lines: Vec<String> = Vec::new();
        for line in lines.iter().take(PLAIN_HEAD_LINES) {
            key_lines.push(line.to_string());
        }
        let omitted = total - PLAIN_HEAD_LINES - PLAIN_TAIL_LINES;
        key_lines.push(format!("... ({} lines omitted) ...", omitted));
        for line in lines.iter().skip(total - PLAIN_TAIL_LINES) {
            key_lines.push(line.to_string());
        }

        let summary = format!("output: {} lines", total);
        let ratio = compute_ratio(stripped, &key_lines);

        CompressedOutput {
            summary,
            key_lines,
            compression_ratio: ratio,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_git_diff() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
+    println!(\"hello\");
 }";
        let result = compress(&OutputClass::GitDiff, "git diff", diff);
        assert_eq!(result.summary, "diff: 1 files, +1 -0");
        assert_eq!(result.key_lines, vec!["src/main.rs"]);
        assert!(result.compression_ratio > 0.0);
    }

    #[test]
    fn test_compress_test_result() {
        let out = "\
running 5 tests
test foo ... ok
test bar ... FAILED
test baz ... ok
test result: 3 passed; 1 failed; 1 ignored";
        let result = compress(&OutputClass::TestResult, "cargo test", out);
        assert_eq!(result.summary, "tests: 3 passed, 1 failed, 1 skipped");
        assert_eq!(result.key_lines.len(), 1); // one FAILED line
    }

    #[test]
    fn test_compress_compile() {
        let out = "\
main.c:10:5: error: undeclared identifier 'x'
main.c:15:3: warning: unused variable 'y'
main.c:10:5: error: undeclared identifier 'x'";
        let result = compress(&OutputClass::CompileOutput, "gcc main.c", out);
        assert_eq!(result.summary, "compile: 2 errors, 1 warnings");
        // Only unique diagnostics
        assert_eq!(result.key_lines.len(), 2);
    }

    #[test]
    fn test_compress_log() {
        let out = "\
2026-04-05T10:00:00Z INFO starting
2026-04-05T10:00:01Z INFO starting
2026-04-05T10:00:02Z WARN disk full";
        let result = compress(&OutputClass::LogOutput, "journalctl", out);
        assert!(result.summary.contains("3 lines"));
        assert!(result.summary.contains("2 unique"));
        // Repeated message should be grouped
        assert!(result.key_lines[0].contains("×2"));
    }

    #[test]
    fn test_compress_git_log() {
        let out = "abc1234 first commit\ndef5678 second commit\nghi9012 third commit";
        let result = compress(&OutputClass::GitLog, "git log --oneline", out);
        assert_eq!(result.summary, "git log: 3 commits shown");
        assert_eq!(result.key_lines.len(), 3);
    }

    #[test]
    fn test_compress_git_status() {
        let out = " M src/lib.rs\n?? new_file.txt\nA  staged.rs";
        let result = compress(&OutputClass::GitStatus, "git status -s", out);
        assert!(result.summary.contains("1 modified"));
        assert!(result.summary.contains("1 untracked"));
        assert!(result.summary.contains("1 staged"));
    }

    #[test]
    fn test_compress_grep() {
        let out = "src/main.rs:10:fn main() {\nsrc/main.rs:20:    println!();\nsrc/lib.rs:1:pub mod foo;";
        let result = compress(&OutputClass::GrepResult, "rg pattern", out);
        assert_eq!(result.summary, "grep: 3 matches in 2 files");
    }

    #[test]
    fn test_compress_json_object() {
        let out = "{\"name\": \"test\", \"version\": \"1.0\"}";
        let result = compress(&OutputClass::JsonOutput, "cat data.json", out);
        assert_eq!(result.summary, "json: object with 2 keys");
    }

    #[test]
    fn test_compress_json_array() {
        let out = "[1, 2, 3, 4, 5]";
        let result = compress(&OutputClass::JsonOutput, "cat data.json", out);
        assert_eq!(result.summary, "json: array of 5 elements");
    }

    #[test]
    fn test_compress_error() {
        let out = "Traceback (most recent call last):\n  File \"main.py\", line 1";
        let result = compress(&OutputClass::ErrorMessage, "python main.py", out);
        assert_eq!(result.summary, "Traceback (most recent call last):");
        assert_eq!(result.compression_ratio, 0.0);
    }

    #[test]
    fn test_compress_plain_short() {
        let out = "line 1\nline 2\nline 3";
        let result = compress(&OutputClass::Plain, "echo", out);
        assert_eq!(result.summary, "output: 3 lines");
        assert_eq!(result.compression_ratio, 0.0);
        assert_eq!(result.key_lines.len(), 3);
    }

    #[test]
    fn test_compress_plain_long() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        let out = lines.join("\n");
        let result = compress(&OutputClass::Plain, "seq 100", &out);
        assert_eq!(result.summary, "output: 100 lines");
        assert!(result.compression_ratio > 0.0);
        // head(10) + omitted marker + tail(10)
        assert_eq!(result.key_lines.len(), 21);
        assert!(result.key_lines[10].contains("80 lines omitted"));
    }

    #[test]
    fn test_compress_empty_input() {
        let result = compress(&OutputClass::Plain, "true", "");
        assert_eq!(result.summary, "output: 0 lines");
        assert_eq!(result.compression_ratio, 0.0);
    }
}

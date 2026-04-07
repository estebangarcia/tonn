use serde::{Deserialize, Serialize};

/// Output classification for domain-specific compression.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum OutputClass {
    GitDiff,
    GitLog,
    GitStatus,
    TestResult,
    CompileOutput,
    LogOutput,
    LsDirectory,
    JsonOutput,
    GrepResult,
    ErrorMessage,
    Interactive,
    Plain,
    #[default]
    Unknown,
}

/// Compressed output for token savings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedOutput {
    pub summary: String,
    pub key_lines: Vec<String>,
    pub compression_ratio: f32,
}

impl Default for CompressedOutput {
    fn default() -> Self {
        Self {
            summary: String::new(),
            key_lines: Vec::new(),
            compression_ratio: 0.0,
        }
    }
}

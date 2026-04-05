//! Block model for Nexterm: command-output pairs with compression tiers.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nex_common::{BlockId, PaneId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A Block represents one command execution cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub pane_id: PaneId,
    pub sequence: u64,

    // Content
    pub prompt: String,
    pub command: String,
    pub output: BlockOutput,

    // Metadata
    pub exit_code: Option<i32>,
    pub cwd: PathBuf,
    pub duration: Option<Duration>,
    pub timestamp: DateTime<Utc>,

    // AI fields
    pub classification: OutputClass,
}

/// Multi-tier output representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockOutput {
    pub stripped_text: String,
    pub compressed: CompressedOutput,
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

/// Output classification for domain-specific compression.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OutputClass {
    GitDiff,
    GitLog,
    GitStatus,
    TestResult,
    CompileOutput,
    LogOutput,
    LsDirectory,
    JsonOutput,
    ErrorMessage,
    Interactive,
    Plain,
    Unknown,
}

impl Default for OutputClass {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Thread-safe block store.
pub struct BlockStore {
    blocks: DashMap<BlockId, Arc<Block>>,
    pane_index: DashMap<PaneId, Vec<BlockId>>,
    next_sequence: AtomicU64,
}

impl BlockStore {
    pub fn new() -> Self {
        Self {
            blocks: DashMap::new(),
            pane_index: DashMap::new(),
            next_sequence: AtomicU64::new(0),
        }
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence.fetch_add(1, Ordering::Relaxed)
    }

    pub fn insert(&self, block: Block) {
        let id = block.id;
        let pane_id = block.pane_id;
        self.blocks.insert(id, Arc::new(block));
        self.pane_index.entry(pane_id).or_default().push(id);
    }

    pub fn get(&self, id: &BlockId) -> Option<Arc<Block>> {
        self.blocks.get(id).map(|b| Arc::clone(&b))
    }

    pub fn get_recent(&self, pane_id: &PaneId, count: usize) -> Vec<Arc<Block>> {
        self.pane_index
            .get(pane_id)
            .map(|ids| {
                ids.iter()
                    .rev()
                    .take(count)
                    .filter_map(|id| self.blocks.get(id).map(|b| Arc::clone(&b)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for BlockStore {
    fn default() -> Self {
        Self::new()
    }
}

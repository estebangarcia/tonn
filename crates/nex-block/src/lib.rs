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

// ---------------------------------------------------------------------------
// BlockBuilder — assembles Blocks from a stream of BlockEvents
// ---------------------------------------------------------------------------

use crossbeam_channel::Receiver;
use nex_ipc::BlockEvent;
use std::collections::HashMap;
use std::time::Instant;

/// Per-pane state machine that assembles a Block from BlockEvents.
struct BlockBuilder {
    pane_id: PaneId,
    state: BuilderState,
    output_bytes: Vec<u8>,
    start_time: Option<Instant>,
    cwd: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuilderState {
    Idle,
    PromptActive,
    CommandInput,
    Executing,
}

impl BlockBuilder {
    fn new(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            state: BuilderState::Idle,
            output_bytes: Vec::new(),
            start_time: None,
            cwd: PathBuf::new(),
        }
    }

    /// Process a BlockEvent, returning a completed Block if one was sealed.
    fn process(&mut self, event: BlockEvent, store: &BlockStore) -> Option<Block> {
        match event {
            BlockEvent::PromptStart { .. } => {
                self.state = BuilderState::PromptActive;
                self.output_bytes.clear();
                None
            }
            BlockEvent::CommandStart { .. } => {
                self.state = BuilderState::CommandInput;
                None
            }
            BlockEvent::ExecutionStart { .. } => {
                self.start_time = Some(Instant::now());
                self.state = BuilderState::Executing;
                self.output_bytes.clear();
                None
            }
            BlockEvent::Output { data, .. } => {
                if self.state == BuilderState::Executing {
                    self.output_bytes.extend_from_slice(&data);
                }
                None
            }
            BlockEvent::CommandFinished { exit_code, .. } => {
                let duration = self.start_time.map(|t| t.elapsed());
                let raw_output = String::from_utf8_lossy(&self.output_bytes);
                let stripped = nex_token_save::strip_ansi(&raw_output);

                let block = Block {
                    id: BlockId::new(),
                    pane_id: self.pane_id,
                    sequence: store.next_sequence(),
                    prompt: String::new(),
                    command: String::new(), // TODO: capture from CommandInput state
                    output: BlockOutput {
                        stripped_text: stripped,
                        compressed: CompressedOutput::default(),
                    },
                    exit_code: Some(exit_code),
                    cwd: self.cwd.clone(),
                    duration,
                    timestamp: Utc::now(),
                    classification: OutputClass::Unknown,
                };

                self.state = BuilderState::Idle;
                self.output_bytes.clear();
                self.start_time = None;
                Some(block)
            }
            BlockEvent::CwdChanged { cwd, .. } => {
                self.cwd = cwd;
                None
            }
        }
    }
}

/// Run the block processor thread. Consumes BlockEvents and populates the BlockStore.
/// Call this from a dedicated thread.
pub fn block_processor_thread(rx: Receiver<BlockEvent>, store: Arc<BlockStore>) {
    let mut builders: HashMap<PaneId, BlockBuilder> = HashMap::new();

    while let Ok(event) = rx.recv() {
        let pane_id = match &event {
            BlockEvent::PromptStart { pane_id }
            | BlockEvent::CommandStart { pane_id }
            | BlockEvent::ExecutionStart { pane_id, .. }
            | BlockEvent::CommandFinished { pane_id, .. }
            | BlockEvent::Output { pane_id, .. }
            | BlockEvent::CwdChanged { pane_id, .. } => *pane_id,
        };

        let builder = builders
            .entry(pane_id)
            .or_insert_with(|| BlockBuilder::new(pane_id));

        if let Some(block) = builder.process(event, &store) {
            tracing::info!(
                pane_id = %block.pane_id,
                exit_code = ?block.exit_code,
                duration = ?block.duration,
                output_len = block.output.stripped_text.len(),
                "Block sealed"
            );
            store.insert(block);
        }
    }

    tracing::info!("Block processor thread exiting");
}

//! Block model for Tonn: command-output pairs with compression tiers.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use nex_common::{BlockId, CompressedOutput, OutputClass, PaneId};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The store keeps at most this much block output (stripped text plus
/// compressed tiers); the oldest blocks are evicted first.
const MAX_RETAINED_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
/// …and at most this many blocks, so many tiny blocks stay bounded too.
const MAX_RETAINED_BLOCKS: usize = 10_000;

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
    pub token_estimate: usize,
}

/// Multi-tier output representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockOutput {
    pub stripped_text: String,
    pub compressed: CompressedOutput,
    /// Bytes of (stripped) output dropped from the middle of a very long
    /// command; `stripped_text` and the compressed tiers cover the rest.
    #[serde(default)]
    pub omitted_bytes: usize,
}

impl Block {
    /// Approximate heap bytes this block keeps alive, for the store's budget.
    fn retained_bytes(&self) -> usize {
        self.output.stripped_text.len()
            + self.output.compressed.summary.len()
            + self.output.compressed.key_lines.iter().map(String::len).sum::<usize>()
            + self.command.len()
    }
}

/// Thread-safe block store, bounded: once over `MAX_RETAINED_OUTPUT_BYTES`
/// or `MAX_RETAINED_BLOCKS` the oldest blocks are evicted. It used to keep
/// every command's full output forever.
pub struct BlockStore {
    blocks: DashMap<BlockId, Arc<Block>>,
    pane_index: DashMap<PaneId, Vec<BlockId>>,
    /// Blocks ever sealed per pane, evicted ones included — a progress counter.
    sealed_counts: DashMap<PaneId, usize>,
    retention: Mutex<Retention>,
    max_retained_bytes: usize,
    max_retained_blocks: usize,
    next_sequence: AtomicU64,
}

/// Stored blocks oldest-first with their sizes, for eviction.
#[derive(Default)]
struct Retention {
    order: VecDeque<(BlockId, PaneId, usize)>,
    bytes: usize,
}

impl BlockStore {
    pub fn new() -> Self {
        Self::with_limits(MAX_RETAINED_OUTPUT_BYTES, MAX_RETAINED_BLOCKS)
    }

    fn with_limits(max_retained_bytes: usize, max_retained_blocks: usize) -> Self {
        Self {
            blocks: DashMap::new(),
            pane_index: DashMap::new(),
            sealed_counts: DashMap::new(),
            retention: Mutex::new(Retention::default()),
            max_retained_bytes,
            max_retained_blocks,
            next_sequence: AtomicU64::new(0),
        }
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence.fetch_add(1, Ordering::Relaxed)
    }

    pub fn insert(&self, block: Block) {
        let id = block.id;
        let pane_id = block.pane_id;
        let size = block.retained_bytes();
        self.blocks.insert(id, Arc::new(block));
        self.pane_index.entry(pane_id).or_default().push(id);
        *self.sealed_counts.entry(pane_id).or_default() += 1;

        let mut retention = self.retention.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        retention.order.push_back((id, pane_id, size));
        retention.bytes += size;
        while retention.bytes > self.max_retained_bytes
            || retention.order.len() > self.max_retained_blocks
        {
            let Some((old_id, old_pane, old_size)) = retention.order.pop_front() else { break };
            retention.bytes -= old_size;
            self.evict(old_id, old_pane);
        }
    }

    fn evict(&self, id: BlockId, pane_id: PaneId) {
        self.blocks.remove(&id);
        if let Some(mut ids) = self.pane_index.get_mut(&pane_id) {
            // Oldest-first eviction → it's the pane's oldest entry.
            if ids.first() == Some(&id) {
                ids.remove(0);
            } else {
                ids.retain(|kept| *kept != id);
            }
        }
        self.pane_index.remove_if(&pane_id, |_, ids| ids.is_empty());
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

    /// Blocks ever sealed for a pane, including evicted ones (O(1)). Use it
    /// to detect that new blocks arrived; it never decreases.
    pub fn sealed_count_for_pane(&self, pane_id: &PaneId) -> usize {
        self.sealed_counts.get(pane_id).map(|count| *count).unwrap_or(0)
    }

    pub fn search(&self, pattern: &str, max_results: usize) -> Vec<Arc<Block>> {
        let re = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        self.blocks
            .iter()
            .filter(|e| re.is_match(&e.value().output.stripped_text))
            .take(max_results)
            .map(|e| Arc::clone(e.value()))
            .collect()
    }

    pub fn get_all_for_pane(&self, pane_id: &PaneId) -> Vec<Arc<Block>> {
        self.pane_index
            .get(pane_id)
            .map(|ids| {
                ids.iter()
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

#[cfg(test)]
mod tests {
    use super::*;
    use nex_common::{BlockId, CompressedOutput, OutputClass, PaneId};
    use std::path::PathBuf;

    fn make_block(pane_id: PaneId, command: &str, output: &str, store: &BlockStore) -> Block {
        Block {
            id: BlockId::new(),
            pane_id,
            sequence: store.next_sequence(),
            prompt: String::new(),
            command: command.to_string(),
            output: BlockOutput {
                stripped_text: output.to_string(),
                compressed: CompressedOutput {
                    summary: format!("ran {command}"),
                    key_lines: vec![],
                    compression_ratio: 1.0,
                },
                omitted_bytes: 0,
            },
            exit_code: Some(0),
            cwd: PathBuf::from("/tmp"),
            duration: Some(Duration::from_millis(100)),
            timestamp: Utc::now(),
            classification: OutputClass::Plain,
            token_estimate: output.len() / TOKEN_ESTIMATE_DIVISOR,
        }
    }

    #[test]
    fn store_evicts_oldest_blocks_over_budget() {
        let pane = PaneId::new();
        let block_size = make_block(pane, "x", "aaaa", &BlockStore::new()).retained_bytes();
        // Room for two blocks of this size.
        let store = BlockStore::with_limits(2 * block_size, MAX_RETAINED_BLOCKS);
        for output in ["aaaa", "bbbb", "cccc"] {
            store.insert(make_block(pane, "x", output, &store));
        }
        let kept: Vec<String> = store
            .get_all_for_pane(&pane)
            .iter()
            .map(|b| b.output.stripped_text.clone())
            .collect();
        assert_eq!(kept, ["bbbb", "cccc"], "oldest evicted, budget respected");
        assert_eq!(store.sealed_count_for_pane(&pane), 3, "progress counter never drops");
    }

    #[test]
    fn store_caps_block_count_and_clears_empty_panes() {
        let (old_pane, new_pane) = (PaneId::new(), PaneId::new());
        let store = BlockStore::with_limits(MAX_RETAINED_OUTPUT_BYTES, 2);
        store.insert(make_block(old_pane, "x", "a", &store));
        store.insert(make_block(new_pane, "x", "b", &store));
        store.insert(make_block(new_pane, "x", "c", &store));
        assert!(store.get_all_for_pane(&old_pane).is_empty());
        assert!(store.pane_index.get(&old_pane).is_none(), "empty pane entry removed");
        assert_eq!(store.get_all_for_pane(&new_pane).len(), 2);
    }

    #[test]
    fn running_command_output_keeps_head_and_tail() {
        let mut output = CappedOutput::default();
        let total = BLOCK_OUTPUT_HEAD_BYTES + BLOCK_OUTPUT_TAIL_BYTES + 5_000;
        for i in 0..total {
            output.push(&[b'a' + (i % 26) as u8]);
        }
        assert_eq!(output.head.len() + output.tail.len(), BLOCK_OUTPUT_HEAD_BYTES + BLOCK_OUTPUT_TAIL_BYTES);

        let (text, omitted) = output.finish();
        assert_eq!(omitted, 5_000);
        assert!(text.starts_with("abcdef"), "head kept");
        assert!(text.contains("[… 5000 bytes of output omitted …]"));
        let last = (b'a' + ((total - 1) % 26) as u8) as char;
        assert!(text.ends_with(last), "most recent output kept");
    }

    #[test]
    fn short_output_is_kept_whole_and_stripped() {
        let mut output = CappedOutput::default();
        output.push(b"hello \x1b[31m");
        output.push(b"world\x1b[0m\r\n");
        assert_eq!(output.finish(), ("hello world\n".to_string(), 0));
    }

    #[test]
    fn escape_sequence_across_the_cut_does_not_swallow_the_tail() {
        // The head fills up inside an OSC (e.g. a title or hyperlink) whose
        // terminator lands in the dropped middle. Cutting raw bytes left that
        // OSC open, and stripping then ate the marker and the entire tail.
        let mut output = CappedOutput::default();
        let mut head = vec![b'a'; BLOCK_OUTPUT_HEAD_BYTES - 2];
        head.extend_from_slice(b"\x1b]0;window title");
        output.push(&head);
        let mut middle = b"\x07".to_vec();
        middle.extend(std::iter::repeat_n(b'b', BLOCK_OUTPUT_TAIL_BYTES + 1_000));
        output.push(&middle);
        output.push(b"\nerror: the final diagnostic");

        let (text, omitted) = output.finish();
        assert!(omitted > 0);
        assert!(text.contains("bytes of output omitted"));
        assert!(text.ends_with("error: the final diagnostic"));
        assert!(!text.contains("window title"), "OSC payload stripped");
    }

    #[test]
    fn tail_never_starts_mid_character() {
        let mut output = CappedOutput::default();
        output.push(&vec![b'a'; BLOCK_OUTPUT_HEAD_BYTES]);
        for _ in 0..(BLOCK_OUTPUT_TAIL_BYTES / 3 + 10) {
            output.push("日".as_bytes());
        }
        let (text, _) = output.finish();
        assert!(!text.contains(char::REPLACEMENT_CHARACTER));
    }

    #[test]
    fn truncated_command_summary_is_marked_partial() {
        let pane = PaneId::new();
        let store = BlockStore::new();
        let mut builder = BlockBuilder::new(pane);
        builder.process(BlockEvent::ExecutionStart { pane_id: pane, command: "make".into() }, &store);
        let filler = vec![b'x'; BLOCK_OUTPUT_HEAD_BYTES + BLOCK_OUTPUT_TAIL_BYTES + 10];
        builder.process(BlockEvent::Output { pane_id: pane, data: filler }, &store);
        let block = builder
            .process(BlockEvent::CommandFinished { pane_id: pane, exit_code: 0 }, &store)
            .unwrap();

        assert_eq!(block.output.omitted_bytes, 10);
        assert!(block.output.compressed.summary.contains("[partial: 10 bytes of output omitted]"));
    }

    #[test]
    fn search_matches_output_regex() {
        let store = BlockStore::new();
        let pane = PaneId::new();
        store.insert(make_block(pane, "ls", "file1.txt\nfile2.rs", &store));
        store.insert(make_block(pane, "echo", "hello world", &store));

        let results = store.search(r"\.rs$", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].command, "ls");
    }

    #[test]
    fn search_invalid_regex_returns_empty() {
        let store = BlockStore::new();
        let pane = PaneId::new();
        store.insert(make_block(pane, "ls", "output", &store));

        let results = store.search(r"[invalid", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_max_results() {
        let store = BlockStore::new();
        let pane = PaneId::new();
        for i in 0..5 {
            store.insert(make_block(pane, &format!("cmd{i}"), "match_me", &store));
        }

        let results = store.search("match_me", 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn get_all_for_pane_returns_correct_blocks() {
        let store = BlockStore::new();
        let pane_a = PaneId::new();
        let pane_b = PaneId::new();
        store.insert(make_block(pane_a, "cmd1", "out1", &store));
        store.insert(make_block(pane_b, "cmd2", "out2", &store));
        store.insert(make_block(pane_a, "cmd3", "out3", &store));

        let blocks_a = store.get_all_for_pane(&pane_a);
        assert_eq!(blocks_a.len(), 2);
        assert!(blocks_a.iter().all(|b| b.pane_id == pane_a));

        let blocks_b = store.get_all_for_pane(&pane_b);
        assert_eq!(blocks_b.len(), 1);
        assert_eq!(blocks_b[0].command, "cmd2");
    }

    #[test]
    fn get_all_for_pane_unknown_returns_empty() {
        let store = BlockStore::new();
        let unknown = PaneId::new();
        assert!(store.get_all_for_pane(&unknown).is_empty());
    }

    #[test]
    fn get_recent_returns_most_recent_first() {
        let store = BlockStore::new();
        let pane = PaneId::new();
        store.insert(make_block(pane, "first", "out", &store));
        store.insert(make_block(pane, "second", "out", &store));
        store.insert(make_block(pane, "third", "out", &store));

        let recent = store.get_recent(&pane, 2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].command, "third");
        assert_eq!(recent[1].command, "second");
    }
}

// ---------------------------------------------------------------------------
// BlockBuilder — assembles Blocks from a stream of BlockEvents
// ---------------------------------------------------------------------------

use crossbeam_channel::Receiver;
use nex_ipc::BlockEvent;
use std::collections::HashMap;
use std::time::Instant;

const TOKEN_ESTIMATE_DIVISOR: usize = 4;

/// Output kept per running command, after ANSI stripping: its first
/// `BLOCK_OUTPUT_HEAD_BYTES` and its most recent `BLOCK_OUTPUT_TAIL_BYTES`.
/// A "command" can be a TUI (e.g. `claude`) streaming redraws for hours;
/// unbounded, this buffer grew to gigabytes.
const BLOCK_OUTPUT_HEAD_BYTES: usize = 64 * 1024;
const BLOCK_OUTPUT_TAIL_BYTES: usize = 960 * 1024;

/// A running command's output, ANSI-stripped as it arrives and capped to its
/// head and tail. Stripping *before* capping keeps escape-sequence state
/// across the dropped middle: cutting raw bytes could leave an unterminated
/// OSC in the head that swallowed the marker and the whole tail.
#[derive(Default)]
struct CappedOutput {
    stripper: nex_token_save::AnsiStripper,
    /// Scratch for one chunk's stripped text (reused).
    chunk: String,
    head: String,
    /// Set once text has gone to the tail; the head never grows after that.
    head_closed: bool,
    /// UTF-8 bytes, always starting on a character boundary.
    tail: VecDeque<u8>,
    omitted: usize,
}

impl CappedOutput {
    fn push(&mut self, data: &[u8]) {
        let Self { stripper, chunk, head, head_closed, tail, omitted } = self;
        chunk.clear();
        stripper.push_bytes(data, chunk);

        let mut text = chunk.as_str();
        if !*head_closed {
            let cut = floor_char_boundary(text, BLOCK_OUTPUT_HEAD_BYTES.saturating_sub(head.len()));
            head.push_str(&text[..cut]);
            text = &text[cut..];
            *head_closed = !text.is_empty();
        }
        tail.extend(text.as_bytes());

        let mut excess = tail.len().saturating_sub(BLOCK_OUTPUT_TAIL_BYTES);
        // Don't start the tail in the middle of a character.
        while tail.get(excess).is_some_and(|&byte| is_utf8_continuation(byte)) {
            excess += 1;
        }
        if excess > 0 {
            tail.drain(..excess);
            *omitted += excess;
        }
    }

    /// The stripped text (head, omission marker, tail) and how many bytes
    /// were dropped from the middle.
    fn finish(self) -> (String, usize) {
        let mut text = self.head;
        if self.omitted > 0 {
            text.push_str(&format!("\n[… {} bytes of output omitted …]\n", self.omitted));
        }
        let (front, back) = self.tail.as_slices();
        text.push_str(&String::from_utf8_lossy(&[front, back].concat()));
        (text, self.omitted)
    }
}

fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

/// The largest character boundary in `s` at or below `index`.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    (0..=index.min(s.len())).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
}

/// Label a summary computed from output whose middle was dropped, so counts
/// like "0 errors" aren't mistaken for complete.
fn partial_summary(summary: &str, omitted_bytes: usize) -> String {
    format!("{summary} [partial: {omitted_bytes} bytes of output omitted]")
}

/// Per-pane state machine that assembles a Block from BlockEvents.
struct BlockBuilder {
    pane_id: PaneId,
    state: BuilderState,
    command: String,
    output: CappedOutput,
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
            command: String::new(),
            output: CappedOutput::default(),
            start_time: None,
            cwd: PathBuf::new(),
        }
    }

    /// Process a BlockEvent, returning a completed Block if one was sealed.
    fn process(&mut self, event: BlockEvent, store: &BlockStore) -> Option<Block> {
        match event {
            BlockEvent::PromptStart { .. } => {
                self.state = BuilderState::PromptActive;
                self.output = CappedOutput::default();
                self.command.clear();
                None
            }
            BlockEvent::CommandStart { .. } => {
                self.state = BuilderState::CommandInput;
                None
            }
            BlockEvent::ExecutionStart { command, .. } => {
                self.command = command;
                self.start_time = Some(Instant::now());
                self.state = BuilderState::Executing;
                self.output = CappedOutput::default();
                None
            }
            BlockEvent::Output { data, .. } => {
                if self.state == BuilderState::Executing {
                    self.output.push(&data);
                }
                None
            }
            BlockEvent::CommandFinished { exit_code, .. } => {
                let duration = self.start_time.map(|t| t.elapsed());
                let (stripped, omitted_bytes) = std::mem::take(&mut self.output).finish();

                let classification = nex_token_save::classify(&self.command, &stripped);
                let mut compressed = nex_token_save::compress(&classification, &self.command, &stripped);
                if omitted_bytes > 0 {
                    compressed.summary = partial_summary(&compressed.summary, omitted_bytes);
                }
                let token_estimate = stripped.len() / TOKEN_ESTIMATE_DIVISOR;

                let block = Block {
                    id: BlockId::new(),
                    pane_id: self.pane_id,
                    sequence: store.next_sequence(),
                    prompt: String::new(),
                    command: std::mem::take(&mut self.command),
                    output: BlockOutput {
                        stripped_text: stripped,
                        compressed,
                        omitted_bytes,
                    },
                    exit_code: Some(exit_code),
                    cwd: self.cwd.clone(),
                    duration,
                    timestamp: Utc::now(),
                    classification,
                    token_estimate,
                };

                self.state = BuilderState::Idle;
                self.start_time = None;
                Some(block)
            }
            BlockEvent::CwdChanged { cwd, .. } => {
                self.cwd = cwd;
                None
            }
            // Handled by the processor loop (the builder is dropped).
            BlockEvent::PaneClosed { .. } => None,
        }
    }
}

/// Run the block processor thread. Consumes BlockEvents and populates the BlockStore.
/// Call this from a dedicated thread.
pub fn block_processor_thread(rx: Receiver<BlockEvent>, store: Arc<BlockStore>) {
    let mut builders: HashMap<PaneId, BlockBuilder> = HashMap::new();

    while let Ok(event) = rx.recv() {
        if let BlockEvent::PaneClosed { pane_id } = event {
            builders.remove(&pane_id);
            continue;
        }
        let pane_id = match &event {
            BlockEvent::PromptStart { pane_id }
            | BlockEvent::CommandStart { pane_id }
            | BlockEvent::ExecutionStart { pane_id, .. }
            | BlockEvent::CommandFinished { pane_id, .. }
            | BlockEvent::Output { pane_id, .. }
            | BlockEvent::CwdChanged { pane_id, .. }
            | BlockEvent::PaneClosed { pane_id } => *pane_id,
        };

        let builder = builders
            .entry(pane_id)
            .or_insert_with(|| BlockBuilder::new(pane_id));

        if let Some(block) = builder.process(event, &store) {
            tracing::info!(
                pane_id = %block.pane_id,
                command = %block.command,
                exit_code = ?block.exit_code,
                duration = ?block.duration,
                classification = ?block.classification,
                token_estimate = block.token_estimate,
                compression_ratio = block.output.compressed.compression_ratio,
                "Block sealed"
            );
            store.insert(block);
        }
    }

    tracing::info!("Block processor thread exiting");
}

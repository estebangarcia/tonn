use crossbeam_channel::{Receiver, Sender, bounded};
use nex_common::PaneId;

/// Events sent from the I/O thread to the block processor.
#[derive(Debug)]
pub enum BlockEvent {
    /// Shell prompt started (OSC 133;A).
    PromptStart { pane_id: PaneId },
    /// Command input started (OSC 133;B).
    CommandStart { pane_id: PaneId },
    /// Command execution started (OSC 133;C).
    ExecutionStart {
        pane_id: PaneId,
        command: String,
    },
    /// Command finished (OSC 133;D).
    CommandFinished {
        pane_id: PaneId,
        exit_code: i32,
    },
    /// Raw output bytes for the current block.
    Output {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    /// Working directory changed.
    CwdChanged {
        pane_id: PaneId,
        cwd: std::path::PathBuf,
    },
}

/// Max block events buffered before backpressure on I/O threads.
const BLOCK_CHANNEL_CAPACITY: usize = 10_000;

/// Create a bounded channel pair for block events.
pub fn block_channel() -> (Sender<BlockEvent>, Receiver<BlockEvent>) {
    bounded(BLOCK_CHANNEL_CAPACITY)
}

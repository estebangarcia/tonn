use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use nex_common::PaneId;

/// Events sent from the I/O thread to the main/UI thread.
#[derive(Debug)]
pub enum IoEvent {
    /// Terminal output changed, needs redraw.
    Redraw(PaneId),
    /// PTY process exited.
    ProcessExited { pane_id: PaneId, exit_code: i32 },
    /// Terminal title changed.
    TitleChanged { pane_id: PaneId, title: String },
    /// Bell character received.
    Bell(PaneId),
}

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

/// Create a bounded channel pair for I/O events.
pub fn io_channel(capacity: usize) -> (Sender<IoEvent>, Receiver<IoEvent>) {
    bounded(capacity)
}

/// Create an unbounded channel pair for block events.
pub fn block_channel() -> (Sender<BlockEvent>, Receiver<BlockEvent>) {
    unbounded()
}

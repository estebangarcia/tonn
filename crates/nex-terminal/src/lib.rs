//! Terminal emulation for Nexterm, wrapping alacritty_terminal.

pub use alacritty_terminal::event::Event as TerminalEvent;
pub use alacritty_terminal::event::EventListener;
pub use alacritty_terminal::grid::Grid;
pub use alacritty_terminal::term::cell::Cell;
pub use alacritty_terminal::term::{Config as TermConfig, Term};
pub use alacritty_terminal::vte::ansi;

use nex_common::PaneId;

/// Event listener that forwards terminal events and detects shell integration.
#[derive(Debug)]
pub struct NexEventListener {
    pub pane_id: PaneId,
}

impl NexEventListener {
    pub fn new(pane_id: PaneId) -> Self {
        Self { pane_id }
    }
}

impl EventListener for NexEventListener {
    fn send_event(&self, event: TerminalEvent) {
        tracing::trace!(?event, pane_id = %self.pane_id, "terminal event");
    }
}

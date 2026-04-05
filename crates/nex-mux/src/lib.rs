//! Built-in terminal multiplexer for Nexterm.
//! Phase 0 stub - full implementation in Phase 1.

use nex_common::MuxSessionId;
use serde::{Deserialize, Serialize};

/// A multiplexer session containing windows with tabs and panes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuxSession {
    pub id: MuxSessionId,
    pub name: String,
}

/// The split direction for pane layouts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// A tree node in the pane layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LayoutNode {
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
    Leaf {
        pane_id: nex_common::PaneId,
    },
}

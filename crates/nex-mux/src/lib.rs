//! Built-in terminal multiplexer for Tonn.
//!
//! Manages tabs, panes (splits), and their associated PTY + terminal instances.
//! Each pane owns a PTY process and a VT emulator (alacritty_terminal::Term).

mod layout;
mod pane;

pub use layout::{LayoutNode, SplitDirection};
pub use pane::Pane;

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use crossbeam_channel::Sender;
use nex_common::{PaneId, TabId, TerminalSize, CELL_WIDTH_RATIO, LINE_HEIGHT_RATIO, PADDING};
use nex_ipc::BlockEvent;
use parking_lot::Mutex;

pub const DEFAULT_TAB_TITLE: &str = "Terminal";

/// Tracks an AI session running in a Tonn pane.
#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub tab_index: usize,
    pub pane_id: PaneId,
    pub initial_block_count: usize,
}

/// Pixel rectangle for a pane's viewport within the window.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

/// A tab containing a layout tree of panes.
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub layout: LayoutNode,
}

/// Top-level multiplexer state.
pub struct Mux {
    panes: HashMap<PaneId, Pane>,
    tabs: Vec<Tab>,
    active_tab: usize,
    focused_pane: PaneId,
    zoomed_pane: Option<PaneId>,
    shell: String,
    font_size: f32,
    scale_factor: f32,
    block_event_tx: Sender<BlockEvent>,
    mcp_port: Option<u16>,
    scrollback_history: usize,
    /// Maps AI session IDs to (tab_index, pane_id, initial_block_count).
    /// The block count at resume time lets us detect when the command finishes.
    active_sessions: HashMap<String, ActiveSession>,
}

impl Mux {
    /// Create a new Mux with one tab containing one pane.
    pub fn new<Proxy: MuxEventProxy + 'static>(
        shell: String,
        initial_bounds: Rect,
        font_size: f32,
        scale_factor: f32,
        block_event_tx: Sender<BlockEvent>,
        event_proxy: Proxy,
        mcp_port: Option<u16>,
        scrollback_history: usize,
    ) -> anyhow::Result<Self> {
        let mut mux = Self {
            panes: HashMap::new(),
            tabs: Vec::new(),
            active_tab: 0,
            focused_pane: PaneId::new(),
            zoomed_pane: None,
            shell,
            font_size,
            scale_factor,
            block_event_tx,
            mcp_port,
            scrollback_history,
            active_sessions: HashMap::new(),
        };

        let pane_id = mux.spawn_pane(initial_bounds, &event_proxy)?;
        let tab = Tab {
            id: TabId::new(),
            title: DEFAULT_TAB_TITLE.to_string(),
            layout: LayoutNode::Leaf { pane_id },
        };
        mux.tabs.push(tab);
        mux.focused_pane = pane_id;

        Ok(mux)
    }

    /// Spawn a new pane: creates PTY + Term + I/O threads.
    fn spawn_pane<Proxy: MuxEventProxy + 'static>(
        &mut self,
        bounds: Rect,
        event_proxy: &Proxy,
    ) -> anyhow::Result<PaneId> {
        let (rows, cols) = self.bounds_to_term_size(&bounds);
        let pane = Pane::spawn(
            &self.shell,
            TerminalSize { rows, cols },
            bounds,
            self.block_event_tx.clone(),
            event_proxy,
            self.mcp_port,
            self.scrollback_history,
        )?;
        let pane_id = pane.id;
        self.panes.insert(pane_id, pane);
        Ok(pane_id)
    }

    fn bounds_to_term_size(&self, bounds: &Rect) -> (u16, u16) {
        let physical_font = self.font_size * self.scale_factor;
        let cell_width = physical_font * CELL_WIDTH_RATIO;
        let line_height = physical_font * LINE_HEIGHT_RATIO;
        let cols = ((bounds.width - PADDING * 2.0) / cell_width).floor().max(1.0) as u16;
        let rows = ((bounds.height - PADDING * 2.0) / line_height).floor().max(1.0) as u16;
        (rows, cols)
    }

    // --- Tab operations ---

    pub fn new_tab<Proxy: MuxEventProxy + 'static>(
        &mut self,
        bounds: Rect,
        event_proxy: &Proxy,
    ) -> anyhow::Result<TabId> {
        let pane_id = self.spawn_pane(bounds, event_proxy)?;
        let tab = Tab {
            id: TabId::new(),
            title: DEFAULT_TAB_TITLE.to_string(),
            layout: LayoutNode::Leaf { pane_id },
        };
        let tab_id = tab.id;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.focused_pane = pane_id;
        Ok(tab_id)
    }

    /// Create a new tab and immediately send an initial command to its PTY.
    /// Used for session resume: spawns a shell, then types the command + Enter.
    pub fn new_tab_with_command<Proxy: MuxEventProxy + 'static>(
        &mut self,
        bounds: Rect,
        event_proxy: &Proxy,
        command: &str,
    ) -> anyhow::Result<TabId> {
        let tab_id = self.new_tab(bounds, event_proxy)?;
        if let Some(writer) = self.focused_pane_writer() {
            let mut w = writer.lock();
            let _ = w.write_all(command.as_bytes());
            let _ = w.write_all(b"\r");
            let _ = w.flush();
        }
        Ok(tab_id)
    }

    /// Open a new tab for an AI session, or focus the existing tab if already active.
    /// Returns true if an existing tab was focused, false if a new one was created.
    pub fn open_session<Proxy: MuxEventProxy + 'static>(
        &mut self,
        session_id: &str,
        bounds: Rect,
        event_proxy: &Proxy,
        command: &str,
        initial_block_count: usize,
    ) -> anyhow::Result<bool> {
        // Check if session is already active in a tab
        if let Some(active) = self.active_sessions.get(session_id) {
            if active.tab_index < self.tabs.len() {
                let idx = active.tab_index;
                self.switch_tab(idx);
                return Ok(true);
            }
            self.active_sessions.remove(session_id);
        }

        let _tab_id = self.new_tab_with_command(bounds, event_proxy, command)?;
        let tab_idx = self.tabs.len() - 1;
        let pane_id = self.focused_pane;
        self.active_sessions.insert(session_id.to_string(), ActiveSession {
            tab_index: tab_idx,
            pane_id,
            initial_block_count,
        });
        Ok(false)
    }

    /// Check if a session is currently active in a tab.
    pub fn is_session_active(&self, session_id: &str) -> bool {
        self.active_sessions.contains_key(session_id)
    }

    /// Remove sessions whose `claude --resume` command has finished.
    /// Call this periodically with the BlockStore to detect completed sessions.
    pub fn cleanup_finished_sessions(&mut self, block_store: &nex_block::BlockStore) {
        let finished: Vec<String> = self.active_sessions.iter()
            .filter(|(_, active)| {
                let current_count = block_store.get_all_for_pane(&active.pane_id).len();
                current_count > active.initial_block_count
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            tracing::debug!(session_id = %id, "Session command finished, removing from active");
            self.active_sessions.remove(&id);
        }
    }

    pub fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        for pane_id in tab.layout.pane_ids() {
            self.panes.remove(&pane_id);
        }
        // Clean up session tracking — remove entries pointing to this tab and fix indices
        self.active_sessions.retain(|_, active| active.tab_index != index);
        for active in self.active_sessions.values_mut() {
            if active.tab_index > index {
                active.tab_index -= 1;
            }
        }
        if self.tabs.is_empty() {
            return; // caller should exit
        }
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        self.focused_pane = self.tabs[self.active_tab].layout.pane_ids()[0];
    }

    pub fn switch_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.zoomed_pane = None;
            self.active_tab = index;
            self.focused_pane = self.tabs[self.active_tab].layout.pane_ids()[0];
        }
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    pub fn set_pane_title(&mut self, pane_id: PaneId, title: String) {
        for tab in &mut self.tabs {
            if tab.layout.pane_ids().contains(&pane_id) {
                tab.title = title;
                return;
            }
        }
    }

    pub fn tab_titles(&self) -> Vec<(TabId, &str, bool)> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| (tab.id, tab.title.as_str(), i == self.active_tab))
            .collect()
    }

    // --- Split operations ---

    pub fn split<Proxy: MuxEventProxy + 'static>(
        &mut self,
        direction: SplitDirection,
        bounds: Rect,
        event_proxy: &Proxy,
    ) -> anyhow::Result<PaneId> {
        self.zoomed_pane = None;
        self._split(direction, bounds, event_proxy)
    }

    fn _split<Proxy: MuxEventProxy + 'static>(
        &mut self,
        direction: SplitDirection,
        bounds: Rect,
        event_proxy: &Proxy,
    ) -> anyhow::Result<PaneId> {
        let new_pane_id = self.spawn_pane(bounds, event_proxy)?;
        let tab = &mut self.tabs[self.active_tab];
        tab.layout.split_leaf(self.focused_pane, new_pane_id, direction);
        self.focused_pane = new_pane_id;
        Ok(new_pane_id)
    }

    pub fn close_pane(&mut self, pane_id: PaneId) {
        self.zoomed_pane = None;

        // Find which tab contains this pane (may be a background tab)
        let tab_index = match self.tabs.iter().position(|tab| tab.layout.pane_ids().contains(&pane_id)) {
            Some(idx) => idx,
            None => return,
        };

        let pane_ids = self.tabs[tab_index].layout.pane_ids();

        if pane_ids.len() <= 1 {
            // Last pane in tab — close the tab
            self.close_tab(tab_index);
            return;
        }

        self.tabs[tab_index].layout.remove_leaf(pane_id);
        self.panes.remove(&pane_id);

        if self.focused_pane == pane_id {
            self.focused_pane = self.tabs[self.active_tab].layout.pane_ids()[0];
        }
    }

    // --- Focus ---

    pub fn focused_pane_id(&self) -> PaneId {
        self.focused_pane
    }

    pub fn focus_pane(&mut self, pane_id: PaneId) {
        if self.panes.contains_key(&pane_id) {
            self.focused_pane = pane_id;
        }
    }

    pub fn focus_pane_at_pixel(&mut self, px: f32, py: f32) -> Option<PaneId> {
        let active_ids = self.tabs[self.active_tab].layout.pane_ids();
        let pane_id = active_ids.into_iter()
            .filter_map(|id| self.panes.get(&id))
            .find(|p| p.bounds.contains(px, py))
            .map(|p| p.id)?;
        self.focused_pane = pane_id;
        Some(pane_id)
    }

    pub fn cycle_focus(&mut self, forward: bool) {
        let tab = &self.tabs[self.active_tab];
        let ids = tab.layout.pane_ids();
        if ids.len() <= 1 {
            return;
        }
        let current = ids.iter().position(|id| *id == self.focused_pane).unwrap_or(0);
        let next = if forward {
            (current + 1) % ids.len()
        } else {
            (current + ids.len() - 1) % ids.len()
        };
        self.focused_pane = ids[next];
    }

    // --- Layout ---

    pub fn recalculate_bounds(&mut self, total_bounds: Rect) {
        // Always compute tree bounds so all panes stay in sync
        let tab = &self.tabs[self.active_tab];
        let mut pane_bounds = HashMap::new();
        tab.layout.compute_bounds(total_bounds, &mut pane_bounds);

        let font_size = self.font_size;
        let scale_factor = self.scale_factor;
        let zoomed_id = self.zoomed_pane;

        for (pane_id, tree_bounds) in pane_bounds {
            // Zoomed pane gets full window bounds; others get tree bounds
            let bounds = if zoomed_id == Some(pane_id) { total_bounds } else { tree_bounds };

            if let Some(pane) = self.panes.get_mut(&pane_id) {
                pane.bounds = bounds;
                let physical_font = font_size * scale_factor;
                let cell_width = physical_font * CELL_WIDTH_RATIO;
                let line_height = physical_font * LINE_HEIGHT_RATIO;
                let cols = ((bounds.width - PADDING * 2.0) / cell_width).floor().max(1.0) as u16;
                let rows = ((bounds.height - PADDING * 2.0) / line_height).floor().max(1.0) as u16;
                let new_size = TerminalSize { rows, cols };
                if new_size != pane.term_size {
                    pane.term_size = new_size;
                    let _ = pane.pty.resize(new_size);
                    pane.terminal.lock().resize(nex_terminal::TermSize::new(cols as usize, rows as usize));
                }
            }
        }
    }

    pub fn update_font(&mut self, font_size: f32, scale_factor: f32) {
        self.font_size = font_size;
        self.scale_factor = scale_factor;
    }

    // --- Accessors ---

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.panes.get(&self.focused_pane)
    }

    pub fn focused_pane_writer(&self) -> Option<Arc<Mutex<Box<dyn Write + Send>>>> {
        self.panes.get(&self.focused_pane).map(|p| Arc::clone(&p.pty_writer))
    }

    pub fn panes_in_active_tab(&self) -> Vec<&Pane> {
        if let Some(zoomed_id) = self.zoomed_pane {
            return self.panes.get(&zoomed_id).into_iter().collect();
        }
        let tab = &self.tabs[self.active_tab];
        tab.layout
            .pane_ids()
            .into_iter()
            .filter_map(|id| self.panes.get(&id))
            .collect()
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed_pane.is_some()
    }

    /// Toggle fullscreen zoom on the focused pane.
    pub fn toggle_zoom(&mut self, total_bounds: Rect) {
        self.zoomed_pane = if self.zoomed_pane.is_some() { None } else { Some(self.focused_pane) };
        self.recalculate_bounds(total_bounds);
    }

    pub fn active_layout(&self) -> &LayoutNode {
        &self.tabs[self.active_tab].layout
    }
}

/// Trait for sending events to the main event loop.
/// This abstracts over winit's EventLoopProxy so nex-mux doesn't depend on concrete UserEvent.
pub trait MuxEventProxy: Send + Sync + Clone {
    fn send_pty_exited(&self, pane_id: PaneId);
    fn send_title(&self, pane_id: PaneId, title: String);
    fn send_reset_title(&self, pane_id: PaneId);
    fn send_bell(&self, pane_id: PaneId);
    fn send_redraw(&self);
}

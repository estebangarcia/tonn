//! Nexterm - AI-First Terminal Emulator
//!
//! Main binary: winit event loop + wgpu rendering + mux-managed panes.

use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use nex_common::{PaneId, CELL_WIDTH_RATIO, LINE_HEIGHT_RATIO, PADDING};
use nex_mux::{Mux, MuxEventProxy, Rect, SplitDirection, DEFAULT_TAB_TITLE};
use nex_render::renderer::{BgCell, PaneContent, Renderer, RenderSpan, SelectionCell, DEFAULT_FONT_SIZE, FONT_SIZE_STEP};

// UI constants
const APP_TITLE: &str = "Nexterm";
const TAB_BAR_HEIGHT_LOGICAL: f32 = 28.0;
const BELL_FLASH_DURATION_MS: u64 = 150;
const DEFAULT_WINDOW_WIDTH: u32 = 960;
const DEFAULT_WINDOW_HEIGHT: u32 = 640;
const SCROLL_LINE_MULTIPLIER: i32 = 3;
const SCROLL_PIXEL_DIVISOR: f64 = 20.0;
const RESIZE_DEBOUNCE_MS: u64 = 50;
const DIVIDER_RENDER_THICKNESS: f32 = 4.0;
const EXECUTE_STDOUT_MAX_CHARS: usize = 4000;
const EXECUTE_STDERR_MAX_CHARS: usize = 2000;
use nex_terminal::{
    Column, Dimensions, Line, Point, Selection, SelectionType, Side,
    read_grid_content,
};

// ---------------------------------------------------------------------------
// UserEvent + MuxEventProxy bridge
// ---------------------------------------------------------------------------

enum UserEvent {
    PtyExited(PaneId),
    Title(PaneId, String),
    ResetTitle(PaneId),
    Bell(PaneId),
    Redraw,
    McpExecute(nex_mcp::ExecuteCommand),
}

impl std::fmt::Debug for UserEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PtyExited(id) => write!(f, "PtyExited({id})"),
            Self::Title(id, t) => write!(f, "Title({id}, {t})"),
            Self::ResetTitle(id) => write!(f, "ResetTitle({id})"),
            Self::Bell(id) => write!(f, "Bell({id})"),
            Self::Redraw => write!(f, "Redraw"),
            Self::McpExecute(cmd) => write!(f, "McpExecute({})", cmd.command),
        }
    }
}

/// Adapts winit's EventLoopProxy to the MuxEventProxy trait.
#[derive(Clone)]
struct WinitProxy(EventLoopProxy<UserEvent>);

impl MuxEventProxy for WinitProxy {
    fn send_pty_exited(&self, pane_id: PaneId) {
        let _ = self.0.send_event(UserEvent::PtyExited(pane_id));
    }
    fn send_title(&self, pane_id: PaneId, title: String) {
        let _ = self.0.send_event(UserEvent::Title(pane_id, title));
    }
    fn send_reset_title(&self, pane_id: PaneId) {
        let _ = self.0.send_event(UserEvent::ResetTitle(pane_id));
    }
    fn send_bell(&self, pane_id: PaneId) {
        let _ = self.0.send_event(UserEvent::Bell(pane_id));
    }
    fn send_redraw(&self) {
        let _ = self.0.send_event(UserEvent::Redraw);
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "nexterm", about = "AI-First Terminal Emulator")]
struct Cli {
    #[arg(short, long)]
    shell: Option<String>,
    #[arg(short, long)]
    verbose: bool,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct TabSwitcher {
    selected_index: usize,
}

struct SessionBrowser {
    trees: Vec<nex_ai_session::SessionTree>,
    nexterm_active_ids: Vec<String>,  // session IDs running in Nexterm panes
    display_entries: Vec<DisplayEntry>,
    selected_index: usize,
    filter: String,
    active_only: bool,
    expanded_projects: std::collections::HashSet<String>,
}

struct DisplayEntry {
    kind: DisplayEntryKind,
}

enum DisplayEntryKind {
    ProjectHeader { name: String, count: usize, expanded: bool },
    Session { flat_entry: nex_ai_session::FlatSessionEntry },
}

impl SessionBrowser {
    fn new(trees: Vec<nex_ai_session::SessionTree>, nexterm_active_ids: Vec<String>) -> Self {
        let expanded_projects: std::collections::HashSet<String> =
            trees.iter().map(|t| t.project_name.clone()).collect();
        let mut browser = Self {
            trees,
            nexterm_active_ids,
            display_entries: Vec::new(),
            selected_index: 0,
            filter: String::new(),
            active_only: false,
            expanded_projects,
        };
        browser.rebuild_display();
        browser
    }

    fn rebuild_display(&mut self) {
        self.display_entries.clear();
        let query = self.filter.to_lowercase();

        for tree in &self.trees {
            let mut flat: Vec<nex_ai_session::FlatSessionEntry> = Vec::new();
            for (i, root) in tree.roots.iter().enumerate() {
                root.flatten(0, i == tree.roots.len() - 1, &mut flat);
            }

            let filtered_flat: Vec<nex_ai_session::FlatSessionEntry> = flat
                .into_iter()
                .filter(|f| {
                    if self.active_only && !self.is_active_in_nexterm(&f.session.id) {
                        return false;
                    }
                    if query.is_empty() {
                        return true;
                    }
                    f.session.project_name.to_lowercase().contains(&query)
                        || f.session.summary.to_lowercase().contains(&query)
                        || f.session.id.to_lowercase().contains(&query)
                })
                .collect();

            if filtered_flat.is_empty() {
                continue;
            }

            let total_count = filtered_flat.len();
            let expanded = self.expanded_projects.contains(&tree.project_name);

            self.display_entries.push(DisplayEntry {
                kind: DisplayEntryKind::ProjectHeader {
                    name: tree.project_name.clone(),
                    count: total_count,
                    expanded,
                },
            });

            if expanded {
                for flat_entry in filtered_flat {
                    self.display_entries.push(DisplayEntry {
                        kind: DisplayEntryKind::Session { flat_entry },
                    });
                }
            }
        }

        if self.selected_index >= self.display_entries.len() {
            self.selected_index = self.display_entries.len().saturating_sub(1);
        }
    }

    fn apply_filter(&mut self) {
        self.rebuild_display();
    }

    fn toggle_active_only(&mut self) {
        self.active_only = !self.active_only;
        self.rebuild_display();
    }

    fn toggle_expand(&mut self) {
        if let Some(entry) = self.display_entries.get(self.selected_index) {
            if let DisplayEntryKind::ProjectHeader { name, .. } = &entry.kind {
                let name = name.clone();
                if self.expanded_projects.contains(&name) {
                    self.expanded_projects.remove(&name);
                } else {
                    self.expanded_projects.insert(name);
                }
                self.rebuild_display();
            }
        }
    }

    fn expand_at_selection(&mut self) {
        if let Some(entry) = self.display_entries.get(self.selected_index) {
            if let DisplayEntryKind::ProjectHeader { name, expanded, .. } = &entry.kind {
                if !expanded {
                    self.expanded_projects.insert(name.clone());
                    self.rebuild_display();
                }
            }
        }
    }

    fn collapse_at_selection(&mut self) {
        if let Some(entry) = self.display_entries.get(self.selected_index) {
            if let DisplayEntryKind::ProjectHeader { name, expanded, .. } = &entry.kind {
                if *expanded {
                    self.expanded_projects.remove(name);
                    self.rebuild_display();
                }
            }
        }
    }

    fn is_header_selected(&self) -> bool {
        self.display_entries
            .get(self.selected_index)
            .map(|e| matches!(e.kind, DisplayEntryKind::ProjectHeader { .. }))
            .unwrap_or(false)
    }

    fn selected_session(&self) -> Option<&nex_ai_session::AiSession> {
        self.display_entries.get(self.selected_index).and_then(|entry| {
            if let DisplayEntryKind::Session { flat_entry } = &entry.kind {
                Some(&flat_entry.session)
            } else {
                None
            }
        })
    }

    fn is_active_in_nexterm(&self, session_id: &str) -> bool {
        self.nexterm_active_ids.iter().any(|id| id == session_id)
    }
}

struct App {
    shell: String,
    proxy: WinitProxy,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    mux: Option<Mux>,
    modifiers: Modifiers,
    mouse_pos: (f64, f64),
    mouse_selecting: bool,
    bell_flash_until: Option<std::time::Instant>,
    tab_switcher: Option<TabSwitcher>,
    session_browser: Option<SessionBrowser>,
    session_manager: Option<Arc<nex_ai_session::SessionManager>>,
    last_tab_count: usize,
    last_active_tab: usize,
    terminal_state: Option<Arc<parking_lot::Mutex<nex_mcp::TerminalStateSnapshot>>>,
    block_store: Option<Arc<nex_block::BlockStore>>,
    pending_resize: Option<(u32, u32, std::time::Instant)>,
    window_focused: bool,
}

impl App {
    fn new(shell: String, proxy: WinitProxy) -> Self {
        Self {
            shell,
            proxy,
            window: None,
            renderer: None,
            mux: None,
            modifiers: Modifiers::default(),
            mouse_pos: (0.0, 0.0),
            mouse_selecting: false,
            bell_flash_until: None,
            tab_switcher: None,
            session_browser: None,
            session_manager: None,
            last_tab_count: 1,
            last_active_tab: 0,
            terminal_state: None,
            block_store: None,
            pending_resize: None,
            window_focused: true,
        }
    }

    fn tab_bar_height_for(renderer: &Renderer, tab_count: usize) -> f32 {
        if tab_count <= 1 { 0.0 } else { TAB_BAR_HEIGHT_LOGICAL * renderer.scale_factor() }
    }

    fn pane_area(renderer: &Renderer, tab_count: usize) -> Rect {
        let (w, h) = renderer.surface_size();
        let tab_h = Self::tab_bar_height_for(renderer, tab_count);
        Rect { x: 0.0, y: tab_h, width: w as f32, height: h as f32 - tab_h }
    }

    /// Convert pixel position to terminal grid point, relative to the focused pane.
    fn pixel_to_grid(&self, x: f64, y: f64) -> Option<(Point, Side)> {
        let mux = self.mux.as_ref()?;
        let pane = mux.focused_pane()?;
        let renderer = self.renderer.as_ref()?;
        let physical_font = renderer.font_size() * renderer.scale_factor();
        let cell_width = physical_font * CELL_WIDTH_RATIO;
        let line_height = physical_font * LINE_HEIGHT_RATIO;

        let local_x = x as f32 - pane.bounds.x;
        let local_y = y as f32 - pane.bounds.y;

        let col = ((local_x - PADDING) / cell_width).max(0.0) as usize;
        let row = ((local_y - PADDING) / line_height).max(0.0) as usize;

        let col = col.min(pane.term_size.cols.saturating_sub(1) as usize);
        let row = row.min(pane.term_size.rows.saturating_sub(1) as usize);

        let display_offset = pane.terminal.lock().grid().display_offset();
        let line = Line(row as i32 - display_offset as i32);
        let side = if local_x > PADDING && (local_x - PADDING) % cell_width > cell_width / 2.0 {
            Side::Right
        } else {
            Side::Left
        };

        Some((Point::new(line, Column(col)), side))
    }

    /// Write bytes to the focused pane's PTY.
    fn write_to_focused(&self, data: &[u8]) -> bool {
        if let Some(writer) = self.mux.as_ref().and_then(|m| m.focused_pane_writer()) {
            let mut w = writer.lock();
            let _ = w.write_all(data);
            let _ = w.flush();
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// ApplicationHandler
// ---------------------------------------------------------------------------

impl ApplicationHandler<UserEvent> for App {
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::PtyExited(pane_id) => {
                if let Some(mux) = &mut self.mux {
                    mux.close_pane(pane_id);
                    if mux.tab_count() == 0 {
                        tracing::info!("All tabs closed, exiting");
                        deregister_mcp();
                        event_loop.exit();
                        return;
                    }
                    mux.recalculate_bounds(Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count()));
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UserEvent::Title(pane_id, title) => {
                if let Some(window) = &self.window {
                    window.set_title(&title);
                }
                if let Some(mux) = &mut self.mux {
                    mux.set_pane_title(pane_id, title);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UserEvent::ResetTitle(pane_id) => {
                if let Some(window) = &self.window {
                    window.set_title(APP_TITLE);
                }
                if let Some(mux) = &mut self.mux {
                    mux.set_pane_title(pane_id, DEFAULT_TAB_TITLE.to_string());
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UserEvent::Bell(_pane_id) => {
                self.bell_flash_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(BELL_FLASH_DURATION_MS));
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UserEvent::Redraw => {
                // Only request redraw when focused — avoids queuing hundreds
                // of redraws while unfocused (e.g., during Claude Code streaming)
                if self.window_focused {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            UserEvent::McpExecute(cmd) => {
                tracing::debug!(command = %cmd.command, "MCP execute: running as subprocess");

                // Get CWD from terminal state for the subprocess
                let cwd = self.terminal_state.as_ref()
                    .and_then(|ts| {
                        let state = ts.lock();
                        state.active_pane_id.as_ref()
                            .and_then(|active_id| {
                                state.panes.iter()
                                    .find(|p| &p.id == active_id)
                                    .and_then(|p| if p.cwd.is_empty() { None } else { Some(p.cwd.clone()) })
                            })
                    });

                // Run as subprocess — don't write to PTY (which might be running Claude Code)
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
                std::thread::spawn(move || {
                    let mut child = std::process::Command::new(&shell);
                    child.args(["-c", &cmd.command]);
                    if let Some(dir) = &cwd {
                        child.current_dir(dir);
                    }
                    child.env("TERM", "dumb"); // no ANSI in captured output

                    let result = match child.output() {
                        Ok(output) => {
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            let exit_code = output.status.code().unwrap_or(-1);
                            let mut result = format!("Exit code: {exit_code}");
                            if let Some(dir) = &cwd {
                                result.push_str(&format!("\nCWD: {dir}"));
                            }
                            if !stdout.is_empty() {
                                let truncated: String = stdout.chars().take(EXECUTE_STDOUT_MAX_CHARS).collect();
                                result.push_str(&format!("\n\nSTDOUT:\n{truncated}"));
                            }
                            if !stderr.is_empty() {
                                let truncated: String = stderr.chars().take(EXECUTE_STDERR_MAX_CHARS).collect();
                                result.push_str(&format!("\n\nSTDERR:\n{truncated}"));
                            }
                            result
                        }
                        Err(e) => format!("Failed to run command: {e}"),
                    };
                    let _ = cmd.response_tx.send(result);
                });
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title(APP_TITLE)
            .with_inner_size(winit::dpi::LogicalSize::new(DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT));

        let window = Arc::new(event_loop.create_window(attrs).expect("Failed to create window"));
        self.window = Some(Arc::clone(&window));

        let renderer = pollster::block_on(Renderer::new(Arc::clone(&window)));
        match renderer {
            Ok(r) => {
                self.renderer = Some(r);
                let pane_area = Self::pane_area(self.renderer.as_ref().unwrap(), 1);
                let renderer = self.renderer.as_ref().unwrap();

                let (block_tx, block_rx) = nex_ipc::block_channel();

                // Pre-allocate MCP port so PTY shells get NEXTERM_MCP_PORT
                let mcp_port = std::net::TcpListener::bind("127.0.0.1:0")
                    .ok()
                    .and_then(|l| l.local_addr().ok())
                    .map(|a| a.port());

                // Spawn block processor thread
                let block_store = Arc::new(nex_block::BlockStore::new());
                let store_clone = Arc::clone(&block_store);
                std::thread::Builder::new()
                    .name("block-processor".into())
                    .spawn(move || {
                        nex_block::block_processor_thread(block_rx, store_clone);
                    })
                    .expect("Failed to spawn block processor thread");

                let mux = Mux::new(
                    self.shell.clone(),
                    pane_area,
                    renderer.font_size(),
                    renderer.scale_factor(),
                    block_tx,
                    self.proxy.clone(),
                    mcp_port,
                )
                .expect("Failed to create mux");

                tracing::info!("Mux initialized with 1 tab, 1 pane");
                self.mux = Some(mux);

                // Initialize session manager
                let session_manager = Arc::new(nex_ai_session::SessionManager::new());
                session_manager.scan();
                tracing::info!("Scanned {} Claude Code sessions", session_manager.count());
                self.session_manager = Some(Arc::clone(&session_manager));

                // Start session file watcher in background
                let watcher_manager = Arc::clone(&session_manager);
                std::thread::Builder::new()
                    .name("session-watcher".into())
                    .spawn(move || {
                        if let Err(e) = watcher_manager.start_watcher() {
                            tracing::warn!("Session file watcher failed: {e}");
                        }
                    })
                    .ok();

                // Store block_store for MCP state updates
                self.block_store = Some(Arc::clone(&block_store));

                // Start MCP server
                let terminal_state = Arc::new(parking_lot::Mutex::new(
                    nex_mcp::TerminalStateSnapshot::default(),
                ));
                self.terminal_state = Some(Arc::clone(&terminal_state));
                let (execute_tx, execute_rx) = std::sync::mpsc::channel::<nex_mcp::ExecuteCommand>();

                // Wire execute commands from MCP to the main event loop.
                // Uses std::sync::mpsc (no tokio runtime needed) for reliable
                // cross-thread delivery.
                let execute_proxy = self.proxy.clone();
                std::thread::Builder::new()
                    .name("mcp-execute-bridge".into())
                    .spawn(move || {
                        tracing::debug!("MCP execute bridge thread started");
                        while let Ok(cmd) = execute_rx.recv() {
                            tracing::debug!(command = %cmd.command, "Bridge: forwarding execute command to event loop");
                            if let Err(ref e) = execute_proxy.0.send_event(UserEvent::McpExecute(cmd)) {
                                tracing::error!("Bridge: failed to send to event loop: {e:?}");
                            }
                        }
                        tracing::debug!("Bridge: execute channel closed, exiting");
                    })
                    .expect("Failed to spawn MCP execute bridge");

                let mcp_server = nex_mcp::NextermMcpServer::new(
                    Arc::clone(&block_store),
                    Arc::clone(&terminal_state),
                    execute_tx,
                    Arc::clone(&session_manager),
                );

                // Spawn MCP HTTP server on the pre-allocated port
                let mcp_port_for_server = mcp_port;
                std::thread::Builder::new()
                    .name("mcp-server".into())
                    .spawn(move || {
                        let Some(port) = mcp_port_for_server else {
                            tracing::warn!("No MCP port allocated, MCP server not started");
                            return;
                        };
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("Failed to build MCP tokio runtime");
                        rt.block_on(async {
                            tracing::info!("MCP server listening on http://127.0.0.1:{port}/mcp");

                            let claude_path = find_claude_cli();

                            // Remove stale registration first (port may have changed)
                            let _ = tokio::process::Command::new(&claude_path)
                                .args(["mcp", "remove", "nexterm", "--scope", "user"])
                                .output()
                                .await;

                            // Register with new port
                            let mcp_url = format!("http://127.0.0.1:{port}/mcp");
                            let register_result = tokio::process::Command::new(&claude_path)
                                .args(["mcp", "add",
                                       "--transport", "http",
                                       "--scope", "user",
                                       "nexterm",
                                       &mcp_url])
                                .output()
                                .await;
                            match register_result {
                                Ok(output) if output.status.success() => {
                                    tracing::info!("Registered MCP with Claude Code at {mcp_url}");
                                }
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    tracing::debug!("Claude Code MCP registration failed: {stderr}");
                                }
                                Err(e) => {
                                    tracing::debug!("claude CLI not found ({e}), skipping MCP auto-registration");
                                }
                            }

                            if let Err(e) = mcp_server.start_http(port).await {
                                tracing::error!("MCP server error: {e}");
                            }
                        });
                    })
                    .expect("Failed to spawn MCP server thread");
            }
            Err(e) => {
                tracing::error!("Failed to initialize renderer: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                deregister_mcp();
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                // Update surface immediately (so rendering doesn't crash)
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
                // Debounce the terminal/PTY resize — only apply after resizing stops
                self.pending_resize = Some((size.width, size.height, std::time::Instant::now()));
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as i32 * SCROLL_LINE_MULTIPLIER,
                    MouseScrollDelta::PixelDelta(pos) => {
                        let lines = pos.y / SCROLL_PIXEL_DIVISOR;
                        if lines.abs() < 1.0 && lines != 0.0 {
                            lines.signum() as i32
                        } else {
                            lines as i32
                        }
                    }
                };
                if scroll_lines != 0 {
                    if let Some(mux) = &self.mux {
                        if let Some(pane) = mux.focused_pane() {
                            let mut term = pane.terminal.lock();
                            term.grid_mut().scroll_display(
                                alacritty_terminal::grid::Scroll::Delta(scroll_lines),
                            );
                        }
                    }
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = (position.x, position.y);
                if self.mouse_selecting {
                    if let Some((point, side)) = self.pixel_to_grid(position.x, position.y) {
                        if let Some(mux) = &self.mux {
                            if let Some(pane) = mux.focused_pane() {
                                let mut term = pane.terminal.lock();
                                if let Some(ref mut sel) = term.selection {
                                    sel.update(point, side);
                                }
                            }
                        }
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                match state {
                    ElementState::Pressed => {
                        // Focus pane at click position
                        if let Some(mux) = &mut self.mux {
                            mux.focus_pane_at_pixel(self.mouse_pos.0 as f32, self.mouse_pos.1 as f32);
                        }
                        // Start selection
                        if let Some((point, side)) = self.pixel_to_grid(self.mouse_pos.0, self.mouse_pos.1) {
                            if let Some(mux) = &self.mux {
                                if let Some(pane) = mux.focused_pane() {
                                    pane.terminal.lock().selection =
                                        Some(Selection::new(SelectionType::Simple, point, side));
                                }
                            }
                        }
                        self.mouse_selecting = true;
                    }
                    ElementState::Released => {
                        self.mouse_selecting = false;
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                // If tab switcher is open and Ctrl was released, apply selection
                if let Some(switcher) = self.tab_switcher.take() {
                    if !new_modifiers.state().control_key() {
                        if let Some(mux) = &mut self.mux {
                            mux.switch_tab(switcher.selected_index);
                            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                            mux.recalculate_bounds(area);
                        }
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    } else {
                        self.tab_switcher = Some(switcher);
                    }
                }
                self.modifiers = new_modifiers;
            }

            WindowEvent::KeyboardInput {
                event: KeyEvent { state: ElementState::Pressed, logical_key, text, .. },
                ..
            } => {
                self.handle_key_input(&logical_key, text.as_deref(), event_loop);
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
            }

            WindowEvent::Focused(focused) => {
                self.window_focused = focused;
                if focused {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }

            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Keyboard handling (extracted for readability)
// ---------------------------------------------------------------------------

impl App {
    fn handle_key_input(
        &mut self,
        logical_key: &Key,
        text: Option<&str>,
        event_loop: &ActiveEventLoop,
    ) {
        // Scroll to bottom on keypress
        if let Some(mux) = &self.mux {
            if let Some(pane) = mux.focused_pane() {
                let mut term = pane.terminal.lock();
                if term.grid().display_offset() > 0 {
                    term.grid_mut()
                        .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                }
            }
        }

        let ctrl = self.modifiers.state().control_key();
        let super_key = self.modifiers.state().super_key();
        let shift = self.modifiers.state().shift_key();

        // --- Session browser (Cmd+Shift+P) ---
        if super_key && shift && matches!(logical_key, Key::Character(c) if c.as_str() == "p" || c.as_str() == "P") {
            if self.session_browser.is_some() {
                self.session_browser = None;
            } else if let Some(mgr) = &self.session_manager {
                let trees = mgr.session_trees();
                let active_ids: Vec<String> = self.mux.as_ref()
                    .map(|m| {
                        trees.iter()
                            .flat_map(|t| {
                                let mut flat = Vec::new();
                                for (i, root) in t.roots.iter().enumerate() {
                                    root.flatten(0, i == t.roots.len() - 1, &mut flat);
                                }
                                flat.into_iter().filter(|f| m.is_session_active(&f.session.id)).map(|f| f.session.id)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.session_browser = Some(SessionBrowser::new(trees, active_ids));
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }

        // Session browser navigation (when open)
        if let Some(browser) = &mut self.session_browser {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.session_browser = None;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    if browser.selected_index > 0 {
                        browser.selected_index -= 1;
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    if browser.selected_index + 1 < browser.display_entries.len() {
                        browser.selected_index += 1;
                    }
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    browser.expand_at_selection();
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                Key::Named(NamedKey::ArrowLeft) => {
                    browser.collapse_at_selection();
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    if browser.is_header_selected() {
                        browser.toggle_expand();
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    } else if let Some(session) = browser.selected_session().cloned() {
                        self.session_browser = None;
                        self.resume_session(&session);
                    }
                    return;
                }
                Key::Named(NamedKey::Tab) => {
                    browser.toggle_active_only();
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    browser.filter.pop();
                    browser.apply_filter();
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
                _ => {
                    if let Some(text) = text {
                        if !ctrl && !super_key {
                            browser.filter.push_str(text);
                            browser.apply_filter();
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                            return;
                        }
                    }
                }
            }
        }

        // --- Tab switcher (Ctrl+Tab / Ctrl+Shift+Tab) ---
        if ctrl && matches!(logical_key, Key::Named(NamedKey::Tab)) {
            if let Some(mux) = &self.mux {
                let tab_count = mux.tab_count();
                if tab_count > 1 {
                    let current = self.tab_switcher
                        .as_ref()
                        .map(|s| s.selected_index)
                        .unwrap_or(mux.active_tab_index());
                    let next = if shift {
                        (current + tab_count - 1) % tab_count
                    } else {
                        (current + 1) % tab_count
                    };
                    self.tab_switcher = Some(TabSwitcher { selected_index: next });
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            return;
        }

        // Esc cancels tab switcher
        if self.tab_switcher.is_some() && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            self.tab_switcher = None;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }

        // --- Paste (Cmd+V / Ctrl+Shift+V) ---
        if matches!(logical_key, Key::Character(c) if c.as_str() == "v")
            && (super_key || (ctrl && shift))
        {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                if let Ok(text) = clipboard.get_text() {
                    self.write_to_focused(b"\x1b[200~");
                    self.write_to_focused(text.as_bytes());
                    self.write_to_focused(b"\x1b[201~");
                }
            }
            return;
        }

        // --- Copy (Cmd+C / Ctrl+Shift+C) ---
        if matches!(logical_key, Key::Character(c) if c.as_str() == "c")
            && (super_key || (ctrl && shift))
        {
            if let Some(mux) = &self.mux {
                if let Some(pane) = mux.focused_pane() {
                    if let Some(text) = pane.terminal.lock().selection_to_string() {
                        if let Ok(mut clipboard) = arboard::Clipboard::new() {
                            let _ = clipboard.set_text(text);
                        }
                    }
                }
            }
            return;
        }

        // --- Mux shortcuts (Cmd+key) ---
        if super_key {
            // Cmd+Shift+Enter → toggle pane zoom
            if shift && matches!(logical_key, Key::Named(NamedKey::Enter)) {
                if let Some(mux) = &mut self.mux {
                    let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                    mux.toggle_zoom(area);
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                return;
            }

            if let Key::Character(c) = logical_key {
                let handled = match c.as_str() {
                    // Cmd+T → new tab
                    "t" => {
                        if let Some(mux) = &mut self.mux {
                            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                            let _ = mux.new_tab(area, &self.proxy);
                            mux.recalculate_bounds(area);
                        }
                        true
                    }
                    // Cmd+W → close pane/tab
                    "w" => {
                        if let Some(mux) = &mut self.mux {
                            let pane_id = mux.focused_pane_id();
                            mux.close_pane(pane_id);
                            if mux.tab_count() == 0 {
                                event_loop.exit();
                                return;
                            }
                            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                            mux.recalculate_bounds(area);
                        }
                        true
                    }
                    // Cmd+D → vertical split, Cmd+Shift+D → horizontal split
                    "d" | "D" => {
                        let dir = if shift || c.as_str() == "D" {
                            SplitDirection::Horizontal
                        } else {
                            SplitDirection::Vertical
                        };
                        if let Some(mux) = &mut self.mux {
                            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                            let _ = mux.split(dir, area, &self.proxy);
                            mux.recalculate_bounds(area);
                        }
                        true
                    }
                    // Cmd+] / Cmd+[ → cycle focus
                    "]" => {
                        if let Some(mux) = &mut self.mux {
                            mux.cycle_focus(true);
                        }
                        true
                    }
                    "[" => {
                        if let Some(mux) = &mut self.mux {
                            mux.cycle_focus(false);
                        }
                        true
                    }
                    _ => {
                        // Cmd+1..9 → switch tab
                        if let Some(digit) = c.chars().next().and_then(|ch| ch.to_digit(10)) {
                            if (1..=9).contains(&digit) {
                                if let Some(mux) = &mut self.mux {
                                    mux.switch_tab((digit - 1) as usize);
                                }
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    }
                };
                if handled {
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                    return;
                }
            }
        }

        // --- Font zoom (Cmd+= / Cmd+- / Cmd+0) ---
        let zoom_mod = super_key || (cfg!(not(target_os = "macos")) && ctrl);
        if zoom_mod {
            let new_size = match logical_key {
                Key::Character(c) => match c.as_str() {
                    "=" | "+" => Some(self.renderer.as_ref().map_or(DEFAULT_FONT_SIZE, |r| r.font_size()) + FONT_SIZE_STEP),
                    "-" => Some(self.renderer.as_ref().map_or(DEFAULT_FONT_SIZE, |r| r.font_size()) - FONT_SIZE_STEP),
                    "0" => Some(DEFAULT_FONT_SIZE),
                    _ => None,
                },
                _ => None,
            };
            if let Some(size) = new_size {
                if let Some(renderer) = &mut self.renderer {
                    if renderer.set_font_size(size) {
                        if let Some(mux) = &mut self.mux {
                            mux.update_font(renderer.font_size(), renderer.scale_factor());
                            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                            mux.recalculate_bounds(area);
                        }
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
                return;
            }
        }


        // --- PTY input ---
        let mut wrote = false;

        if ctrl {
            wrote = match logical_key {
                Key::Character(c) => {
                    let ch = c.chars().next().unwrap_or('\0');
                    if ch.is_ascii_alphabetic() {
                        let ctrl_code = (ch.to_ascii_lowercase() as u8) - b'a' + 1;
                        self.write_to_focused(&[ctrl_code])
                    } else {
                        match ch {
                            '[' | '3' => self.write_to_focused(b"\x1b"),
                            '\\' | '4' => self.write_to_focused(b"\x1c"),
                            ']' | '5' => self.write_to_focused(b"\x1d"),
                            '/' | '7' => self.write_to_focused(b"\x1f"),
                            ' ' | '2' => self.write_to_focused(b"\x00"),
                            _ => false,
                        }
                    }
                }
                _ => false,
            };
        }

        if !wrote {
            wrote = true;
            // Check if the focused pane is in application cursor mode
            let app_cursor = self.mux.as_ref()
                .and_then(|m| m.focused_pane())
                .map(|p| p.terminal.lock().mode().contains(nex_terminal::TermMode::APP_CURSOR))
                .unwrap_or(false);

            let data: Option<&[u8]> = match logical_key {
                Key::Named(NamedKey::Enter) => Some(b"\r"),
                Key::Named(NamedKey::Backspace) => Some(b"\x7f"),
                Key::Named(NamedKey::Tab) => Some(b"\t"),
                Key::Named(NamedKey::Escape) => Some(b"\x1b"),
                Key::Named(NamedKey::ArrowUp) => Some(if app_cursor { b"\x1bOA" } else { b"\x1b[A" }),
                Key::Named(NamedKey::ArrowDown) => Some(if app_cursor { b"\x1bOB" } else { b"\x1b[B" }),
                Key::Named(NamedKey::ArrowRight) => Some(if app_cursor { b"\x1bOC" } else { b"\x1b[C" }),
                Key::Named(NamedKey::ArrowLeft) => Some(if app_cursor { b"\x1bOD" } else { b"\x1b[D" }),
                Key::Named(NamedKey::Home) => Some(if app_cursor { b"\x1bOH" } else { b"\x1b[H" }),
                Key::Named(NamedKey::End) => Some(if app_cursor { b"\x1bOF" } else { b"\x1b[F" }),
                Key::Named(NamedKey::Delete) => Some(b"\x1b[3~"),
                Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~"),
                Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~"),
                _ => {
                    if !ctrl {
                        if let Some(text) = text {
                            // write_to_focused returns bool
                            if self.write_to_focused(text.as_bytes()) {
                                // wrote stays true
                            } else {
                                wrote = false;
                            }
                        } else {
                            wrote = false;
                        }
                        None // already handled
                    } else {
                        wrote = false;
                        None
                    }
                }
            };
            if let Some(data) = data {
                self.write_to_focused(data);
            }
        }

        if wrote {
            // Clear selection after typing
            if let Some(mux) = &self.mux {
                if let Some(pane) = mux.focused_pane() {
                    pane.terminal.lock().selection = None;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Session resume
// ---------------------------------------------------------------------------

impl App {
    fn resume_session(&mut self, session: &nex_ai_session::AiSession) {
        let claude = find_claude_cli();
        let project_dir = session.project_dir.display();
        let session_id = &session.id;

        // Build the resume command with error handling wrapper
        let command = format!(
            "cd {project_dir} && {claude} --resume {session_id} || {{ \
            echo ''; \
            echo '╭──────────────────────────────────────────╮'; \
            echo '│  Could not resume this session.           │'; \
            echo '│  It may be active in another terminal.    │'; \
            echo '│                                           │'; \
            echo '│  Press Enter to close this pane.          │'; \
            echo '╰──────────────────────────────────────────╯'; \
            read; }}",
            claude = claude.display(),
        );

        if let Some(mux) = &mut self.mux {
            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count() + 1);
            let initial_blocks = self.block_store.as_ref()
                .map(|s| s.get_all_for_pane(&mux.focused_pane_id()).len())
                .unwrap_or(0);
            match mux.open_session(session_id, area, &self.proxy, &command, initial_blocks) {
                Ok(was_existing) => {
                    if was_existing {
                        tracing::info!(session_id, "Focused existing session tab");
                    } else {
                        tracing::info!(session_id, %project_dir, "Resuming Claude Code session in new tab");
                    }
                    mux.recalculate_bounds(area);
                }
                Err(e) => {
                    tracing::error!("Failed to resume session: {e}");
                }
            }
        }

        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

// Rendering
// ---------------------------------------------------------------------------

impl App {
    fn render_frame(&mut self) {
        // Apply debounced resize if enough time has passed
        if let Some((_w, _h, when)) = self.pending_resize {
            if when.elapsed() > std::time::Duration::from_millis(RESIZE_DEBOUNCE_MS) {
                self.pending_resize = None;
                if let Some(mux) = &mut self.mux {
                    let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                    mux.recalculate_bounds(area);
                }
            } else {
                // Schedule another redraw to check again soon
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }

        // Recalculate bounds when tab count or active tab changes
        if let Some(mux) = &mut self.mux {
            let current_count = mux.tab_count();
            let active_tab = mux.active_tab_index();
            if current_count != self.last_tab_count || active_tab != self.last_active_tab {
                self.last_tab_count = current_count;
                self.last_active_tab = active_tab;
                let area = Self::pane_area(self.renderer.as_ref().unwrap(), current_count);
                mux.recalculate_bounds(area);
            }
        }

        let mux = match &self.mux {
            Some(m) => m,
            _ => return,
        };

        let focused_id = mux.focused_pane_id();
        let bell_active = self.bell_flash_until
            .map(|t| std::time::Instant::now() < t)
            .unwrap_or(false);
        if !bell_active {
            self.bell_flash_until = None;
        }

        // Build content for each pane in the active tab
        let pane_contents: Vec<PaneContent> = mux.panes_in_active_tab().iter().map(|pane| {
            let term = pane.terminal.lock();
            let content = read_grid_content(&term);

            let spans = content.spans.iter().map(|s| RenderSpan {
                text: s.text.clone(),
                r: s.fg.r, g: s.fg.g, b: s.fg.b,
                bold: s.bold, italic: s.italic,
            }).collect();

            let bg_cells = content.bg_cells.iter().map(|c| BgCell {
                row: c.row, col: c.col,
                r: c.bg.r, g: c.bg.g, b: c.bg.b,
            }).collect();

            let selection_cells = if let Some(sel) = &content.selection {
                let display_offset = term.grid().display_offset();
                let start_row = (sel.start.line.0 + display_offset as i32).max(0) as usize;
                let end_row = (sel.end.line.0 + display_offset as i32).max(0) as usize;
                let mut cells = Vec::new();
                for row in start_row..=end_row {
                    let col_start = if row == start_row { sel.start.column.0 } else { 0 };
                    let col_end = if row == end_row {
                        sel.end.column.0
                    } else {
                        term.grid().columns().saturating_sub(1)
                    };
                    for col in col_start..=col_end {
                        cells.push(SelectionCell { row, col });
                    }
                }
                cells
            } else {
                Vec::new()
            };

            PaneContent {
                pane_id: pane.id,
                x: pane.bounds.x,
                y: pane.bounds.y,
                width: pane.bounds.width,
                height: pane.bounds.height,
                spans,
                bg_cells,
                selection_cells,
                cursor_row: content.cursor_row,
                cursor_col: content.cursor_col,
                is_focused: pane.id == focused_id,
                bell_active: bell_active && pane.id == focused_id,
            }
        }).collect();

        // Build divider lines (skip when zoomed — only one pane visible)
        let divider_lines: Vec<nex_render::renderer::DividerLine> = if mux.is_zoomed() {
            Vec::new()
        } else {
            let layout = mux.active_layout();
            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
            let thickness = DIVIDER_RENDER_THICKNESS;
            layout
                .divider_lines(nex_mux::Rect { x: area.x, y: area.y, width: area.width, height: area.height })
                .into_iter()
                .map(|d| {
                    use nex_mux::SplitDirection;
                    let half = thickness / 2.0;
                    match d.direction {
                        SplitDirection::Vertical => nex_render::renderer::DividerLine {
                            x: d.x - half,
                            y: d.y,
                            width: thickness,
                            height: d.length,
                        },
                        SplitDirection::Horizontal => nex_render::renderer::DividerLine {
                            x: d.x,
                            y: d.y - half,
                            width: d.length,
                            height: thickness,
                        },
                    }
                })
                .collect()
        };

        let tab_infos: Vec<nex_render::renderer::TabInfo> = mux.tab_titles()
            .into_iter()
            .map(|(_, title, active)| nex_render::renderer::TabInfo {
                title: title.to_string(),
                is_active: active,
            })
            .collect();

        let active_pane_ids: Vec<PaneId> = pane_contents.iter().map(|p| p.pane_id).collect();
        let renderer = self.renderer.as_mut().unwrap();
        renderer.cleanup_pane_buffers(&active_pane_ids);
        // Hide tab bar when only 1 tab
        let visible_tabs = if tab_infos.len() <= 1 { &[][..] } else { &tab_infos[..] };
        let tab_h = Self::tab_bar_height_for(renderer, tab_infos.len());
        // Build overlay if tab switcher is active
        let overlay = self.tab_switcher.as_ref().map(|switcher| {
            nex_render::renderer::OverlayContent {
                entries: tab_infos.iter().enumerate().map(|(i, t)| {
                    nex_render::renderer::OverlayEntry {
                        label: format!("{}. {}", i + 1, t.title),
                        is_active: t.is_active,
                    }
                }).collect(),
                selected_index: switcher.selected_index,
            }
        });

        // Build session browser overlay if active
        let session_overlay = self.session_browser.as_ref().map(|browser| {
            let filter_display = if browser.active_only {
                if browser.filter.is_empty() {
                    "[Active] Search...".to_string()
                } else {
                    format!("[Active] {}", browser.filter)
                }
            } else {
                browser.filter.clone()
            };
            let entries: Vec<nex_render::renderer::SessionOverlayEntry> = browser
                .display_entries
                .iter()
                .enumerate()
                .map(|(i, de)| {
                    let kind = match &de.kind {
                        DisplayEntryKind::ProjectHeader { name, count, expanded } => {
                            nex_render::renderer::SessionEntryKind::ProjectHeader {
                                name: name.clone(),
                                session_count: *count,
                                expanded: *expanded,
                            }
                        }
                        DisplayEntryKind::Session { flat_entry } => {
                            let s = &flat_entry.session;
                            let tree_prefix = if flat_entry.depth == 0 {
                                "  ".to_string()
                            } else if flat_entry.is_last_child {
                                "  └─ ".to_string()
                            } else {
                                "  ├─ ".to_string()
                            };
                            nex_render::renderer::SessionEntryKind::Session {
                                project_name: s.project_name.clone(),
                                summary: s.summary.clone(),
                                time_ago: format_time_ago(s.updated_at),
                                message_count: s.message_count,
                                model: s.model.clone().unwrap_or_default(),
                                is_active: browser.is_active_in_nexterm(&s.id),
                                depth: flat_entry.depth,
                                tree_prefix,
                            }
                        }
                    };
                    nex_render::renderer::SessionOverlayEntry {
                        kind,
                        is_selected: i == browser.selected_index,
                    }
                })
                .collect();
            nex_render::renderer::SessionOverlay {
                entries,
                selected_index: browser.selected_index,
                filter: filter_display,
            }
        });

        if let Err(e) = renderer.render_frame(visible_tabs, tab_h, &pane_contents, &divider_lines, overlay.as_ref(), session_overlay.as_ref()) {
            tracing::error!("Render error: {e}");
        }

        if bell_active {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }

        // Clean up finished AI sessions (detect when claude --resume exits)
        if let (Some(mux), Some(store)) = (&mut self.mux, &self.block_store) {
            mux.cleanup_finished_sessions(store);
        }

        // Update MCP terminal state snapshot
        if let (Some(ts), Some(mux), Some(store)) =
            (&self.terminal_state, &self.mux, &self.block_store)
        {
            let pane_infos: Vec<nex_mcp::PaneInfo> = mux.panes_in_active_tab().iter().map(|pane| {
                let recent = store.get_recent(&pane.id, 1);
                let last_exit = recent.first().and_then(|b| b.exit_code);
                let cwd = recent.first()
                    .map(|b| b.cwd.display().to_string())
                    .unwrap_or_default();
                nex_mcp::PaneInfo {
                    id: pane.id.to_string(),
                    tab_title: mux.tab_titles().iter()
                        .find(|(_, _, active)| *active)
                        .map(|(_, title, _)| title.to_string())
                        .unwrap_or_default(),
                    cwd,
                    term_rows: pane.term_size.rows,
                    term_cols: pane.term_size.cols,
                    last_exit_code: last_exit,
                }
            }).collect();

            let active_id = mux.focused_pane().map(|p| p.id.to_string());
            let mut state = ts.lock();
            state.panes = pane_infos;
            state.active_pane_id = active_id;
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

/// Find the Claude CLI binary, checking common install locations.
fn find_claude_cli() -> std::path::PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    [
        home.join(".local/bin/claude"),
        home.join(".claude/bin/claude"),
        std::path::PathBuf::from("/usr/local/bin/claude"),
        std::path::PathBuf::from("/opt/homebrew/bin/claude"),
    ]
    .into_iter()
    .find(|p| p.exists())
    .unwrap_or_else(|| std::path::PathBuf::from("claude"))
}

/// Format a timestamp as a human-readable "time ago" string.
fn format_time_ago(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(dt);
    let secs = duration.num_seconds();
    if secs < 60 { return "just now".to_string(); }
    let mins = duration.num_minutes();
    if mins < 60 { return format!("{mins} min ago"); }
    let hours = duration.num_hours();
    if hours < 24 { return format!("{hours}h ago"); }
    let days = duration.num_days();
    if days < 30 { return format!("{days}d ago"); }
    format!("{}mo ago", days / 30)
}

/// Best-effort MCP deregistration with Claude Code on shutdown.
fn deregister_mcp() {
    let claude_path = find_claude_cli();

    match std::process::Command::new(&claude_path)
        .args(["mcp", "remove", "nexterm", "--scope", "user"])
        .output()
    {
        Ok(output) if output.status.success() => {
            tracing::info!("Deregistered MCP from Claude Code");
        }
        _ => {}
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        "nexterm=debug,nex_render=debug,nex_pty=debug,nex_mux=debug,nex_block=debug,nex_shell_integration=debug"
    } else {
        "nexterm=info"
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .init();

    tracing::info!("Starting Nexterm v{}", env!("CARGO_PKG_VERSION"));

    let config = nex_config::load_config();
    let shell = cli.shell
        .or(config.general.shell.clone())
        .unwrap_or_else(nex_pty::default_shell);

    tracing::info!("Using shell: {shell}");

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let proxy = WinitProxy(event_loop.create_proxy());
    let mut app = App::new(shell, proxy);
    event_loop.run_app(&mut app)?;

    Ok(())
}

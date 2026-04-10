//! Tonn - AI-First Terminal Emulator
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
use nex_render::renderer::{BgCell, PaneContent, Renderer, RenderSpan, SelectionCell, SettingsFieldType, DEFAULT_FONT_SIZE, FONT_SIZE_STEP};

// UI constants
const APP_TITLE: &str = "Tonn";
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
const UNFOCUSED_REDRAW_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
use nex_terminal::{
    Column, Line, Point, Selection, SelectionType, Side,
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
#[command(name = "tonn", about = "AI-First Terminal Emulator")]
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

/// Filter tabs for the session browser.
const SESSION_FILTER_ALL: &str = "All";
const SESSION_FILTER_ACTIVE: &str = "Active";

struct SessionBrowser {
    trees: Vec<nex_ai_session::SessionTree>,
    tonn_active_ids: Vec<String>,
    tool_names: Vec<String>,        // provider names from registered providers
    selected_tab: usize,            // 0=All, 1..N=tool, N+1=Active
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
    fn new(
        trees: Vec<nex_ai_session::SessionTree>,
        tonn_active_ids: Vec<String>,
        tool_names: Vec<String>,
    ) -> Self {
        let expanded_projects: std::collections::HashSet<String> =
            trees.iter().map(|t| t.project_name.clone()).collect();
        let mut browser = Self {
            trees,
            tonn_active_ids,
            tool_names,
            selected_tab: 0,
            display_entries: Vec::new(),
            selected_index: 0,
            filter: String::new(),
            active_only: false,
            expanded_projects,
        };
        browser.rebuild_display();
        browser
    }

    fn tab_labels(&self) -> Vec<String> {
        let mut labels = vec![SESSION_FILTER_ALL.to_string()];
        labels.extend(self.tool_names.clone());
        labels.push(SESSION_FILTER_ACTIVE.to_string());
        labels
    }

    fn cycle_tab(&mut self, forward: bool) {
        let count = self.tab_labels().len();
        if forward {
            self.selected_tab = (self.selected_tab + 1) % count;
        } else {
            self.selected_tab = (self.selected_tab + count - 1) % count;
        }
        // Update active_only based on tab
        let labels = self.tab_labels();
        self.active_only = labels.get(self.selected_tab).is_some_and(|l| l == SESSION_FILTER_ACTIVE);
        self.rebuild_display();
    }

    fn selected_tool_filter(&self) -> Option<&str> {
        let labels = self.tab_labels();
        let label = labels.get(self.selected_tab)?;
        if label == SESSION_FILTER_ALL || label == SESSION_FILTER_ACTIVE {
            None
        } else {
            Some(self.tool_names.get(self.selected_tab - 1).map(|s| s.as_str())?)
        }
    }

    fn rebuild_display(&mut self) {
        self.display_entries.clear();
        let query = self.filter.to_lowercase();
        let tool_filter = self.selected_tool_filter().map(|s| s.to_string());

        for tree in &self.trees {
            let mut flat: Vec<nex_ai_session::FlatSessionEntry> = Vec::new();
            for (i, root) in tree.roots.iter().enumerate() {
                root.flatten(0, i == tree.roots.len() - 1, &mut flat);
            }

            let filtered_flat: Vec<nex_ai_session::FlatSessionEntry> = flat
                .into_iter()
                .filter(|f| {
                    // Tool filter
                    if let Some(ref tool_name) = tool_filter
                        && f.session.tool.to_string() != *tool_name {
                            return false;
                        }
                    if self.active_only && !self.is_active_in_tonn(&f.session.id) {
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

    fn toggle_expand(&mut self) {
        if let Some(entry) = self.display_entries.get(self.selected_index)
            && let DisplayEntryKind::ProjectHeader { name, .. } = &entry.kind {
                let name = name.clone();
                if self.expanded_projects.contains(&name) {
                    self.expanded_projects.remove(&name);
                } else {
                    self.expanded_projects.insert(name);
                }
                self.rebuild_display();
            }
    }

    fn expand_at_selection(&mut self) {
        if let Some(entry) = self.display_entries.get(self.selected_index)
            && let DisplayEntryKind::ProjectHeader { name, expanded, .. } = &entry.kind
                && !expanded {
                    self.expanded_projects.insert(name.clone());
                    self.rebuild_display();
                }
    }

    fn collapse_at_selection(&mut self) {
        if let Some(entry) = self.display_entries.get(self.selected_index)
            && let DisplayEntryKind::ProjectHeader { name, expanded, .. } = &entry.kind
                && *expanded {
                    self.expanded_projects.remove(name);
                    self.rebuild_display();
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

    fn is_active_in_tonn(&self, session_id: &str) -> bool {
        self.tonn_active_ids.iter().any(|id| id == session_id)
    }
}

struct SelectPicker {
    options: Vec<String>,
    filtered: Vec<usize>,
    selected: usize,
    filter: String,
    target_key: String,
    original_value: String,
}

impl SelectPicker {
    fn new(options: Vec<String>, target_key: String, current_value: String) -> Self {
        let filtered: Vec<usize> = (0..options.len()).collect();
        let selected = options.iter().position(|o| o == &current_value).unwrap_or(0);
        Self { options, filtered, selected, filter: String::new(), target_key, original_value: current_value }
    }

    fn apply_filter(&mut self) {
        let query = self.filter.to_lowercase();
        self.filtered = if query.is_empty() {
            (0..self.options.len()).collect()
        } else {
            self.options.iter().enumerate()
                .filter(|(_, o)| o.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect()
        };
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    fn selected_option(&self) -> Option<&str> {
        self.filtered.get(self.selected)
            .and_then(|&idx| self.options.get(idx))
            .map(|s| s.as_str())
    }
}

struct SettingsPanel {
    config: nex_config::TonnConfig,
    #[allow(dead_code)]
    original: nex_config::TonnConfig,
    fields: Vec<SettingItem>,
    selected: usize,
    editing: bool,
    edit_buffer: String,
    picker: Option<SelectPicker>,
}

struct SettingItem {
    section: String,
    label: String,
    key: String,
    field_type: SettingsFieldType,
}

fn build_settings_fields(font_families: Vec<String>) -> Vec<SettingItem> {
    vec![
        SettingItem { section: "General".to_string(), label: "Shell".to_string(), key: "general.shell".to_string(), field_type: SettingsFieldType::Text },
        SettingItem { section: "General".to_string(), label: "Font".to_string(), key: "general.font_family".to_string(), field_type: SettingsFieldType::Select(font_families) },
        SettingItem { section: "General".to_string(), label: "Font Size".to_string(), key: "general.font_size".to_string(), field_type: SettingsFieldType::Number },
        SettingItem { section: "General".to_string(), label: "Theme".to_string(), key: "general.theme".to_string(), field_type: SettingsFieldType::Select(nex_config::AVAILABLE_THEMES.iter().map(|s| s.to_string()).collect()) },
        SettingItem { section: "General".to_string(), label: "Auto Update".to_string(), key: "general.auto_update".to_string(), field_type: SettingsFieldType::Toggle },
        SettingItem { section: "Terminal".to_string(), label: "Scrollback".to_string(), key: "general.scrollback_history".to_string(), field_type: SettingsFieldType::Number },
        SettingItem { section: "MCP Server".to_string(), label: "Enabled".to_string(), key: "mcp.enabled".to_string(), field_type: SettingsFieldType::Toggle },
    ]
}

impl SettingsPanel {
    fn new(config: nex_config::TonnConfig, font_families: Vec<String>) -> Self {
        Self {
            original: config.clone(),
            config,
            fields: build_settings_fields(font_families),
            selected: 0,
            editing: false,
            edit_buffer: String::new(),
            picker: None,
        }
    }

    fn get_value(&self, key: &str) -> String {
        match key {
            "general.shell" => self.config.general.shell.clone().unwrap_or_default(),
            "general.font_family" => {
                if self.config.general.font_family.is_empty() {
                    "System Default (Monospace)".to_string()
                } else {
                    self.config.general.font_family.clone()
                }
            }
            "general.font_size" => self.config.general.font_size.to_string(),
            "general.theme" => self.config.general.theme.clone(),
            "general.auto_update" => if self.config.general.auto_update { "On" } else { "Off" }.to_string(),
            "general.scrollback_history" => self.config.general.scrollback_history.to_string(),
            "mcp.enabled" => if self.config.mcp.enabled { "On" } else { "Off" }.to_string(),
            _ => String::new(),
        }
    }

    fn set_value(&mut self, key: &str, value: &str) {
        match key {
            "general.shell" => self.config.general.shell = if value.is_empty() { None } else { Some(value.to_string()) },
            "general.font_family" => {
                self.config.general.font_family = if value.starts_with("System Default") {
                    String::new()
                } else {
                    value.to_string()
                };
            }
            "general.font_size" => if let Ok(v) = value.parse() { self.config.general.font_size = v; },
            "general.theme" => self.config.general.theme = value.to_string(),
            "general.auto_update" => self.config.general.auto_update = value == "On",
            "general.scrollback_history" => if let Ok(v) = value.parse() { self.config.general.scrollback_history = v; },
            "mcp.enabled" => self.config.mcp.enabled = value == "On",
            _ => {}
        }
    }

    fn total_fields(&self) -> usize {
        self.fields.len()
    }
}

fn save_config(config: &nex_config::TonnConfig) {
    let dir = nex_config::config_dir();
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("config.toml");
    if let Ok(toml_str) = toml::to_string_pretty(config) {
        std::fs::write(&path, toml_str).ok();
    }
}

struct App {
    shell: String,
    config: nex_config::TonnConfig,
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
    settings_panel: Option<SettingsPanel>,
    session_manager: Option<Arc<nex_ai_session::SessionManager>>,
    last_tab_count: usize,
    last_active_tab: usize,
    terminal_state: Option<Arc<parking_lot::Mutex<nex_mcp::TerminalStateSnapshot>>>,
    block_store: Option<Arc<nex_block::BlockStore>>,
    pending_resize: Option<(u32, u32, std::time::Instant)>,
    last_slow_update: Option<std::time::Instant>,
    last_unfocused_redraw: Option<std::time::Instant>,
    window_focused: bool,
}

impl App {
    fn new(shell: String, config: nex_config::TonnConfig, proxy: WinitProxy) -> Self {
        Self {
            shell,
            config,
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
            settings_panel: None,
            session_manager: None,
            last_tab_count: 1,
            last_active_tab: 0,
            terminal_state: None,
            block_store: None,
            pending_resize: None,
            last_slow_update: None,
            last_unfocused_redraw: None,
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
                if let Some(window) = &self.window {
                    if self.window_focused {
                        window.request_redraw();
                    } else {
                        // Throttle redraws when unfocused so output is still
                        // visible without wasting GPU cycles.
                        let now = std::time::Instant::now();
                        let due = self.last_unfocused_redraw
                            .is_none_or(|t| now.duration_since(t) >= UNFOCUSED_REDRAW_INTERVAL);
                        if due {
                            self.last_unfocused_redraw = Some(now);
                            window.request_redraw();
                        }
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

        let theme = self.config.theme();
        let renderer = pollster::block_on(Renderer::new(
            Arc::clone(&window),
            theme,
            self.config.general.font_family.clone(),
            self.config.general.font_size,
        ));
        match renderer {
            Ok(r) => {
                self.renderer = Some(r);
                let pane_area = Self::pane_area(self.renderer.as_ref().unwrap(), 1);
                let renderer = self.renderer.as_ref().unwrap();

                let (block_tx, block_rx) = nex_ipc::block_channel();

                // Pre-allocate MCP port so PTY shells get TONN_MCP_PORT
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
                    self.config.general.scrollback_history,
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

                let mcp_server = nex_mcp::TonnMcpServer::new(
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
                                .args(["mcp", "remove", "tonn", "--scope", "user"])
                                .output()
                                .await;

                            // Register with new port
                            let mcp_url = format!("http://127.0.0.1:{port}/mcp");
                            let register_result = tokio::process::Command::new(&claude_path)
                                .args(["mcp", "add",
                                       "--transport", "http",
                                       "--scope", "user",
                                       "tonn",
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

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let new_scale = scale_factor as f32;
                let changed = self.renderer.as_mut()
                    .is_some_and(|r| r.set_scale_factor(new_scale));

                if changed {
                    // Defensive surface reconfigure in case no Resized follows.
                    if let (Some(renderer), Some(window)) = (&mut self.renderer, &self.window) {
                        let size = window.inner_size();
                        renderer.resize(size.width, size.height);
                    }

                    if let Some(mux) = &mut self.mux {
                        mux.set_scale_factor(new_scale);
                        let area = Self::pane_area(
                            self.renderer.as_ref().unwrap(),
                            mux.tab_count(),
                        );
                        mux.recalculate_bounds(area);

                        // Mark active-tab panes dirty so their text buffers
                        // reshape on the next render.
                        for pane in mux.panes_in_active_tab() {
                            pane.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }

                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
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
                if scroll_lines != 0
                    && let Some(mux) = &self.mux
                        && let Some(pane) = mux.focused_pane() {
                            let mut term = pane.terminal.lock();
                            term.grid_mut().scroll_display(
                                nex_terminal::Scroll::Delta(scroll_lines),
                            );
                            pane.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = (position.x, position.y);
                if self.mouse_selecting {
                    if let Some((point, side)) = self.pixel_to_grid(position.x, position.y)
                        && let Some(mux) = &self.mux
                            && let Some(pane) = mux.focused_pane() {
                                let mut term = pane.terminal.lock();
                                if let Some(ref mut sel) = term.selection {
                                    sel.update(point, side);
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
                        if let Some((point, side)) = self.pixel_to_grid(self.mouse_pos.0, self.mouse_pos.1)
                            && let Some(mux) = &self.mux
                                && let Some(pane) = mux.focused_pane() {
                                    pane.terminal.lock().selection =
                                        Some(Selection::new(SelectionType::Simple, point, side));
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
                if focused
                    && let Some(window) = &self.window {
                        window.request_redraw();
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
        if let Some(mux) = &self.mux
            && let Some(pane) = mux.focused_pane() {
                let mut term = pane.terminal.lock();
                if term.grid().display_offset() > 0 {
                    term.grid_mut()
                        .scroll_display(nex_terminal::Scroll::Bottom);
                    pane.dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }

        let ctrl = self.modifiers.state().control_key();
        let super_key = self.modifiers.state().super_key();
        let shift = self.modifiers.state().shift_key();
        let alt = self.modifiers.state().alt_key();

        // --- Settings panel (Cmd+,) ---
        if super_key && matches!(logical_key, Key::Character(c) if c.as_str() == ",") {
            if self.settings_panel.is_some() {
                if let Some(panel) = self.settings_panel.take() {
                    save_config(&panel.config);
                    self.config = panel.config;
                }
            } else {
                let font_families = self.renderer.as_ref()
                    .map(|r| r.list_font_families())
                    .unwrap_or_default();
                self.settings_panel = Some(SettingsPanel::new(self.config.clone(), font_families));
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }

        // Settings panel navigation (when open)
        if self.settings_panel.is_some() {
            self.handle_settings_key(logical_key, text, ctrl, super_key);
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }

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
                let tool_names = mgr.provider_names();
                self.session_browser = Some(SessionBrowser::new(trees, active_ids, tool_names));
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
                    browser.cycle_tab(!shift);
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
                    if let Some(text) = text
                        && !ctrl && !super_key {
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
            if let Ok(mut clipboard) = arboard::Clipboard::new()
                && let Ok(text) = clipboard.get_text() {
                    self.write_to_focused(b"\x1b[200~");
                    self.write_to_focused(text.as_bytes());
                    self.write_to_focused(b"\x1b[201~");
                }
            return;
        }

        // --- Copy (Cmd+C / Ctrl+Shift+C) ---
        if matches!(logical_key, Key::Character(c) if c.as_str() == "c")
            && (super_key || (ctrl && shift))
        {
            if let Some(mux) = &self.mux
                && let Some(pane) = mux.focused_pane()
                    && let Some(text) = pane.terminal.lock().selection_to_string()
                        && let Ok(mut clipboard) = arboard::Clipboard::new() {
                            let _ = clipboard.set_text(text);
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
                if let Some(renderer) = &mut self.renderer
                    && renderer.set_font_size(size) {
                        if let Some(mux) = &mut self.mux {
                            mux.update_font(renderer.font_size(), renderer.scale_factor());
                            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                            mux.recalculate_bounds(area);
                        }
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                return;
            }
        }


        // --- PTY input ---
        let key_input = map_winit_key(logical_key, text);
        let wrote = if let Some(ref key_input) = key_input {
            let mods = nex_terminal::keys::Modifiers { shift, ctrl, alt };
            let mode = self.mux.as_ref()
                .and_then(|m| m.focused_pane())
                .map(|p| {
                    let term = p.terminal.lock();
                    nex_terminal::keys::Mode {
                        app_cursor: term.mode().contains(nex_terminal::TermMode::APP_CURSOR),
                    }
                })
                .unwrap_or_default();
            if let Some(data) = nex_terminal::keys::encode(key_input, &mods, &mode) {
                self.write_to_focused(&data)
            } else {
                false
            }
        } else {
            false
        };

        if wrote {
            // Clear selection after typing
            if let Some(mux) = &self.mux
                && let Some(pane) = mux.focused_pane() {
                    pane.terminal.lock().selection = None;
                }
        }
    }
}

/// Convert a winit key + text into the windowing-agnostic key type.
fn map_winit_key(key: &Key, text: Option<&str>) -> Option<nex_terminal::keys::Key> {
    use nex_terminal::keys::{Key as K, NamedKey as NK};
    match key {
        Key::Named(NamedKey::Enter) => Some(K::Named(NK::Enter)),
        Key::Named(NamedKey::Backspace) => Some(K::Named(NK::Backspace)),
        Key::Named(NamedKey::Tab) => Some(K::Named(NK::Tab)),
        Key::Named(NamedKey::Escape) => Some(K::Named(NK::Escape)),
        Key::Named(NamedKey::ArrowUp) => Some(K::Named(NK::ArrowUp)),
        Key::Named(NamedKey::ArrowDown) => Some(K::Named(NK::ArrowDown)),
        Key::Named(NamedKey::ArrowLeft) => Some(K::Named(NK::ArrowLeft)),
        Key::Named(NamedKey::ArrowRight) => Some(K::Named(NK::ArrowRight)),
        Key::Named(NamedKey::Home) => Some(K::Named(NK::Home)),
        Key::Named(NamedKey::End) => Some(K::Named(NK::End)),
        Key::Named(NamedKey::Insert) => Some(K::Named(NK::Insert)),
        Key::Named(NamedKey::Delete) => Some(K::Named(NK::Delete)),
        Key::Named(NamedKey::PageUp) => Some(K::Named(NK::PageUp)),
        Key::Named(NamedKey::PageDown) => Some(K::Named(NK::PageDown)),
        Key::Named(NamedKey::F1) => Some(K::Named(NK::F(1))),
        Key::Named(NamedKey::F2) => Some(K::Named(NK::F(2))),
        Key::Named(NamedKey::F3) => Some(K::Named(NK::F(3))),
        Key::Named(NamedKey::F4) => Some(K::Named(NK::F(4))),
        Key::Named(NamedKey::F5) => Some(K::Named(NK::F(5))),
        Key::Named(NamedKey::F6) => Some(K::Named(NK::F(6))),
        Key::Named(NamedKey::F7) => Some(K::Named(NK::F(7))),
        Key::Named(NamedKey::F8) => Some(K::Named(NK::F(8))),
        Key::Named(NamedKey::F9) => Some(K::Named(NK::F(9))),
        Key::Named(NamedKey::F10) => Some(K::Named(NK::F(10))),
        Key::Named(NamedKey::F11) => Some(K::Named(NK::F(11))),
        Key::Named(NamedKey::F12) => Some(K::Named(NK::F(12))),
        Key::Character(c) => c.chars().next().map(K::Char),
        _ => text.and_then(|t| t.chars().next()).map(K::Char),
    }
}

// ---------------------------------------------------------------------------
// Settings panel
// ---------------------------------------------------------------------------

impl App {
    fn handle_settings_key(&mut self, logical_key: &Key, text: Option<&str>, ctrl: bool, super_key: bool) {
        // When picker is open, route all keys to picker
        let picker_open = self.settings_panel.as_ref().is_some_and(|p| p.picker.is_some());
        if picker_open {
            self.handle_picker_key(logical_key, text, ctrl, super_key);
            return;
        }

        match logical_key {
            Key::Named(NamedKey::Escape) => {
                let editing = self.settings_panel.as_ref().is_some_and(|p| p.editing);
                if editing {
                    let panel = self.settings_panel.as_mut().unwrap();
                    panel.editing = false;
                    panel.edit_buffer.clear();
                } else if let Some(panel) = self.settings_panel.take() {
                    save_config(&panel.config);
                    self.config = panel.config;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let panel = self.settings_panel.as_mut().unwrap();
                if !panel.editing && panel.selected > 0 {
                    panel.selected -= 1;
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                let panel = self.settings_panel.as_mut().unwrap();
                if !panel.editing && panel.selected + 1 < panel.total_fields() {
                    panel.selected += 1;
                }
            }
            Key::Named(NamedKey::Enter) => {
                self.handle_settings_enter();
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(panel) = self.settings_panel.as_mut()
                    && panel.editing {
                        panel.edit_buffer.pop();
                    }
            }
            _ => {
                if let Some(panel) = self.settings_panel.as_mut()
                    && panel.editing
                        && let Some(text) = text
                            && !ctrl && !super_key {
                                panel.edit_buffer.push_str(text);
                            }
            }
        }
    }

    fn handle_picker_key(&mut self, logical_key: &Key, text: Option<&str>, ctrl: bool, super_key: bool) {
        let panel = self.settings_panel.as_mut().unwrap();
        let picker = panel.picker.as_mut().unwrap();

        match logical_key {
            Key::Named(NamedKey::Escape) => {
                let original = picker.original_value.clone();
                let target = picker.target_key.clone();
                panel.picker = None;
                panel.set_value(&target, &original);
                self.apply_settings_preview(&target);
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(option) = picker.selected_option() {
                    let value = option.to_string();
                    let target = picker.target_key.clone();
                    panel.picker = None;
                    panel.set_value(&target, &value);
                    self.apply_settings_preview(&target);
                } else {
                    panel.picker = None;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if picker.selected > 0 {
                    picker.selected -= 1;
                }
                // Live preview
                if let Some(option) = picker.selected_option() {
                    let value = option.to_string();
                    let target = picker.target_key.clone();
                    panel.set_value(&target, &value);
                    self.apply_settings_preview(&target);
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if picker.selected + 1 < picker.filtered.len() {
                    picker.selected += 1;
                }
                // Live preview
                if let Some(option) = picker.selected_option() {
                    let value = option.to_string();
                    let target = picker.target_key.clone();
                    panel.set_value(&target, &value);
                    self.apply_settings_preview(&target);
                }
            }
            Key::Named(NamedKey::Backspace) => {
                picker.filter.pop();
                picker.apply_filter();
                // Live preview
                if let Some(option) = picker.selected_option() {
                    let value = option.to_string();
                    let target = picker.target_key.clone();
                    panel.set_value(&target, &value);
                    self.apply_settings_preview(&target);
                }
            }
            _ => {
                if let Some(text) = text
                    && !ctrl && !super_key {
                        picker.filter.push_str(text);
                        picker.apply_filter();
                        // Live preview
                        if let Some(option) = picker.selected_option() {
                            let value = option.to_string();
                            let target = picker.target_key.clone();
                            panel.set_value(&target, &value);
                            self.apply_settings_preview(&target);
                        }
                    }
            }
        }
    }

    fn handle_settings_enter(&mut self) {
        let (key, field_type, editing, edit_value) = {
            let panel = self.settings_panel.as_ref().unwrap();
            let item = &panel.fields[panel.selected];
            (
                item.key.clone(),
                item.field_type.clone(),
                panel.editing,
                panel.edit_buffer.clone(),
            )
        };

        if editing {
            {
                let panel = self.settings_panel.as_mut().unwrap();
                panel.set_value(&key, &edit_value);
                panel.editing = false;
                panel.edit_buffer.clear();
            }
            self.apply_settings_preview(&key);
        } else {
            match field_type {
                SettingsFieldType::Toggle => {
                    let panel = self.settings_panel.as_mut().unwrap();
                    let current = panel.get_value(&key);
                    let new_val = if current == "On" { "Off" } else { "On" };
                    panel.set_value(&key, new_val);
                }
                SettingsFieldType::Select(ref opts) => {
                    let current = {
                        let panel = self.settings_panel.as_ref().unwrap();
                        panel.get_value(&key)
                    };
                    let panel = self.settings_panel.as_mut().unwrap();
                    panel.picker = Some(SelectPicker::new(opts.clone(), key.clone(), current));
                }
                _ => {
                    let panel = self.settings_panel.as_mut().unwrap();
                    panel.editing = true;
                    panel.edit_buffer = panel.get_value(&key);
                }
            }
        }
    }

    fn apply_settings_preview(&mut self, key: &str) {
        let (theme, font_family, font_size) = {
            let Some(panel) = &self.settings_panel else { return; };
            (
                panel.config.theme(),
                panel.config.general.font_family.clone(),
                panel.config.general.font_size,
            )
        };
        match key {
            "general.theme" => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_theme(theme);
                }
            }
            "general.font_family" => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_font_family(font_family);
                }
            }
            "general.font_size" => {
                if let Some(renderer) = &mut self.renderer
                    && renderer.set_font_size(font_size)
                        && let Some(mux) = &mut self.mux {
                            mux.update_font(renderer.font_size(), renderer.scale_factor());
                            let area = Self::pane_area(self.renderer.as_ref().unwrap(), mux.tab_count());
                            mux.recalculate_bounds(area);
                        }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Session resume
// ---------------------------------------------------------------------------

impl App {
    fn resume_session(&mut self, session: &nex_ai_session::AiSession) {
        let session_id = &session.id;
        let project_dir = session.project_dir.display();

        let command = self
            .session_manager
            .as_ref()
            .map(|mgr| mgr.resume_command(session))
            .unwrap_or_default();

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
        let theme = self.renderer.as_ref().unwrap().theme();
        let ansi_palette = theme.ansi_palette();
        let theme_fg = theme.fg;
        let theme_bg = theme.bg;
        let pane_contents: Vec<PaneContent> = mux.panes_in_active_tab().iter().map(|pane| {
            let is_dirty = pane.dirty.swap(false, std::sync::atomic::Ordering::Relaxed);
            let term = pane.terminal.lock();
            let content = read_grid_content(&term, &ansi_palette, theme_fg, theme_bg);

            // Only clone text spans when content has changed (dirty flag)
            let spans = if is_dirty {
                content.spans.iter().map(|s| RenderSpan {
                    text: s.text.clone(),
                    r: s.fg.r, g: s.fg.g, b: s.fg.b,
                    bold: s.bold, italic: s.italic,
                }).collect()
            } else {
                Vec::new() // renderer will reuse previous buffer
            };

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
                needs_reshape: is_dirty,
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
            // Build tab bar + filter display
            let tab_labels = browser.tab_labels();
            let tab_bar: String = tab_labels.iter().enumerate().map(|(i, label)| {
                if i == browser.selected_tab {
                    format!("[{label}]")
                } else {
                    format!(" {label} ")
                }
            }).collect::<Vec<_>>().join("  ");
            let filter_display = if browser.filter.is_empty() {
                format!("{tab_bar}\nSearch...")
            } else {
                format!("{tab_bar}\n{}", browser.filter)
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
                            } else {
                                // Check if this is the last visible sibling at this depth
                                let is_last_visible = {
                                    let current_idx = i;
                                    let mut is_last = true;
                                    for j in (current_idx + 1)..browser.display_entries.len() {
                                        if let DisplayEntryKind::Session { flat_entry: next } = &browser.display_entries[j].kind {
                                            if next.depth == flat_entry.depth {
                                                is_last = false;
                                                break;
                                            }
                                            if next.depth < flat_entry.depth {
                                                break;
                                            }
                                        } else {
                                            break;
                                        }
                                    }
                                    is_last
                                };
                                // Indent deeper levels further
                                let depth_indent = "   ".repeat(flat_entry.depth);
                                let connector = if is_last_visible { "└─ " } else { "├─ " };
                                format!("{depth_indent}{connector}")
                            };
                            nex_render::renderer::SessionEntryKind::Session {
                                project_name: s.project_name.clone(),
                                summary: s.summary.clone(),
                                time_ago: format_time_ago(s.updated_at),
                                message_count: s.message_count,
                                model: s.model.clone().unwrap_or_default(),
                                is_active: browser.is_active_in_tonn(&s.id),
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

        // Build settings overlay if active
        let settings_overlay = self.settings_panel.as_ref().map(|panel| {
            let mut sections: Vec<nex_render::renderer::SettingsSection> = Vec::new();
            let mut current_section_title = String::new();
            let mut current_fields: Vec<nex_render::renderer::SettingsField> = Vec::new();

            for item in &panel.fields {
                if item.section != current_section_title {
                    if !current_section_title.is_empty() {
                        sections.push(nex_render::renderer::SettingsSection {
                            title: current_section_title.clone(),
                            fields: std::mem::take(&mut current_fields),
                        });
                    }
                    current_section_title = item.section.clone();
                }
                current_fields.push(nex_render::renderer::SettingsField {
                    label: item.label.clone(),
                    value: panel.get_value(&item.key),
                    field_type: item.field_type.clone(),
                });
            }
            if !current_section_title.is_empty() {
                sections.push(nex_render::renderer::SettingsSection {
                    title: current_section_title,
                    fields: current_fields,
                });
            }

            nex_render::renderer::SettingsOverlay {
                sections,
                selected_row: panel.selected,
                editing: panel.editing,
                edit_value: panel.edit_buffer.clone(),
                picker: panel.picker.as_ref().map(|picker| {
                    let field_label = panel.fields.iter()
                        .find(|f| f.key == picker.target_key)
                        .map(|f| f.label.clone())
                        .unwrap_or_else(|| "Select".to_string());
                    nex_render::renderer::PickerOverlay {
                        title: format!("Select {field_label}"),
                        entries: picker.filtered.iter().map(|&idx| {
                            let label = picker.options[idx].clone();
                            let is_current = label == picker.original_value;
                            nex_render::renderer::PickerEntry { label, is_current }
                        }).collect(),
                        selected_index: picker.selected,
                        filter: picker.filter.clone(),
                    }
                }),
            }
        });

        if let Err(e) = renderer.render_frame(visible_tabs, tab_h, &pane_contents, &divider_lines, overlay.as_ref(), session_overlay.as_ref(), settings_overlay.as_ref()) {
            tracing::error!("Render error: {e}");
        }

        if bell_active
            && let Some(window) = &self.window {
                window.request_redraw();
            }

        // Throttle slow updates (session cleanup + MCP state) to once per second
        let now = std::time::Instant::now();
        let should_slow_update = self.last_slow_update
            .map(|t| now.duration_since(t).as_secs() >= 1)
            .unwrap_or(true);

        if should_slow_update {
            self.last_slow_update = Some(now);

            // Clean up finished AI sessions
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
        .args(["mcp", "remove", "tonn", "--scope", "user"])
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
        "tonn=debug,nex_render=debug,nex_pty=debug,nex_mux=debug,nex_block=debug,nex_shell_integration=debug"
    } else {
        "tonn=info"
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .init();

    tracing::info!("Starting Tonn v{}", env!("CARGO_PKG_VERSION"));

    let config = nex_config::load_config();
    let shell = cli.shell
        .or(config.general.shell.clone())
        .unwrap_or_else(nex_pty::default_shell);

    tracing::info!("Using shell: {shell}");

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let proxy = WinitProxy(event_loop.create_proxy());
    let mut app = App::new(shell, config, proxy);
    event_loop.run_app(&mut app)?;

    Ok(())
}

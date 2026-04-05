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
use nex_terminal::{
    Column, Dimensions, Line, Point, Selection, SelectionType, Side,
    read_grid_content,
};

// ---------------------------------------------------------------------------
// UserEvent + MuxEventProxy bridge
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum UserEvent {
    PtyExited(PaneId),
    Title(PaneId, String),
    ResetTitle(PaneId),
    Bell(PaneId),
    Redraw,
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
    last_tab_count: usize,
    last_active_tab: usize,
    /// Pending resize: we debounce rapid resize events and only apply the final one.
    pending_resize: Option<(u32, u32, std::time::Instant)>,
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
            last_tab_count: 1,
            last_active_tab: 0,
            pending_resize: None,
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
                    window.request_redraw();
                }
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
                )
                .expect("Failed to create mux");

                tracing::info!("Mux initialized with 1 tab, 1 pane");
                self.mux = Some(mux);
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

            WindowEvent::Focused(true) => {
                if let Some(window) = &self.window {
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

        if let Err(e) = renderer.render_frame(visible_tabs, tab_h, &pane_contents, &divider_lines, overlay.as_ref()) {
            tracing::error!("Render error: {e}");
        }

        if bell_active {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

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

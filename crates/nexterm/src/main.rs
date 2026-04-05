//! Nexterm - AI-First Terminal Emulator
//!
//! Main binary that orchestrates:
//! - winit window + wgpu GPU rendering
//! - PTY spawn with user's default shell
//! - VT terminal emulation via alacritty_terminal
//! - I/O thread for PTY communication
//! - Proper cursor tracking and scrollback

use std::io::{Read, Write};
use std::sync::{Arc, mpsc};

use anyhow::Result;
use clap::Parser;
use parking_lot::Mutex;
use tracing_subscriber::EnvFilter;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Custom events sent to the winit event loop from background threads.
#[derive(Debug, Clone)]
enum UserEvent {
    PtyExited,
    Title(String),
    ResetTitle,
    Bell,
}

use nex_common::PaneId;
use nex_config::NextermConfig;
use nex_pty::NexPty;
use nex_render::renderer::{Renderer, RenderSpan, SelectionCell, DEFAULT_FONT_SIZE, FONT_SIZE_STEP};
use nex_terminal::{
    Column, Dimensions, Line, NexEventListener, Point, Selection, SelectionType, Side, Term,
    TermConfig, TermSize, ansi, read_grid_content,
};

#[derive(Parser, Debug)]
#[command(name = "nexterm", about = "AI-First Terminal Emulator")]
struct Cli {
    /// Shell command to execute
    #[arg(short, long)]
    shell: Option<String>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

struct App {
    config: NextermConfig,
    shell: String,
    event_proxy: EventLoopProxy<UserEvent>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty_writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    terminal: Option<Arc<Mutex<Term<NexEventListener>>>>,
    pty: Option<NexPty>,
    modifiers: Modifiers,
    mouse_pos: (f64, f64),
    mouse_selecting: bool,
    bell_flash_until: Option<std::time::Instant>,
}

impl App {
    fn new(config: NextermConfig, shell: String, event_proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            config,
            shell,
            event_proxy,
            window: None,
            renderer: None,
            pty_writer: None,
            terminal: None,
            pty: None,
            modifiers: Modifiers::default(),
            mouse_pos: (0.0, 0.0),
            mouse_selecting: false,
            bell_flash_until: None,
        }
    }

    /// Convert pixel position to terminal grid point.
    fn pixel_to_grid(&self, x: f64, y: f64) -> (Point, Side) {
        let renderer = self.renderer.as_ref().unwrap();
        let cell_width = renderer.font_size() * 0.6;
        let line_height = renderer.font_size() * 1.3;
        let padding = 8.0;

        let col = ((x as f32 - padding) / cell_width).max(0.0) as usize;
        let row = ((y as f32 - padding) / line_height).max(0.0) as usize;

        let (term_rows, term_cols) = renderer.terminal_size();
        let col = col.min(term_cols as usize - 1);
        let row = row.min(term_rows as usize - 1);

        let display_offset = self
            .terminal
            .as_ref()
            .map(|t| t.lock().grid().display_offset())
            .unwrap_or(0);

        let line = Line(row as i32 - display_offset as i32);
        let side = if (x as f32 - padding) % cell_width > cell_width / 2.0 {
            Side::Right
        } else {
            Side::Left
        };

        (Point::new(line, Column(col)), side)
    }

    fn spawn_pty(&mut self, rows: u16, cols: u16) {
        let size = nex_common::TerminalSize { rows, cols };

        match NexPty::spawn(&self.shell, size) {
            Ok((pty, reader, writer)) => {
                let pty_writer = Arc::new(Mutex::new(writer));
                self.pty_writer = Some(Arc::clone(&pty_writer));
                self.pty = Some(pty);

                // Channel for PtyWrite events (DSR responses, etc.)
                let (pty_write_tx, pty_write_rx) = mpsc::channel::<String>();

                // Create alacritty_terminal Term for VT emulation
                let term_config = TermConfig {
                    scrolling_history: 10_000,
                    ..Default::default()
                };
                let term_size = TermSize::new(cols as usize, rows as usize);
                let proxy_for_listener = self.event_proxy.clone();
                let event_callback: nex_terminal::EventCallback = Box::new(move |event| {
                    match event {
                        nex_terminal::TerminalEvent::Title(title) => {
                            let _ = proxy_for_listener.send_event(UserEvent::Title(title));
                        }
                        nex_terminal::TerminalEvent::ResetTitle => {
                            let _ = proxy_for_listener.send_event(UserEvent::ResetTitle);
                        }
                        nex_terminal::TerminalEvent::Bell => {
                            let _ = proxy_for_listener.send_event(UserEvent::Bell);
                        }
                        _ => {}
                    }
                });
                let event_listener = NexEventListener::new(PaneId::new(), pty_write_tx, event_callback);
                let term = Term::new(term_config, &term_size, event_listener);
                let terminal = Arc::new(Mutex::new(term));
                self.terminal = Some(Arc::clone(&terminal));

                // Spawn I/O reader thread
                let window = self.window.clone();
                let proxy = self.event_proxy.clone();
                std::thread::Builder::new()
                    .name("nexterm-io".to_string())
                    .spawn(move || {
                        io_thread(reader, terminal, window, proxy);
                    })
                    .expect("Failed to spawn I/O thread");

                // Spawn PtyWrite forwarding thread (for DSR responses)
                let pty_writer_for_events = Arc::clone(&self.pty_writer.as_ref().unwrap());
                std::thread::Builder::new()
                    .name("nexterm-pty-write".to_string())
                    .spawn(move || {
                        while let Ok(text) = pty_write_rx.recv() {
                            let mut writer = pty_writer_for_events.lock();
                            let _ = writer.write_all(text.as_bytes());
                            let _ = writer.flush();
                        }
                    })
                    .expect("Failed to spawn PTY write thread");

                tracing::info!("PTY spawned: shell={}, size={}x{}", self.shell, cols, rows);
            }
            Err(e) => {
                tracing::error!("Failed to spawn PTY: {e}");
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::PtyExited => {
                tracing::info!("Shell exited, closing terminal");
                event_loop.exit();
            }
            UserEvent::Title(title) => {
                if let Some(window) = &self.window {
                    window.set_title(&title);
                }
            }
            UserEvent::ResetTitle => {
                if let Some(window) = &self.window {
                    window.set_title("Nexterm");
                }
            }
            UserEvent::Bell => {
                self.bell_flash_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(150));
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
            .with_title("Nexterm")
            .with_inner_size(winit::dpi::LogicalSize::new(960, 640));

        let window = Arc::new(event_loop.create_window(attrs).expect("Failed to create window"));
        self.window = Some(Arc::clone(&window));

        // Initialize renderer
        let renderer = pollster::block_on(Renderer::new(Arc::clone(&window)));
        match renderer {
            Ok(r) => {
                let (rows, cols) = r.terminal_size();
                self.renderer = Some(r);
                tracing::info!("GPU renderer initialized ({}x{})", cols, rows);

                // Spawn PTY with calculated terminal size
                self.spawn_pty(rows, cols);
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
                tracing::info!("Window close requested");
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                    let (rows, cols) = renderer.terminal_size();

                    // Resize PTY
                    if let Some(pty) = &self.pty {
                        let new_size = nex_common::TerminalSize { rows, cols };
                        let _ = pty.resize(new_size);
                    }

                    // Resize terminal emulator
                    if let Some(terminal) = &self.terminal {
                        let mut term = terminal.lock();
                        let term_size = TermSize::new(cols as usize, rows as usize);
                        term.resize(term_size);
                    }
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let scroll_lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as i32 * 3,
                    MouseScrollDelta::PixelDelta(pos) => (pos.y / 20.0) as i32,
                };

                if let Some(terminal) = &self.terminal {
                    let mut term = terminal.lock();
                    term.grid_mut().scroll_display(
                        alacritty_terminal::grid::Scroll::Delta(scroll_lines),
                    );
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = (position.x, position.y);
                if self.mouse_selecting {
                    let (point, side) = self.pixel_to_grid(position.x, position.y);
                    if let Some(terminal) = &self.terminal {
                        let mut term = terminal.lock();
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
                        let (point, side) = self.pixel_to_grid(self.mouse_pos.0, self.mouse_pos.1);
                        if let Some(terminal) = &self.terminal {
                            let mut term = terminal.lock();
                            term.selection = Some(Selection::new(SelectionType::Simple, point, side));
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
                self.modifiers = new_modifiers;
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => {
                // Reset scroll position to bottom on keypress
                if let Some(terminal) = &self.terminal {
                    let mut term = terminal.lock();
                    if term.grid().display_offset() > 0 {
                        term.grid_mut().scroll_display(
                            alacritty_terminal::grid::Scroll::Bottom,
                        );
                    }
                }

                let ctrl = self.modifiers.state().control_key();
                let super_key = self.modifiers.state().super_key();
                let shift = self.modifiers.state().shift_key();

                // Cmd+V (macOS) / Ctrl+Shift+V — paste from clipboard
                let paste_shortcut = match &logical_key {
                    Key::Character(c) if c.as_str() == "v" => {
                        super_key || (ctrl && shift)
                    }
                    _ => false,
                };
                if paste_shortcut {
                    if let Some(writer) = &self.pty_writer {
                        if let Ok(mut clipboard) = arboard::Clipboard::new() {
                            if let Ok(text) = clipboard.get_text() {
                                let mut writer = writer.lock();
                                // Bracket paste mode: wrap in \e[200~ ... \e[201~
                                let _ = writer.write_all(b"\x1b[200~");
                                let _ = writer.write_all(text.as_bytes());
                                let _ = writer.write_all(b"\x1b[201~");
                                let _ = writer.flush();
                            }
                        }
                    }
                    return;
                }

                // Cmd+C (macOS) / Ctrl+Shift+C — copy selection to clipboard
                let copy_shortcut = match &logical_key {
                    Key::Character(c) if c.as_str() == "c" => {
                        super_key || (ctrl && shift)
                    }
                    _ => false,
                };
                if copy_shortcut {
                    if let Some(terminal) = &self.terminal {
                        let term = terminal.lock();
                        if let Some(text) = term.selection_to_string() {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                let _ = clipboard.set_text(text);
                            }
                        }
                    }
                    return;
                }




                // Cmd+= / Cmd+- for font size (macOS), Ctrl+= / Ctrl+- elsewhere
                let zoom_mod = super_key || (cfg!(not(target_os = "macos")) && ctrl);
                if zoom_mod {
                    let new_size = match &logical_key {
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
                                let (rows, cols) = renderer.terminal_size();
                                if let Some(pty) = &self.pty {
                                    let _ = pty.resize(nex_common::TerminalSize { rows, cols });
                                }
                                if let Some(terminal) = &self.terminal {
                                    terminal.lock().resize(TermSize::new(cols as usize, rows as usize));
                                }
                                if let Some(window) = &self.window {
                                    window.request_redraw();
                                }
                            }
                        }
                        return;
                    }
                }


                if let Some(writer) = &self.pty_writer {
                    let mut writer = writer.lock();
                    let mut wrote = false;

                    // Handle Ctrl+key combinations first
                    if ctrl {
                        wrote = match &logical_key {
                            Key::Character(c) => {
                                let ch = c.chars().next().unwrap_or('\0');
                                if ch.is_ascii_alphabetic() {
                                    let ctrl_code = (ch.to_ascii_lowercase() as u8) - b'a' + 1;
                                    let _ = writer.write_all(&[ctrl_code]);
                                    true
                                } else {
                                    match ch {
                                        '[' | '3' => { let _ = writer.write_all(b"\x1b"); true }
                                        '\\' | '4' => { let _ = writer.write_all(b"\x1c"); true }
                                        ']' | '5' => { let _ = writer.write_all(b"\x1d"); true }
                                        '/' | '7' => { let _ = writer.write_all(b"\x1f"); true }
                                        ' ' | '2' => { let _ = writer.write_all(b"\x00"); true }
                                        _ => false,
                                    }
                                }
                            }
                            _ => false,
                        };
                    }

                    // Named keys and text input
                    if !wrote {
                        wrote = true;
                        match &logical_key {
                            Key::Named(NamedKey::Enter) => { let _ = writer.write_all(b"\r"); }
                            Key::Named(NamedKey::Backspace) => { let _ = writer.write_all(b"\x7f"); }
                            Key::Named(NamedKey::Tab) => { let _ = writer.write_all(b"\t"); }
                            Key::Named(NamedKey::Escape) => { let _ = writer.write_all(b"\x1b"); }
                            Key::Named(NamedKey::ArrowUp) => { let _ = writer.write_all(b"\x1b[A"); }
                            Key::Named(NamedKey::ArrowDown) => { let _ = writer.write_all(b"\x1b[B"); }
                            Key::Named(NamedKey::ArrowRight) => { let _ = writer.write_all(b"\x1b[C"); }
                            Key::Named(NamedKey::ArrowLeft) => { let _ = writer.write_all(b"\x1b[D"); }
                            Key::Named(NamedKey::Home) => { let _ = writer.write_all(b"\x1b[H"); }
                            Key::Named(NamedKey::End) => { let _ = writer.write_all(b"\x1b[F"); }
                            Key::Named(NamedKey::Delete) => { let _ = writer.write_all(b"\x1b[3~"); }
                            Key::Named(NamedKey::PageUp) => { let _ = writer.write_all(b"\x1b[5~"); }
                            Key::Named(NamedKey::PageDown) => { let _ = writer.write_all(b"\x1b[6~"); }
                            _ => {
                                if !ctrl {
                                    if let Some(text) = &text {
                                        let _ = writer.write_all(text.as_bytes());
                                    } else {
                                        wrote = false;
                                    }
                                } else {
                                    wrote = false;
                                }
                            }
                        }
                    }

                    if wrote {
                        let _ = writer.flush();
                        if let Some(terminal) = &self.terminal {
                            terminal.lock().selection = None;
                        }
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                if let (Some(renderer), Some(terminal)) =
                    (&mut self.renderer, &self.terminal)
                {
                    let term = terminal.lock();
                    let content = read_grid_content(&term);

                    let render_spans: Vec<RenderSpan> = content
                        .spans
                        .iter()
                        .map(|s| RenderSpan {
                            text: s.text.clone(),
                            r: s.fg.r,
                            g: s.fg.g,
                            b: s.fg.b,
                            bold: s.bold,
                            italic: s.italic,
                        })
                        .collect();

                    let bg_cells: Vec<nex_render::renderer::BgCell> = content
                        .bg_cells
                        .iter()
                        .map(|c| nex_render::renderer::BgCell {
                            row: c.row,
                            col: c.col,
                            r: c.bg.r,
                            g: c.bg.g,
                            b: c.bg.b,
                        })
                        .collect();

                    // Build selection highlight cells
                    let selection_cells: Vec<SelectionCell> = if let Some(sel) = &content.selection {
                        let display_offset = term.grid().display_offset();
                        let mut cells = Vec::new();
                        let start_row = (sel.start.line.0 + display_offset as i32) as usize;
                        let end_row = (sel.end.line.0 + display_offset as i32) as usize;
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
                    drop(term);

                    let bell_active = self
                        .bell_flash_until
                        .map(|t| std::time::Instant::now() < t)
                        .unwrap_or(false);
                    if !bell_active {
                        self.bell_flash_until = None;
                    }

                    renderer.set_content(&render_spans);
                    if let Err(e) = renderer.render(content.cursor_row, content.cursor_col, &bg_cells, &selection_cells, bell_active) {
                        tracing::error!("Render error: {e}");
                    }

                    // Schedule another redraw to clear the bell flash
                    if bell_active {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::Focused(true) => {
                // Redraw when window regains focus
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            _ => {}
        }
    }
}

/// I/O thread: reads PTY output and feeds it through the VT parser.
fn io_thread(
    mut reader: Box<dyn Read + Send>,
    terminal: Arc<Mutex<Term<NexEventListener>>>,
    window: Option<Arc<Window>>,
    event_proxy: EventLoopProxy<UserEvent>,
) {
    let mut processor = ansi::Processor::<ansi::StdSyncHandler>::new();
    let mut buf = [0u8; 8192];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                tracing::info!("PTY EOF - shell exited");
                let _ = event_proxy.send_event(UserEvent::PtyExited);
                break;
            }
            Ok(n) => {
                {
                    let mut term = terminal.lock();
                    processor.advance(&mut *term, &buf[..n]);
                }
                if let Some(window) = &window {
                    window.request_redraw();
                }
            }
            Err(e) => {
                tracing::error!("PTY read error: {e}");
                let _ = event_proxy.send_event(UserEvent::PtyExited);
                break;
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        "nexterm=debug,nex_render=debug,nex_pty=debug"
    } else {
        "nexterm=info"
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .init();

    tracing::info!("Starting Nexterm v{}", env!("CARGO_PKG_VERSION"));

    let config = nex_config::load_config();
    let shell = cli
        .shell
        .or(config.general.shell.clone())
        .unwrap_or_else(nex_pty::default_shell);

    tracing::info!("Using shell: {shell}");

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let event_proxy = event_loop.create_proxy();
    let mut app = App::new(config, shell, event_proxy);
    event_loop.run_app(&mut app)?;

    Ok(())
}

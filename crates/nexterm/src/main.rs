//! Nexterm - AI-First Terminal Emulator
//!
//! Main binary that orchestrates all components:
//! - winit window + wgpu GPU rendering
//! - PTY spawn with user's default shell
//! - VT terminal emulation (alacritty_terminal)
//! - I/O thread for PTY communication
//! - Block processor for AI integration

use std::io::{Read, Write};
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use parking_lot::Mutex;
use tracing_subscriber::EnvFilter;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use nex_config::NextermConfig;
use nex_pty::NexPty;
use nex_render::renderer::Renderer;

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
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty_writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    output_buffer: Arc<Mutex<String>>,
}

impl App {
    fn new(config: NextermConfig, shell: String) -> Self {
        Self {
            config,
            shell,
            window: None,
            renderer: None,
            pty_writer: None,
            output_buffer: Arc::new(Mutex::new(String::from(
                "Nexterm - AI-First Terminal\nStarting...\n",
            ))),
        }
    }

    fn spawn_pty(&mut self) {
        let size = nex_common::TerminalSize {
            rows: 24,
            cols: 80,
        };

        match NexPty::spawn(&self.shell, size) {
            Ok((_pty, reader, writer)) => {
                self.pty_writer = Some(Arc::new(Mutex::new(writer)));

                // Spawn I/O reader thread
                let output_buffer = Arc::clone(&self.output_buffer);
                let window = self.window.clone();
                std::thread::Builder::new()
                    .name("nexterm-io".to_string())
                    .spawn(move || {
                        io_thread(reader, output_buffer, window);
                    })
                    .expect("Failed to spawn I/O thread");

                tracing::info!("PTY spawned with shell: {}", self.shell);
            }
            Err(e) => {
                tracing::error!("Failed to spawn PTY: {e}");
                *self.output_buffer.lock() = format!("Error: Failed to spawn shell: {e}\n");
            }
        }
    }
}

impl ApplicationHandler for App {
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
                self.renderer = Some(r);
                tracing::info!("GPU renderer initialized");
            }
            Err(e) => {
                tracing::error!("Failed to initialize renderer: {e}");
                event_loop.exit();
                return;
            }
        }

        // Spawn PTY
        self.spawn_pty();
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
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
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
                if let Some(writer) = &self.pty_writer {
                    let mut writer = writer.lock();
                    // Handle special keys
                    match &logical_key {
                        Key::Named(NamedKey::Enter) => {
                            let _ = writer.write_all(b"\r");
                        }
                        Key::Named(NamedKey::Backspace) => {
                            let _ = writer.write_all(b"\x7f");
                        }
                        Key::Named(NamedKey::Tab) => {
                            let _ = writer.write_all(b"\t");
                        }
                        Key::Named(NamedKey::Escape) => {
                            let _ = writer.write_all(b"\x1b");
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            let _ = writer.write_all(b"\x1b[A");
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            let _ = writer.write_all(b"\x1b[B");
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            let _ = writer.write_all(b"\x1b[C");
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            let _ = writer.write_all(b"\x1b[D");
                        }
                        _ => {
                            // Send text characters
                            if let Some(text) = &text {
                                let _ = writer.write_all(text.as_bytes());
                            }
                        }
                    }
                    let _ = writer.flush();
                }
            }

            WindowEvent::RedrawRequested => {
                if let Some(renderer) = &mut self.renderer {
                    let text = self.output_buffer.lock().clone();
                    renderer.set_text(&text);
                    if let Err(e) = renderer.render() {
                        tracing::error!("Render error: {e}");
                    }
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

/// I/O thread: reads from PTY and updates the output buffer.
fn io_thread(
    mut reader: Box<dyn Read + Send>,
    output_buffer: Arc<Mutex<String>>,
    window: Option<Arc<Window>>,
) {
    let mut buf = [0u8; 4096];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                tracing::info!("PTY EOF");
                break;
            }
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]);
                // Strip ANSI for now (Phase 0 - raw text display)
                let stripped = nex_token_save::strip_ansi(&text);
                {
                    let mut buffer = output_buffer.lock();
                    buffer.push_str(&stripped);
                    // Keep buffer at a reasonable size
                    if buffer.len() > 100_000 {
                        let start = buffer.len() - 50_000;
                        *buffer = buffer[start..].to_string();
                    }
                }
                // Request redraw
                if let Some(window) = &window {
                    window.request_redraw();
                }
            }
            Err(e) => {
                tracing::error!("PTY read error: {e}");
                break;
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        "nexterm=debug,nex_render=debug,nex_pty=debug"
    } else {
        "nexterm=info"
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()))
        .init();

    tracing::info!("Starting Nexterm v{}", env!("CARGO_PKG_VERSION"));

    // Load config
    let config = nex_config::load_config();
    let shell = cli
        .shell
        .or(config.general.shell.clone())
        .unwrap_or_else(nex_pty::default_shell);

    tracing::info!("Using shell: {shell}");

    // Create event loop and run
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let mut app = App::new(config, shell);
    event_loop.run_app(&mut app)?;

    Ok(())
}

//! A single terminal pane: owns PTY, Term, and I/O threads.

use std::io::{Read, Write};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use nex_common::{PaneId, TerminalSize};
use nex_ipc::BlockEvent;
use nex_pty::NexPty;
use nex_terminal::{
    NexEventListener, Term, TermConfig, TermSize, TerminalEvent,
    ansi,
};
use parking_lot::Mutex;

use nex_shell_integration::{OscScanner, ShellState};

use crate::{MuxEventProxy, Rect};

const SCROLLBACK_HISTORY: usize = 10_000;
const IO_BUFFER_SIZE: usize = 8192;

/// A terminal pane with its own PTY and VT emulator.
pub struct Pane {
    pub id: PaneId,
    pub terminal: Arc<Mutex<Term<NexEventListener>>>,
    pub pty: NexPty,
    pub pty_writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub bounds: Rect,
    pub term_size: TerminalSize,
    _io_thread: JoinHandle<()>,
    _pty_write_thread: JoinHandle<()>,
}

impl Pane {
    /// Spawn a new pane with PTY, terminal emulator, and I/O threads.
    pub fn spawn<Proxy: MuxEventProxy + 'static>(
        shell: &str,
        size: TerminalSize,
        bounds: Rect,
        block_event_tx: Sender<BlockEvent>,
        event_proxy: &Proxy,
    ) -> anyhow::Result<Self> {
        let pane_id = PaneId::new();
        let (pty, reader, writer) = NexPty::spawn(shell, size)?;
        let pty_writer = Arc::new(Mutex::new(writer));

        // Channel for PtyWrite events (DSR responses)
        let (pty_write_tx, pty_write_rx) = mpsc::channel::<String>();

        // Event callback for title/bell
        let proxy = event_proxy.clone();
        let pid = pane_id;
        let event_callback: nex_terminal::EventCallback = Box::new(move |event| {
            match event {
                TerminalEvent::Title(title) => proxy.send_title(pid, title),
                TerminalEvent::ResetTitle => proxy.send_reset_title(pid),
                TerminalEvent::Bell => proxy.send_bell(pid),
                _ => {}
            }
        });

        let listener = NexEventListener::new(pane_id, pty_write_tx, event_callback);
        let term_config = TermConfig {
            scrolling_history: SCROLLBACK_HISTORY,
            ..Default::default()
        };
        let term = Term::new(
            term_config,
            &TermSize::new(size.cols as usize, size.rows as usize),
            listener,
        );
        let terminal = Arc::new(Mutex::new(term));

        // I/O reader thread
        let io_proxy = event_proxy.clone();
        let io_terminal = Arc::clone(&terminal);
        let _io_thread = std::thread::Builder::new()
            .name(format!("io-{pane_id}"))
            .spawn(move || {
                io_thread(pane_id, reader, io_terminal, io_proxy, block_event_tx);
            })
            .expect("Failed to spawn I/O thread");

        // PtyWrite forwarding thread
        let pw = Arc::clone(&pty_writer);
        let _pty_write_thread = std::thread::Builder::new()
            .name(format!("pty-write-{pane_id}"))
            .spawn(move || {
                while let Ok(text) = pty_write_rx.recv() {
                    let mut w = pw.lock();
                    let _ = w.write_all(text.as_bytes());
                    let _ = w.flush();
                }
            })
            .expect("Failed to spawn PTY write thread");

        tracing::info!(%pane_id, shell, rows = size.rows, cols = size.cols, "Pane spawned");

        Ok(Self {
            id: pane_id,
            terminal,
            pty,
            pty_writer,
            bounds,
            term_size: size,
            _io_thread,
            _pty_write_thread,
        })
    }
}

/// I/O thread: reads PTY output, scans for OSC 133 block events, feeds VT parser.
fn io_thread<Proxy: MuxEventProxy>(
    pane_id: PaneId,
    mut reader: Box<dyn Read + Send>,
    terminal: Arc<Mutex<Term<NexEventListener>>>,
    event_proxy: Proxy,
    block_event_tx: Sender<BlockEvent>,
) {
    let mut processor = ansi::Processor::<ansi::StdSyncHandler>::new();
    let mut osc_scanner = OscScanner::new();
    let mut shell_state = ShellState::Idle;
    let mut buf = [0u8; IO_BUFFER_SIZE];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                tracing::info!(%pane_id, "PTY EOF");
                event_proxy.send_pty_exited(pane_id);
                break;
            }
            Ok(n) => {
                let data = &buf[..n];

                // Scan for OSC 133 sequences and emit BlockEvents
                for event in osc_scanner.scan(data) {
                    shell_state = shell_state.transition(&event);
                    let block_event = match event {
                        nex_shell_integration::Osc133::PromptStart =>
                            Some(BlockEvent::PromptStart { pane_id }),
                        nex_shell_integration::Osc133::CommandStart =>
                            Some(BlockEvent::CommandStart { pane_id }),
                        nex_shell_integration::Osc133::ExecutionStart =>
                            Some(BlockEvent::ExecutionStart { pane_id, command: String::new() }),
                        nex_shell_integration::Osc133::CommandFinished { exit_code } =>
                            Some(BlockEvent::CommandFinished { pane_id, exit_code }),
                    };
                    if let Some(evt) = block_event {
                        let _ = block_event_tx.send(evt);
                    }
                }

                // Capture raw output bytes when a command is executing
                if shell_state == ShellState::CommandOutput {
                    let _ = block_event_tx.send(BlockEvent::Output {
                        pane_id,
                        data: data.to_vec(),
                    });
                }

                // Feed all bytes to VT parser
                {
                    let mut term = terminal.lock();
                    processor.advance(&mut *term, data);
                }
                event_proxy.send_redraw();
            }
            Err(e) => {
                tracing::error!(%pane_id, "PTY read error: {e}");
                event_proxy.send_pty_exited(pane_id);
                break;
            }
        }
    }
}

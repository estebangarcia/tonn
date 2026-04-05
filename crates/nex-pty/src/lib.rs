//! Cross-platform PTY abstraction for Nexterm.

use nex_common::{NexError, Result, TerminalSize};
use portable_pty::{CommandBuilder, MasterPty, PtyPair, PtySize, native_pty_system};
use std::io::{Read, Write};

/// A managed PTY instance.
pub struct NexPty {
    master: Box<dyn MasterPty + Send>,
    pub size: TerminalSize,
}

impl NexPty {
    /// Spawn a new PTY with the given shell command.
    pub fn spawn(shell: &str, size: TerminalSize) -> Result<(Self, Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let pty_system = native_pty_system();

        let pty_size = PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let PtyPair { master, slave } = pty_system
            .openpty(pty_size)
            .map_err(|e| NexError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("NEXTERM", "1");

        slave
            .spawn_command(cmd)
            .map_err(|e| NexError::Pty(e.to_string()))?;

        let reader = master
            .try_clone_reader()
            .map_err(|e| NexError::Pty(e.to_string()))?;

        let writer = master
            .take_writer()
            .map_err(|e| NexError::Pty(e.to_string()))?;

        Ok((
            Self { master, size },
            reader,
            writer,
        ))
    }

    /// Resize the PTY.
    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        let pty_size = PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        self.master
            .resize(pty_size)
            .map_err(|e| NexError::Pty(e.to_string()))
    }
}

/// Get the user's default shell.
pub fn default_shell() -> String {
    #[cfg(unix)]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_string())
    }
}

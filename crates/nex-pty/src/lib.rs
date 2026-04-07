//! Cross-platform PTY abstraction for Tonn.

use nex_common::{NexError, Result, TerminalSize};
use portable_pty::{CommandBuilder, MasterPty, PtyPair, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;

// Shell integration scripts embedded at compile time.
const SHELL_INTEGRATION_ZSH: &str = include_str!("../../../shell/tonn.zsh");
const SHELL_INTEGRATION_BASH: &str = include_str!("../../../shell/tonn.bash");
const SHELL_INTEGRATION_FISH: &str = include_str!("../../../shell/tonn.fish");

/// Write embedded shell integration scripts to a runtime directory.
/// Returns the directory path. Prefers a user-local data directory to avoid
/// writing to a predictable world-writable /tmp path.
fn ensure_shell_integration_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .map(|d| d.join("tonn"))
        .unwrap_or_else(|| std::env::temp_dir().join("tonn-shell-integration"));
    std::fs::create_dir_all(&dir)?;

    std::fs::write(dir.join("tonn.zsh"), SHELL_INTEGRATION_ZSH)?;
    std::fs::write(dir.join("tonn.bash"), SHELL_INTEGRATION_BASH)?;
    std::fs::write(dir.join("tonn.fish"), SHELL_INTEGRATION_FISH)?;

    Ok(dir)
}

/// Detect the shell type from the shell path.
fn shell_type(shell: &str) -> &'static str {
    let name = std::path::Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name.contains("zsh") {
        "zsh"
    } else if name.contains("bash") {
        "bash"
    } else if name.contains("fish") {
        "fish"
    } else {
        "unknown"
    }
}

/// A managed PTY instance.
pub struct NexPty {
    master: Box<dyn MasterPty + Send>,
    pub size: TerminalSize,
}

impl NexPty {
    /// Spawn a new PTY with the given shell command.
    /// Automatically injects shell integration for supported shells.
    #[allow(clippy::type_complexity)]
    pub fn spawn(shell: &str, size: TerminalSize, mcp_port: Option<u16>) -> Result<(Self, Box<dyn Read + Send>, Box<dyn Write + Send>)> {
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
        cmd.env("TONN", "1");
        if let Some(port) = mcp_port {
            cmd.env("TONN_MCP_PORT", port.to_string());
        }

        // Auto-inject shell integration
        if let Ok(integration_dir) = ensure_shell_integration_dir() {
            let script_path = integration_dir.display().to_string();
            match shell_type(shell) {
                "zsh" => {
                    // ZDOTDIR trick: we create a .zshrc that sources the user's original
                    // .zshrc then sources our integration. This is the same approach
                    // VS Code and iTerm2 use.
                    let zdotdir = integration_dir.join("zsh");
                    std::fs::create_dir_all(&zdotdir).ok();

                    let user_zdotdir = std::env::var("ZDOTDIR")
                        .unwrap_or_else(|_| {
                            dirs::home_dir()
                                .unwrap_or_else(|| PathBuf::from("/"))
                                .display()
                                .to_string()
                        });

                    // .zshenv: keep ZDOTDIR as wrapper dir (so zsh finds our .zshrc),
                    // but source user's .zshenv from their home.
                    let zshenv = format!(
                        r#"TONN_USER_ZDOTDIR="{user_zdotdir}"
[[ -f "{user_zdotdir}/.zshenv" ]] && ZDOTDIR="{user_zdotdir}" source "{user_zdotdir}/.zshenv"
"#
                    );
                    std::fs::write(zdotdir.join(".zshenv"), zshenv).ok();

                    // .zshrc: source user's config first, then fix HISTFILE if it
                    // points to our wrapper dir, then source our integration.
                    let zshrc = format!(
                        r#"ZDOTDIR="{user_zdotdir}"
[[ -f "{user_zdotdir}/.zshrc" ]] && source "{user_zdotdir}/.zshrc"
# Fix HISTFILE if it got set to the wrapper ZDOTDIR
[[ "$HISTFILE" == *"/tonn/"* ]] && HISTFILE="$HOME/.zsh_history"
source "{script_path}/tonn.zsh"
"#
                    );
                    std::fs::write(zdotdir.join(".zshrc"), zshrc).ok();
                    cmd.env("ZDOTDIR", zdotdir.display().to_string());
                    cmd.env("TONN_USER_ZDOTDIR", &user_zdotdir);
                }
                "bash" => {
                    // Bash: use --rcfile or ENV to source our integration after .bashrc
                    let rcfile = integration_dir.join("bash_init.sh");
                    let wrapper = format!(
                        r#"# Tonn auto-injected wrapper
if [[ -f ~/.bashrc ]]; then
    source ~/.bashrc
fi
source "{script_path}/tonn.bash"
"#
                    );
                    std::fs::write(&rcfile, wrapper).ok();
                    cmd.args(["--rcfile", &rcfile.display().to_string()]);
                }
                "fish" => {
                    // Fish: use --init-command
                    let init = format!("source {script_path}/tonn.fish");
                    cmd.args(["--init-command", &init]);
                }
                _ => {}
            }
        }

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

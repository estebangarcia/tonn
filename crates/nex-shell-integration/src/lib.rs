/// OSC 133 sequence types (FinalTerm semantic prompts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc133 {
    /// A - Prompt start
    PromptStart,
    /// B - Command input start (end of prompt)
    CommandStart,
    /// C - Command execution start
    ExecutionStart,
    /// D - Command finished with exit code
    CommandFinished { exit_code: i32 },
}

/// Custom Nexterm OSC 1337 extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextermOsc {
    /// Current working directory changed
    Cwd(String),
    /// Environment variable changed
    Env { key: String, value: String },
    /// Git state update
    Git { branch: String, status: String },
    /// Virtual environment activated
    Venv(String),
}

/// All shell integration events the terminal can detect.
#[derive(Debug, Clone)]
pub enum ShellEvent {
    Osc133(Osc133),
    NextermOsc(NextermOsc),
}

/// Parse an OSC payload to detect shell integration sequences.
pub fn parse_osc(params: &[&[u8]]) -> Option<ShellEvent> {
    if params.is_empty() {
        return None;
    }

    let first = params[0];

    // OSC 133 ; <code> [; <params>]
    if first == b"133" && params.len() >= 2 {
        let code = params[1];
        return match code {
            b"A" => Some(ShellEvent::Osc133(Osc133::PromptStart)),
            b"B" => Some(ShellEvent::Osc133(Osc133::CommandStart)),
            b"C" => Some(ShellEvent::Osc133(Osc133::ExecutionStart)),
            b"D" => {
                let exit_code = if params.len() >= 3 {
                    std::str::from_utf8(params[2])
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0)
                } else {
                    0
                };
                Some(ShellEvent::Osc133(Osc133::CommandFinished { exit_code }))
            }
            _ => None,
        };
    }

    None
}

/// The state machine for tracking shell prompt/command lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellState {
    Idle,
    Prompt,
    CommandInput,
    CommandOutput,
}

impl ShellState {
    pub fn transition(self, event: &Osc133) -> Self {
        match (self, event) {
            (_, Osc133::PromptStart) => ShellState::Prompt,
            (ShellState::Prompt, Osc133::CommandStart) => ShellState::CommandInput,
            (ShellState::CommandInput, Osc133::ExecutionStart) => ShellState::CommandOutput,
            (ShellState::CommandOutput, Osc133::CommandFinished { .. }) => ShellState::Idle,
            // Allow transitions from any state for robustness
            (_, Osc133::CommandFinished { .. }) => ShellState::Idle,
            _ => self,
        }
    }
}

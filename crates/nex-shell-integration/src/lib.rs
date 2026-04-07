/// OSC 133 sequence types (FinalTerm semantic prompts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc133 {
    PromptStart,
    CommandStart,
    ExecutionStart,
    CommandFinished { exit_code: i32 },
}

/// Events detected by scanning the PTY byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOscEvent {
    Osc133(Osc133),
    CwdChanged(String),
}

/// Maximum length of OSC body before the scanner bails out (safety limit).
const OSC_BODY_MAX_LEN: usize = 512; // Increased for CWD paths

/// Streaming byte-level scanner that detects OSC 133 sequences in raw PTY output.
/// Handles sequences split across read() boundaries.
///
/// Detects: `ESC ] 133 ; <code> [; <params>] BEL` or `ESC ] 133 ; <code> [; <params>] ESC \`
pub struct OscScanner {
    state: ScanState,
    buf: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Normal,
    Esc,         // saw \x1b
    OscStart,    // saw \x1b ], accumulating number digits
    OscBody133,  // inside OSC 133 body
    OscBody1337, // inside OSC 1337 body
    OscSkip,     // inside an unrecognized OSC, skip until terminator
}

impl OscScanner {
    pub fn new() -> Self {
        Self {
            state: ScanState::Normal,
            buf: Vec::with_capacity(64),
        }
    }

    /// Scan a chunk of bytes, returning detected OSC events.
    pub fn scan(&mut self, data: &[u8]) -> Vec<TerminalOscEvent> {
        let mut events = Vec::new();
        for &byte in data {
            match self.state {
                ScanState::Normal => {
                    if byte == 0x1b {
                        self.state = ScanState::Esc;
                    }
                }
                ScanState::Esc => {
                    if byte == b']' {
                        self.state = ScanState::OscStart;
                        self.buf.clear();
                    } else {
                        self.state = ScanState::Normal;
                    }
                }
                ScanState::OscStart => {
                    if byte == b';' {
                        let next = if self.buf == b"133" {
                            ScanState::OscBody133
                        } else if self.buf == b"1337" {
                            ScanState::OscBody1337
                        } else {
                            ScanState::OscSkip
                        };
                        self.buf.clear();
                        self.state = next;
                    } else if byte.is_ascii_digit() {
                        self.buf.push(byte);
                        if self.buf.len() > 5 {
                            self.state = ScanState::OscSkip;
                            self.buf.clear();
                        }
                    } else {
                        self.state = ScanState::Normal;
                        self.buf.clear();
                    }
                }
                ScanState::OscBody133 => {
                    if byte == 0x07 || byte == 0x1b {
                        if let Some(event) = self.parse_osc133() {
                            events.push(TerminalOscEvent::Osc133(event));
                        }
                        self.buf.clear();
                        self.state = if byte == 0x1b { ScanState::Esc } else { ScanState::Normal };
                    } else {
                        self.buf.push(byte);
                        if self.buf.len() > OSC_BODY_MAX_LEN {
                            self.state = ScanState::Normal;
                            self.buf.clear();
                        }
                    }
                }
                ScanState::OscBody1337 => {
                    if byte == 0x07 || byte == 0x1b {
                        if let Some(event) = self.parse_osc1337() {
                            events.push(event);
                        }
                        self.buf.clear();
                        self.state = if byte == 0x1b { ScanState::Esc } else { ScanState::Normal };
                    } else {
                        self.buf.push(byte);
                        if self.buf.len() > OSC_BODY_MAX_LEN {
                            self.state = ScanState::Normal;
                            self.buf.clear();
                        }
                    }
                }
                ScanState::OscSkip => {
                    if byte == 0x07 || byte == 0x1b {
                        self.buf.clear();
                        self.state = if byte == 0x1b { ScanState::Esc } else { ScanState::Normal };
                    }
                }
            }
        }
        events
    }

    fn parse_osc133(&self) -> Option<Osc133> {
        if self.buf.is_empty() {
            return None;
        }
        let code = self.buf[0];
        match code {
            b'A' => Some(Osc133::PromptStart),
            b'B' => Some(Osc133::CommandStart),
            b'C' => Some(Osc133::ExecutionStart),
            b'D' => {
                let exit_code = if self.buf.len() > 2 && self.buf[1] == b';' {
                    std::str::from_utf8(&self.buf[2..])
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0)
                } else {
                    0
                };
                Some(Osc133::CommandFinished { exit_code })
            }
            _ => None,
        }
    }

    fn parse_osc1337(&self) -> Option<TerminalOscEvent> {
        // Body format: "Tonn=cwd;/path/to/dir" or "Tonn=git;branch;status"
        let body = std::str::from_utf8(&self.buf).ok()?;
        let rest = body.strip_prefix("Tonn=")?;
        if let Some(path) = rest.strip_prefix("cwd;") {
            Some(TerminalOscEvent::CwdChanged(path.to_string()))
        } else {
            None // git, env, venv — not handled yet
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scanner_basic() {
        let mut scanner = OscScanner::new();
        let events = scanner.scan(b"\x1b]133;A\x07");
        assert_eq!(events, vec![TerminalOscEvent::Osc133(Osc133::PromptStart)]);
    }

    #[test]
    fn test_scanner_with_exit_code() {
        let mut scanner = OscScanner::new();
        let events = scanner.scan(b"\x1b]133;D;1\x07");
        assert_eq!(events, vec![TerminalOscEvent::Osc133(Osc133::CommandFinished { exit_code: 1 })]);
    }

    #[test]
    fn test_scanner_split_across_reads() {
        let mut scanner = OscScanner::new();
        let e1 = scanner.scan(b"\x1b]13");
        assert!(e1.is_empty());
        let e2 = scanner.scan(b"3;C\x07");
        assert_eq!(e2, vec![TerminalOscEvent::Osc133(Osc133::ExecutionStart)]);
    }

    #[test]
    fn test_scanner_multiple_in_one_chunk() {
        let mut scanner = OscScanner::new();
        let events = scanner.scan(b"\x1b]133;A\x07hello\x1b]133;B\x07");
        assert_eq!(events, vec![TerminalOscEvent::Osc133(Osc133::PromptStart), TerminalOscEvent::Osc133(Osc133::CommandStart)]);
    }

    #[test]
    fn test_scanner_st_terminator() {
        let mut scanner = OscScanner::new();
        let events = scanner.scan(b"\x1b]133;A\x1b\\");
        assert_eq!(events, vec![TerminalOscEvent::Osc133(Osc133::PromptStart)]);
    }

    #[test]
    fn test_scanner_ignores_non_133() {
        let mut scanner = OscScanner::new();
        let events = scanner.scan(b"\x1b]7;file:///tmp\x07\x1b]133;A\x07");
        assert_eq!(events, vec![TerminalOscEvent::Osc133(Osc133::PromptStart)]);
    }

    #[test]
    fn test_scanner_cwd_change() {
        let mut scanner = OscScanner::new();
        let events = scanner.scan(b"\x1b]1337;Tonn=cwd;/Users/test/project\x07");
        assert_eq!(events, vec![TerminalOscEvent::CwdChanged("/Users/test/project".to_string())]);
    }

    #[test]
    fn test_scanner_mixed_133_and_1337() {
        let mut scanner = OscScanner::new();
        let events = scanner.scan(b"\x1b]1337;Tonn=cwd;/tmp\x07\x1b]133;A\x07");
        assert_eq!(events, vec![
            TerminalOscEvent::CwdChanged("/tmp".to_string()),
            TerminalOscEvent::Osc133(Osc133::PromptStart),
        ]);
    }
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

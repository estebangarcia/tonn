//! Clipboard paste → PTY bytes.
//!
//! Pure logic, like `keys.rs` and `mouse.rs`. Follows xterm/alacritty:
//!
//! - Bracketed paste on (DECSET 2004): wrap the text in `ESC[200~ … ESC[201~`
//!   so the app can tell pasted text from typing. ESC and ETX are stripped so
//!   the payload can't close the bracket early (paste injection) or make a
//!   shell abort the paste.
//! - Off: the app can't tell paste from keystrokes, so send the text as if it
//!   were typed — line breaks become CR, which is what Enter sends. Shells turn
//!   bracketed paste off before running a command, so password prompts
//!   (`docker login`, `psql`) land here; sending the markers anyway would make
//!   them part of the password.

use crate::mode::TermMode;

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
const ESC: char = '\x1b';
/// ETX (Ctrl+C) — some shells abort a bracketed paste when they see it.
const ETX: char = '\x03';
const CRLF: &str = "\r\n";
const LF: char = '\n';
const CR: &str = "\r";

/// Encode clipboard text for the PTY according to the app's paste mode.
pub fn encode(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let filtered = text.replace([ESC, ETX], "");
        [BRACKETED_PASTE_START, filtered.as_bytes(), BRACKETED_PASTE_END].concat()
    } else {
        text.replace(CRLF, CR).replace(LF, CR).into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paste_has_no_markers() {
        assert_eq!(encode("s3cret", TermMode::empty()), b"s3cret");
    }

    #[test]
    fn plain_paste_line_breaks_become_enter() {
        assert_eq!(encode("a\r\nb\nc", TermMode::empty()), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_wraps_text_verbatim() {
        assert_eq!(
            encode("echo hi\nls", TermMode::BRACKETED_PASTE),
            b"\x1b[200~echo hi\nls\x1b[201~",
        );
    }

    #[test]
    fn bracketed_paste_cannot_be_closed_early() {
        // A payload carrying its own end marker must not escape the bracket.
        assert_eq!(
            encode("a\x1b[201~rm -rf ~\x03", TermMode::BRACKETED_PASTE),
            b"\x1b[200~a[201~rm -rf ~\x1b[201~",
        );
    }
}

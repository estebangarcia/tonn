/// Strip ANSI escape sequences from text.
pub fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    AnsiStripper::default().push_str(input, &mut output);
    output
}

/// Incremental ANSI stripper: `strip_ansi`'s rules, fed in chunks. Escape
/// sequence and UTF-8 decoding state carry over between calls, so a sequence
/// or character split across chunks is handled — stripping chunk by chunk
/// gives the same text as stripping the whole stream at once.
#[derive(Debug, Default)]
pub struct AnsiStripper {
    state: StripState,
    /// Bytes of a UTF-8 character cut off at the end of the last chunk.
    pending_utf8: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum StripState {
    #[default]
    Ground,
    /// Just saw ESC.
    Escape,
    /// Inside `ESC [ …`, until a final byte.
    Csi,
    /// Inside `ESC ] …`, until BEL or ESC (ST).
    Osc,
    /// Saw ESC inside an OSC: a following `\` belongs to the ST.
    OscEscape,
}

impl AnsiStripper {
    /// Strip raw bytes, appending the text to `out`. Invalid UTF-8 becomes
    /// U+FFFD; a character split across calls is completed by the next one.
    pub fn push_bytes(&mut self, bytes: &[u8], out: &mut String) {
        let mut input = std::mem::take(&mut self.pending_utf8);
        input.extend_from_slice(bytes);
        let mut rest = input.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    self.push_str(valid, out);
                    return;
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    // `valid_up_to` marks a valid UTF-8 prefix, so this never falls back.
                    self.push_str(std::str::from_utf8(valid).unwrap_or_default(), out);
                    match error.error_len() {
                        Some(invalid_len) => {
                            self.push_str(char::REPLACEMENT_CHARACTER.encode_utf8(&mut [0; 4]), out);
                            rest = &after[invalid_len..];
                        }
                        None => {
                            self.pending_utf8 = after.to_vec();
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Strip decoded text, appending the text to `out`.
    pub fn push_str(&mut self, input: &str, out: &mut String) {
        for c in input.chars() {
            self.state = match self.state {
                StripState::Ground => Self::ground(c, out),
                // Two-character escapes (`ESC (`, `ESC 7`, …) drop their second char.
                StripState::Escape => match c {
                    '[' => StripState::Csi,
                    ']' => StripState::Osc,
                    _ => StripState::Ground,
                },
                StripState::Csi if c.is_ascii_alphabetic() || c == '@' || c == '`' => StripState::Ground,
                StripState::Csi => StripState::Csi,
                StripState::Osc => match c {
                    '\x07' => StripState::Ground,
                    '\x1b' => StripState::OscEscape,
                    _ => StripState::Osc,
                },
                // The ESC ended the OSC; anything but the ST's `\` is ordinary input.
                StripState::OscEscape if c == '\\' => StripState::Ground,
                StripState::OscEscape => Self::ground(c, out),
            };
        }
    }

    fn ground(c: char, out: &mut String) -> StripState {
        match c {
            '\x1b' => StripState::Escape,
            // Carriage returns are dropped.
            '\r' => StripState::Ground,
            _ => {
                out.push(c);
                StripState::Ground
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_plain() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn test_strip_ansi_colors() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn test_strip_ansi_csi() {
        assert_eq!(strip_ansi("\x1b[1;32mbold green\x1b[0m"), "bold green");
    }

    #[test]
    fn test_strip_ansi_osc_st_terminator() {
        // OSC terminated by ESC \ should not leak the backslash
        assert_eq!(strip_ansi("\x1b]0;title\x1b\\hello"), "hello");
    }

    #[test]
    fn test_strip_ansi_osc_bel_terminator() {
        assert_eq!(strip_ansi("\x1b]0;title\x07hello"), "hello");
    }

    #[test]
    fn streaming_matches_whole_input_at_every_split() {
        let input = "a\x1b[1;31mred\x1b[0m \x1b]0;title\x07t\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ é日本 \x1b(Bx\r\n";
        let expected = strip_ansi(input);
        let bytes = input.as_bytes();
        for split in 0..=bytes.len() {
            let mut stripper = AnsiStripper::default();
            let mut out = String::new();
            stripper.push_bytes(&bytes[..split], &mut out);
            stripper.push_bytes(&bytes[split..], &mut out);
            assert_eq!(out, expected, "split at byte {split}");
        }
    }

    #[test]
    fn streaming_replaces_invalid_utf8() {
        let mut stripper = AnsiStripper::default();
        let mut out = String::new();
        stripper.push_bytes(b"a\xffb", &mut out);
        assert_eq!(out, "a\u{FFFD}b");
    }
}

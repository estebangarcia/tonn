//! Token-saving output compression pipeline for Nexterm.
//! Phase 0 stub - full implementation in Phase 2.

/// Strip ANSI escape sequences from text.
pub fn strip_ansi(input: &str) -> String {
    // Simple state machine to strip ANSI escape sequences
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // ESC - start of escape sequence
            if let Some(&next) = chars.peek() {
                match next {
                    '[' => {
                        // CSI sequence: ESC [ ... final_byte
                        chars.next();
                        while let Some(&c) = chars.peek() {
                            if c.is_ascii_alphabetic() || c == '@' || c == '`' {
                                chars.next();
                                break;
                            }
                            chars.next();
                        }
                    }
                    ']' => {
                        // OSC sequence: ESC ] ... ST
                        chars.next();
                        while let Some(c) = chars.next() {
                            if c == '\x07' {
                                break;
                            }
                            if c == '\x1b' {
                                // Consume the \ of ST (ESC \)
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                        }
                    }
                    _ => {
                        chars.next();
                    }
                }
            }
        } else if c == '\r' {
            // Skip carriage returns
            continue;
        } else {
            output.push(c);
        }
    }

    output
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
}

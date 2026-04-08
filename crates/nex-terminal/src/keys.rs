//! Keyboard input → terminal escape sequence encoding.
//!
//! Pure logic — no winit, no PTY, no side effects. Takes a logical key +
//! modifiers + terminal mode and returns the byte sequence to write to the PTY.

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A logical key, decoupled from any windowing library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Named(NamedKey),
    Char(char),
}

/// Named (non-character) keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Backspace,
    Tab,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    F(u8),
}

/// Modifier key state.
#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Terminal modes that affect key encoding.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mode {
    pub app_cursor: bool,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Encode a key press into the bytes to write to the PTY.
///
/// Returns `None` if the key has no PTY representation (e.g. bare modifier
/// keys, or unrecognised named keys).
pub fn encode(key: &Key, mods: &Modifiers, mode: &Mode) -> Option<Vec<u8>> {
    match key {
        Key::Named(named) => encode_named(*named, mods, mode),
        Key::Char(ch) => encode_char(*ch, mods),
    }
}

// ---------------------------------------------------------------------------
// Named key encoding
// ---------------------------------------------------------------------------

fn encode_named(key: NamedKey, mods: &Modifiers, mode: &Mode) -> Option<Vec<u8>> {
    match key {
        // --- Simple keys (ignoring modifiers) ---
        NamedKey::Enter => Some(b"\r".to_vec()),
        NamedKey::Backspace => Some(b"\x7f".to_vec()),
        NamedKey::Escape => Some(b"\x1b".to_vec()),

        // --- Tab / Shift+Tab ---
        NamedKey::Tab if mods.shift => Some(b"\x1b[Z".to_vec()),
        NamedKey::Tab => Some(b"\t".to_vec()),

        // --- Arrow keys (CSI / SS3 with app_cursor) ---
        NamedKey::ArrowUp => Some(encode_csi_or_ss3(b'A', mods, mode.app_cursor)),
        NamedKey::ArrowDown => Some(encode_csi_or_ss3(b'B', mods, mode.app_cursor)),
        NamedKey::ArrowRight => Some(encode_csi_or_ss3(b'C', mods, mode.app_cursor)),
        NamedKey::ArrowLeft => Some(encode_csi_or_ss3(b'D', mods, mode.app_cursor)),

        // --- Home / End ---
        NamedKey::Home => Some(encode_csi_or_ss3(b'H', mods, mode.app_cursor)),
        NamedKey::End => Some(encode_csi_or_ss3(b'F', mods, mode.app_cursor)),

        // --- Tilde-style keys ---
        NamedKey::Insert => Some(encode_tilde(2, mods)),
        NamedKey::Delete => Some(encode_tilde(3, mods)),
        NamedKey::PageUp => Some(encode_tilde(5, mods)),
        NamedKey::PageDown => Some(encode_tilde(6, mods)),

        // --- Function keys ---
        NamedKey::F(n) => encode_function_key(n, mods),
    }
}

// ---------------------------------------------------------------------------
// Character encoding
// ---------------------------------------------------------------------------

fn encode_char(ch: char, mods: &Modifiers) -> Option<Vec<u8>> {
    if mods.ctrl && let Some(code) = ctrl_code(ch) {
        return if mods.alt {
            // Alt+Ctrl+letter → ESC + control code
            Some(vec![0x1b, code])
        } else {
            Some(vec![code])
        };
    }

    if mods.alt {
        // Alt+char → ESC prefix + UTF-8 char
        let mut buf = vec![0x1b];
        let mut char_buf = [0u8; 4];
        buf.extend_from_slice(ch.encode_utf8(&mut char_buf).as_bytes());
        return Some(buf);
    }

    // Regular character — pass through as UTF-8
    let mut buf = [0u8; 4];
    Some(ch.encode_utf8(&mut buf).as_bytes().to_vec())
}

/// Map Ctrl+key to the corresponding ASCII control code.
fn ctrl_code(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some(ch as u8 - b'a' + 1),
        'A'..='Z' => Some(ch as u8 - b'A' + 1),
        '[' | '3' => Some(0x1b), // ESC
        '\\' | '4' => Some(0x1c), // FS
        ']' | '5' => Some(0x1d), // GS
        '/' | '7' => Some(0x1f), // US
        ' ' | '2' => Some(0x00), // NUL
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

/// Compute the CSI modifier parameter (2–8).
///
/// | Modifier      | Param |
/// |---------------|-------|
/// | Shift         | 2     |
/// | Alt           | 3     |
/// | Shift+Alt     | 4     |
/// | Ctrl          | 5     |
/// | Shift+Ctrl    | 6     |
/// | Alt+Ctrl      | 7     |
/// | Shift+Alt+Ctrl| 8     |
fn modifier_param(mods: &Modifiers) -> Option<u8> {
    let code = 1
        + if mods.shift { 1 } else { 0 }
        + if mods.alt { 2 } else { 0 }
        + if mods.ctrl { 4 } else { 0 };
    if code > 1 { Some(code) } else { None }
}

/// Encode a key that uses CSI (`\x1b[`) or SS3 (`\x1bO`) form.
///
/// Used by: arrows (A-D), Home (H), End (F), F1-F4 (P-S).
///
/// - No modifiers + app_mode: `\x1bO{final}`
/// - No modifiers + normal:   `\x1b[{final}`
/// - With modifiers (always):  `\x1b[1;{m}{final}`
fn encode_csi_or_ss3(final_char: u8, mods: &Modifiers, app_mode: bool) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[1;{m}{}", final_char as char).into_bytes(),
        None if app_mode => vec![0x1b, b'O', final_char],
        None => vec![0x1b, b'[', final_char],
    }
}

/// Encode a key that uses the tilde (`\x1b[{n}~`) form.
///
/// Used by: Insert(2), Delete(3), PageUp(5), PageDown(6), F5-F12.
///
/// - No modifiers: `\x1b[{n}~`
/// - With modifiers: `\x1b[{n};{m}~`
fn encode_tilde(num: u8, mods: &Modifiers) -> Vec<u8> {
    match modifier_param(mods) {
        Some(m) => format!("\x1b[{num};{m}~").into_bytes(),
        None => format!("\x1b[{num}~").into_bytes(),
    }
}

/// Encode function keys F1–F12.
///
/// F1-F4 use SS3 form bare (`\x1bOP`), CSI with modifiers (`\x1b[1;{m}P`).
/// F5-F12 use tilde form with specific numbers.
fn encode_function_key(n: u8, mods: &Modifiers) -> Option<Vec<u8>> {
    // F1-F4 always use SS3 when bare (app_mode=true forces SS3 path)
    match n {
        1 => Some(encode_csi_or_ss3(b'P', mods, true)),
        2 => Some(encode_csi_or_ss3(b'Q', mods, true)),
        3 => Some(encode_csi_or_ss3(b'R', mods, true)),
        4 => Some(encode_csi_or_ss3(b'S', mods, true)),
        5 => Some(encode_tilde(15, mods)),
        6 => Some(encode_tilde(17, mods)),
        7 => Some(encode_tilde(18, mods)),
        8 => Some(encode_tilde(19, mods)),
        9 => Some(encode_tilde(20, mods)),
        10 => Some(encode_tilde(21, mods)),
        11 => Some(encode_tilde(23, mods)),
        12 => Some(encode_tilde(24, mods)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const NO_MODS: Modifiers = Modifiers { shift: false, ctrl: false, alt: false };
    const SHIFT: Modifiers = Modifiers { shift: true, ctrl: false, alt: false };
    const CTRL: Modifiers = Modifiers { shift: false, ctrl: true, alt: false };
    const ALT: Modifiers = Modifiers { shift: false, ctrl: false, alt: true };
    const SHIFT_CTRL: Modifiers = Modifiers { shift: true, ctrl: true, alt: false };
    const ALT_CTRL: Modifiers = Modifiers { shift: false, ctrl: true, alt: true };
    const NORMAL: Mode = Mode { app_cursor: false };
    const APP_CURSOR: Mode = Mode { app_cursor: true };

    // --- Basic keys ---

    #[test]
    fn enter() {
        assert_eq!(encode(&Key::Named(NamedKey::Enter), &NO_MODS, &NORMAL), Some(b"\r".to_vec()));
    }

    #[test]
    fn tab() {
        assert_eq!(encode(&Key::Named(NamedKey::Tab), &NO_MODS, &NORMAL), Some(b"\t".to_vec()));
    }

    #[test]
    fn backspace() {
        assert_eq!(encode(&Key::Named(NamedKey::Backspace), &NO_MODS, &NORMAL), Some(b"\x7f".to_vec()));
    }

    #[test]
    fn escape() {
        assert_eq!(encode(&Key::Named(NamedKey::Escape), &NO_MODS, &NORMAL), Some(b"\x1b".to_vec()));
    }

    // --- Shift+Tab ---

    #[test]
    fn shift_tab() {
        assert_eq!(encode(&Key::Named(NamedKey::Tab), &SHIFT, &NORMAL), Some(b"\x1b[Z".to_vec()));
    }

    // --- Arrow keys ---

    #[test]
    fn arrow_up_normal() {
        assert_eq!(encode(&Key::Named(NamedKey::ArrowUp), &NO_MODS, &NORMAL), Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn arrow_up_app_cursor() {
        assert_eq!(encode(&Key::Named(NamedKey::ArrowUp), &NO_MODS, &APP_CURSOR), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn shift_arrow_up() {
        // Modifier 2 = Shift
        assert_eq!(encode(&Key::Named(NamedKey::ArrowUp), &SHIFT, &NORMAL), Some(b"\x1b[1;2A".to_vec()));
    }

    #[test]
    fn ctrl_left() {
        // Modifier 5 = Ctrl
        assert_eq!(encode(&Key::Named(NamedKey::ArrowLeft), &CTRL, &NORMAL), Some(b"\x1b[1;5D".to_vec()));
    }

    #[test]
    fn shift_ctrl_right() {
        // Modifier 6 = Shift+Ctrl
        assert_eq!(encode(&Key::Named(NamedKey::ArrowRight), &SHIFT_CTRL, &NORMAL), Some(b"\x1b[1;6C".to_vec()));
    }

    #[test]
    fn arrow_with_modifier_ignores_app_cursor() {
        // With modifiers, always CSI form even in app_cursor mode
        assert_eq!(encode(&Key::Named(NamedKey::ArrowUp), &CTRL, &APP_CURSOR), Some(b"\x1b[1;5A".to_vec()));
    }

    // --- Home / End ---

    #[test]
    fn home_normal() {
        assert_eq!(encode(&Key::Named(NamedKey::Home), &NO_MODS, &NORMAL), Some(b"\x1b[H".to_vec()));
    }

    #[test]
    fn end_app_cursor() {
        assert_eq!(encode(&Key::Named(NamedKey::End), &NO_MODS, &APP_CURSOR), Some(b"\x1bOF".to_vec()));
    }

    #[test]
    fn shift_home() {
        assert_eq!(encode(&Key::Named(NamedKey::Home), &SHIFT, &NORMAL), Some(b"\x1b[1;2H".to_vec()));
    }

    // --- Editing keys ---

    #[test]
    fn delete_bare() {
        assert_eq!(encode(&Key::Named(NamedKey::Delete), &NO_MODS, &NORMAL), Some(b"\x1b[3~".to_vec()));
    }

    #[test]
    fn delete_with_ctrl() {
        assert_eq!(encode(&Key::Named(NamedKey::Delete), &CTRL, &NORMAL), Some(b"\x1b[3;5~".to_vec()));
    }

    #[test]
    fn insert() {
        assert_eq!(encode(&Key::Named(NamedKey::Insert), &NO_MODS, &NORMAL), Some(b"\x1b[2~".to_vec()));
    }

    #[test]
    fn page_up_with_shift() {
        assert_eq!(encode(&Key::Named(NamedKey::PageUp), &SHIFT, &NORMAL), Some(b"\x1b[5;2~".to_vec()));
    }

    // --- Function keys ---

    #[test]
    fn f1_bare() {
        assert_eq!(encode(&Key::Named(NamedKey::F(1)), &NO_MODS, &NORMAL), Some(b"\x1bOP".to_vec()));
    }

    #[test]
    fn f4_bare() {
        assert_eq!(encode(&Key::Named(NamedKey::F(4)), &NO_MODS, &NORMAL), Some(b"\x1bOS".to_vec()));
    }

    #[test]
    fn f1_with_shift() {
        assert_eq!(encode(&Key::Named(NamedKey::F(1)), &SHIFT, &NORMAL), Some(b"\x1b[1;2P".to_vec()));
    }

    #[test]
    fn f5_bare() {
        assert_eq!(encode(&Key::Named(NamedKey::F(5)), &NO_MODS, &NORMAL), Some(b"\x1b[15~".to_vec()));
    }

    #[test]
    fn f12_bare() {
        assert_eq!(encode(&Key::Named(NamedKey::F(12)), &NO_MODS, &NORMAL), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn f12_with_ctrl() {
        assert_eq!(encode(&Key::Named(NamedKey::F(12)), &CTRL, &NORMAL), Some(b"\x1b[24;5~".to_vec()));
    }

    #[test]
    fn f_out_of_range() {
        assert_eq!(encode(&Key::Named(NamedKey::F(13)), &NO_MODS, &NORMAL), None);
    }

    // --- Ctrl+letter ---

    #[test]
    fn ctrl_c() {
        assert_eq!(encode(&Key::Char('c'), &CTRL, &NORMAL), Some(vec![0x03]));
    }

    #[test]
    fn ctrl_a() {
        assert_eq!(encode(&Key::Char('a'), &CTRL, &NORMAL), Some(vec![0x01]));
    }

    #[test]
    fn ctrl_z() {
        assert_eq!(encode(&Key::Char('z'), &CTRL, &NORMAL), Some(vec![0x1a]));
    }

    #[test]
    fn ctrl_bracket() {
        assert_eq!(encode(&Key::Char('['), &CTRL, &NORMAL), Some(vec![0x1b]));
    }

    #[test]
    fn ctrl_space() {
        assert_eq!(encode(&Key::Char(' '), &CTRL, &NORMAL), Some(vec![0x00]));
    }

    // --- Alt+letter ---

    #[test]
    fn alt_f() {
        assert_eq!(encode(&Key::Char('f'), &ALT, &NORMAL), Some(vec![0x1b, b'f']));
    }

    #[test]
    fn alt_b() {
        assert_eq!(encode(&Key::Char('b'), &ALT, &NORMAL), Some(vec![0x1b, b'b']));
    }

    // --- Alt+Ctrl ---

    #[test]
    fn alt_ctrl_c() {
        assert_eq!(encode(&Key::Char('c'), &ALT_CTRL, &NORMAL), Some(vec![0x1b, 0x03]));
    }

    // --- Regular characters ---

    #[test]
    fn regular_char() {
        assert_eq!(encode(&Key::Char('a'), &NO_MODS, &NORMAL), Some(b"a".to_vec()));
    }

    #[test]
    fn unicode_char() {
        assert_eq!(encode(&Key::Char('é'), &NO_MODS, &NORMAL), Some("é".as_bytes().to_vec()));
    }

    // --- Modifier parameter ---

    #[test]
    fn modifier_param_none() {
        assert_eq!(modifier_param(&NO_MODS), None);
    }

    #[test]
    fn modifier_param_shift() {
        assert_eq!(modifier_param(&SHIFT), Some(2));
    }

    #[test]
    fn modifier_param_alt() {
        assert_eq!(modifier_param(&ALT), Some(3));
    }

    #[test]
    fn modifier_param_ctrl() {
        assert_eq!(modifier_param(&CTRL), Some(5));
    }

    #[test]
    fn modifier_param_shift_ctrl() {
        assert_eq!(modifier_param(&SHIFT_CTRL), Some(6));
    }

    #[test]
    fn modifier_param_alt_ctrl() {
        assert_eq!(modifier_param(&ALT_CTRL), Some(7));
    }
}

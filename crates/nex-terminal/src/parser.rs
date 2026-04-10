//! VT sequence parser — wraps `vte::Parser` and dispatches to `Term`.
//!
//! The `vte` crate handles the Paul Williams state machine (byte-level
//! recognition of CSI / OSC / ESC / DCS sequences and UTF-8 decoding). This
//! module implements `vte::Perform` to translate those dispatch calls into
//! semantic operations on our `Term` / `Grid`.
//!
//! Everything semantic — SGR interpretation, cursor motion, scroll region,
//! mode flags, alt screen, DEC line-drawing charset — lives here.

use std::marker::PhantomData;

use vte::{Params, Parser, Perform};

use crate::ansi::{Color, NamedColor, Rgb, StdSyncHandler};
use crate::cell::{Cell, Flags};
use crate::event::EventListener;
use crate::index::{Column, Line};
use crate::mode::TermMode;
use crate::term::Term;

/// Parser wrapper. Generic over the handler type only for call-site
/// compatibility with the legacy backend (`Processor::<StdSyncHandler>::new()`).
pub struct Processor<H = StdSyncHandler> {
    parser: Parser,
    _phantom: PhantomData<H>,
}

impl<H> Default for Processor<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> Processor<H> {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            _phantom: PhantomData,
        }
    }

    /// Feed PTY bytes into the parser, dispatching updates to `term`.
    pub fn advance<L: EventListener>(&mut self, term: &mut Term<L>, bytes: &[u8]) {
        let mut handler = Handler {
            term,
            g0_line_drawing: false,
            charset_selected: false,
        };
        self.parser.advance(&mut handler, bytes);
    }
}

/// The transient bridge between vte's `Perform` callbacks and our `Term`.
struct Handler<'a, L: EventListener> {
    term: &'a mut Term<L>,
    /// Whether G0 is currently pointing at the DEC line-drawing charset.
    g0_line_drawing: bool,
    /// True while we're inside an `ESC ( ?` sequence and waiting for the final byte.
    charset_selected: bool,
}

// ---------------------------------------------------------------------------
// vte::Perform impl
// ---------------------------------------------------------------------------

impl<L: EventListener> Perform for Handler<'_, L> {
    fn print(&mut self, c: char) {
        let translated = if self.g0_line_drawing {
            translate_dec_line_drawing(c)
        } else {
            c
        };
        self.term.input(translated);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.term.bell(),
            0x08 => self.term.backspace(),
            0x09 => self.term.tab(1),
            0x0A..=0x0C => self.term.linefeed(),
            0x0D => self.term.carriage_return(),
            0x0E => self.g0_line_drawing = false, // SO → G1 (we only track G0)
            0x0F => self.g0_line_drawing = false, // SI → G0
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, c: char) {
        self.dispatch_csi(params, intermediates, c);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.dispatch_esc(intermediates, byte);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.dispatch_osc(params);
    }

    // DCS — no-op for MVP. Sixel graphics and DECRQSS replies are ignored.
    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _: u8) {}
    fn unhook(&mut self) {}
}

// ---------------------------------------------------------------------------
// CSI dispatch
// ---------------------------------------------------------------------------

impl<L: EventListener> Handler<'_, L> {
    fn dispatch_csi(&mut self, params: &Params, intermediates: &[u8], c: char) {
        let private = intermediates.first() == Some(&b'?');

        // CSI sequences with intermediate bytes other than `?` (e.g. `>`,
        // `<`, `=`, ` `, `!`, multi-byte intermediates) are distinct
        // sequences from their plain counterparts — `\x1b[<u` is a Kitty
        // keyboard protocol query, not SCORC (CSI u). Treat them as
        // unhandled so they don't collide with the main dispatch table.
        let has_unknown_intermediates = !intermediates.is_empty() && !private
            || intermediates.len() > 1;
        if has_unknown_intermediates {
            // Well-known sequences that need explicit replies or handling.
            match (intermediates, c) {
                // DECSTR — soft reset (CSI ! p)
                (&[b'!'], 'p') => self.term.reset_all(),
                // DECSCUSR — cursor style (CSI Space q) — ignore for MVP
                (&[b' '], 'q') => {}
                // XTVERSION query (CSI > q) — reply with terminal name/version.
                (&[b'>'], 'q') => {
                    self.term.pty_write(format!(
                        "\x1bP>|tonn({})\x1b\\",
                        env!("CARGO_PKG_VERSION")
                    ));
                }
                // XTMODKEYS (CSI > Pm m) — modify-keys option. No reply needed.
                (&[b'>'], 'm') => {}
                // Kitty keyboard protocol pop (CSI < Pn u). No reply needed.
                (&[b'<'], 'u') => {}
                // Kitty keyboard protocol push (CSI > Pn u). No reply needed.
                (&[b'>'], 'u') => {}
                // Secondary DA (CSI > c) — reply with a plausible device attrs.
                (&[b'>'], 'c') => {
                    self.term.pty_write("\x1b[>0;1;0c".to_string());
                }
                _ => {
                    tracing::warn!(?intermediates, ?c, "unhandled CSI with intermediates");
                }
            }
            return;
        }

        // Pull numeric parameters; default zero values become the sequence's
        // documented default (typically 1).
        let n = first_param(params, 1);

        match (c, private) {
            ('@', false) => self.term.grid_mut().insert_blank(n),
            ('A', false) => self.term.grid_mut().cursor_up(n),
            ('B' | 'e', false) => self.term.grid_mut().cursor_down(n),
            ('C' | 'a', false) => self.term.grid_mut().cursor_forward(n),
            ('D', false) => self.term.grid_mut().cursor_back(n),
            ('E', false) => {
                self.term.grid_mut().cursor_down(n);
                self.term.grid_mut().carriage_return();
            }
            ('F', false) => {
                self.term.grid_mut().cursor_up(n);
                self.term.grid_mut().carriage_return();
            }
            ('G' | '`', false) => {
                let col = n.saturating_sub(1);
                let line = self.term.grid().cursor.point.line;
                self.term.grid_mut().goto(line, Column(col));
            }
            ('H' | 'f', false) => {
                let row = first_param(params, 1).saturating_sub(1);
                let col = nth_param(params, 1, 1).saturating_sub(1);
                self.term.grid_mut().goto(Line(row as i32), Column(col));
            }
            ('I', false) => self.term.tab(n),
            ('J', false) => {
                let mode = first_param(params, 0) as u16;
                self.term.grid_mut().erase_in_display(mode);
            }
            ('K', false) => {
                let mode = first_param(params, 0) as u16;
                self.term.grid_mut().erase_in_line(mode);
            }
            ('L', false) => self.term.grid_mut().insert_lines(n),
            ('M', false) => self.term.grid_mut().delete_lines(n),
            ('P', false) => self.term.grid_mut().delete_chars(n),
            ('S', false) => self.term.grid_mut().scroll_up_region(n),
            ('T', false) => self.term.grid_mut().scroll_down_region(n),
            ('X', false) => self.term.grid_mut().erase_chars(n),
            ('Z', false) => {
                // Back-tab (CBT). Approximate: step back in multiples of 8.
                for _ in 0..n {
                    let col = self.term.grid().cursor.point.column.0;
                    let prev = if col == 0 { 0 } else { ((col - 1) / 8) * 8 };
                    let line = self.term.grid().cursor.point.line;
                    self.term.grid_mut().goto(line, Column(prev));
                }
            }
            ('c', false) => {
                // Primary DA — identify as VT102.
                self.term.pty_write("\x1b[?6c".to_string());
            }
            ('d', false) => {
                let row = n.saturating_sub(1);
                self.term.grid_mut().goto_line(Line(row as i32));
            }
            ('g', false) => {
                // TBC — tab clear. We use fixed stops, so no-op.
            }
            ('h', false) => self.set_ansi_mode(params, true),
            ('l', false) => self.set_ansi_mode(params, false),
            ('h', true) => self.set_private_mode(params, true),
            ('l', true) => self.set_private_mode(params, false),
            ('m', false) => self.handle_sgr(params),
            ('n', false) => {
                let kind = first_param(params, 0);
                if kind == 5 {
                    self.term.pty_write("\x1b[0n".to_string());
                } else if kind == 6 {
                    let row = self.term.grid().cursor.point.line.0 + 1;
                    let col = self.term.grid().cursor.point.column.0 + 1;
                    self.term.pty_write(format!("\x1b[{row};{col}R"));
                }
            }
            ('r', false) => {
                let top = first_param(params, 1).saturating_sub(1);
                let bottom = nth_param(params, 1, self.term.grid().screen_lines());
                self.term.grid_mut().set_scroll_region(top, bottom);
            }
            ('s', false) => self.term.save_cursor(),
            ('u', false) => self.term.restore_cursor(),
            ('t', false) => {
                // XTWINOPS — reply to size queries, ignore the rest.
                let op = first_param(params, 0);
                if op == 18 {
                    let rows = self.term.grid().screen_lines();
                    let cols = self.term.grid().columns();
                    self.term.pty_write(format!("\x1b[8;{rows};{cols}t"));
                }
            }
            _ => {
                tracing::trace!(?c, ?private, "unhandled CSI");
            }
        }
    }

    fn set_ansi_mode(&mut self, params: &Params, enable: bool) {
        for p in params.iter() {
            if let Some(&code) = p.first()
                && code == 4 {
                    self.term.set_mode(TermMode::INSERT, enable);
                }
        }
    }

    fn set_private_mode(&mut self, params: &Params, enable: bool) {
        for p in params.iter() {
            let Some(&code) = p.first() else { continue };
            match code {
                1 => self.term.set_mode(TermMode::APP_CURSOR, enable),
                7 => self.term.set_mode(TermMode::LINE_WRAP, enable),
                25 => self.term.set_mode(TermMode::SHOW_CURSOR, enable),
                47 | 1047 => self.term.swap_alt_screen(enable, false),
                1000 => self.term.set_mode(TermMode::MOUSE_REPORT_CLICK, enable),
                1002 => self.term.set_mode(TermMode::MOUSE_DRAG, enable),
                1003 => self.term.set_mode(TermMode::MOUSE_MOTION, enable),
                1004 => self.term.set_mode(TermMode::FOCUS_IN_OUT, enable),
                1006 => self.term.set_mode(TermMode::SGR_MOUSE, enable),
                1048 => {
                    if enable {
                        self.term.save_cursor();
                    } else {
                        self.term.restore_cursor();
                    }
                }
                1049 => self.term.swap_alt_screen(enable, true),
                2004 => self.term.set_mode(TermMode::BRACKETED_PASTE, enable),
                // Synchronized output mode — we process bytes immediately so
                // there's no buffering, but apps set/reset this constantly.
                2026 => {}
                // Color-scheme-change notification mode. Silently accept.
                2031 => {}
                _ => tracing::trace!(code, enable, "unhandled private mode"),
            }
        }
    }

    // -----------------------------------------------------------------
    // SGR (Select Graphic Rendition)
    // -----------------------------------------------------------------

    fn handle_sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.term.reset_template();
            return;
        }

        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            let Some(&v) = param.first() else { continue };

            // Colon-delimited sub-params take priority for extended colours.
            if (v == 38 || v == 48) && param.len() > 1
                && let Some(color) = parse_extended_from_subparams(&param[1..]) {
                    let is_fg = v == 38;
                    self.apply_sgr_color(color, is_fg);
                    continue;
                }

            match v {
                0 => self.term.reset_template(),
                1 => self.set_flag(Flags::BOLD, true),
                2 => self.set_flag(Flags::DIM, true),
                3 => self.set_flag(Flags::ITALIC, true),
                4 => self.set_flag(Flags::UNDERLINE, true),
                7 => self.set_flag(Flags::INVERSE, true),
                8 => self.set_flag(Flags::HIDDEN, true),
                9 => self.set_flag(Flags::STRIKEOUT, true),
                21 | 22 => {
                    self.set_flag(Flags::BOLD, false);
                    self.set_flag(Flags::DIM, false);
                }
                23 => self.set_flag(Flags::ITALIC, false),
                24 => self.set_flag(Flags::UNDERLINE, false),
                27 => self.set_flag(Flags::INVERSE, false),
                28 => self.set_flag(Flags::HIDDEN, false),
                29 => self.set_flag(Flags::STRIKEOUT, false),
                30..=37 => self.set_fg(Color::Named(NamedColor::from_palette_index((v - 30) as u8))),
                38 => {
                    if let Some(color) = parse_extended_from_iter(&mut iter) {
                        self.apply_sgr_color(color, true);
                    }
                }
                39 => self.set_fg(Color::Named(NamedColor::Foreground)),
                40..=47 => self.set_bg(Color::Named(NamedColor::from_palette_index((v - 40) as u8))),
                48 => {
                    if let Some(color) = parse_extended_from_iter(&mut iter) {
                        self.apply_sgr_color(color, false);
                    }
                }
                49 => self.set_bg(Color::Named(NamedColor::Background)),
                90..=97 => self.set_fg(Color::Named(NamedColor::from_palette_index((v - 90 + 8) as u8))),
                100..=107 => self.set_bg(Color::Named(NamedColor::from_palette_index((v - 100 + 8) as u8))),
                _ => {}
            }
        }
    }

    fn set_flag(&mut self, flag: Flags, enable: bool) {
        let t = self.term.template_mut();
        if enable {
            t.flags.insert(flag);
        } else {
            t.flags.remove(flag);
        }
    }

    fn set_fg(&mut self, color: Color) {
        self.term.template_mut().fg = color;
    }

    fn set_bg(&mut self, color: Color) {
        self.term.template_mut().bg = color;
    }

    fn apply_sgr_color(&mut self, color: Color, is_fg: bool) {
        if is_fg {
            self.set_fg(color);
        } else {
            self.set_bg(color);
        }
    }

    // -----------------------------------------------------------------
    // ESC dispatch
    // -----------------------------------------------------------------

    fn dispatch_esc(&mut self, intermediates: &[u8], byte: u8) {
        match (intermediates.first(), byte) {
            (None, b'7') => self.term.save_cursor(),
            (None, b'8') => self.term.restore_cursor(),
            (None, b'=') => self.term.set_mode(TermMode::APP_KEYPAD, true),
            (None, b'>') => self.term.set_mode(TermMode::APP_KEYPAD, false),
            (None, b'D') => self.term.linefeed(),
            (None, b'E') => {
                self.term.carriage_return();
                self.term.linefeed();
            }
            (None, b'H') => {
                // HTS — fixed tab stops, so no-op.
            }
            (None, b'M') => self.term.grid_mut().reverse_index(),
            (None, b'c') => self.term.reset_all(),
            (Some(&b'('), byte) => {
                self.g0_line_drawing = byte == b'0';
                self.charset_selected = true;
            }
            _ => {
                tracing::trace!(?intermediates, byte, "unhandled ESC");
            }
        }
    }

    // -----------------------------------------------------------------
    // OSC dispatch
    // -----------------------------------------------------------------

    fn dispatch_osc(&mut self, params: &[&[u8]]) {
        if params.is_empty() {
            return;
        }
        let Ok(num_str) = std::str::from_utf8(params[0]) else { return };
        let num: u64 = match num_str.parse() {
            Ok(n) => n,
            Err(_) => return,
        };

        match num {
            0 | 2 => {
                if let Some(title_bytes) = params.get(1)
                    && let Ok(title) = std::str::from_utf8(title_bytes) {
                        self.term.set_title(Some(title.to_string()));
                    }
            }
            // OSC 1 (icon name), OSC 7 (cwd), OSC 9 (notification): ignore.
            1 | 7 | 9 => {}
            // OSC 133 (shell integration) and OSC 1337 (iTerm2 CWD) are
            // handled by `nex-shell-integration`'s OSC scanner which sees the
            // raw bytes before they reach the parser. We silently absorb them
            // here to prevent the payload from being printed.
            133 | 1337 => {}
            _ => {
                tracing::trace!(num, "unhandled OSC");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter helpers
// ---------------------------------------------------------------------------

fn first_param(params: &Params, default: usize) -> usize {
    params
        .iter()
        .next()
        .and_then(|p| p.first().copied())
        .map(|v| if v == 0 { default } else { v as usize })
        .unwrap_or(default)
}

fn nth_param(params: &Params, n: usize, default: usize) -> usize {
    params
        .iter()
        .nth(n)
        .and_then(|p| p.first().copied())
        .map(|v| if v == 0 { default } else { v as usize })
        .unwrap_or(default)
}

/// Parse a `38;5;n` or `38;2;r;g;b` sequence where the sub-parameters come
/// from following semicolon-separated params. Advances the iterator.
fn parse_extended_from_iter<'a, I>(iter: &mut I) -> Option<Color>
where
    I: Iterator<Item = &'a [u16]>,
{
    let mode = iter.next()?.first().copied()?;
    match mode {
        2 => {
            let r = iter.next()?.first().copied()? as u8;
            let g = iter.next()?.first().copied()? as u8;
            let b = iter.next()?.first().copied()? as u8;
            Some(Color::Spec(Rgb { r, g, b }))
        }
        5 => {
            let idx = iter.next()?.first().copied()? as u8;
            Some(Color::Indexed(idx))
        }
        _ => None,
    }
}

/// Parse a `38:5:n` or `38:2::r:g:b` sequence where the colour comes from
/// colon-delimited sub-parameters within a single param group.
fn parse_extended_from_subparams(sub: &[u16]) -> Option<Color> {
    let mode = *sub.first()?;
    match mode {
        2 => {
            // Common forms: `38:2::r:g:b` (4 sub-params) or `38:2:r:g:b` (3).
            // We try both.
            let (r, g, b) = match sub.len() {
                4 => (sub[1], sub[2], sub[3]),
                5 => (sub[2], sub[3], sub[4]),
                _ => return None,
            };
            Some(Color::Spec(Rgb { r: r as u8, g: g as u8, b: b as u8 }))
        }
        5 => {
            let idx = *sub.get(1)? as u8;
            Some(Color::Indexed(idx))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// DEC line-drawing charset translation
// ---------------------------------------------------------------------------

/// Translate an ASCII character to its DEC Special Graphics equivalent.
/// Only ASCII letters `j`..`~` map; everything else passes through.
fn translate_dec_line_drawing(c: char) -> char {
    match c {
        '_' => ' ',
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Unused cell import (silences warning — `Cell` is re-imported by term impl)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn _cell_assert(_: Cell) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::event::Event;
    use crate::grid::Dimensions;

    struct NoopListener;
    impl EventListener for NoopListener {
        fn send_event(&self, _: Event) {}
    }

    fn term(cols: usize, rows: usize) -> Term<NoopListener> {
        struct Dim(usize, usize);
        impl Dimensions for Dim {
            fn total_lines(&self) -> usize { self.1 }
            fn screen_lines(&self) -> usize { self.1 }
            fn columns(&self) -> usize { self.0 }
        }
        Term::new(Config { scrolling_history: 100 }, &Dim(cols, rows), NoopListener)
    }

    fn feed(t: &mut Term<NoopListener>, bytes: &[u8]) {
        let mut p = Processor::<StdSyncHandler>::new();
        p.advance(t, bytes);
    }

    #[test]
    fn plain_text() {
        let mut t = term(20, 3);
        feed(&mut t, b"hello");
        assert_eq!(t.grid()[Line(0)].cells[0].c, 'h');
        assert_eq!(t.grid()[Line(0)].cells[4].c, 'o');
    }

    #[test]
    fn cr_lf() {
        let mut t = term(20, 3);
        feed(&mut t, b"abc\r\nxyz");
        assert_eq!(t.grid()[Line(0)].cells[0].c, 'a');
        assert_eq!(t.grid()[Line(1)].cells[0].c, 'x');
    }

    #[test]
    fn sgr_red_then_reset() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[31mred\x1b[0mok");
        assert_eq!(
            t.grid()[Line(0)].cells[0].fg,
            Color::Named(NamedColor::Red)
        );
        assert_eq!(
            t.grid()[Line(0)].cells[3].fg,
            Color::Named(NamedColor::Foreground)
        );
    }

    #[test]
    fn sgr_truecolor_semicolons() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[38;2;10;20;30mx");
        assert_eq!(
            t.grid()[Line(0)].cells[0].fg,
            Color::Spec(Rgb { r: 10, g: 20, b: 30 })
        );
    }

    #[test]
    fn sgr_indexed_256() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[38;5;196mx");
        assert_eq!(t.grid()[Line(0)].cells[0].fg, Color::Indexed(196));
    }

    #[test]
    fn bold_flag() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[1mBold");
        assert!(t.grid()[Line(0)].cells[0].flags.contains(Flags::BOLD));
    }

    #[test]
    fn cursor_position_cup() {
        let mut t = term(20, 5);
        feed(&mut t, b"\x1b[3;5Hx");
        // CUP is 1-based: row 3, col 5 → Line(2), Column(4)
        assert_eq!(t.grid()[Line(2)].cells[4].c, 'x');
    }

    #[test]
    fn erase_in_display_all() {
        let mut t = term(20, 3);
        feed(&mut t, b"hello\x1b[2J");
        assert_eq!(t.grid()[Line(0)].cells[0].c, ' ');
    }

    #[test]
    fn alt_screen_1049() {
        let mut t = term(20, 3);
        feed(&mut t, b"main");
        feed(&mut t, b"\x1b[?1049h"); // enter alt
        assert_eq!(t.grid()[Line(0)].cells[0].c, ' ');
        feed(&mut t, b"alt");
        assert_eq!(t.grid()[Line(0)].cells[0].c, 'a');
        feed(&mut t, b"\x1b[?1049l"); // leave alt
        assert_eq!(t.grid()[Line(0)].cells[0].c, 'm');
    }

    #[test]
    fn app_cursor_mode() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[?1h");
        assert!(t.mode().contains(TermMode::APP_CURSOR));
        feed(&mut t, b"\x1b[?1l");
        assert!(!t.mode().contains(TermMode::APP_CURSOR));
    }

    #[test]
    fn bracketed_paste_mode() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b[?2004h");
        assert!(t.mode().contains(TermMode::BRACKETED_PASTE));
    }

    #[test]
    fn claude_code_banner_repro() {
        let mut t = term(40, 3);
        // Common patterns that could emit "Claude Code v2.1.100".
        feed(&mut t, b"\x1b[1;38;2;255;153;0mClaude Code v2.1.100\x1b[0m");
        let row = &t.grid()[Line(0)];
        let text: String = (0..20).map(|i| row.cells[i].c).collect();
        assert_eq!(text, "Claude Code v2.1.100", "truecolor bold form");
    }

    #[test]
    fn claude_code_banner_colon_form() {
        let mut t = term(40, 3);
        feed(&mut t, b"\x1b[1;38:2::255:153:0mClaude Code v2.1.100\x1b[0m");
        let row = &t.grid()[Line(0)];
        let text: String = (0..20).map(|i| row.cells[i].c).collect();
        assert_eq!(text, "Claude Code v2.1.100", "truecolor colon form");
    }

    #[test]
    fn claude_code_banner_256_indexed() {
        let mut t = term(40, 3);
        feed(&mut t, b"\x1b[1;38;5;208mClaude Code v2.1.100\x1b[0m");
        let row = &t.grid()[Line(0)];
        let text: String = (0..20).map(|i| row.cells[i].c).collect();
        assert_eq!(text, "Claude Code v2.1.100", "256-indexed form");
    }

    #[test]
    fn csi_lt_u_is_not_cursor_restore() {
        // `\x1b[<u` is a Kitty keyboard protocol query, NOT CSI u (SCORC).
        // If we treat it as SCORC, any earlier DECSC (`\x1b7`) would cause
        // the cursor to teleport mid-stream — which was the root cause of
        // Claude Code's banner showing stale characters from the trust
        // prompt in CUF-skipped cells.
        let mut t = term(20, 3);
        feed(&mut t, b"hello\x1b7");              // save cursor at col 5
        feed(&mut t, b"\r\nworld");                // cursor at row 1 col 5
        feed(&mut t, b"\x1b[<u");                  // MUST NOT restore
        feed(&mut t, b"!");
        // Cursor should still be on row 1 after "world", not jumped back.
        assert_eq!(t.grid()[Line(1)].cells[5].c, '!');
    }

    #[test]
    fn cuf_skips_without_writing_spaces() {
        // Claude Code uses CSI 1 C instead of literal spaces to position
        // between words. The skipped cells should still be whatever was
        // previously at that position — if the grid was blank, they should
        // render as spaces.
        let mut t = term(40, 3);
        feed(&mut t, b"Claude\x1b[1CCode\x1b[1Cv2.1.100");
        let row = &t.grid()[Line(0)];
        let text: String = row.cells.iter().take(20).map(|c| c.c).collect();
        assert_eq!(text, "Claude Code v2.1.100", "cuf-skipped cells should be blank");
    }

    #[test]
    fn claude_code_box_with_line_drawing() {
        // Common TUI pattern: draw box border with DEC line drawing, exit,
        // write text, draw side border with line drawing again.
        let mut t = term(40, 3);
        // Line 0: top border
        feed(&mut t, b"\x1b(0lqqqqqqqqqqqqqqqqqqqqqqqqk\x1b(B\r\n");
        // Line 1: side border + title + side border
        feed(&mut t, b"\x1b(0x\x1b(B Claude Code v2.1.100 \x1b(0x\x1b(B\r\n");
        let row = &t.grid()[Line(1)];
        let text: String = row.cells.iter().take(24).map(|c| c.c).collect();
        assert_eq!(text, "│ Claude Code v2.1.100 │");
    }

    #[test]
    fn line_drawing_charset() {
        let mut t = term(20, 3);
        feed(&mut t, b"\x1b(0lqk");
        assert_eq!(t.grid()[Line(0)].cells[0].c, '┌');
        assert_eq!(t.grid()[Line(0)].cells[1].c, '─');
        assert_eq!(t.grid()[Line(0)].cells[2].c, '┐');
        feed(&mut t, b"\x1b(B");
        feed(&mut t, b"x");
        assert_eq!(t.grid()[Line(0)].cells[3].c, 'x');
    }
}

//! Top-level `Term` struct: owns grid, alt grid, modes, selection, event listener.
//!
//! The parser calls the `pub(crate)` mutation methods on `Term`; Tonn reads
//! grid content through `grid()`, checks modes via `mode()`, and manages
//! selection through the `selection` field.

use crate::cell::{Cell, Flags};
use crate::config::Config;
use crate::event::{Event, EventListener};
use crate::grid::{Cursor, Dimensions, Grid};
#[cfg_attr(not(test), allow(unused_imports))]
use crate::index::{Column, Line, Point};
use crate::mode::TermMode;
use crate::selection::Selection;

pub struct Term<L: EventListener> {
    grid: Grid,
    alt_grid: Grid,
    mode: TermMode,
    current_title: Option<String>,
    title_stack: Vec<Option<String>>,
    pub selection: Option<Selection>,
    /// Saved cursor state for the inactive (non-alt) screen, used by
    /// DECSET/DECRST 1049 so entering and leaving alt-screen preserves the
    /// main-screen cursor.
    saved_cursor_main: Cursor,
    saved_cursor_alt: Cursor,
    event_listener: L,
}

impl<L: EventListener> Term<L> {
    pub fn new<D: Dimensions>(config: Config, dim: &D, listener: L) -> Self {
        let cols = dim.columns();
        let screen_lines = dim.screen_lines();
        let history = config.scrolling_history;
        Self {
            grid: Grid::new(cols, screen_lines, history),
            alt_grid: Grid::new(cols, screen_lines, 0),
            mode: TermMode::default_mode(),
            current_title: None,
            title_stack: Vec::new(),
            selection: None,
            saved_cursor_main: Cursor::default(),
            saved_cursor_alt: Cursor::default(),
            event_listener: listener,
        }
    }

    // ---------------------------------------------------------------------
    // Read-only accessors
    // ---------------------------------------------------------------------

    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.grid
    }

    pub fn mode(&self) -> TermMode {
        self.mode
    }

    // ---------------------------------------------------------------------
    // Resize
    // ---------------------------------------------------------------------

    pub fn resize<D: Dimensions>(&mut self, dim: D) {
        let cols = dim.columns();
        let lines = dim.screen_lines();
        self.grid.resize(cols, lines);
        self.alt_grid.resize(cols, lines);
    }

    // ---------------------------------------------------------------------
    // Selection copy-to-string
    // ---------------------------------------------------------------------

    pub fn selection_to_string(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        let range = sel.to_range(self)?;
        let mut out = String::new();
        let last_col = self.grid.columns().saturating_sub(1);
        let last_line = range.end.line.0;
        for line in range.start.line.0..=last_line {
            let row = &self.grid[Line(line)];
            let start_col = if line == range.start.line.0 { range.start.column.0 } else { 0 };
            let end_col = if line == last_line { range.end.column.0 } else { last_col };
            let mut buf = String::new();
            for col in start_col..=end_col.min(last_col) {
                let c = row.cells[col].c;
                buf.push(if c == '\0' { ' ' } else { c });
            }
            // Trim trailing spaces on non-wrapped lines so copied text looks
            // like what the user sees, but keep them on wrapped lines so the
            // content reassembles correctly.
            if line != last_line {
                if !row.wrapped {
                    out.push_str(buf.trim_end());
                    out.push('\n');
                } else {
                    out.push_str(&buf);
                }
            } else {
                out.push_str(buf.trim_end());
            }
        }
        Some(out)
    }

    // ---------------------------------------------------------------------
    // Parser-facing mutation surface
    // ---------------------------------------------------------------------

    pub(crate) fn input(&mut self, c: char) {
        let wrap = self.mode.contains(TermMode::LINE_WRAP);
        self.grid.input(c, wrap);
    }

    pub(crate) fn linefeed(&mut self) {
        self.grid.line_feed();
    }

    pub(crate) fn carriage_return(&mut self) {
        self.grid.carriage_return();
    }

    pub(crate) fn backspace(&mut self) {
        self.grid.backspace();
    }

    pub(crate) fn tab(&mut self, n: usize) {
        self.grid.tab(n);
    }

    pub(crate) fn bell(&mut self) {
        self.event_listener.send_event(Event::Bell);
    }

    pub(crate) fn set_title(&mut self, title: Option<String>) {
        self.current_title = title.clone();
        match title {
            Some(t) => self.event_listener.send_event(Event::Title(t)),
            None => self.event_listener.send_event(Event::ResetTitle),
        }
    }

    pub(crate) fn pty_write(&mut self, text: String) {
        self.event_listener.send_event(Event::PtyWrite(text));
    }

    pub(crate) fn set_mode(&mut self, flag: TermMode, enable: bool) {
        if enable {
            self.mode.insert(flag);
        } else {
            self.mode.remove(flag);
        }
    }

    /// Enter or leave the alternate screen. When `save_cursor` is true
    /// (DECSET 1049) the cursor state is preserved across the swap.
    pub(crate) fn swap_alt_screen(&mut self, to_alt: bool, save_cursor: bool) {
        let currently_alt = self.mode.contains(TermMode::ALT_SCREEN);
        if currently_alt == to_alt {
            return;
        }

        if save_cursor {
            if to_alt {
                self.saved_cursor_main = self.grid.cursor;
            } else {
                self.saved_cursor_alt = self.grid.cursor;
            }
        }

        std::mem::swap(&mut self.grid, &mut self.alt_grid);
        self.mode.set(TermMode::ALT_SCREEN, to_alt);

        if to_alt {
            // Entering alt: clear the alt buffer and park cursor at origin.
            self.grid.erase_in_display(2);
            self.grid.goto(Line(0), Column(0));
        }

        if save_cursor {
            self.grid.cursor = if to_alt {
                self.saved_cursor_alt
            } else {
                self.saved_cursor_main
            };
        }
    }

    pub(crate) fn reset_all(&mut self) {
        self.grid.full_reset();
        self.alt_grid.full_reset();
        self.mode = TermMode::default_mode();
        self.current_title = None;
        self.title_stack.clear();
        self.selection = None;
    }

    /// Save cursor (ESC 7 / CSI s).
    pub(crate) fn save_cursor(&mut self) {
        self.grid.save_cursor();
    }

    /// Restore cursor (ESC 8 / CSI u).
    pub(crate) fn restore_cursor(&mut self) {
        self.grid.restore_cursor();
    }

    /// Apply an SGR attribute change to the cursor's template cell.
    pub(crate) fn template_mut(&mut self) -> &mut Cell {
        self.grid.template_mut()
    }

    /// Clear the SGR template — used for SGR 0.
    pub(crate) fn reset_template(&mut self) {
        let t = self.grid.template_mut();
        t.fg = Cell::default().fg;
        t.bg = Cell::default().bg;
        t.flags = Flags::empty();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

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

    #[test]
    fn input_roundtrips() {
        let mut t = term(10, 3);
        for c in "hi".chars() { t.input(c); }
        assert_eq!(t.grid()[Line(0)].cells[0].c, 'h');
        assert_eq!(t.grid()[Line(0)].cells[1].c, 'i');
    }

    #[test]
    fn alt_screen_swap_preserves_main() {
        let mut t = term(10, 3);
        for c in "main".chars() { t.input(c); }
        t.swap_alt_screen(true, true);
        // Alt grid is empty.
        assert_eq!(t.grid()[Line(0)].cells[0].c, ' ');
        // Write something on alt.
        for c in "alt".chars() { t.input(c); }
        assert_eq!(t.grid()[Line(0)].cells[0].c, 'a');
        // Swap back.
        t.swap_alt_screen(false, true);
        assert_eq!(t.grid()[Line(0)].cells[0].c, 'm');
        assert_eq!(t.grid()[Line(0)].cells[3].c, 'n');
    }

    #[test]
    fn selection_to_string_trims_trailing_spaces() {
        let mut t = term(10, 3);
        for c in "abc".chars() { t.input(c); }
        t.selection = Some(Selection::new(
            crate::selection::SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            crate::index::Side::Left,
        ));
        if let Some(s) = &mut t.selection {
            s.update(Point::new(Line(0), Column(9)), crate::index::Side::Right);
        }
        assert_eq!(t.selection_to_string().unwrap(), "abc");
    }

    #[test]
    fn resize_clamps_cursor() {
        let mut t = term(10, 3);
        t.grid_mut().goto(Line(2), Column(9));
        struct Dim;
        impl Dimensions for Dim {
            fn total_lines(&self) -> usize { 2 }
            fn screen_lines(&self) -> usize { 2 }
            fn columns(&self) -> usize { 5 }
        }
        t.resize(Dim);
        assert!(t.grid().cursor.point.line.0 < 2);
        assert!(t.grid().cursor.point.column.0 < 5);
    }
}

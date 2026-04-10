//! Grid storage, scrollback buffer, cursor state, and `Dimensions` trait.
//!
//! # Model
//!
//! The grid holds `scrollback_len + screen_lines` rows in a single `VecDeque`,
//! oldest rows at the front. The visible screen is always the last
//! `screen_lines` rows of the deque. A separate `display_offset` shifts the
//! viewport window backwards into the scrollback history.
//!
//! `Line(0)` addresses the top of the visible screen when `display_offset == 0`.
//! Negative values address rows above the visible screen (scrollback).
//!
//! # Scroll-up algorithm
//!
//! When the cursor advances past the bottom of the scroll region and the
//! region is the entire screen, we append a blank row at the back of the deque
//! and (if over the history limit) drop the oldest row from the front. The
//! "screen top" index slides forward naturally — no rows move.

use std::collections::VecDeque;
use std::ops::{Index, IndexMut, Range};

use crate::cell::Cell;
#[cfg(test)]
use crate::cell::Flags;
use crate::index::{Column, Line, Point};

/// Implemented by anything that can report terminal dimensions.
pub trait Dimensions {
    fn total_lines(&self) -> usize;
    fn screen_lines(&self) -> usize;
    fn columns(&self) -> usize;

    fn last_column(&self) -> Column {
        Column(self.columns().saturating_sub(1))
    }
    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines() as i32 - 1)
    }
}

/// Scrollback navigation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    Delta(i32),
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// Cursor state. Carries a `template` cell holding the current SGR attributes
/// so new characters inherit them without a separate lookup.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    pub point: Point,
    pub template: Cell,
    pub input_needs_wrap: bool,
}


/// A single row of cells.
#[derive(Debug, Clone)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Set when this row's content wrapped into the next row (for
    /// paste/selection newline suppression).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
    }

    pub fn resize(&mut self, cols: usize) {
        self.cells.resize(cols, Cell::default());
    }

    pub fn clear_with(&mut self, template: &Cell) {
        for c in &mut self.cells {
            c.reset_with(template);
        }
        self.wrapped = false;
    }
}

impl Index<Column> for Row {
    type Output = Cell;
    fn index(&self, c: Column) -> &Cell {
        &self.cells[c.0]
    }
}
impl IndexMut<Column> for Row {
    fn index_mut(&mut self, c: Column) -> &mut Cell {
        &mut self.cells[c.0]
    }
}

/// A dense grid with scrollback history.
#[derive(Debug)]
pub struct Grid {
    rows: VecDeque<Row>,
    cols: usize,
    screen_lines: usize,
    history_limit: usize,
    display_offset: usize,
    pub cursor: Cursor,
    saved_cursor: Cursor,
    /// Top (inclusive) and bottom (exclusive) screen-relative line bounds for
    /// DECSTBM scroll region.
    scroll_region: Range<usize>,
}

impl Grid {
    pub fn new(cols: usize, screen_lines: usize, history_limit: usize) -> Self {
        let mut rows = VecDeque::with_capacity(screen_lines + history_limit.min(1024));
        for _ in 0..screen_lines {
            rows.push_back(Row::new(cols));
        }
        Self {
            rows,
            cols,
            screen_lines,
            history_limit,
            display_offset: 0,
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            scroll_region: 0..screen_lines,
        }
    }

    // ---------------------------------------------------------------------
    // Read-only accessors
    // ---------------------------------------------------------------------

    pub fn columns(&self) -> usize {
        self.cols
    }

    pub fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    pub fn total_lines(&self) -> usize {
        self.rows.len()
    }

    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    pub fn history_size(&self) -> usize {
        self.rows.len() - self.screen_lines
    }

    // ---------------------------------------------------------------------
    // Indexing
    // ---------------------------------------------------------------------

    /// Translate a screen-relative `Line` into a deque index.
    ///
    /// `Line(0)` is the top of the *visible* screen (honouring display_offset).
    /// Negative values go into scrollback.
    fn line_to_index(&self, line: Line) -> usize {
        let screen_top = self.rows.len() - self.screen_lines - self.display_offset;
        let idx = screen_top as i32 + line.0;
        idx.clamp(0, self.rows.len() as i32 - 1) as usize
    }

    /// Line of the cursor in deque-index space (ignores display_offset).
    fn cursor_deque_index(&self) -> usize {
        let screen_top = self.rows.len() - self.screen_lines;
        screen_top + self.cursor.point.line.0.max(0) as usize
    }

    // ---------------------------------------------------------------------
    // Scrollback display control
    // ---------------------------------------------------------------------

    pub fn scroll_display(&mut self, scroll: Scroll) {
        let history = self.history_size();
        let new_offset = match scroll {
            Scroll::Delta(d) => {
                let cur = self.display_offset as i32;
                (cur + d).max(0).min(history as i32) as usize
            }
            Scroll::PageUp => (self.display_offset + self.screen_lines).min(history),
            Scroll::PageDown => self.display_offset.saturating_sub(self.screen_lines),
            Scroll::Top => history,
            Scroll::Bottom => 0,
        };
        self.display_offset = new_offset;
    }

    // ---------------------------------------------------------------------
    // Cursor motion
    // ---------------------------------------------------------------------

    pub fn cursor_up(&mut self, n: usize) {
        let top = self.scroll_region.start as i32;
        let new = (self.cursor.point.line.0 - n as i32).max(top);
        self.cursor.point.line = Line(new);
        self.cursor.input_needs_wrap = false;
    }

    pub fn cursor_down(&mut self, n: usize) {
        let bottom = self.scroll_region.end as i32 - 1;
        let new = (self.cursor.point.line.0 + n as i32).min(bottom);
        self.cursor.point.line = Line(new);
        self.cursor.input_needs_wrap = false;
    }

    pub fn cursor_forward(&mut self, n: usize) {
        let new = (self.cursor.point.column.0 + n).min(self.cols - 1);
        self.cursor.point.column = Column(new);
        self.cursor.input_needs_wrap = false;
    }

    pub fn cursor_back(&mut self, n: usize) {
        self.cursor.point.column = Column(self.cursor.point.column.0.saturating_sub(n));
        self.cursor.input_needs_wrap = false;
    }

    pub fn goto(&mut self, line: Line, col: Column) {
        let line = line.0.clamp(0, self.screen_lines as i32 - 1);
        let col = col.0.min(self.cols - 1);
        self.cursor.point = Point::new(Line(line), Column(col));
        self.cursor.input_needs_wrap = false;
    }

    pub fn goto_line(&mut self, line: Line) {
        let col = self.cursor.point.column;
        self.goto(line, col);
    }

    pub fn goto_col(&mut self, col: Column) {
        let line = self.cursor.point.line;
        self.goto(line, col);
    }

    // ---------------------------------------------------------------------
    // Text input / control characters
    // ---------------------------------------------------------------------

    /// Write a character at the cursor, advancing by one column. Honours
    /// `line_wrap` for deferred wrap-to-next-line.
    pub fn input(&mut self, c: char, line_wrap: bool) {
        if self.cursor.input_needs_wrap {
            if line_wrap {
                let idx = self.cursor_deque_index();
                self.rows[idx].wrapped = true;
                self.cursor.point.column = Column(0);
                self.line_feed();
            } else {
                self.cursor.point.column = Column(self.cols - 1);
            }
            self.cursor.input_needs_wrap = false;
        }

        let template = self.cursor.template;
        let col = self.cursor.point.column.0;
        let idx = self.cursor_deque_index();
        let cell = &mut self.rows[idx].cells[col];
        cell.c = c;
        cell.fg = template.fg;
        cell.bg = template.bg;
        cell.flags = template.flags;

        if col + 1 >= self.cols {
            if line_wrap {
                self.cursor.input_needs_wrap = true;
            }
            // else: cursor stays at last column, next input overwrites.
        } else {
            self.cursor.point.column = Column(col + 1);
        }
    }

    pub fn carriage_return(&mut self) {
        self.cursor.point.column = Column(0);
        self.cursor.input_needs_wrap = false;
    }

    pub fn backspace(&mut self) {
        if self.cursor.point.column.0 > 0 {
            self.cursor.point.column.0 -= 1;
        }
        self.cursor.input_needs_wrap = false;
    }

    pub fn tab(&mut self, n: usize) {
        // Fixed tab stops every 8 columns.
        for _ in 0..n {
            let next = ((self.cursor.point.column.0 / 8) + 1) * 8;
            self.cursor.point.column = Column(next.min(self.cols - 1));
            if self.cursor.point.column.0 >= self.cols - 1 {
                break;
            }
        }
        self.cursor.input_needs_wrap = false;
    }

    /// Move cursor down one row, scrolling the region if at the bottom.
    pub fn line_feed(&mut self) {
        let bottom = self.scroll_region.end as i32 - 1;
        if self.cursor.point.line.0 == bottom {
            self.scroll_up_region(1);
        } else {
            self.cursor.point.line.0 += 1;
        }
        self.cursor.input_needs_wrap = false;
    }

    /// Reverse index: move cursor up one row, scrolling the region downwards
    /// if already at the top.
    pub fn reverse_index(&mut self) {
        let top = self.scroll_region.start as i32;
        if self.cursor.point.line.0 == top {
            self.scroll_down_region(1);
        } else {
            self.cursor.point.line.0 -= 1;
        }
        self.cursor.input_needs_wrap = false;
    }

    // ---------------------------------------------------------------------
    // Scrolling (DECSTBM scroll region)
    // ---------------------------------------------------------------------

    /// Scroll the region up by `n` rows (lines move towards the top).
    ///
    /// When the region covers the whole screen, the scrolled-off rows go into
    /// scrollback history. When the region is partial, rows within the region
    /// are rotated in-place and the bottom `n` rows are cleared.
    pub fn scroll_up_region(&mut self, n: usize) {
        let region_size = self.scroll_region.end - self.scroll_region.start;
        let n = n.min(region_size);
        if n == 0 {
            return;
        }

        let full_screen = self.scroll_region.start == 0 && self.scroll_region.end == self.screen_lines;
        let template = self.cursor.template;

        if full_screen {
            for _ in 0..n {
                let mut row = Row::new(self.cols);
                row.clear_with(&template);
                self.rows.push_back(row);
                if self.rows.len() > self.history_limit + self.screen_lines {
                    self.rows.pop_front();
                }
            }
            // Reset display_offset to clamp to new history.
            if self.display_offset > self.history_size() {
                self.display_offset = self.history_size();
            }
        } else {
            // Rotate within the region. Rows at [top..top+n) are dropped,
            // rows at [top+n..bottom) slide up to [top..bottom-n), and
            // rows [bottom-n..bottom) become blank.
            let screen_top = self.rows.len() - self.screen_lines;
            for i in self.scroll_region.start..(self.scroll_region.end - n) {
                let src = screen_top + i + n;
                let dst = screen_top + i;
                self.rows.swap(src, dst);
            }
            for i in (self.scroll_region.end - n)..self.scroll_region.end {
                let idx = screen_top + i;
                self.rows[idx].clear_with(&template);
            }
        }
    }

    /// Scroll the region down by `n` rows (lines move towards the bottom).
    pub fn scroll_down_region(&mut self, n: usize) {
        let region_size = self.scroll_region.end - self.scroll_region.start;
        let n = n.min(region_size);
        if n == 0 {
            return;
        }
        let template = self.cursor.template;
        let screen_top = self.rows.len() - self.screen_lines;

        // Move rows down: [top..bottom-n) -> [top+n..bottom)
        // Walk in reverse so we don't overwrite.
        for i in (self.scroll_region.start..(self.scroll_region.end - n)).rev() {
            let src = screen_top + i;
            let dst = screen_top + i + n;
            self.rows.swap(src, dst);
        }
        // Clear top n rows.
        for i in self.scroll_region.start..(self.scroll_region.start + n) {
            let idx = screen_top + i;
            self.rows[idx].clear_with(&template);
        }
    }

    // ---------------------------------------------------------------------
    // Erase / insert / delete
    // ---------------------------------------------------------------------

    /// ED — Erase in Display. `mode`: 0 = below cursor, 1 = above cursor, 2 = all, 3 = all+scrollback.
    pub fn erase_in_display(&mut self, mode: u16) {
        let template = self.cursor.template;
        let screen_top = self.rows.len() - self.screen_lines;
        let cursor_line = self.cursor.point.line.0 as usize;

        match mode {
            0 => {
                // From cursor to end of screen.
                self.erase_in_line(0);
                for l in (cursor_line + 1)..self.screen_lines {
                    self.rows[screen_top + l].clear_with(&template);
                }
            }
            1 => {
                // From start of screen to cursor.
                for l in 0..cursor_line {
                    self.rows[screen_top + l].clear_with(&template);
                }
                self.erase_in_line(1);
            }
            2 => {
                for l in 0..self.screen_lines {
                    self.rows[screen_top + l].clear_with(&template);
                }
            }
            3 => {
                for l in 0..self.screen_lines {
                    self.rows[screen_top + l].clear_with(&template);
                }
                // Drop scrollback.
                while self.rows.len() > self.screen_lines {
                    self.rows.pop_front();
                }
                self.display_offset = 0;
            }
            _ => {}
        }
    }

    /// EL — Erase in Line. `mode`: 0 = right of cursor, 1 = left of cursor, 2 = whole line.
    pub fn erase_in_line(&mut self, mode: u16) {
        let template = self.cursor.template;
        let col = self.cursor.point.column.0;
        let idx = self.cursor_deque_index();
        let row = &mut self.rows[idx];
        let range: Range<usize> = match mode {
            0 => col..self.cols,
            1 => 0..(col + 1),
            2 => 0..self.cols,
            _ => return,
        };
        for i in range {
            row.cells[i].reset_with(&template);
        }
    }

    /// ECH — Erase `n` characters to the right without moving the cursor.
    pub fn erase_chars(&mut self, n: usize) {
        let template = self.cursor.template;
        let col = self.cursor.point.column.0;
        let idx = self.cursor_deque_index();
        let row = &mut self.rows[idx];
        let end = (col + n).min(self.cols);
        for i in col..end {
            row.cells[i].reset_with(&template);
        }
    }

    /// DCH — Delete `n` characters at the cursor (shift the rest left).
    pub fn delete_chars(&mut self, n: usize) {
        let template = self.cursor.template;
        let col = self.cursor.point.column.0;
        let idx = self.cursor_deque_index();
        let row = &mut self.rows[idx];
        let count = n.min(self.cols - col);
        for i in col..(self.cols - count) {
            row.cells[i] = row.cells[i + count];
        }
        for i in (self.cols - count)..self.cols {
            row.cells[i].reset_with(&template);
        }
    }

    /// ICH — Insert `n` blank cells at the cursor (shift the rest right).
    pub fn insert_blank(&mut self, n: usize) {
        let template = self.cursor.template;
        let col = self.cursor.point.column.0;
        let idx = self.cursor_deque_index();
        let row = &mut self.rows[idx];
        let count = n.min(self.cols - col);
        for i in (col + count..self.cols).rev() {
            row.cells[i] = row.cells[i - count];
        }
        for i in col..(col + count) {
            row.cells[i].reset_with(&template);
        }
    }

    /// IL — Insert `n` blank lines at the cursor line (within the scroll region).
    pub fn insert_lines(&mut self, n: usize) {
        let line = self.cursor.point.line.0 as usize;
        if line < self.scroll_region.start || line >= self.scroll_region.end {
            return;
        }
        // Temporarily shrink the scroll region and scroll down.
        let saved = self.scroll_region.clone();
        self.scroll_region.start = line;
        self.scroll_down_region(n);
        self.scroll_region = saved;
    }

    /// DL — Delete `n` lines starting at the cursor line.
    pub fn delete_lines(&mut self, n: usize) {
        let line = self.cursor.point.line.0 as usize;
        if line < self.scroll_region.start || line >= self.scroll_region.end {
            return;
        }
        let saved = self.scroll_region.clone();
        self.scroll_region.start = line;
        self.scroll_up_region(n);
        self.scroll_region = saved;
    }

    // ---------------------------------------------------------------------
    // Scroll region + save/restore cursor
    // ---------------------------------------------------------------------

    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        let top = top.min(self.screen_lines.saturating_sub(1));
        let bottom = bottom.min(self.screen_lines);
        if top < bottom {
            self.scroll_region = top..bottom;
        } else {
            self.scroll_region = 0..self.screen_lines;
        }
    }

    pub fn reset_scroll_region(&mut self) {
        self.scroll_region = 0..self.screen_lines;
    }

    pub fn scroll_region(&self) -> Range<usize> {
        self.scroll_region.clone()
    }

    pub fn save_cursor(&mut self) {
        self.saved_cursor = self.cursor;
    }

    pub fn restore_cursor(&mut self) {
        self.cursor = self.saved_cursor;
    }

    // ---------------------------------------------------------------------
    // Resize (clip-and-pad, no reflow)
    // ---------------------------------------------------------------------

    pub fn resize(&mut self, cols: usize, lines: usize) {
        // Resize every row to the new width.
        for row in &mut self.rows {
            row.resize(cols);
        }
        self.cols = cols;

        // Adjust the number of screen rows.
        if lines > self.screen_lines {
            let extra = lines - self.screen_lines;
            for _ in 0..extra {
                self.rows.push_back(Row::new(cols));
            }
        } else if lines < self.screen_lines {
            // Shrinking: excess bottom rows are dropped (cursor stays anchored
            // near the top). We just update screen_lines — the deque is large
            // enough, and the extra rows become scrollback or are trimmed.
            let extra = self.screen_lines - lines;
            // Drop `extra` rows from the back if they're empty; otherwise push
            // them into scrollback by leaving them and trimming scrollback.
            // Simplest: leave rows, trim from front if over history limit.
            while self.rows.len() > self.history_limit + lines {
                self.rows.pop_front();
            }
            // Ensure we don't leave a dangling pointer — cursor will be clamped.
            let _ = extra;
        }
        self.screen_lines = lines;

        // Clamp cursor.
        let max_line = (lines - 1) as i32;
        if self.cursor.point.line.0 > max_line {
            self.cursor.point.line.0 = max_line;
        }
        if self.cursor.point.column.0 >= cols {
            self.cursor.point.column.0 = cols - 1;
        }
        self.cursor.input_needs_wrap = false;

        // Reset scroll region.
        self.scroll_region = 0..lines;
        self.display_offset = self.display_offset.min(self.history_size());
    }

    // ---------------------------------------------------------------------
    // SGR state helpers
    // ---------------------------------------------------------------------

    pub fn template_mut(&mut self) -> &mut Cell {
        &mut self.cursor.template
    }

    /// Clear all cells in the grid including scrollback — used by RIS (ESC c).
    pub fn full_reset(&mut self) {
        self.rows.clear();
        for _ in 0..self.screen_lines {
            self.rows.push_back(Row::new(self.cols));
        }
        self.cursor = Cursor::default();
        self.saved_cursor = Cursor::default();
        self.scroll_region = 0..self.screen_lines;
        self.display_offset = 0;
    }
}

impl Index<Line> for Grid {
    type Output = Row;
    fn index(&self, line: Line) -> &Row {
        &self.rows[self.line_to_index(line)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_grid() -> Grid {
        Grid::new(10, 5, 100)
    }

    #[test]
    fn input_writes_cells_and_advances() {
        let mut g = make_grid();
        for ch in "hello".chars() {
            g.input(ch, true);
        }
        let row = &g[Line(0)];
        assert_eq!(row.cells[0].c, 'h');
        assert_eq!(row.cells[4].c, 'o');
        assert_eq!(g.cursor.point.column.0, 5);
    }

    #[test]
    fn line_feed_scrolls_when_at_bottom() {
        let mut g = make_grid();
        // Write 'a' at line 0, then move cursor to the bottom and scroll.
        g.input('a', true);
        g.goto(Line(4), Column(0));
        g.line_feed();
        // After one scroll, the row that was Line(0) becomes Line(-1) in
        // scrollback.
        assert_eq!(g[Line(-1)].cells[0].c, 'a');
    }

    #[test]
    fn scrollback_respects_history_limit() {
        let mut g = Grid::new(5, 2, 3);
        // Position cursor at the bottom so every line_feed scrolls.
        g.goto(Line(1), Column(0));
        for _ in 0..10 {
            g.line_feed();
        }
        assert_eq!(g.history_size(), 3);
        assert_eq!(g.total_lines(), 5);
    }

    #[test]
    fn scroll_display_delta_and_bottom() {
        let mut g = Grid::new(5, 2, 10);
        g.goto(Line(1), Column(0));
        for _ in 0..5 {
            g.line_feed();
        }
        assert_eq!(g.history_size(), 5);
        g.scroll_display(Scroll::Delta(3));
        assert_eq!(g.display_offset(), 3);
        g.scroll_display(Scroll::Bottom);
        assert_eq!(g.display_offset(), 0);
    }

    #[test]
    fn erase_in_display_clears_all() {
        let mut g = make_grid();
        for ch in "abc".chars() {
            g.input(ch, true);
        }
        g.erase_in_display(2);
        assert_eq!(g[Line(0)].cells[0].c, ' ');
    }

    #[test]
    fn line_wrap_defers_to_next_char() {
        let mut g = Grid::new(3, 2, 10);
        g.input('a', true);
        g.input('b', true);
        g.input('c', true);
        // Cursor at col 2, input_needs_wrap = true.
        assert!(g.cursor.input_needs_wrap);
        g.input('d', true);
        // Should have wrapped to row 1.
        assert_eq!(g[Line(1)].cells[0].c, 'd');
        assert!(g[Line(0)].wrapped);
    }

    #[test]
    fn scroll_region_partial_rotate() {
        let mut g = Grid::new(3, 5, 10);
        for l in 0..5 {
            g.goto(Line(l), Column(0));
            g.input((b'a' + l as u8) as char, true);
        }
        g.set_scroll_region(1, 4); // rows 1..4 scroll; rows 0 and 4 fixed
        g.goto(Line(3), Column(0));
        g.line_feed(); // scroll region [1..4) up by 1
        assert_eq!(g[Line(0)].cells[0].c, 'a'); // fixed
        assert_eq!(g[Line(1)].cells[0].c, 'c'); // was row 2
        assert_eq!(g[Line(2)].cells[0].c, 'd'); // was row 3
        assert_eq!(g[Line(4)].cells[0].c, 'e'); // fixed
    }

    #[test]
    fn resize_wider_pads_and_clamps() {
        let mut g = Grid::new(5, 3, 10);
        g.input('x', true);
        g.resize(10, 3);
        assert_eq!(g.columns(), 10);
        assert_eq!(g[Line(0)].cells[0].c, 'x');
        assert_eq!(g[Line(0)].cells[9].c, ' ');
    }

    #[test]
    fn resize_narrower_truncates_cursor() {
        let mut g = Grid::new(10, 3, 10);
        g.goto(Line(2), Column(9));
        g.resize(5, 3);
        assert!(g.cursor.point.column.0 < 5);
    }

    #[test]
    fn save_and_restore_cursor() {
        let mut g = make_grid();
        g.goto(Line(2), Column(3));
        g.save_cursor();
        g.goto(Line(0), Column(0));
        g.restore_cursor();
        assert_eq!(g.cursor.point.line.0, 2);
        assert_eq!(g.cursor.point.column.0, 3);
    }

    #[test]
    fn delete_chars_shifts_left() {
        let mut g = Grid::new(5, 1, 10);
        for ch in "abcde".chars() {
            g.input(ch, true);
        }
        g.goto(Line(0), Column(1));
        g.delete_chars(2); // delete 'b', 'c'
        assert_eq!(g[Line(0)].cells[0].c, 'a');
        assert_eq!(g[Line(0)].cells[1].c, 'd');
        assert_eq!(g[Line(0)].cells[2].c, 'e');
        assert_eq!(g[Line(0)].cells[3].c, ' ');
    }

    #[test]
    fn insert_blank_shifts_right() {
        let mut g = Grid::new(5, 1, 10);
        for ch in "abcde".chars() {
            g.input(ch, true);
        }
        g.goto(Line(0), Column(1));
        g.insert_blank(2);
        assert_eq!(g[Line(0)].cells[0].c, 'a');
        assert_eq!(g[Line(0)].cells[1].c, ' ');
        assert_eq!(g[Line(0)].cells[2].c, ' ');
        assert_eq!(g[Line(0)].cells[3].c, 'b');
        assert_eq!(g[Line(0)].cells[4].c, 'c');
    }

    #[test]
    fn tab_stops_every_8() {
        let mut g = Grid::new(20, 1, 10);
        g.tab(1);
        assert_eq!(g.cursor.point.column.0, 8);
        g.tab(1);
        assert_eq!(g.cursor.point.column.0, 16);
    }

    #[test]
    fn sgr_template_applied_to_new_cells() {
        let mut g = make_grid();
        g.template_mut().flags.insert(Flags::BOLD);
        g.input('x', true);
        assert!(g[Line(0)].cells[0].flags.contains(Flags::BOLD));
    }
}

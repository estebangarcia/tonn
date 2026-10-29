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
//! When the cursor advances past the bottom of a scroll region whose top is
//! the top of the screen, we insert a blank row directly below the region and
//! (if over the history limit) drop the oldest row from the front. The
//! "screen top" index slides forward, so the region's top row becomes
//! scrollback while rows below the region keep their screen positions. For a
//! full-screen region the insert point is the back of the deque — no rows
//! move. Regions that don't start at the top are rotated in place and never
//! touch scrollback.

use std::collections::VecDeque;
use std::ops::{Index, IndexMut, Range};

use crate::cell::Cell;
#[cfg(test)]
use crate::cell::Flags;
use crate::index::{Column, Line, Point};

/// Extra history rows kept past `history_limit` while the user is scrolled
/// back, so streaming output can't evict the lines being read — the view
/// stays frozen. Bounds memory on a firehose (~16 B/cell: ≈160 MB at 200
/// columns); past it the oldest rows go and the view starts moving again.
/// Trimmed back to the limit once the view returns to the bottom.
const MAX_HELD_HISTORY_ROWS: usize = 50_000;

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

    /// Number of cells up to and including the last non-default cell.
    /// Returns 0 if the row is entirely default cells.
    pub fn occupied_len(&self) -> usize {
        self.cells.iter().rposition(|c| !c.is_default()).map_or(0, |i| i + 1)
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
    /// Rows ever scrolled off the top of the screen (wrapping counter). Lets
    /// `Term` keep a selection on its content as output streams.
    rows_scrolled_off: usize,
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
            rows_scrolled_off: 0,
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

    /// Rows ever scrolled off the top of the screen (wrapping). Compare two
    /// readings with `wrapping_sub` to learn how far content moved up.
    pub fn rows_scrolled_off(&self) -> usize {
        self.rows_scrolled_off
    }

    /// Row at `line` counted from the top of the *live* screen, regardless of
    /// `display_offset` (negative = scrollback). Selections live in this
    /// space; `grid[line]` instead addresses the displayed viewport.
    pub fn live_row(&self, line: Line) -> &Row {
        &self.rows[self.index_from_top(line, 0)]
    }

    // ---------------------------------------------------------------------
    // Indexing
    // ---------------------------------------------------------------------

    /// Translate a screen-relative `Line` into a deque index.
    ///
    /// `Line(0)` is the top of the *visible* screen (honouring display_offset).
    /// Negative values go into scrollback.
    fn line_to_index(&self, line: Line) -> usize {
        self.index_from_top(line, self.display_offset)
    }

    /// Deque index of `line`, counted from a screen top `offset` rows back
    /// into history. Clamped to the stored rows.
    fn index_from_top(&self, line: Line, offset: usize) -> usize {
        let screen_top = self.rows.len() - self.screen_lines - offset;
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
        if self.display_offset == 0 {
            // Back on the live screen: release any history held while frozen.
            self.trim_history();
        }
    }

    /// Scroll the viewport so live `line` is visible, centring it if it isn't
    /// already on screen.
    pub fn reveal_line(&mut self, line: Line) {
        let viewport_row = line.0 + self.display_offset as i32;
        if (0..self.screen_lines as i32).contains(&viewport_row) {
            return;
        }
        let centred = (self.screen_lines as i32 / 2 - line.0).clamp(0, self.history_size() as i32);
        self.scroll_display(Scroll::Delta(centred - self.display_offset as i32));
    }

    /// History rows to retain: `history_limit`, plus a held allowance while
    /// scrolled back (see `MAX_HELD_HISTORY_ROWS`).
    fn retained_history(&self) -> usize {
        if self.display_offset > 0 {
            self.history_limit + MAX_HELD_HISTORY_ROWS
        } else {
            self.history_limit
        }
    }

    /// Drop the oldest rows beyond `retained_history()`.
    fn trim_history(&mut self) {
        while self.rows.len() > self.retained_history() + self.screen_lines {
            self.rows.pop_front();
        }
        self.display_offset = self.display_offset.min(self.history_size());
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
        let line = self.cursor.point.line.0;
        if line == bottom {
            self.scroll_up_region(1);
        } else if line < self.screen_lines as i32 - 1 {
            // Below the scroll region the cursor still stops at the last row.
            self.cursor.point.line.0 += 1;
        }
        self.cursor.input_needs_wrap = false;
    }

    /// Reverse index: move cursor up one row, scrolling the region downwards
    /// if already at the top.
    pub fn reverse_index(&mut self) {
        let top = self.scroll_region.start as i32;
        let line = self.cursor.point.line.0;
        if line == top {
            self.scroll_down_region(1);
        } else if line > 0 {
            // Above the scroll region the cursor still stops at the first row.
            self.cursor.point.line.0 -= 1;
        }
        self.cursor.input_needs_wrap = false;
    }

    // ---------------------------------------------------------------------
    // Scrolling (DECSTBM scroll region)
    // ---------------------------------------------------------------------

    /// Scroll the region up by `n` rows (lines move towards the top). This is
    /// the LF / IND / SU path.
    ///
    /// When the region's top is the top of the screen, the scrolled-off rows
    /// are saved into scrollback — even if the region's bottom is above the
    /// screen bottom. That matches xterm/alacritty/kitty, and inline TUIs
    /// (e.g. Codex without the alt screen) rely on it to push transcript lines
    /// into history from above a fixed viewport. Other regions are rotated
    /// in place.
    pub fn scroll_up_region(&mut self, n: usize) {
        if self.scroll_region.start == 0 {
            self.scroll_into_history(n);
        } else {
            self.rotate_region_up(n);
        }
    }

    /// Move the top `n` rows of a top-anchored scroll region into scrollback.
    ///
    /// Inserting `n` blank rows directly below the region shifts the screen
    /// top forward by `n`: the region's top rows fall into history, the rest
    /// of the region moves up, and rows below the region stay put.
    fn scroll_into_history(&mut self, n: usize) {
        let n = n.min(self.scroll_region.len());
        if n == 0 {
            return;
        }

        let insert_at = self.rows.len() - self.screen_lines + self.scroll_region.end;
        let blank = self.blank_row();
        for _ in 0..n {
            self.rows.insert(insert_at, blank.clone());
        }
        self.rows_scrolled_off = self.rows_scrolled_off.wrapping_add(n);
        // Keep a scrolled-back viewport anchored on the content it shows.
        if self.display_offset > 0 {
            self.display_offset += n;
        }
        self.trim_history();
    }

    /// Rotate the scroll region up by `n` rows in place: rows at
    /// [top..top+n) are dropped, rows at [top+n..bottom) slide up, and the
    /// bottom `n` rows become blank. Never touches scrollback.
    fn rotate_region_up(&mut self, n: usize) {
        let n = n.min(self.scroll_region.len());
        if n == 0 {
            return;
        }

        let template = self.cursor.template;
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

    /// A blank row carrying the cursor's current SGR background.
    fn blank_row(&self) -> Row {
        let mut row = Row::new(self.cols);
        row.clear_with(&self.cursor.template);
        row
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
        // Rotate only: deleted lines must never land in scrollback, even
        // when the cursor is on the top line.
        let saved = self.scroll_region.clone();
        self.scroll_region.start = line;
        self.rotate_region_up(n);
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
        self.set_cursor(self.saved_cursor);
    }

    /// Install a previously saved cursor (DECRC, leaving the alt screen),
    /// clamped to the current screen: the grid may have been resized since it
    /// was saved, and an off-screen cursor panics on the next cell write.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.cursor = cursor;
        self.cursor.point = self.clamp_to_screen(cursor.point);
    }

    /// Clamp a point onto the visible screen.
    fn clamp_to_screen(&self, point: Point) -> Point {
        Point::new(
            Line(point.line.0.clamp(0, self.screen_lines as i32 - 1)),
            Column(point.column.0.min(self.cols - 1)),
        )
    }

    // ---------------------------------------------------------------------
    // Reflow: re-wrap lines when column count changes
    // ---------------------------------------------------------------------

    /// Re-wrap all content (screen + scrollback) from the current column
    /// width to `new_cols`. Wrapped physical rows are merged into logical
    /// lines, then re-broken at the new width. Cursor position is tracked
    /// through the transformation.
    pub fn reflow(&mut self, new_cols: usize) {
        if new_cols == self.cols || self.cols == 0 || new_cols == 0 {
            return;
        }

        // Positions carried through the re-wrap as (deque row, column): the
        // cursor, the saved cursor, and the first visible row — so a
        // scrolled-back view stays on the same content after a resize.
        const CURSOR: usize = 0;
        const SAVED_CURSOR: usize = 1;
        const VIEWPORT_TOP: usize = 2;
        let screen_top = self.rows.len().saturating_sub(self.screen_lines);
        let deque_row = |point: Point| (screen_top as i32 + point.line.0).max(0) as usize;
        let anchors = [
            (deque_row(self.cursor.point), self.cursor.point.column.0),
            (deque_row(self.saved_cursor.point), self.saved_cursor.point.column.0),
            (screen_top.saturating_sub(self.display_offset), 0),
        ];

        // Phase 1: decompose into logical lines, recording each anchor as
        // (logical line index, cell offset within that line).
        let mut logical_lines: Vec<Vec<Cell>> = Vec::new();
        let mut anchor_offsets: [Option<(usize, usize)>; 3] = [None; 3];
        let mut cur_cells: Vec<Cell> = Vec::new();

        for (deque_idx, row) in self.rows.iter().enumerate() {
            let real_len = row.occupied_len();
            for (&(anchor_row, anchor_col), slot) in anchors.iter().zip(&mut anchor_offsets) {
                if anchor_row == deque_idx {
                    let clamped = if real_len == 0 { 0 } else { anchor_col.min(real_len - 1) };
                    *slot = Some((logical_lines.len(), cur_cells.len() + clamped));
                }
            }

            cur_cells.extend_from_slice(&row.cells[..real_len]);

            if !row.wrapped {
                logical_lines.push(std::mem::take(&mut cur_cells));
            }
        }
        // Handle trailing wrapped row (shouldn't happen normally).
        let anchored_on_trailing = anchor_offsets.iter().flatten().any(|&(line, _)| line == logical_lines.len());
        if !cur_cells.is_empty() || anchored_on_trailing {
            logical_lines.push(std::mem::take(&mut cur_cells));
        }

        // Phase 2: re-break each logical line at new_cols and rebuild.
        let mut new_rows: VecDeque<Row> = VecDeque::new();
        let mut line_starts: Vec<usize> = Vec::with_capacity(logical_lines.len());

        for cells in &logical_lines {
            line_starts.push(new_rows.len());

            if cells.is_empty() {
                new_rows.push_back(Row::new(new_cols));
                continue;
            }

            let num_chunks = cells.len().div_ceil(new_cols);
            for (i, chunk) in cells.chunks(new_cols).enumerate() {
                let mut row = Row::new(new_cols);
                row.cells[..chunk.len()].copy_from_slice(chunk);
                row.wrapped = i + 1 < num_chunks;
                new_rows.push_back(row);
            }
        }

        // Map each anchor to its (new deque row, new column).
        let new_anchors = anchor_offsets.map(|anchor| {
            anchor.map(|(line, off)| {
                let base = line_starts[line];
                let len = logical_lines[line].len();
                if len == 0 {
                    (base, off.min(new_cols.saturating_sub(1)))
                } else {
                    let off = off.min(len - 1);
                    (base + off / new_cols, off % new_cols)
                }
            })
        });

        // Ensure at least screen_lines rows exist.
        while new_rows.len() < self.screen_lines {
            new_rows.push_back(Row::new(new_cols));
        }

        // Trim oldest scrollback if over the retained history.
        let retained = self.retained_history();
        let mut trimmed: usize = 0;
        while new_rows.len() > retained + self.screen_lines {
            new_rows.pop_front();
            trimmed += 1;
        }

        // Phase 3: apply.
        self.rows = new_rows;
        self.cols = new_cols;

        let new_screen_top = self.rows.len().saturating_sub(self.screen_lines);
        let screen_line = |deque_idx: usize| {
            let line = deque_idx.saturating_sub(trimmed) as i32 - new_screen_top as i32;
            Line(line.clamp(0, self.screen_lines as i32 - 1))
        };

        // Map cursor back to screen-relative coordinates.
        if let Some((di, col)) = new_anchors[CURSOR] {
            self.cursor.point = Point::new(screen_line(di), Column(col));
        }
        self.cursor.input_needs_wrap = false;

        if let Some((di, col)) = new_anchors[SAVED_CURSOR] {
            self.saved_cursor.point = Point::new(screen_line(di), Column(col));
        }

        // Keep a scrolled-back viewport's first row on the same content.
        if self.display_offset > 0
            && let Some((di, _)) = new_anchors[VIEWPORT_TOP]
        {
            self.display_offset = new_screen_top.saturating_sub(di.saturating_sub(trimmed));
        }
        self.display_offset = self.display_offset.min(self.history_size());
    }

    // ---------------------------------------------------------------------
    // Resize
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
            // Shrinking: prefer removing empty rows BELOW the cursor first
            // so the cursor (and the content around it) stays visible.
            // Whatever deficit remains is pushed from the top into scrollback.
            let rows_to_remove = self.screen_lines - lines;
            let rows_below_cursor =
                (self.screen_lines as i32 - 1 - self.cursor.point.line.0).max(0) as usize;
            let removed_from_bottom = rows_to_remove.min(rows_below_cursor);
            let pushed_to_scrollback = rows_to_remove - removed_from_bottom;

            // Pop the bottom rows (below cursor, typically blank after a zoom).
            for _ in 0..removed_from_bottom {
                self.rows.pop_back();
            }

            // The remaining excess comes from the top → becomes scrollback.
            // Adjust cursor so it stays on the same content, and keep a
            // scrolled-back viewport anchored on what it was showing.
            self.cursor.point.line.0 -= pushed_to_scrollback as i32;
            if self.display_offset > 0 {
                self.display_offset += pushed_to_scrollback;
            }

            // Trim oldest scrollback if over the history limit.
            while self.rows.len() > self.retained_history() + lines {
                self.rows.pop_front();
            }
        }
        self.screen_lines = lines;

        // Clamp cursor to the visible screen.
        self.cursor.point = self.clamp_to_screen(self.cursor.point);
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
        assert_eq!(g.history_size(), 0, "non-top region must not feed history");
    }

    /// Write `chars[i]` into column 0 of screen row `i`.
    fn fill_rows(g: &mut Grid, chars: &str) {
        for (l, ch) in chars.chars().enumerate() {
            g.goto(Line(l as i32), Column(0));
            g.input(ch, true);
        }
    }

    fn first_char(g: &Grid, line: i32) -> char {
        g[Line(line)].cells[0].c
    }

    #[test]
    fn top_anchored_region_feeds_history() {
        // Codex inline mode: region covers rows above a fixed viewport
        // (rows 4..6 here); newlines at the region bottom push transcript
        // lines into scrollback.
        let mut g = Grid::new(3, 6, 100);
        fill_rows(&mut g, "abcdef");
        g.set_scroll_region(0, 4);
        g.goto(Line(3), Column(0));
        g.line_feed();
        g.line_feed();

        assert_eq!(g.history_size(), 2);
        assert_eq!(first_char(&g, -2), 'a');
        assert_eq!(first_char(&g, -1), 'b');
        assert_eq!(first_char(&g, 0), 'c');
        assert_eq!(first_char(&g, 1), 'd');
        assert_eq!(first_char(&g, 2), ' ');
        assert_eq!(first_char(&g, 3), ' ');
        // Rows below the region are untouched.
        assert_eq!(first_char(&g, 4), 'e');
        assert_eq!(first_char(&g, 5), 'f');
    }

    #[test]
    fn delete_lines_at_top_does_not_feed_history() {
        let mut g = Grid::new(3, 4, 100);
        fill_rows(&mut g, "abcd");
        g.goto(Line(0), Column(0));
        g.delete_lines(1);
        assert_eq!(g.history_size(), 0);
        assert_eq!(first_char(&g, 0), 'b');
        assert_eq!(first_char(&g, 3), ' ');
    }

    #[test]
    fn top_anchored_region_without_history_drops_lines() {
        // The alt grid has no history: lines scrolled off the top vanish
        // but the region still scrolls correctly.
        let mut g = Grid::new(3, 4, 0);
        fill_rows(&mut g, "abcd");
        g.set_scroll_region(0, 3);
        g.goto(Line(2), Column(0));
        g.line_feed();
        assert_eq!(g.history_size(), 0);
        assert_eq!(first_char(&g, 0), 'b');
        assert_eq!(first_char(&g, 1), 'c');
        assert_eq!(first_char(&g, 2), ' ');
        assert_eq!(first_char(&g, 3), 'd');
    }

    #[test]
    fn scrolled_back_viewport_stays_anchored() {
        let mut g = Grid::new(3, 2, 100);
        g.goto(Line(1), Column(0));
        for ch in "abcde".chars() {
            g.carriage_return();
            g.input(ch, true);
            g.line_feed();
        }
        g.scroll_display(Scroll::Delta(2));
        let anchored = first_char(&g, 0);

        for ch in "xyz".chars() {
            g.carriage_return();
            g.input(ch, true);
            g.line_feed();
        }
        assert_eq!(first_char(&g, 0), anchored);
        assert_eq!(g.display_offset(), 5);
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

    // --- Reflow tests ---

    #[test]
    fn reflow_shrink_splits_long_line() {
        // Use a 1-row screen + scrollback so both halves are accessible.
        let mut g = Grid::new(10, 1, 100);
        for ch in "abcdefghij".chars() {
            g.input(ch, true);
        }
        g.reflow(5);
        assert_eq!(g.columns(), 5);
        // After reflow: "abcde" (wrapped) in scrollback, "fghij" on screen.
        // Cursor was at col 9 → maps to row 1 col 4 → Line(0) = "fghij".
        let text0: String = g[Line(0)].cells.iter().take(5).map(|c| c.c).collect();
        assert_eq!(text0, "fghij");
        assert!(!g[Line(0)].wrapped);
        // Scrollback row has "abcde" (wrapped).
        let text_sb: String = g[Line(-1)].cells.iter().take(5).map(|c| c.c).collect();
        assert_eq!(text_sb, "abcde");
        assert!(g[Line(-1)].wrapped);
    }

    #[test]
    fn reflow_grow_merges_wrapped_lines() {
        let mut g = Grid::new(5, 3, 100);
        // Write 10 chars → wraps at col 5
        for ch in "abcdefghij".chars() {
            g.input(ch, true);
        }
        assert!(g[Line(0)].wrapped);
        g.reflow(10);
        let text: String = g[Line(0)].cells.iter().take(10).map(|c| c.c).collect();
        assert_eq!(text, "abcdefghij");
        assert!(!g[Line(0)].wrapped);
    }

    #[test]
    fn reflow_cursor_tracking_shrink() {
        // 10-col, 2-row screen so both halves fit.
        let mut g = Grid::new(10, 2, 100);
        for ch in "abcdefghij".chars() {
            g.input(ch, true);
        }
        g.goto(Line(0), Column(7));
        g.reflow(5);
        // Offset 7 in a 10-char line → row 1 col 2 in 5-col grid.
        // But screen is 2 rows: "abcde" goes to scrollback, "fghij" is Line(0),
        // and Line(1) is blank. Cursor offset 7 = row 1 in the logical → Line(0) col 2.
        assert_eq!(g.cursor.point.line.0, 0);
        assert_eq!(g.cursor.point.column.0, 2);
    }

    #[test]
    fn reflow_cursor_tracking_grow() {
        let mut g = Grid::new(5, 3, 100);
        for ch in "abcdefghij".chars() {
            g.input(ch, true);
        }
        // Cursor after writing is on row 1. Move to row 1 col 2.
        g.goto(Line(1), Column(2));
        g.reflow(10);
        // Offset = 5 + 2 = 7 → row 0 col 7 in 10-col grid
        assert_eq!(g.cursor.point.line.0, 0);
        assert_eq!(g.cursor.point.column.0, 7);
    }

    #[test]
    fn reflow_preserves_non_wrapped_short_lines() {
        let mut g = Grid::new(10, 3, 100);
        for ch in "abc".chars() {
            g.input(ch, true);
        }
        g.line_feed();
        g.carriage_return();
        for ch in "xyz".chars() {
            g.input(ch, true);
        }
        g.reflow(5);
        // Two short lines should NOT merge — they weren't wrapped.
        let t0: String = g[Line(0)].cells.iter().take(3).map(|c| c.c).collect();
        let t1: String = g[Line(1)].cells.iter().take(3).map(|c| c.c).collect();
        assert_eq!(t0, "abc");
        assert_eq!(t1, "xyz");
        assert!(!g[Line(0)].wrapped);
    }

    #[test]
    fn reflow_preserves_blank_lines() {
        let mut g = Grid::new(10, 5, 100);
        for ch in "hello".chars() {
            g.input(ch, true);
        }
        g.line_feed();
        g.carriage_return();
        // blank line
        g.line_feed();
        g.carriage_return();
        for ch in "world".chars() {
            g.input(ch, true);
        }
        g.reflow(5);
        let t0: String = g[Line(0)].cells.iter().take(5).map(|c| c.c).collect();
        assert_eq!(t0, "hello");
        assert_eq!(g[Line(1)].occupied_len(), 0); // blank line preserved
        let t2: String = g[Line(2)].cells.iter().take(5).map(|c| c.c).collect();
        assert_eq!(t2, "world");
    }

    #[test]
    fn reflow_same_cols_is_noop() {
        let mut g = Grid::new(10, 3, 100);
        for ch in "hello".chars() {
            g.input(ch, true);
        }
        g.reflow(10);
        let text: String = g[Line(0)].cells.iter().take(5).map(|c| c.c).collect();
        assert_eq!(text, "hello");
    }

    /// Feed `count` numbered single-line outputs ("0", "1", …) at the bottom.
    fn stream_lines(g: &mut Grid, from: usize, count: usize) {
        for i in from..from + count {
            g.carriage_return();
            for ch in i.to_string().chars() {
                g.input(ch, true);
            }
            g.line_feed();
        }
    }

    fn visible_text(g: &Grid) -> Vec<String> {
        (0..g.screen_lines() as i32)
            .map(|l| g[Line(l)].cells.iter().map(|c| c.c).collect::<String>())
            .collect()
    }

    #[test]
    fn scrolled_back_view_stays_frozen_past_history_limit() {
        let limit = 10;
        let mut g = Grid::new(6, 3, limit);
        g.goto(Line(2), Column(0));
        stream_lines(&mut g, 0, 20);
        g.scroll_display(Scroll::Delta(5));
        let frozen = visible_text(&g);

        // Far more output than the history limit: the view must not move.
        stream_lines(&mut g, 20, limit * 5);
        assert_eq!(visible_text(&g), frozen);
        assert!(g.history_size() > limit, "history held past the limit");

        // Returning to the bottom releases the held history.
        g.scroll_display(Scroll::Bottom);
        assert_eq!(g.history_size(), limit);
    }

    #[test]
    fn history_hold_is_bounded() {
        let limit = 10;
        let mut g = Grid::new(6, 3, limit);
        g.goto(Line(2), Column(0));
        stream_lines(&mut g, 0, 20);
        g.scroll_display(Scroll::Delta(1));

        stream_lines(&mut g, 20, MAX_HELD_HISTORY_ROWS + limit * 2);
        assert_eq!(g.history_size(), limit + MAX_HELD_HISTORY_ROWS);
    }

    #[test]
    fn history_at_bottom_respects_limit() {
        let limit = 10;
        let mut g = Grid::new(6, 3, limit);
        g.goto(Line(2), Column(0));
        stream_lines(&mut g, 0, limit * 5);
        assert_eq!(g.history_size(), limit);
    }

    /// Grid of `cols` × `screen` whose history holds one 8-char line per
    /// letter (`aaaaaaaa`, `bbbbbbbb`, …), with the cursor on the last row.
    fn grid_with_letter_lines(cols: usize, screen: usize, letters: &str) -> Grid {
        let mut g = Grid::new(cols, screen, 100);
        g.goto(Line(screen as i32 - 1), Column(0));
        for letter in letters.chars() {
            g.carriage_return();
            for _ in 0..8 {
                g.input(letter, true);
            }
            g.line_feed();
        }
        g
    }

    #[test]
    fn reflow_keeps_scrolled_back_view_on_same_content() {
        // Narrowing re-wraps every line into two rows; without anchoring the
        // same display_offset would point at different content.
        let mut g = grid_with_letter_lines(10, 3, "abcdefgh");
        g.scroll_display(Scroll::Delta(5));
        let top = first_char(&g, 0);

        g.reflow(4);
        assert!(g.display_offset() > 0);
        assert_eq!(first_char(&g, 0), top);
        assert!(!g[Line(-1)].wrapped, "top row starts its logical line");
    }

    #[test]
    fn reflow_at_bottom_stays_at_bottom() {
        let mut g = grid_with_letter_lines(10, 3, "abcdefgh");
        g.reflow(4);
        assert_eq!(g.display_offset(), 0);
    }

    #[test]
    fn shrinking_rows_keeps_scrolled_back_view_on_same_content() {
        let mut g = grid_with_letter_lines(10, 6, "abcdefghij");
        g.scroll_display(Scroll::Delta(2));
        let top = first_char(&g, 0);

        g.resize(10, 3);
        assert_eq!(first_char(&g, 0), top);
    }

    #[test]
    fn reflow_scrollback_rows() {
        let mut g = Grid::new(10, 2, 100);
        g.goto(Line(1), Column(0));
        // Push some content into scrollback
        for ch in "abcdefghij".chars() {
            g.input(ch, true);
        }
        g.line_feed();
        g.carriage_return();
        for ch in "klmnopqrst".chars() {
            g.input(ch, true);
        }
        g.line_feed(); // scrolls: "abcdefghij" goes to scrollback
        // Now reflow scrollback from 10 to 5
        g.reflow(5);
        // Scrollback should have "abcde" (wrapped) + "fghij" (not wrapped)
        assert!(g.history_size() >= 2);
    }
}

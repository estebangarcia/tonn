//! Text search over the scrollback and screen.
//!
//! Pure logic over a `Grid`. Matches are in *live* coordinates, like
//! selections — `Line(0)` is the top of the live screen and negative lines are
//! history — so they stay valid while the viewport scrolls. Soft-wrapped rows
//! are joined into logical lines, so a match can span a wrap. Smart case: the
//! search ignores case unless the query contains an uppercase letter. Every
//! occurrence counts, overlapping ones included (`aa` in `aaaa` is 3), which
//! also makes a windowed scan (just the viewport) agree exactly with a full one.
//!
//! Matching is linear (KMP) so repetitive output plus a near-matching query
//! can't stall the UI; highlighting scans only the viewport plus the rows a
//! match could spill over from.

use std::ops::RangeInclusive;

use crate::grid::Grid;
use crate::index::{Column, Line, Point};
use crate::selection::SelectionRange;

/// One match, inclusive at both ends, in live coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: Point,
    pub end: Point,
}

impl SearchMatch {
    /// The match as a selection range, e.g. to map it into the viewport.
    pub fn range(&self) -> SelectionRange {
        SelectionRange { start: self.start, end: self.end, is_block: false }
    }
}

/// Which way `SearchState::step` moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    /// Up, towards older output.
    Older,
    /// Down, towards newer output.
    Newer,
}

/// An open search on one terminal: the query and the match being shown.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    /// The match navigated to, kept on its text as output scrolls.
    pub current: Option<SearchMatch>,
    /// 0-based position of `current` among all matches and the match count,
    /// as of the last query change or step (not live while output streams).
    pub index: Option<usize>,
    pub total: usize,
}

impl SearchState {
    /// Replace the query and jump to the nearest match at or above the
    /// bottom of the viewport (the newest one in view, or above it).
    pub fn set_query(&mut self, grid: &mut Grid, query: String) {
        self.query = query;
        let matches = all_matches(grid, &self.query);
        let view_bottom = grid.screen_lines() as i32 - 1 - grid.display_offset() as i32;
        let nearest = matches.iter().rposition(|m| m.start.line.0 <= view_bottom);
        self.select(grid, &matches, nearest.or(matches.len().checked_sub(1)));
    }

    /// Move to the next match in `direction`, wrapping around at either end.
    /// Matches are recomputed, so output that arrived since is included.
    pub fn step(&mut self, grid: &mut Grid, direction: SearchDirection) {
        let matches = all_matches(grid, &self.query);
        let newest = matches.len().checked_sub(1);
        let next = match (self.current, direction) {
            (None, _) => newest,
            (Some(current), SearchDirection::Older) => {
                matches.iter().rposition(|m| m.start < current.start).or(newest)
            }
            (Some(current), SearchDirection::Newer) => matches
                .iter()
                .position(|m| m.start > current.start)
                .or((!matches.is_empty()).then_some(0)),
        };
        self.select(grid, &matches, next);
    }

    /// Matches overlapping the viewport, for highlighting.
    pub fn visible_matches(&self, grid: &Grid) -> Vec<SearchMatch> {
        let top = -(grid.display_offset() as i32);
        find_matches(grid, &self.query, top..=top + grid.screen_lines() as i32 - 1)
    }

    /// Status text for the search bar: `3/17`, `No matches`, or empty.
    pub fn status(&self) -> String {
        match (self.query.is_empty(), self.index) {
            (true, _) => String::new(),
            (false, Some(index)) => format!("{}/{}", index + 1, self.total),
            (false, None) => "No matches".to_string(),
        }
    }

    /// Forget the current match (its position no longer means anything,
    /// e.g. after a resize re-wrapped the text). The query stays.
    pub fn clear_current(&mut self) {
        self.current = None;
        self.index = None;
    }

    /// Keep `current` on its text after output scrolled `lines` rows off the
    /// top; drop it once it falls out of history.
    pub(crate) fn rotate(&mut self, lines: i32, oldest_line: i32) {
        if let Some(current) = &mut self.current {
            current.start.line.0 -= lines;
            current.end.line.0 -= lines;
            if current.end.line.0 < oldest_line {
                self.clear_current();
            }
        }
    }

    fn select(&mut self, grid: &mut Grid, matches: &[SearchMatch], index: Option<usize>) {
        self.total = matches.len();
        self.index = index;
        self.current = index.map(|i| matches[i]);
        if let Some(current) = self.current {
            grid.reveal_line(current.start.line);
        }
    }
}

fn all_matches(grid: &Grid, query: &str) -> Vec<SearchMatch> {
    find_matches(grid, query, i32::MIN..=i32::MAX)
}

/// Every match overlapping `lines` (live coordinates, clamped to the stored
/// rows), top to bottom.
pub fn find_matches(grid: &Grid, query: &str, lines: RangeInclusive<i32>) -> Vec<SearchMatch> {
    let case_sensitive = query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, case_sensitive)).collect();
    let oldest = -(grid.history_size() as i32);
    let newest = grid.screen_lines() as i32 - 1;
    let first = (*lines.start()).max(oldest);
    let last = (*lines.end()).min(newest);
    if needle.is_empty() || first > last {
        return Vec::new();
    }

    // A match overlapping [first, last] starts at most `reach` rows above
    // `first` and ends at most `reach` rows below `last`, so only that window
    // is scanned — even inside a huge soft-wrapped line.
    let columns = grid.columns();
    let reach = (needle.len() - 1).div_ceil(columns).min(i32::MAX as usize) as i32;
    let scan_last = last.saturating_add(reach).min(newest);
    let mut line = first.saturating_sub(reach).max(oldest);

    let mut matches = Vec::new();
    let mut haystack: Vec<char> = Vec::new();
    while line <= scan_last {
        // One run of soft-wrapped rows (the part of it inside the window).
        let run_start = line;
        haystack.clear();
        loop {
            let row = grid.live_row(Line(line));
            haystack.extend(row.cells.iter().map(|cell| fold(cell.c, case_sensitive)));
            line += 1;
            if !row.wrapped || line > scan_last {
                break;
            }
        }

        let to_point = |offset: usize| {
            Point::new(Line(run_start + (offset / columns) as i32), Column(offset % columns))
        };
        for start in match_starts(&haystack, &needle) {
            let found = SearchMatch { start: to_point(start), end: to_point(start + needle.len() - 1) };
            if found.end.line.0 >= first && found.start.line.0 <= last {
                matches.push(found);
            }
        }
    }
    matches
}

/// Start offsets of every occurrence of `needle` (non-empty) in `haystack`,
/// overlapping ones included, in O(n + m) — Knuth–Morris–Pratt. A naive scan
/// is O(n·m), which a long repetitive line plus a near-matching query turns
/// into a UI stall.
fn match_starts(haystack: &[char], needle: &[char]) -> Vec<usize> {
    // fallback[i]: length of the longest proper prefix of needle[..=i] that is
    // also a suffix of it — where to resume after a mismatch.
    let mut fallback = vec![0; needle.len()];
    let mut k = 0;
    for i in 1..needle.len() {
        while k > 0 && needle[i] != needle[k] {
            k = fallback[k - 1];
        }
        if needle[i] == needle[k] {
            k += 1;
        }
        fallback[i] = k;
    }

    let mut starts = Vec::new();
    let mut matched = 0;
    for (i, &c) in haystack.iter().enumerate() {
        while matched > 0 && c != needle[matched] {
            matched = fallback[matched - 1];
        }
        if c == needle[matched] {
            matched += 1;
        }
        if matched == needle.len() {
            starts.push(i + 1 - needle.len());
            matched = fallback[matched - 1];
        }
    }
    starts
}

fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive { c } else { c.to_lowercase().next().unwrap_or(c) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10-column, 3-row grid with history, holding `lines` (one per row,
    /// oldest first) with the last line at the bottom of the screen.
    fn grid_with(lines: &[&str]) -> Grid {
        let mut g = Grid::new(10, 3, 100);
        for (i, text) in lines.iter().enumerate() {
            if i > 0 {
                g.carriage_return();
                g.line_feed();
            }
            for c in text.chars() {
                g.input(c, true);
            }
        }
        g
    }

    fn starts(matches: &[SearchMatch]) -> Vec<(i32, usize)> {
        matches.iter().map(|m| (m.start.line.0, m.start.column.0)).collect()
    }

    #[test]
    fn finds_matches_in_history_and_screen() {
        // 5 lines on a 3-row screen: lines 0–1 are in history (-2, -1).
        let g = grid_with(&["error one", "ok", "error two", "ok", "error"]);
        let all = all_matches(&g, "error");
        assert_eq!(starts(&all), [(-2, 0), (0, 0), (2, 0)]);
        assert_eq!(all[0].end, Point::new(Line(-2), Column(4)));
    }

    #[test]
    fn smart_case() {
        let g = grid_with(&["Error", "error"]);
        assert_eq!(all_matches(&g, "error").len(), 2, "lowercase query ignores case");
        assert_eq!(all_matches(&g, "Error").len(), 1, "uppercase makes it case-sensitive");
    }

    #[test]
    fn matches_across_a_soft_wrap() {
        // "abcdefghijklm" wraps after 10 columns; "jklm" spans the wrap.
        let g = grid_with(&["abcdefghijklm"]);
        let found = all_matches(&g, "jklm");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, Point::new(Line(0), Column(9)));
        assert_eq!(found[0].end, Point::new(Line(1), Column(2)));
    }

    #[test]
    fn visible_matches_only_cover_the_viewport() {
        let mut g = grid_with(&["hit", "x", "hit", "x", "hit"]);
        let search = SearchState { query: "hit".into(), ..Default::default() };
        assert_eq!(starts(&search.visible_matches(&g)), [(0, 0), (2, 0)]);
        g.scroll_display(crate::grid::Scroll::Top);
        assert_eq!(starts(&search.visible_matches(&g)), [(-2, 0), (0, 0)]);
    }

    #[test]
    fn typing_jumps_to_the_newest_match_and_steps_wrap() {
        let mut g = grid_with(&["hit", "x", "hit", "x", "hit"]);
        let mut search = SearchState::default();
        search.set_query(&mut g, "hit".into());
        assert_eq!(search.status(), "3/3");

        search.step(&mut g, SearchDirection::Older);
        assert_eq!(search.status(), "2/3");
        search.step(&mut g, SearchDirection::Older);
        search.step(&mut g, SearchDirection::Older);
        assert_eq!(search.status(), "3/3", "wraps to the newest");
        search.step(&mut g, SearchDirection::Newer);
        assert_eq!(search.status(), "1/3", "wraps to the oldest");
    }

    #[test]
    fn stepping_reveals_a_match_in_history() {
        let mut g = grid_with(&["hit", "x", "x", "x", "x", "x", "x"]);
        let mut search = SearchState::default();
        search.set_query(&mut g, "hit".into());
        let current = search.current.unwrap();
        let viewport_row = current.start.line.0 + g.display_offset() as i32;
        assert!((0..3).contains(&viewport_row), "match scrolled into view");
    }

    #[test]
    fn overlapping_occurrences_all_count() {
        let g = grid_with(&["aaaa"]);
        assert_eq!(starts(&all_matches(&g, "aa")), [(0, 0), (0, 1), (0, 2)]);
    }

    #[test]
    fn linear_matcher_agrees_with_a_naive_scan() {
        // Small alphabet → lots of partial matches and overlaps.
        let naive = |hay: &[char], needle: &[char]| -> Vec<usize> {
            (0..=hay.len().saturating_sub(needle.len()))
                .filter(|&i| hay.len() >= needle.len() && hay[i..i + needle.len()] == *needle)
                .collect()
        };
        let mut seed: u32 = 7;
        let mut next = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ['a', 'b'][(seed >> 16) as usize % 2]
        };
        for _ in 0..500 {
            let hay: Vec<char> = (0..40).map(|_| next()).collect();
            let needle: Vec<char> = (0..1 + (next() as usize % 5)).map(|_| next()).collect();
            assert_eq!(match_starts(&hay, &needle), naive(&hay, &needle), "{hay:?} / {needle:?}");
        }
    }

    #[test]
    fn near_matching_query_on_repetitive_text_is_linear() {
        // A naive scan does ~10^11 comparisons here (minutes); KMP ~10^6.
        let hay = vec!['a'; 1_000_000];
        let mut needle = vec!['a'; 100_000];
        needle.push('b');
        let started = std::time::Instant::now();
        assert!(match_starts(&hay, &needle).is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn viewport_matches_agree_with_a_full_scan_inside_a_wrapped_line() {
        // One long soft-wrapped line: the viewport scan starts mid-line, yet
        // must find exactly the full scan's matches that overlap the view.
        let mut g = Grid::new(10, 3, 100);
        for i in 0..300 {
            g.input(if i % 7 == 0 { 'x' } else { 'a' }, true);
        }
        g.scroll_display(crate::grid::Scroll::Delta(12));
        let search = SearchState { query: "xaaaaaax".into(), ..Default::default() };

        let top = -(g.display_offset() as i32);
        let expected: Vec<_> = all_matches(&g, &search.query)
            .into_iter()
            .filter(|m| m.end.line.0 >= top && m.start.line.0 <= top + 2)
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(search.visible_matches(&g), expected);
    }

    #[test]
    fn status_text() {
        let mut g = grid_with(&["abc"]);
        let mut search = SearchState::default();
        assert_eq!(search.status(), "");
        search.set_query(&mut g, "zzz".into());
        assert_eq!(search.status(), "No matches");
    }
}

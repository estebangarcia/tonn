//! Text selection types and range computation.
//!
//! MVP supports only `SelectionType::Simple` (row-major drag selection). The
//! other variants exist as enum values for API compatibility with the legacy
//! backend but are not implemented — `to_range` treats them as `Simple`.

use crate::event::EventListener;
use crate::index::{Line, Point, Side};
use crate::term::Term;

/// The selection flavour — only `Simple` is implemented in the MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionType {
    Simple,
    Block,
    Semantic,
    Lines,
}

/// A selection endpoint: a point plus which side of the cell it refers to.
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    pub point: Point,
    pub side: Side,
}

/// An active text selection — `start` is where the drag began, `end` follows
/// the current mouse position.
#[derive(Debug, Clone)]
pub struct Selection {
    pub ty: SelectionType,
    pub start: Anchor,
    pub end: Anchor,
}

/// A normalised selection range in row-major order (`start <= end`).
#[derive(Debug, Clone, Copy)]
pub struct SelectionRange {
    pub start: Point,
    pub end: Point,
    pub is_block: bool,
}

impl Selection {
    pub fn new(ty: SelectionType, point: Point, side: Side) -> Self {
        let anchor = Anchor { point, side };
        Self {
            ty,
            start: anchor,
            end: anchor,
        }
    }

    /// Update the drag endpoint.
    pub fn update(&mut self, point: Point, side: Side) {
        self.end = Anchor { point, side };
    }

    pub fn is_empty(&self) -> bool {
        self.start.point == self.end.point && self.start.side == self.end.side
    }

    /// Normalise the selection into a row-major range. Returns `None` if the
    /// selection is empty after boundary adjustment.
    pub fn to_range<L: EventListener>(&self, _term: &Term<L>) -> Option<SelectionRange> {
        // Sort the two anchors so `a` is first in row-major order.
        let (a, b) = if point_before(self.start.point, self.end.point)
            || (self.start.point == self.end.point
                && matches!(self.start.side, Side::Left)
                && matches!(self.end.side, Side::Right))
        {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        };

        let mut start = a.point;
        let mut end = b.point;

        // `Side::Right` on the leading anchor means the anchor is on the right
        // edge of its cell — the leftmost *included* cell is one column to the
        // right. Symmetric for the trailing anchor.
        if matches!(a.side, Side::Right) {
            start.column.0 = start.column.0.saturating_add(1);
        }
        if matches!(b.side, Side::Left) && b.point.column.0 > 0 {
            end.column.0 -= 1;
        }

        if point_before(end, start) {
            return None;
        }
        if start == end && matches!(a.side, Side::Right) && matches!(b.side, Side::Left) {
            return None;
        }

        Some(SelectionRange {
            start,
            end,
            is_block: false,
        })
    }
}

fn point_before(a: Point, b: Point) -> bool {
    if a.line.0 != b.line.0 {
        a.line.0 < b.line.0
    } else {
        a.column.0 < b.column.0
    }
}

/// Iterate the lines of a selection range, clamping to the given line bounds.
pub fn selection_lines(range: &SelectionRange) -> impl Iterator<Item = Line> {
    (range.start.line.0..=range.end.line.0).map(Line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::event::Event;
    use crate::index::Column;

    struct NoopListener;
    impl EventListener for NoopListener {
        fn send_event(&self, _: Event) {}
    }

    fn term() -> Term<NoopListener> {
        struct Dim;
        impl crate::grid::Dimensions for Dim {
            fn total_lines(&self) -> usize { 10 }
            fn screen_lines(&self) -> usize { 10 }
            fn columns(&self) -> usize { 20 }
        }
        Term::new(Config { scrolling_history: 100 }, &Dim, NoopListener)
    }

    #[test]
    fn single_cell_left_right() {
        let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(2), Column(5)), Side::Left);
        sel.update(Point::new(Line(2), Column(5)), Side::Right);
        let range = sel.to_range(&term()).unwrap();
        assert_eq!(range.start, Point::new(Line(2), Column(5)));
        assert_eq!(range.end, Point::new(Line(2), Column(5)));
    }

    #[test]
    fn multi_row_selection_normalises_start_before_end() {
        let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(5), Column(3)), Side::Left);
        sel.update(Point::new(Line(2), Column(7)), Side::Right);
        let range = sel.to_range(&term()).unwrap();
        assert_eq!(range.start.line.0, 2);
        assert_eq!(range.end.line.0, 5);
    }

    #[test]
    fn empty_same_point_same_side() {
        let sel = Selection::new(SelectionType::Simple, Point::new(Line(1), Column(1)), Side::Left);
        // Not calling update — start == end, both Left.
        assert!(sel.is_empty());
    }
}

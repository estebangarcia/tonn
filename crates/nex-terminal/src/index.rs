//! Strongly-typed coordinate types.
//!
//! `Line` is a signed row index: `Line(0)` is the top of the visible screen,
//! negative values point into the scrollback buffer. `Column` is an unsigned
//! zero-based column index. `Point` pairs them.

use std::ops::{Add, AddAssign, Sub, SubAssign};

/// A row index. Negative values refer to scrollback (above the visible screen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Line(pub i32);

impl Add<i32> for Line {
    type Output = Line;
    fn add(self, rhs: i32) -> Line { Line(self.0 + rhs) }
}
impl Sub<i32> for Line {
    type Output = Line;
    fn sub(self, rhs: i32) -> Line { Line(self.0 - rhs) }
}
impl AddAssign<i32> for Line {
    fn add_assign(&mut self, rhs: i32) { self.0 += rhs; }
}
impl SubAssign<i32> for Line {
    fn sub_assign(&mut self, rhs: i32) { self.0 -= rhs; }
}
impl Sub for Line {
    type Output = i32;
    fn sub(self, rhs: Line) -> i32 { self.0 - rhs.0 }
}

/// A zero-based column index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Column(pub usize);

impl Add<usize> for Column {
    type Output = Column;
    fn add(self, rhs: usize) -> Column { Column(self.0 + rhs) }
}
impl Sub<usize> for Column {
    type Output = Column;
    fn sub(self, rhs: usize) -> Column { Column(self.0.saturating_sub(rhs)) }
}
impl AddAssign<usize> for Column {
    fn add_assign(&mut self, rhs: usize) { self.0 += rhs; }
}
impl SubAssign<usize> for Column {
    fn sub_assign(&mut self, rhs: usize) { self.0 = self.0.saturating_sub(rhs); }
}

/// A grid coordinate (row + column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Point<L = Line> {
    pub line: L,
    pub column: Column,
}

impl Point<Line> {
    pub fn new(line: Line, column: Column) -> Self {
        Self { line, column }
    }
}

/// Which side of a cell boundary a point refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Direction for motion / search operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_arithmetic() {
        assert_eq!(Line(5) + 3, Line(8));
        assert_eq!(Line(5) - 3, Line(2));
        assert_eq!(Line(5) - Line(2), 3);
        let mut l = Line(1);
        l += 4;
        assert_eq!(l, Line(5));
    }

    #[test]
    fn column_saturating_sub() {
        assert_eq!(Column(2) - 5, Column(0));
    }

    #[test]
    fn point_fields() {
        let p = Point::new(Line(3), Column(7));
        assert_eq!(p.line.0, 3);
        assert_eq!(p.column.0, 7);
    }
}

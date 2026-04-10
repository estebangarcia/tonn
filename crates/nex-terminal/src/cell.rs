//! Cell model and cell-level style flags.

use crate::ansi::{Color, NamedColor};

bitflags::bitflags! {
    /// Cell-level style flags. Matches the shape of the legacy backend so
    /// `cell.flags.contains(Flags::BOLD)` works identically at call sites.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Flags: u16 {
        const BOLD              = 1 << 0;
        const ITALIC            = 1 << 1;
        const INVERSE           = 1 << 2;
        const UNDERLINE         = 1 << 3;
        const HIDDEN            = 1 << 4;
        const STRIKEOUT         = 1 << 5;
        const WRAPLINE          = 1 << 6;
        const WIDE_CHAR         = 1 << 7;
        const WIDE_CHAR_SPACER  = 1 << 8;
        const DIM               = 1 << 9;
    }
}

/// A single character cell.
///
/// Kept deliberately small: `char` (4 bytes) + two `Color` enums + `Flags`
/// bitfield. No `Arc<CellExtra>` escape hatch — the MVP does not support
/// combining characters, hyperlinks, or per-cell underline colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
        }
    }
}

impl Cell {
    /// Reset the cell to a blank with the given template's colours and flags.
    /// Used when clearing a region so the erased cells pick up the current
    /// background colour (important for SGR-set backgrounds).
    pub fn reset_with(&mut self, template: &Cell) {
        self.c = ' ';
        self.fg = template.fg;
        self.bg = template.bg;
        self.flags = template.flags;
    }
}

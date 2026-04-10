//! ANSI colour types.
//!
//! Re-exports `Processor` from the parser module so call sites using the
//! legacy path `ansi::Processor::<ansi::StdSyncHandler>::new()` keep working.

pub use crate::parser::Processor;

/// 24-bit RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Named ANSI colours. The first 16 are the standard palette plus their bright
/// variants. The remaining variants cover semantic roles (foreground, cursor,
/// etc.) and are accepted from SGR sequences even when we never render them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Foreground,
    Background,
    Cursor,
    DimForeground,
    DimBlack,
    DimRed,
    DimGreen,
    DimYellow,
    DimBlue,
    DimMagenta,
    DimCyan,
    DimWhite,
    BrightForeground,
}

impl NamedColor {
    /// Map a 0..=15 palette index to the corresponding named colour.
    pub fn from_palette_index(idx: u8) -> Self {
        match idx {
            0 => NamedColor::Black,
            1 => NamedColor::Red,
            2 => NamedColor::Green,
            3 => NamedColor::Yellow,
            4 => NamedColor::Blue,
            5 => NamedColor::Magenta,
            6 => NamedColor::Cyan,
            7 => NamedColor::White,
            8 => NamedColor::BrightBlack,
            9 => NamedColor::BrightRed,
            10 => NamedColor::BrightGreen,
            11 => NamedColor::BrightYellow,
            12 => NamedColor::BrightBlue,
            13 => NamedColor::BrightMagenta,
            14 => NamedColor::BrightCyan,
            _ => NamedColor::BrightWhite,
        }
    }
}

/// A terminal cell colour — either a named palette entry, a direct RGB triple,
/// or a 0..=255 palette index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Named(NamedColor),
    Spec(Rgb),
    Indexed(u8),
}

impl Default for Color {
    fn default() -> Self {
        Color::Named(NamedColor::Foreground)
    }
}

/// Marker type kept for call-site compatibility with the legacy backend.
/// The `Processor` is generic over this so call sites using `Processor::<StdSyncHandler>::new()`
/// still compile.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdSyncHandler;

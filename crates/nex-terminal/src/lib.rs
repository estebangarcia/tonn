//! Terminal emulation for Tonn, wrapping alacritty_terminal.

pub mod keys;

pub use alacritty_terminal::event::Event as TerminalEvent;
pub use alacritty_terminal::event::EventListener;
pub use alacritty_terminal::grid::{Dimensions, Grid};
pub use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
pub use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
pub use alacritty_terminal::term::cell::{Cell, Flags as CellFlags};
pub use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
pub use alacritty_terminal::vte::ansi;
pub use ansi::StdSyncHandler;

use nex_common::PaneId;
use std::sync::mpsc;

/// Callback for terminal events that need to reach the main event loop.
pub type EventCallback = Box<dyn Fn(TerminalEvent) + Send + Sync>;

/// Event listener that forwards terminal events.
pub struct NexEventListener {
    pub pane_id: PaneId,
    pub pty_write_tx: mpsc::Sender<String>,
    pub event_callback: EventCallback,
}

impl NexEventListener {
    pub fn new(
        pane_id: PaneId,
        pty_write_tx: mpsc::Sender<String>,
        event_callback: EventCallback,
    ) -> Self {
        Self { pane_id, pty_write_tx, event_callback }
    }
}

impl EventListener for NexEventListener {
    fn send_event(&self, event: TerminalEvent) {
        match &event {
            TerminalEvent::PtyWrite(text) => {
                let _ = self.pty_write_tx.send(text.clone());
            }
            TerminalEvent::Title(_)
            | TerminalEvent::ResetTitle
            | TerminalEvent::Bell => {
                (self.event_callback)(event);
            }
            _ => {
                tracing::trace!(?event, pane_id = %self.pane_id, "terminal event");
            }
        }
    }
}

/// Simple Dimensions implementation for creating a Term.
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl TermSize {
    pub fn new(columns: usize, screen_lines: usize) -> Self {
        Self { columns, screen_lines }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A colored text span for rendering.
pub struct ColoredSpan {
    pub text: String,
    pub fg: Rgb8,
    pub bold: bool,
    pub italic: bool,
}

/// A cell with a non-default background color.
pub struct BgCell {
    pub row: usize,
    pub col: usize,
    pub bg: Rgb8,
}

/// Terminal content extracted for rendering.
pub struct GridContent {
    pub spans: Vec<ColoredSpan>,
    pub bg_cells: Vec<BgCell>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub selection: Option<SelectionRange>,
}

/// 16-entry ANSI color palette: indices 0..=15.
pub type AnsiPalette = [[u8; 3]; 16];

/// Resolve a named ANSI color to RGB using the given palette.
fn named_color_to_rgb(color: ansi::NamedColor, palette: &AnsiPalette, fg: [u8; 3]) -> Rgb8 {
    let idx = match color {
        ansi::NamedColor::Black => 0,
        ansi::NamedColor::Red => 1,
        ansi::NamedColor::Green => 2,
        ansi::NamedColor::Yellow => 3,
        ansi::NamedColor::Blue => 4,
        ansi::NamedColor::Magenta => 5,
        ansi::NamedColor::Cyan => 6,
        ansi::NamedColor::White => 7,
        ansi::NamedColor::BrightBlack => 8,
        ansi::NamedColor::BrightRed => 9,
        ansi::NamedColor::BrightGreen => 10,
        ansi::NamedColor::BrightYellow => 11,
        ansi::NamedColor::BrightBlue => 12,
        ansi::NamedColor::BrightMagenta => 13,
        ansi::NamedColor::BrightCyan => 14,
        ansi::NamedColor::BrightWhite => 15,
        // Foreground/Background/Cursor/DimForeground/etc — use theme fg
        _ => return Rgb8 { r: fg[0], g: fg[1], b: fg[2] },
    };
    let c = palette[idx];
    Rgb8 { r: c[0], g: c[1], b: c[2] }
}

/// Convert the 256-color indexed palette to RGB.
fn indexed_color_to_rgb(index: u8, palette: &AnsiPalette, _fg: [u8; 3]) -> Rgb8 {
    match index {
        0..=15 => {
            let c = palette[index as usize];
            Rgb8 { r: c[0], g: c[1], b: c[2] }
        }
        16..=231 => {
            // 6x6x6 color cube
            let idx = index - 16;
            let r = if idx / 36 > 0 { (idx / 36) * 40 + 55 } else { 0 };
            let g = if (idx % 36) / 6 > 0 { ((idx % 36) / 6) * 40 + 55 } else { 0 };
            let b = if !idx.is_multiple_of(6) { (idx % 6) * 40 + 55 } else { 0 };
            Rgb8 { r, g, b }
        }
        232..=255 => {
            // Grayscale ramp
            let v = (index - 232) * 10 + 8;
            Rgb8 { r: v, g: v, b: v }
        }
    }
}

/// Resolve a terminal foreground color to RGB, with bold brightening for standard colors.
fn resolve_fg_color(color: ansi::Color, bold: bool, palette: &AnsiPalette, fg: [u8; 3]) -> Rgb8 {
    match color {
        ansi::Color::Named(named) => {
            // Bold + standard color (0-7) → bright variant (8-15)
            let named = if bold {
                match named {
                    ansi::NamedColor::Black => ansi::NamedColor::BrightBlack,
                    ansi::NamedColor::Red => ansi::NamedColor::BrightRed,
                    ansi::NamedColor::Green => ansi::NamedColor::BrightGreen,
                    ansi::NamedColor::Yellow => ansi::NamedColor::BrightYellow,
                    ansi::NamedColor::Blue => ansi::NamedColor::BrightBlue,
                    ansi::NamedColor::Magenta => ansi::NamedColor::BrightMagenta,
                    ansi::NamedColor::Cyan => ansi::NamedColor::BrightCyan,
                    ansi::NamedColor::White => ansi::NamedColor::BrightWhite,
                    other => other,
                }
            } else {
                named
            };
            named_color_to_rgb(named, palette, fg)
        }
        ansi::Color::Spec(rgb) => Rgb8 { r: rgb.r, g: rgb.g, b: rgb.b },
        ansi::Color::Indexed(idx) => {
            // Bold + indexed 0-7 → indexed 8-15
            let idx = if bold && idx < 8 { idx + 8 } else { idx };
            indexed_color_to_rgb(idx, palette, fg)
        }
    }
}

/// Resolve a background color to RGB. No bold-brightening for backgrounds.
fn resolve_bg_color(color: ansi::Color, palette: &AnsiPalette, fg: [u8; 3]) -> Option<Rgb8> {
    match color {
        ansi::Color::Named(ansi::NamedColor::Background) => None, // default bg, skip
        ansi::Color::Named(named) => Some(named_color_to_rgb(named, palette, fg)),
        ansi::Color::Spec(rgb) => Some(Rgb8 { r: rgb.r, g: rgb.g, b: rgb.b }),
        ansi::Color::Indexed(idx) => Some(indexed_color_to_rgb(idx, palette, fg)),
    }
}

/// Read the current visible terminal content with colors, using the given ANSI palette.
pub fn read_grid_content<T: EventListener>(
    term: &Term<T>,
    palette: &AnsiPalette,
    fg: [u8; 3],
    bg: [u8; 3],
) -> GridContent {
    let default_fg = Rgb8 { r: fg[0], g: fg[1], b: fg[2] };
    let default_bg = Rgb8 { r: bg[0], g: bg[1], b: bg[2] };

    let grid = term.grid();
    let num_lines = grid.screen_lines();
    let num_cols = grid.columns();
    let display_offset = grid.display_offset();

    let mut spans: Vec<ColoredSpan> = Vec::new();
    let mut bg_cells: Vec<BgCell> = Vec::new();

    for line_idx in 0..num_lines {
        let row = &grid[Line(line_idx as i32 - display_offset as i32)];

        // Build spans by grouping consecutive cells with the same color
        let mut current_text = String::new();
        let mut current_fg = default_fg;
        let mut current_bold = false;
        let mut current_italic = false;

        for col_idx in 0..num_cols {
            let cell = &row[Column(col_idx)];
            let c = if cell.c == '\0' { ' ' } else { cell.c };
            let bold = cell.flags.contains(CellFlags::BOLD);
            let italic = cell.flags.contains(CellFlags::ITALIC);
            let inverse = cell.flags.contains(CellFlags::INVERSE);

            // Swap fg/bg when inverse (reverse video) is set
            let (cell_fg, bg_color) = if inverse {
                let bg = resolve_bg_color(cell.bg, palette, fg).unwrap_or(default_bg);
                let fg_resolved = resolve_fg_color(cell.fg, bold, palette, fg);
                (bg, Some(fg_resolved))
            } else {
                (resolve_fg_color(cell.fg, bold, palette, fg), resolve_bg_color(cell.bg, palette, fg))
            };

            if let Some(bg) = bg_color {
                bg_cells.push(BgCell { row: line_idx, col: col_idx, bg });
            }

            if cell_fg != current_fg || bold != current_bold || italic != current_italic {
                if !current_text.is_empty() {
                    spans.push(ColoredSpan {
                        text: current_text,
                        fg: current_fg,
                        bold: current_bold,
                        italic: current_italic,
                    });
                }
                current_text = String::new();
                current_fg = cell_fg;
                current_bold = bold;
                current_italic = italic;
            }
            current_text.push(c);
        }

        // Trim trailing spaces from last span on this line
        let trimmed = current_text.trim_end().to_string();
        if !trimmed.is_empty() {
            spans.push(ColoredSpan {
                text: trimmed,
                fg: current_fg,
                bold: current_bold,
                italic: current_italic,
            });
        }

        // Add newline between lines (except the last)
        if line_idx < num_lines - 1 {
            spans.push(ColoredSpan {
                text: "\n".to_string(),
                fg: default_fg,
                bold: false,
                italic: false,
            });
        }
    }

    let cursor_point = grid.cursor.point;
    let cursor_row = (cursor_point.line.0 + display_offset as i32).max(0) as usize;
    let cursor_col = cursor_point.column.0;

    let selection = term.selection.as_ref().and_then(|s| s.to_range(term));

    GridContent {
        spans,
        bg_cells,
        cursor_row,
        cursor_col,
        selection,
    }
}

pub mod error;
pub mod output;
pub mod types;

pub use error::{NexError, Result};
pub use output::{CompressedOutput, OutputClass};
pub use types::*;

/// Cell geometry constants shared across rendering, layout, and input.
pub const CELL_WIDTH_RATIO: f32 = 0.6;
pub const LINE_HEIGHT_RATIO: f32 = 1.3;
pub const PADDING: f32 = 8.0;

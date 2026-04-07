//! Token-saving output compression pipeline for Tonn.

mod classify;
mod compress;
mod strip;

pub use classify::classify;
pub use compress::compress;
pub use strip::strip_ansi;

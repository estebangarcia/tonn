//! Token-saving output compression pipeline for Nexterm.

mod classify;
mod compress;
mod strip;

pub use classify::classify;
pub use compress::compress;
pub use strip::strip_ansi;

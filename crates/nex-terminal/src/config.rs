//! Terminal configuration.

/// Top-level terminal configuration. Kept minimal for the MVP — Tonn only
/// constructs this with `scrolling_history` set and relies on defaults for
/// everything else.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub scrolling_history: usize,
}

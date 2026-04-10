//! Terminal event enum and `EventListener` trait.
//!
//! Matches the shape of the legacy backend so `NexEventListener` in `lib.rs`
//! can match on the same variants. Only `PtyWrite`, `Title`, `ResetTitle`,
//! and `Bell` are actually emitted in the MVP — the other variants exist so
//! exhaustive matches on the legacy `Event` enum still typecheck here.

/// Terminal event delivered to the host application.
#[derive(Debug, Clone)]
pub enum Event {
    /// Write bytes back to the PTY (e.g. DSR replies).
    PtyWrite(String),
    /// Window title change (OSC 0/2).
    Title(String),
    /// Reset the window title to the default.
    ResetTitle,
    /// Terminal bell (BEL / C0 0x07).
    Bell,
}

/// Trait for receiving terminal events.
pub trait EventListener {
    fn send_event(&self, _event: Event) {}
}

//! Terminal mode flags (DECSET/DECRST state).

bitflags::bitflags! {
    /// Terminal mode flags. Only `APP_CURSOR`, `ALT_SCREEN`, `LINE_WRAP`,
    /// `BRACKETED_PASTE`, and `SHOW_CURSOR` are actively used by Tonn today.
    /// The rest are tracked so DECSET/DECRST does not report false state.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct TermMode: u32 {
        const SHOW_CURSOR        = 1 << 0;
        const APP_CURSOR         = 1 << 1;
        const APP_KEYPAD         = 1 << 2;
        const MOUSE_REPORT_CLICK = 1 << 3;
        const BRACKETED_PASTE    = 1 << 4;
        const SGR_MOUSE          = 1 << 5;
        const LINE_WRAP          = 1 << 6;
        const LINE_FEED_NEW_LINE = 1 << 7;
        const ORIGIN             = 1 << 8;
        const INSERT             = 1 << 9;
        const FOCUS_IN_OUT       = 1 << 10;
        const ALT_SCREEN         = 1 << 11;
        const MOUSE_MOTION       = 1 << 12;
        const MOUSE_DRAG         = 1 << 13;
        const UTF8_MOUSE         = 1 << 14;
        const ALTERNATE_SCROLL   = 1 << 15;
    }
}

impl TermMode {
    /// Default mode set on fresh terminal init and after RIS (`ESC c`).
    pub const fn default_mode() -> Self {
        Self::from_bits_truncate(
            Self::SHOW_CURSOR.bits()
                | Self::LINE_WRAP.bits()
                | Self::ALTERNATE_SCROLL.bits(),
        )
    }
}

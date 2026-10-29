//! Mouse input → terminal mouse-report encoding.
//!
//! Pure logic — no winit, no PTY, no side effects. Mirrors `keys.rs`: takes a
//! windowing-agnostic mouse event plus the terminal mode and returns the bytes
//! to write to the PTY, or decides where wheel input should go.
//!
//! Tracking levels (DECSET): 1000 reports press/release + wheel, 1002 adds
//! motion while a button is held, 1003 adds all motion. Encodings: SGR
//! (DECSET 1006, `CSI < Cb ; Cx ; Cy M|m`) and the legacy X10 byte form
//! (`CSI M Cb Cx Cy`).

use crate::keys::Modifiers;
use crate::mode::TermMode;

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

const BUTTON_LEFT: u8 = 0;
const BUTTON_MIDDLE: u8 = 1;
const BUTTON_RIGHT: u8 = 2;
/// "No button" — used for motion without a held button, and by X10 for every
/// release (that encoding cannot say which button was released).
const BUTTON_NONE: u8 = 3;
const BUTTON_WHEEL_UP: u8 = 64;
const BUTTON_WHEEL_DOWN: u8 = 65;

const MOD_SHIFT: u8 = 4;
const MOD_ALT: u8 = 8;
const MOD_CTRL: u8 = 16;
const MOTION_FLAG: u8 = 32;

/// X10 sends each value as one byte offset by 32 so it stays printable.
const X10_OFFSET: u32 = 32;
/// Largest 1-based coordinate that still fits in an X10 byte (255 - 32).
const X10_MAX_COORD: u32 = 223;

const CSI_X10_PREFIX: &[u8] = b"\x1b[M";
const SGR_PRESS: char = 'M';
const SGR_RELEASE: char = 'm';

const FOCUS_IN: &[u8] = b"\x1b[I";
const FOCUS_OUT: &[u8] = b"\x1b[O";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A mouse button, including the wheel directions xterm reports as buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            Self::Left => BUTTON_LEFT,
            Self::Middle => BUTTON_MIDDLE,
            Self::Right => BUTTON_RIGHT,
            Self::WheelUp => BUTTON_WHEEL_UP,
            Self::WheelDown => BUTTON_WHEEL_DOWN,
        }
    }

    fn is_wheel(self) -> bool {
        matches!(self, Self::WheelUp | Self::WheelDown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Motion,
}

/// A mouse event in viewport cell coordinates.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    /// The button pressed/released, or for `Motion` the button held (if any).
    pub button: Option<MouseButton>,
    pub kind: MouseEventKind,
    /// 0-based viewport column.
    pub col: usize,
    /// 0-based viewport row.
    pub row: usize,
    pub mods: Modifiers,
}

/// Where a mouse-wheel step should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAction {
    /// Report it to the application as a wheel button press.
    Report,
    /// Send arrow keys (alternate scroll mode on the alt screen).
    ArrowKeys,
    /// Scroll Tonn's own scrollback.
    Scrollback,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Whether the application has enabled any mouse tracking level.
pub fn reporting_active(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Decide where wheel input goes. Holding Shift always bypasses the
/// application so the user can scroll Tonn's scrollback.
pub fn wheel_action(mode: TermMode, shift: bool) -> WheelAction {
    if shift {
        WheelAction::Scrollback
    } else if reporting_active(mode) {
        WheelAction::Report
    } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        WheelAction::ArrowKeys
    } else {
        WheelAction::Scrollback
    }
}

/// Focus-change report for DECSET 1004.
pub fn focus_report(focused: bool) -> &'static [u8] {
    if focused { FOCUS_IN } else { FOCUS_OUT }
}

/// Encode a mouse event as a report, or `None` if the current tracking mode
/// does not want it (or X10 cannot represent the coordinates).
pub fn encode(event: &MouseEvent, mode: TermMode) -> Option<Vec<u8>> {
    if !should_report(event, mode) {
        return None;
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        Some(encode_sgr(event))
    } else {
        encode_x10(event)
    }
}

// ---------------------------------------------------------------------------
// Encoding helpers
// ---------------------------------------------------------------------------

fn should_report(event: &MouseEvent, mode: TermMode) -> bool {
    match (event.kind, event.button) {
        (MouseEventKind::Press, Some(_)) => reporting_active(mode),
        // Wheel "buttons" only ever press.
        (MouseEventKind::Release, Some(b)) => reporting_active(mode) && !b.is_wheel(),
        (MouseEventKind::Release | MouseEventKind::Press, None) => false,
        (MouseEventKind::Motion, held) => {
            mode.contains(TermMode::MOUSE_MOTION)
                || (mode.contains(TermMode::MOUSE_DRAG) && held.is_some())
        }
    }
}

fn modifier_bits(mods: &Modifiers) -> u8 {
    (if mods.shift { MOD_SHIFT } else { 0 })
        | (if mods.alt { MOD_ALT } else { 0 })
        | (if mods.ctrl { MOD_CTRL } else { 0 })
}

/// Button code with modifier and motion bits. `release_as_none` selects the
/// X10 convention where every release is reported as "no button".
fn button_code(event: &MouseEvent, release_as_none: bool) -> u8 {
    let base = match (event.kind, event.button) {
        (MouseEventKind::Release, _) if release_as_none => BUTTON_NONE,
        (_, Some(button)) => button.code(),
        (_, None) => BUTTON_NONE,
    };
    let motion = if event.kind == MouseEventKind::Motion { MOTION_FLAG } else { 0 };
    base + motion + modifier_bits(&event.mods)
}

fn encode_sgr(event: &MouseEvent) -> Vec<u8> {
    let cb = button_code(event, false);
    let suffix = if event.kind == MouseEventKind::Release { SGR_RELEASE } else { SGR_PRESS };
    format!("\x1b[<{cb};{};{}{suffix}", event.col + 1, event.row + 1).into_bytes()
}

fn encode_x10(event: &MouseEvent) -> Option<Vec<u8>> {
    let col = event.col as u32 + 1;
    let row = event.row as u32 + 1;
    if col > X10_MAX_COORD || row > X10_MAX_COORD {
        return None;
    }
    let cb = button_code(event, true) as u32;
    let mut out = CSI_X10_PREFIX.to_vec();
    out.extend([cb, col, row].map(|v| (X10_OFFSET + v) as u8));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_MODS: Modifiers = Modifiers { shift: false, ctrl: false, alt: false };

    fn event(kind: MouseEventKind, button: Option<MouseButton>, col: usize, row: usize) -> MouseEvent {
        MouseEvent { button, kind, col, row, mods: NO_MODS }
    }

    fn sgr(level: TermMode) -> TermMode {
        level | TermMode::SGR_MOUSE
    }

    #[test]
    fn no_report_without_tracking() {
        let e = event(MouseEventKind::Press, Some(MouseButton::Left), 0, 0);
        assert_eq!(encode(&e, TermMode::SGR_MOUSE), None);
    }

    #[test]
    fn sgr_press_and_release_are_one_based() {
        let mode = sgr(TermMode::MOUSE_REPORT_CLICK);
        let press = event(MouseEventKind::Press, Some(MouseButton::Left), 4, 9);
        let release = event(MouseEventKind::Release, Some(MouseButton::Left), 4, 9);
        assert_eq!(encode(&press, mode).unwrap(), b"\x1b[<0;5;10M");
        assert_eq!(encode(&release, mode).unwrap(), b"\x1b[<0;5;10m");
    }

    #[test]
    fn sgr_right_button_with_modifiers() {
        let mode = sgr(TermMode::MOUSE_REPORT_CLICK);
        let mut e = event(MouseEventKind::Press, Some(MouseButton::Right), 0, 0);
        e.mods = Modifiers { shift: true, ctrl: true, alt: false };
        // 2 (right) + 4 (shift) + 16 (ctrl)
        assert_eq!(encode(&e, mode).unwrap(), b"\x1b[<22;1;1M");
    }

    #[test]
    fn sgr_wheel_up_and_down() {
        let mode = sgr(TermMode::MOUSE_REPORT_CLICK);
        let up = event(MouseEventKind::Press, Some(MouseButton::WheelUp), 2, 3);
        let down = event(MouseEventKind::Press, Some(MouseButton::WheelDown), 2, 3);
        assert_eq!(encode(&up, mode).unwrap(), b"\x1b[<64;3;4M");
        assert_eq!(encode(&down, mode).unwrap(), b"\x1b[<65;3;4M");
    }

    #[test]
    fn wheel_release_is_never_reported() {
        let mode = sgr(TermMode::MOUSE_REPORT_CLICK);
        let e = event(MouseEventKind::Release, Some(MouseButton::WheelUp), 0, 0);
        assert_eq!(encode(&e, mode), None);
    }

    #[test]
    fn motion_gated_by_tracking_level() {
        let drag = event(MouseEventKind::Motion, Some(MouseButton::Left), 1, 1);
        let hover = event(MouseEventKind::Motion, None, 1, 1);

        assert_eq!(encode(&drag, sgr(TermMode::MOUSE_REPORT_CLICK)), None);

        // 1002: only motion with a button held.
        assert_eq!(encode(&drag, sgr(TermMode::MOUSE_DRAG)).unwrap(), b"\x1b[<32;2;2M");
        assert_eq!(encode(&hover, sgr(TermMode::MOUSE_DRAG)), None);

        // 1003: all motion; no button is code 3 + motion flag.
        assert_eq!(encode(&hover, sgr(TermMode::MOUSE_MOTION)).unwrap(), b"\x1b[<35;2;2M");
    }

    #[test]
    fn x10_press_and_release() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let press = event(MouseEventKind::Press, Some(MouseButton::Middle), 0, 1);
        let release = event(MouseEventKind::Release, Some(MouseButton::Middle), 0, 1);
        assert_eq!(encode(&press, mode).unwrap(), vec![0x1b, b'[', b'M', 32 + 1, 32 + 1, 32 + 2]);
        // X10 reports every release as button 3.
        assert_eq!(encode(&release, mode).unwrap(), vec![0x1b, b'[', b'M', 32 + 3, 32 + 1, 32 + 2]);
    }

    #[test]
    fn x10_out_of_range_coordinates_are_dropped() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let e = event(MouseEventKind::Press, Some(MouseButton::Left), 223, 0);
        assert_eq!(encode(&e, mode), None);
        // SGR has no such limit.
        assert!(encode(&e, sgr(mode)).is_some());
    }

    #[test]
    fn wheel_routing() {
        let tracking = TermMode::MOUSE_REPORT_CLICK;
        let alt_scroll = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;

        assert_eq!(wheel_action(tracking, false), WheelAction::Report);
        assert_eq!(wheel_action(tracking | alt_scroll, false), WheelAction::Report);
        assert_eq!(wheel_action(alt_scroll, false), WheelAction::ArrowKeys);
        assert_eq!(wheel_action(TermMode::ALT_SCREEN, false), WheelAction::Scrollback);
        assert_eq!(wheel_action(TermMode::ALTERNATE_SCROLL, false), WheelAction::Scrollback);
        assert_eq!(wheel_action(TermMode::empty(), false), WheelAction::Scrollback);
        // Shift bypasses the application.
        assert_eq!(wheel_action(tracking, true), WheelAction::Scrollback);
        assert_eq!(wheel_action(alt_scroll, true), WheelAction::Scrollback);
    }

    #[test]
    fn focus_reports() {
        assert_eq!(focus_report(true), b"\x1b[I");
        assert_eq!(focus_report(false), b"\x1b[O");
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Pure mouse- and focus-event encoders. Given the active [`MouseProtocol`] and
//! an event, produce the exact bytes xterm would send (legacy/UTF-8/SGR/urxvt
//! mouse reporting, and `ESC [ I` / `ESC [ O` focus reporting). No terminal
//! state is touched here, so these functions are trivially unit-testable in
//! `super::encoding_tests`.

use super::types::{
    MouseButton, MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, MouseTracking,
};

/// Base button code (xterm "Cb" low bits) before modifiers/motion are folded
/// in. Wheel events set bit 6 (64).
fn mouse_button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
        // xterm uses the "no button" code 3 for hover motion (same base value
        // as a release, where the specific button is not distinguishable).
        MouseButton::NoButton => 3,
    }
}
/// Modifier bits folded into Cb: Shift = 4, Alt/Meta = 8, Ctrl = 16.
fn mouse_modifier_bits(mods: MouseModifiers) -> u16 {
    (if mods.shift { 4 } else { 0 })
        | (if mods.alt { 8 } else { 0 })
        | (if mods.ctrl { 16 } else { 0 })
}
/// Encode a mouse event into the exact bytes a terminal would send to the host
/// for the given [`MouseProtocol`], or `None` when the event should not be
/// reported under the active protocol (or cannot be represented).
///
/// `column`/`row` are 1-based screen coordinates. The result honors the
/// tracking gate (X10 reports presses only and carries no modifiers; normal
/// drops motion; button/any-event allow motion) and the selected encoding
/// (legacy byte, UTF-8, SGR, urxvt). For the legacy byte encoding a coordinate
/// beyond 223 cannot fit in a byte and the report is dropped, matching xterm.
pub fn encode_mouse_event(
    protocol: MouseProtocol,
    button: MouseButton,
    kind: MouseEventKind,
    column: usize,
    row: usize,
    mods: MouseModifiers,
) -> Option<Vec<u8>> {
    let mod_bits = mouse_tracking_gate(protocol.tracking, button, kind, mods)?;

    match protocol.encoding {
        MouseEncoding::Sgr => Some(encode_mouse_sgr(button, kind, column, row, mod_bits)),
        // SgrPixel (1016) shares the SGR wire shape and differs only in the
        // *units* of the coordinates. The cell-based entry has only cell
        // coordinates, so it emits the SGR-pixel shape with the coordinates it
        // was given — a transitional pass-through, not a cell→pixel invention.
        // A front end that wants true pixel coordinates routes 1016 through
        // [`encode_mouse_event_pixel`]; until the native pixel seam lands this
        // keeps 1016 from silently dropping every event.
        MouseEncoding::SgrPixel => Some(encode_mouse_sgr(button, kind, column, row, mod_bits)),
        MouseEncoding::Urxvt => Some(encode_mouse_urxvt(button, kind, column, row, mod_bits)),
        MouseEncoding::Default => encode_mouse_legacy(button, kind, column, row, mod_bits, false),
        MouseEncoding::Utf8 => encode_mouse_legacy(button, kind, column, row, mod_bits, true),
    }
}
/// Encode a mouse event for SGR-pixel reporting (DECSET 1016) from caller-owned
/// pixel coordinates. Emits the same `CSI < Cb ; Px ; Py M|m` wire shape as SGR
/// (1006) — including the lowercase `m` release terminator that preserves the
/// button code — but `px`/`py` are 1-based physical pixel coordinates supplied
/// by the front end. Core never converts cells to pixels: the front end owns
/// the cell→pixel metric (`CellMetrics`) and passes the pixel position here.
///
/// Returns `None` when the active encoding is not [`MouseEncoding::SgrPixel`]
/// (so a front end can call this only on the 1016 path) or when the active
/// tracking gate drops the event — identical gating to [`encode_mouse_event`]
/// (X10 reports presses only and strips modifiers; normal drops motion;
/// button-event drops no-button hover; any-event reports all motion).
pub fn encode_mouse_event_pixel(
    protocol: MouseProtocol,
    button: MouseButton,
    kind: MouseEventKind,
    px: usize,
    py: usize,
    mods: MouseModifiers,
) -> Option<Vec<u8>> {
    if protocol.encoding != MouseEncoding::SgrPixel {
        return None;
    }
    let mod_bits = mouse_tracking_gate(protocol.tracking, button, kind, mods)?;
    Some(encode_mouse_sgr(button, kind, px, py, mod_bits))
}
/// Apply the active tracking mode's reporting gate and compute the modifier
/// bits to fold into Cb. Returns `None` when the tracking mode would not report
/// this event, so every encoder (cell and pixel) shares identical gating:
/// X10 reports presses only and carries no modifiers; normal drops motion;
/// button-event (1002) drops no-button hover but keeps held-button motion;
/// any-event (1003) reports all motion.
fn mouse_tracking_gate(
    tracking: MouseTracking,
    button: MouseButton,
    kind: MouseEventKind,
    mods: MouseModifiers,
) -> Option<u16> {
    match tracking {
        MouseTracking::Off => return None,
        MouseTracking::X10 => {
            if kind != MouseEventKind::Press {
                return None;
            }
        }
        MouseTracking::Normal => {
            if kind == MouseEventKind::Motion {
                return None;
            }
        }
        MouseTracking::ButtonEvent => {
            if kind == MouseEventKind::Motion && button == MouseButton::NoButton {
                return None;
            }
        }
        MouseTracking::AnyEvent => {}
    }

    // X10 carries no modifiers; every other mode folds them into Cb.
    Some(if tracking == MouseTracking::X10 {
        0
    } else {
        mouse_modifier_bits(mods)
    })
}
/// Encode a focus change for DECSET 1004 focus reporting: focus-in is `ESC [ I`
/// and focus-out is `ESC [ O`. Returns `None` when `reporting` is off, so a
/// front end can call this unconditionally on every window focus change and let
/// the terminal state decide whether to emit anything. Pure: no terminal state.
pub fn encode_focus_event(reporting: bool, focused: bool) -> Option<Vec<u8>> {
    if !reporting {
        return None;
    }
    Some(if focused {
        vec![0x1b, b'[', b'I']
    } else {
        vec![0x1b, b'[', b'O']
    })
}
/// Cb for the legacy/urxvt encodings: release collapses to button bits `3`
/// (the specific button is not distinguishable); press/motion use the real
/// button code. Motion sets bit 5 (32). Modifiers are pre-folded by the caller.
fn legacy_cb(button: MouseButton, kind: MouseEventKind, mod_bits: u16) -> u16 {
    let base = match kind {
        MouseEventKind::Release => 3,
        _ => mouse_button_code(button),
    };
    let motion = if kind == MouseEventKind::Motion {
        32
    } else {
        0
    };
    base + mod_bits + motion
}
/// SGR (1006) and SGR-pixel (1016): `CSI < Cb ; Cx ; Cy M|m`. Cb keeps the real
/// button code even on release (the release is conveyed by the lowercase `m`
/// terminator), so SGR is the only family that reports which button was
/// released. The wire shape is identical for both modes; the caller decides
/// whether `column`/`row` carry cell (1006) or pixel (1016) coordinates.
fn encode_mouse_sgr(
    button: MouseButton,
    kind: MouseEventKind,
    column: usize,
    row: usize,
    mod_bits: u16,
) -> Vec<u8> {
    let motion = if kind == MouseEventKind::Motion {
        32
    } else {
        0
    };
    let cb = mouse_button_code(button) + mod_bits + motion;
    let terminator = if kind == MouseEventKind::Release {
        'm'
    } else {
        'M'
    };
    format!("\x1b[<{cb};{column};{row}{terminator}").into_bytes()
}
/// urxvt (1015): `CSI Cb ; Cx ; Cy M` with decimal values. Cb is offset by 32
/// (as in the legacy byte protocol) but written as a decimal parameter, and the
/// coordinates are plain 1-based decimals, so there is no 223 limit.
fn encode_mouse_urxvt(
    button: MouseButton,
    kind: MouseEventKind,
    column: usize,
    row: usize,
    mod_bits: u16,
) -> Vec<u8> {
    let cb = 32 + legacy_cb(button, kind, mod_bits);
    format!("\x1b[{cb};{column};{row}M").into_bytes()
}
/// Legacy byte encoding: `CSI M Cb Cx Cy`, each value offset by 32. With
/// `utf8` false (default protocol) each value must fit in a single byte, so a
/// coordinate above 223 makes the report unrepresentable and the whole event is
/// dropped (`None`). With `utf8` true (mode 1005) values are encoded as UTF-8,
/// extending the range to U+07FF.
fn encode_mouse_legacy(
    button: MouseButton,
    kind: MouseEventKind,
    column: usize,
    row: usize,
    mod_bits: u16,
    utf8: bool,
) -> Option<Vec<u8>> {
    let cb = 32 + legacy_cb(button, kind, mod_bits);
    let cx = 32 + column as u32;
    let cy = 32 + row as u32;

    let mut out = vec![0x1b, b'[', b'M'];
    push_legacy_value(&mut out, cb as u32, utf8)?;
    push_legacy_value(&mut out, cx, utf8)?;
    push_legacy_value(&mut out, cy, utf8)?;
    Some(out)
}
/// Append one legacy-encoded value. Byte mode rejects values above 255 (the
/// 223-coordinate limit once the +32 offset is applied); UTF-8 mode encodes up
/// to U+07FF as one or two bytes and rejects anything larger.
fn push_legacy_value(out: &mut Vec<u8>, value: u32, utf8: bool) -> Option<()> {
    if utf8 {
        if value < 0x80 {
            out.push(value as u8);
        } else if value <= 0x7FF {
            out.push(0xC0 | (value >> 6) as u8);
            out.push(0x80 | (value & 0x3F) as u8);
        } else {
            return None;
        }
    } else if value <= 0xFF {
        out.push(value as u8);
    } else {
        return None;
    }
    Some(())
}

use crate::core::{
    MouseButton as CoreMouseButton, MouseEventKind, MouseModifiers as CoreMouseModifiers,
    MouseProtocol, MouseTracking, Terminal, encode_focus_event, encode_mouse_event,
};
use crate::input::{Key, Modifiers};
use crate::selection::CellPoint;

use winit::event::{MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::viewport::wheel_lines;

pub(super) fn changed_window_title(terminal: &mut Terminal, default_title: &str) -> Option<String> {
    terminal
        .take_title_changed()
        .then(|| terminal.title().unwrap_or(default_title).to_owned())
}

fn core_mouse_modifiers(mods: Modifiers) -> CoreMouseModifiers {
    CoreMouseModifiers {
        // Shift is reserved for local selection when mouse reporting is active.
        shift: false,
        alt: mods.alt,
        ctrl: mods.ctrl,
    }
}

pub(super) fn encode_native_mouse_report(
    protocol: MouseProtocol,
    point: CellPoint,
    button: CoreMouseButton,
    kind: MouseEventKind,
    mods: Modifiers,
) -> Option<Vec<u8>> {
    encode_mouse_event(
        protocol,
        button,
        kind,
        point.column + 1,
        point.row + 1,
        core_mouse_modifiers(mods),
    )
}

pub(super) fn motion_report_button(
    protocol: MouseProtocol,
    held_button: Option<CoreMouseButton>,
) -> Option<CoreMouseButton> {
    held_button.or_else(|| {
        (protocol.tracking == MouseTracking::AnyEvent).then_some(CoreMouseButton::NoButton)
    })
}

pub(super) fn encode_native_focus_report(terminal: &Terminal, focused: bool) -> Option<Vec<u8>> {
    encode_focus_event(terminal.focus_reporting(), focused)
}

pub(super) fn wheel_report_button(delta: MouseScrollDelta) -> Option<CoreMouseButton> {
    match wheel_lines(delta, 1).cmp(&0) {
        std::cmp::Ordering::Greater => Some(CoreMouseButton::WheelUp),
        std::cmp::Ordering::Less => Some(CoreMouseButton::WheelDown),
        std::cmp::Ordering::Equal => None,
    }
}

pub(super) fn is_copy_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    if !(mods.ctrl && mods.shift) || mods.alt {
        return false;
    }

    matches!(logical, WinitKey::Character(text) if text.eq_ignore_ascii_case("c"))
}

pub(super) fn is_paste_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    if !(mods.ctrl && mods.shift) || mods.alt {
        return false;
    }

    matches!(logical, WinitKey::Character(text) if text.eq_ignore_ascii_case("v"))
}

/// Shift+PageUp pages the scrollback viewport upward. Shift only (no Ctrl/Alt)
/// so plain PageUp still reaches the PTY.
pub(super) fn is_scroll_up_key(logical: &WinitKey, mods: Modifiers) -> bool {
    mods.shift && !mods.ctrl && !mods.alt && matches!(logical, WinitKey::Named(NamedKey::PageUp))
}

/// Shift+PageDown pages the scrollback viewport toward the live bottom.
pub(super) fn is_scroll_down_key(logical: &WinitKey, mods: Modifiers) -> bool {
    mods.shift && !mods.ctrl && !mods.alt && matches!(logical, WinitKey::Named(NamedKey::PageDown))
}

/// Translate a `winit` [`NamedKey`] into the neutral [`Key`] model.
///
/// `shift` is consulted only to turn Tab into [`Key::BackTab`] (Shift-Tab),
/// matching how the crossterm front end distinguishes the two. `Space` is
/// mapped to [`Key::Char(' ')`] rather than a named key so Ctrl-Space encodes
/// to `NUL` via the shared encoder. Named keys the prototype does not handle
/// (function keys, media keys, etc.) return `None`.
pub(super) fn map_named_key(named: NamedKey, shift: bool) -> Option<Key> {
    Some(match named {
        NamedKey::Enter => Key::Enter,
        NamedKey::Backspace => Key::Backspace,
        NamedKey::ArrowLeft => Key::Left,
        NamedKey::ArrowRight => Key::Right,
        NamedKey::ArrowUp => Key::Up,
        NamedKey::ArrowDown => Key::Down,
        NamedKey::Home => Key::Home,
        NamedKey::End => Key::End,
        NamedKey::PageUp => Key::PageUp,
        NamedKey::PageDown => Key::PageDown,
        NamedKey::Tab if shift => Key::BackTab,
        NamedKey::Tab => Key::Tab,
        NamedKey::Delete => Key::Delete,
        NamedKey::Insert => Key::Insert,
        NamedKey::Escape => Key::Esc,
        NamedKey::Space => Key::Char(' '),
        _ => return None,
    })
}

pub(super) fn map_winit_mouse_button(button: WinitMouseButton) -> Option<CoreMouseButton> {
    Some(match button {
        WinitMouseButton::Left => CoreMouseButton::Left,
        WinitMouseButton::Middle => CoreMouseButton::Middle,
        WinitMouseButton::Right => CoreMouseButton::Right,
        _ => return None,
    })
}

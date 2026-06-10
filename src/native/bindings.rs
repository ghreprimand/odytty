use crate::core::{
    MouseButton as CoreMouseButton, MouseEventKind, MouseModifiers as CoreMouseModifiers,
    MouseProtocol, MouseTracking, Terminal, encode_focus_event, encode_mouse_event,
};
use crate::input::{Key, Modifiers};
use crate::selection::CellPoint;
use crate::settings::{
    BindableAction, KeyBindingKey, KeyBindingModifiers, KeyBindingNamedKey, KeyBindingOverride,
    KeyChord,
};

use winit::event::{MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::keyboard::{Key as WinitKey, NamedKey};

use super::viewport::wheel_lines;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeyBindings {
    bindings: Vec<(KeyChord, BindableAction)>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            bindings: default_key_bindings(),
        }
    }
}

impl KeyBindings {
    pub(super) fn from_overrides(overrides: &[KeyBindingOverride]) -> Self {
        let mut bindings = default_key_bindings();
        for override_ in overrides {
            bindings.retain(|(_, action)| *action != override_.action);
            bindings.push((override_.chord, override_.action));
        }
        Self { bindings }
    }

    pub(super) fn action_for(
        &self,
        logical: &WinitKey,
        mods: Modifiers,
        super_key: bool,
    ) -> Option<BindableAction> {
        let chord = chord_from_winit(logical, mods, super_key)?;
        self.bindings
            .iter()
            .rev()
            .find_map(|(candidate, action)| (*candidate == chord).then_some(*action))
    }
}

fn default_key_bindings() -> Vec<(KeyChord, BindableAction)> {
    vec![
        (
            char_chord('f', true, true, false, false),
            BindableAction::Search,
        ),
        (
            char_chord('c', true, true, false, false),
            BindableAction::Copy,
        ),
        (
            char_chord('v', true, true, false, false),
            BindableAction::Paste,
        ),
        (
            named_chord(KeyBindingNamedKey::PageUp, false, true, false, false),
            BindableAction::ScrollPageUp,
        ),
        (
            named_chord(KeyBindingNamedKey::PageDown, false, true, false, false),
            BindableAction::ScrollPageDown,
        ),
    ]
}

fn char_chord(ch: char, ctrl: bool, shift: bool, alt: bool, super_key: bool) -> KeyChord {
    KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl,
            shift,
            alt,
            super_key,
        },
        key: KeyBindingKey::Character(ch.to_ascii_lowercase()),
    }
}

fn named_chord(
    named: KeyBindingNamedKey,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_key: bool,
) -> KeyChord {
    KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl,
            shift,
            alt,
            super_key,
        },
        key: KeyBindingKey::Named(named),
    }
}

pub(super) fn chord_from_winit(
    logical: &WinitKey,
    mods: Modifiers,
    super_key: bool,
) -> Option<KeyChord> {
    let key = match logical {
        WinitKey::Character(text) => {
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() || !ch.is_ascii_alphanumeric() {
                return None;
            }
            KeyBindingKey::Character(ch.to_ascii_lowercase())
        }
        WinitKey::Named(named) => KeyBindingKey::Named(binding_named_key(*named)?),
        _ => return None,
    };
    Some(KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl: mods.ctrl,
            shift: mods.shift,
            alt: mods.alt,
            super_key,
        },
        key,
    })
}

fn binding_named_key(named: NamedKey) -> Option<KeyBindingNamedKey> {
    Some(match named {
        NamedKey::Enter => KeyBindingNamedKey::Enter,
        NamedKey::Backspace => KeyBindingNamedKey::Backspace,
        NamedKey::Escape => KeyBindingNamedKey::Escape,
        NamedKey::Tab => KeyBindingNamedKey::Tab,
        NamedKey::Space => KeyBindingNamedKey::Space,
        NamedKey::PageUp => KeyBindingNamedKey::PageUp,
        NamedKey::PageDown => KeyBindingNamedKey::PageDown,
        NamedKey::Home => KeyBindingNamedKey::Home,
        NamedKey::End => KeyBindingNamedKey::End,
        NamedKey::Delete => KeyBindingNamedKey::Delete,
        NamedKey::Insert => KeyBindingNamedKey::Insert,
        NamedKey::ArrowUp => KeyBindingNamedKey::ArrowUp,
        NamedKey::ArrowDown => KeyBindingNamedKey::ArrowDown,
        NamedKey::ArrowLeft => KeyBindingNamedKey::ArrowLeft,
        NamedKey::ArrowRight => KeyBindingNamedKey::ArrowRight,
        NamedKey::F1 => KeyBindingNamedKey::F(1),
        NamedKey::F2 => KeyBindingNamedKey::F(2),
        NamedKey::F3 => KeyBindingNamedKey::F(3),
        NamedKey::F4 => KeyBindingNamedKey::F(4),
        NamedKey::F5 => KeyBindingNamedKey::F(5),
        NamedKey::F6 => KeyBindingNamedKey::F(6),
        NamedKey::F7 => KeyBindingNamedKey::F(7),
        NamedKey::F8 => KeyBindingNamedKey::F(8),
        NamedKey::F9 => KeyBindingNamedKey::F(9),
        NamedKey::F10 => KeyBindingNamedKey::F(10),
        NamedKey::F11 => KeyBindingNamedKey::F(11),
        NamedKey::F12 => KeyBindingNamedKey::F(12),
        NamedKey::F13 => KeyBindingNamedKey::F(13),
        NamedKey::F14 => KeyBindingNamedKey::F(14),
        NamedKey::F15 => KeyBindingNamedKey::F(15),
        NamedKey::F16 => KeyBindingNamedKey::F(16),
        NamedKey::F17 => KeyBindingNamedKey::F(17),
        NamedKey::F18 => KeyBindingNamedKey::F(18),
        NamedKey::F19 => KeyBindingNamedKey::F(19),
        NamedKey::F20 => KeyBindingNamedKey::F(20),
        NamedKey::F21 => KeyBindingNamedKey::F(21),
        NamedKey::F22 => KeyBindingNamedKey::F(22),
        NamedKey::F23 => KeyBindingNamedKey::F(23),
        NamedKey::F24 => KeyBindingNamedKey::F(24),
        _ => return None,
    })
}

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

#[cfg(test)]
pub(super) fn is_copy_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::Copy)
}

#[cfg(test)]
pub(super) fn is_paste_shortcut(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::Paste)
}

/// Shift+PageUp pages the scrollback viewport upward. Shift only (no Ctrl/Alt)
/// so plain PageUp still reaches the PTY.
#[cfg(test)]
pub(super) fn is_scroll_up_key(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::ScrollPageUp)
}

/// Shift+PageDown pages the scrollback viewport toward the live bottom.
#[cfg(test)]
pub(super) fn is_scroll_down_key(logical: &WinitKey, mods: Modifiers) -> bool {
    KeyBindings::default().action_for(logical, mods, false) == Some(BindableAction::ScrollPageDown)
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

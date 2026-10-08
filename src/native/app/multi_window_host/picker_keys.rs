// SPDX-License-Identifier: GPL-3.0-only
//! Merge-picker key routing. While a picker is open a pressed 1-9 digit or
//! Escape drives it; the physical key it consumed is remembered so that key's
//! repeats and release never reach a terminal, even after the picker closed.

use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key as WinitKey, NamedKey, PhysicalKey};

use super::{MultiWindowHost, PickerKey};

/// The parts of a keyboard event the picker routing reads, so the routing
/// can be driven by tests (winit key events cannot be built outside winit).
pub(super) struct PickerKeyInput<'a> {
    pub(super) physical: PhysicalKey,
    pub(super) pressed: bool,
    pub(super) repeat: bool,
    pub(super) logical: &'a WinitKey,
}

impl MultiWindowHost {
    /// Route one window keyboard event through the picker. Returns `true`
    /// when the event was consumed.
    pub(super) fn route_picker_key(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        self.route_picker_input(PickerKeyInput {
            physical: key.physical_key,
            pressed: key.state == ElementState::Pressed,
            repeat: key.repeat,
            logical: &key.logical_key,
        })
    }

    /// Consume a picker key while a picker is open, and a repeat or release
    /// of a key the picker consumed. A fresh press of such a key (its release
    /// was lost) clears the record and routes normally.
    pub(super) fn route_picker_input(&mut self, key: PickerKeyInput<'_>) -> bool {
        if let Some(index) = self
            .picker_consumed_keys
            .iter()
            .position(|consumed| *consumed == key.physical)
        {
            if !key.pressed {
                self.picker_consumed_keys.remove(index);
                return true;
            }
            if key.repeat {
                return true;
            }
            self.picker_consumed_keys.remove(index);
        }
        if self.picker.is_none() {
            return false;
        }
        let Some(action) = decode_picker_key(&key) else {
            return false;
        };
        self.picker_consumed_keys.push(key.physical);
        self.handle_picker_key(action);
        true
    }
}

/// Decode a merge-picker keypress. Only a pressed Escape or a single 1-9
/// digit character is intercepted; everything else (including a text of more
/// than one character) returns `None` and falls through to the window.
fn decode_picker_key(key: &PickerKeyInput<'_>) -> Option<PickerKey> {
    if !key.pressed || key.repeat {
        return None;
    }
    match key.logical {
        WinitKey::Named(NamedKey::Escape) => Some(PickerKey::Cancel),
        WinitKey::Character(text) => {
            let mut chars = text.chars();
            let digit = chars.next()?.to_digit(10)?;
            if chars.next().is_some() {
                return None;
            }
            let numeral = u8::try_from(digit).ok()?;
            crate::native::merge_picker::MergePicker::is_keyboard_selectable(numeral)
                .then_some(PickerKey::Select(numeral))
        }
        _ => None,
    }
}

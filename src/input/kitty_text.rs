// SPDX-License-Identifier: GPL-3.0-only
//! Generated text admission for Kitty keyboard reports.

use super::{KITTY_REPORT_ALL_KEYS, KITTY_REPORT_ASSOCIATED_TEXT, Key, KeyEventType, Modifiers};

/// Synthetic keys have no platform text record. Infer text only for ordinary
/// press/repeat characters without Ctrl or Alt.
pub(super) fn synthetic_text(
    key: Key,
    mods: Modifiers,
    event_type: KeyEventType,
    flags: u16,
) -> Option<String> {
    if flags & (KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT)
        != (KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT)
    {
        return None;
    }
    match key {
        Key::Char(ch) if event_type != KeyEventType::Release && !mods.ctrl && !mods.alt => {
            Some(ch.to_string())
        }
        _ => None,
    }
}

pub(super) fn associated_text(
    text: Option<&str>,
    flags: u16,
    event_type: KeyEventType,
) -> Option<String> {
    if event_type == KeyEventType::Release
        || flags & (KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT)
            != (KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT)
    {
        return None;
    }
    let text = text.filter(|text| !text.is_empty() && !text.chars().any(char::is_control))?;
    Some(
        text.chars()
            .map(|ch| (ch as u32).to_string())
            .collect::<Vec<_>>()
            .join(":"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KITTY_REPORT_EVENT_TYPES, KeyModes, encode_key_event_with_text};

    fn modes() -> KeyModes {
        KeyModes {
            kitty_keyboard_flags: KITTY_REPORT_ALL_KEYS
                | KITTY_REPORT_ASSOCIATED_TEXT
                | KITTY_REPORT_EVENT_TYPES,
            ..KeyModes::default()
        }
    }

    #[test]
    fn generated_text_is_independent_of_identity_and_modifier() {
        assert_eq!(
            encode_key_event_with_text(
                Key::Char('e'),
                Modifiers::ALT,
                modes(),
                KeyEventType::Press,
                Some("e\u{301}")
            ),
            b"\x1b[101;3;101:769u"
        );
        assert_eq!(
            encode_key_event_with_text(
                Key::Char('a'),
                Modifiers::NONE,
                modes(),
                KeyEventType::Repeat,
                Some("A")
            ),
            b"\x1b[97;1:2;65u"
        );
        assert_eq!(
            encode_key_event_with_text(
                Key::Char('a'),
                Modifiers::NONE,
                modes(),
                KeyEventType::Release,
                Some("a")
            ),
            b"\x1b[97;1:3u"
        );
        assert_eq!(
            encode_key_event_with_text(
                Key::Char('a'),
                Modifiers::NONE,
                modes(),
                KeyEventType::Press,
                None
            ),
            b"\x1b[97u"
        );
        assert_eq!(
            encode_key_event_with_text(
                Key::KeypadDigit(1),
                Modifiers::NONE,
                modes(),
                KeyEventType::Press,
                Some("1")
            ),
            b"\x1b[57400;;49u"
        );
        assert_eq!(
            encode_key_event_with_text(
                Key::Char('d'),
                Modifiers::CTRL,
                modes(),
                KeyEventType::Press,
                Some("\u{4}")
            ),
            b"\x1b[100;5u"
        );
    }

    #[test]
    fn associated_text_rejects_controls_and_empty_values() {
        for text in ["", "a\n", "a\u{7f}", "a\u{85}"] {
            assert_eq!(
                encode_key_event_with_text(
                    Key::Char('a'),
                    Modifiers::NONE,
                    modes(),
                    KeyEventType::Press,
                    Some(text)
                ),
                b"\x1b[97u"
            );
        }
    }

    #[test]
    fn generated_text_does_not_change_legacy_or_win32_encoding() {
        for modes in [
            KeyModes::default(),
            KeyModes {
                win32_input: true,
                ..modes()
            },
        ] {
            assert_eq!(
                encode_key_event_with_text(
                    Key::Char('a'),
                    Modifiers::NONE,
                    modes,
                    KeyEventType::Press,
                    Some("e\u{301}")
                ),
                crate::input::encode_key_event(
                    Key::Char('a'),
                    Modifiers::NONE,
                    modes,
                    KeyEventType::Press
                )
            );
        }
    }
}

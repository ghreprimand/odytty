// SPDX-License-Identifier: GPL-3.0-only
//! Keyboard encoder regression tests.

use super::*;

#[test]
fn encodes_printable_chars() {
    assert_eq!(
        encode_key(Key::Char('a'), Modifiers::NONE, KeyModes::default()),
        b"a"
    );
    assert_eq!(
        encode_key(Key::Char('Z'), Modifiers::NONE, KeyModes::default()),
        b"Z"
    );
    assert_eq!(
        encode_key(Key::Char('@'), Modifiers::NONE, KeyModes::default()),
        b"@"
    );
}

#[test]
fn encodes_enter_and_backspace() {
    assert_eq!(
        encode_key(Key::Enter, Modifiers::NONE, KeyModes::default()),
        b"\r"
    );
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::NONE, KeyModes::default()),
        vec![0x7f]
    );
}

#[test]
fn encodes_arrows_and_named_keys() {
    assert_eq!(
        encode_key(Key::Up, Modifiers::NONE, KeyModes::default()),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key(Key::Down, Modifiers::NONE, KeyModes::default()),
        b"\x1b[B"
    );
    assert_eq!(
        encode_key(Key::Right, Modifiers::NONE, KeyModes::default()),
        b"\x1b[C"
    );
    assert_eq!(
        encode_key(Key::Left, Modifiers::NONE, KeyModes::default()),
        b"\x1b[D"
    );
    assert_eq!(
        encode_key(Key::Home, Modifiers::NONE, KeyModes::default()),
        b"\x1b[H"
    );
    assert_eq!(
        encode_key(Key::End, Modifiers::NONE, KeyModes::default()),
        b"\x1b[F"
    );
    assert_eq!(
        encode_key(Key::Delete, Modifiers::NONE, KeyModes::default()),
        b"\x1b[3~"
    );
    assert_eq!(
        encode_key(Key::BackTab, Modifiers::NONE, KeyModes::default()),
        b"\x1b[Z"
    );
}

#[test]
fn encodes_control_letters() {
    // Ctrl-C -> 0x03, Ctrl-D -> 0x04.
    assert_eq!(
        encode_key(Key::Char('c'), Modifiers::CTRL, KeyModes::default()),
        vec![3]
    );
    assert_eq!(
        encode_key(Key::Char('d'), Modifiers::CTRL, KeyModes::default()),
        vec![4]
    );
    // Case-insensitive.
    assert_eq!(
        encode_key(Key::Char('C'), Modifiers::CTRL, KeyModes::default()),
        vec![3]
    );
}

#[test]
fn ctrl_without_mapping_forwards_translated_text() {
    // A digit has no classic control byte. The Character event is already
    // translated text, so legacy mode forwards it instead of swallowing a
    // valid key event.
    assert_eq!(
        encode_key(Key::Char('1'), Modifiers::CTRL, KeyModes::default()),
        b"1"
    );
}

#[test]
fn alt_prefixes_escape() {
    assert_eq!(
        encode_key(Key::Char('b'), Modifiers::ALT, KeyModes::default()),
        b"\x1bb"
    );
    assert_eq!(
        encode_key(Key::Left, Modifiers::ALT, KeyModes::default()),
        b"\x1b[1;3D"
    );
}

#[test]
fn application_cursor_mode_uses_ss3_for_unmodified_cursor_keys() {
    let modes = KeyModes {
        application_cursor: true,
        application_keypad: false,
        ..KeyModes::default()
    };

    assert_eq!(encode_key(Key::Up, Modifiers::NONE, modes), b"\x1bOA");
    assert_eq!(encode_key(Key::Down, Modifiers::NONE, modes), b"\x1bOB");
    assert_eq!(encode_key(Key::Right, Modifiers::NONE, modes), b"\x1bOC");
    assert_eq!(encode_key(Key::Left, Modifiers::NONE, modes), b"\x1bOD");
    assert_eq!(encode_key(Key::Home, Modifiers::NONE, modes), b"\x1bOH");
    assert_eq!(encode_key(Key::End, Modifiers::NONE, modes), b"\x1bOF");
}

#[test]
fn modified_named_keys_use_xterm_modifier_table() {
    assert_eq!(
        encode_key(Key::Right, Modifiers::CTRL, KeyModes::default()),
        b"\x1b[1;5C"
    );
    assert_eq!(
        encode_key(
            Key::Left,
            Modifiers {
                shift: true,
                alt: true,
                ctrl: true,
            },
            KeyModes::default()
        ),
        b"\x1b[1;8D"
    );
    assert_eq!(
        encode_key(
            Key::Delete,
            Modifiers {
                shift: true,
                alt: false,
                ctrl: true,
            },
            KeyModes::default()
        ),
        b"\x1b[3;6~"
    );
    assert_eq!(
        encode_key(
            Key::PageDown,
            Modifiers {
                shift: false,
                alt: true,
                ctrl: true,
            },
            KeyModes::default()
        ),
        b"\x1b[6;7~"
    );
}

#[test]
fn application_keypad_mode_uses_ss3_keypad_forms() {
    let modes = KeyModes {
        application_cursor: false,
        application_keypad: true,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key(Key::KeypadDigit(0), Modifiers::NONE, modes),
        b"\x1bOp"
    );
    assert_eq!(
        encode_key(Key::KeypadDigit(9), Modifiers::NONE, modes),
        b"\x1bOy"
    );
    assert_eq!(
        encode_key(Key::KeypadDecimal, Modifiers::NONE, modes),
        b"\x1bOn"
    );
    assert_eq!(
        encode_key(Key::KeypadAdd, Modifiers::NONE, modes),
        b"\x1bOk"
    );
    assert_eq!(
        encode_key(Key::KeypadSubtract, Modifiers::NONE, modes),
        b"\x1bOm"
    );
    assert_eq!(
        encode_key(Key::KeypadMultiply, Modifiers::NONE, modes),
        b"\x1bOj"
    );
    assert_eq!(
        encode_key(Key::KeypadDivide, Modifiers::NONE, modes),
        b"\x1bOo"
    );
    assert_eq!(
        encode_key(Key::KeypadEnter, Modifiers::NONE, modes),
        b"\x1bOM"
    );
}

#[test]
fn normal_keypad_mode_sends_numeric_payloads() {
    let modes = KeyModes::default();

    assert_eq!(
        encode_key(Key::KeypadDigit(2), Modifiers::NONE, modes),
        b"2"
    );
    assert_eq!(encode_key(Key::KeypadDecimal, Modifiers::NONE, modes), b".");
    assert_eq!(encode_key(Key::KeypadAdd, Modifiers::NONE, modes), b"+");
    assert_eq!(
        encode_key(Key::KeypadSubtract, Modifiers::NONE, modes),
        b"-"
    );
    assert_eq!(
        encode_key(Key::KeypadMultiply, Modifiers::NONE, modes),
        b"*"
    );
    assert_eq!(encode_key(Key::KeypadDivide, Modifiers::NONE, modes), b"/");
    assert_eq!(encode_key(Key::KeypadEnter, Modifiers::NONE, modes), b"\r");
}

#[test]
fn function_keys_encode_legacy_forms() {
    let modes = KeyModes::default();
    let expected: [&[u8]; 12] = [
        b"\x1bOP",
        b"\x1bOQ",
        b"\x1bOR",
        b"\x1bOS",
        b"\x1b[15~",
        b"\x1b[17~",
        b"\x1b[18~",
        b"\x1b[19~",
        b"\x1b[20~",
        b"\x1b[21~",
        b"\x1b[23~",
        b"\x1b[24~",
    ];

    for (index, bytes) in expected.iter().enumerate() {
        let number = index as u8 + 1;
        assert_eq!(
            encode_key(Key::F(number), Modifiers::NONE, modes),
            *bytes,
            "F{number}"
        );
    }
    // Outside the supported range: no output rather than junk bytes.
    assert!(encode_key(Key::F(0), Modifiers::NONE, modes).is_empty());
    assert!(encode_key(Key::F(13), Modifiers::NONE, modes).is_empty());
}

#[test]
fn modified_function_keys_use_xterm_modifier_forms() {
    let modes = KeyModes::default();
    let shift = Modifiers {
        ctrl: false,
        alt: false,
        shift: true,
    };
    let all = Modifiers {
        ctrl: true,
        alt: true,
        shift: true,
    };

    assert_eq!(encode_key(Key::F(1), Modifiers::CTRL, modes), b"\x1b[1;5P");
    assert_eq!(encode_key(Key::F(2), shift, modes), b"\x1b[1;2Q");
    assert_eq!(encode_key(Key::F(3), Modifiers::ALT, modes), b"\x1b[1;3R");
    assert_eq!(encode_key(Key::F(4), all, modes), b"\x1b[1;8S");
    assert_eq!(encode_key(Key::F(5), Modifiers::CTRL, modes), b"\x1b[15;5~");
    assert_eq!(encode_key(Key::F(10), shift, modes), b"\x1b[21;2~");
    assert_eq!(encode_key(Key::F(12), all, modes), b"\x1b[24;8~");
}

#[test]
fn kitty_flags_encode_function_keys_with_functional_table_forms() {
    // Under active kitty flags the functional-key table applies: F1/F2/F4
    // use the CSI letter forms (parameters omitted unmodified), F3 uses
    // CSI 13~ (CSI R clashes with the Cursor Position Report), and F5..F12
    // keep their tilde codes.
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };

    assert_eq!(encode_key(Key::F(1), Modifiers::NONE, modes), b"\x1b[P");
    assert_eq!(encode_key(Key::F(2), Modifiers::NONE, modes), b"\x1b[Q");
    assert_eq!(encode_key(Key::F(3), Modifiers::NONE, modes), b"\x1b[13~");
    assert_eq!(encode_key(Key::F(4), Modifiers::NONE, modes), b"\x1b[S");
    assert_eq!(encode_key(Key::F(5), Modifiers::NONE, modes), b"\x1b[15~");
    assert_eq!(encode_key(Key::F(12), Modifiers::NONE, modes), b"\x1b[24~");
    assert_eq!(encode_key(Key::F(1), Modifiers::CTRL, modes), b"\x1b[1;5P");
    assert_eq!(encode_key(Key::F(3), Modifiers::CTRL, modes), b"\x1b[13;5~");

    let event_modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES,
        ..KeyModes::default()
    };
    assert_eq!(
        encode_key_event(
            Key::F(5),
            Modifiers::NONE,
            event_modes,
            KeyEventType::Release
        ),
        b"\x1b[15;1:3~"
    );
    assert_eq!(
        encode_key_event(
            Key::F(1),
            Modifiers::CTRL,
            event_modes,
            KeyEventType::Repeat
        ),
        b"\x1b[1;5:2P"
    );
}

#[test]
fn ctrl_punctuation_controls() {
    assert_eq!(ctrl_char('['), Some(0x1b));
    assert_eq!(ctrl_char(' '), Some(0));
    assert_eq!(ctrl_char('a'), Some(1));
    assert_eq!(ctrl_char('1'), None);
    // The classic NUL / DEL pair: Ctrl-@ (NUL, like Ctrl-Space) and
    // Ctrl-? (DEL) round out the xterm punctuation ladder.
    assert_eq!(ctrl_char('@'), Some(0x00));
    assert_eq!(ctrl_char('?'), Some(0x7f));
}

#[test]
fn kitty_flags_zero_preserves_legacy_bytes() {
    let legacy_modes = KeyModes::default();
    let kitty_zero = KeyModes {
        kitty_keyboard_flags: 0,
        ..KeyModes::default()
    };

    let cases = [
        (Key::Char('c'), Modifiers::CTRL),
        (Key::Char('b'), Modifiers::ALT),
        (Key::Up, Modifiers::NONE),
        (
            Key::Left,
            Modifiers {
                shift: true,
                alt: true,
                ctrl: true,
            },
        ),
        (Key::Enter, Modifiers::NONE),
        (Key::BackTab, Modifiers::NONE),
    ];

    for (key, mods) in cases {
        assert_eq!(
            encode_key(key, mods, kitty_zero),
            encode_key(key, mods, legacy_modes),
            "{key:?} {mods:?}"
        );
    }
}

#[test]
fn kitty_disambiguate_encodes_ambiguous_text_keys() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key(Key::Char('i'), Modifiers::CTRL, modes),
        b"\x1b[105;5u"
    );
    assert_eq!(
        encode_key(
            Key::Char('I'),
            Modifiers {
                shift: true,
                alt: false,
                ctrl: true,
            },
            modes
        ),
        b"\x1b[105;6u"
    );
    assert_eq!(
        encode_key(Key::Char('['), Modifiers::ALT, modes),
        b"\x1b[91;3u"
    );
    assert_eq!(
        encode_key(
            Key::Char('#'),
            Modifiers {
                shift: true,
                alt: false,
                ctrl: true,
            },
            modes
        ),
        b"\x1b[51;6u"
    );
}

#[test]
fn kitty_disambiguate_keeps_recovery_keys_legacy() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };

    assert_eq!(encode_key(Key::Enter, Modifiers::NONE, modes), b"\r");
    assert_eq!(encode_key(Key::Tab, Modifiers::NONE, modes), b"\t");
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::NONE, modes),
        vec![0x7f]
    );
}

#[test]
fn kitty_disambiguate_encodes_modified_recovery_keys() {
    // The recoverability carve-out covers only the unmodified keys: with
    // modifiers held, disambiguate mode must produce CSI-u forms so apps
    // can tell Ctrl+Enter from Enter and Shift+Enter from Enter.
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };
    let ctrl_shift = Modifiers {
        ctrl: true,
        alt: false,
        shift: true,
    };
    let shift = Modifiers {
        ctrl: false,
        alt: false,
        shift: true,
    };

    assert_eq!(
        encode_key(Key::Enter, Modifiers::CTRL, modes),
        b"\x1b[13;5u"
    );
    assert_eq!(encode_key(Key::Enter, shift, modes), b"\x1b[13;2u");
    assert_eq!(encode_key(Key::Enter, Modifiers::ALT, modes), b"\x1b[13;3u");
    assert_eq!(encode_key(Key::Enter, ctrl_shift, modes), b"\x1b[13;6u");
    assert_eq!(encode_key(Key::Tab, Modifiers::CTRL, modes), b"\x1b[9;5u");
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::CTRL, modes),
        b"\x1b[127;5u"
    );
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::ALT, modes),
        b"\x1b[127;3u"
    );
    assert_eq!(encode_key(Key::Backspace, shift, modes), b"\x1b[127;2u");
}

#[test]
fn modified_recovery_keys_use_compatible_legacy_forms_at_flags_zero() {
    // Outside an app-requested protocol, modified recovery keys use their
    // established VT forms. Ctrl+Backspace is BS so it stays distinct from
    // ordinary Backspace's DEL without sending CSI-u to arbitrary apps.
    let modes = KeyModes::default();
    let shift = Modifiers {
        ctrl: false,
        alt: false,
        shift: true,
    };

    assert_eq!(encode_key(Key::Enter, Modifiers::CTRL, modes), b"\r");
    assert_eq!(encode_key(Key::Enter, shift, modes), b"\r");
    assert_eq!(encode_key(Key::Tab, Modifiers::CTRL, modes), b"\t");
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::CTRL, modes),
        vec![0x08]
    );
    assert_eq!(encode_key(Key::Backspace, Modifiers::NONE, modes), b"\x7f");
}

#[test]
fn control_text_editing_forms_match_named_keys_in_legacy_and_kitty_modes() {
    let kitty = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };
    let cases = [
        ('\u{8}', Key::Backspace),
        ('\u{7f}', Key::Backspace),
        ('\t', Key::Tab),
        ('\r', Key::Enter),
        ('\n', Key::Enter),
        ('\u{1b}', Key::Esc),
    ];

    for modes in [KeyModes::default(), kitty] {
        for (reported, named) in cases {
            assert_eq!(
                encode_key(Key::Char(reported), Modifiers::CTRL, modes),
                encode_key(named, Modifiers::CTRL, modes),
                "control-text {reported:?} must encode like {named:?} in {modes:?}"
            );
        }
    }

    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    assert_eq!(
        encode_key(Key::Char('\t'), shift, KeyModes::default()),
        encode_key(Key::BackTab, shift, KeyModes::default())
    );
}

#[test]
fn ctrl_character_deliveries_never_silently_encode_to_zero_bytes() {
    let kitty = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };
    let mut reported = (0..=0x1f).filter_map(char::from_u32).collect::<Vec<_>>();
    reported.extend(['\u{7f}', '1', '.', 'é']);

    for modes in [KeyModes::default(), kitty] {
        for ch in &reported {
            assert!(
                !encode_key(Key::Char(*ch), Modifiers::CTRL, modes).is_empty(),
                "Ctrl Character({ch:?}) silently vanished in {modes:?}"
            );
        }
    }
}

#[test]
fn kitty_event_types_report_modified_recovery_key_lifecycle() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key_event(Key::Enter, Modifiers::CTRL, modes, KeyEventType::Press),
        b"\x1b[13;5u"
    );
    assert_eq!(
        encode_key_event(Key::Enter, Modifiers::CTRL, modes, KeyEventType::Repeat),
        b"\x1b[13;5:2u"
    );
    assert_eq!(
        encode_key_event(Key::Enter, Modifiers::CTRL, modes, KeyEventType::Release),
        b"\x1b[13;5:3u"
    );
    // The unmodified keys stay carved out even for release reporting.
    assert!(encode_key_event(Key::Enter, Modifiers::NONE, modes, KeyEventType::Release).is_empty());
    assert!(encode_key_event(Key::Tab, Modifiers::NONE, modes, KeyEventType::Release).is_empty());
}

#[test]
fn kitty_disambiguate_overrides_application_cursor_for_named_keys() {
    let modes = KeyModes {
        application_cursor: true,
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };

    assert_eq!(encode_key(Key::Up, Modifiers::NONE, modes), b"\x1b[A");
    assert_eq!(encode_key(Key::Right, Modifiers::CTRL, modes), b"\x1b[1;5C");
    assert_eq!(
        encode_key(Key::BackTab, Modifiers::NONE, modes),
        b"\x1b[9;2u"
    );
    assert_eq!(encode_key(Key::Esc, Modifiers::NONE, modes), b"\x1b[27u");
}

#[test]
fn kitty_report_all_keys_encodes_text_and_recovery_keys() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_REPORT_ALL_KEYS,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key(Key::Char('a'), Modifiers::NONE, modes),
        b"\x1b[97u"
    );
    assert_eq!(encode_key(Key::Enter, Modifiers::NONE, modes), b"\x1b[13u");
    assert_eq!(encode_key(Key::Tab, Modifiers::NONE, modes), b"\x1b[9u");
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::NONE, modes),
        b"\x1b[127u"
    );
}

#[test]
fn kitty_event_types_report_functional_repeat_and_release() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_REPORT_EVENT_TYPES,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key_event(Key::Up, Modifiers::NONE, modes, KeyEventType::Press),
        b"\x1b[A"
    );
    assert_eq!(
        encode_key_event(Key::Up, Modifiers::NONE, modes, KeyEventType::Repeat),
        b"\x1b[1;1:2A"
    );
    assert_eq!(
        encode_key_event(Key::Up, Modifiers::NONE, modes, KeyEventType::Release),
        b"\x1b[1;1:3A"
    );
    assert_eq!(
        encode_key_event(Key::Delete, Modifiers::NONE, modes, KeyEventType::Repeat),
        b"\x1b[3;1:2~"
    );
}

#[test]
fn kitty_release_events_require_event_type_flag() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };

    assert!(encode_key_event(Key::Up, Modifiers::NONE, modes, KeyEventType::Release).is_empty());
    assert!(
        encode_key_event(
            Key::Char('i'),
            Modifiers::CTRL,
            modes,
            KeyEventType::Release
        )
        .is_empty()
    );
}

#[test]
fn kitty_event_types_for_text_require_report_all_or_disambiguation() {
    let event_only = KeyModes {
        kitty_keyboard_flags: KITTY_REPORT_EVENT_TYPES,
        ..KeyModes::default()
    };
    let report_all = KeyModes {
        kitty_keyboard_flags: KITTY_REPORT_EVENT_TYPES | KITTY_REPORT_ALL_KEYS,
        ..KeyModes::default()
    };
    let disambiguate = KeyModes {
        kitty_keyboard_flags: KITTY_REPORT_EVENT_TYPES | KITTY_DISAMBIGUATE,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key_event(
            Key::Char('a'),
            Modifiers::NONE,
            event_only,
            KeyEventType::Repeat
        ),
        b"a"
    );
    assert!(
        encode_key_event(
            Key::Char('a'),
            Modifiers::NONE,
            event_only,
            KeyEventType::Release
        )
        .is_empty()
    );
    assert_eq!(
        encode_key_event(
            Key::Char('a'),
            Modifiers::NONE,
            report_all,
            KeyEventType::Repeat
        ),
        b"\x1b[97;1:2u"
    );
    assert_eq!(
        encode_key_event(
            Key::Char('a'),
            Modifiers::NONE,
            report_all,
            KeyEventType::Release
        ),
        b"\x1b[97;1:3u"
    );
    assert_eq!(
        encode_key_event(
            Key::Char('i'),
            Modifiers::CTRL,
            disambiguate,
            KeyEventType::Repeat
        ),
        b"\x1b[105;5:2u"
    );
}

#[test]
fn kitty_alternate_keys_add_shifted_and_base_layout_fields() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_DISAMBIGUATE | KITTY_REPORT_ALTERNATE_KEYS,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key_event(
            Key::Char('#'),
            Modifiers {
                shift: true,
                alt: false,
                ctrl: true,
            },
            modes,
            KeyEventType::Press
        ),
        b"\x1b[51:35:51;6u"
    );
    assert_eq!(
        encode_key_event(
            Key::Char('I'),
            Modifiers {
                shift: true,
                alt: false,
                ctrl: true,
            },
            modes,
            KeyEventType::Press
        ),
        b"\x1b[105:73:105;6u"
    );
}

#[test]
fn kitty_associated_text_uses_third_parameter_with_report_all() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key_event(
            Key::Char('A'),
            Modifiers {
                shift: true,
                alt: false,
                ctrl: false,
            },
            modes,
            KeyEventType::Press
        ),
        b"\x1b[97;2;65u"
    );
    assert_eq!(
        encode_key_event(Key::Char('a'), Modifiers::NONE, modes, KeyEventType::Press),
        b"\x1b[97;;97u"
    );
    assert_eq!(
        encode_key_event(Key::Enter, Modifiers::NONE, modes, KeyEventType::Press),
        b"\x1b[13u"
    );
}

#[test]
fn kitty_associated_text_flag_alone_preserves_legacy_text() {
    let modes = KeyModes {
        kitty_keyboard_flags: KITTY_REPORT_ASSOCIATED_TEXT,
        ..KeyModes::default()
    };

    assert_eq!(
        encode_key_event(Key::Char('a'), Modifiers::NONE, modes, KeyEventType::Press),
        b"a"
    );
}

#[test]
fn encodes_plain_paste_without_brackets() {
    assert_eq!(encode_paste("abc\n", false), b"abc\n");
    assert_eq!(encode_paste("a\x1b[201~b", false), b"a\x1b[201~b");
}

#[test]
fn wraps_paste_when_bracketed_paste_is_enabled() {
    assert_eq!(encode_paste("abc", true), b"\x1b[200~abc\x1b[201~");
}

#[test]
fn strips_embedded_end_marker_from_bracketed_paste() {
    // A payload smuggling its own end marker must not break out of the guard.
    let encoded = encode_paste("safe\x1b[201~rm -rf /\r", true);

    assert_eq!(encoded, b"\x1b[200~saferm -rf /\r\x1b[201~");
    // Exactly one start and one end marker survive.
    assert_eq!(encoded.windows(6).filter(|w| *w == b"\x1b[201~").count(), 1);
    assert_eq!(encoded.windows(6).filter(|w| *w == b"\x1b[200~").count(), 1);
}

fn mok_modes(level: u8) -> KeyModes {
    KeyModes {
        modify_other_keys: level,
        ..KeyModes::default()
    }
}

const SHIFT: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: true,
};
const CTRL_SHIFT: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: true,
};

#[test]
fn modify_other_keys_level_two_encodes_modified_ordinary_keys() {
    // Fixtures follow xterm's "Other Modified Keys" table
    // (CSI 27 ; modifier ; codepoint ~): the codepoint is the produced
    // character's, so shifted punctuation reports the shifted glyph.
    let modes = mok_modes(2);

    // The well-known Ctrl combinations are encoded at level 2.
    assert_eq!(
        encode_key(Key::Char('c'), Modifiers::CTRL, modes),
        b"\x1b[27;5;99~"
    );
    assert_eq!(
        encode_key(Key::Char('i'), Modifiers::CTRL, modes),
        b"\x1b[27;5;105~"
    );
    assert_eq!(
        encode_key(Key::Char('b'), Modifiers::ALT, modes),
        b"\x1b[27;3;98~"
    );
    // Ctrl+Shift+letter carries the produced uppercase glyph.
    assert_eq!(
        encode_key(Key::Char('C'), CTRL_SHIFT, modes),
        b"\x1b[27;6;67~"
    );
    // Shifted punctuation: Ctrl+Shift+3 produces '#' (codepoint 35).
    assert_eq!(
        encode_key(Key::Char('#'), CTRL_SHIFT, modes),
        b"\x1b[27;6;35~"
    );
    assert_eq!(
        encode_key(Key::Char(';'), Modifiers::CTRL, modes),
        b"\x1b[27;5;59~"
    );
    // Modified Enter/Tab/Backspace encode at level 2.
    assert_eq!(
        encode_key(Key::Enter, Modifiers::CTRL, modes),
        b"\x1b[27;5;13~"
    );
    assert_eq!(encode_key(Key::Enter, SHIFT, modes), b"\x1b[27;2;13~");
    assert_eq!(
        encode_key(Key::Tab, Modifiers::CTRL, modes),
        b"\x1b[27;5;9~"
    );
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::CTRL, modes),
        b"\x1b[27;5;127~"
    );
}

#[test]
fn modify_other_keys_level_two_leaves_unmodified_and_shift_only_keys_legacy() {
    let modes = mok_modes(2);

    // Unmodified keys are never touched (mok modifies OTHER keys).
    assert_eq!(encode_key(Key::Char('a'), Modifiers::NONE, modes), b"a");
    assert_eq!(encode_key(Key::Enter, Modifiers::NONE, modes), b"\r");
    assert_eq!(encode_key(Key::Tab, Modifiers::NONE, modes), b"\t");
    // Shift alone on a printable is consumed producing the glyph - xterm
    // sends the plain character (the WezTerm/fish fallout zone).
    assert_eq!(encode_key(Key::Char('A'), SHIFT, modes), b"A");
    assert_eq!(encode_key(Key::Char('#'), SHIFT, modes), b"#");
    // Shift-Tab keeps kcbt.
    assert_eq!(encode_key(Key::BackTab, SHIFT, modes), b"\x1b[Z");
    // Cursor/navigation/function keys keep their xterm modifier forms.
    assert_eq!(encode_key(Key::Right, Modifiers::CTRL, modes), b"\x1b[1;5C");
    assert_eq!(
        encode_key(Key::Delete, Modifiers::CTRL, modes),
        b"\x1b[3;5~"
    );
    assert_eq!(encode_key(Key::F(5), Modifiers::CTRL, modes), b"\x1b[15;5~");
    // Escape stays raw.
    assert_eq!(encode_key(Key::Esc, Modifiers::CTRL, modes), b"\x1b");
}

#[test]
fn modify_other_keys_level_one_encodes_only_keys_without_legacy_encodings() {
    let modes = mok_modes(1);

    // Well-known combinations keep their legacy bytes at level 1.
    assert_eq!(encode_key(Key::Char('c'), Modifiers::CTRL, modes), vec![3]);
    assert_eq!(encode_key(Key::Char('b'), Modifiers::ALT, modes), b"\x1bb");
    assert_eq!(encode_key(Key::Enter, Modifiers::CTRL, modes), b"\r");
    assert_eq!(encode_key(Key::Tab, Modifiers::CTRL, modes), b"\t");
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::CTRL, modes),
        vec![0x08]
    );
    // Combinations that would otherwise lose their modifiers encode.
    assert_eq!(
        encode_key(Key::Char('1'), Modifiers::CTRL, modes),
        b"\x1b[27;5;49~"
    );
    assert_eq!(
        encode_key(Key::Char('.'), Modifiers::CTRL, modes),
        b"\x1b[27;5;46~"
    );
    assert_eq!(
        encode_key(Key::Char(';'), Modifiers::CTRL, modes),
        b"\x1b[27;5;59~"
    );
}

#[test]
fn modify_other_keys_has_no_event_types() {
    let modes = mok_modes(2);

    // Repeats encode like presses; releases produce nothing.
    assert_eq!(
        encode_key_event(Key::Enter, Modifiers::CTRL, modes, KeyEventType::Repeat),
        b"\x1b[27;5;13~"
    );
    assert!(encode_key_event(Key::Enter, Modifiers::CTRL, modes, KeyEventType::Release).is_empty());
}

#[test]
fn nonzero_kitty_flags_take_precedence_over_modify_other_keys() {
    // Table-driven precedence: kitty flags nonzero => CSI-u forms; kitty
    // flags zero + mok >= 1 => CSI 27~ forms; both zero => legacy. Apps
    // (fish) set both protocols; the kitty encoding must win.
    let cases: [(Key, Modifiers); 4] = [
        (Key::Enter, Modifiers::CTRL),
        (Key::Char('i'), Modifiers::CTRL),
        (Key::Char('#'), CTRL_SHIFT),
        (Key::Backspace, Modifiers::CTRL),
    ];
    for (key, mods) in cases {
        for mok in [0u8, 1, 2] {
            let kitty = KeyModes {
                kitty_keyboard_flags: KITTY_DISAMBIGUATE,
                modify_other_keys: mok,
                ..KeyModes::default()
            };
            let kitty_only = KeyModes {
                kitty_keyboard_flags: KITTY_DISAMBIGUATE,
                ..KeyModes::default()
            };
            assert_eq!(
                encode_key(key, mods, kitty),
                encode_key(key, mods, kitty_only),
                "kitty flags must win over mok {mok} for {key:?} {mods:?}"
            );
            assert!(
                encode_key(key, mods, kitty).starts_with(b"\x1b["),
                "{key:?} {mods:?} under kitty flags must be CSI-encoded"
            );
        }
    }
    // And at kitty flags 0, mok owns the encoding.
    assert_eq!(
        encode_key(Key::Enter, Modifiers::CTRL, mok_modes(2)),
        b"\x1b[27;5;13~"
    );
    // Both zero: legacy bytes.
    assert_eq!(
        encode_key(Key::Enter, Modifiers::CTRL, KeyModes::default()),
        b"\r"
    );
}

#[test]
fn win32_input_encodes_key_record_fields_and_event_lifecycle() {
    let modes = KeyModes {
        win32_input: true,
        ..KeyModes::default()
    };
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    let cases = [
        (
            Key::Backspace,
            Modifiers::NONE,
            KeyEventType::Press,
            b"\x1b[8;14;8;1;0;1_".as_slice(),
        ),
        (
            Key::Backspace,
            Modifiers::CTRL,
            KeyEventType::Press,
            b"\x1b[8;14;8;1;8;1_".as_slice(),
        ),
        (
            Key::Enter,
            shift,
            KeyEventType::Press,
            b"\x1b[13;28;13;1;16;1_".as_slice(),
        ),
        (
            Key::Char('a'),
            Modifiers::NONE,
            KeyEventType::Press,
            b"\x1b[65;30;97;1;0;1_".as_slice(),
        ),
        (
            Key::Char('a'),
            Modifiers::NONE,
            KeyEventType::Release,
            b"\x1b[65;30;97;0;0;1_".as_slice(),
        ),
    ];
    for (key, mods, event_type, expected) in cases {
        assert_eq!(encode_key_event(key, mods, modes, event_type), expected);
    }
}

#[test]
fn win32_input_precedes_kitty_and_modify_other_keys() {
    let modes = KeyModes {
        win32_input: true,
        kitty_keyboard_flags: KITTY_DISAMBIGUATE | KITTY_REPORT_EVENT_TYPES,
        modify_other_keys: 2,
        ..KeyModes::default()
    };
    assert_eq!(
        encode_key_event(
            Key::Backspace,
            Modifiers::CTRL,
            modes,
            KeyEventType::Release
        ),
        b"\x1b[8;14;8;0;8;1_"
    );
}

#[test]
fn disabled_win32_input_preserves_legacy_fallback() {
    assert_eq!(
        encode_key(Key::Backspace, Modifiers::CTRL, KeyModes::default()),
        vec![0x08]
    );
    assert_eq!(
        encode_key(Key::Enter, Modifiers::NONE, KeyModes::default()),
        b"\r"
    );
}

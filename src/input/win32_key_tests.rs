// SPDX-License-Identifier: GPL-3.0-only
//! Win32 input-mode (`CSI ? 9001 h`) key-record assertions.
//!
//! ConPTY consumes the full `CSI Vk;Sc;Uc;Kd;Cs;Rc _` record, so a wrong
//! virtual key, scan code, UTF-16 unit, modifier bit, or key-down flag is a
//! silent input defect on Windows rather than a visible failure. The neutral
//! mapping that builds those records is ordinary cross-platform Rust with no
//! `cfg` gate, so these assertions compile and execute on every supported
//! platform, including the blocking Windows leg.
//!
//! Each test states the arithmetic or match arm it pins. The identities are
//! the standard Win32 `VK_*` values and set-1 scan codes, written as literals
//! rather than recomputed from the implementation, so a change to a formula
//! cannot silently carry its expectation along with it.

use super::{Key, KeyEventType, KeyModes, Modifiers, encode_key_event};

/// Modes with Win32 input mode active and every other protocol left off, so a
/// record that fails to build falls through to an empty write rather than to
/// another encoder.
fn win32_modes() -> KeyModes {
    KeyModes {
        win32_input: true,
        ..KeyModes::default()
    }
}

/// Encode one press and render it as text for readable assertions.
fn press(key: Key, mods: Modifiers) -> String {
    record(key, mods, KeyEventType::Press)
}

fn record(key: Key, mods: Modifiers, event_type: KeyEventType) -> String {
    let bytes = encode_key_event(key, mods, win32_modes(), event_type);
    String::from_utf8(bytes).expect("Win32 key records are ASCII")
}

const SHIFT: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: true,
};

/// Function keys derive both halves of their identity by offset from a base.
///
/// Pins `0x6f + n` (virtual key) and `0x3a + n` (scan code) for F1-F10 against
/// substituting another operator for either addition: F5 would report 555/290
/// under multiplication and 106/53 under subtraction. F11 and F12 leave the
/// offset range and are pinned as their own arms.
#[test]
fn win32_function_keys_offset_virtual_key_and_scan_code_from_their_base() {
    assert_eq!(press(Key::F(1), Modifiers::NONE), "\x1b[112;59;0;1;0;1_");
    assert_eq!(press(Key::F(5), Modifiers::NONE), "\x1b[116;63;0;1;0;1_");
    assert_eq!(press(Key::F(10), Modifiers::NONE), "\x1b[121;68;0;1;0;1_");
    assert_eq!(press(Key::F(11), Modifiers::NONE), "\x1b[122;87;0;1;0;1_");
    assert_eq!(press(Key::F(12), Modifiers::NONE), "\x1b[123;88;0;1;0;1_");
    // Beyond F12 there is no Win32 identity, so no record is written at all.
    assert_eq!(press(Key::F(13), Modifiers::NONE), "");
}

/// Keypad digits carry a numpad virtual key, a positional scan code, and the
/// ASCII digit as their UTF-16 unit.
///
/// Pins `0x60 + digit` and `b'0' + digit`. Digit zero cannot separate addition
/// from subtraction, so the non-zero digits carry the assertion: keypad 5 would
/// report virtual key 480 or 91 and unicode 240 or 43 under a substituted
/// operator.
#[test]
fn win32_keypad_digits_carry_numpad_identity_and_ascii_unicode() {
    assert_eq!(
        press(Key::KeypadDigit(0), Modifiers::NONE),
        "\x1b[96;82;48;1;0;1_"
    );
    assert_eq!(
        press(Key::KeypadDigit(1), Modifiers::NONE),
        "\x1b[97;79;49;1;0;1_"
    );
    assert_eq!(
        press(Key::KeypadDigit(5), Modifiers::NONE),
        "\x1b[101;76;53;1;0;1_"
    );
    assert_eq!(
        press(Key::KeypadDigit(9), Modifiers::NONE),
        "\x1b[105;73;57;1;0;1_"
    );
}

/// The top-row digits are a match arm of their own, distinct from the keypad.
///
/// Pins the arm's existence, its `- '0'` index derivation, and its
/// `b'0' + index` virtual key. Deleting the arm drops the record entirely;
/// dividing instead of subtracting maps every digit onto one; adding overruns
/// the scan table.
#[test]
fn win32_top_row_digits_map_to_digit_virtual_keys_and_row_scan_codes() {
    assert_eq!(
        press(Key::Char('0'), Modifiers::NONE),
        "\x1b[48;11;48;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('1'), Modifiers::NONE),
        "\x1b[49;2;49;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('5'), Modifiers::NONE),
        "\x1b[53;6;53;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('9'), Modifiers::NONE),
        "\x1b[57;10;57;1;0;1_"
    );
}

/// A top-row digit and its keypad twin are different keys to ConPTY.
///
/// Both report the same UTF-16 unit, so only the virtual key and scan code
/// distinguish them; an application reading raw records relies on that.
#[test]
fn win32_top_row_and_keypad_digits_stay_distinguishable() {
    assert_ne!(
        press(Key::Char('7'), Modifiers::NONE),
        press(Key::KeypadDigit(7), Modifiers::NONE)
    );
    assert_eq!(
        press(Key::Char('7'), Modifiers::NONE),
        "\x1b[55;8;55;1;0;1_"
    );
    assert_eq!(
        press(Key::KeypadDigit(7), Modifiers::NONE),
        "\x1b[103;71;55;1;0;1_"
    );
}

/// Letters index a scan table by distance from `a` and offset `VK_A`.
///
/// Pins `u16::from(b'A') + index`. The first letter is a fixed point of both
/// addition and subtraction, so letters later in the alphabet carry the
/// assertion: `z` would report virtual key 40 under subtraction.
#[test]
fn win32_letter_identity_spans_the_alphabet() {
    assert_eq!(
        press(Key::Char('a'), Modifiers::NONE),
        "\x1b[65;30;97;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('m'), Modifiers::NONE),
        "\x1b[77;50;109;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('z'), Modifiers::NONE),
        "\x1b[90;44;122;1;0;1_"
    );
}

/// Case affects the reported UTF-16 unit but never the key identity.
///
/// The virtual key and scan code describe the physical key, so they follow the
/// lowercased character while the unicode field carries the character as typed.
#[test]
fn win32_uppercase_letters_keep_lowercase_key_identity() {
    assert_eq!(press(Key::Char('Z'), SHIFT), "\x1b[90;44;90;1;16;1_");
    assert_eq!(press(Key::Char('A'), SHIFT), "\x1b[65;30;65;1;16;1_");
}

/// Space is its own match arm with a virtual key and scan code shared with no
/// other key; deleting the arm silences the spacebar entirely.
#[test]
fn win32_space_has_a_dedicated_virtual_key_and_scan_code() {
    assert_eq!(
        press(Key::Char(' '), Modifiers::NONE),
        "\x1b[32;57;32;1;0;1_"
    );
}

/// Ctrl replaces the reported character with its control byte, and reports
/// zero when the chord has no classic control mapping.
///
/// This is the control/text boundary: the key identity is unchanged, only the
/// UTF-16 unit moves.
#[test]
fn win32_ctrl_substitutes_the_control_byte_for_the_unicode_unit() {
    assert_eq!(
        press(Key::Char('a'), Modifiers::CTRL),
        "\x1b[65;30;1;1;8;1_"
    );
    assert_eq!(
        press(Key::Char('z'), Modifiers::CTRL),
        "\x1b[90;44;26;1;8;1_"
    );
    // Ctrl+A and Ctrl+a are the same chord; the shifted form still reports 1.
    assert_eq!(
        press(Key::Char('A'), Modifiers::CTRL),
        "\x1b[65;30;1;1;8;1_"
    );
    // Ctrl+Space is NUL, which is a reported zero rather than a missing value.
    assert_eq!(
        press(Key::Char(' '), Modifiers::CTRL),
        "\x1b[32;57;0;1;8;1_"
    );
    // A digit has no classic control mapping, so the unit is zero while the
    // key identity survives intact.
    assert_eq!(press(Key::Char('5'), Modifiers::CTRL), "\x1b[53;6;0;1;8;1_");
}

/// Each modifier contributes its own control-key-state bit, accumulated rather
/// than replaced.
///
/// Pins the Alt bit independently of Ctrl and Shift: clearing instead of
/// setting leaves the field at zero, which a Ctrl-only assertion cannot see.
#[test]
fn win32_modifier_bits_accumulate_independently() {
    assert_eq!(
        press(Key::Char('a'), Modifiers::ALT),
        "\x1b[65;30;97;1;2;1_"
    );
    assert_eq!(press(Key::Char('a'), SHIFT), "\x1b[65;30;97;1;16;1_");
    assert_eq!(
        press(Key::Char('a'), Modifiers::CTRL),
        "\x1b[65;30;1;1;8;1_"
    );

    let ctrl_alt = Modifiers {
        ctrl: true,
        alt: true,
        shift: false,
    };
    assert_eq!(press(Key::Char('a'), ctrl_alt), "\x1b[65;30;1;1;10;1_");

    let all = Modifiers {
        ctrl: true,
        alt: true,
        shift: true,
    };
    assert_eq!(press(Key::Char('a'), all), "\x1b[65;30;1;1;26;1_");
}

/// Extended keys set the enhanced-key bit on top of any held modifiers.
///
/// Clearing instead of setting that bit would make every navigation key look
/// like a non-extended key to ConPTY.
#[test]
fn win32_extended_keys_set_the_enhanced_bit() {
    assert_eq!(press(Key::Left, Modifiers::NONE), "\x1b[37;75;0;1;256;1_");
    assert_eq!(press(Key::Up, Modifiers::NONE), "\x1b[38;72;0;1;256;1_");
    assert_eq!(press(Key::Right, Modifiers::NONE), "\x1b[39;77;0;1;256;1_");
    assert_eq!(press(Key::Down, Modifiers::NONE), "\x1b[40;80;0;1;256;1_");
    assert_eq!(press(Key::Home, Modifiers::NONE), "\x1b[36;71;0;1;256;1_");
    assert_eq!(press(Key::End, Modifiers::NONE), "\x1b[35;79;0;1;256;1_");
    assert_eq!(press(Key::PageUp, Modifiers::NONE), "\x1b[33;73;0;1;256;1_");
    assert_eq!(
        press(Key::PageDown, Modifiers::NONE),
        "\x1b[34;81;0;1;256;1_"
    );
    assert_eq!(press(Key::Insert, Modifiers::NONE), "\x1b[45;82;0;1;256;1_");
    assert_eq!(press(Key::Delete, Modifiers::NONE), "\x1b[46;83;0;1;256;1_");

    // The enhanced bit is additive with the modifier bits, not exclusive.
    let ctrl_shift = Modifiers {
        ctrl: true,
        alt: false,
        shift: true,
    };
    assert_eq!(press(Key::Left, ctrl_shift), "\x1b[37;75;0;1;280;1_");
}

/// Keypad Enter shares its virtual key and scan identity with main-block Enter
/// and is separated only by the enhanced bit.
///
/// This is the sharpest test of that bit: without it the two keys become
/// byte-identical records.
#[test]
fn win32_keypad_twins_differ_from_main_block_only_by_the_enhanced_bit() {
    assert_eq!(press(Key::Enter, Modifiers::NONE), "\x1b[13;28;13;1;0;1_");
    assert_eq!(
        press(Key::KeypadEnter, Modifiers::NONE),
        "\x1b[13;28;13;1;256;1_"
    );
    assert_ne!(
        press(Key::Enter, Modifiers::NONE),
        press(Key::KeypadEnter, Modifiers::NONE)
    );

    // Divide is enhanced; the other keypad operators are not.
    assert_eq!(
        press(Key::KeypadDivide, Modifiers::NONE),
        "\x1b[111;53;47;1;256;1_"
    );
    assert_eq!(
        press(Key::KeypadMultiply, Modifiers::NONE),
        "\x1b[106;55;42;1;0;1_"
    );
    assert_eq!(
        press(Key::KeypadSubtract, Modifiers::NONE),
        "\x1b[109;74;45;1;0;1_"
    );
    assert_eq!(
        press(Key::KeypadAdd, Modifiers::NONE),
        "\x1b[107;78;43;1;0;1_"
    );
    assert_eq!(
        press(Key::KeypadDecimal, Modifiers::NONE),
        "\x1b[110;83;46;1;0;1_"
    );
}

/// The key-down flag distinguishes press and repeat from release, and every
/// record carries a repeat count of one.
///
/// Winit reports each repeat as its own event, so a record never aggregates
/// them; the trailing count is a contract with ConPTY, not a placeholder.
#[test]
fn win32_records_carry_the_key_down_flag_and_a_unit_repeat_count() {
    assert_eq!(
        record(Key::Char('a'), Modifiers::NONE, KeyEventType::Press),
        "\x1b[65;30;97;1;0;1_"
    );
    assert_eq!(
        record(Key::Char('a'), Modifiers::NONE, KeyEventType::Repeat),
        "\x1b[65;30;97;1;0;1_"
    );
    assert_eq!(
        record(Key::Char('a'), Modifiers::NONE, KeyEventType::Release),
        "\x1b[65;30;97;0;0;1_"
    );

    // Release is reported for extended and named keys too: Win32 input mode is
    // a full event stream, unlike the legacy encoder which drops releases.
    assert_eq!(
        record(Key::Left, Modifiers::NONE, KeyEventType::Release),
        "\x1b[37;75;0;0;256;1_"
    );
    assert_eq!(
        record(Key::F(1), Modifiers::NONE, KeyEventType::Release),
        "\x1b[112;59;0;0;0;1_"
    );

    for event_type in [
        KeyEventType::Press,
        KeyEventType::Repeat,
        KeyEventType::Release,
    ] {
        assert!(
            record(Key::Char('a'), Modifiers::NONE, event_type).ends_with(";1_"),
            "every record ends with a repeat count of one"
        );
    }
}

/// Control-text keys are normalized to their named identity before the Win32
/// mapping runs, so a front end that reports Enter as carriage return still
/// produces the named-key record rather than a character record.
#[test]
fn win32_normalizes_control_text_to_named_key_records() {
    assert_eq!(
        press(Key::Char('\r'), Modifiers::NONE),
        "\x1b[13;28;13;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('\n'), Modifiers::NONE),
        "\x1b[13;28;13;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('\t'), Modifiers::NONE),
        "\x1b[9;15;9;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('\u{1b}'), Modifiers::NONE),
        "\x1b[27;1;27;1;0;1_"
    );
    assert_eq!(
        press(Key::Char('\u{7f}'), Modifiers::NONE),
        "\x1b[8;14;8;1;0;1_"
    );
    // Shift+Tab normalizes to BackTab, which shares Tab's Win32 identity and is
    // separated by the shift bit alone.
    assert_eq!(press(Key::Char('\t'), SHIFT), "\x1b[9;15;9;1;16;1_");
}

/// A character with no Win32 identity produces no record rather than a
/// malformed or partially populated one.
///
/// Win32 input mode takes precedence over every other encoder, so the absence
/// of a record here is the whole behavior: there is no fallback path that would
/// catch a wrongly admitted character.
#[test]
fn win32_declines_characters_without_a_win32_identity() {
    for ch in ['!', '-', '.', '/', '\u{e9}', '\u{20ac}', '\u{1}'] {
        assert_eq!(
            press(Key::Char(ch), Modifiers::NONE),
            "",
            "{ch:?} has no Win32 key identity"
        );
    }
}

/// Win32 input mode is consulted before Kitty and modifyOtherKeys, and a
/// declined key does not fall through to either.
#[test]
fn win32_precedence_holds_even_when_no_record_is_produced() {
    let modes = KeyModes {
        win32_input: true,
        kitty_keyboard_flags: 0b1111,
        modify_other_keys: 2,
        ..KeyModes::default()
    };
    assert!(
        encode_key_event(Key::Char('!'), Modifiers::CTRL, modes, KeyEventType::Press).is_empty()
    );
    // The same key with Win32 input mode off does reach another encoder, so the
    // empty result above is precedence rather than a universally dead key.
    let without = KeyModes {
        win32_input: false,
        ..modes
    };
    assert!(
        !encode_key_event(
            Key::Char('!'),
            Modifiers::CTRL,
            without,
            KeyEventType::Press
        )
        .is_empty()
    );
}

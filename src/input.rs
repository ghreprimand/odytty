// SPDX-License-Identifier: GPL-3.0-only
//! Source-agnostic keyboard encoding: the single source of truth for the byte
//! sequences OdyTTY sends to the PTY in response to key presses.
//!
//! Front ends produce key events in shapes owned by their input source. The
//! `winit` native window maps its event types onto the neutral [`Key`] /
//! [`Modifiers`] model here and calls [`encode_key`]. The encoder is
//! deliberately free of windowing and GPU dependencies.
//!
//! The byte sequences match the common DEC/xterm conventions a PTY shell
//! expects: `\r` for Enter, `0x7f` for Backspace, DEC/xterm cursor-key forms,
//! control bytes for Ctrl-letter, and xterm modifier encodings for named keys.
//! Quit/close affordances are intentionally *not* modeled here — those are an
//! interactive-front-end concern (e.g. the headless debug mode's Ctrl-Q); the
//! encoder only ever produces the bytes a real terminal would send.

/// A neutral, front-end-independent key identity.
///
/// Printable text is carried as [`Key::Char`]; everything else is a named key.
/// This mirrors the distinction `winit` draws between `Key::Character` and
/// `Key::Named`, so callers can map onto it without information loss for the
/// keys the prototype handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character (the front end has already resolved Shift/layout,
    /// so this is the actual glyph, e.g. `'A'` or `'@'`).
    Char(char),
    Enter,
    Backspace,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    Esc,
    /// Function key F1..=F12. Numbers outside that range produce no output;
    /// higher function keys stay reserved for OdyTTY's own chord system until
    /// a PTY encoding is defined for them.
    F(u8),
    /// Numeric keypad digit, distinguishable only when the front end exposes a
    /// physical keypad key.
    KeypadDigit(u8),
    KeypadDecimal,
    KeypadAdd,
    KeypadSubtract,
    KeypadMultiply,
    KeypadDivide,
    KeypadEnter,
}

/// Active modifier keys at the time of the press.
///
/// `shift` is included for completeness but is not consulted by [`encode_key`]:
/// front ends resolve Shift into the produced [`Key::Char`] glyph (and into
/// [`Key::BackTab`] for Shift-Tab) before encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    /// No modifiers held.
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };

    /// Just Ctrl held.
    pub const CTRL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
    };

    /// Just Alt held.
    pub const ALT: Self = Self {
        ctrl: false,
        alt: true,
        shift: false,
    };
}

/// Kind of keyboard event being encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    Press,
    Repeat,
    Release,
}

/// Terminal modes that affect keyboard encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// DECCKM: cursor keys use application SS3 forms when no xterm modifiers are
    /// present.
    pub application_cursor: bool,
    /// DECKPAM/DECKPNM: keypad keys use application keypad SS3 forms.
    pub application_keypad: bool,
    /// ConPTY Win32 input mode (`CSI ? 9001 h/l`). Native Windows input uses
    /// [`encode_win32_key_event`] while this is active, ahead of Kitty and
    /// modifyOtherKeys. Non-Windows front ends must leave this false.
    pub win32_input: bool,
    /// Kitty keyboard protocol progressive enhancement flags active for this
    /// screen. Consulted after Win32 input mode; zero preserves the legacy
    /// DEC/xterm encoder byte-for-byte.
    pub kitty_keyboard_flags: u16,
    /// xterm modifyOtherKeys level (0/1/2). Consulted only while
    /// Win32 input mode is off and `kitty_keyboard_flags` is zero. An app that
    /// enables Kitty and modifyOtherKeys (fish does) gets the Kitty encoding.
    /// Level 1 encodes modified keys that lack a legacy encoding; level 2
    /// encodes all modified keys as `CSI 27 ; modifier ; codepoint ~`.
    pub modify_other_keys: u8,
}

/// The fields of a Windows `KEY_EVENT_RECORD` carried by ConPTY's Win32 input
/// mode. Values are already narrowed to the protocol's 16-bit parameter range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Win32KeyEvent {
    pub virtual_key: u16,
    pub scan_code: u16,
    pub unicode_char: u16,
    pub control_key_state: u16,
}

/// Win32 `dwControlKeyState` bits used by the neutral and native mappers.
pub const WIN32_RIGHT_ALT: u16 = 0x0001;
pub const WIN32_LEFT_ALT: u16 = 0x0002;
pub const WIN32_RIGHT_CTRL: u16 = 0x0004;
pub const WIN32_LEFT_CTRL: u16 = 0x0008;
pub const WIN32_SHIFT: u16 = 0x0010;
pub const WIN32_ENHANCED_KEY: u16 = 0x0100;

/// Kitty keyboard protocol flag: send ambiguous keys as CSI-u forms.
pub const KITTY_DISAMBIGUATE: u16 = 0b1;
/// Kitty keyboard protocol flag: report press/repeat/release event types.
pub const KITTY_REPORT_EVENT_TYPES: u16 = 0b10;
/// Kitty keyboard protocol flag: add shifted and base-layout alternate keys.
pub const KITTY_REPORT_ALTERNATE_KEYS: u16 = 0b100;
/// Kitty keyboard protocol flag: report all keys as escape sequences.
pub const KITTY_REPORT_ALL_KEYS: u16 = 0b1000;
/// Kitty keyboard protocol flag: include generated text as code points.
pub const KITTY_REPORT_ASSOCIATED_TEXT: u16 = 0b10000;

/// Encode a key press into the bytes to write to the PTY.
///
/// Returns an empty vector when the key has no defined encoding; callers should
/// treat an empty result as "ignore". Alt prefixes printable characters with
/// `ESC`, matching xterm's meta-sends-escape convention; named keys carry Alt
/// through the xterm modifier table instead. Ctrl turns [`Key::Char`] letters
/// into control bytes and named keys into modified CSI forms. If a windowing
/// system has already translated a Ctrl chord into text, that text is retained
/// rather than silently discarded.
pub fn encode_key(key: Key, mods: Modifiers, modes: KeyModes) -> Vec<u8> {
    encode_key_event(key, mods, modes, KeyEventType::Press)
}

/// Encode a key event into the bytes to write to the PTY.
///
/// [`encode_key`] is the compatibility wrapper for press events. This variant
/// exposes repeat/release so native front ends can honor Kitty's event-type
/// progressive enhancement without changing legacy key behavior.
pub fn encode_key_event(
    key: Key,
    mods: Modifiers,
    modes: KeyModes,
    event_type: KeyEventType,
) -> Vec<u8> {
    // Some Wayland stacks report editing keys as their translated control-text
    // value instead of a named key. Normalize before every protocol decision so
    // Kitty, W32IM's neutral/synthetic path, and legacy VT all see the same key
    // identity. DEL is treated as Backspace here because it is the alternate
    // text form observed for that physical key; a named forward Delete remains
    // Key::Delete and keeps CSI 3~.
    let key = normalize_control_text_key(key, mods);

    // W32IM represents the complete Windows key event and therefore takes
    // precedence over both Kitty and modifyOtherKeys. The native Windows path
    // supplies physical VK/scan data directly; this neutral mapping keeps
    // synthesized terminal keys (alternate scroll, click-to-position) on the
    // same protocol.
    if modes.win32_input {
        return win32_event_from_neutral_key(key, mods)
            .map_or_else(Vec::new, |event| encode_win32_key_event(event, event_type));
    }

    if should_encode_kitty_key(key, mods, modes.kitty_keyboard_flags, event_type) {
        return encode_kitty_key(key, mods, modes.kitty_keyboard_flags, event_type);
    }

    if event_type == KeyEventType::Release {
        return Vec::new();
    }

    // xterm modifyOtherKeys: consulted only at kitty flags 0 (non-zero kitty
    // flags win — apps like fish enable both protocols and expect the kitty
    // encoding). No event types: releases were dropped above, repeats encode
    // like presses, matching xterm.
    if modes.kitty_keyboard_flags == 0
        && modes.modify_other_keys >= 1
        && let Some(bytes) = encode_modify_other_key(key, mods, modes.modify_other_keys)
    {
        return bytes;
    }

    let mut bytes = match key {
        // Legacy terminals conventionally distinguish Ctrl+Backspace from the
        // ordinary key as BS versus DEL. Keep that compatibility path when no
        // application keyboard protocol is active; Kitty/modifyOtherKeys were
        // already consulted above and retain their explicit modifier forms.
        Key::Backspace if mods.ctrl => vec![0x08],
        Key::Backspace => vec![0x7f],
        Key::Enter => b"\r".to_vec(),
        Key::Left => encode_cursor_key(b'D', mods, modes),
        Key::Right => encode_cursor_key(b'C', mods, modes),
        Key::Up => encode_cursor_key(b'A', mods, modes),
        Key::Down => encode_cursor_key(b'B', mods, modes),
        Key::Home => encode_cursor_key(b'H', mods, modes),
        Key::End => encode_cursor_key(b'F', mods, modes),
        Key::PageUp => encode_tilde_key(5, mods),
        Key::PageDown => encode_tilde_key(6, mods),
        Key::Tab => b"\t".to_vec(),
        Key::BackTab => b"\x1b[Z".to_vec(),
        Key::Delete => encode_tilde_key(3, mods),
        Key::Insert => encode_tilde_key(2, mods),
        Key::Esc => b"\x1b".to_vec(),
        Key::F(number) => encode_function_key(number, mods),
        Key::KeypadDigit(digit) => encode_keypad_digit(digit, modes),
        Key::KeypadDecimal => encode_keypad(b".", b"n", modes),
        Key::KeypadAdd => encode_keypad(b"+", b"k", modes),
        Key::KeypadSubtract => encode_keypad(b"-", b"m", modes),
        Key::KeypadMultiply => encode_keypad(b"*", b"j", modes),
        Key::KeypadDivide => encode_keypad(b"/", b"o", modes),
        Key::KeypadEnter => encode_keypad(b"\r", b"M", modes),
        Key::Char(ch) if mods.ctrl => encode_legacy_ctrl_char(ch),
        Key::Char(ch) => ch.to_string().into_bytes(),
    };

    if bytes.is_empty() {
        return bytes;
    }

    if mods.alt && matches!(key, Key::Char(_)) {
        bytes.insert(0, b'\x1b');
    }

    bytes
}

fn normalize_control_text_key(key: Key, mods: Modifiers) -> Key {
    match key {
        Key::Char('\u{8}' | '\u{7f}') => Key::Backspace,
        Key::Char('\t') if mods.shift => Key::BackTab,
        Key::Char('\t') => Key::Tab,
        Key::Char('\r' | '\n') => Key::Enter,
        Key::Char('\u{1b}') => Key::Esc,
        _ => key,
    }
}

fn encode_legacy_ctrl_char(ch: char) -> Vec<u8> {
    ctrl_char(ch).map_or_else(
        // A Character event is already layout/window-system translated text.
        // When no classic Ctrl mapping exists, forwarding that text matches
        // xterm's pass-through behavior and prevents a valid key event from
        // becoming an invisible zero-byte write.
        || ch.to_string().into_bytes(),
        |byte| vec![byte],
    )
}

/// Encode one Win32 input-mode key record as the full, explicit ConPTY form:
/// `CSI Vk;Sc;Uc;Kd;Cs;Rc _`.
///
/// Winit reports each repeat separately, so every sequence carries a repeat
/// count of one. Press and repeat are key-down records; release is key-up.
pub fn encode_win32_key_event(event: Win32KeyEvent, event_type: KeyEventType) -> Vec<u8> {
    let key_down = u8::from(event_type != KeyEventType::Release);
    format!(
        "\x1b[{};{};{};{};{};1_",
        event.virtual_key, event.scan_code, event.unicode_char, key_down, event.control_key_state
    )
    .into_bytes()
}

fn win32_event_from_neutral_key(key: Key, mods: Modifiers) -> Option<Win32KeyEvent> {
    let (virtual_key, scan_code, unicode_char, enhanced) = match key {
        Key::Backspace => (0x08, 0x0e, 0x08, false),
        Key::Tab | Key::BackTab => (0x09, 0x0f, 0x09, false),
        Key::Enter => (0x0d, 0x1c, 0x0d, false),
        Key::Esc => (0x1b, 0x01, 0x1b, false),
        Key::Left => (0x25, 0x4b, 0, true),
        Key::Up => (0x26, 0x48, 0, true),
        Key::Right => (0x27, 0x4d, 0, true),
        Key::Down => (0x28, 0x50, 0, true),
        Key::PageUp => (0x21, 0x49, 0, true),
        Key::PageDown => (0x22, 0x51, 0, true),
        Key::End => (0x23, 0x4f, 0, true),
        Key::Home => (0x24, 0x47, 0, true),
        Key::Insert => (0x2d, 0x52, 0, true),
        Key::Delete => (0x2e, 0x53, 0, true),
        Key::F(number @ 1..=10) => (0x6f + u16::from(number), 0x3a + u16::from(number), 0, false),
        Key::F(11) => (0x7a, 0x57, 0, false),
        Key::F(12) => (0x7b, 0x58, 0, false),
        Key::F(_) => return None,
        Key::KeypadDigit(digit @ 0..=9) => {
            let scans = [0x52, 0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48, 0x49];
            (
                0x60 + u16::from(digit),
                scans[usize::from(digit)],
                u16::from(b'0' + digit),
                false,
            )
        }
        Key::KeypadDigit(_) => return None,
        Key::KeypadMultiply => (0x6a, 0x37, u16::from(b'*'), false),
        Key::KeypadAdd => (0x6b, 0x4e, u16::from(b'+'), false),
        Key::KeypadSubtract => (0x6d, 0x4a, u16::from(b'-'), false),
        Key::KeypadDecimal => (0x6e, 0x53, u16::from(b'.'), false),
        Key::KeypadDivide => (0x6f, 0x35, u16::from(b'/'), true),
        Key::KeypadEnter => (0x0d, 0x1c, 0x0d, true),
        Key::Char(ch) => win32_char_identity(ch, mods)?,
    };

    let mut control_key_state = 0;
    if mods.ctrl {
        control_key_state |= WIN32_LEFT_CTRL;
    }
    if mods.alt {
        control_key_state |= WIN32_LEFT_ALT;
    }
    if mods.shift {
        control_key_state |= WIN32_SHIFT;
    }
    if enhanced {
        control_key_state |= WIN32_ENHANCED_KEY;
    }

    Some(Win32KeyEvent {
        virtual_key,
        scan_code,
        unicode_char,
        control_key_state,
    })
}

fn win32_char_identity(ch: char, mods: Modifiers) -> Option<(u16, u16, u16, bool)> {
    let lower = ch.to_ascii_lowercase();
    let (virtual_key, scan_code) = match lower {
        'a'..='z' => {
            const SCANS: [u16; 26] = [
                0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
                0x18, 0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
            ];
            let index = usize::try_from(u32::from(lower) - u32::from('a')).ok()?;
            (u16::from(b'A') + u16::try_from(index).ok()?, SCANS[index])
        }
        '0'..='9' => {
            const SCANS: [u16; 10] = [0x0b, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a];
            let index = usize::try_from(u32::from(lower) - u32::from('0')).ok()?;
            (u16::from(b'0') + u16::try_from(index).ok()?, SCANS[index])
        }
        ' ' => (0x20, 0x39),
        _ => return None,
    };
    let unicode_char = if mods.ctrl {
        ctrl_char(ch).map_or(0, u16::from)
    } else {
        u16::try_from(u32::from(ch)).ok()?
    };
    Some((virtual_key, scan_code, unicode_char, false))
}

fn should_encode_kitty_key(
    key: Key,
    mods: Modifiers,
    flags: u16,
    event_type: KeyEventType,
) -> bool {
    if event_type == KeyEventType::Release {
        if flags & KITTY_REPORT_EVENT_TYPES == 0 {
            return false;
        }
        if matches!(key, Key::Enter | Key::Tab | Key::Backspace)
            && flags & KITTY_REPORT_ALL_KEYS == 0
            && !is_kitty_escape_key(key, mods, flags)
        {
            return false;
        }
        return flags & KITTY_REPORT_ALL_KEYS != 0
            || is_kitty_escape_key(key, mods, flags)
            || is_functional_event_key(key);
    }

    if event_type == KeyEventType::Repeat
        && flags & KITTY_REPORT_EVENT_TYPES != 0
        && (flags & KITTY_REPORT_ALL_KEYS != 0
            || is_kitty_escape_key(key, mods, flags)
            || is_functional_event_key(key))
    {
        return true;
    }

    if flags & KITTY_REPORT_ALL_KEYS != 0 {
        return true;
    }
    if flags & KITTY_DISAMBIGUATE == 0 {
        return false;
    }

    is_kitty_escape_key(key, mods, flags)
}

fn is_functional_event_key(key: Key) -> bool {
    !matches!(key, Key::Char(_) | Key::Enter | Key::Tab | Key::Backspace)
}

fn is_kitty_escape_key(key: Key, mods: Modifiers, flags: u16) -> bool {
    if flags & KITTY_DISAMBIGUATE == 0 {
        return false;
    }

    match key {
        // The Kitty spec keeps these recoverable in disambiguation-only mode so
        // users can still type `reset` after a crashed app leaves the flag set.
        // The carve-out covers only the unmodified keys: with modifiers held,
        // kitty encodes them (Ctrl+Enter `CSI 13;5u`, Shift+Enter `CSI 13;2u`,
        // Ctrl+Backspace `CSI 127;5u`), which is what lets shells distinguish
        // Shift+Enter from Enter at the prompt.
        Key::Enter | Key::Tab | Key::Backspace => mods.ctrl || mods.alt || mods.shift,
        Key::Char(_) => mods.ctrl || mods.alt,
        Key::Esc | Key::BackTab => true,
        _ => true,
    }
}

fn encode_kitty_key(key: Key, mods: Modifiers, flags: u16, event_type: KeyEventType) -> Vec<u8> {
    let modifier = kitty_modifier(mods);
    match key {
        Key::Left => encode_kitty_final_key(b'D', modifier, flags, event_type),
        Key::Right => encode_kitty_final_key(b'C', modifier, flags, event_type),
        Key::Up => encode_kitty_final_key(b'A', modifier, flags, event_type),
        Key::Down => encode_kitty_final_key(b'B', modifier, flags, event_type),
        Key::Home => encode_kitty_final_key(b'H', modifier, flags, event_type),
        Key::End => encode_kitty_final_key(b'F', modifier, flags, event_type),
        Key::PageUp => encode_kitty_tilde_key(5, modifier, flags, event_type),
        Key::PageDown => encode_kitty_tilde_key(6, modifier, flags, event_type),
        Key::Delete => encode_kitty_tilde_key(3, modifier, flags, event_type),
        Key::Insert => encode_kitty_tilde_key(2, modifier, flags, event_type),
        Key::BackTab => encode_kitty_codepoint_key(
            KittyKeyCode::new(9),
            kitty_modifier(Modifiers {
                shift: true,
                ..mods
            }),
            flags,
            event_type,
            None,
        ),
        Key::Tab => {
            encode_kitty_codepoint_key(KittyKeyCode::new(9), modifier, flags, event_type, None)
        }
        Key::Enter => {
            encode_kitty_codepoint_key(KittyKeyCode::new(13), modifier, flags, event_type, None)
        }
        Key::Backspace => {
            encode_kitty_codepoint_key(KittyKeyCode::new(127), modifier, flags, event_type, None)
        }
        Key::Esc => {
            encode_kitty_codepoint_key(KittyKeyCode::new(27), modifier, flags, event_type, None)
        }
        // Functional-key table forms: F1/F2/F4 use the `CSI 1;mod [PQS]` final
        // letters (parameters omitted when unmodified), F3 uses `CSI 13~`
        // because `CSI R` clashes with the Cursor Position Report, and F5..F12
        // keep their xterm tilde codes.
        Key::F(1) => encode_kitty_final_key(b'P', modifier, flags, event_type),
        Key::F(2) => encode_kitty_final_key(b'Q', modifier, flags, event_type),
        Key::F(3) => encode_kitty_tilde_key(13, modifier, flags, event_type),
        Key::F(4) => encode_kitty_final_key(b'S', modifier, flags, event_type),
        Key::F(number) => match function_key_tilde_code(number) {
            Some(code) => encode_kitty_tilde_key(code, modifier, flags, event_type),
            None => Vec::new(),
        },
        Key::KeypadDigit(digit) if digit <= 9 => encode_kitty_codepoint_key(
            KittyKeyCode::new(57399 + digit as u32),
            modifier,
            flags,
            event_type,
            None,
        ),
        Key::KeypadDecimal => {
            encode_kitty_codepoint_key(KittyKeyCode::new(57409), modifier, flags, event_type, None)
        }
        Key::KeypadDivide => {
            encode_kitty_codepoint_key(KittyKeyCode::new(57410), modifier, flags, event_type, None)
        }
        Key::KeypadMultiply => {
            encode_kitty_codepoint_key(KittyKeyCode::new(57411), modifier, flags, event_type, None)
        }
        Key::KeypadSubtract => {
            encode_kitty_codepoint_key(KittyKeyCode::new(57412), modifier, flags, event_type, None)
        }
        Key::KeypadAdd => {
            encode_kitty_codepoint_key(KittyKeyCode::new(57413), modifier, flags, event_type, None)
        }
        Key::KeypadEnter => {
            encode_kitty_codepoint_key(KittyKeyCode::new(57414), modifier, flags, event_type, None)
        }
        Key::Char(ch) => {
            let key_code = KittyKeyCode::from_char(ch, mods, flags);
            let associated_text = kitty_associated_text(ch, flags);
            encode_kitty_codepoint_key(key_code, modifier, flags, event_type, associated_text)
        }
        Key::KeypadDigit(_) => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyKeyCode {
    primary: u32,
    shifted: Option<u32>,
    base_layout: Option<u32>,
}

impl KittyKeyCode {
    fn new(primary: u32) -> Self {
        Self {
            primary,
            shifted: None,
            base_layout: None,
        }
    }

    fn from_char(ch: char, mods: Modifiers, flags: u16) -> Self {
        let primary = kitty_char_code(ch);
        if flags & KITTY_REPORT_ALTERNATE_KEYS == 0 {
            return Self::new(primary);
        }

        let shifted = mods.shift.then_some(ch as u32);
        let base_layout = kitty_base_layout_code(ch);
        Self {
            primary,
            shifted,
            base_layout,
        }
    }

    fn parameter(self) -> String {
        match (self.shifted, self.base_layout) {
            (Some(shifted), Some(base_layout)) => {
                format!("{}:{shifted}:{base_layout}", self.primary)
            }
            (Some(shifted), None) => format!("{}:{shifted}", self.primary),
            (None, Some(base_layout)) => format!("{}::{base_layout}", self.primary),
            (None, None) => self.primary.to_string(),
        }
    }
}

fn encode_kitty_codepoint_key(
    key_code: KittyKeyCode,
    modifier: u8,
    flags: u16,
    event_type: KeyEventType,
    associated_text: Option<String>,
) -> Vec<u8> {
    let mut params = vec![key_code.parameter()];
    let modifier_field = kitty_modifier_field(modifier, flags, event_type);
    if modifier_field.is_some() || associated_text.is_some() {
        params.push(modifier_field.unwrap_or_default());
    }
    if let Some(text) = associated_text {
        params.push(text);
    }
    format!("\x1b[{}u", params.join(";")).into_bytes()
}

fn encode_kitty_final_key(
    final_byte: u8,
    modifier: u8,
    flags: u16,
    event_type: KeyEventType,
) -> Vec<u8> {
    let modifier_field = kitty_modifier_field(modifier, flags, event_type);
    if let Some(field) = modifier_field {
        format!("\x1b[1;{}{}", field, final_byte as char).into_bytes()
    } else {
        vec![b'\x1b', b'[', final_byte]
    }
}

fn encode_kitty_tilde_key(code: u8, modifier: u8, flags: u16, event_type: KeyEventType) -> Vec<u8> {
    let modifier_field = kitty_modifier_field(modifier, flags, event_type);
    if let Some(field) = modifier_field {
        format!("\x1b[{code};{field}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

fn kitty_modifier_field(modifier: u8, flags: u16, event_type: KeyEventType) -> Option<String> {
    let event_code = match event_type {
        KeyEventType::Press => None,
        KeyEventType::Repeat if flags & KITTY_REPORT_EVENT_TYPES != 0 => Some(2),
        KeyEventType::Release if flags & KITTY_REPORT_EVENT_TYPES != 0 => Some(3),
        KeyEventType::Repeat | KeyEventType::Release => None,
    };

    match (modifier, event_code) {
        (1, None) => None,
        (modifier, None) => Some(modifier.to_string()),
        (modifier, Some(event_code)) => Some(format!("{modifier}:{event_code}")),
    }
}

fn kitty_modifier(mods: Modifiers) -> u8 {
    1 + u8::from(mods.shift) + (u8::from(mods.alt) << 1) + (u8::from(mods.ctrl) << 2)
}

fn kitty_char_code(ch: char) -> u32 {
    let base = match ch {
        'A'..='Z' => ch.to_ascii_lowercase(),
        ')' => '0',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        '~' => '`',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => ch,
    };
    base as u32
}

fn kitty_base_layout_code(ch: char) -> Option<u32> {
    let base = kitty_char_code(ch);
    (base != ch as u32).then_some(base)
}

fn kitty_associated_text(ch: char, flags: u16) -> Option<String> {
    if flags & (KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT)
        != (KITTY_REPORT_ALL_KEYS | KITTY_REPORT_ASSOCIATED_TEXT)
    {
        return None;
    }
    let codepoint = ch as u32;
    if codepoint < 0x20 || (0x80..=0x9f).contains(&codepoint) {
        None
    } else {
        Some(codepoint.to_string())
    }
}

fn encode_cursor_key(final_byte: u8, mods: Modifiers, modes: KeyModes) -> Vec<u8> {
    if let Some(modifier) = xterm_modifier(mods) {
        format!("\x1b[1;{}{}", modifier, final_byte as char).into_bytes()
    } else if modes.application_cursor {
        vec![b'\x1b', b'O', final_byte]
    } else {
        vec![b'\x1b', b'[', final_byte]
    }
}

fn encode_tilde_key(code: u8, mods: Modifiers) -> Vec<u8> {
    if let Some(modifier) = xterm_modifier(mods) {
        format!("\x1b[{};{}~", code, modifier).into_bytes()
    } else {
        format!("\x1b[{}~", code).into_bytes()
    }
}

/// xterm modifyOtherKeys encoding: `CSI 27 ; modifier ; codepoint ~`.
///
/// Applies to "ordinary" keys — printable characters plus Enter/Tab/Backspace
/// (Escape stays raw `ESC`: xterm leaves it alone and TUIs depend on that).
/// Cursor, navigation, and function keys keep their xterm modifier encodings
/// unconditionally, and Shift-Tab keeps `CSI Z` (kcbt). Returns `None` when
/// the level does not claim the key, falling through to the legacy encoder.
///
/// Level semantics follow xterm:
/// - Level 1 encodes only modified keys that would otherwise lose their
///   modifiers entirely — combinations with a well-known legacy encoding
///   (Ctrl+letter control bytes, Alt's ESC prefix, unshifted printables,
///   modified Enter/Tab/Backspace) stay legacy.
/// - Level 2 encodes every modified ordinary key, including the well-known
///   cases, except Shift-as-the-only-modifier on a printable (Shift is
///   consumed producing the glyph; xterm sends the plain character).
///
/// The codepoint is the produced character's (shifted punctuation reports the
/// shifted codepoint — `Ctrl+Shift+3` is `CSI 27;6;35~`, `#` not `3`), and the
/// modifier parameter is the same 1+bitmask the CSI-u forms use.
fn encode_modify_other_key(key: Key, mods: Modifiers, level: u8) -> Option<Vec<u8>> {
    let has_mods = mods.ctrl || mods.alt || mods.shift;
    if !has_mods {
        return None;
    }

    let codepoint = match key {
        Key::Char(ch) => {
            // Shift alone is consumed producing the glyph.
            if !mods.ctrl && !mods.alt {
                return None;
            }
            if level == 1 && has_legacy_char_encoding(ch, mods) {
                return None;
            }
            ch as u32
        }
        // Modified Enter/Tab/Backspace have well-known legacy behavior, so
        // level 1 leaves them alone; level 2 encodes them.
        Key::Enter | Key::Tab | Key::Backspace if level >= 2 => match key {
            Key::Enter => 13,
            Key::Tab => 9,
            _ => 127,
        },
        _ => return None,
    };

    let modifier = kitty_modifier(mods);
    Some(format!("\x1b[27;{modifier};{codepoint}~").into_bytes())
}

/// Whether a modified printable already has a well-known legacy encoding that
/// modifyOtherKeys level 1 must preserve: Ctrl combinations that map to a
/// control byte, and Alt-only combinations (the ESC prefix carries Alt).
fn has_legacy_char_encoding(ch: char, mods: Modifiers) -> bool {
    if mods.ctrl {
        return ctrl_char(ch).is_some();
    }
    // Alt without Ctrl: the ESC-prefix convention encodes it.
    mods.alt
}

/// Legacy (non-kitty) function-key encoding: F1..F4 send the DEC SS3 forms
/// `ESC O P..S` when unmodified and the xterm modifier forms `CSI 1;mod P..S`
/// otherwise; F5..F12 use the xterm tilde codes with the shared modified-tilde
/// encoding. Numbers outside 1..=12 produce no output.
fn encode_function_key(number: u8, mods: Modifiers) -> Vec<u8> {
    match number {
        1..=4 => {
            let final_byte = b'P' + (number - 1);
            if let Some(modifier) = xterm_modifier(mods) {
                format!("\x1b[1;{}{}", modifier, final_byte as char).into_bytes()
            } else {
                vec![b'\x1b', b'O', final_byte]
            }
        }
        _ => match function_key_tilde_code(number) {
            Some(code) => encode_tilde_key(code, mods),
            None => Vec::new(),
        },
    }
}

/// xterm tilde codes for F5..F12 (the numbering skips 16 and 22).
fn function_key_tilde_code(number: u8) -> Option<u8> {
    match number {
        5 => Some(15),
        6 => Some(17),
        7 => Some(18),
        8 => Some(19),
        9 => Some(20),
        10 => Some(21),
        11 => Some(23),
        12 => Some(24),
        _ => None,
    }
}

fn xterm_modifier(mods: Modifiers) -> Option<u8> {
    let mut modifier = 1;
    if mods.shift {
        modifier += 1;
    }
    if mods.alt {
        modifier += 2;
    }
    if mods.ctrl {
        modifier += 4;
    }

    (modifier != 1).then_some(modifier)
}

fn encode_keypad_digit(digit: u8, modes: KeyModes) -> Vec<u8> {
    if digit > 9 {
        return Vec::new();
    }
    encode_keypad(&[b'0' + digit], &[b'p' + digit], modes)
}

fn encode_keypad(normal: &[u8], application_final: &[u8], modes: KeyModes) -> Vec<u8> {
    if modes.application_keypad {
        let mut bytes = b"\x1bO".to_vec();
        bytes.extend_from_slice(application_final);
        bytes
    } else {
        normal.to_vec()
    }
}

/// Map a character to its ASCII control byte, if one exists.
///
/// Covers Ctrl-A..Z (case-insensitive) and the classic punctuation controls
/// (`Ctrl-@`, `Ctrl-[`, `Ctrl-\`, `Ctrl-]`, `Ctrl-^`, `Ctrl-_`, `Ctrl-?`,
/// `Ctrl-Space`). Anything without a classic mapping returns `None`; the legacy
/// encoder forwards the window system's translated character unchanged.
pub fn ctrl_char(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some((ch as u8) - b'a' + 1),
        'A'..='Z' => Some((ch as u8) - b'A' + 1),
        // Ctrl-@ is NUL (the xterm/VT classic alongside Ctrl-Space); Ctrl-?
        // is DEL. Both were missing while every neighboring punctuation
        // control was mapped.
        '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        ' ' => Some(0),
        _ => None,
    }
}

/// Encode pasted text for the PTY.
///
/// When bracketed paste mode is active, the payload is wrapped in the xterm
/// bracketed-paste guard and any embedded end marker is removed so pasted text
/// cannot break out of the guard early. When bracketed paste is inactive, bytes
/// are sent unchanged to preserve the plain terminal behavior.
pub fn encode_paste(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if bracketed_paste {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(&sanitize_paste(text.as_bytes()));
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.as_bytes().to_vec()
    }
}

/// Strip any embedded bracketed-paste end marker from pasted bytes. Without
/// this, a crafted clipboard payload containing `ESC [ 2 0 1 ~` would close the
/// paste guard early and inject its tail as live keystrokes/commands.
///
/// Deleting a match can bring the surrounding bytes together to form a NEW
/// match, so a naive forward scan does not converge: `ESC[2` + `ESC[201~` +
/// `01~` collapses to exactly `ESC[201~`, re-emitting the marker it exists to
/// remove. This scan checks the growing OUTPUT tail after every byte, so a
/// marker reassembled by a prior deletion is caught in the same pass. The
/// result is a fixed point in one linear pass: `sanitize_paste(sanitize_paste(x))
/// == sanitize_paste(x)` and no `ESC[201~` survives in the output.
pub fn sanitize_paste(text: &[u8]) -> Vec<u8> {
    const END: &[u8] = b"\x1b[201~";
    let mut output = Vec::with_capacity(text.len());
    for &byte in text {
        output.push(byte);
        if output.ends_with(END) {
            output.truncate(output.len() - END.len());
        }
    }
    output
}

#[cfg(test)]
mod win32_key_tests;

#[cfg(test)]
mod paste_tests;

#[cfg(test)]
mod tests {
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
        assert!(
            encode_key_event(Key::Enter, Modifiers::NONE, modes, KeyEventType::Release).is_empty()
        );
        assert!(
            encode_key_event(Key::Tab, Modifiers::NONE, modes, KeyEventType::Release).is_empty()
        );
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

        assert!(
            encode_key_event(Key::Up, Modifiers::NONE, modes, KeyEventType::Release).is_empty()
        );
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
        // Shift alone on a printable is consumed producing the glyph — xterm
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
        assert!(
            encode_key_event(Key::Enter, Modifiers::CTRL, modes, KeyEventType::Release).is_empty()
        );
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
}

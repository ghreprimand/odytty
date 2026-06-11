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

/// Terminal modes that affect keyboard encoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModes {
    /// DECCKM: cursor keys use application SS3 forms when no xterm modifiers are
    /// present.
    pub application_cursor: bool,
    /// DECKPAM/DECKPNM: keypad keys use application keypad SS3 forms.
    pub application_keypad: bool,
}

/// Encode a key press into the bytes to write to the PTY.
///
/// Returns an empty vector when the key produces no output (e.g. a bare
/// Ctrl with a character that has no control mapping); callers should treat an
/// empty result as "ignore". Alt prefixes printable characters with `ESC`,
/// matching xterm's meta-sends-escape convention; named keys carry Alt through
/// the xterm modifier table instead. Ctrl turns [`Key::Char`] letters into
/// control bytes and named keys into modified CSI forms.
pub fn encode_key(key: Key, mods: Modifiers, modes: KeyModes) -> Vec<u8> {
    let mut bytes = match key {
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
        Key::KeypadDigit(digit) => encode_keypad_digit(digit, modes),
        Key::KeypadDecimal => encode_keypad(b".", b"n", modes),
        Key::KeypadAdd => encode_keypad(b"+", b"k", modes),
        Key::KeypadSubtract => encode_keypad(b"-", b"m", modes),
        Key::KeypadMultiply => encode_keypad(b"*", b"j", modes),
        Key::KeypadDivide => encode_keypad(b"/", b"o", modes),
        Key::KeypadEnter => encode_keypad(b"\r", b"M", modes),
        Key::Char(ch) if mods.ctrl => ctrl_char(ch).map_or_else(Vec::new, |byte| vec![byte]),
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
/// (`Ctrl-[`, `Ctrl-\`, `Ctrl-]`, `Ctrl-^`, `Ctrl-_`, `Ctrl-Space`). Anything
/// without a control mapping returns `None`, which [`encode_key`] treats as no
/// output.
pub fn ctrl_char(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some((ch as u8) - b'a' + 1),
        'A'..='Z' => Some((ch as u8) - b'A' + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
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
pub fn sanitize_paste(text: &[u8]) -> Vec<u8> {
    const END: &[u8] = b"\x1b[201~";
    let mut output = Vec::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text[index..].starts_with(END) {
            index += END.len();
        } else {
            output.push(text[index]);
            index += 1;
        }
    }
    output
}

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
    fn ctrl_without_mapping_is_ignored() {
        // A digit has no control byte; encode produces nothing (ignore).
        assert!(encode_key(Key::Char('1'), Modifiers::CTRL, KeyModes::default()).is_empty());
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
    fn ctrl_punctuation_controls() {
        assert_eq!(ctrl_char('['), Some(0x1b));
        assert_eq!(ctrl_char(' '), Some(0));
        assert_eq!(ctrl_char('a'), Some(1));
        assert_eq!(ctrl_char('1'), None);
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
}

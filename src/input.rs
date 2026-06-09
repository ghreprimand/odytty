//! Source-agnostic keyboard encoding: the single source of truth for the byte
//! sequences OdyTTY sends to the PTY in response to key presses.
//!
//! Two front ends produce key events in two different, incompatible shapes:
//! the crossterm-driven interactive debug mode (`crate::app`) and the `winit`
//! native window (`crate::native`). Rather than duplicate the escape-sequence
//! table in both — which guarantees drift — each front end maps its own event
//! type onto the neutral [`Key`] / [`Modifiers`] model here and calls
//! [`encode_key`]. The encoder is deliberately free of any windowing, GPU, or
//! crossterm dependency so both callers depend on it without depending on each
//! other.
//!
//! The byte sequences match the common DEC/xterm conventions a PTY shell
//! expects: `\r` for Enter, `0x7f` for Backspace, `ESC [ A..D` for arrows,
//! control bytes for Ctrl-letter, and an `ESC` prefix for Alt-modified keys.
//! Quit/close affordances are intentionally *not* modeled here — those are an
//! interactive-front-end concern (e.g. the crossterm debug mode's Ctrl-Q); the
//! encoder only ever produces the bytes a real terminal would send.

/// A neutral, front-end-independent key identity.
///
/// Printable text is carried as [`Key::Char`]; everything else is a named key.
/// This mirrors the distinction `winit` draws between `Key::Character` and
/// `Key::Named`, and the one crossterm draws between `KeyCode::Char` and its
/// named variants, so both callers map onto it without information loss for the
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
}

/// Active modifier keys at the time of the press.
///
/// `shift` is included for completeness but is not consulted by [`encode_key`]:
/// front ends resolve Shift into the produced [`Key::Char`] glyph (and into
/// [`Key::BackTab`] for Shift-Tab) before encoding, matching the prior
/// crossterm behavior.
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

/// Encode a key press into the bytes to write to the PTY.
///
/// Returns an empty vector when the key produces no output (e.g. a bare
/// Ctrl with a character that has no control mapping); callers should treat an
/// empty result as "ignore". An Alt modifier prefixes the sequence with `ESC`,
/// matching xterm's meta-sends-escape convention. Ctrl applies only to
/// [`Key::Char`] (turning a letter into its control byte); named keys ignore
/// Ctrl here, preserving the prior crossterm behavior.
pub fn encode_key(key: Key, mods: Modifiers) -> Vec<u8> {
    let mut bytes = match key {
        Key::Backspace => vec![0x7f],
        Key::Enter => b"\r".to_vec(),
        Key::Left => b"\x1b[D".to_vec(),
        Key::Right => b"\x1b[C".to_vec(),
        Key::Up => b"\x1b[A".to_vec(),
        Key::Down => b"\x1b[B".to_vec(),
        Key::Home => b"\x1b[H".to_vec(),
        Key::End => b"\x1b[F".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::BackTab => b"\x1b[Z".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Insert => b"\x1b[2~".to_vec(),
        Key::Esc => b"\x1b".to_vec(),
        Key::Char(ch) if mods.ctrl => ctrl_char(ch).map_or_else(Vec::new, |byte| vec![byte]),
        Key::Char(ch) => ch.to_string().into_bytes(),
    };

    if bytes.is_empty() {
        return bytes;
    }

    if mods.alt {
        bytes.insert(0, b'\x1b');
    }

    bytes
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
        assert_eq!(encode_key(Key::Char('a'), Modifiers::NONE), b"a");
        assert_eq!(encode_key(Key::Char('Z'), Modifiers::NONE), b"Z");
        assert_eq!(encode_key(Key::Char('@'), Modifiers::NONE), b"@");
    }

    #[test]
    fn encodes_enter_and_backspace() {
        assert_eq!(encode_key(Key::Enter, Modifiers::NONE), b"\r");
        assert_eq!(encode_key(Key::Backspace, Modifiers::NONE), vec![0x7f]);
    }

    #[test]
    fn encodes_arrows_and_named_keys() {
        assert_eq!(encode_key(Key::Up, Modifiers::NONE), b"\x1b[A");
        assert_eq!(encode_key(Key::Down, Modifiers::NONE), b"\x1b[B");
        assert_eq!(encode_key(Key::Right, Modifiers::NONE), b"\x1b[C");
        assert_eq!(encode_key(Key::Left, Modifiers::NONE), b"\x1b[D");
        assert_eq!(encode_key(Key::Home, Modifiers::NONE), b"\x1b[H");
        assert_eq!(encode_key(Key::End, Modifiers::NONE), b"\x1b[F");
        assert_eq!(encode_key(Key::Delete, Modifiers::NONE), b"\x1b[3~");
        assert_eq!(encode_key(Key::BackTab, Modifiers::NONE), b"\x1b[Z");
    }

    #[test]
    fn encodes_control_letters() {
        // Ctrl-C -> 0x03, Ctrl-D -> 0x04.
        assert_eq!(encode_key(Key::Char('c'), Modifiers::CTRL), vec![3]);
        assert_eq!(encode_key(Key::Char('d'), Modifiers::CTRL), vec![4]);
        // Case-insensitive.
        assert_eq!(encode_key(Key::Char('C'), Modifiers::CTRL), vec![3]);
    }

    #[test]
    fn ctrl_without_mapping_is_ignored() {
        // A digit has no control byte; encode produces nothing (ignore).
        assert!(encode_key(Key::Char('1'), Modifiers::CTRL).is_empty());
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(encode_key(Key::Char('b'), Modifiers::ALT), b"\x1bb");
        // Alt also prefixes named-key sequences.
        assert_eq!(encode_key(Key::Left, Modifiers::ALT), b"\x1b\x1b[D");
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

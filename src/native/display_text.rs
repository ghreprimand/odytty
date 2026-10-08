// SPDX-License-Identifier: GPL-3.0-only
//! Shared rules for showing untrusted or user-authored text in overlay rows:
//! desktop-entry names, session titles, workspace and profile names, host
//! aliases, palette labels, and the paste confirmation preview.
//!
//! Overlay rows are painted in logical order with no bidirectional
//! reordering, so a direction override or isolate in a label cannot reorder
//! anything there; it would only take up a cell as an invisible character, and
//! a zero-width space or byte-order mark would hide inside a name. These are
//! dropped from labels and shown as escapes in the paste preview. The joiners
//! (U+200C, U+200D) and emoji tag characters stay: emoji sequences and several
//! scripts need them.

/// An invisible format character (general category Cf) that no overlay label
/// needs: the soft hyphen, the Arabic letter mark, the Mongolian vowel
/// separator, zero-width space, the left-to-right and right-to-left marks,
/// direction embeddings, overrides and isolates, word joiner and invisible
/// operators, the byte-order mark, interlinear annotation controls, shorthand
/// format controls, musical symbol format controls, and the language tag.
pub(in crate::native) fn is_hidden_format_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x00AD
            | 0x061C
            | 0x180E
            | 0x200B
            | 0x200E..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
    )
}

/// A direction embedding, override, or isolate control (U+202A..U+202E,
/// U+2066..U+2069). Text carrying one can display in a different order from
/// the bytes a program receives.
pub(in crate::native) fn is_bidi_override_or_isolate(ch: char) -> bool {
    matches!(ch as u32, 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Text for one overlay row: control characters and hidden format characters
/// removed, so a malformed name can never inject escape sequences or hide
/// characters in a picker row.
pub(in crate::native) fn sanitize_row_text(text: &str) -> String {
    text.chars()
        .filter(|&ch| !ch.is_control() && !is_hidden_format_char(ch))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_text_drops_controls_and_hidden_format_characters() {
        assert_eq!(
            sanitize_row_text("a\u{1b}[31mb\u{202e}c\u{2066}d\u{2069}\u{200b}e\u{feff}f\u{ad}g"),
            "a[31mbcdefg"
        );
    }

    #[test]
    fn row_text_keeps_joiners_and_emoji_tags() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let persian = "\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}";
        let scotland = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}";
        for text in [family, persian, scotland, "caf\u{e9}", "e\u{301}"] {
            assert_eq!(sanitize_row_text(text), text);
        }
    }
}

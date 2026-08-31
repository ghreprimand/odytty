// SPDX-License-Identifier: GPL-3.0-only
//! Cross-platform policy for text entering the terminal through a paste source.
//!
//! Classification always examines the original text, before the existing PTY
//! encoder normalizes line endings. The policy is deliberately structural: it
//! recognizes line breaks and control characters, never shell commands.
//! The user-facing contract and trigger matrix live in
//! `docs/features.md#paste-safety`.

/// Maximum escaped preview retained by the confirmation UI. The limit applies
/// to rendered UTF-8 bytes after escaping, so hostile control-heavy input cannot
/// expand into unbounded chrome.
pub(in crate::native) const MAX_ESCAPED_PREVIEW_BYTES: usize = 512;

/// Maximum allocation allowed for the optional lossless one-line encoding.
const MAX_ONE_LINE_PASTE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum PasteSource {
    Clipboard,
    Primary,
    /// Reserved for the external text-drop route when that platform event is
    /// added. Keeping the source in this policy prevents a future bypass.
    ExternalTextDrop,
    /// Reserved for explicitly authorized local automation paste requests.
    Automation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct PasteAssessment {
    pub(in crate::native) risky: bool,
    pub(in crate::native) line_count: usize,
    pub(in crate::native) byte_count: usize,
    pub(in crate::native) escaped_preview: String,
    pub(in crate::native) preview_truncated: bool,
    pub(in crate::native) one_line_available: bool,
}

/// Inspect original source text. A paste is risky when it contains a CR/LF line
/// break or a Unicode control character other than tab. Tabs alone remain on
/// the historical direct path.
pub(in crate::native) fn assess(text: &str) -> PasteAssessment {
    let multiline = text.contains(['\r', '\n']);
    let disallowed_control = text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n'));
    let (escaped_preview, preview_truncated) = escaped_preview(text);
    PasteAssessment {
        risky: multiline || disallowed_control,
        line_count: logical_line_count(text),
        byte_count: text.len(),
        escaped_preview,
        preview_truncated,
        one_line_available: multiline
            && !disallowed_control
            && one_line_encoded_len(text).is_some(),
    }
}

/// Encode line endings and backslashes as visible ASCII escapes. Doubling an
/// existing backslash makes the mapping reversible, so this explicit one-line
/// variant never drops or merges source bytes. It is unavailable for other
/// controls and for output that would exceed the allocation ceiling.
pub(in crate::native) fn lossless_one_line(text: &str) -> Option<String> {
    let capacity = one_line_encoded_len(text)?;
    if !text.contains(['\r', '\n'])
        || text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\r' | '\n'))
    {
        return None;
    }
    let mut output = String::with_capacity(capacity);
    for ch in text.chars() {
        match ch {
            '\r' => output.push_str("\\r"),
            '\n' => output.push_str("\\n"),
            '\\' => output.push_str("\\\\"),
            _ => output.push(ch),
        }
    }
    Some(output)
}

fn one_line_encoded_len(text: &str) -> Option<usize> {
    let mut len = 0usize;
    for ch in text.chars() {
        let addition = match ch {
            '\r' | '\n' | '\\' => 2,
            _ => ch.len_utf8(),
        };
        len = len.checked_add(addition)?;
        if len > MAX_ONE_LINE_PASTE_BYTES {
            return None;
        }
    }
    Some(len)
}

fn logical_line_count(text: &str) -> usize {
    let mut lines = 1usize;
    let mut bytes = text.as_bytes().iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match byte {
            b'\r' => {
                if bytes.peek() == Some(&b'\n') {
                    bytes.next();
                }
                lines = lines.saturating_add(1);
            }
            b'\n' => lines = lines.saturating_add(1),
            _ => {}
        }
    }
    lines
}

fn escaped_preview(text: &str) -> (String, bool) {
    let mut output = String::new();
    for ch in text.chars() {
        let escaped = match ch {
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\t' => "\\t".to_owned(),
            '\\' => "\\\\".to_owned(),
            ch if ch.is_control() && (ch as u32) <= 0xff => format!("\\x{:02X}", ch as u32),
            ch if ch.is_control() => format!("\\u{{{:X}}}", ch as u32),
            ch => ch.to_string(),
        };
        if output.len().saturating_add(escaped.len()) > MAX_ESCAPED_PREVIEW_BYTES {
            return (output, true);
        }
        output.push_str(&escaped);
    }
    (output, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_uses_original_line_endings_and_controls() {
        for text in [
            "a\rb",
            "a\nb",
            "a\r\nb",
            "\u{1b}[31m",
            "a\u{7f}b",
            "a\u{85}b",
        ] {
            assert!(assess(text).risky, "{text:?}");
        }
        for text in ["", "one line", "tab\there", "snowman \u{2603}"] {
            assert!(!assess(text).risky, "{text:?}");
        }
    }

    #[test]
    fn line_counts_treat_crlf_as_one_break_and_keep_empty_lines() {
        assert_eq!(assess("").line_count, 1);
        assert_eq!(assess("a\r\nb").line_count, 2);
        assert_eq!(assess("a\r\n\r\nb").line_count, 3);
        assert_eq!(assess("\n\n").line_count, 3);
    }

    #[test]
    fn preview_is_escaped_and_bounded() {
        let assessment = assess("a\n\t\u{1b}\\b");
        assert_eq!(assessment.escaped_preview, "a\\n\\t\\x1B\\\\b");
        let large = assess(&"\u{1b}".repeat(MAX_ESCAPED_PREVIEW_BYTES));
        assert!(large.preview_truncated);
        assert!(large.escaped_preview.len() <= MAX_ESCAPED_PREVIEW_BYTES);
    }

    #[test]
    fn one_line_variant_is_explicit_reversible_escaping() {
        let input = "one\\path\r\ntwo\n";
        let assessment = assess(input);
        assert!(assessment.one_line_available);
        assert_eq!(
            lossless_one_line(input).as_deref(),
            Some("one\\\\path\\r\\ntwo\\n")
        );
        assert!(lossless_one_line("one line").is_none());
        assert!(lossless_one_line("a\n\u{1b}").is_none());
    }

    #[test]
    fn all_declared_sources_share_the_same_source_type() {
        let sources = [
            PasteSource::Clipboard,
            PasteSource::Primary,
            PasteSource::ExternalTextDrop,
            PasteSource::Automation,
        ];
        assert_eq!(sources.len(), 4);
    }
}

//! Unit tests for the partial UTF-8 continuation buffer.

use super::utf8::{PartialResult, PartialUtf8};

#[test]
fn fresh_buffer_is_not_pending() {
    let p = PartialUtf8::default();
    assert!(!p.is_pending());
}

#[test]
fn completes_two_byte_codepoint_across_split() {
    // 'é' = 0xC3 0xA9. Stash the lead byte, then feed the trailing byte.
    let mut p = PartialUtf8::default();
    p.stash(&[0xC3]);
    assert!(p.is_pending());
    match p.advance(&[0xA9]) {
        PartialResult::Char { ch, consumed } => {
            assert_eq!(ch, 'é');
            assert_eq!(consumed, 1); // one new byte taken
        }
        other => panic!("expected Char, got {other:?}"),
    }
    assert!(!p.is_pending());
}

#[test]
fn completes_four_byte_codepoint_byte_by_byte() {
    // U+1F600 = 0xF0 0x9F 0x98 0x80.
    let bytes = '\u{1F600}'.to_string().into_bytes();
    let mut p = PartialUtf8::default();
    p.stash(&bytes[..1]);
    // Feed the remaining three bytes one at a time.
    assert!(matches!(
        p.advance(&bytes[1..2]),
        PartialResult::NeedMore { consumed: 1 }
    ));
    assert!(matches!(
        p.advance(&bytes[2..3]),
        PartialResult::NeedMore { consumed: 1 }
    ));
    match p.advance(&bytes[3..4]) {
        PartialResult::Char { ch, consumed } => {
            assert_eq!(ch, '\u{1F600}');
            assert_eq!(consumed, 1);
        }
        other => panic!("expected Char, got {other:?}"),
    }
}

#[test]
fn reports_consumed_excluding_already_buffered() {
    // Two lead bytes of a 3-byte codepoint buffered; completing byte arrives.
    // '→' = 0xE2 0x86 0x92.
    let mut p = PartialUtf8::default();
    p.stash(&[0xE2, 0x86]);
    match p.advance(&[0x92]) {
        PartialResult::Char { ch, consumed } => {
            assert_eq!(ch, '→');
            assert_eq!(consumed, 1); // only the one new byte
        }
        other => panic!("expected Char, got {other:?}"),
    }
}

#[test]
fn invalid_continuation_reports_invalid() {
    // 0xC3 expects a continuation byte; 0x28 ('(') is not one.
    let mut p = PartialUtf8::default();
    p.stash(&[0xC3]);
    match p.advance(&[0x28]) {
        PartialResult::Invalid { consumed } => {
            // The lead byte alone is the invalid unit; no new byte consumed.
            assert_eq!(consumed, 0);
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert!(!p.is_pending());
}

#[test]
fn trailing_scalar_after_completion_is_left_for_caller() {
    // Buffer one lead byte of 'é', then feed its trailing byte plus a whole
    // ASCII byte. Only the codepoint completes; the ASCII byte is not consumed.
    let mut p = PartialUtf8::default();
    p.stash(&[0xC3]);
    match p.advance(&[0xA9, b'Z']) {
        PartialResult::Char { ch, consumed } => {
            assert_eq!(ch, 'é');
            assert_eq!(consumed, 1); // 'Z' is left for the ground path
        }
        other => panic!("expected Char, got {other:?}"),
    }
}

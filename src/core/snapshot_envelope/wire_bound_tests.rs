// SPDX-License-Identifier: GPL-3.0-only
//! Wire-bound validation (fallible encode) and byte-identity coverage: encode
//! refuses externally constructed envelopes whose `usize` fields exceed their
//! narrowed on-wire integer widths instead of truncating them into bytes the
//! envelope's own decoder cannot read, while the capture path
//! (`from_terminal`) remains structurally bounded and byte-identical.

use super::*;

// `PromptKind` and `Dimensions` are named only by the wire-width tests, which
// exist only where `usize` is wider than the field they overflow.
#[cfg(target_pointer_width = "64")]
use crate::core::prompt_marks::PromptKind;
use crate::core::screen::Terminal;
#[cfg(target_pointer_width = "64")]
use crate::core::types::Dimensions;

fn sample_envelope() -> SnapshotEnvelope {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"hi");
    SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
}

fn expect_too_large(envelope: &SnapshotEnvelope, what: &str) {
    match envelope.encode() {
        Err(SnapshotEnvelopeError::ValueTooLarge { what: got, .. }) => {
            assert_eq!(got, what);
        }
        Err(other) => panic!("expected ValueTooLarge({what}), got {other:?}"),
        Ok(_) => panic!("expected ValueTooLarge({what}), got Ok"),
    }
}

#[cfg(target_pointer_width = "64")]
#[test]
fn oversized_u32_fields_refuse_to_encode() {
    const OVER: usize = u32::MAX as usize + 1;

    let mut envelope = sample_envelope();
    envelope.terminal.cursor.row = OVER;
    expect_too_large(&envelope, "cursor row");

    let mut envelope = sample_envelope();
    envelope.terminal.dimensions = Dimensions::new(OVER, 2);
    expect_too_large(&envelope, "columns");

    let mut envelope = sample_envelope();
    envelope.prompt_marks.push(SnapshotPromptMark {
        row: OVER,
        kind: PromptKind::PromptStart,
    });
    expect_too_large(&envelope, "prompt mark row");

    let mut envelope = sample_envelope();
    envelope.layout.scroll_region = Some(SnapshotScrollRegion {
        top: OVER,
        bottom: OVER,
    });
    expect_too_large(&envelope, "scroll region top");
}

#[test]
fn oversized_title_refuses_to_encode() {
    let mut envelope = sample_envelope();
    envelope.metadata.title = Some("t".repeat(u16::MAX as usize + 1));
    expect_too_large(&envelope, "title length");
}

#[test]
fn oversized_producer_version_refuses_to_encode() {
    // Header sibling of the section-string checks: without validation a
    // producer version one past the u16 width would encode with a
    // zero-truncated length prefix and desync every byte after the
    // header. `from_terminal` cannot hit this (compile-time package
    // version), so only externally constructed envelopes are affected.
    let mut envelope = sample_envelope();
    envelope.producer_version = "v".repeat(u16::MAX as usize + 1);
    assert!(matches!(
        envelope.validate_wire_bounds(),
        Err(SnapshotEnvelopeError::ValueTooLarge {
            what: "producer version length",
            ..
        })
    ));
    expect_too_large(&envelope, "producer version length");
}

#[test]
fn producer_version_at_the_u16_wire_maximum_round_trips() {
    let mut envelope = sample_envelope();
    envelope.producer_version = "v".repeat(u16::MAX as usize);
    envelope
        .validate_wire_bounds()
        .expect("boundary producer version validates");
    let bytes = envelope
        .encode()
        .expect("boundary producer version encodes");
    let caps = SnapshotEnvelopeCaps {
        max_string_bytes: 80_000,
        ..SnapshotEnvelopeCaps::default()
    };
    let decoded =
        SnapshotEnvelope::decode(&bytes, caps).expect("boundary producer version decodes");
    assert_eq!(decoded.producer_version, envelope.producer_version);
}

#[test]
fn oversized_combining_count_refuses_to_encode() {
    let mut envelope = sample_envelope();
    envelope.terminal.visible_rows[0].cells[0].combining = vec!['\u{0301}'; u8::MAX as usize + 1];
    expect_too_large(&envelope, "combining mark count");
}

#[test]
fn title_at_the_u16_wire_maximum_round_trips() {
    let mut envelope = sample_envelope();
    envelope.metadata.title = Some("t".repeat(u16::MAX as usize));
    let bytes = envelope.encode().expect("boundary title encodes");
    let caps = SnapshotEnvelopeCaps {
        max_string_bytes: 80_000,
        ..SnapshotEnvelopeCaps::default()
    };
    let decoded = SnapshotEnvelope::decode(&bytes, caps).expect("boundary title decodes");
    assert_eq!(decoded.metadata.title, envelope.metadata.title);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn prompt_mark_row_at_the_u32_wire_maximum_round_trips() {
    let mut envelope = sample_envelope();
    envelope.prompt_marks.push(SnapshotPromptMark {
        row: u32::MAX as usize,
        kind: PromptKind::PromptStart,
    });
    let bytes = envelope.encode().expect("boundary mark encodes");
    let decoded =
        SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).expect("decodes");
    assert_eq!(decoded.prompt_marks, envelope.prompt_marks);
}

#[test]
fn from_terminal_encode_bytes_are_pinned() {
    // Full-envelope byte identity for a fixed capture: any change to this
    // fixture is a deliberate wire-format change (bump the snapshot format
    // version and regenerate), never an accident of refactoring.
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b"hi");
    let mut envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    envelope.producer_version = "pin".to_owned();
    let bytes = envelope.encode().expect("encode");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let expected = concat!(
        "4f44595454592d534e415053484f5406000100030070696e050001000100b900",
        "0000000000000200000009010000000000000300000002000000000000000400",
        "00000400000000000000050000001f0000000000000004000000020000000000",
        "0000020000000100010001000000000000000000000000000002000000000400",
        "0000680000000000000000000000000000000069000000000000000000000000",
        "0000000020000000000000000000000000000000002000000000000000000000",
        "0000000000000004000000200000000000000000000000000000000020000000",
        "0000000000000000000000000020000000000000000000000000000000002000",
        "000000000000000000000000000000cccccc0b0c10cccccc0000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000004",
        "0000000000000000010000000001000000000000000000000000000000",
    );
    assert_eq!(hex, expected);
}

#[test]
fn extension_count_is_bounded_on_encode_and_decode() {
    let mut envelope = sample_envelope();
    envelope.terminal.visible_rows[0].cells[0].combining = vec!['\u{301}'; 17];
    expect_too_large(&envelope, "combining mark count");
    assert!(matches!(
        Terminal::from_snapshot_envelope(&envelope),
        Err(SnapshotEnvelopeError::ValueTooLarge {
            what: "combining mark count",
            value: 17,
            max: 16
        })
    ));
    envelope.terminal.visible_rows[0].cells[0].combining.clear();
    let mut wire = envelope.encode().unwrap();
    let producer_len = u16::from_le_bytes([wire[19], wire[20]]) as usize;
    let table = 23 + producer_len;
    let terminal_start = table + 5 * 12;
    // Prelude 31, history count 4, visible count 4, row prefix 5.
    // A default-attribute cell has 4 scalar bytes, 2 flags, 1 underline style,
    // 1 optional underline color, 1 fg, 1 bg, 4 link, protection and ownership.
    let count = terminal_start + 31 + 4 + 4 + 5 + 16;
    assert_eq!(wire[count], 0);
    wire[count] = 17;
    assert!(matches!(
        SnapshotEnvelope::decode(&wire, SnapshotEnvelopeCaps::default()),
        Err(SnapshotEnvelopeError::ValueTooLarge {
            what: "combining mark count",
            value: 17,
            max: 16
        })
    ));
}

#[test]
fn legacy_v4_restores_scalars_with_extension_disabled() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance("a\u{301}\u{302}\u{303}\u{304}".as_bytes());
    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let mut wire = envelope.encode().unwrap();
    wire[15..17].copy_from_slice(&4u16.to_le_bytes());
    let producer_len = u16::from_le_bytes([wire[19], wire[20]]) as usize;
    let layout_len_at = 23 + producer_len + 4 * 12 + 4;
    // Strip the version 5 owner and pending-wrap fields (ten bytes) and the
    // version 6 default print state (twelve bytes).
    let appended = 10 + 12;
    let len = u64::from_le_bytes(wire[layout_len_at..layout_len_at + 8].try_into().unwrap());
    wire[layout_len_at..layout_len_at + 8].copy_from_slice(&(len - appended).to_le_bytes());
    wire.truncate(wire.len() - appended as usize);
    let decoded = SnapshotEnvelope::decode(&wire, SnapshotEnvelopeCaps::default()).unwrap();
    assert_eq!(decoded.layout.cluster_owner, None);
    assert!(!decoded.layout.pending_wrap);
    let mut restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
    assert_eq!(restored.snapshot().cells, terminal.snapshot().cells);
    restored.advance("\u{305}".as_bytes());
    assert_eq!(
        restored.screen().cell(0, 0).unwrap().grapheme(),
        "a\u{301}\u{302}\u{303}\u{304}"
    );
    assert_eq!(restored.screen().cell(0, 1).unwrap().ch, '\u{305}');
}

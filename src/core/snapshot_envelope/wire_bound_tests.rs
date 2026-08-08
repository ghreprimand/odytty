// SPDX-License-Identifier: GPL-3.0-only
//! Wire-bound validation (fallible encode) and byte-identity coverage: encode
//! refuses externally constructed envelopes whose `usize` fields exceed their
//! narrowed on-wire integer widths instead of truncating them into bytes the
//! envelope's own decoder cannot read, while the capture path
//! (`from_terminal`) remains structurally bounded and byte-identical.

use super::*;

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
        "4f44595454592d534e415053484f5403000100030070696e050001000100b900",
        "0000000000000200000009010000000000000300000002000000000000000400",
        "0000040000000000000005000000090000000000000004000000020000000000",
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
        "00000000000000",
    );
    assert_eq!(hex, expected);
}

// SPDX-License-Identifier: GPL-3.0-only
// Project-authored fixtures: the print state a snapshot carries for output
// resumed after it, and the bounds a decoded or owned envelope must meet.
use odytty::core::{
    SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, SnapshotEnvelopeError, Terminal,
};

fn envelope_of(terminal: &Terminal) -> SnapshotEnvelope {
    SnapshotEnvelope::from_terminal(terminal, SnapshotCaptureLimits::default())
}

fn round_trip(envelope: &SnapshotEnvelope) -> SnapshotEnvelope {
    let bytes = envelope.encode().unwrap();
    SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap()
}

#[test]
fn resumed_output_keeps_print_protection() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"\x1b[1\"qA");
    let mut restored =
        Terminal::from_snapshot_envelope(&round_trip(&envelope_of(&source))).unwrap();
    source.advance(b"B\x1b[?2K");
    restored.advance(b"B\x1b[?2K");
    // Selective erase spares protected cells, so equal rows prove `B` was
    // printed protected on both sides.
    assert_eq!(source.snapshot().cells, restored.snapshot().cells);
    assert_eq!(restored.screen().cell(0, 1).unwrap().ch, 'B');
}

#[test]
fn print_state_survives_the_wire() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"\x1b[1;4:3;38;5;9m\x1b[1\"qx\xe2\x82");
    let envelope = envelope_of(&source);
    assert_eq!(envelope.layout.pending_utf8, vec![0xe2, 0x82]);
    assert!(envelope.layout.print_protected);
    assert!(envelope.layout.print_attrs.bold);
    assert_eq!(round_trip(&envelope).layout, envelope.layout);
}

#[test]
fn pending_bytes_must_start_exactly_one_unfinished_scalar() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"ok");
    let mut envelope = envelope_of(&terminal);
    for (bytes, expected) in [
        (vec![0x80], SnapshotEnvelopeError::InvalidUtf8),
        (vec![b'A'], SnapshotEnvelopeError::InvalidUtf8),
        (vec![0xc3, 0xa9], SnapshotEnvelopeError::InvalidUtf8),
        (vec![0xed, 0xa0], SnapshotEnvelopeError::InvalidUtf8),
        (
            vec![0xf0, 0x9f, 0x98, 0x80],
            SnapshotEnvelopeError::ValueTooLarge {
                what: "pending UTF-8 bytes",
                value: 4,
                max: 3,
            },
        ),
    ] {
        envelope.layout.pending_utf8 = bytes.clone();
        assert_eq!(
            Terminal::from_snapshot_envelope(&envelope).err(),
            Some(expected),
            "{bytes:02x?}"
        );
    }
    envelope.layout.pending_utf8 = vec![0xf0, 0x9f];
    let mut restored = Terminal::from_snapshot_envelope(&envelope).unwrap();
    restored.advance(b"\x98\x80");
    assert_eq!(restored.screen().cell(0, 2).unwrap().ch, '\u{1f600}');
}

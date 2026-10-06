// SPDX-License-Identifier: GPL-3.0-only
// Project-authored bounded protocol fixtures.
use odytty::core::{SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal};

fn copy_terminal(source: &Terminal) -> Terminal {
    let bytes = SnapshotEnvelope::from_terminal(source, SnapshotCaptureLimits::default())
        .encode()
        .unwrap();
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
    Terminal::from_snapshot_envelope(&decoded).unwrap()
}

#[test]
fn resumed_output_keeps_the_current_rendition() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"\x1b[31mA");
    let mut restored = copy_terminal(&source);
    assert_eq!(source.snapshot(), restored.snapshot());
    source.advance(b"B");
    restored.advance(b"B");
    assert_eq!(source.snapshot().cells, restored.snapshot().cells);
}

#[test]
fn resuming_after_a_split_utf8_scalar_matches_uninterrupted_output() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"Q\xf0");
    let mut restored = copy_terminal(&source);
    assert_eq!(source.snapshot(), restored.snapshot());
    source.advance(b"\x9f\x98\x80");
    restored.advance(b"\x9f\x98\x80");
    assert_eq!(source.snapshot().cells, restored.snapshot().cells);
}

#[test]
fn resumed_output_keeps_truecolor_and_underline_rendition() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"\x1b[38;2;1;2;3;48;2;4;5;6;58;5;42;4:3mA");
    let mut restored = copy_terminal(&source);
    source.advance(b"B");
    restored.advance(b"B");
    assert_eq!(source.snapshot().cells, restored.snapshot().cells);
}

#[test]
fn resuming_after_two_utf8_bytes_matches_uninterrupted_output() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"Q\xf0\x9f");
    let mut restored = copy_terminal(&source);
    source.advance(b"\x98\x80");
    restored.advance(b"\x98\x80");
    assert_eq!(source.snapshot().cells, restored.snapshot().cells);
}

#[test]
fn resuming_after_three_utf8_bytes_matches_uninterrupted_output() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"Q\xf0\x9f\x98");
    let mut restored = copy_terminal(&source);
    source.advance(b"\x80");
    restored.advance(b"\x80");
    assert_eq!(source.snapshot().cells, restored.snapshot().cells);
}

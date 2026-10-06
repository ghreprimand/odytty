// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored restore fixtures for the per-logical-line cell ceiling.
//! Honest captures of lines the live store retains must restore; the ceiling
//! counts source cells, not layout padding or the blank fill a capture adds to
//! a line's final row. The terminal core is shared by Linux, macOS and Windows.

use odytty::core::{
    SnapshotCaptureLimits, SnapshotCell, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal,
};

/// The live store's per-line ceiling (2^20 cells).
const CEILING: usize = 1 << 20;
const COLUMNS: usize = 1024;

fn round_trip(source: &Terminal) -> Terminal {
    let bytes = SnapshotEnvelope::from_terminal(source, SnapshotCaptureLimits::default())
        .encode()
        .unwrap();
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
    Terminal::from_snapshot_envelope(&decoded).expect("an honest capture restores")
}

fn feed_cells(terminal: &mut Terminal, count: usize) {
    let chunk = vec![b'x'; 64 * 1024];
    let mut left = count;
    while left > 0 {
        let take = left.min(chunk.len());
        terminal.advance(&chunk[..take]);
        left -= take;
    }
}

#[test]
fn line_saturated_at_the_live_ceiling_and_continuing_on_screen_round_trips() {
    let mut source = Terminal::new(COLUMNS, 4);
    // Exactly the ceiling scrolls into history as one open logical line (the
    // live store trims only past it), and the same line continues through
    // three wrapped visible rows into a partial fourth.
    feed_cells(&mut source, CEILING + 3 * COLUMNS + 7);
    assert_eq!(source.screen().scrollback_len(), CEILING / COLUMNS);
    let restored = round_trip(&source);
    assert_eq!(restored.snapshot(), source.snapshot());
    assert_eq!(
        restored.screen().scrollback_len(),
        source.screen().scrollback_len()
    );
}

#[test]
fn saturated_line_captured_after_a_width_change_round_trips() {
    let mut source = Terminal::new(COLUMNS, 4);
    feed_cells(&mut source, CEILING);
    source.advance(b"\r\nend\r\n\r\n\r\n\r\n");
    assert_eq!(source.screen().scrollback_len(), CEILING / COLUMNS + 1);
    // At 1,000 columns the closed line projects to 1,049 rows whose final row
    // carries 424 blank fill cells the stored line never held.
    source.resize(1000, 4);
    let restored = round_trip(&source);
    assert_eq!(restored.snapshot(), source.snapshot());
}

fn plain(ch: char) -> SnapshotCell {
    let mut cell = SnapshotCell::from(odytty::core::Cell::blank());
    cell.ch = ch;
    cell
}

#[test]
fn layout_padding_slots_do_not_count_toward_the_ceiling() {
    let terminal = Terminal::new(COLUMNS, 1);
    let mut envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let template = envelope.terminal.visible_rows[0].clone();
    let mut padded = template.clone();
    for cell in &mut padded.cells {
        *cell = plain('x');
    }
    padded.cells[COLUMNS - 1].layout_padding = true;
    padded.cells[COLUMNS - 1].ch = ' ';
    padded.wrapped = true;
    let mut full = template.clone();
    for cell in &mut full.cells {
        *cell = plain('x');
    }
    full.wrapped = false;
    // 1,024 rows of 1,023 source cells plus one padding slot, then one full
    // row: exactly the ceiling in source cells, 1,024 cells over it in slots.
    let mut rows = vec![padded; COLUMNS];
    rows.push(full.clone());
    envelope.terminal.scrollback_rows = rows.clone();
    assert!(Terminal::from_snapshot_envelope(&envelope).is_ok());

    // One more source cell in the same line is refused with the existing
    // bound error.
    let last = rows.len() - 1;
    rows[last - 1].cells[COLUMNS - 1] = plain('x');
    envelope.terminal.scrollback_rows = rows;
    let error = Terminal::from_snapshot_envelope(&envelope)
        .err()
        .expect("one source cell over the ceiling is refused");
    assert!(error.to_string().contains("logical line"), "{error}");
}

#[test]
fn blank_fill_is_uncounted_only_in_the_final_row() {
    let terminal = Terminal::new(COLUMNS, 1);
    let mut envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let mut content = envelope.terminal.visible_rows[0].clone();
    for cell in &mut content.cells {
        *cell = plain('x');
    }
    content.wrapped = true;
    let mut blank = envelope.terminal.visible_rows[0].clone();
    blank.wrapped = true;
    // The ceiling in content followed by a wrapped all-blank row and a closed
    // all-blank row: the blanks before the final row are source cells.
    let mut rows = vec![content; CEILING / COLUMNS];
    rows.push(blank.clone());
    blank.wrapped = false;
    rows.push(blank);
    envelope.terminal.scrollback_rows = rows;
    assert!(Terminal::from_snapshot_envelope(&envelope).is_err());
}

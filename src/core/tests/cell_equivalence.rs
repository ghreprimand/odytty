// SPDX-License-Identifier: GPL-3.0-only
//! Equivalence surface for the cell / scrollback representation.
//!
//! Pins **today's** observable grapheme behavior so a later storage change
//! (combining-mark side table, projection bounding) cannot land a silent
//! semantic drift that looks like a test update. Whole-buffer vs byte-split
//! feeds, combining at and past `MAX_COMBINING`, projection width after
//! mutation, and the snapshot-envelope / selection / transcript-shaped
//! extractors are compared cell by cell.
//!
//! Windows: no platform surface. This is core storage and CSI only; the same
//! tests run on every CI leg.

use super::*;
use crate::core::types::MAX_COMBINING;
use crate::selection::{CellPoint, SelectionRange, selected_text};
use std::mem::{align_of, size_of};

/// Measured on rustc 1.96 (x86_64). Do not infer alignment from the leading
/// field: `Attrs` starts with a `u16` but `Color` / `Option<LinkId>` raise it
/// to 4. The `Cell` comment in `types.rs` claiming 36 bytes predates the
/// four-slot combining array.
const _: () = assert!(size_of::<Attrs>() == 20);
const _: () = assert!(align_of::<Attrs>() == 4);
const _: () = assert!(size_of::<Cell>() == 44);
const _: () = assert!(align_of::<Cell>() == 4);

const MARKS: [char; 5] = ['\u{0301}', '\u{0302}', '\u{0303}', '\u{0304}', '\u{0305}'];

fn needs_copy<T: Copy>(_: T) {}

fn combining_cluster(base: char, mark_count: usize) -> String {
    let mut s = String::new();
    s.push(base);
    for mark in MARKS.iter().take(mark_count) {
        s.push(*mark);
    }
    s
}

fn feed(columns: usize, rows: usize, bytes: &[u8]) -> Terminal {
    let mut terminal = Terminal::new(columns, rows);
    terminal.advance(bytes);
    terminal
}

fn feed_bytesplit(columns: usize, rows: usize, bytes: &[u8]) -> Terminal {
    let mut terminal = Terminal::new(columns, rows);
    for byte in bytes {
        terminal.advance(std::slice::from_ref(byte));
    }
    terminal
}

/// Visible-grid graphemes, one `String` per cell, row-major.
fn visible_grapheme_grid(terminal: &Terminal) -> Vec<Vec<String>> {
    let snap = terminal.snapshot();
    let columns = snap.dimensions.columns;
    snap.cells
        .chunks(columns)
        .map(|row| row.iter().map(Cell::grapheme).collect())
        .collect()
}

fn snapshot_cell_grapheme(cell: &SnapshotCell) -> String {
    let mut s = String::new();
    s.push(cell.ch);
    for mark in &cell.combining {
        s.push(*mark);
    }
    s
}

/// Physical history as the session-host capture path sees it: scrollback
/// (oldest first) then the visible grid. Each inner vec is one physical row.
fn physical_grapheme_grid(terminal: &Terminal) -> Vec<Vec<String>> {
    let state = terminal.snapshot_state(100_000);
    state
        .scrollback_rows
        .iter()
        .chain(state.visible_rows.iter())
        .map(|row| row.cells.iter().map(snapshot_cell_grapheme).collect())
        .collect()
}

fn find_grapheme(grid: &[Vec<String>], needle: &str) -> bool {
    grid.iter()
        .any(|row| row.iter().any(|grapheme| grapheme == needle))
}

fn envelope_physical_graphemes(envelope: &SnapshotEnvelope) -> Vec<Vec<String>> {
    envelope
        .terminal
        .scrollback_rows
        .iter()
        .chain(envelope.terminal.visible_rows.iter())
        .map(|row| row.cells.iter().map(snapshot_cell_grapheme).collect())
        .collect()
}

#[test]
fn cell_and_attrs_layout_matches_recorded_sizes() {
    needs_copy(Cell::blank());
    needs_copy(Attrs::default());
    assert_eq!(size_of::<Attrs>(), 20);
    assert_eq!(align_of::<Attrs>(), 4);
    assert_eq!(size_of::<Cell>(), 44);
    assert_eq!(align_of::<Cell>(), 4);
    assert_eq!(MAX_COMBINING, 4);
}

#[test]
fn combining_keeps_four_marks_and_drops_the_fifth() {
    let mut cell = Cell::new('e', Attrs::default());
    for (i, mark) in MARKS.iter().enumerate() {
        let kept = cell.push_combining(*mark);
        if i < MAX_COMBINING {
            assert!(kept, "mark {i} must fit in MAX_COMBINING={MAX_COMBINING}");
        } else {
            assert!(!kept, "mark {i} must be dropped");
        }
    }
    assert_eq!(cell.combining(), &MARKS[..MAX_COMBINING]);
    assert_eq!(cell.grapheme(), combining_cluster('e', MAX_COMBINING));

    let mut terminal = Terminal::new(8, 2);
    terminal.advance(combining_cluster('e', 5).as_bytes());
    let printed = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(printed.ch, 'e');
    assert_eq!(printed.combining(), &MARKS[..MAX_COMBINING]);
    assert_eq!(printed.grapheme(), combining_cluster('e', MAX_COMBINING));
}

#[test]
fn cell_equality_includes_combining_marks() {
    let mut with_mark = Cell::new('e', Attrs::default());
    assert!(with_mark.push_combining('\u{0301}'));
    let bare = Cell::new('e', Attrs::default());
    assert_ne!(with_mark, bare);
    assert_eq!(with_mark, with_mark);
    assert_eq!(bare, Cell::new('e', Attrs::default()));
}

#[test]
fn whole_feed_and_bytesplit_feed_agree_cell_by_cell() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(combining_cluster('A', 4).as_bytes());
    bytes.extend_from_slice(b"bc\r\n");
    bytes.extend_from_slice(combining_cluster('D', 1).as_bytes());
    bytes.extend_from_slice(b"ef\r\n");
    bytes.extend_from_slice(combining_cluster('G', 3).as_bytes());
    bytes.extend_from_slice(b"hi\r\n");
    bytes.extend_from_slice(combining_cluster('J', 5).as_bytes());
    bytes.extend_from_slice(b"kl");

    let whole = feed(8, 3, &bytes);
    let split = feed_bytesplit(8, 3, &bytes);
    assert_eq!(visible_grapheme_grid(&whole), visible_grapheme_grid(&split));
    assert_eq!(
        physical_grapheme_grid(&whole),
        physical_grapheme_grid(&split)
    );
}

#[test]
fn combining_survives_scroll_into_scrollback() {
    let cluster = combining_cluster('e', 4);
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(cluster.as_bytes());
    terminal.advance(b"\r\nline2\r\nline3\r\nline4\r\n");

    let physical = physical_grapheme_grid(&terminal);
    assert!(
        find_grapheme(&physical, &cluster),
        "four-mark cluster must still be in physical history after scroll; grid={physical:?}"
    );
    assert!(
        !find_grapheme(&visible_grapheme_grid(&terminal), &cluster),
        "the cluster scrolled off the live 3-row grid"
    );
}

#[test]
fn combining_survives_erase_elsewhere_and_clears_on_erase_of_its_cell() {
    let cluster = combining_cluster('e', 2);
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(cluster.as_bytes());
    terminal.advance(b"xyz");
    // EL 0 from column 3 ('y'): must not strip marks on column 0.
    terminal.advance(b"\x1b[1;3H\x1b[K");
    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.grapheme(), cluster);

    // CUP home + EL 0: erases from the combining cell onward.
    terminal.advance(b"\x1b[H\x1b[K");
    let cleared = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cleared.ch, ' ');
    assert!(cleared.combining().is_empty());
}

#[test]
fn combining_survives_width_changing_resize_and_every_row_matches_new_width() {
    let cluster = combining_cluster('e', 4);
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(cluster.as_bytes());
    terminal.advance(b"XXXXXXX\r\nYYYYYYYY\r\n");
    terminal.resize(5, 4);

    let live = terminal.snapshot();
    assert_eq!(live.dimensions.columns, 5);
    assert_eq!(live.cells.len(), 5 * 4);
    for row in visible_grapheme_grid(&terminal) {
        assert_eq!(row.len(), 5, "live snapshot row width");
    }
    for row in physical_grapheme_grid(&terminal) {
        assert_eq!(row.len(), 5, "physical/capture row width after resize");
    }
    assert!(
        find_grapheme(&physical_grapheme_grid(&terminal), &cluster),
        "reflow must keep the four-mark cluster attached to its base"
    );
}

#[test]
fn combining_on_primary_is_isolated_across_alternate_screen() {
    let cluster = combining_cluster('e', 3);
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(cluster.as_bytes());
    terminal.advance(b"\r\nprimary\r\n");

    let primary_before = physical_grapheme_grid(&terminal);
    assert!(find_grapheme(&primary_before, &cluster));

    terminal.advance(b"\x1b[?1049h");
    let alt = visible_grapheme_grid(&terminal);
    assert!(
        !find_grapheme(&alt, &cluster),
        "alternate screen must not leak primary combining graphemes"
    );
    // Alternate scrollback is empty; every offset clamps to the live alt grid.
    let alt_scrolled = terminal.snapshot_with_scrollback(terminal.screen().scrollback_len().max(1));
    let alt_scrolled_grid: Vec<Vec<String>> = alt_scrolled
        .cells
        .chunks(alt_scrolled.dimensions.columns)
        .map(|row| row.iter().map(Cell::grapheme).collect())
        .collect();
    assert!(
        !find_grapheme(&alt_scrolled_grid, &cluster),
        "scrolled alternate snapshot must not show primary history"
    );

    terminal.advance(b"\x1b[?1049l");
    assert!(
        find_grapheme(&physical_grapheme_grid(&terminal), &cluster),
        "leaving the alternate screen must restore primary combining history"
    );
}

#[test]
fn snapshot_envelope_roundtrip_preserves_combining_graphemes() {
    let cluster = combining_cluster('e', 4);
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(cluster.as_bytes());
    terminal.advance(b"\r\nnext\r\n");

    let live = physical_grapheme_grid(&terminal);
    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    assert_eq!(envelope_physical_graphemes(&envelope), live);

    let bytes = envelope.encode().expect("honest capture must encode");
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
        .expect("honest envelope must decode");
    assert_eq!(envelope_physical_graphemes(&decoded), live);

    for row in decoded
        .terminal
        .scrollback_rows
        .iter()
        .chain(decoded.terminal.visible_rows.iter())
    {
        for snap_cell in &row.cells {
            assert_eq!(
                snap_cell.to_cell().grapheme(),
                snapshot_cell_grapheme(snap_cell)
            );
        }
    }
}

#[test]
fn selected_text_includes_combining_marks() {
    let cluster = combining_cluster('e', 4);
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(cluster.as_bytes());
    terminal.advance(b"Z");
    let snap = terminal.snapshot();
    let copied = selected_text(
        &snap,
        SelectionRange {
            start: CellPoint { row: 0, column: 0 },
            end: CellPoint { row: 0, column: 1 },
        },
    );
    assert!(
        copied.starts_with(&cluster),
        "copy must emit base+marks, got {copied:?}"
    );
    assert!(copied.contains('Z'));
}

#[test]
fn osc133_output_row_selection_preserves_grapheme_boundary_fixtures() {
    use crate::core::v013_fixtures::{GRAPHEME_BOUNDARY_FIXTURES, OscTerminator, osc133};

    for fixture in GRAPHEME_BOUNDARY_FIXTURES {
        let mut terminal = Terminal::new(64, 4);
        terminal.advance(&osc133(b"A", OscTerminator::Bell));
        terminal.advance(b"prompt$ ");
        terminal.advance(&osc133(b"B", OscTerminator::Bell));
        terminal.advance(b"show-output\r\n");
        terminal.advance(&osc133(b"C", OscTerminator::Bell));
        terminal.advance(fixture.text.as_bytes());
        terminal.advance(b"\r\n");
        terminal.advance(&osc133(b"D;0", OscTerminator::Bell));

        let blocks = command_blocks(&terminal.prompt_marks());
        let block = blocks.first().expect("one completed command block");
        let (start, end) = command_output_cell_range(block, 3, 64)
            .expect("fixture command has addressable output");
        let copied = selected_text(
            &terminal.snapshot(),
            SelectionRange {
                start: CellPoint {
                    row: start.row,
                    column: start.column,
                },
                end: CellPoint {
                    row: end.row,
                    column: end.column,
                },
            },
        );

        assert_eq!(copied, fixture.text, "grapheme fixture: {}", fixture.label);
    }
}

#[test]
fn transcript_shaped_export_matches_visible_grapheme_concatenation() {
    // Transcript export of the grid is the row-wise concatenation of
    // non-continuation graphemes. Pin it here so a storage change cannot
    // drop marks in the extractor used by copy and by headless transcripts.
    let cluster = combining_cluster('e', 4);
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(cluster.as_bytes());
    terminal.advance(b"X\r\nY");
    let snap = terminal.snapshot();
    let exported: Vec<String> = snap
        .cells
        .chunks(snap.dimensions.columns)
        .map(|row| {
            row.iter()
                .filter(|cell| !cell.wide_continuation)
                .flat_map(|cell| std::iter::once(cell.ch).chain(cell.combining().iter().copied()))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    assert_eq!(exported[0], format!("{cluster}X"));
    assert_eq!(exported[1], "Y");
}

#[test]
fn projection_same_width_reread_after_scroll_is_not_stale() {
    // Observable contract of the memoized Projection: after a mutation that
    // pushes a row into scrollback, a later read at the same width must
    // include the newly scrolled content — not a cached projection from
    // before the push.
    let first = combining_cluster('A', 2);
    let second = combining_cluster('B', 2);
    let mut terminal = Terminal::new(6, 2);
    terminal.advance(first.as_bytes());
    terminal.advance(b"\r\n");
    terminal.advance(second.as_bytes());
    terminal.advance(b"\r\nCCCCCC\r\n");

    let after_first_scroll = physical_grapheme_grid(&terminal);
    assert!(find_grapheme(&after_first_scroll, &first));
    assert!(find_grapheme(&after_first_scroll, &second));

    let third = combining_cluster('D', 4);
    terminal.advance(third.as_bytes());
    terminal.advance(b"\r\n");

    let after_more = physical_grapheme_grid(&terminal);
    assert!(
        find_grapheme(&after_more, &first),
        "older combining history must survive further pushes; grid={after_more:?}"
    );
    assert!(
        find_grapheme(&after_more, &third),
        "newly scrolled combining row must appear in the projection; grid={after_more:?}"
    );

    // Same-width resize must not drop or duplicate combining clusters.
    terminal.resize(6, 2);
    let after_same_width = physical_grapheme_grid(&terminal);
    assert_eq!(after_same_width, after_more);
}

#[test]
fn projection_every_snapshot_offset_row_has_current_width() {
    let mut terminal = Terminal::new(8, 3);
    for i in 0..12u8 {
        let cluster = combining_cluster(char::from(b'A' + i), 1 + (i as usize % 4));
        terminal.advance(cluster.as_bytes());
        terminal.advance(b"\r\n");
    }
    terminal.resize(5, 3);

    let columns = terminal.screen().dimensions().columns;
    assert_eq!(columns, 5);
    let scrollback_len = terminal.screen().scrollback_len();
    for offset in 0..=scrollback_len {
        let snap = terminal.snapshot_with_scrollback(offset);
        assert_eq!(snap.dimensions.columns, columns);
        assert_eq!(snap.cells.len(), snap.dimensions.rows * columns);
        for row in snap.cells.chunks(columns) {
            assert_eq!(row.len(), columns);
        }
    }
}

#[test]
fn ed2_does_not_fuse_old_wrapped_combining_history_on_next_resize() {
    // ED2 severs a trailing open wrap. After clear + new content, a width
    // change must not glue the old combining cluster onto the new line.
    let mut terminal = Terminal::new(8, 3);
    let old = combining_cluster('e', 3);
    terminal.advance(old.as_bytes());
    terminal.advance(b"WWWWWWWWW"); // force wrap
    terminal.advance(b"\x1b[2J\x1b[H");
    terminal.advance(b"NEW");
    terminal.resize(20, 3);

    let live = visible_grapheme_grid(&terminal);
    let concatenated: String = live.iter().flat_map(|row| row.iter().cloned()).collect();
    assert!(
        concatenated.contains("NEW"),
        "fresh content must survive the resize; live={live:?}"
    );
    // The old cluster may remain in scrollback, but it must not be fused
    // into the live "NEW" line as e+marks+NEW.
    let live_line0: String = live[0].concat();
    assert!(
        !live_line0.contains(&format!("{old}NEW")) && !live_line0.contains(&format!("{old}N")),
        "ED2-severed wrap must not fuse old combining history with NEW; line0={live_line0:?}"
    );
}

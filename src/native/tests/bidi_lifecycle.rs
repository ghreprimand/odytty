// SPDX-License-Identifier: GPL-3.0-only
//! S3c: lifecycle surfaces under the test-only bidi display gate. The map is
//! planned per frame from the rows on screen plus the paragraph above them,
//! so a width change, a history scroll, and a restored session place cells
//! exactly as a terminal that shows the same rows directly; export stays
//! logical. With the gate off nothing here plans a map.

use super::*;

use crate::core::{SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal};
use crate::grid::{BidiDisplayMap, BidiParagraphContext, max_bidi_context_rows};

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 4;

/// Project-authored mixed text: twelve six-letter Hebrew words in one
/// paragraph that soft-wraps over five rows at 20 columns, then two short
/// lines. At 20 columns every wrapped row after the first opens with a space
/// whose direction comes from the Hebrew at the end of the row above, so a
/// row planned without the paragraph above it places that space differently.
fn mixed_output() -> Vec<u8> {
    let mut text = String::new();
    for _ in 0..12 {
        text.push_str("\u{05D0}\u{05D1}\u{05D2}\u{05D3}\u{05D4}\u{05D5} ");
    }
    text.push_str("\r\nxy \u{05D3}\u{05D4}\r\nend");
    text.into_bytes()
}

/// The map the planner gives `terminal` at scrollback `offset`, with the
/// paragraph context above it: what any frame of these rows must show.
fn plan_of(terminal: &Terminal, offset: usize) -> BidiDisplayMap {
    let snapshot = terminal.snapshot_with_scrollback(offset);
    let wrapped: Vec<bool> = terminal
        .visible_search_rows(offset)
        .iter()
        .map(|row| row.wrapped)
        .collect();
    let max_rows = max_bidi_context_rows(snapshot.dimensions.columns);
    let (rows, overflow) = terminal.paragraph_context_rows(offset, max_rows);
    let context = BidiParagraphContext {
        rows: rows.into_iter().map(|row| row.cells).collect(),
        overflow,
    };
    BidiDisplayMap::plan_with_context(&snapshot, &wrapped, &context)
}

fn gated_app(columns: usize, rows: usize) -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(columns, rows),
        Settings::default(),
    );
    terminal.lock().expect("terminal").advance(&mixed_output());
    app.set_test_cell_for_test(CELL);
    app.set_bidi_display_for_test(true);
    (app, terminal)
}

fn presented(app: &mut App) -> BidiDisplayMap {
    let _ = app.present_bidi_frame_for_test(std::time::Instant::now());
    app.bidi_frame_map_for_test().expect("gate map").clone()
}

#[test]
fn a_width_change_replans_from_the_reflowed_rows() {
    let (mut app, terminal) = gated_app(COLUMNS, ROWS);
    let before = presented(&mut app);
    assert!(!before.is_identity(), "the fixture reorders");
    for columns in [12, 16, 31, COLUMNS] {
        assert!(app.resize_grid(CELL, columns as u32 * CELL.width, ROWS as u32 * CELL.height));
        let map = presented(&mut app);
        let mut printed = Terminal::new(columns, ROWS);
        printed.advance(&mixed_output());
        assert_eq!(
            map,
            plan_of(&printed, 0),
            "{columns} columns: the reflowed rows place as rows printed at that width"
        );
        let live = terminal.lock().expect("terminal");
        assert_eq!(
            live.snapshot().dimensions.columns,
            columns,
            "the model reflowed"
        );
    }
}

#[test]
fn a_history_scroll_replans_with_the_paragraph_above_at_every_offset() {
    let (mut app, terminal) = gated_app(COLUMNS, ROWS);
    let scrollback = app.scrollback_len_for_test();
    assert!(scrollback >= 2, "the paragraph reaches into history");
    // Every row of the buffer visible at once: the full paragraph is planned
    // with nothing above it.
    let total = scrollback + ROWS;
    let mut tall = Terminal::new(COLUMNS, total);
    tall.advance(&mixed_output());
    let whole = plan_of(&tall, 0);
    let mut reordered_rows = 0;
    for offset in 0..=scrollback {
        let at_bottom = app.viewport_offset_for_test();
        if offset > at_bottom {
            app.scroll_up_for_test(offset - at_bottom);
        }
        assert_eq!(app.viewport_offset_for_test(), offset);
        let map = presented(&mut app);
        let top = total - ROWS - offset;
        for row in 0..ROWS {
            reordered_rows += usize::from(map.row_is_reordered(row));
            for column in 0..COLUMNS {
                assert_eq!(
                    map.visual_column(row, column),
                    whole.visual_column(top + row, column),
                    "offset {offset} row {row} column {column}"
                );
            }
        }
        drop(terminal.lock().expect("terminal"));
    }
    assert!(reordered_rows > 0, "not vacuous");
}

#[test]
fn a_restored_session_plans_exactly_as_the_live_one() {
    let mut live = Terminal::new(COLUMNS, ROWS);
    live.advance(&mixed_output());
    let envelope = SnapshotEnvelope::from_terminal(&live, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().expect("encode");
    let decoded =
        SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).expect("decode");
    let restored = Terminal::from_snapshot_envelope(&decoded).expect("restore");
    let scrollback = live.screen().scrollback_len();
    assert_eq!(restored.screen().scrollback_len(), scrollback);
    for offset in 0..=scrollback {
        let map = plan_of(&live, offset);
        assert_eq!(plan_of(&restored, offset), map, "offset {offset}");
    }
    assert!(!plan_of(&live, 0).is_identity());
}

#[test]
fn export_rows_stay_in_logical_order() {
    let (mut app, terminal) = gated_app(COLUMNS, ROWS);
    let map = presented(&mut app);
    assert!(!map.is_identity());
    let terminal = terminal.lock().expect("terminal");
    let screen = terminal.screen();
    let mut rows = Vec::new();
    while rows.len() < screen.export_row_count() {
        let chunk = screen.export_chunk(rows.len(), 64);
        assert!(!chunk.rows.is_empty(), "export advances");
        rows.extend(chunk.rows);
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for row in &rows {
        for cell in &row.cells {
            if !cell.wide_continuation {
                line.push_str(&cell.grapheme());
            }
        }
        if !row.wrapped {
            lines.push(line.trim_end().to_owned());
            line.clear();
        }
    }
    let text = lines.join("\n");
    let expected: Vec<String> = String::from_utf8(mixed_output())
        .expect("utf8")
        .split("\r\n")
        .map(|line| line.trim_end().to_owned())
        .collect();
    let expected = expected.join("\n");
    assert_eq!(text.trim_end(), expected.trim_end());
}

#[test]
fn output_replayed_in_any_read_split_plans_as_one_read() {
    let output = mixed_output();
    let mut whole = Terminal::new(COLUMNS, ROWS);
    whole.advance(&output);
    let scrollback = whole.screen().scrollback_len();
    for split in [1, 2, 3, 5, 7] {
        let mut replayed = Terminal::new(COLUMNS, ROWS);
        for chunk in output.chunks(split) {
            replayed.advance(chunk);
        }
        for offset in 0..=scrollback {
            assert_eq!(
                plan_of(&replayed, offset),
                plan_of(&whole, offset),
                "{split}-byte reads, offset {offset}"
            );
        }
    }
}

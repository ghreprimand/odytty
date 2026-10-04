// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored Unicode fixtures for logical terminal ownership.
//! These headless checks cover Linux, macOS, and Windows without fonts or PTYs.
//! They preserve valid semantics, without asserting known-divergent widths.

use odytty::atlas::CellSize;
use odytty::core::{
    Dimensions, Position, SearchOptions, SnapshotCaptureLimits, SnapshotEnvelope,
    SnapshotEnvelopeCaps, Terminal,
};
use odytty::selection::{
    CellPoint, SelectionRange, cell_at_physical, selected_text, selected_text_block,
};

fn range(start: (usize, usize), end: (usize, usize)) -> SelectionRange {
    SelectionRange {
        start: CellPoint {
            row: start.0,
            column: start.1,
        },
        end: CellPoint {
            row: end.0,
            column: end.1,
        },
    }
}

#[test]
fn attached_marks_keep_arrival_order_and_logical_cursor() {
    for text in [
        "e\u{0301}\u{0323}",
        "\u{05D0}\u{05B0}\u{05B1}",
        "\u{0628}\u{064E}\u{0651}",
        "\u{0915}\u{093C}",
    ] {
        let mut terminal = Terminal::new(12, 2);
        terminal.advance(text.as_bytes());
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
        assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), text);
        assert_eq!(
            selected_text(&terminal.snapshot(), range((0, 0), (0, 0))),
            text
        );
    }
}

#[test]
fn mixed_direction_copy_and_cursor_addressing_stay_logical() {
    let text = "A\u{05D0}\u{05D1}12\u{0628}\u{062A}Z";
    let mut terminal = Terminal::new(16, 2);
    terminal.advance(text.as_bytes());
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 8 });
    assert_eq!(
        selected_text(&terminal.snapshot(), range((0, 0), (0, 7))),
        text
    );
    terminal.advance(b"\x1b[1;3HX");
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, '\u{05D0}');
    assert_eq!(terminal.screen().cell(0, 2).unwrap().ch, 'X');
    assert_eq!(terminal.screen().cell(0, 3).unwrap().ch, '1');
}

#[test]
fn utf8_read_boundaries_do_not_change_cells_or_marks() {
    let text = "\u{0915}\u{093C}\u{0628}\u{064E}\u{05D0}\u{05B0}\u{754C}\u{0301}";
    let mut whole = Terminal::new(20, 2);
    whole.advance(text.as_bytes());
    for split in 0..=text.len() {
        let mut split_terminal = Terminal::new(20, 2);
        split_terminal.advance(&text.as_bytes()[..split]);
        split_terminal.advance(&text.as_bytes()[split..]);
        assert_eq!(split_terminal.snapshot().cells, whole.snapshot().cells);
        assert_eq!(split_terminal.screen().cursor(), whole.screen().cursor());
    }
}

#[test]
fn wide_pair_and_marks_wrap_together() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance("abc\u{754C}\u{0301}X".as_bytes());
    let rows = terminal.visible_search_rows(0);
    assert!(rows[0].wrapped);
    assert_eq!(rows[1].cells[0].grapheme(), "\u{754C}\u{0301}");
    assert!(rows[1].cells[1].wide_continuation);
    assert_eq!(rows[1].cells[2].ch, 'X');
    assert_eq!(terminal.screen().cursor(), Position { row: 1, column: 3 });
}

#[test]
fn pending_wrap_mark_attaches_before_next_base_wraps() {
    let mut terminal = Terminal::new(3, 2);
    terminal.advance("ab\u{05D0}\u{05B0}".as_bytes());
    assert_eq!(
        terminal.screen().cell(0, 2).unwrap().grapheme(),
        "\u{05D0}\u{05B0}"
    );
    assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 2 });
    terminal.advance(b"X");
    assert!(terminal.visible_search_rows(0)[0].wrapped);
    assert_eq!(terminal.screen().cell(1, 0).unwrap().ch, 'X');
}

#[test]
fn logical_search_crosses_soft_wrap_but_not_hard_break() {
    let query = "\u{05D0}\u{05B0}\u{0628}\u{064E}";
    let mut soft = Terminal::new(3, 3);
    soft.advance(format!("ab{query}").as_bytes());
    let found = soft.search(query, SearchOptions::case_sensitive());
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].start.row, found[0].start.column), (0, 2));
    assert_eq!((found[0].end.row, found[0].end.column), (1, 0));
    let mut hard = Terminal::new(3, 3);
    hard.advance("ab\u{05D0}\u{05B0}\r\n\u{0628}\u{064E}".as_bytes());
    assert!(
        hard.search(query, SearchOptions::case_sensitive())
            .is_empty()
    );
}

#[test]
fn reflow_preserves_logical_search_and_attached_marks() {
    // Wide owner first: this pins valid reflow without the known wide-edge
    // padding provenance gap. That gap needs a failing-before fix separately.
    let text = "\u{754C}\u{0301}A\u{05D0}\u{05B0}\u{0628}\u{064E}Z";
    let mut terminal = Terminal::new(12, 8);
    terminal.advance(text.as_bytes());
    for columns in [4, 9, 3, 12] {
        terminal.resize(columns, 8);
        assert_eq!(
            terminal.search(text, SearchOptions::case_sensitive()).len(),
            1
        );
        let rows = terminal.visible_search_rows(0);
        for row in rows {
            for (col, cell) in row.cells.iter().enumerate() {
                if cell.wide_continuation {
                    assert!(col > 0);
                    assert_eq!(row.cells[col - 1].grapheme(), "\u{754C}\u{0301}");
                }
            }
        }
    }
}

#[test]
fn rectangular_copy_preserves_each_logical_column_band() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance("X\u{05D0}\u{05B0}\u{05D1}Y\r\nX\u{0628}\u{064E}\u{062A}Y".as_bytes());
    assert_eq!(
        selected_text_block(&terminal.snapshot(), range((0, 1), (1, 2))),
        "\u{05D0}\u{05B0}\u{05D1}\n\u{0628}\u{064E}\u{062A}"
    );
}

#[test]
fn overwriting_wide_continuation_clears_its_old_owner_and_marks() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance("\u{754C}\u{0301}\u{05D0}".as_bytes());
    terminal.advance(b"\x1b[1;2HX");
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), " ");
    assert!(!terminal.screen().cell(0, 1).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(0, 1).unwrap().ch, 'X');
    assert_eq!(terminal.screen().cell(0, 2).unwrap().ch, '\u{05D0}');
}

#[test]
fn snapshot_wire_roundtrip_preserves_logical_cells_and_cursor() {
    let mut terminal = Terminal::new(8, 3);
    terminal
        .advance("\u{05D0}\u{05B0}\u{0628}\u{064E}\u{754C}\u{0301}\r\n\u{0915}\u{093C}".as_bytes());
    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let wire = envelope.encode().unwrap();
    let decoded = SnapshotEnvelope::decode(&wire, SnapshotEnvelopeCaps::default()).unwrap();
    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
    assert_eq!(restored.snapshot().cells, terminal.snapshot().cells);
    assert_eq!(restored.screen().cursor(), terminal.screen().cursor());
    assert_eq!(
        restored.screen().plain_text(),
        terminal.screen().plain_text()
    );
}

#[test]
fn alternate_screen_edits_do_not_replace_primary_logical_text() {
    let mut terminal = Terminal::new(12, 3);
    terminal.advance("\u{05D0}\u{05B0}\u{0628}\u{064E}".as_bytes());
    let primary = terminal.snapshot();
    terminal.advance(b"\x1b[?1049h");
    terminal.advance("\u{0915}\u{093C}".as_bytes());
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.snapshot().cells, primary.cells);
    assert_eq!(terminal.snapshot().cursor, primary.cursor);
}

#[test]
fn physical_hit_testing_uses_cell_boundaries_at_multiple_scales() {
    for scale in [1, 2, 3] {
        let cell = CellSize {
            width: 8 * scale,
            height: 16 * scale,
            baseline: 12 * scale,
        };
        let dimensions = Dimensions::new(12, 4);
        let x = f64::from(cell.width * 3);
        let y = f64::from(cell.height * 2);
        assert_eq!(
            cell_at_physical(x - 0.25, y, cell, dimensions),
            CellPoint { row: 2, column: 2 }
        );
        assert_eq!(
            cell_at_physical(x, y, cell, dimensions),
            CellPoint { row: 2, column: 3 }
        );
        assert_eq!(
            cell_at_physical(-1.0, -1.0, cell, dimensions),
            CellPoint { row: 0, column: 0 }
        );
        assert_eq!(
            cell_at_physical(10000.0, 10000.0, cell, dimensions),
            CellPoint { row: 3, column: 11 }
        );
    }
}

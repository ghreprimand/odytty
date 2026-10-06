// SPDX-License-Identifier: GPL-3.0-only
//! Shaped-script owners combined with emoji clusters, Latin text, bidi
//! reordering, hyperlinks, search, selection, reflow, and split reads in one
//! terminal, through the public model API. Headless: the same on Linux,
//! macOS, and Windows, without fonts or PTYs.

use odytty::core::{SearchOptions, Terminal};
use odytty::grid::BidiDisplayMap;
use odytty::selection::{CellPoint, SelectionRange, selected_text, selected_text_block};

/// Devanagari ka, virama, ssa: one two-cell owner.
const CONJUNCT: &str = "\u{0915}\u{094D}\u{0937}";
/// Devanagari ra, virama, ka: one two-cell owner, shaped as a reph.
const REPH: &str = "\u{0930}\u{094D}\u{0915}";
/// Khmer ka, coeng, ka, vowel sign e: one two-cell owner.
const KHMER: &str = "\u{1780}\u{17D2}\u{1780}\u{17C1}";
const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
const LINK_OPEN: &str = "\x1b]8;;https://example.com/x\x1b\\";
const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

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

fn wrapped(terminal: &Terminal) -> Vec<bool> {
    terminal
        .visible_search_rows(0)
        .iter()
        .map(|row| row.wrapped)
        .collect()
}

/// `(lead column, span)` of every owner on `row` whose text is `text`.
fn owners_of(terminal: &Terminal, row: usize, text: &str) -> Vec<(usize, usize)> {
    let snapshot = terminal.snapshot();
    let columns = snapshot.dimensions.columns;
    let cells = &snapshot.cells[row * columns..(row + 1) * columns];
    let mut out = Vec::new();
    for (column, cell) in cells.iter().enumerate() {
        if !cell.wide_continuation && cell.grapheme() == text {
            let span = 1 + cells[column + 1..]
                .iter()
                .take_while(|cell| cell.wide_continuation)
                .count();
            out.push((column, span));
        }
    }
    out
}

#[test]
fn hyperlinks_search_and_selection_cover_whole_shaped_owners_on_a_mixed_row() {
    // go, a link over the conjunct and the family cluster, a -> b, Hebrew,
    // the reph owner, and the Khmer owner.
    let visible = format!("go {CONJUNCT}{FAMILY} a->b \u{05D0}\u{05D1} {REPH} {KHMER}");
    let stream = format!(
        "go {LINK_OPEN}{CONJUNCT}{FAMILY}{LINK_CLOSE} a->b \u{05D0}\u{05D1} {REPH} {KHMER}"
    );
    let mut terminal = Terminal::new(30, 3);
    terminal.advance(stream.as_bytes());
    let snapshot = terminal.snapshot();
    assert_eq!(owners_of(&terminal, 0, CONJUNCT), vec![(3, 2)]);
    assert_eq!(owners_of(&terminal, 0, FAMILY), vec![(5, 2)]);
    assert_eq!(owners_of(&terminal, 0, REPH), vec![(16, 2)]);
    assert_eq!(owners_of(&terminal, 0, KHMER), vec![(19, 2)]);
    assert_eq!(
        selected_text(&snapshot, range((0, 0), (0, 29))).trim_end(),
        visible
    );

    // Every cell of both linked owners, continuations included, carries the
    // link; no other cell does.
    let link = snapshot.cells[3].attrs.hyperlink.expect("linked owner");
    assert_eq!(
        terminal.hyperlink(link).map(|l| l.uri.as_str()),
        Some("https://example.com/x")
    );
    for (column, cell) in snapshot.cells[..30].iter().enumerate() {
        let expected = (3..7).contains(&column).then_some(link);
        assert_eq!(cell.attrs.hyperlink, expected, "column {column}");
    }

    // Search finds each owner exactly once and spans its cells.
    for (query, start, last) in [
        (CONJUNCT, 3, 4),
        (FAMILY, 5, 6),
        (REPH, 16, 17),
        (KHMER, 19, 20),
    ] {
        let found = terminal.search(query, SearchOptions::case_sensitive());
        assert_eq!(found.len(), 1, "{query:?}");
        assert_eq!(
            (found[0].start.column, found[0].end.column),
            (start, last),
            "{query:?}"
        );
    }
    // A selection holding an owner's lead cell copies the whole owner, never
    // part of its scalars; one holding only the continuation copies nothing,
    // the same rule as a CJK wide cell.
    for (lead, text) in [(3, CONJUNCT), (5, FAMILY), (16, REPH), (19, KHMER)] {
        assert_eq!(selected_text(&snapshot, range((0, lead), (0, lead))), text);
        assert_eq!(
            selected_text(&snapshot, range((0, lead + 1), (0, lead + 1))),
            ""
        );
    }
    let mut cjk = Terminal::new(4, 1);
    cjk.advance("\u{754C}".as_bytes());
    assert_eq!(selected_text(&cjk.snapshot(), range((0, 1), (0, 1))), "");
    assert_eq!(
        selected_text_block(&snapshot, range((0, 3), (0, 6))),
        format!("{CONJUNCT}{FAMILY}")
    );

    // Pointer hit testing under the display plan: every visual column of a
    // linked owner maps back to a linked logical cell of that owner.
    let map = BidiDisplayMap::plan(&snapshot, &wrapped(&terminal));
    assert!(map.row_is_reordered(0), "the Hebrew pair reorders the row");
    for visual in 0..30 {
        let logical = map.logical_column(0, visual);
        assert_eq!(map.visual_column(0, logical), visual, "inverse at {visual}");
        let linked = snapshot.cells[logical].attrs.hyperlink.is_some();
        assert_eq!(linked, (3..7).contains(&logical), "visual {visual}");
    }
    for (lead, span) in [(3, 2), (5, 2), (16, 2), (19, 2)] {
        let visual: Vec<usize> = (lead..lead + span)
            .map(|column| map.visual_column(0, column))
            .collect();
        let expected: Vec<usize> = (visual[0]..visual[0] + span).collect();
        assert_eq!(visual, expected, "owner at {lead} stays contiguous");
    }
}

#[test]
fn reflow_keeps_links_search_and_copy_on_owners_moved_across_wrap_padding() {
    let visible = format!("abc{CONJUNCT}d{KHMER}{FAMILY}z{REPH}");
    let stream = format!("abc{LINK_OPEN}{CONJUNCT}d{KHMER}{LINK_CLOSE}{FAMILY}z{REPH}");
    let mut terminal = Terminal::new(20, 8);
    terminal.advance(stream.as_bytes());
    for columns in [4, 5, 3, 7, 20] {
        terminal.resize(columns, 8);
        let snapshot = terminal.snapshot();
        let rows = snapshot.dimensions.rows;
        assert_eq!(
            selected_text(&snapshot, range((0, 0), (rows - 1, columns - 1)))
                .trim_end()
                .replace('\n', ""),
            visible,
            "copy at {columns} columns"
        );
        let found = terminal.search(&visible, SearchOptions::case_sensitive());
        assert_eq!(found.len(), 1, "search at {columns} columns");
        let mut linked_owners = 0;
        for row in 0..rows {
            for text in [CONJUNCT, KHMER] {
                for (lead, span) in owners_of(&terminal, row, text) {
                    assert_eq!(span, 2, "{text:?} at {columns} columns");
                    let cells = &snapshot.cells[row * columns..(row + 1) * columns];
                    assert!(
                        cells[lead..lead + span]
                            .iter()
                            .all(|cell| cell.attrs.hyperlink.is_some())
                    );
                    linked_owners += 1;
                }
            }
            for (column, cell) in snapshot.cells[row * columns..(row + 1) * columns]
                .iter()
                .enumerate()
            {
                if cell.layout_padding {
                    assert!(cell.attrs.hyperlink.is_none(), "padding at {row},{column}");
                }
            }
        }
        assert_eq!(linked_owners, 2, "both linked owners at {columns} columns");
        if columns == 4 {
            assert!(
                snapshot.cells[3].layout_padding,
                "the conjunct wraps past generated padding"
            );
        }
    }
}

#[test]
fn split_reads_through_shaped_owners_never_show_a_partial_owner_state() {
    // Each recorded frame (the replay recorder stores the snapshot after
    // every read) must hold exactly the complete scalars received so far.
    let text = format!("{REPH}\u{05D0}{CONJUNCT}{FAMILY}{KHMER}a->b");
    let bytes = text.as_bytes();
    let mut whole = Terminal::new(24, 2);
    whole.advance(bytes);
    for chunk in 1..=4 {
        let mut terminal = Terminal::new(24, 2);
        for (index, piece) in bytes.chunks(chunk).enumerate() {
            terminal.advance(piece);
            let received = (index + 1) * chunk;
            let complete = match std::str::from_utf8(&bytes[..received.min(bytes.len())]) {
                Ok(text) => text,
                Err(error) => std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap(),
            };
            let frame = terminal.snapshot();
            assert_eq!(
                selected_text(&frame, range((0, 0), (0, 23))).trim_end(),
                complete,
                "frame after {received} bytes in {chunk}-byte reads"
            );
            for (column, cell) in frame.cells[..24].iter().enumerate() {
                if cell.wide_continuation {
                    assert!(column > 0 && !frame.cells[column - 1].wide_continuation);
                }
            }
        }
        assert_eq!(terminal.snapshot().cells, whole.snapshot().cells);
        assert_eq!(terminal.screen().cursor(), whole.screen().cursor());
    }
}

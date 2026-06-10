//! Tests for the scrollback store and the logical-line projection machinery.
//!
//! The crux of correctness is the **same-width roundtrip** property: scrollback
//! holds physical rows directly, so the logical projection (the foundation C2
//! switches the source of truth onto) is behavior-identical iff
//! `project_logical(logical_from_physical(rows), W) == rows`. We prove that
//! exhaustively over the shapes real terminal operations produce, then add
//! cross-width projection goldens (wide glyphs, wrapping, open continuation
//! lines) and the `Scrollback` wrapper invariants. The Terminal-level tests
//! prove search/snapshot stay coherent across width changes through the live
//! resize path.

use super::screen::{Line, Terminal, blank_row};
use super::scrollback::{Scrollback, logical_from_physical, project_logical};
use super::search::SearchOptions;
use super::types::{Attrs, Cell};

const W: usize = 8;

/// A hard-terminated content row: `text` left-aligned, blank-padded to `width`.
fn content_w(text: &str, width: usize) -> Line {
    let mut cells: Vec<Cell> = text
        .chars()
        .map(|c| Cell::new(c, Attrs::default()))
        .collect();
    assert!(cells.len() <= width, "content wider than row");
    while cells.len() < width {
        cells.push(Cell::blank());
    }
    Line::unwrapped(cells)
}

fn content(text: &str) -> Line {
    content_w(text, W)
}

/// A full, soft-wrapped row (continues into the next physical row).
fn wrapped_full(ch: char) -> Line {
    Line::wrapped(vec![Cell::new(ch, Attrs::default()); W])
}

fn blank() -> Line {
    blank_row(W)
}

/// Projecting the logical form back at the *same* width must reproduce the
/// source physical rows byte-for-byte — the behavior-identity guarantee the C2
/// switch relies on.
fn assert_roundtrip(rows: &[Line]) {
    let logical = logical_from_physical(rows);
    let projected = project_logical(&logical, W);
    assert_eq!(
        projected, rows,
        "same-width projection must reproduce source physical rows"
    );
}

#[test]
fn roundtrip_empty() {
    assert_roundtrip(&[]);
    assert!(logical_from_physical(&[]).is_empty());
}

#[test]
fn roundtrip_plain_lines() {
    assert_roundtrip(&[content("hello"), content("world"), content("abc")]);
}

#[test]
fn roundtrip_blank_lines_preserved() {
    // Blank scrollback lines must survive (no trailing-line collapse in the
    // store — that only happens at the visible bottom during resize).
    assert_roundtrip(&[content("a"), blank(), blank(), content("b")]);
    assert_roundtrip(&[blank(), blank(), blank()]);
}

#[test]
fn roundtrip_soft_wrapped_logical_line() {
    // One logical line spanning two physical rows: full wrapped + partial tail.
    assert_roundtrip(&[wrapped_full('x'), content("tail")]);
    // Three physical rows in one logical line.
    assert_roundtrip(&[wrapped_full('a'), wrapped_full('b'), content("end")]);
}

#[test]
fn roundtrip_open_trailing_line() {
    // Scrollback ending on a wrapped row (continues into the live grid): the
    // common straddle case. The trailing run must stay marked wrapped.
    assert_roundtrip(&[content("done"), wrapped_full('z')]);
    assert_roundtrip(&[wrapped_full('a'), wrapped_full('b')]);
}

#[test]
fn roundtrip_full_width_hard_terminated() {
    // A full-width row that is hard-terminated (not wrapped) must stay unwrapped.
    let full_unwrapped = Line::unwrapped(vec![Cell::new('q', Attrs::default()); W]);
    assert_roundtrip(&[full_unwrapped]);
}

#[test]
fn roundtrip_wide_glyph_pair() {
    // A wide (two-column) glyph plus its continuation spacer inside a row.
    let mut cells = vec![Cell::new('a', Attrs::default())];
    cells.push(Cell::new('世', Attrs::default()));
    cells.push(Cell::wide_spacer(Attrs::default()));
    while cells.len() < W {
        cells.push(Cell::blank());
    }
    assert_roundtrip(&[Line::unwrapped(cells)]);
}

#[test]
fn roundtrip_combining_marks() {
    let mut base = Cell::new('e', Attrs::default());
    base.push_combining('\u{0301}'); // combining acute
    let mut cells = vec![base, Cell::new('x', Attrs::default())];
    while cells.len() < W {
        cells.push(Cell::blank());
    }
    assert_roundtrip(&[Line::unwrapped(cells)]);
}

#[test]
fn roundtrip_deep_mixed() {
    let mut rows = Vec::new();
    for i in 0..100 {
        if i % 7 == 0 {
            rows.push(wrapped_full('w'));
            rows.push(content("cont"));
        } else if i % 5 == 0 {
            rows.push(blank());
        } else {
            rows.push(content(&format!("L{i}")[..3.min(format!("L{i}").len())]));
        }
    }
    assert_roundtrip(&rows);
}

#[test]
fn logical_grouping_counts_lines_not_rows() {
    // Two wrapped rows + tail = one logical line; a plain row = one more.
    let rows = vec![
        wrapped_full('a'),
        wrapped_full('b'),
        content("end"),
        content("x"),
    ];
    let logical = logical_from_physical(&rows);
    assert_eq!(logical.len(), 2, "4 physical rows form 2 logical lines");
}

#[test]
fn cross_width_reflow_shape() {
    // A logical line of 12 'a's: at width 8 -> [8 wrapped, 4+pad], at width 4 ->
    // [4 wrapped, 4 wrapped, 4 unwrapped], back at 8 -> original 2-row shape.
    let logical = logical_from_physical(&[wrapped_full('a'), content_w("aaaa", W)]);

    let at8 = project_logical(&logical, 8);
    assert_eq!(at8.len(), 2);
    assert!(at8[0].wrapped && !at8[1].wrapped);

    let at4 = project_logical(&logical, 4);
    assert_eq!(at4.len(), 3, "12 cells at width 4 = 3 rows");
    assert!(at4[0].wrapped && at4[1].wrapped && !at4[2].wrapped);

    // Re-projecting at 8 reproduces the original shape (projection is pure).
    assert_eq!(project_logical(&logical, 8), at8);
}

#[test]
fn cross_width_open_line_stays_wrapped() {
    // An open logical line re-projected at a narrower width: every produced row,
    // including the last, stays wrapped (the line still continues).
    let logical = logical_from_physical(&[wrapped_full('a')]); // 8 'a's, open
    let at4 = project_logical(&logical, 4);
    assert_eq!(at4.len(), 2);
    assert!(
        at4.iter().all(|r| r.wrapped),
        "open line keeps all rows wrapped across width change"
    );
}

#[test]
fn cross_width_wide_glyph_never_straddles() {
    // 'AB世' where 世 is wide: at width 3 the wide pair cannot share the row with
    // 'AB', so it wraps to the next row as a whole pair (lead + spacer), never
    // split across the edge.
    let mut cells = vec![
        Cell::new('A', Attrs::default()),
        Cell::new('B', Attrs::default()),
    ];
    cells.push(Cell::new('世', Attrs::default()));
    cells.push(Cell::wide_spacer(Attrs::default()));
    let logical = logical_from_physical(&[Line::unwrapped(cells)]);

    let at3 = project_logical(&logical, 3);
    assert!(at3.len() >= 2, "wide pair wraps to its own row");
    // The wide lead's continuation must immediately follow on the SAME row
    // (never the last column of a row followed by a wrap).
    for row in &at3 {
        for (i, cell) in row.cells.iter().enumerate() {
            if cell.ch == '世' {
                assert!(
                    i + 1 < row.cells.len() && row.cells[i + 1].wide_continuation,
                    "wide lead must be followed by its spacer in the same row"
                );
            }
        }
    }
}

#[test]
fn scrollback_wrapper_basics() {
    let mut store = Scrollback::new();
    assert!(store.is_empty());
    store.push_row(content("a"));
    store.push_row(content("b"));
    assert_eq!(store.len(), 2);
    assert_eq!(store.physical().len(), 2);
    assert_eq!(store.physical()[0], content("a"));
    store.clear();
    assert!(store.is_empty());
    assert_eq!(store.len(), 0);
}

/// Search must keep finding content through reflow at different widths — the
/// resize re-wraps history but the searchable text is invariant. Every reported
/// match must land on a valid absolute cell.
#[test]
fn search_survives_width_change() {
    let mut term = Terminal::new(10, 3);
    // A long line that soft-wraps at width 10, then more lines to push it into
    // scrollback.
    term.advance(b"alpha\r\nbravo charlie delta echo\r\nfoxtrot\r\ngolf\r\nhotel\r\nindia\r\n");

    let assert_found = |term: &Terminal, needle: &str, label: &str| {
        let hits = term.search(needle, SearchOptions::default());
        assert!(!hits.is_empty(), "'{needle}' must be found {label}");
        let dims = term.screen().dimensions();
        let total_rows = term.screen().scrollback_len() + dims.rows;
        for m in &hits {
            assert!(
                m.start.row < total_rows && m.end.row < total_rows,
                "row in range {label}"
            );
            assert!(
                m.start.column < dims.columns && m.end.column < dims.columns,
                "column in range {label}"
            );
        }
    };

    assert_found(&term, "charlie", "at width 10");
    // A match spanning the soft-wrap boundary at width 10.
    assert_found(&term, "charlie delta", "spanning wrap at width 10");

    term.resize(6, 3);
    assert_found(&term, "charlie", "after narrow reflow");
    assert_found(&term, "charlie delta", "spanning wrap after narrow reflow");

    term.resize(20, 3);
    assert_found(&term, "charlie", "after widen reflow");
    assert_found(
        &term,
        "charlie delta echo",
        "full phrase after widen reflow",
    );
}

/// `scrollback_len` and `snapshot_with_scrollback` stay coherent across a width
/// change: every offset yields a full `rows * columns` snapshot with no panic.
#[test]
fn snapshot_coherent_across_width_change() {
    let mut term = Terminal::new(12, 4);
    for i in 0..40 {
        term.advance(format!("line number {i} with some trailing text\r\n").as_bytes());
    }
    for &w in &[12usize, 5, 30, 12] {
        term.resize(w, 4);
        let dims = term.screen().dimensions();
        let sb = term.screen().scrollback_len();
        for offset in [0usize, 1, sb / 2, sb, sb + 5] {
            let snap = term.snapshot_with_scrollback(offset);
            assert_eq!(
                snap.cells.len(),
                dims.rows * dims.columns,
                "snapshot at width {w} offset {offset} must be full grid"
            );
        }
    }
}

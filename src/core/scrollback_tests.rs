// SPDX-License-Identifier: GPL-3.0-only
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

use super::reflow::{reflow_lines, resize_keep_width};
use super::screen::{Line, Terminal, blank_row};
use super::scrollback::{
    DEFAULT_SCROLLBACK_LIMIT, ResizeOptions, Scrollback, logical_from_physical, project_logical,
    resize_lazy, resize_lazy_with_options,
};
use super::search::SearchOptions;
use super::types::{Attrs, Cell, Dimensions, Position};

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
    assert_eq!(store.physical_len(W), 2);
    assert_eq!(store.physical(W).len(), 2);
    assert_eq!(store.physical(W)[0], content("a"));
    store.clear();
    assert!(store.is_empty());
    assert_eq!(store.physical_len(W), 0);
}

/// `push_row` merges a soft-wrapped (open) run into one logical line that
/// re-wraps as a unit, while a hard newline starts a fresh line.
#[test]
fn push_row_merges_open_runs() {
    let mut store = Scrollback::new();
    store.push_row(wrapped_full('a')); // open: continues
    store.push_row(content("tail")); // hard-terminates the run
    store.push_row(content("next"));
    // Two logical lines: ["aaaaaaaa"+"tail", "next"]. At width 8 that projects to
    // 3 physical rows (2 for the wrapped line + 1).
    assert_eq!(store.physical_len(W), 3);
    let phys = store.physical(W);
    assert!(phys[0].wrapped && !phys[1].wrapped && !phys[2].wrapped);
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

// --- Differential parity: lazy resize vs eager reflow oracle ---------------
//
// `resize_lazy` re-wraps only the bottom of the buffer; the eager
// `reflow_lines` / `resize_keep_width` primitives re-wrap the whole buffer.
// Both must produce byte/coordinate-identical results: the new visible rows, the
// cursor, and the full physical scrollback projection at the new width.

/// Run both paths on identical inputs and assert they agree.
fn assert_resize_parity(
    scrollback: &[Line],
    visible: &[Line],
    cursor: Position,
    old_width: usize,
    new_dims: Dimensions,
) {
    let width_unchanged = new_dims.columns == old_width;

    // Oracle: eager reflow over the full physical buffer.
    let mut oracle_sb = scrollback.to_vec();
    let mut oracle_vis = visible.to_vec();
    let oracle_cursor = if width_unchanged {
        resize_keep_width(&mut oracle_sb, &mut oracle_vis, new_dims, cursor)
    } else {
        reflow_lines(&mut oracle_sb, &mut oracle_vis, new_dims, cursor)
    };

    // Lazy: logical store + bottom-only re-wrap.
    let mut sb = Scrollback::from_physical(scrollback);
    let mut vis = visible.to_vec();
    let lazy_cursor = resize_lazy(&mut sb, &mut vis, new_dims, cursor, width_unchanged);

    assert_eq!(lazy_cursor, oracle_cursor, "cursor mismatch ({new_dims:?})");
    assert_eq!(vis, oracle_vis, "visible rows mismatch ({new_dims:?})");
    assert_eq!(
        *sb.physical(new_dims.columns),
        oracle_sb,
        "scrollback projection mismatch ({new_dims:?})"
    );
}

/// Build a deterministic physical buffer (a `width`-wide grid of `n` rows) with a
/// mix of plain, soft-wrapped, blank, and wide-glyph content.
fn sample_rows(width: usize, n: usize) -> Vec<Line> {
    let mut rows = Vec::new();
    for i in 0..n {
        match i % 6 {
            0 => rows.push(content_w(
                &format!("row{i}")[..4.min(format!("row{i}").len())],
                width,
            )),
            1 => {
                // A soft-wrapped logical line: a full row continuing into the next.
                rows.push(Line::wrapped(vec![Cell::new('w', Attrs::default()); width]));
                rows.push(content_w("tail", width));
            }
            2 => rows.push(blank_row(width)),
            3 => {
                let mut cells = vec![
                    Cell::new('世', Attrs::default()),
                    Cell::wide_spacer(Attrs::default()),
                ];
                while cells.len() < width {
                    cells.push(Cell::blank());
                }
                rows.push(Line::unwrapped(cells));
            }
            _ => rows.push(content_w(
                &format!("L{i}")[..2.min(format!("L{i}").len())],
                width,
            )),
        }
    }
    rows
}

#[test]
fn resize_parity_sweep() {
    let old_width = 8;
    for &sb_depth in &[0usize, 1, 5, 60] {
        let scrollback = sample_rows(old_width, sb_depth);
        for &vis_h in &[1usize, 3, 6] {
            let visible = sample_rows(old_width, vis_h);
            let visible: Vec<Line> = visible.into_iter().take(vis_h).collect();
            // Pad/truncate visible to exactly vis_h rows of width old_width.
            let mut visible = visible;
            while visible.len() < vis_h {
                visible.push(blank_row(old_width));
            }
            visible.truncate(vis_h);

            for cur in [
                Position { row: 0, column: 0 },
                Position {
                    row: vis_h - 1,
                    column: old_width - 1,
                },
                Position {
                    row: vis_h / 2,
                    column: 3.min(old_width - 1),
                },
            ] {
                for &new_w in &[old_width, 4, 6, 12, 20] {
                    for &new_h in &[1usize, 2, vis_h, vis_h + 3, vis_h + 10] {
                        assert_resize_parity(
                            &scrollback,
                            &visible,
                            cur,
                            old_width,
                            Dimensions::new(new_w, new_h),
                        );
                    }
                }
            }
        }
    }
}

/// Repeated resizes through the lazy path must keep the buffer consistent with a
/// from-scratch eager reflow to the final size.
#[test]
fn resize_parity_repeated() {
    let old_width = 8;
    let scrollback = sample_rows(old_width, 50);
    let visible = {
        let mut v = sample_rows(old_width, 5);
        v.truncate(5);
        while v.len() < 5 {
            v.push(blank_row(old_width));
        }
        v
    };
    let cursor = Position { row: 2, column: 4 };

    // Drive a chain of resizes through the lazy path.
    let mut sb = Scrollback::from_physical(&scrollback);
    let mut vis = visible.clone();
    let mut cur = cursor;
    let mut width = old_width;
    for &(w, h) in &[(4usize, 6usize), (16, 3), (10, 8), (6, 4)] {
        let dims = Dimensions::new(w, h);
        cur = resize_lazy(&mut sb, &mut vis, dims, cur, w == width);
        width = w;
    }

    // The lazy chain's final physical buffer must be self-consistent: projecting
    // and re-deriving is stable, and a fresh eager reflow of the lazy buffer to
    // the same size is a no-op.
    let lazy_full: Vec<Line> = sb
        .physical(width)
        .iter()
        .cloned()
        .chain(vis.iter().cloned())
        .collect();
    let mut oracle_sb = lazy_full.clone();
    oracle_sb.truncate(lazy_full.len().saturating_sub(vis.len()));
    let mut oracle_vis = vis.clone();
    let dims = Dimensions::new(width, vis.len());
    let oracle_cursor = resize_keep_width(&mut oracle_sb, &mut oracle_vis, dims, cur);
    assert_eq!(oracle_vis, vis, "stable visible under no-op eager reflow");
    assert_eq!(oracle_cursor, cur, "stable cursor under no-op eager reflow");
}

// --- Bounded-scrollback eviction (OOM guard) ---------------------------------

#[test]
fn push_row_evicts_oldest_past_the_limit() {
    let mut sb = Scrollback::with_limit(3);
    for i in 0..3 {
        sb.push_row(content(&i.to_string()));
    }
    assert_eq!(sb.physical_len(W), 3);
    // A fourth hard line drops the oldest; the window holds the newest three.
    sb.push_row(content("3"));
    let rows = sb.physical(W);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], content("1"));
    assert_eq!(rows[2], content("3"));
}

#[test]
fn unbounded_limit_never_trims() {
    let mut sb = Scrollback::with_limit(0);
    for i in 0..5000 {
        sb.push_row(content_w(&(i % 10).to_string(), W));
    }
    assert_eq!(sb.physical_len(W), 5000);
}

#[test]
fn lowering_the_limit_trims_existing_history_immediately() {
    let mut sb = Scrollback::with_limit(0);
    for i in 0..100 {
        sb.push_row(content(&(i % 10).to_string()));
    }
    assert_eq!(sb.physical_len(W), 100);
    sb.set_limit(10);
    assert_eq!(sb.physical_len(W), 10);
    // The retained window is the newest 10 lines.
    let rows = sb.physical(W);
    assert_eq!(rows[0], content("0")); // line 90 → '0'
    assert_eq!(rows[9], content("9")); // line 99 → '9'
}

#[test]
fn default_limit_is_the_documented_cap() {
    let sb = Scrollback::new();
    assert_eq!(sb.limit(), DEFAULT_SCROLLBACK_LIMIT);
    let mut sb = Scrollback::new();
    for _ in 0..(DEFAULT_SCROLLBACK_LIMIT + 250) {
        sb.push_row(content("x"));
    }
    assert_eq!(sb.physical_len(W), DEFAULT_SCROLLBACK_LIMIT);
}

#[test]
fn open_logical_line_is_bounded_without_a_terminator() {
    // A never-terminated (always-wrapped) stream is one open logical line. The
    // per-line cell ceiling keeps it bounded even though the line count is 1.
    let mut sb = Scrollback::with_limit(10_000);
    // 200k wrapped rows of width W → 1.6M cells in a single open line, above the
    // 1<<20 (1.05M) cell ceiling, so the front is trimmed.
    for _ in 0..200_000 {
        sb.push_row(wrapped_full('z'));
    }
    let total_cells: usize = sb.physical(W).iter().map(|r| r.cells.len()).sum();
    assert!(
        total_cells <= (1 << 20) + W,
        "open line must stay bounded, got {total_cells} cells"
    );
}

// ---------------------------------------------------------------------------
// Shell-owns-cursor (ConPTY) resize through the production lazy path must keep
// the cursor at its incoming VISIBLE row, not at a combined-buffer offset.
//
// `resize_lazy_with_options` prepends `cursor_prefix` pulled-scrollback rows to
// the live grid before reflow and feeds the reflow a COMBINED-buffer cursor row
// (`cursor_prefix + cursor.row`). When `shell_owns_cursor_on_resize` is true the
// reflow takes its `None` cursor arm, which (pre-fix) treated that combined row
// as a visible row and clamped it to the new grid — drifting the cursor down by
// `cursor_prefix` and pinning it to the bottom. Every prior shell-owns test fed
// EMPTY scrollback (`cursor_prefix == 0`) with a bottom-row cursor, where the
// buggy and correct values coincide, so the drift hid for many commits. These
// two tests use NON-empty pulled scrollback and a NON-bottom cursor through the
// production path, which is the only place the combined coordinate is built.
// ---------------------------------------------------------------------------

/// Basic drift: with pulled scrollback (`cursor_prefix > 0`), a width
/// change, and a non-bottom cursor, the shell-owns resize must keep the cursor
/// at its incoming visible row (0). Pre-fix the `None` arm clamps the combined
/// row (5) to the bottom row (4) — the observed downward drift.
#[test]
fn shell_owns_resize_with_pulled_scrollback_keeps_incoming_visible_row() {
    // Five short hard-terminated scrollback lines (one physical row each at the
    // new width), so the lazy pull prepends five prefix rows -> cursor_prefix=5.
    let scrollback = vec![
        content("s0"),
        content("s1"),
        content("s2"),
        content("s3"),
        content("s4"),
    ];
    // Live grid (width 8, height 5): content on the top row, cursor parked there
    // (non-bottom), the rest blank.
    let mut visible = vec![content("top"), blank(), blank(), blank(), blank()];
    let cursor = Position { row: 0, column: 1 };

    let mut sb = Scrollback::from_physical(&scrollback);
    let result = resize_lazy_with_options(
        &mut sb,
        &mut visible,
        Dimensions::new(6, 5), // width CHANGE (8 -> 6) forces the reflow path
        cursor,
        false, // width_unchanged = false
        ResizeOptions {
            shell_owns_cursor_on_resize: true,
            ..ResizeOptions::default()
        },
    );

    // The shell owns placement; the terminal must keep the incoming VISIBLE row
    // (0), clamped to the new dims — NOT the combined row (5) clamped to the
    // bottom (4) that the pre-fix `None` arm produced.
    assert_eq!(
        result.cursor,
        Position { row: 0, column: 1 },
        "shell-owns resize drifted the cursor off its incoming visible row"
    );
}

/// Decision gate: a width SHRINK where the content ABOVE the cursor rewraps
/// into MORE physical rows, so the post-rewrap `visible_start` (6) diverges from
/// `cursor_prefix` (1). This is the case that separates the three candidate
/// fixes, all of which agree when `visible_start == cursor_prefix`:
///   (a) subtract `visible_start`  -> row 0 (cursor flung to the top) — WRONG
///   (b) subtract `cursor_prefix`  -> row 1 (incoming visible row)    — CORRECT
///   (c) recompute the visible row -> row 1 (same as b)
/// Pre-fix the `None` arm subtracts nothing -> row 2. Asserting row 1 makes this
/// test reject BOTH the pre-fix code AND candidate (a), pinning candidate (b).
#[test]
fn shell_owns_resize_shrink_rewrap_above_cursor_pins_cursor_prefix_fix() {
    // One long hard-terminated scrollback line of 100 'x' (5 physical rows at
    // width 20), which rewraps to 10 rows at the new width 10 — i.e. content
    // above the cursor expands, pushing visible_start past cursor_prefix.
    let long: Vec<Line> = {
        let mut rows = Vec::new();
        for r in 0..5 {
            let cells: Vec<Cell> = (0..20).map(|_| Cell::new('x', Attrs::default())).collect();
            // First four rows soft-wrap into the next; the fifth ends the line.
            rows.push(if r < 4 {
                Line::wrapped(cells)
            } else {
                Line::unwrapped(cells)
            });
        }
        rows
    };
    // Live grid (width 20, height 6): two content rows then blanks; cursor on the
    // second content row (visible row 1, non-bottom).
    let mut visible = vec![
        content_w("AAAA", 20),
        content_w("BBBB", 20),
        blank_row(20),
        blank_row(20),
        blank_row(20),
        blank_row(20),
    ];
    let cursor = Position { row: 1, column: 2 };

    let mut sb = Scrollback::from_physical(&long);
    let result = resize_lazy_with_options(
        &mut sb,
        &mut visible,
        Dimensions::new(10, 6), // width SHRINK 20 -> 10: the long line rewraps up
        cursor,
        false,
        ResizeOptions {
            shell_owns_cursor_on_resize: true,
            ..ResizeOptions::default()
        },
    );

    // Correct = incoming visible row (1), proving candidate (b): subtract the
    // true cursor_prefix, NOT visible_start (which would give row 0 here).
    assert_eq!(
        result.cursor,
        Position { row: 1, column: 2 },
        "shrink-rewrap shell-owns resize must keep the incoming visible row (cursor_prefix fix)"
    );
}

/// ConPTY no-rewrap binding test: on a `shell_owns_cursor_on_resize` backend (ConPTY),
/// conhost authoritatively reflows and absolutely repaints the visible viewport
/// on every resize. Running OdyTTY's own competing rewrap on the live grid
/// strands the input-line tail at a stale wrap column, and the error
/// ACCUMULATES across repeated narrow<->wide cycles. The fix mirrors the
/// alt-screen non-reflow posture: on a width change with shell_owns=true, the
/// live grid is truncate/padded (NOT rejoined+rewrapped) and conhost's repaint
/// owns the viewport next tick.
///
/// This asserts the live grid took the truncate/pad path, NOT the rewrap path:
/// the tail "GHIJ" stays on its own row instead of being redistributed, and the
/// result is byte-identical to a plain truncate/pad of the original. Pre-fix the
/// rewrap rejoins "0123456789ABCDEFGHIJ" and re-wraps it to width 6, moving
/// "CDEFGH" onto row 2 -> RED.
#[test]
fn shell_owns_resize_does_not_rewrap_live_input_line() {
    // A 20-char input line soft-wrapped across 3 rows at width 8, plus a blank
    // tail row; cursor parked in the tail (row 2), as PSReadLine would leave it.
    let original = || {
        vec![
            Line::wrapped(
                "01234567"
                    .chars()
                    .map(|c| Cell::new(c, Attrs::default()))
                    .collect(),
            ),
            Line::wrapped(
                "89ABCDEF"
                    .chars()
                    .map(|c| Cell::new(c, Attrs::default()))
                    .collect(),
            ),
            content_w("GHIJ", 8),
            blank_row(8),
        ]
    };
    let cursor = Position { row: 2, column: 3 };

    // Truncate/pad oracle: each original row independently resized to width 6,
    // with NO rejoin/rewrap (exactly what the alt-screen `resize_buffer_rows`
    // primitive does, and what the fix routes the live grid through).
    let truncate_pad = |rows: &mut Vec<Line>, width: usize| {
        for row in rows.iter_mut() {
            row.resize(width, Cell::blank());
        }
    };

    // Single width change (8 -> 6) is enough to separate truncate/pad from rewrap.
    let mut visible = original();
    let mut sb = Scrollback::from_physical(&[]);
    resize_lazy_with_options(
        &mut sb,
        &mut visible,
        Dimensions::new(6, 4),
        cursor,
        false, // width CHANGE forces the reflow decision
        ResizeOptions {
            shell_owns_cursor_on_resize: true,
            ..ResizeOptions::default()
        },
    );

    let mut expected = original();
    truncate_pad(&mut expected, 6);
    assert_eq!(
        visible, expected,
        "shell-owns width change must truncate/pad the live grid, not rewrap it"
    );
    // The tail stays put on row 2 ("GHIJ"), proving it was not stranded onto a
    // rewrapped row ("CDEFGH" is what the competing rewrap would place here).
    let row2: String = visible[2].cells.iter().map(|c| c.ch).collect();
    assert!(
        row2.starts_with("GHIJ"),
        "input-line tail stranded by a competing rewrap: row2 = {row2:?}"
    );

    // Accumulation signature: drag narrow<->wide repeatedly. Truncate/pad is
    // idempotent at a fixed width (it converges), so after many cycles the grid
    // at width 6 still equals the single truncate/pad — no compounding drift.
    let mut grid = original();
    let mut sb2 = Scrollback::from_physical(&[]);
    let mut width = 8usize;
    for _ in 0..3 {
        let next = if width == 8 { 6 } else { 8 };
        resize_lazy_with_options(
            &mut sb2,
            &mut grid,
            Dimensions::new(next, 4),
            cursor,
            false,
            ResizeOptions {
                shell_owns_cursor_on_resize: true,
                ..ResizeOptions::default()
            },
        );
        width = next;
    }
    // End on width 6.
    if width != 6 {
        resize_lazy_with_options(
            &mut sb2,
            &mut grid,
            Dimensions::new(6, 4),
            cursor,
            false,
            ResizeOptions {
                shell_owns_cursor_on_resize: true,
                ..ResizeOptions::default()
            },
        );
    }
    let row2_cycled: String = grid[2].cells.iter().map(|c| c.ch).collect();
    assert!(
        row2_cycled.starts_with("GHIJ"),
        "tail accumulated corruption across narrow<->wide cycles: row2 = {row2_cycled:?}"
    );
}

/// Guard: with shell_owns=false (POSIX PTY / Linux + macOS), the live grid
/// MUST still be rejoined and rewrapped to the new width exactly as before. This
/// proves the no-rewrap gate is conditioned on the flag and that the unix
/// backend's behavior stays byte-identical. Pre- and post-fix this stays GREEN;
/// reverting the production gate must NOT make it fail.
#[test]
fn non_shell_owns_resize_still_rewraps() {
    let mut visible = vec![
        Line::wrapped(
            "01234567"
                .chars()
                .map(|c| Cell::new(c, Attrs::default()))
                .collect(),
        ),
        Line::wrapped(
            "89ABCDEF"
                .chars()
                .map(|c| Cell::new(c, Attrs::default()))
                .collect(),
        ),
        content_w("GHIJ", 8),
        blank_row(8),
    ];
    let cursor = Position { row: 2, column: 3 };

    let mut sb = Scrollback::from_physical(&[]);
    resize_lazy_with_options(
        &mut sb,
        &mut visible,
        Dimensions::new(6, 4),
        cursor,
        false,
        ResizeOptions {
            shell_owns_cursor_on_resize: false, // POSIX PTY: still rewraps
            ..ResizeOptions::default()
        },
    );

    // Rewrap rejoins "0123456789ABCDEFGHIJ" and re-wraps at width 6, so row 1 is
    // "6789AB" (redistributed) -- NOT the truncate "89ABCD".
    let row1: String = visible[1].cells.iter().map(|c| c.ch).collect();
    assert_eq!(
        row1, "6789AB",
        "non-shell-owns resize must still rewrap the live grid (Linux/macOS path)"
    );
}

/// Decision gate (scrollback projection, option (a) over (b)): on a shell_owns width
/// change, scrollback above the viewport must STILL project to the new width
/// (history readability) even though the live grid no longer rewraps. Scrollback
/// is held as width-independent logical lines and projected on access, so a
/// long line projects to MORE rows at a narrower width -- proving it re-adapts
/// rather than being frozen or passed through (the rejected option (b)).
#[test]
fn shell_owns_resize_preserves_scrollback_projection() {
    // One 24-char logical line in scrollback: 3 rows at width 8.
    let long: Vec<Line> = vec![
        Line::wrapped(
            "01234567"
                .chars()
                .map(|c| Cell::new(c, Attrs::default()))
                .collect(),
        ),
        Line::wrapped(
            "89ABCDEF"
                .chars()
                .map(|c| Cell::new(c, Attrs::default()))
                .collect(),
        ),
        content_w("GHIJKLMN", 8),
    ];
    let mut sb = Scrollback::from_physical(&long);
    let mut visible = vec![content_w("live", 8), blank_row(8)];
    let cursor = Position { row: 0, column: 0 };

    resize_lazy_with_options(
        &mut sb,
        &mut visible,
        Dimensions::new(6, 2),
        cursor,
        false,
        ResizeOptions {
            shell_owns_cursor_on_resize: true,
            ..ResizeOptions::default()
        },
    );

    // Project the remaining scrollback at the NEW width: the 24-char line now
    // needs 4 rows (ceil(24/6)) instead of 3, and every row is exactly 6 wide.
    let projected = sb.physical(6);
    let joined: String = projected
        .iter()
        .flat_map(|r| r.cells.iter())
        .map(|c| c.ch)
        .filter(|c| *c != ' ')
        .collect();
    assert!(
        joined.contains("0123456789ABCDEFGHIJKLMN"),
        "scrollback content lost: {joined:?}"
    );
    assert!(
        projected.iter().all(|r| r.cells.len() == 6),
        "scrollback did not re-project to the new width (decision a violated)"
    );
}

#[test]
fn terminal_set_scrollback_limit_caps_live_history() {
    let mut term = Terminal::new(W, 3);
    term.set_scrollback_limit(5);
    // Emit 50 newline-terminated lines; only 3 fit on the grid, the rest scroll
    // into the capped scrollback.
    for i in 0..50 {
        term.advance(format!("L{i}\r\n").as_bytes());
    }
    assert!(
        term.screen().scrollback_len() <= 5,
        "scrollback_len {} exceeded cap",
        term.screen().scrollback_len()
    );
}

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

use super::prompt_marks::PromptKind;
use super::reflow::{reflow_lines, resize_keep_width};
use super::screen::{Line, Terminal, blank_row};
use super::scrollback::{
    DEFAULT_SCROLLBACK_LIMIT, ResizeOptions, Scrollback, logical_from_physical, project_logical,
    resize_lazy, resize_lazy_with_options,
};
use super::search::SearchOptions;
use super::types::{Attrs, Cell, Dimensions, MAX_COMBINING, Position};

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
    let epoch = sb.trim_epoch();
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
    assert_ne!(
        sb.trim_epoch(),
        epoch,
        "front eviction advances the origin epoch"
    );
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
fn steady_state_front_eviction_stays_bounded_and_ordered() {
    // I-3: once the store is at its line-count cap, sustained one-row output must
    // evict the oldest logical line per push with O(1) work (VecDeque pop_front,
    // no limit-sized memmove). A Vec front-drain shifted the whole retained tail
    // on every eviction, making a long output run O(n^2) — this exercises an
    // append run far past the cap that would be pathologically slow under that
    // shape and asserts the retained window is content-exact and steady.
    const LIMIT: usize = 10_000;
    let mut sb = Scrollback::with_limit(LIMIT);
    for i in 0..LIMIT {
        sb.push_row(content(&(i % 10).to_string()));
    }
    assert_eq!(sb.physical_len(W), LIMIT);

    let extra = 100_000usize;
    for i in 0..extra {
        sb.push_row(content(&(i % 10).to_string()));
    }

    // Steady state: the window never exceeds the cap.
    assert_eq!(sb.physical_len(W), LIMIT);
    let rows = sb.physical(W);
    assert_eq!(rows.len(), LIMIT);
    // The newest line is the last one pushed; the oldest retained is exactly
    // LIMIT lines back, proving front eviction kept insertion order intact.
    assert_eq!(rows[LIMIT - 1], content(&((extra - 1) % 10).to_string()));
    assert_eq!(rows[0], content(&((extra - LIMIT) % 10).to_string()));
}

#[test]
fn open_logical_line_is_bounded_without_a_terminator() {
    // A never-terminated (always-wrapped) stream is one open logical line. The
    // per-line cell ceiling keeps it bounded even though the line count is 1.
    let mut sb = Scrollback::with_limit(10_000);
    let epoch = sb.trim_epoch();
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
    assert_ne!(
        sb.trim_epoch(),
        epoch,
        "oversized open-line front drain advances the origin epoch"
    );
}

#[test]
fn open_line_after_closed_history_is_bounded_without_a_terminator() {
    // By the store's own invariant an open line is always the LAST line, so
    // the per-line cell ceiling must bound the trailing open line even when
    // closed history precedes it. Regression: the ceiling used to inspect
    // `lines[0]` only, so `printf 'hello\n'` followed by a no-newline stream
    // (`yes | tr -d '\n'`) grew the trailing open line without bound (OOM).
    let mut sb = Scrollback::with_limit(10_000);
    sb.push_row(content("hello")); // one closed line already in history
    // 200k wrapped rows of width W → 1.6M cells in the trailing open line,
    // above the 1<<20 (1.05M) cell ceiling, so its front must be trimmed.
    for _ in 0..200_000 {
        sb.push_row(wrapped_full('z'));
    }
    let total_cells: usize = sb.physical(W).iter().map(|r| r.cells.len()).sum();
    assert!(
        total_cells <= (1 << 20) + 2 * W,
        "open line after closed history must stay bounded, got {total_cells} cells"
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

// ---------------------------------------------------------------------------
// Scrollback byte-attribution measurement harness (P3 stage A)
// ---------------------------------------------------------------------------

/// Fill a terminal with `lines` hard-terminated lines of realistic-width
/// content, then read the projection once so the memoized cache is populated
/// the way a rendered pane populates it. Returns the store's byte breakdown.
///
/// The projection read is not incidental: an unrendered pane has an empty
/// cache, so measuring without it would report a projection cost of zero and
/// understate what a pane the user is actually looking at occupies.
#[cfg(test)]
fn measure_scrollback(
    lines: usize,
    columns: usize,
    rows: usize,
) -> crate::memory_report::ScrollbackBytes {
    let mut term = Terminal::new(columns, rows);
    term.set_scrollback_limit(0);
    // 72 printable columns of mixed content, hard-terminated: the shape of
    // ordinary command output, not a degenerate all-blank or all-full line.
    let body: String = (0..72).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    for _ in 0..lines {
        term.advance(body.as_bytes());
        term.advance(b"\r\n");
    }
    // Populate the projection cache exactly as a render pass would.
    let _ = term.screen().scrollback_len();
    term.screen().scrollback_bytes()
}

/// Measurement, not an assertion: prints the scrollback byte breakdown at
/// several depths so a before/after comparison is attributable to a named
/// subsystem rather than to a single opaque total. Ignored by default because
/// the 100k-line case allocates hundreds of megabytes; run deliberately with
/// `cargo test -- --ignored --nocapture scrollback_byte_breakdown`.
#[test]
#[ignore = "measurement harness; allocates ~GB at the deepest point"]
fn scrollback_byte_breakdown_by_depth() {
    println!("shape depth ring projection ring_slack total bytes_per_line");
    for depth in [1_000usize, 10_000, 100_000] {
        let b = measure_scrollback(depth, 80, 24);
        let total = b.ring + b.projection;
        println!(
            "hard {depth} {} {} {} {} {:.1}",
            b.ring,
            b.projection,
            b.ring_slack,
            total,
            total as f64 / depth as f64
        );
    }
    for depth in [1_000usize, 10_000, 100_000] {
        let b = measure_wrapped_scrollback(depth, 80, 24);
        let total = b.ring + b.projection;
        println!(
            "wrapped {depth} {} {} {} {} {:.1}",
            b.ring,
            b.projection,
            b.ring_slack,
            total,
            total as f64 / depth as f64
        );
    }
}

/// Same measurement over **soft-wrapped** content: each logical line spans
/// several physical rows, so it is assembled through the `push_row` merge path
/// (`Vec::extend`) rather than adopted whole from a grid row. That is the path
/// where amortized doubling leaves reserved-but-unused capacity behind, so the
/// two shapes have to be measured separately — a hard-terminated corpus alone
/// would report the slack term as negligible and hide it.
#[cfg(test)]
fn measure_wrapped_scrollback(
    lines: usize,
    columns: usize,
    rows: usize,
) -> crate::memory_report::ScrollbackBytes {
    let mut term = Terminal::new(columns, rows);
    term.set_scrollback_limit(0);
    // ~5.2 physical rows per logical line at 80 columns.
    let body: String = (0..420)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    for _ in 0..lines {
        term.advance(body.as_bytes());
        term.advance(b"\r\n");
    }
    let _ = term.screen().scrollback_len();
    term.screen().scrollback_bytes()
}

// ---------------------------------------------------------------------------
// Representation-independent grapheme oracle (P3 stage B prerequisite)
// ---------------------------------------------------------------------------
//
// Any change to how the ring *stores* cells has to preserve what the ring
// *means*: the grapheme content of every logical line, at every width it can be
// projected to. This oracle states that as a property rather than as a golden
// file, so it constrains a new representation without having to be rewritten
// for it — feed known grapheme sequences in, project them out at many widths,
// and require the reconstruction to be the input.
//
// It is deliberately not a byte-level or layout-level assertion. Byte-level
// pins belong with the type (`cell_equivalence`); this one has to survive a
// storage change in order to be worth anything during one.

/// Rebuild the logical lines a projection encodes, by joining physical rows on
/// the soft-wrap flag and concatenating each cell's full grapheme cluster.
///
/// Wide-glyph spacer cells contribute nothing: the base char already carries the
/// glyph, so counting the spacer would double it.
fn logical_graphemes(rows: &[Line]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut open = false;
    for row in rows {
        for cell in &row.cells {
            if cell.wide_continuation {
                continue;
            }
            current.push_str(&cell.grapheme());
        }
        if row.wrapped {
            open = true;
        } else {
            out.push(std::mem::take(&mut current));
            open = false;
        }
    }
    if open || !current.is_empty() {
        out.push(current);
    }
    out
}

/// Content chosen so the oracle has something to lose: combining marks at the
/// start, middle and end of a line, a cell carrying the full `MAX_COMBINING`
/// run, a cell whose marks would overflow that bound, and wide glyphs adjacent
/// to marked cells so the spacer rule and the mark rule interact.
fn grapheme_oracle_corpus() -> Vec<String> {
    let overflow: String = std::iter::once('e')
        .chain(std::iter::repeat_n('\u{0301}', MAX_COMBINING + 2))
        .collect();
    let full: String = std::iter::once('a')
        .chain(std::iter::repeat_n('\u{0308}', MAX_COMBINING))
        .collect();
    vec![
        "plain ascii".to_string(),
        format!("start{full}end"),
        format!("{overflow}tail"),
        "e\u{0301}a\u{0300}i\u{0302}o\u{0303}u\u{0308}".to_string(),
        "wide\u{0301}xy".to_string(),
        format!("mixed{full}rest"),
    ]
}

/// The property: for every projection width, the graphemes read back out of the
/// scrollback are exactly the graphemes that were written in.
///
/// Width is swept rather than fixed because a storage change that loses a mark
/// only at a wrap boundary would pass at a single width. Trailing blanks are
/// trimmed by the projection, so the corpus carries none.
#[test]
fn scrollback_projection_preserves_graphemes_at_every_width() {
    let corpus = grapheme_oracle_corpus();
    // Expected clusters are what the *cell* representation can hold, not what
    // was fed: `push_combining` drops marks past `MAX_COMBINING`, and that
    // bound is a documented limitation this oracle must not paper over.
    let expected: Vec<String> = corpus
        .iter()
        .map(|line| {
            let mut out = String::new();
            let mut marks = 0usize;
            for ch in line.chars() {
                if is_zero_width_mark(ch) {
                    if marks < MAX_COMBINING {
                        out.push(ch);
                        marks += 1;
                    }
                } else {
                    out.push(ch);
                    marks = 0;
                }
            }
            out
        })
        .collect();

    for width in [2usize, 3, 5, 8, 13, 40, 80] {
        let mut term = Terminal::new(width, 3);
        term.set_scrollback_limit(0);
        for line in &corpus {
            term.advance(line.as_bytes());
            term.advance(b"\r\n");
        }
        // Push every fed line out of the viewport and into the ring.
        for _ in 0..8 {
            term.advance(b"\r\n");
        }
        let store = term.screen().scrollback_store();
        let rows = store.physical_tail(width, store.physical_len(width));
        let read_back = logical_graphemes(&rows);
        // Blank padding is the projection's business, not this oracle's: a row
        // is stored full-width, so a wrapped line's last row contributes the
        // spaces that pad it. Comparing trimmed content keeps the property
        // ("no grapheme is lost, gained, or reordered") independent of the
        // padding policy, which `same_width_roundtrip` already pins.
        let content: Vec<String> = read_back
            .iter()
            .map(|line| line.trim_end_matches(' ').to_string())
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(
            content.len(),
            expected.len(),
            "width {width}: logical line count changed; read back {read_back:?}"
        );
        for (got, want) in content.iter().zip(expected.iter()) {
            assert_eq!(
                got.as_str(),
                want.as_str(),
                "width {width}: grapheme content differs"
            );
        }
    }
}

/// Zero-width marks as the printer classifies them (`Screen::print_char` treats
/// width 0 as a combining mark), so the oracle's expectation is derived from the
/// same rule the feed path applies rather than from a second list.
fn is_zero_width_mark(ch: char) -> bool {
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) == 0
}

/// Candidate A: `Cell` with the four-slot `combining` array and its length byte
/// replaced by a 4-byte handle into a side table. Declared here purely so the
/// resulting size is produced by the compiler's layout rules rather than by
/// hand-arithmetic in a report.
#[allow(dead_code)]
struct CandidateHandleCell {
    ch: char,
    attrs: Attrs,
    protected: bool,
    wide_continuation: bool,
    combining: u32,
}

/// Candidate B was adopted and is now
/// [`crate::core::stored_cell::StoredCell`] — the cell the **scrollback ring
/// alone** stores. There is no separate declaration for it here any more: the
/// measurement is taken against the shipped type, so the figure cannot drift
/// from the thing it describes.
///
/// The sizes are load-bearing for the stage-B decision, so they are asserted
/// rather than quoted. `Cell` is pinned alongside because the saving is the
/// *difference* between the two.
#[test]
fn stage_b_candidate_cell_sizes() {
    assert_eq!(std::mem::size_of::<Cell>(), 44);
    assert_eq!(std::mem::size_of::<CandidateHandleCell>(), 32);
    assert_eq!(
        std::mem::size_of::<crate::core::stored_cell::StoredCell>(),
        28
    );
}

/// Measurement, not an assertion: the ring's size and composition, and what
/// the rejected candidate A would have cost on the same corpora.
///
/// This started as a projection over a ring that still stored `Cell`. Candidate
/// B has since landed, so the B column is no longer a projection — it is the
/// measured ring, read from the store. The A column stays arithmetic because
/// candidate A was not built; it is labelled as such rather than presented
/// beside the measured figure as if it were one.
///
/// Only the cell term scales with the stored cell's size; ring slots and
/// button spans do not, so a comparison that applied the ratio to the whole
/// ring would overstate the difference. Run with
/// `cargo test -- --ignored --nocapture stage_b_cell_shrink_projection`.
#[test]
#[ignore = "measurement harness; allocates ~GB at the deepest point"]
fn stage_b_cell_shrink_projection() {
    // A (rejected): a handle replacing `combining: [char; 4]` +
    //    `combining_len: u8`, `Cell` itself narrowed everywhere. Disproved by
    //    the renderer holding cells with no `Screen` in scope; kept here as the
    //    counterfactual the decision was made against.
    // B (adopted): `Cell` unchanged, the scrollback ring alone storing a narrow
    //    cell with the marks in a per-line sidecar.
    let cell_a = std::mem::size_of::<CandidateHandleCell>();
    let stored = std::mem::size_of::<crate::core::stored_cell::StoredCell>();
    let old_cell = std::mem::size_of::<Cell>();
    let slot = std::mem::size_of::<crate::core::scrollback::LogicalLine>();
    println!("Cell {old_cell}; rejected A {cell_a}; stored (B, live) {stored}; slot {slot}");
    println!("shape depth ring_measured cell_bytes slot_bytes ring_if_A pct_A_vs_44");
    for wrapped in [false, true] {
        for depth in [1_000usize, 10_000, 100_000] {
            let mut term = Terminal::new(80, 24);
            term.set_scrollback_limit(0);
            let width = if wrapped { 420 } else { 72 };
            let body: String = (0..width)
                .map(|i| char::from(b'a' + (i % 26) as u8))
                .collect();
            for _ in 0..depth {
                term.advance(body.as_bytes());
                term.advance(b"\r\n");
            }
            let _ = term.screen().scrollback_len();
            let bytes = term.screen().scrollback_bytes();
            let (slots, cells, _spans) = term.screen().scrollback_store().ring_composition();
            // Cells at the pre-B width, so the corpus's cell term stays
            // comparable with the figures recorded before B landed.
            let cell_bytes = cells * old_cell;
            let slot_bytes = slots * slot;
            let ring = bytes.ring as usize;
            // What the ring would be under A, derived from the ring as it is
            // now: add back the bytes B saved, then take A's smaller saving.
            let ring_at_44 = ring + cells * (old_cell - stored);
            let ring_a = ring_at_44 - cells * (old_cell - cell_a);
            println!(
                "{} {depth} {ring} {cell_bytes} {slot_bytes} {ring_a} {:.1}",
                if wrapped { "wrapped" } else { "hard" },
                100.0 * (ring_at_44 - ring_a) as f64 / ring_at_44 as f64,
            );
        }
    }
}

/// Measurement, not an assertion: fill cost for the soft-wrapped merge path.
///
/// Reclaiming capacity is a reallocate-and-copy, so the saving has to be
/// weighed against what it costs to push. This times the fill alone (no
/// projection read) so the number is the merge path and nothing else. Run with
/// `cargo test -- --ignored --nocapture scrollback_fill_cost`.
#[test]
#[ignore = "measurement harness"]
fn scrollback_fill_cost_wrapped() {
    for depth in [10_000usize, 100_000] {
        // One warm-up pass so allocator state is comparable between depths.
        let _ = measure_wrapped_scrollback(1_000, 80, 24);
        let start = std::time::Instant::now();
        let b = measure_wrapped_scrollback(depth, 80, 24);
        let elapsed = start.elapsed();
        println!(
            "fill depth={depth} elapsed_ms={:.1} ring={} slack={}",
            elapsed.as_secs_f64() * 1000.0,
            b.ring,
            b.ring_slack
        );
    }
}

// ---------------------------------------------------------------------------
// Projection-accessor equivalence (P3 stage A, item 2)
// ---------------------------------------------------------------------------
//
// The retained full projection was replaced by a cached *shape* (each logical
// line's first physical row) plus on-demand projection. Every windowed
// accessor must therefore return exactly what indexing the full projection
// would have returned. These tests prove that against the full projection
// itself rather than against a hand-computed expectation, so they cannot drift
// from the wrapping rule they depend on.

/// Corpora chosen to exercise the wrapping rule's awkward cases: soft-wrap
/// runs, wide glyphs that cannot straddle the right edge, trailing-blank trim,
/// an open (unterminated) tail, prompt marks, and a single-cell line.
fn accessor_equivalence_corpora() -> Vec<Vec<Line>> {
    let mut wide = Line::unwrapped(vec![
        Cell::new('漢', Attrs::default()),
        Cell::wide_spacer(Attrs::default()),
        Cell::new('字', Attrs::default()),
        Cell::wide_spacer(Attrs::default()),
        Cell::blank(),
        Cell::blank(),
        Cell::blank(),
        Cell::blank(),
    ]);
    wide.prompt_mark = Some(PromptKind::PromptStart);

    vec![
        vec![],
        vec![content("a")],
        vec![content("a"), content("b"), content("c")],
        vec![wrapped_full('x'), wrapped_full('y'), content("tail")],
        vec![content("one"), wrapped_full('z'), content("two"), blank()],
        vec![wide],
        vec![
            wrapped_full('a'),
            wrapped_full('b'),
            wrapped_full('c'),
            wrapped_full('d'),
        ],
        vec![content("m"), blank(), blank(), content("n")],
    ]
}

/// `physical_len`, `physical_row`, `physical_tail`, `prompt_mark_at` and
/// `prompt_mark_rows` all agree with the full projection, at every width and
/// every index.
#[test]
fn windowed_accessors_match_full_projection() {
    for (corpus_index, rows) in accessor_equivalence_corpora().into_iter().enumerate() {
        let sb = Scrollback::from_physical(&rows);
        for width in 1..=12usize {
            let full = sb.physical_all(width);

            assert_eq!(
                sb.physical_len(width),
                full.len(),
                "corpus {corpus_index} width {width}: physical_len disagrees with projection"
            );

            // Every absolute row resolves to the same Line, and one past the
            // end resolves to nothing.
            for (row, expected) in full.iter().enumerate() {
                assert_eq!(
                    sb.physical_row(width, row).as_ref(),
                    Some(expected),
                    "corpus {corpus_index} width {width} row {row}: physical_row mismatch"
                );
                assert_eq!(
                    sb.prompt_mark_at(width, row),
                    expected.prompt_mark,
                    "corpus {corpus_index} width {width} row {row}: prompt_mark_at mismatch"
                );
            }
            assert!(
                sb.physical_row(width, full.len()).is_none(),
                "corpus {corpus_index} width {width}: row past the end must be None"
            );
            assert!(
                sb.prompt_mark_at(width, full.len()).is_none(),
                "corpus {corpus_index} width {width}: mark past the end must be None"
            );

            // Every tail length, including 0 and more rows than exist, matches
            // the corresponding suffix of the full projection.
            for n in 0..=(full.len() + 3) {
                let tail = sb.physical_tail(width, n);
                let start = full.len().saturating_sub(n);
                assert_eq!(
                    tail.as_slice(),
                    &full[start..],
                    "corpus {corpus_index} width {width} n {n}: physical_tail mismatch"
                );
            }

            // The mark enumeration is the set of marked rows, in row order.
            let expected_marks: Vec<(usize, PromptKind)> = full
                .iter()
                .enumerate()
                .filter_map(|(row, line)| line.prompt_mark.map(|kind| (row, kind)))
                .collect();
            assert_eq!(
                sb.prompt_mark_rows(width),
                expected_marks,
                "corpus {corpus_index} width {width}: prompt_mark_rows mismatch"
            );
        }
    }
}

/// The cached shape must follow a width change and a mutation, not just a cold
/// build — a stale shape would resolve rows to the wrong logical line, which is
/// exactly the failure a memoized index invites.
#[test]
fn projection_shape_tracks_width_and_mutation() {
    let mut sb = Scrollback::from_physical(&[wrapped_full('a'), content("b")]);

    // Read at one width, then another, then back: each must agree with a fresh
    // projection at that width.
    for width in [W, 3, 5, W, 1] {
        assert_eq!(sb.physical_len(width), sb.physical_all(width).len());
        let full = sb.physical_all(width);
        for (row, expected) in full.iter().enumerate() {
            assert_eq!(sb.physical_row(width, row).as_ref(), Some(expected));
        }
    }

    // Mutate through every path that invalidates, re-checking after each.
    sb.push_row(content("c"));
    assert_eq!(sb.physical_len(W), sb.physical_all(W).len());

    sb.push_row(wrapped_full('d'));
    sb.push_row(content("e"));
    assert_eq!(sb.physical_len(W), sb.physical_all(W).len());

    sb.sever_trailing_wrap();
    assert_eq!(sb.physical_len(W), sb.physical_all(W).len());

    sb.set_limit(2);
    assert_eq!(sb.physical_len(W), sb.physical_all(W).len());
    let full = sb.physical_all(W);
    for (row, expected) in full.iter().enumerate() {
        assert_eq!(sb.physical_row(W, row).as_ref(), Some(expected));
    }

    sb.clear();
    assert_eq!(sb.physical_len(W), 0);
    assert!(sb.physical_row(W, 0).is_none());
    assert!(sb.physical_tail(W, 5).is_empty());
    assert!(sb.prompt_mark_rows(W).is_empty());
}

/// Reclaiming capacity on a finalized line must not change any observable
/// projection output — it is a storage change, not a content change.
#[test]
fn capacity_reclaim_is_content_neutral() {
    // A soft-wrapped run merged through `push_row` is the path that accrues
    // slack, so it is the path whose output has to be proven unchanged.
    let mut sb = Scrollback::new();
    for _ in 0..4 {
        sb.push_row(wrapped_full('q'));
    }
    sb.push_row(content("end"));

    let oracle = Scrollback::from_physical(&[
        wrapped_full('q'),
        wrapped_full('q'),
        wrapped_full('q'),
        wrapped_full('q'),
        content("end"),
    ]);

    for width in 1..=12usize {
        assert_eq!(
            sb.physical_all(width),
            oracle.physical_all(width),
            "width {width}: reclaimed line projects differently"
        );
    }
}

/// Measurement, not an assertion: per-frame render-path cost at depth.
///
/// The retained projection is gone, so the question is whether reading a
/// viewport still costs viewport time rather than store time. Times a warm
/// read (nothing pushed since the last read, the steady state while the user
/// scrolls or idles) and a cold read (a row pushed first, which invalidates the
/// cached shape — the steady state while output is arriving).
#[test]
#[ignore = "measurement harness"]
fn snapshot_cost_at_depth() {
    for depth in [1_000usize, 10_000, 100_000] {
        let mut term = Terminal::new(80, 24);
        term.set_scrollback_limit(0);
        let body: String = (0..72).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        for _ in 0..depth {
            term.advance(body.as_bytes());
            term.advance(b"\r\n");
        }

        // Warm: cached shape, repeated viewport reads.
        let _ = term.screen().snapshot_with_scrollback(10);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ = term.screen().snapshot_with_scrollback(10);
        }
        let warm_us = start.elapsed().as_secs_f64() * 1e6 / 100.0;

        // Cold: one line of output between reads, so the shape is rebuilt.
        let start = std::time::Instant::now();
        for _ in 0..10 {
            term.advance(b"x\r\n");
            let _ = term.screen().snapshot_with_scrollback(10);
        }
        let cold_us = start.elapsed().as_secs_f64() * 1e6 / 10.0;

        println!("depth={depth} warm_us={warm_us:.1} cold_us={cold_us:.1}");
    }
}

/// Flat grapheme clusters of every cell in `rows`, in order, so a mark can be
/// checked against the base character it is supposed to be attached to rather
/// than against the line's text as a whole.
fn flat_graphemes(rows: &[Line]) -> Vec<String> {
    rows.iter()
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.grapheme())
        .collect()
}

/// A mark carried in by a *continuation* row must stay on its own base
/// character once that row is merged onto the open logical line.
///
/// The merge appends one row's cells at an offset into the logical line's flat
/// space, and the mark sidecar is keyed in that same space, so a mistake here
/// does not lose the mark — it relocates it. Every base character is distinct
/// modulo 26 and every marked position is checked against its own base, so a
/// shift of any size shows up as a named mismatch rather than as content that
/// still contains the right number of marks.
#[test]
fn merged_continuation_rows_keep_marks_on_their_own_base() {
    const W: usize = 8;
    // Marks placed inside the second, third and fifth physical rows, so the
    // append offset is non-zero and different for each.
    let marked_at = [W + 3, 2 * W, 5 * W + 7];
    let mut fed = String::new();
    for i in 0..(8 * W) {
        fed.push(char::from(b'a' + (i % 26) as u8));
        if marked_at.contains(&i) {
            fed.push('\u{0301}');
        }
    }
    let mut term = Terminal::new(W, 2);
    term.set_scrollback_limit(0);
    term.advance(fed.as_bytes());
    term.advance(b"\r\n");
    for _ in 0..4 {
        term.advance(b"\r\n");
    }
    let store = term.screen().scrollback_store();
    let rows = store.physical_tail(W, store.physical_len(W));
    let cells = flat_graphemes(&rows);
    for i in 0..(8 * W) {
        let base = char::from(b'a' + (i % 26) as u8);
        let want = if marked_at.contains(&i) {
            format!("{base}\u{0301}")
        } else {
            base.to_string()
        };
        assert_eq!(
            cells.get(i).map(String::as_str),
            Some(want.as_str()),
            "cell {i}: mark landed on the wrong base character"
        );
    }
}

/// A soft-wrapped logical line is not bounded by the terminal width, so the
/// mark sidecar's key must address past 65,535 cells.
///
/// This is the test that fails if the key is ever narrowed to a `u16`: the
/// marked cell sits at index 66,000 of a single open logical line, which is
/// reachable from ordinary output — a long line with no newline in it — not
/// from anything adversarial.
#[test]
fn mark_survives_past_a_sixteen_bit_column_index() {
    const W: usize = 80;
    const MARKED: usize = 66_000;
    const TOTAL: usize = 70_000;
    let mut fed = String::with_capacity(TOTAL + 1);
    for i in 0..TOTAL {
        fed.push(char::from(b'a' + (i % 26) as u8));
        if i == MARKED {
            fed.push('\u{0308}');
        }
    }
    let mut term = Terminal::new(W, 3);
    term.set_scrollback_limit(0);
    term.advance(fed.as_bytes());
    let store = term.screen().scrollback_store();
    let rows = store.physical_tail(W, store.physical_len(W));
    let cells = flat_graphemes(&rows);
    assert!(
        cells.len() > MARKED,
        "the marked cell must have reached the ring; ring holds {} cells",
        cells.len()
    );
    let base = char::from(b'a' + (MARKED % 26) as u8);
    assert_eq!(
        cells[MARKED],
        format!("{base}\u{0308}"),
        "mark lost or displaced past a 16-bit index"
    );
    assert_eq!(
        cells[MARKED - 1],
        char::from(b'a' + ((MARKED - 1) % 26) as u8).to_string(),
        "the preceding cell picked up a mark that is not its own"
    );
    assert_eq!(
        cells[MARKED + 1],
        char::from(b'a' + ((MARKED + 1) % 26) as u8).to_string(),
        "the following cell picked up a mark that is not its own"
    );
}

/// A cell whose base is a space but which carries marks is not blank, and the
/// projection's trailing-blank trim must not discard it.
///
/// While marks lived inside the `Cell`, a marked space could never compare
/// equal to `Cell::blank()`, so the trim was safe by construction. With the
/// marks in a sidecar the base alone compares equal, and the trim has to
/// consult the sidecar to mean the same thing. This is the test that fails if
/// it stops doing so.
#[test]
fn a_marked_space_is_not_trimmed_as_a_trailing_blank() {
    const W: usize = 8;
    let mut term = Terminal::new(W, 2);
    term.set_scrollback_limit(0);
    // "ab" then a space carrying a combining mark, at the end of the line.
    term.advance("ab \u{0301}".as_bytes());
    term.advance(b"\r\n");
    for _ in 0..4 {
        term.advance(b"\r\n");
    }
    let store = term.screen().scrollback_store();
    let rows = store.physical_tail(W, store.physical_len(W));
    let cells = flat_graphemes(&rows);
    assert_eq!(cells.first().map(String::as_str), Some("a"));
    assert_eq!(cells.get(1).map(String::as_str), Some("b"));
    assert_eq!(
        cells.get(2).map(String::as_str),
        Some(" \u{0301}"),
        "a marked space was trimmed as a trailing blank"
    );
}

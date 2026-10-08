// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for the pane layout and chrome geometry in `panes.rs`.

use super::*;
use crate::atlas::CellSize;
use crate::native::app::TAB_BAR_ROWS;
use crate::native::viewport::grid_dimensions_for_with_padding;

fn cell() -> CellSize {
    CellSize {
        width: 10,
        height: 20,
        baseline: 0,
    }
}

#[test]
fn tab_bar_filler_rows_copy_only_background_attributes() {
    let mut attrs = crate::core::Attrs::default();
    attrs.background = crate::core::Color::Rgb(10, 20, 30);
    attrs.foreground = crate::core::Color::Rgb(220, 230, 240);
    attrs.set_bold(true);
    attrs.set_underline(true);
    attrs.set_strikethrough(true);
    let glyph = super::super::tab_bar::TabBarGlyph {
        col: 0,
        ch: 'g',
        attrs,
    };
    let mut cells = vec![crate::core::Cell::default(); 3];
    place_tab_bar_glyphs(&mut cells, vec![glyph], 1, 3, 0);

    assert_eq!(cells[1].ch, 'g');
    assert!(cells[1].attrs.underline());
    assert!(cells[1].attrs.strikethrough());
    for filler in [&cells[0], &cells[2]] {
        assert_eq!(filler.ch, ' ');
        assert_eq!(filler.attrs.background, attrs.background);
        assert!(!filler.attrs.bold());
        assert!(!filler.attrs.underline());
        assert!(!filler.attrs.strikethrough());
    }
}

#[test]
fn single_pane_content_rect_matches_the_legacy_grid_math() {
    // The pane content rect's cell dimensions must equal the dims the
    // single-pane resize path produces, so a lone-leaf tab stays
    // byte-identical (no tab bar case).
    let cell = cell();
    let padding = WindowPadding::from_logical(8.0, 1.0);
    let (w, h) = (1280u32, 800u32);
    let rect = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
    let (cols, rows) = crate::native::layout::grid_dims_for_rect(rect, cell.width, cell.height);
    let legacy = grid_dimensions_for_with_padding(w, h, cell, padding);
    assert_eq!((cols, rows), (legacy.columns, legacy.rows));
}

#[test]
fn tab_bar_shrinks_the_content_rect_by_exactly_the_strip() {
    let cell = cell();
    let padding = WindowPadding::from_logical(8.0, 1.0);
    let (w, h) = (1280u32, 800u32);
    let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
    let with = pane_content_rect(w, h, cell, padding, TabReserve::top());
    // Same width and x; the strip eats `TAB_BAR_ROWS` cell-heights PLUS the
    // chrome-facing padding gap off the top, shifting y down and reducing
    // height by the same amount (CHROME-GAP: content never touches chrome).
    assert!((without.w - with.w).abs() < f32::EPSILON);
    assert!((without.x - with.x).abs() < f32::EPSILON);
    let strip = cell.height as f32 * TAB_BAR_ROWS as f32 + padding.as_f32();
    assert!((with.y - (without.y + strip)).abs() < f32::EPSILON);
    assert!((with.h - (without.h - strip)).abs() < f32::EPSILON);
}

#[test]
fn left_rail_shrinks_the_content_rect_by_the_rail_width_plus_the_gap() {
    // F4-V2 + CHROME-GAP: a left rail reserves `left_cols` off the LEFT and
    // the window padding opens between band and content, so the content
    // x-origin shifts right by rail + gap and the width shrinks by the
    // same; height and y are unchanged (the rail reserves columns, not
    // rows).
    let cell = cell();
    let padding = WindowPadding::from_logical(8.0, 1.0);
    let (w, h) = (1280u32, 800u32);
    let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
    let reserve = TabReserve {
        top_rows: 0,
        left_cols: 16,
        right_cols: 0,
        gap_cols: 0,
    };
    let with = pane_content_rect(w, h, cell, padding, reserve);
    let rail = cell.width as f32 * 16.0 + padding.as_f32();
    assert!((with.y - without.y).abs() < f32::EPSILON, "y unchanged");
    assert!(
        (with.h - without.h).abs() < f32::EPSILON,
        "height unchanged"
    );
    assert!(
        (with.x - (without.x + rail)).abs() < f32::EPSILON,
        "x shifts right by the rail width plus the gap"
    );
    assert!(
        (with.w - (without.w - rail)).abs() < f32::EPSILON,
        "width shrinks by the rail width plus the gap"
    );
}

#[test]
fn right_rail_shrinks_content_from_the_right_with_the_band_a_gap_away() {
    // F4-P2 layout mirror + CHROME-GAP: a right rail reserves `right_cols`
    // off the RIGHT plus the chrome-facing gap: the content width shrinks
    // by band + gap but its x-origin stays put (content on the LEFT), the
    // mirror of the left rail (which shifts the origin right). y/height are
    // unchanged (a rail reserves columns, not rows).
    let cell = cell();
    let padding = WindowPadding::from_logical(8.0, 1.0);
    let (w, h) = (1280u32, 800u32);
    let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
    let reserve = TabReserve {
        top_rows: 0,
        left_cols: 0,
        right_cols: 16,
        gap_cols: 0,
    };
    let with = pane_content_rect(w, h, cell, padding, reserve);
    let reserved = cell.width as f32 * 16.0 + padding.as_f32();
    assert!(
        (with.x - without.x).abs() < f32::EPSILON,
        "x-origin stays put (content on the left)"
    );
    assert!((with.y - without.y).abs() < f32::EPSILON, "y unchanged");
    assert!(
        (with.h - without.h).abs() < f32::EPSILON,
        "height unchanged"
    );
    assert!(
        (with.w - (without.w - reserved)).abs() < f32::EPSILON,
        "width shrinks from the right by the rail band plus the gap"
    );
    assert_eq!(reserve.left_reserved_cols(), 0);
    assert_eq!(reserve.right_reserved_cols(), 16);
}

#[test]
fn zero_padding_keeps_every_chrome_band_flush() {
    // CHROME-GAP flush identity: at zero window padding there is no
    // chrome-facing gap either: every reserve reproduces the historical
    // flush geometry exactly (byte-identical at padding 0).
    let cell = cell();
    let padding = WindowPadding::ZERO;
    let (w, h) = (1280u32, 800u32);
    let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
    for reserve in [
        TabReserve::top(),
        TabReserve {
            top_rows: 0,
            left_cols: 16,
            right_cols: 0,
            gap_cols: 0,
        },
        TabReserve {
            top_rows: 2,
            left_cols: 0,
            right_cols: 16,
            gap_cols: 0,
        },
    ] {
        assert_eq!(reserve.chrome_gap(padding), ChromeGap::default());
        let with = pane_content_rect(w, h, cell, padding, reserve);
        let rail_w = cell.width as f32
            * (reserve.left_reserved_cols() + reserve.right_reserved_cols()) as f32;
        let bar_h = cell.height as f32 * reserve.top_rows as f32;
        assert!((with.w - (without.w - rail_w)).abs() < f32::EPSILON);
        assert!((with.h - (without.h - bar_h)).abs() < f32::EPSILON);
        let left_w = cell.width as f32 * reserve.left_reserved_cols() as f32;
        assert!((with.x - (without.x + left_w)).abs() < f32::EPSILON);
        assert!((with.y - (without.y + bar_h)).abs() < f32::EPSILON);
    }
}

#[test]
fn chrome_gap_tracks_each_shown_band_at_the_padding_value() {
    // CHROME-GAP: each SHOWN band gets a gap equal to the window padding;
    // absent bands get none, so nothing changes where no chrome is pinned.
    let padding = WindowPadding::from_logical(8.0, 1.0);
    assert_eq!(
        TabReserve::NONE.chrome_gap(padding),
        ChromeGap::default(),
        "no chrome, no gap"
    );
    let both = TabReserve {
        top_rows: 2,
        left_cols: 16,
        right_cols: 0,
        gap_cols: 0,
    };
    assert_eq!(
        both.chrome_gap(padding),
        ChromeGap {
            left: 8.0,
            right: 0.0,
            top: 8.0,
        }
    );
    let right = TabReserve {
        top_rows: 0,
        left_cols: 0,
        right_cols: 12,
        gap_cols: 0,
    };
    assert_eq!(
        right.chrome_gap(padding),
        ChromeGap {
            left: 0.0,
            right: 8.0,
            top: 0.0,
        }
    );
}

#[test]
fn gap_cols_off_the_top_bar_reserves_nothing_extra() {
    // The top-bar reservation carries no side gap, so the content columns are
    // unchanged (byte-identical top-bar path).
    let r = TabReserve::top();
    assert_eq!(r.left_reserved_cols(), 0);
    assert_eq!(r.right_reserved_cols(), 0);
}

#[test]
fn pane_focus_dim_focused_is_always_identity() {
    // The focused pane is never dimmed regardless of the configured amount.
    assert_eq!(pane_focus_dim(true, 0.0), 0.0);
    assert_eq!(pane_focus_dim(true, 0.3), 0.0);
    assert_eq!(pane_focus_dim(true, 1.0), 0.0);
}

#[test]
fn pane_focus_dim_inactive_uses_configured_amount() {
    // Non-focused panes recede by exactly the configured value.
    assert_eq!(pane_focus_dim(false, 0.25), 0.25);
    assert_eq!(pane_focus_dim(false, 1.0), 1.0);
}

#[test]
fn pane_focus_dim_off_is_byte_identical_for_every_pane() {
    // The default-off path: with `inactive_dim == 0.0` both the focused and
    // inactive panes get `0.0`, identical to the pre-feature hardcoded
    // value, so the multi-pane frame is byte-identical. The grid layer
    // already proves `focus_dim == 0.0` is an exact no-op.
    assert_eq!(pane_focus_dim(true, 0.0), 0.0);
    assert_eq!(pane_focus_dim(false, 0.0), 0.0);
}

// --- Window-level overlay geometry (multi-pane) ---

fn filled_snapshot(cols: usize, rows: usize, ch: char) -> Snapshot {
    Snapshot {
        dimensions: Dimensions::new(cols, rows),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: crate::core::DynamicColors::default(),
        cells: (0..cols * rows)
            .map(|i| {
                // Encode the linear index so the crop can be checked cell-wise.
                let c = char::from_u32('a' as u32 + (i as u32 % 26)).unwrap_or(ch);
                crate::core::Cell::new(c, crate::core::Attrs::default())
            })
            .collect(),
    }
}

#[test]
fn crop_snapshot_copies_the_requested_subrect() {
    // A 6x4 source cropped to a 3x2 box at (left=2, top=1) yields exactly
    // those cells, in order, with the cropped dimensions.
    let src = filled_snapshot(6, 4, 'x');
    let cropped = crop_snapshot(&src, 2, 1, 3, 2);
    assert_eq!(cropped.dimensions, Dimensions::new(3, 2));
    for r in 0..2 {
        for c in 0..3 {
            let src_idx = (1 + r) * 6 + (2 + c);
            let dst_idx = r * 3 + c;
            assert_eq!(
                cropped.cells[dst_idx].ch, src.cells[src_idx].ch,
                "cell ({r},{c}) mismatch"
            );
        }
    }
}

#[test]
fn crop_snapshot_out_of_bounds_falls_back_to_default() {
    // A crop that runs past the source edge fills the overflow with default
    // cells rather than panicking (defensive path).
    let src = filled_snapshot(3, 3, 'x');
    let cropped = crop_snapshot(&src, 2, 2, 3, 3);
    assert_eq!(cropped.dimensions, Dimensions::new(3, 3));
    // Top-left came from src (2,2); the rest overflow to default (space).
    assert_eq!(cropped.cells[0].ch, src.cells[2 * 3 + 2].ch);
    let default_ch = crate::core::Cell::default().ch;
    assert_eq!(cropped.cells[8].ch, default_ch);
}

#[test]
fn crop_snapshot_past_the_right_edge_never_reads_the_next_row() {
    // Columns past the source width fall back to default even when the flat
    // index lands on a real cell of the following row.
    let src = filled_snapshot(3, 3, 'x');
    let cropped = crop_snapshot(&src, 2, 0, 2, 1);
    assert_eq!(cropped.cells[0].ch, src.cells[2].ch, "in-bounds (0,2)");
    assert_eq!(
        cropped.cells[1],
        crate::core::Cell::default(),
        "(0,3) is outside the source, not row 1 column 0"
    );
}

#[test]
fn window_overlay_cell_maps_into_the_content_grid() {
    // Content rect offset from the window origin by (x=10, y=40), for example a
    // tab bar pushes y down. A pointer inside maps to the content-grid cell
    // relative to that origin, NOT the raw window origin.
    let cell = cell(); // 10x20
    let content = PaneRect::new(10.0, 40.0, 200.0, 200.0); // 20x10 cells
    // Pointer at window px (35, 75): col = (35-10)/10 = 2, row = (75-40)/20 = 1.
    let mapped = window_overlay_cell(content, cell, 35.0, 75.0).expect("cell");
    assert_eq!(mapped, CellPoint { row: 1, column: 2 });
}

#[test]
fn window_overlay_cell_clamps_to_grid_bounds() {
    let cell = cell();
    let content = PaneRect::new(10.0, 40.0, 200.0, 200.0); // 20x10 cells
    // Far past the bottom-right: clamps to the last cell.
    let mapped = window_overlay_cell(content, cell, 9000.0, 9000.0).expect("cell");
    assert_eq!(mapped, CellPoint { row: 9, column: 19 });
    // Above/left of the content origin clamps to (0,0).
    let mapped = window_overlay_cell(content, cell, 0.0, 0.0).expect("cell");
    assert_eq!(mapped, CellPoint { row: 0, column: 0 });
}

/// A left leaf whose width leaves a sub-cell remainder draws its grid shifted
/// right by that remainder (flush to the divider). The pointer maps against the
/// drawn origin: the remainder strip holds no cell, and a pointer just past the
/// drawn origin is over drawn column zero.
#[test]
fn pane_relative_cell_measures_from_the_drawn_grid_origin() {
    let cell = cell();
    let content = PaneRect::new(0.0, 0.0, 60.0, 40.0);
    let left = PaneRect::new(0.0, 0.0, 29.0, 40.0);
    let origin = crate::native::layout::pane_grid_origin(left, content, 10, 20);
    assert_eq!(origin, [9.0, 0.0], "a 29px leaf draws two cells from x=9");
    assert_eq!(
        pane_relative_cell(left, origin, cell, 11.0, 5.0),
        Some(CellPoint { row: 0, column: 0 }),
        "x=11 is over drawn column zero"
    );
    assert_eq!(
        pane_relative_cell(left, origin, cell, 20.0, 5.0),
        Some(CellPoint { row: 0, column: 1 })
    );
    assert_eq!(
        pane_relative_cell(left, origin, cell, 4.0, 5.0),
        None,
        "the remainder strip before the drawn grid has no cell"
    );
}

#[test]
fn pane_relative_cell_rejects_padding_and_collapsed_axes() {
    let cell = cell();
    let inner = PaneRect::new(64.0, 32.0, 25.0, 45.0);
    assert_eq!(
        pane_relative_cell(inner, [inner.x, inner.y], cell, 74.0, 52.0),
        Some(CellPoint { row: 1, column: 1 })
    );
    assert_eq!(
        pane_relative_cell(inner, [inner.x, inner.y], cell, 63.9, 52.0),
        None,
        "left padding is not clamped into column zero"
    );
    assert_eq!(
        pane_relative_cell(inner, [inner.x, inner.y], cell, 89.0, 52.0),
        None,
        "the exclusive inner edge has no hit target"
    );
    assert_eq!(
        pane_relative_cell(
            PaneRect::new(64.0, 32.0, 9.0, 45.0),
            [64.0, 32.0],
            cell,
            68.0,
            52.0
        ),
        None,
        "a sub-cell-width pane has no drawable column"
    );
    assert_eq!(
        pane_relative_cell(
            PaneRect::new(64.0, 32.0, 25.0, 19.0),
            [64.0, 32.0],
            cell,
            74.0,
            42.0
        ),
        None,
        "a sub-cell-height pane has no drawable row"
    );
}

#[test]
fn tab_bar_hit_test_columns_match_the_rendered_strip_width() {
    // Bug A guard: the tab strip renders across the window content columns
    // (`tab_bar_strip`: (surface_w - 2·pad)/cell.width), and the hit-test
    // must use the *same* column count (`tab_bar_grid_cols` ==
    // `overlay_grid_dims().0` == grid_dims_for_rect over the content rect).
    // If these diverged, multi-pane tabs would render at one set of columns
    // and hit-test at another (the focused pane's narrower sub-grid),
    // misaligning hover and dropping clicks. This proves the two formulas
    // agree across a matrix of widths and paddings.
    let cell = cell(); // 10x20
    for surface_w in [320u32, 800, 1280, 1366, 1920, 37] {
        for pad_logical in [0.0f32, 1.0, 8.0, 12.0] {
            let padding = WindowPadding::from_logical(pad_logical, 1.0);
            let pad = padding.physical_px();
            // The render-side strip column formula.
            let strip_cols =
                (surface_w.saturating_sub(pad.saturating_mul(2)) / cell.width.max(1)) as usize;
            // The hit-test-side content column count (what tab_bar_grid_cols
            // returns in multi-pane: grid_dims_for_rect over the content
            // rect). Tab bar shown, so the height arg is irrelevant to cols.
            let content = pane_content_rect(surface_w, 800, cell, padding, TabReserve::top());
            let (content_cols, _) =
                crate::native::layout::grid_dims_for_rect(content, cell.width, cell.height);
            // Both formulas permit zero columns; the strip then bails.
            assert_eq!(
                content_cols, strip_cols,
                "surface_w={surface_w} pad={pad}: render/hit-test column mismatch"
            );
        }
    }
}

#[test]
fn window_overlay_cell_degenerate_content_has_no_cell() {
    // A degenerate rect has no drawable cell and must not manufacture an
    // overlay hit target.
    let cell = cell();
    let content = PaneRect::new(0.0, 0.0, 0.0, 0.0);
    assert_eq!(window_overlay_cell(content, cell, 5.0, 5.0), None);
}

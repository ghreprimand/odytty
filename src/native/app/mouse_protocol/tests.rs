// SPDX-License-Identifier: GPL-3.0-only
//! Tests for mouse report encoding, pixel coordinates, and click-to-position
//! travel.

use super::*;
use crate::core::MouseTracking;

// --- SH-CLICK: click-to-position arrow encoding (Finding A) ---

fn app_cursor_modes() -> KeyModes {
    KeyModes {
        application_cursor: true,
        ..KeyModes::default()
    }
}

#[test]
fn click_position_emits_right_arrows_in_csi_mode() {
    // A positive delta (click right of the cursor) emits that many Right
    // cursor keys in the default CSI form.
    let bytes = click_position_bytes(5, KeyModes::default());
    assert_eq!(bytes, b"\x1b[C".repeat(5));
}

#[test]
fn click_position_emits_left_arrows_in_csi_mode() {
    // A negative delta (click left of the cursor) emits Left cursor keys.
    let bytes = click_position_bytes(-3, KeyModes::default());
    assert_eq!(bytes, b"\x1b[D".repeat(3));
}

#[test]
fn click_position_honors_decckm_application_cursor_mode() {
    // Finding A (the highest-risk encoding trap): a shell in DECCKM
    // application-cursor mode must receive the SS3 forms (\x1bOC / \x1bOD),
    // byte-identical to a real arrow keypress, NOT the CSI forms. This is
    // why the burst routes through `encode_key_event`, never hardcoded bytes.
    let right = click_position_bytes(5, app_cursor_modes());
    assert_eq!(right, b"\x1bOC".repeat(5));
    let left = click_position_bytes(-2, app_cursor_modes());
    assert_eq!(left, b"\x1bOD".repeat(2));
}

#[test]
fn click_position_burst_length_matches_delta_magnitude() {
    // The number of arrows equals |delta|; a single-cell move emits one key.
    assert_eq!(click_position_bytes(1, KeyModes::default()), b"\x1b[C");
    assert_eq!(
        click_position_bytes(-1, KeyModes::default()).len(),
        b"\x1b[D".len()
    );
    // A wide delta maps to exactly that many arrows (no off-by-one), without
    // exercising an absurd allocation.
    let wide = click_position_bytes(200, KeyModes::default());
    assert_eq!(wide.len(), b"\x1b[C".len() * 200);
}

// --- MS2: SGR-pixel (1016) native pixel seam ---

fn cell_8x16() -> CellSize {
    CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    }
}

#[test]
fn pixel_coords_origin_maps_to_one_based() {
    // Cursor at the top-left physical pixel maps to (1, 1): the protocol is
    // 1-based and zero padding keeps the grid at the window origin.
    let dims = Dimensions::new(80, 24);
    assert_eq!(
        pixel_coords_for_report(0.0, 0.0, cell_8x16(), dims, WindowPadding::ZERO),
        (1, 1)
    );
}

#[test]
fn pixel_coords_floor_then_one_base() {
    // Sub-pixel fractions floor; 10.9px -> pixel index 10 -> 1-based 11.
    let dims = Dimensions::new(80, 24);
    assert_eq!(
        pixel_coords_for_report(10.9, 33.2, cell_8x16(), dims, WindowPadding::ZERO),
        (11, 34)
    );
}

#[test]
fn pixel_coords_are_independent_of_cell_size() {
    // The pixel path reports raw physical pixels, NOT cells: the same cursor
    // position yields the same pixel coords regardless of cell metrics
    // (a larger cell only changes the clamp extent, not the mapping).
    let dims = Dimensions::new(80, 24);
    let small = CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    };
    let large = CellSize {
        width: 20,
        height: 40,
        baseline: 30,
    };
    assert_eq!(
        pixel_coords_for_report(100.0, 100.0, small, dims, WindowPadding::ZERO),
        pixel_coords_for_report(100.0, 100.0, large, dims, WindowPadding::ZERO)
    );
}

#[test]
fn pixel_coords_clamp_negative_to_one() {
    // A cursor left of / above the grid (negative physical coords during a
    // drag) saturates to pixel 1, mirroring cell_at_physical's max(0.0).
    let dims = Dimensions::new(80, 24);
    assert_eq!(
        pixel_coords_for_report(-50.0, -5.0, cell_8x16(), dims, WindowPadding::ZERO),
        (1, 1)
    );
}

#[test]
fn pixel_coords_clamp_to_grid_extent() {
    // Grid is 80x24 cells of 8x16 px = 640x384 px. A cursor at or beyond the
    // bottom-right edge clamps to the last in-grid pixel (640, 384).
    let dims = Dimensions::new(80, 24);
    assert_eq!(
        pixel_coords_for_report(640.0, 384.0, cell_8x16(), dims, WindowPadding::ZERO),
        (640, 384)
    );
    assert_eq!(
        pixel_coords_for_report(9999.0, 9999.0, cell_8x16(), dims, WindowPadding::ZERO),
        (640, 384)
    );
}

#[test]
fn pixel_coords_last_in_grid_pixel_is_not_clamped() {
    // 639.0px -> index 639 -> 1-based 640, the max; still inside the grid so
    // it is reported as-is (the clamp only bites at/after the extent).
    let dims = Dimensions::new(80, 24);
    assert_eq!(
        pixel_coords_for_report(639.0, 383.0, cell_8x16(), dims, WindowPadding::ZERO),
        (640, 384)
    );
}

#[test]
fn pixel_coords_subtract_window_padding_before_reporting() {
    let dims = Dimensions::new(80, 24);
    let padding = WindowPadding::from_logical(8.0, 1.0);

    assert_eq!(
        pixel_coords_for_report(8.0, 8.0, cell_8x16(), dims, padding),
        (1, 1)
    );
    assert_eq!(
        pixel_coords_for_report(18.9, 41.2, cell_8x16(), dims, padding),
        (11, 34)
    );
}

#[test]
fn sgr_pixel_encoder_emits_pixel_wire_shape() {
    // The 1016 seam feeds computed pixel coords to the core encoder, which
    // emits the SGR wire shape with those pixel values (here 101;201).
    let protocol = MouseProtocol {
        tracking: MouseTracking::Normal,
        encoding: MouseEncoding::SgrPixel,
    };
    let dims = Dimensions::new(80, 24);
    let (px, py) = pixel_coords_for_report(100.0, 200.0, cell_8x16(), dims, WindowPadding::ZERO);
    let mods = MouseModifiers {
        shift: false,
        alt: false,
        ctrl: false,
    };
    let bytes = encode_mouse_event_pixel(
        protocol,
        CoreMouseButton::Left,
        MouseEventKind::Press,
        px,
        py,
        mods,
    )
    .expect("1016 press encodes");
    assert_eq!(bytes, b"\x1b[<0;101;201M");
}

#[test]
fn pixel_encoder_guard_rejects_non_1016_encodings() {
    // The pixel encoder only fires for SgrPixel; for every other encoding it
    // returns None, so send_mouse_report's branch leaves the cell path
    // authoritative for legacy/UTF-8/SGR/urxvt.
    let dims = Dimensions::new(80, 24);
    let (px, py) = pixel_coords_for_report(100.0, 200.0, cell_8x16(), dims, WindowPadding::ZERO);
    let mods = MouseModifiers {
        shift: false,
        alt: false,
        ctrl: false,
    };
    for encoding in [
        MouseEncoding::Default,
        MouseEncoding::Utf8,
        MouseEncoding::Sgr,
        MouseEncoding::Urxvt,
    ] {
        let protocol = MouseProtocol {
            tracking: MouseTracking::Normal,
            encoding,
        };
        assert!(
            encode_mouse_event_pixel(
                protocol,
                CoreMouseButton::Left,
                MouseEventKind::Press,
                px,
                py,
                mods,
            )
            .is_none(),
            "encoding {encoding:?} must not take the pixel seam"
        );
    }
}

// --- HALF-CELL (nearest-boundary) click-to-position targeting ---
//
// Click-to-place snaps the caret target to the nearest column BOUNDARY: a
// right-half click (`round_up`) targets a glyph's trailing edge, one column
// further than a left-half click on the same cell. The prompt-side guard
// tests the floored cell, so rounding up never crosses the input start. All
// single-width and non-destructive, so these run on every platform;
// click-to-place carries no per-platform behaviour, so these expectations
// are platform-uniform.

/// Signed travel for a click on the floored cell `col` (with `round_up` =
/// the pixel fell in that cell's right half) against a single-row,
/// single-width input of `len` glyphs starting at `start`, cursor at
/// `cursor_col`. Uses the `Exact` certainty so these assert the pure
/// half-cell rounding (target-column) logic; it is platform-uniform, as is
/// the rest of the click-to-place travel.
fn halfcell_delta(
    start: usize,
    len: usize,
    col: usize,
    round_up: bool,
    cursor_col: usize,
) -> Option<i32> {
    let columns = 80usize;
    let rows = 28usize;
    let mut cells = vec![crate::core::Cell::blank(); columns * rows];
    for i in 0..len {
        cells[start + i] = crate::core::Cell::new('x', crate::core::Attrs::default());
    }
    let snapshot = Snapshot {
        dimensions: Dimensions::new(columns, rows),
        cursor: Position {
            row: 0,
            column: cursor_col,
        },
        cursor_visible: true,
        colors: crate::core::DynamicColors::default(),
        cells,
    };
    let region = crate::core::InputRegion {
        start_row: 0,
        start_col: start,
        end_row: 0,
        end_col: start + len,
        joins: Vec::new(),
        certainty: InputCertainty::Exact,
        row_spans: vec![(start, start + len)],
    };
    click_travel_delta(
        &snapshot,
        &region,
        CellPoint {
            row: 0,
            column: col,
        },
        round_up,
        Position {
            row: 0,
            column: cursor_col,
        },
        0,
        rows,
    )
}

#[test]
fn halfcell_right_half_targets_one_column_further_than_left_half() {
    // Input "xxxxx" at cols 2..7, cursor at the append origin (col 7 = 5
    // glyphs). A click on the 3rd glyph (col 4): the LEFT half targets
    // before it (2 glyphs in -> delta -3), the RIGHT half targets after it
    // (3 glyphs in -> delta -2). Exactly one column apart — the
    // nearest-boundary behaviour that fixes the one-cell-left mis-land.
    assert_eq!(halfcell_delta(2, 5, 4, false, 7), Some(-3));
    assert_eq!(halfcell_delta(2, 5, 4, true, 7), Some(-2));
}

#[test]
fn halfcell_last_glyph_right_half_clamps_to_append_origin() {
    // Cursor pulled back to col 4 (2 glyphs in). A right-half click on the
    // LAST glyph (col 6) targets the append origin (5 glyphs), never past it
    // -> delta +3; a click well past the input clamps to the same origin.
    assert_eq!(halfcell_delta(2, 5, 6, true, 4), Some(3));
    assert_eq!(halfcell_delta(2, 5, 10, true, 4), Some(3));
}

#[test]
fn halfcell_prompt_side_right_half_never_rounds_into_the_input() {
    // A right-half click on the last prompt cell (col 1; input starts at
    // col 2) stays a clean no-op: the guard tests the floored cell, so
    // rounding up to col 2 never fires a bogus travel toward position 0.
    assert_eq!(halfcell_delta(2, 5, 1, true, 7), None);
    assert_eq!(halfcell_delta(2, 5, 1, false, 7), None);
}

#[test]
fn halfcell_left_half_of_first_glyph_is_the_input_start() {
    // A left-half click on the first input glyph (col 2) targets buffer
    // position 0; from the append origin (col 7 = 5 glyphs) that is -5.
    // Rounding up moves one glyph in (delta -4) — the boundary after the
    // first glyph.
    assert_eq!(halfcell_delta(2, 5, 2, false, 7), Some(-5));
    assert_eq!(halfcell_delta(2, 5, 2, true, 7), Some(-4));
}

// SPDX-License-Identifier: GPL-3.0-only
//! ID3/U5 background treatments (gradient / vignette), driven through the real
//! grid cell-vertex background seam via the CPU compositor.

use odytty::core::{CursorStyle, Snapshot, Terminal};
use odytty::grid::{BackgroundTreatment, BackgroundTreatmentParams};

use crate::harness::*;

/// A `cols × rows` grid where every cell is a space on a bright gray background,
/// so a treatment's per-cell background darkening is directly measurable.
fn filled_bg_snapshot(cols: usize, rows: usize) -> Snapshot {
    let mut term = Terminal::new(cols, rows);
    term.advance(b"\x1b[?25l");
    for r in 0..rows {
        term.advance(format!("\x1b[{};1H", r + 1).as_bytes());
        term.advance(b"\x1b[48;2;200;200;200m");
        term.advance(" ".repeat(cols).as_bytes());
    }
    term.snapshot()
}

/// Mean luminance over a whole cell (cells are spaces, so this is the background).
fn cell_bg_luminance(frame: &Frame, col: usize, row: usize) -> f32 {
    let (x0, y0, x1, y1) = frame.cell_bounds(col, row);
    let mut sum = 0.0;
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            sum += luminance(frame.pixel(x, y));
            n += 1;
        }
    }
    sum / n as f32
}

/// KILL-SHOT (trap 1): the default (inactive) treatment composites a frame that
/// is byte-identical to the plain render — the off-path is pixel-identical.
#[test]
fn off_path_is_pixel_identical() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = filled_bg_snapshot(12, 8);
    let plain = composite(&snapshot, &atlas, CursorStyle::Block);
    let treated = composite_background_treatment(
        &snapshot,
        &atlas,
        CursorStyle::Block,
        BackgroundTreatmentParams::default(),
    );
    assert!(
        frames_match(&plain, &treated),
        "inactive treatment must be pixel-identical to the plain render"
    );
}

/// Gradient darkens the background toward the bottom rows; the top row is
/// unchanged from the plain render (falloff 0 there).
#[test]
fn gradient_darkens_toward_bottom() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = filled_bg_snapshot(12, 8);
    let plain = composite(&snapshot, &atlas, CursorStyle::Block);
    let treated = composite_background_treatment(
        &snapshot,
        &atlas,
        CursorStyle::Block,
        BackgroundTreatmentParams {
            kind: BackgroundTreatment::Gradient,
            strength: 1.0,
        },
    );

    let plain_top = cell_bg_luminance(&plain, 0, 0);
    let top = cell_bg_luminance(&treated, 0, 0);
    let mid = cell_bg_luminance(&treated, 0, 4);
    let bottom = cell_bg_luminance(&treated, 0, 7);

    assert!(
        (top - plain_top).abs() < 1e-3,
        "top row falloff is 0 ⇒ unchanged ({top} vs plain {plain_top})"
    );
    assert!(mid < top - 1e-3, "middle darker than top ({mid} < {top})");
    assert!(
        bottom < mid - 1e-3,
        "bottom darker than middle ({bottom} < {mid})"
    );
}

/// Vignette darkens the background toward the corners; the center cell is
/// unchanged from the plain render (falloff 0 there).
#[test]
fn vignette_darkens_toward_corners() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Odd extents so a single cell sits at the exact center.
    let snapshot = filled_bg_snapshot(9, 9);
    let plain = composite(&snapshot, &atlas, CursorStyle::Block);
    let treated = composite_background_treatment(
        &snapshot,
        &atlas,
        CursorStyle::Block,
        BackgroundTreatmentParams {
            kind: BackgroundTreatment::Vignette,
            strength: 1.0,
        },
    );

    let plain_center = cell_bg_luminance(&plain, 4, 4);
    let center = cell_bg_luminance(&treated, 4, 4);
    let corner = cell_bg_luminance(&treated, 0, 0);
    let edge_mid = cell_bg_luminance(&treated, 0, 4);

    assert!(
        (center - plain_center).abs() < 1e-3,
        "center falloff is 0 ⇒ unchanged ({center} vs plain {plain_center})"
    );
    assert!(
        corner < center - 1e-3,
        "corner darker than center ({corner} < {center})"
    );
    assert!(
        corner < edge_mid - 1e-3,
        "corner (farthest) darker than edge midpoint ({corner} < {edge_mid})"
    );
}

/// KILL-SHOT (T2 / off-path identity): with `cell_bg_opacity = 1.0` the cells
/// are fully opaque, so even with a background image present behind them the
/// composited cell backgrounds are byte-identical to the plain render — the
/// image is fully hidden behind opaque cells. This is the `bg[3] * 1.0 == bg[3]`
/// guarantee at the pixel level.
#[test]
fn image_opaque_cells_hide_image_pixel_identical() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = filled_bg_snapshot(12, 8);
    let plain = composite(&snapshot, &atlas, CursorStyle::Block);
    // A vivid image color behind fully-opaque cells must not show through.
    let imaged =
        composite_background_image(&snapshot, &atlas, CursorStyle::Block, 1.0, [1.0, 0.0, 0.0]);
    assert!(
        frames_match(&plain, &imaged),
        "opaque cells (opacity 1.0) must hide the image — pixel-identical to plain"
    );
}

/// With `cell_bg_opacity < 1.0` the background image shows through behind the
/// cell backgrounds: a bright image lifts a cell's composited luminance above
/// the opaque-cell baseline, and lower opacity reveals MORE of the image
/// (monotonic). Proves the `bg[3] *= cell_bg_opacity` alpha path composites.
#[test]
fn image_translucent_cells_reveal_image() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = filled_bg_snapshot(12, 8);
    // The grid cells are a gray (200) background; the image is pure white, so
    // letting it show through must raise the cell's composited luminance.
    let opaque =
        composite_background_image(&snapshot, &atlas, CursorStyle::Block, 1.0, [1.0, 1.0, 1.0]);
    let half =
        composite_background_image(&snapshot, &atlas, CursorStyle::Block, 0.5, [1.0, 1.0, 1.0]);
    let quarter =
        composite_background_image(&snapshot, &atlas, CursorStyle::Block, 0.25, [1.0, 1.0, 1.0]);

    let l_opaque = cell_bg_luminance(&opaque, 0, 0);
    let l_half = cell_bg_luminance(&half, 0, 0);
    let l_quarter = cell_bg_luminance(&quarter, 0, 0);

    assert!(
        l_half > l_opaque + 1e-3,
        "translucent cells reveal the bright image ({l_half} > {l_opaque})"
    );
    assert!(
        l_quarter > l_half + 1e-3,
        "lower opacity reveals more of the bright image ({l_quarter} > {l_half})"
    );
}

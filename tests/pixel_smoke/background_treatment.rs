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

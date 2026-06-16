// SPDX-License-Identifier: GPL-3.0-only
//! ID1 v1 soft cursor glow: composited draw-order proof (the glow halo is
//! emitted as cursor-layer overlays that draw BEHIND the cursor block per the
//! D-GLOW-3 reorder) and the off-path identity gate.

use odytty::core::{CursorStyle, RgbColor, Terminal};
use odytty::grid::SolidQuad;

use crate::harness::*;

/// Distinct opaque cursor color (green) so the block fill is unambiguously
/// separable from the glow halo color in composited pixels.
const CURSOR_RGB: (u8, u8, u8) = (0x4C, 0xD9, 0x9F);

/// A 3x3 grid with the cursor parked on the center cell `(1, 1)` and a distinct
/// green cursor color, so the glow rings around it have a full cell of blank
/// background on every side to land on.
fn center_cursor_snapshot() -> odytty::core::Snapshot {
    let mut term = Terminal::new(3, 3);
    term.set_base_colors(
        RgbColor::new(0xCC, 0xCC, 0xCC),
        RgbColor::new(0x0B, 0x0C, 0x10),
        RgbColor::new(CURSOR_RGB.0, CURSOR_RGB.1, CURSOR_RGB.2),
    );
    // Move the cursor to row 2, col 2 (1-based) = center cell (1, 1).
    term.advance(b"\x1b[2;2H");
    term.snapshot()
}

/// Build the three glow rings around cursor cell `(1, 1)` in OPAQUE red. Opaque
/// (not the live 0.05/0.09/0.13 alphas — those are asserted in the native unit
/// test) makes the draw-order unambiguous: if the rings drew ON TOP of the
/// cursor, the cursor cell would turn red; with the reorder they draw behind, so
/// the cursor cell stays green.
fn red_rings(cell_w: f32, cell_h: f32) -> Vec<SolidQuad> {
    let x0 = cell_w; // cell (1,1) origin (no window padding in this compositor)
    let y0 = cell_h;
    let x1 = x0 + cell_w;
    let y1 = y0 + cell_h;
    let red = [1.0, 0.0, 0.0, 1.0];
    [8.0f32, 4.0, 1.0]
        .into_iter()
        .map(|e| SolidQuad {
            rect: [x0 - e, y0 - e, x1 + e, y1 + e],
            color: red,
        })
        .collect()
}

#[test]
fn glow_draws_behind_the_cursor_block() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = center_cursor_snapshot();
    assert!(
        snapshot.cursor_visible,
        "cursor must be visible for this case"
    );

    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    let rings = red_rings(cell_w, cell_h);
    let frame = composite_with_cursor_overlays(&snapshot, &atlas, CursorStyle::Block, &rings);

    // T3: the cursor cell interior reads the green cursor block, NOT red — so
    // the opaque red rings drew behind the (opaque) cursor block.
    let cursor_lin = [
        odytty::color::srgb_to_linear(CURSOR_RGB.0),
        odytty::color::srgb_to_linear(CURSOR_RGB.1),
        odytty::color::srgb_to_linear(CURSOR_RGB.2),
    ];
    assert_eq!(
        cell_modal_color(&frame, 1, 1),
        quant3(cursor_lin),
        "the cursor block must dominate its cell (glow drew behind it)"
    );

    // The halo IS present: a pixel one row above the cursor cell (inside the
    // inner ring's 1px extension, on an otherwise-blank cell) reads red.
    let probe_x = (cell_w * 1.5) as usize; // horizontal center of cell (1,1)
    let probe_y = atlas.cell.height as usize - 1; // 1px band above the cell top
    let p = frame.pixel(probe_x, probe_y);
    assert!(
        p[0] > 0.5 && p[1] < 0.25 && p[2] < 0.25,
        "the glow halo must be visible just outside the cursor cell, got {p:?}"
    );
}

#[test]
fn empty_overlays_match_the_plain_cursor_frame() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = center_cursor_snapshot();
    // Off-path identity: with no overlay quads the overlay-aware compositor must
    // produce a byte-identical frame to the plain cursor path — the glow feature
    // adds nothing when off.
    let plain = composite(&snapshot, &atlas, CursorStyle::Block);
    let with_none = composite_with_cursor_overlays(&snapshot, &atlas, CursorStyle::Block, &[]);
    assert!(
        frames_match(&plain, &with_none),
        "empty cursor-overlay set must be byte-identical to the plain cursor frame"
    );
}

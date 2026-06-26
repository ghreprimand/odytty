// SPDX-License-Identifier: GPL-3.0-only
//! Symbol/icon glyph fit-to-cell + center behavior (the fastfetch ragged-icon
//! fix). Symbol-fallback and SYMMAP-override (icon) faces are rasterized at the
//! body em-size, which does not match OdyTTY's cell aspect — wide icons overflow
//! and clip and each glyph's distinct bearing puts ink at a different x, so an
//! icon column reads ragged. The fit pass measures each glyph's natural ink box,
//! scales it so the ink HEIGHT fills `SYMBOL_CELL_FILL` of the cell (width-capped
//! so it can never clip), and centers it on the cell box. Text paths (primary
//! font, ASCII, synthetic) pass `None` and are byte-identical.

use super::*;
use std::sync::Arc;

/// The inked symbol-marker fixture (covers U+2731 / U+25CF with real ink). At
/// 1000 upm its U+2731 outline is ~0.70 em wide/tall — wider than a typical
/// ~0.6 em monospace cell, so without a fit pass it overflows and clips.
fn marker_inked_font() -> FontVec {
    FontVec::try_from_vec(
        include_bytes!("../../../tests/fixtures/fonts/symbol-markers-inked.ttf").to_vec(),
    )
    .expect("parse inked marker fixture")
}

/// Tight bbox `(min_x, min_y, max_x, max_y)` of inked grayscale pixels anywhere
/// in the slot's drawable region (cell + overflow margin) around the inner cell
/// origin a UV points at. Off-mode atlas only (the test build default).
fn ink_bbox(atlas: &GlyphAtlas, uv: [f32; 4]) -> Option<(i32, i32, i32, i32)> {
    let (cx, cy) = inner_origin(atlas, uv);
    let (cx, cy) = (cx as i32, cy as i32);
    let m = overflow_margin(atlas.cell) as i32;
    let cw = atlas.cell.width as i32;
    let ch = atlas.cell.height as i32;
    let (mut minx, mut miny, mut maxx, mut maxy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in (cy - m)..(cy + ch + m) {
        for x in (cx - m)..(cx + cw + m) {
            if x < 0 || y < 0 || x as u32 >= atlas.width || y as u32 >= atlas.height {
                continue;
            }
            if atlas.data[(y as u32 * atlas.width + x as u32) as usize] > 0 {
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
        }
    }
    (maxx >= minx).then_some((minx, miny, maxx, maxy))
}

// ---------------------------------------------------------------------------
// Pure scale helper — deterministic, host-font-independent.
// ---------------------------------------------------------------------------

#[test]
fn fit_scale_height_binds_for_tall_narrow_glyph() {
    // Tall/narrow glyph: the height ratio binds, the width cap is slack.
    // nat 10x20, target_h 10, generous width cap 1000, no upscale clamp hit.
    let s = symbol_fit_scale_v2(10.0, 20.0, 10.0, 1000.0, 2.0);
    assert!((s - 0.5).abs() < 1e-6, "height must bind, got {s}");
}

#[test]
fn fit_scale_width_cap_binds_for_wide_glyph() {
    // Wide/short glyph: scaling to height would overflow the width cap, so the
    // width cap binds and the glyph stays within the drawable region (no clip).
    // height-only scale = target_h/nat_h = 40/10 = 4.0, but width cap =
    // max_draw_w/nat_w = 20/40 = 0.5 → 0.5 wins.
    let s = symbol_fit_scale_v2(40.0, 10.0, 40.0, 20.0, 2.0);
    assert!((s - 0.5).abs() < 1e-6, "width cap must bind, got {s}");
}

#[test]
fn fit_scale_caps_upscale_of_subcell_glyph() {
    // A tiny glyph wants a huge upscale to reach the height target; the cap
    // (2.0) holds it. height-only = 20/2 = 10.0, width cap = 1000/2 = 500 →
    // both slack vs the 2.0 cap.
    let s = symbol_fit_scale_v2(2.0, 2.0, 20.0, 1000.0, 2.0);
    assert!(
        (s - 2.0).abs() < 1e-6,
        "sub-cell glyph must cap at 2.0, got {s}"
    );
}

#[test]
fn fit_scale_degenerate_inputs_stay_positive_and_capped() {
    // Zero/negative ink is clamped to 1px; the result is finite, > 0, and capped.
    let s = symbol_fit_scale_v2(0.0, 0.0, 10.0, 10.0, 2.0);
    assert!(
        s.is_finite() && s > 0.0,
        "degenerate scale must be positive, got {s}"
    );
    assert!(
        s <= 2.0 + 1e-6,
        "degenerate scale must respect cap, got {s}"
    );
}

// ---------------------------------------------------------------------------
// Integration — height-fraction fit + center on a real cell via a SYMMAP
// override icon face.
// ---------------------------------------------------------------------------

#[test]
fn override_symbol_glyph_fits_within_cell_and_is_centered() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let symbol = '\u{2731}'; // HEAVY ASTERISK — inked in the fixture (~0.70 em sq)
    let cp = symbol as u32;
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    // Force the inked icon fixture for this codepoint regardless of primary
    // coverage; the override path is one of the two fit-enabled call sites.
    atlas.set_symbol_map_fonts(vec![(cp, cp, Arc::new(marker_inked_font()))]);
    let uv = atlas.ensure(&font, symbol).expect("override glyph uv");

    let (cx, cy) = inner_origin(&atlas, uv);
    let (cx, cy) = (cx as i32, cy as i32);
    let cw = atlas.cell.width as i32;
    let ch = atlas.cell.height as i32;
    let margin = overflow_margin(atlas.cell) as i32;
    // The slot's drawable region inside the ATLAS_PAD gutter and the inset pad,
    // matching `max_draw_w` in the fitter. `outer_w` for a single cell is
    // `slot_w(cell)`; the per-side inset pad mirrors the production formula.
    let pad = (SYMBOL_CELL_INSET * atlas.cell.width.min(atlas.cell.height) as f32).round();
    let outer_w = slot_w(atlas.cell) as f32;
    let max_draw_w = (outer_w - 2.0 * ATLAS_PAD as f32 - 2.0 * pad).max(1.0);

    let (minx, miny, maxx, maxy) = ink_bbox(&atlas, uv).expect("fitted glyph must have ink");
    let ink_w = (maxx - minx + 1) as f32;
    let ink_h = (maxy - miny + 1) as f32;

    // 1) Ink HEIGHT ≈ SYMBOL_CELL_FILL * cell.height (within a couple px from
    //    rounding + the fixture's symmetric-but-not-cell-square outline).
    let target_h = SYMBOL_CELL_FILL * ch as f32;
    assert!(
        (ink_h - target_h).abs() <= 2.0,
        "ink height {ink_h} should be ~{target_h} (SYMBOL_CELL_FILL * cell)"
    );

    // 2) Fully within the slot drawable region in BOTH axes — no clip. (Width
    //    may exceed one cell into the overflow margin; that is intended.)
    assert!(
        minx >= cx - margin,
        "ink left {minx} spilled past drawable region"
    );
    assert!(
        maxx <= cx + cw - 1 + margin,
        "ink right {maxx} spilled past drawable region"
    );
    assert!(
        miny >= cy - margin,
        "ink top {miny} spilled past drawable region"
    );
    assert!(
        maxy <= cy + ch - 1 + margin,
        "ink bottom {maxy} spilled past drawable region"
    );

    // 3) Width never exceeds the width safety cap (so it cannot clip/kiss).
    assert!(
        ink_w <= max_draw_w + 1.0,
        "ink width {ink_w} must not exceed max_draw_w {max_draw_w}"
    );

    // 4) Centered on the full cell box, both axes: symmetric margins (the icon
    //    fixture is symmetric, so a centered fit yields equal opposite margins).
    let left_margin = minx - cx;
    let right_margin = (cx + cw - 1) - maxx;
    let top_margin = miny - cy;
    let bottom_margin = (cy + ch - 1) - maxy;
    assert!(
        (left_margin - right_margin).abs() <= 2,
        "horizontal off-center: left {left_margin} vs right {right_margin}"
    );
    assert!(
        (top_margin - bottom_margin).abs() <= 2,
        "vertical off-center: top {top_margin} vs bottom {bottom_margin}"
    );
}

// ---------------------------------------------------------------------------
// Byte-identity guard — the fit machinery never perturbs primary text pixels.
// ---------------------------------------------------------------------------

#[test]
fn fit_machinery_leaves_primary_ascii_byte_identical() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    // Per-ASCII-slot ink after a plain build (all rasterized through the `None`
    // fit path).
    let before: Vec<u64> = (FIRST_CHAR..=LAST_CHAR)
        .map(|c| {
            let slot = c - FIRST_CHAR + 1;
            cell_ink(&atlas, atlas.slot_uv(slot))
        })
        .collect();

    // Install a SYMMAP override and rasterize a *fitted* symbol glyph into a
    // fresh dynamic slot. This must not touch any ASCII slot's pixels.
    atlas.set_symbol_map_fonts(vec![(0x2731, 0x2731, Arc::new(marker_inked_font()))]);
    let _ = atlas
        .ensure(&font, '\u{2731}')
        .expect("fitted override glyph");

    let after: Vec<u64> = (FIRST_CHAR..=LAST_CHAR)
        .map(|c| {
            let slot = c - FIRST_CHAR + 1;
            cell_ink(&atlas, atlas.slot_uv(slot))
        })
        .collect();

    assert_eq!(
        before, after,
        "primary ASCII ink must be byte-identical with the fit path present"
    );
}

// SPDX-License-Identifier: GPL-3.0-only
//! Symbol/icon glyph fit-to-cell + center behavior (the fastfetch ragged-icon
//! fix). Symbol-fallback and SYMMAP-override (icon) faces are rasterized at the
//! body em-size, which does not match OdyTTY's cell aspect — wide icons overflow
//! and clip and each glyph's distinct bearing puts ink at a different x, so an
//! icon column reads ragged. The fit pass measures each glyph's natural ink box,
//! scales it aspect-preserving to fit the cell box minus inset padding, and
//! centers it on the cell box. Text paths (primary font, ASCII, synthetic) pass
//! `None` and are byte-identical.

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
fn fit_scale_downscales_oversized_uncapped() {
    // Ink twice the target in both axes → scale 0.5, no lower clamp.
    let s = symbol_fit_scale(20.0, 20.0, 10.0, 10.0);
    assert!((s - 0.5).abs() < 1e-6, "expected 0.5, got {s}");
    // A grossly oversized glyph downscales freely (downscale is never clamped).
    let s = symbol_fit_scale(1000.0, 1000.0, 10.0, 10.0);
    assert!(
        (s - 0.01).abs() < 1e-6,
        "downscale must not be clamped, got {s}"
    );
}

#[test]
fn fit_scale_is_aspect_preserving_min_ratio() {
    // Wider than tall vs a square target → the width ratio (smaller) binds.
    let s = symbol_fit_scale(20.0, 10.0, 10.0, 10.0);
    assert!((s - 0.5).abs() < 1e-6, "width ratio must bind, got {s}");
    // Taller than wide → the height ratio binds.
    let s = symbol_fit_scale(10.0, 20.0, 10.0, 10.0);
    assert!((s - 0.5).abs() < 1e-6, "height ratio must bind, got {s}");
}

#[test]
fn fit_scale_caps_upscale_of_subcell_glyph() {
    // A tiny glyph wants a 50x upscale; the cap holds it to SYMBOL_MAX_UPSCALE.
    let s = symbol_fit_scale(2.0, 2.0, 100.0, 100.0);
    assert!(
        (s - SYMBOL_MAX_UPSCALE).abs() < 1e-6,
        "sub-cell glyph must cap at {SYMBOL_MAX_UPSCALE}, got {s}"
    );
    // A modest sub-cell glyph upscales below the cap unclamped.
    let s = symbol_fit_scale(10.0, 10.0, 12.0, 12.0);
    assert!((s - 1.2).abs() < 1e-6, "expected 1.2, got {s}");
}

#[test]
fn fit_scale_degenerate_inputs_stay_positive_and_capped() {
    // Zero/negative ink is clamped to 1px; the result is finite, > 0, and capped.
    let s = symbol_fit_scale(0.0, 0.0, 10.0, 10.0);
    assert!(
        s.is_finite() && s > 0.0,
        "degenerate scale must be positive, got {s}"
    );
    assert!(
        s <= SYMBOL_MAX_UPSCALE + 1e-6,
        "degenerate scale must respect cap, got {s}"
    );
}

// ---------------------------------------------------------------------------
// Integration — fit + center on a real cell via a SYMMAP override icon face.
// ---------------------------------------------------------------------------

#[test]
fn override_symbol_glyph_fits_within_cell_and_is_centered() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let symbol = '\u{2731}'; // HEAVY ASTERISK — inked in the fixture (~0.70 em)
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
    let pad = (SYMBOL_CELL_INSET * atlas.cell.width.min(atlas.cell.height) as f32).round() as i32;

    let (minx, miny, maxx, maxy) = ink_bbox(&atlas, uv).expect("fitted glyph must have ink");

    // 1) Fits strictly inside the cell box in BOTH axes — no overflow, no clip.
    //    Today (natural bearing at body em) this glyph overflows the cell width.
    assert!(
        minx >= cx,
        "ink left {minx} must not spill left of cell {cx}"
    );
    assert!(
        maxx < cx + cw,
        "ink right {maxx} must not spill past cell right {}",
        cx + cw - 1
    );
    assert!(miny >= cy, "ink top {miny} must not spill above cell {cy}");
    assert!(
        maxy < cy + ch,
        "ink bottom {maxy} must not spill below cell {}",
        cy + ch - 1
    );

    // 2) Inset padding honored: the glyph never kisses the cell edge.
    assert!(
        minx - cx >= pad.max(1) - 1,
        "left inset {} < pad {pad}",
        minx - cx
    );
    assert!(
        miny - cy >= pad.max(1) - 1,
        "top inset {} < pad {pad}",
        miny - cy
    );

    // 3) Centered on the cell box (not the text baseline): symmetric margins.
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

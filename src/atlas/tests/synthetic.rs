// SPDX-License-Identifier: GPL-3.0-only
//! Synthetic bold + italic fallback (SB1): when the font family has no real
//! face for a style, the Regular outline is emboldened (horizontal
//! double-strike) and/or sheared (~12-degree oblique) at rasterization time.
//! These tests drive the atlas contract directly via
//! [`GlyphAtlas::set_synthetic_styles`]; the native layer sets that mask from
//! `Arc` identity of its loaded faces.
//!
//! Two measurement rules matter here:
//! - Glyphs are identified by their UV rect, not `slot_count()`: a Regular ASCII
//!   lookup resolves to its prebuilt slot and allocates nothing.
//! - UVs are fetched **after all insertions** via [`fresh_uv`]: inserting a
//!   glyph can grow the atlas, which changes the V denominator and would
//!   invalidate any UV captured earlier (the atlas recomputes UVs on demand for
//!   exactly this reason).

use super::*;

/// Re-fetch a resident glyph's UV against the atlas's *current* dimensions, so
/// measurements stay correct after an insertion grows the atlas height.
fn fresh_uv(atlas: &GlyphAtlas, style: FontStyle, ch: char) -> [f32; 4] {
    atlas
        .uv_rect_styled(style, ch)
        .expect("glyph should be resident")
}

/// The slot's **outer** top-left `(ox, oy)` in atlas pixels, recovered from a UV
/// rect. Works for both prebuilt-ASCII (Regular) and dynamic (styled) slots.
fn slot_origin_from_uv(atlas: &GlyphAtlas, uv: [f32; 4]) -> (u32, u32) {
    let (cx, cy) = inner_origin(atlas, uv);
    let b = slot_border(atlas.cell);
    (cx - b, cy - b)
}

/// Sum of coverage bytes over the `span`-cell slot region `uv` points at (cell +
/// overflow margin), so emboldening/shear that spills past the inner cell still
/// counts. `span` is `1` for normal glyphs, `2` for a wide lead slot.
fn uv_coverage(atlas: &GlyphAtlas, uv: [f32; 4], span: u32) -> u64 {
    let (ox, oy) = slot_origin_from_uv(atlas, uv);
    let (sw, sh) = (slot_w(atlas.cell), slot_h(atlas.cell));
    let mut sum = 0u64;
    for y in oy..oy + sh {
        for x in ox..ox + span * sw {
            sum += atlas.data[(y * atlas.width + x) as usize] as u64;
        }
    }
    sum
}

/// Mean inked-pixel column (absolute atlas x), restricted to atlas rows in
/// `[y0, y1)`, scanning the single-cell slot `uv` points at. `None` if no ink.
fn mean_ink_x_in_rows(atlas: &GlyphAtlas, uv: [f32; 4], y0: u32, y1: u32) -> Option<f64> {
    let (ox, _) = slot_origin_from_uv(atlas, uv);
    let sw = slot_w(atlas.cell);
    let mut sum_x = 0u64;
    let mut count = 0u64;
    for y in y0..y1 {
        for x in ox..ox + sw {
            if atlas.data[(y * atlas.width + x) as usize] > 0 {
                sum_x += x as u64;
                count += 1;
            }
        }
    }
    (count > 0).then(|| sum_x as f64 / count as f64)
}

/// With the bold synthetic bit set and only the Regular font supplied, a
/// bold-keyed glyph must ink strictly more than the same glyph in Regular —
/// the double-strike thickens it — while the shared cell metrics are untouched.
#[test]
fn synthetic_bold_inks_more_than_regular() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 28.0);
    let plain_cell = atlas.cell;
    atlas.set_synthetic_styles(true, false, false);

    atlas
        .ensure_styled(&font, FontStyle::Regular, 'M')
        .expect("regular M");
    atlas
        .ensure_styled(&font, FontStyle::Bold, 'M')
        .expect("bold M");

    let regular_ink = uv_coverage(&atlas, fresh_uv(&atlas, FontStyle::Regular, 'M'), 1);
    let bold_ink = uv_coverage(&atlas, fresh_uv(&atlas, FontStyle::Bold, 'M'), 1);
    assert!(
        bold_ink > regular_ink,
        "synthetic bold should ink more (bold={bold_ink}, regular={regular_ink})"
    );
    // Metrics unchanged by construction: emboldening never touches the cell.
    assert_eq!(
        atlas.cell, plain_cell,
        "synthetic bold must not alter metrics"
    );
}

/// "Real faces always win": with **no** synthetic bit set (the state when a real
/// bold face is present), a bold-keyed glyph inks identically to Regular — no
/// synthesis fires. The native layer leaves the bit clear when a real face
/// loads, so this is the real-face no-regression guard.
#[test]
fn no_synthetic_mask_leaves_bold_identical_to_regular() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 28.0);
    // Mask defaults to 0; do NOT call set_synthetic_styles.
    atlas
        .ensure_styled(&font, FontStyle::Regular, 'M')
        .expect("regular M");
    atlas
        .ensure_styled(&font, FontStyle::Bold, 'M')
        .expect("bold M");
    assert_eq!(
        uv_coverage(&atlas, fresh_uv(&atlas, FontStyle::Bold, 'M'), 1),
        uv_coverage(&atlas, fresh_uv(&atlas, FontStyle::Regular, 'M'), 1),
        "without the synthetic bit, bold must equal regular (real-face path)"
    );
}

/// Synthetic italic shears the outline about the baseline: rows above the
/// baseline lean right, so the mean inked column in the top band sits strictly
/// right of the mean in the bottom band. A regular (un-sheared) glyph of the
/// same shape shows a far smaller top-vs-bottom delta.
#[test]
fn synthetic_italic_leans_right_above_baseline() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 32.0);
    atlas.set_synthetic_styles(false, true, false);

    atlas
        .ensure_styled(&font, FontStyle::Regular, 'I')
        .expect("regular I");
    atlas
        .ensure_styled(&font, FontStyle::Italic, 'I')
        .expect("italic I");
    let reg_uv = fresh_uv(&atlas, FontStyle::Regular, 'I');
    let ital_uv = fresh_uv(&atlas, FontStyle::Italic, 'I');

    let (_, oy) = slot_origin_from_uv(&atlas, ital_uv);
    let band = atlas.cell.height / 3;
    let top = (oy, oy + band);
    let bot = (oy + slot_h(atlas.cell) - band, oy + slot_h(atlas.cell));

    let (Some(ital_top), Some(ital_bot)) = (
        mean_ink_x_in_rows(&atlas, ital_uv, top.0, top.1),
        mean_ink_x_in_rows(&atlas, ital_uv, bot.0, bot.1),
    ) else {
        eprintln!("skipping: glyph inked too sparsely to measure lean");
        return;
    };
    // Italic: top band leans clearly right of the bottom band.
    assert!(
        ital_top > ital_bot + 1.0,
        "synthetic italic top should lean right of bottom (top={ital_top:.2}, bot={ital_bot:.2})"
    );

    // The same glyph in Regular shows a much smaller top-vs-bottom skew.
    let (_, reg_oy) = slot_origin_from_uv(&atlas, reg_uv);
    let reg_top = mean_ink_x_in_rows(&atlas, reg_uv, reg_oy, reg_oy + band);
    let reg_bot = mean_ink_x_in_rows(
        &atlas,
        reg_uv,
        reg_oy + slot_h(atlas.cell) - band,
        reg_oy + slot_h(atlas.cell),
    );
    if let (Some(rt), Some(rb)) = (reg_top, reg_bot) {
        let reg_skew = (rt - rb).abs();
        let ital_skew = ital_top - ital_bot;
        assert!(
            ital_skew > reg_skew,
            "italic skew ({ital_skew:.2}) should exceed regular skew ({reg_skew:.2})"
        );
    }
}

/// Bold-italic composes both transforms: it inks more than Regular (emboldened)
/// and leans right above the baseline (sheared).
#[test]
fn synthetic_bold_italic_combines_both() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 32.0);
    atlas.set_synthetic_styles(false, false, true);

    atlas
        .ensure_styled(&font, FontStyle::Regular, 'H')
        .expect("regular H");
    atlas
        .ensure_styled(&font, FontStyle::BoldItalic, 'H')
        .expect("bold-italic H");
    let reg_uv = fresh_uv(&atlas, FontStyle::Regular, 'H');
    let bi_uv = fresh_uv(&atlas, FontStyle::BoldItalic, 'H');

    assert!(
        uv_coverage(&atlas, bi_uv, 1) > uv_coverage(&atlas, reg_uv, 1),
        "bold-italic should ink more than regular"
    );

    let (_, oy) = slot_origin_from_uv(&atlas, bi_uv);
    let band = atlas.cell.height / 3;
    if let (Some(top), Some(bot)) = (
        mean_ink_x_in_rows(&atlas, bi_uv, oy, oy + band),
        mean_ink_x_in_rows(
            &atlas,
            bi_uv,
            oy + slot_h(atlas.cell) - band,
            oy + slot_h(atlas.cell),
        ),
    ) {
        assert!(
            top > bot,
            "bold-italic should also lean right (top={top:.2}, bot={bot:.2})"
        );
    }
}

/// Invalidation contract: a freshly built atlas with the synthetic bit **clear**
/// (the state after a font change swaps in a real bold face) inks bold exactly
/// like Regular — no stale synthesis survives. This mirrors what the native
/// layer does: a font change rebuilds the atlas from scratch and recomputes the
/// mask, so the old synthetic slots vanish with the old atlas.
#[test]
fn clearing_mask_removes_synthesis() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Atlas A: bold synthesized.
    let mut synth = GlyphAtlas::build(&font, 28.0);
    synth.set_synthetic_styles(true, false, false);
    synth
        .ensure_styled(&font, FontStyle::Bold, 'M')
        .expect("synth bold M");
    let synth_ink = uv_coverage(&synth, fresh_uv(&synth, FontStyle::Bold, 'M'), 1);

    // Atlas B: a fresh build with the bold bit cleared (real face now present).
    let mut real = GlyphAtlas::build(&font, 28.0);
    real.set_synthetic_styles(false, false, false);
    real.ensure_styled(&font, FontStyle::Regular, 'M')
        .expect("regular M");
    real.ensure_styled(&font, FontStyle::Bold, 'M')
        .expect("real bold M");
    let regular_ink = uv_coverage(&real, fresh_uv(&real, FontStyle::Regular, 'M'), 1);
    let real_bold_ink = uv_coverage(&real, fresh_uv(&real, FontStyle::Bold, 'M'), 1);

    assert_eq!(
        real_bold_ink, regular_ink,
        "cleared mask must render bold == regular (no stale synthesis)"
    );
    assert!(
        synth_ink > real_bold_ink,
        "the synthesized atlas should still ink more than the cleared one \
         (synth={synth_ink}, cleared={real_bold_ink})"
    );
}

/// A synthetic bold/italic wide (East Asian width-2) glyph stays inside its
/// two-cell slot: the rasterizer's clip keeps the smear and shear from leaking
/// past the slot's drawable region into a neighbor. Skips on hosts without a
/// wide-capable font.
#[test]
fn synthetic_wide_glyph_stays_within_slot() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(wide) = wide_glyph_supported(&font) else {
        eprintln!("skipping: no wide glyph in the loaded font");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 28.0);
    atlas.set_synthetic_styles(true, true, true);
    atlas
        .ensure_styled(&font, FontStyle::BoldItalic, wide)
        .expect("synthetic wide bold-italic");
    let uv = fresh_uv(&atlas, FontStyle::BoldItalic, wide);

    // Lead slot spans two cells; the drawable region's right clip edge is
    // `ox + 2 * slot_w - ATLAS_PAD`. No inked pixel may reach or pass it.
    let (ox, oy) = slot_origin_from_uv(&atlas, uv);
    let span = 2u32;
    let x_hi = ox + span * slot_w(atlas.cell) - ATLAS_PAD;
    let x_lo = ox + ATLAS_PAD;
    let sh = slot_h(atlas.cell);
    let (mut max_x, mut min_x, mut inked) = (0u32, atlas.width, false);
    for y in oy..oy + sh {
        for x in ox..ox + span * slot_w(atlas.cell) {
            if atlas.data[(y * atlas.width + x) as usize] > 0 {
                inked = true;
                max_x = max_x.max(x);
                min_x = min_x.min(x);
            }
        }
    }
    if !inked {
        eprintln!("skipping: wide glyph produced no ink");
        return;
    }
    assert!(
        max_x < x_hi,
        "synthetic wide ink must stay left of the slot clip (max_x={max_x}, x_hi={x_hi})"
    );
    assert!(
        min_x >= x_lo,
        "synthetic wide ink must stay right of the slot's left pad (min_x={min_x}, x_lo={x_lo})"
    );
}

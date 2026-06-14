// SPDX-License-Identifier: GPL-3.0-only
//! Glyph ink geometry (strokes, baseline, descender) and styled-slot tests. (M5 mechanical split from atlas.rs).

use super::*;

/// Box-drawing strokes must reach the cell edges so adjacent cells join
/// seamlessly. The horizontal line U+2500 should ink the full cell width;
/// the vertical line U+2502 should ink the full cell height.
#[test]
fn box_drawing_strokes_reach_cell_edges() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    if !font_has_glyph(&font, '\u{2500}') || !font_has_glyph(&font, '\u{2502}') {
        eprintln!("skipping: font lacks box-drawing glyphs");
        return;
    }
    let mut atlas = GlyphAtlas::build(&font, 28.0);
    let cw = atlas.cell.width;
    let ch = atlas.cell.height;

    // Horizontal line: across its inked row band, ink reaches the left and
    // right cell edges (within 1px tolerance for font bearing).
    let h = atlas.ensure(&font, '\u{2500}').expect("U+2500 uv");
    let (hx, hy) = inner_origin(&atlas, h);
    let (mut min_col, mut max_col) = (cw, 0u32);
    for y in hy..hy + ch {
        for x in hx..hx + cw {
            if atlas.data[(y * atlas.width + x) as usize] > 0 {
                min_col = min_col.min(x - hx);
                max_col = max_col.max(x - hx);
            }
        }
    }
    assert!(
        min_col <= 1,
        "─ should ink the left edge (min_col={min_col})"
    );
    assert!(
        max_col >= cw - 2,
        "─ should ink the right edge (max_col={max_col}, cw={cw})"
    );

    // Vertical line: across its inked column band, ink reaches the top and
    // bottom cell edges.
    let v = atlas.ensure(&font, '\u{2502}').expect("U+2502 uv");
    let (vx, vy) = inner_origin(&atlas, v);
    let (mut min_row, mut max_row) = (ch, 0u32);
    for y in vy..vy + ch {
        for x in vx..vx + cw {
            if atlas.data[(y * atlas.width + x) as usize] > 0 {
                min_row = min_row.min(y - vy);
                max_row = max_row.max(y - vy);
            }
        }
    }
    assert!(
        min_row <= 1,
        "│ should ink the top edge (min_row={min_row})"
    );
    assert!(
        max_row >= ch - 2,
        "│ should ink the bottom edge (max_row={max_row}, ch={ch})"
    );
}

/// Every glyph is placed on the one shared integer baseline. Two cap-height
/// letters ('E', 'F') with flat tops therefore start inking on the same row,
/// proving a single consistent baseline rather than per-glyph drift.
#[test]
fn glyphs_share_one_baseline() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 26.0);
    let top_row = |ch: char| -> Option<u32> {
        let uv = atlas.uv_rect(ch)?;
        let (ix, iy) = inner_origin(&atlas, uv);
        for y in iy..iy + atlas.cell.height {
            for x in ix..ix + atlas.cell.width {
                if atlas.data[(y * atlas.width + x) as usize] > 0 {
                    return Some(y - iy);
                }
            }
        }
        None
    };
    // 'E' and 'F' share a flat cap top; on a consistent baseline their first
    // inked row matches.
    assert_eq!(top_row('E'), top_row('F'));
    // The recorded baseline sits within the cell box.
    assert!(atlas.cell.baseline > 0 && atlas.cell.baseline <= atlas.cell.height);
}

/// A descender ('g') inks the lower part of the cell and is not cropped at
/// the cell box — its ink extends below the baseline.
#[test]
fn descender_is_not_cropped() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 28.0);
    let uv = atlas.uv_rect('g').expect("g uv");
    let (ix, iy) = inner_origin(&atlas, uv);
    let baseline = atlas.cell.baseline;
    // Some ink exists strictly below the baseline row (the descender).
    let mut below = false;
    for y in (iy + baseline + 1)..(iy + atlas.cell.height) {
        for x in ix..ix + atlas.cell.width {
            if atlas.data[(y * atlas.width + x) as usize] > 0 {
                below = true;
            }
        }
    }
    assert!(below, "'g' descender should ink below the baseline");
}

/// The default `FontStyle` is `Regular`, and the regular styled lookups are
/// byte-for-byte the legacy ones, so existing native call sites are
/// unaffected by the `(style, char)` keying.
#[test]
fn regular_style_matches_legacy_lookup() {
    assert_eq!(FontStyle::default(), FontStyle::Regular);
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    // ASCII, fallback, and control behave identically through both entry points.
    assert_eq!(
        atlas.uv_rect('A'),
        atlas.uv_rect_styled(FontStyle::Regular, 'A')
    );
    assert_eq!(
        atlas.uv_rect('\u{2603}'),
        atlas.uv_rect_styled(FontStyle::Regular, '\u{2603}')
    );
    assert_eq!(
        atlas.uv_rect('\n'),
        atlas.uv_rect_styled(FontStyle::Regular, '\n')
    );
}

/// A non-`Regular` style of a glyph-bearing codepoint lands in its own slot,
/// so styled variants never collide with the regular glyph.
#[test]
fn styled_glyph_gets_a_distinct_slot() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(ch) = glyph_bearing_non_ascii(&font) else {
        eprintln!("skipping: font has no non-ASCII glyph");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let regular = atlas
        .ensure_styled(&font, FontStyle::Regular, ch)
        .expect("regular uv");
    let count_after_regular = atlas.slot_count();
    let bold = atlas
        .ensure_styled(&font, FontStyle::Bold, ch)
        .expect("bold uv");
    // Distinct style => distinct slot => distinct uv, and a new slot consumed.
    assert_ne!(regular, bold, "bold must not reuse the regular slot");
    assert!(
        atlas.slot_count() > count_after_regular,
        "bold should allocate"
    );
    // Re-resolving each style is a stable cache hit.
    assert_eq!(atlas.uv_rect_styled(FontStyle::Regular, ch), Some(regular));
    assert_eq!(atlas.uv_rect_styled(FontStyle::Bold, ch), Some(bold));
}

/// For a non-`Regular` style, even printable ASCII flows through the dynamic
/// region: the immutable lookup returns the fallback until `ensure_styled`
/// rasterizes it, after which both resolve to the same real slot.
#[test]
fn styled_ascii_uses_dynamic_region() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let fallback = atlas.slot_uv(FALLBACK_SLOT);
    // Bold 'A' is not prebuilt: immutable lookup is the fallback box.
    assert_eq!(atlas.uv_rect_styled(FontStyle::Bold, 'A'), Some(fallback));
    // Regular 'A' still resolves to its prebuilt slot, untouched.
    assert_ne!(atlas.uv_rect('A'), Some(fallback));
    // ensure_styled rasterizes a real bold-keyed slot, distinct from regular.
    let bold_a = atlas
        .ensure_styled(&font, FontStyle::Bold, 'A')
        .expect("bold A uv");
    assert_ne!(bold_a, fallback);
    assert_ne!(Some(bold_a), atlas.uv_rect('A'));
    assert_eq!(atlas.uv_rect_styled(FontStyle::Bold, 'A'), Some(bold_a));
}

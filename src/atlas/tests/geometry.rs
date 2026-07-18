// SPDX-License-Identifier: GPL-3.0-only
//! Glyph ink geometry (strokes, baseline, descender) and styled-slot tests. (M5 mechanical split from atlas.rs).

use super::*;

/// The atlas backing-buffer byte length must survive products beyond
/// `u32::MAX`. Width ~5552 × height ~266240 × 4 bpp ≈ 5.9 GB is a real shape
/// at the 72 px font cap on a 4× HiDPI display once a full 8192-slot subpixel
/// atlas has grown. Regression: computed in `u32` this overflowed — a debug
/// build panicked, a release build wrapped to a tiny allocation and later
/// raster writes went out of bounds (heap corruption).
#[test]
fn atlas_byte_len_survives_products_beyond_u32() {
    assert_eq!(atlas_byte_len(5552, 266_240, 4), 5_912_657_920usize);
    assert!(atlas_byte_len(5552, 266_240, 4) > u32::MAX as usize);
}

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

// ----- Over-wide contextual (ligature) spans vs. the row-major slot grid -----

/// A contextual span wider than one atlas row (`ATLAS_COLS` slots) can never
/// be stored contiguously: the reserved cells would wrap onto later rows, the
/// rasterized ink strip would overwrite other glyphs' coverage, and `slot_uv`
/// would hand out u1 > 1.0. `ensure_shaped` must refuse it — leaving the key
/// non-resident so the renderer keeps scalar per-cell fallback, exactly like
/// atlas exhaustion — without burning slots or touching pixels.
#[test]
fn over_wide_shaped_span_falls_back_to_scalar() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let slots_before = atlas.next_slot;
    let pixels_before = atlas.data.clone();
    let revision_before = atlas.revision;
    let key = ShapedGlyphKey {
        face_fingerprint: 1,
        style: FontStyle::Regular,
        glyph_id: font.glyph_id('=').0,
        span_cells: (ATLAS_COLS + 1) as u8,
        anchor_cell: 0,
    };
    assert_eq!(atlas.ensure_shaped(&font, key), None);
    assert!(
        !atlas.contains_shaped(key),
        "an over-wide span must not become resident"
    );
    assert_eq!(atlas.shaped_slot_count(), 0);
    assert_eq!(
        atlas.next_slot, slots_before,
        "a refused span must not burn slots"
    );
    assert_eq!(atlas.data, pixels_before, "pixels must be untouched");
    assert_eq!(atlas.revision, revision_before);
}

/// Defense-in-depth at the allocator: `allocate_slots` itself rejects a span
/// wider than the atlas row, independent of the `ensure_shaped` screen.
#[test]
fn allocate_slots_rejects_span_wider_than_a_row() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let slots_before = atlas.next_slot;
    assert_eq!(atlas.allocate_slots(ATLAS_COLS + 1), None);
    assert_eq!(atlas.next_slot, slots_before, "no slots may be consumed");
}

/// Boundary: a span of exactly `ATLAS_COLS` starting at a row boundary is
/// storable. The prebuilt fallback + ASCII region ends row-aligned, so a
/// fresh atlas allocates a full-row span at column 0 with in-range UVs.
#[test]
fn full_row_shaped_span_allocates_within_row_bounds() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    assert_eq!(
        atlas.next_slot % atlas.cols,
        0,
        "precondition: the prebuilt region ends row-aligned"
    );
    let key = ShapedGlyphKey {
        face_fingerprint: 1,
        style: FontStyle::Regular,
        glyph_id: font.glyph_id('=').0,
        span_cells: ATLAS_COLS as u8,
        anchor_cell: 0,
    };
    let _ = atlas.ensure_shaped(&font, key);
    assert!(
        atlas.contains_shaped(key),
        "a full-row span at a row boundary must be storable"
    );
    let slot = *atlas.shaped.get(&key).expect("resident slot");
    assert_eq!(slot % atlas.cols, 0, "the lead must sit at column 0");
    let uv = atlas.slot_uv(slot);
    assert!(uv[0] >= 0.0 && uv[2] <= 1.0, "u range in bounds: {uv:?}");
    assert!(uv[1] >= 0.0 && uv[3] <= 1.0, "v range in bounds: {uv:?}");
}

/// A 3..=16-cell span whose lead would land near the row edge must burn
/// fillers to the NEXT ROW BOUNDARY, not just one slot: with the allocator at
/// column 14, a 3-cell span burning a single filler would start at column 15
/// and still cross the row — reserved cells wrapping onto the next row, ink
/// overwriting other glyphs' pixels, and a UV rect past the right atlas edge
/// (u1 > 1.0). The run must instead start at column 0 of the next row with
/// fully in-bounds UVs, and the burned fillers must keep the slot bookkeeping
/// dense so existing UVs stay valid.
#[test]
fn midrow_multi_cell_span_advances_to_the_next_row_boundary() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    // Advance the allocator to column 14 of a row.
    while atlas.next_slot % atlas.cols != 14 {
        atlas.allocate_slots(1).expect("single-slot filler");
    }
    let at_column_14 = atlas.next_slot;
    let lead = atlas.allocate_slots(3).expect("3-cell span");
    assert_eq!(lead % atlas.cols, 0, "lead must start a fresh row");
    assert_eq!(
        lead,
        at_column_14 + 2,
        "columns 14 and 15 are burned as fillers"
    );
    assert_eq!(
        atlas.next_slot,
        lead + 3,
        "bookkeeping stays dense past the reserved cells"
    );
    assert_eq!(
        atlas.slot_ink.len() as u32,
        atlas.next_slot,
        "every consumed slot (fillers included) has a dense ink entry"
    );
    assert_eq!(atlas.slot_span[lead as usize], 3);
    let uv = atlas.slot_uv(lead);
    assert!(
        uv[0] >= 0.0 && uv[2] <= 1.0,
        "u range must stay in bounds: {uv:?}"
    );
    assert!(
        uv[1] >= 0.0 && uv[3] <= 1.0,
        "v range must stay in bounds: {uv:?}"
    );
}

/// The boundary-adjacent case that must NOT burn fillers: a span that exactly
/// fits the remaining columns of the current row allocates in place.
#[test]
fn multi_cell_span_exactly_fitting_the_row_allocates_in_place() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    while atlas.next_slot % atlas.cols != 13 {
        atlas.allocate_slots(1).expect("single-slot filler");
    }
    let at_column_13 = atlas.next_slot;
    let lead = atlas.allocate_slots(3).expect("3-cell span");
    assert_eq!(lead, at_column_13, "columns 13..16 fit without wrapping");
    let uv = atlas.slot_uv(lead);
    assert!(uv[2] <= 1.0, "u1 in bounds at the exact fit: {uv:?}");
}

/// Exhaustion stays clean through the filler-burn loop: when the hard slot cap
/// lands mid-burn, the allocation reports `None` without panicking and without
/// corrupting the dense bookkeeping.
#[test]
fn filler_burn_hitting_the_slot_cap_fails_cleanly() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    while atlas.next_slot % atlas.cols != 15 {
        atlas.allocate_slots(1).expect("single-slot filler");
    }
    // Cap the atlas at one slot of headroom: the 3-cell span at column 15
    // needs a filler burn plus three slots — the cap must stop it cleanly
    // (whether the burn or the span reservation trips it).
    atlas.max_slots = atlas.next_slot + 1;
    assert_eq!(atlas.allocate_slots(3), None);
    assert_eq!(
        atlas.slot_ink.len() as u32,
        atlas.next_slot,
        "bookkeeping stays dense after a failed allocation"
    );
}

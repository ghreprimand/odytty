// SPDX-License-Identifier: GPL-3.0-only
//! Bearing-aware glyph-quad geometry tests. (M5 mechanical split from atlas.rs).

use super::*;

/// The bearing-aware quad for a missing/unsupported glyph is the fallback
/// box, and its bounds are the full cell with a UV identical to `uv_rect` —
/// so missing glyphs render exactly as before (no regression).
#[test]
fn glyph_quad_fallback_is_full_cell_and_matches_uv_rect() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 24.0);
    let q = atlas.glyph_quad('é').expect("fallback quad");
    assert_eq!((q.offset_x, q.offset_y), (0, 0));
    assert_eq!((q.width, q.height), (atlas.cell.width, atlas.cell.height));
    assert_eq!(q.uv, atlas.uv_rect('é').expect("fallback uv"));
    // Control characters resolve to nothing through either entry point;
    // space resolves to its (blank) ASCII slot exactly like `uv_rect`, and
    // the grid skips it via the `ch != ' '` guard rather than a `None`.
    assert!(atlas.glyph_quad('\n').is_none());
    assert_eq!(atlas.uv_rect('\n'), None);
    assert!(atlas.glyph_quad(' ').is_some());
    assert!(atlas.uv_rect(' ').is_some());
}

/// A glyph's quad bounds are tight to its actual inked pixels: the UV rect
/// reconstructs the exact ink bounding box scanned from the bitmap, and the
/// reported offset/size match it relative to the cell origin.
#[test]
fn glyph_quad_bounds_track_actual_ink() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 28.0);
    let slot = 'g' as u32 - FIRST_CHAR + 1;
    let (minx, miny, maxx, maxy) = scan_slot_ink(&atlas, slot).expect("'g' has ink");
    let q = atlas.glyph_quad('g').expect("g quad");

    // UV reconstructs the exact ink bounding box (inclusive max -> +1).
    let ix = (q.uv[0] * atlas.width as f32).round() as i32;
    let iy = (q.uv[1] * atlas.height as f32).round() as i32;
    let ex = (q.uv[2] * atlas.width as f32).round() as i32;
    let ey = (q.uv[3] * atlas.height as f32).round() as i32;
    assert_eq!((ix, iy), (minx, miny), "uv top-left == ink top-left");
    assert_eq!((ex, ey), (maxx + 1, maxy + 1), "uv extent == ink extent");
    assert_eq!(q.width, (maxx - minx + 1) as u32);
    assert_eq!(q.height, (maxy - miny + 1) as u32);

    // Offset is the ink top-left relative to the cell's inner origin.
    let (ox, oy) = slot_offset(slot, atlas.cols, atlas.cell);
    let border = slot_border(atlas.cell) as i32;
    assert_eq!(q.offset_x, minx - (ox as i32 + border));
    assert_eq!(q.offset_y, miny - (oy as i32 + border));
}

/// Box-drawing strokes must join seamlessly across cells: the horizontal line
/// U+2500's quad spans the full cell width (its ink reaches both edges), so
/// adjacent cells' strokes meet flush rather than leaving a gutter.
#[test]
fn box_drawing_quad_spans_full_cell_width() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    if !font_has_glyph(&font, '\u{2500}') {
        eprintln!("skipping: font lacks box-drawing glyph");
        return;
    }
    let mut atlas = GlyphAtlas::build(&font, 28.0);
    atlas.ensure(&font, '\u{2500}').expect("U+2500 uv");
    let q = atlas.glyph_quad('\u{2500}').expect("U+2500 quad");
    let cw = atlas.cell.width as i32;
    // Ink starts at (or just past) the left edge and reaches the right edge:
    // the quad spans the cell horizontally so neighbors join flush.
    assert!(
        q.offset_x <= 1,
        "horizontal rule should start at the left edge"
    );
    assert!(
        q.offset_x + q.width as i32 >= cw - 1,
        "horizontal rule should reach the right edge"
    );
}

/// At least one real glyph inks beyond the cell box, and its quad reports
/// that overflow (negative offset or size exceeding the cell) instead of
/// clipping to the cell — the core R3 capability. Best-effort across a broad
/// codepoint range; skipped only if the loaded font never overflows a cell.
#[test]
fn some_glyph_quad_overflows_the_cell() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 28.0);
    let cw = atlas.cell.width as i32;
    let ch_h = atlas.cell.height as i32;
    let exceeds = |q: &GlyphBounds| {
        q.offset_x < 0
            || q.offset_y < 0
            || q.offset_x + q.width as i32 > cw
            || q.offset_y + q.height as i32 > ch_h
    };

    // Printable ASCII first (already resident), then a sweep of common
    // glyph-bearing codepoints (rasterized on demand) likely to overflow.
    let ascii = (FIRST_CHAR..=LAST_CHAR).filter_map(char::from_u32);
    let extras = (0x00A1u32..=0x2600).filter_map(char::from_u32);
    let mut found = None;
    for ch in ascii {
        if let Some(q) = atlas.glyph_quad(ch)
            && exceeds(&q)
        {
            found = Some((ch, q));
            break;
        }
    }
    if found.is_none() {
        for ch in extras {
            if !font_has_glyph(&font, ch) {
                continue;
            }
            atlas.ensure(&font, ch);
            if let Some(q) = atlas.glyph_quad(ch)
                && exceeds(&q)
            {
                found = Some((ch, q));
                break;
            }
        }
    }

    match found {
        Some((ch, q)) => assert!(
            exceeds(&q),
            "glyph {ch:?} quad {q:?} should exceed the {cw}x{ch_h} cell"
        ),
        None => eprintln!("skipping: loaded font has no cell-overflowing glyph"),
    }
}

/// `glyph_quad` resolution mirrors `uv_rect`: a resident styled glyph yields
/// its own slot's bounds, distinct from the regular glyph.
#[test]
fn styled_glyph_quad_resolves_styled_slot() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas
        .ensure_styled(&font, FontStyle::Bold, 'A')
        .expect("bold A");
    let regular = atlas.glyph_quad('A').expect("regular A quad");
    let bold = atlas
        .glyph_quad_styled(FontStyle::Bold, 'A')
        .expect("bold A quad");
    // Distinct slots => distinct UV rects.
    assert_ne!(regular.uv, bold.uv);
}

/// A zero-width combining mark rasterizes with a one-cell pen anchor, so its
/// (left-hanging) ink lands over the slot's cell box and the renderer can
/// draw the mark quad at the base cell's origin. The recorded ink must be
/// non-empty and horizontally intersect the cell.
#[test]
fn combining_mark_ink_lands_over_the_cell_box() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mark = '\u{0301}'; // combining acute accent
    if !font_has_glyph(&font, mark) {
        eprintln!("skipping: font has no combining acute");
        return;
    }
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas
        .ensure_styled(&font, FontStyle::Regular, mark)
        .expect("mark uv");
    let quad = atlas
        .combining_mark_quad(FontStyle::Regular, mark)
        .expect("mark quad");
    assert!(quad.width > 0 && quad.height > 0, "mark must ink pixels");
    let cell_w = atlas.cell.width as i32;
    assert!(
        quad.offset_x < cell_w && quad.offset_x + quad.width as i32 > 0,
        "mark ink [{}, {}) must intersect the cell [0, {cell_w})",
        quad.offset_x,
        quad.offset_x + quad.width as i32
    );
}

/// `combining_mark_quad` never yields the hollow-box fallback: a mark that is
/// not resident — or one the font lacks (which `ensure_styled` caches as the
/// fallback slot) — returns `None`, because the mark quad composites OVER an
/// already-drawn base glyph and a tofu box there would obscure the base.
#[test]
fn combining_mark_quad_filters_missing_marks() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    // Never ensured: no resident slot, no quad (glyph_quad_styled would have
    // reported the fallback box here).
    assert!(
        atlas
            .combining_mark_quad(FontStyle::Regular, '\u{0301}')
            .is_none()
    );
    // A mark the font lacks resolves to the fallback slot via ensure_styled
    // and must still be filtered.
    let missing = (0x0591..=0x05BD_u32)
        .chain(0x0E31..=0x0E31)
        .filter_map(char::from_u32)
        .find(|&ch| is_combining_mark(ch) && !font_has_glyph(&font, ch));
    let Some(missing) = missing else {
        eprintln!("skipping: font covers every probed mark");
        return;
    };
    atlas
        .ensure_styled(&font, FontStyle::Regular, missing)
        .expect("fallback uv");
    assert!(
        atlas
            .combining_mark_quad(FontStyle::Regular, missing)
            .is_none(),
        "font-missing mark must not resolve to the tofu box"
    );
}

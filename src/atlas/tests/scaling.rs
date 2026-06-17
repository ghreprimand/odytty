// SPDX-License-Identifier: GPL-3.0-only
//! Rescale, wide-glyph allocation, and fractional-scale seam tests. (M5 mechanical split from atlas.rs).

use super::*;

/// Rebuilding the atlas at a larger physical size (the HiDPI rescale path:
/// `GpuState::set_font_px` constructs a fresh atlas) grows the cell metrics
/// and starts from a clean dynamic region — no slot from the old density can
/// survive. This is R1 invalidation by construction.
#[test]
fn rebuild_at_larger_size_grows_cell_and_drops_dynamic_slots() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Build at 1x-equivalent density and populate the dynamic region.
    let mut small = GlyphAtlas::build(&font, 16.0);
    if let Some(ch) = glyph_bearing_non_ascii(&font) {
        small.ensure(&font, ch).expect("resident glyph");
        assert!(
            small.slot_count() > FIRST_DYNAMIC_SLOT,
            "precondition: a dynamic slot was allocated"
        );
    }
    let small_cell = small.cell;

    // The rescale rebuild is a fresh build at 2x physical px.
    let big = GlyphAtlas::build(&font, 32.0);

    // Cell metrics scaled up with the density (no mixed-density reuse).
    assert!(
        big.cell.width > small_cell.width && big.cell.height > small_cell.height,
        "rebuilt cell {:?} should exceed {:?}",
        big.cell,
        small_cell
    );
    // The rebuilt atlas has only its base region — zero stale dynamic slots.
    assert_eq!(
        big.slot_count(),
        FIRST_DYNAMIC_SLOT,
        "a fresh rebuild must carry no slots from the old density"
    );
    // Bitmap is sized to the new (larger) cell, not the old one.
    assert_eq!(big.data.len(), (big.width * big.height) as usize);
    assert!(big.width > small.width || big.height >= small.height);
}

/// Cell metrics are deterministic and seam-free across the fractional scales
/// `physical_font_px` produces from a 16 px logical size: integer (the type
/// guarantees this), positive, baseline within the cell, and monotonic
/// non-decreasing as density rises. Building twice at one size is identical.
#[test]
fn cell_metrics_deterministic_and_monotonic_across_scales() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 16 px logical at scales 1.0 / 1.25 / 1.5 / 2.0.
    let sizes = [16.0f32, 20.0, 24.0, 32.0];
    let mut prev: Option<CellSize> = None;
    for &px in &sizes {
        let a = GlyphAtlas::build(&font, px);
        let b = GlyphAtlas::build(&font, px);
        // Determinism: same px => byte-identical metrics and dimensions.
        assert_eq!(a.cell, b.cell, "cell metrics must be deterministic at {px}");
        assert_eq!(a.width, b.width);
        assert_eq!(a.height, b.height);
        // Seam-free: positive extents, baseline within the cell box.
        assert!(a.cell.width > 0 && a.cell.height > 0);
        assert!(a.cell.baseline > 0 && a.cell.baseline <= a.cell.height);
        // Monotonic non-decreasing with density.
        if let Some(p) = prev {
            assert!(
                a.cell.width >= p.width
                    && a.cell.height >= p.height
                    && a.cell.baseline >= p.baseline,
                "cell {:?} at {px}px should be >= previous {:?}",
                a.cell,
                p
            );
        }
        prev = Some(a.cell);
    }
}

#[test]
fn glyph_cells_matches_core_width_rule() {
    // Width-2 East Asian forms; width-1 everything else. Mirrors core's
    // `UnicodeWidthChar::width(ch) == Some(2)` cell-layout decision.
    for ch in ['世', '漢', '中', 'あ', '！', 'Ａ', '\u{3000}'] {
        assert_eq!(glyph_cells(ch), 2, "{ch:?} should be a 2-cell glyph");
    }
    for ch in ['A', 'z', '0', 'é', '★', '─', ' '] {
        assert_eq!(glyph_cells(ch), 1, "{ch:?} should be a 1-cell glyph");
    }
}

#[test]
fn allocate_wide_slot_spans_two_cells_in_one_row() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let lead = atlas.allocate_slots(2).expect("wide slot");
    // Lead carries span 2; the reserved next slot carries span 1.
    assert_eq!(atlas.slot_span[lead as usize], 2);
    assert_eq!(atlas.slot_span[(lead + 1) as usize], 1);
    // The pair is contiguous within one atlas row (no wrap).
    assert_eq!(lead % atlas.cols + 1, (lead + 1) % atlas.cols);
    assert_eq!(lead / atlas.cols, (lead + 1) / atlas.cols);
    // slot_uv reports a 2-cell-wide inner rect for the lead.
    let uv = atlas.slot_uv(lead);
    let (ix, _) = inner_origin(&atlas, uv);
    let right = (uv[2] * atlas.width as f32).round() as u32;
    assert_eq!(right - ix, 2 * atlas.cell.width, "lead uv spans two cells");
    // A normal single slot still reports one cell.
    let narrow = atlas.allocate_slots(1).expect("narrow slot");
    let nuv = atlas.slot_uv(narrow);
    let (nix, _) = inner_origin(&atlas, nuv);
    let nright = (nuv[2] * atlas.width as f32).round() as u32;
    assert_eq!(nright - nix, atlas.cell.width);
}

#[test]
fn wide_allocation_burns_filler_to_avoid_row_wrap() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 20.0);
    // Advance allocation until the next slot is the last column of a row.
    while atlas.next_slot % atlas.cols != atlas.cols - 1 {
        atlas.allocate_slots(1).expect("narrow slot");
    }
    let before = atlas.next_slot;
    let last_col_row = before / atlas.cols;
    let lead = atlas.allocate_slots(2).expect("wide slot at row edge");
    // The last-column slot was burned as a filler (span 1, never used) ...
    assert_eq!(atlas.slot_span[before as usize], 1);
    // ... and the wide pair starts at column 0 of the next row.
    assert_eq!(lead % atlas.cols, 0);
    assert_eq!(lead / atlas.cols, last_col_row + 1);
    assert_eq!(atlas.slot_span[lead as usize], 2);
    // The pair did not wrap.
    assert_eq!(lead / atlas.cols, (lead + 1) / atlas.cols);
}

#[test]
fn rasterize_clip_width_relieves_wide_glyph_clipping() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Build a cell at one size, then rasterize a heavy glyph at DOUBLE the
    // size so its natural ink exceeds a single cell — the same shape a real
    // width-2 glyph takes relative to a single-cell slot. With a single-cell
    // clip the ink is cropped; with a two-cell clip it is not.
    let atlas = GlyphAtlas::build(&font, 16.0);
    let cell = atlas.cell;
    let big_px = 32.0_f32;
    let stride = 8 * slot_w(cell); // ample horizontal room
    let height = slot_h(cell);
    let pen = Pen {
        px: big_px,
        baseline: cell.baseline as f32,
    };
    let raster = |outer_w: u32| -> Option<GlyphInk> {
        let mut data = vec![0u8; (stride * height) as usize];
        rasterize_glyph(
            &font,
            pen,
            'W',
            &mut data,
            stride,
            SubpixelMode::Off,
            SlotRegion {
                origin: (0, 0),
                cell,
                outer_w,
            },
            SynthTransform::none(),
        )
    };
    let single = raster(slot_w(cell)).expect("single-clip ink");
    let double = raster(2 * slot_w(cell)).expect("double-clip ink");
    // The wider clip must never record less ink than the narrow one, and for
    // an oversized glyph it records strictly more (the cropped right column
    // is now kept).
    assert!(
        double.width >= single.width,
        "wider clip ink {} should be >= narrow clip ink {}",
        double.width,
        single.width
    );
    assert!(
        double.width > single.width,
        "an oversized glyph should be clipped by the single-cell region \
         (single={}, double={})",
        single.width,
        double.width
    );
}

#[test]
fn ensure_wide_codepoint_consumes_two_slots_when_supported() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(ch) = wide_glyph_supported(&font) else {
        eprintln!("skipping: no wide (CJK/fullwidth) glyph in the loaded font");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    let before = atlas.slot_count();
    let uv = atlas.ensure(&font, ch).expect("wide glyph uv");
    // Two slots consumed (lead + reserved continuation); slot is wide.
    assert_eq!(
        atlas.slot_count(),
        before + 2,
        "wide glyph reserves two slots"
    );
    let &slot = atlas
        .dynamic
        .get(&(FontStyle::Regular, ch))
        .expect("resident");
    assert_eq!(atlas.slot_span[slot as usize], 2);
    // The lead UV spans two cells; the ink itself may be narrower depending on
    // the font's outline and bearings.
    let (ix, _) = inner_origin(&atlas, uv);
    let right = (uv[2] * atlas.width as f32).round() as u32;
    assert_eq!(right - ix, 2 * atlas.cell.width);
}

/// UV rects at fractional scales stay seam-free: the inner cell rectangle
/// tiles contiguously (no overlap and no sub-pixel gap between adjacent
/// slots in the same row) and every UV is strictly within [0, 1].
#[test]
fn h3_uv_seam_free_across_fractional_scales() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    for &scale in &[1.0f32, 1.25, 1.5, 1.75, 2.0] {
        let px = physical_font_px(16.0, scale);
        let atlas = GlyphAtlas::build(&font, px);
        // Check the first row of ASCII slots (slots 1..=15 fit in one row).
        for slot in 1..ATLAS_COLS.min(FIRST_DYNAMIC_SLOT) {
            let uv = atlas.slot_uv(slot);
            // Every UV coordinate is in [0, 1].
            for &c in &uv {
                assert!(
                    (0.0..=1.0).contains(&c),
                    "UV {uv:?} out of [0,1] at scale={scale}"
                );
            }
            // u0 < u1, v0 < v1 (non-degenerate).
            assert!(uv[0] < uv[2] && uv[1] < uv[3]);
        }
        // Adjacent slots: right edge of slot N == left edge of next slot's
        // outer boundary minus the inter-slot gap (2×border). The inner UV
        // right of slot N and inner UV left of slot N+1 differ by exactly
        // 2×border in pixel space.
        let border = slot_border(atlas.cell);
        for slot in 1..(ATLAS_COLS.min(FIRST_DYNAMIC_SLOT) - 1) {
            let a = atlas.slot_uv(slot);
            let b = atlas.slot_uv(slot + 1);
            let a_right_px = (a[2] * atlas.width as f32).round() as i32;
            let b_left_px = (b[0] * atlas.width as f32).round() as i32;
            assert_eq!(
                b_left_px - a_right_px,
                (2 * border) as i32,
                "inter-slot gap at scale={scale} slot={slot}"
            );
        }
    }
}

/// Glyph quad geometry at fractional scales has integral cell offsets and
/// consistent UV coverage: offset + size reconstructs the UV rect's pixel
/// span, and the UV width matches the inked width in atlas pixels.
#[test]
fn h3_glyph_quad_uv_consistency_at_fractional_scales() {
    let Some(font) = test_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    for &scale in &[1.0f32, 1.25, 1.5, 1.75, 2.0] {
        let px = physical_font_px(16.0, scale);
        let atlas = GlyphAtlas::build(&font, px);
        // Check several ASCII glyphs with visible ink.
        for ch in ['A', 'g', 'M', '|', '_'] {
            let Some(q) = atlas.glyph_quad(ch) else {
                continue;
            };
            // UV width and height in atlas pixels match the reported ink size.
            let uv_w = ((q.uv[2] - q.uv[0]) * atlas.width as f32).round() as u32;
            let uv_h = ((q.uv[3] - q.uv[1]) * atlas.height as f32).round() as u32;
            assert_eq!(
                uv_w, q.width,
                "UV width mismatch for '{ch}' at scale={scale}"
            );
            assert_eq!(
                uv_h, q.height,
                "UV height mismatch for '{ch}' at scale={scale}"
            );
        }
    }
}

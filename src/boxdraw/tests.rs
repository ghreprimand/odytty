// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for the geometric box-drawing module. These are pure: they call
//! [`coverage`]/[`covers`] with explicit cell sizes and never touch a font, the
//! atlas, or any global state, so they run deterministically everywhere.

use super::*;

/// A representative non-square cell, like a real monospace metric.
const W: u32 = 9;
const H: u32 = 18;

fn cov(ch: char) -> Vec<u8> {
    coverage(ch, W, H).expect("covered codepoint")
}

fn at(buf: &[u8], x: u32, y: u32) -> u8 {
    buf[(y * W + x) as usize]
}

/// Any pixel in row `y` inked.
fn row_has_ink(buf: &[u8], y: u32) -> bool {
    (0..W).any(|x| at(buf, x, y) > 0)
}

/// Any pixel in column `x` inked.
fn col_has_ink(buf: &[u8], x: u32) -> bool {
    (0..H).any(|y| at(buf, x, y) > 0)
}

#[test]
fn coverage_buffer_has_exact_cell_size() {
    let buf = coverage('\u{2500}', W, H).unwrap();
    assert_eq!(buf.len(), (W * H) as usize);
}

#[test]
fn zero_size_cell_is_none() {
    assert!(coverage('\u{2500}', 0, 10).is_none());
    assert!(coverage('\u{2500}', 10, 0).is_none());
}

#[test]
fn covers_matches_coverage() {
    // A covered glyph and an uncovered one stay consistent across both entry
    // points.
    assert!(covers('\u{2500}'));
    assert!(coverage('\u{2500}', W, H).is_some());
    assert!(!covers('A'));
    assert!(coverage('A', W, H).is_none());
    assert!(!covers(' '));
}

#[test]
fn light_horizontal_fills_full_width_on_one_band() {
    let buf = cov('\u{2500}'); // ─
    let mid = H / 2;
    // The midline row is inked edge to edge.
    for x in 0..W {
        assert!(at(&buf, x, mid) > 0, "gap at x={x}");
    }
    // Top and bottom rows are clear (a line, not a fill).
    assert!(!row_has_ink(&buf, 0));
    assert!(!row_has_ink(&buf, H - 1));
}

#[test]
fn light_vertical_fills_full_height() {
    let buf = cov('\u{2502}'); // │
    let mid = W / 2;
    for y in 0..H {
        assert!(at(&buf, mid, y) > 0, "gap at y={y}");
    }
    assert!(!col_has_ink(&buf, 0));
    assert!(!col_has_ink(&buf, W - 1));
}

#[test]
fn heavy_line_is_thicker_than_light() {
    let light = cov('\u{2500}'); // ─
    let heavy = cov('\u{2501}'); // ━
    let count = |b: &[u8]| b.iter().filter(|&&v| v > 0).count();
    assert!(
        count(&heavy) > count(&light),
        "heavy line should ink more pixels than light"
    );
}

#[test]
fn corner_and_line_join_seamlessly_across_the_cell_boundary() {
    // "┌─": the corner's right arm must reach the right edge on the same rows
    // that the horizontal line inks at its left edge, so the two cells meet with
    // no gap at the shared boundary.
    let corner = cov('\u{250C}'); // ┌
    let line = cov('\u{2500}'); // ─
    let mut joined = false;
    for y in 0..H {
        if at(&corner, W - 1, y) > 0 {
            assert!(
                at(&line, 0, y) > 0,
                "corner inks right edge at y={y} but line has no ink at its left edge"
            );
            joined = true;
        }
    }
    assert!(joined, "corner never reached the right edge");
}

#[test]
fn vertical_lines_stack_seamlessly() {
    // "│" over "│": the bottom row of one cell and the top row of the next are
    // both inked on the same column.
    let buf = cov('\u{2502}');
    let mid = W / 2;
    assert!(at(&buf, mid, 0) > 0, "no ink at top edge");
    assert!(at(&buf, mid, H - 1) > 0, "no ink at bottom edge");
}

#[test]
fn cross_inks_both_axes_through_the_center() {
    let buf = cov('\u{253C}'); // ┼
    let midx = W / 2;
    let midy = H / 2;
    // Full horizontal and full vertical through the center.
    for x in 0..W {
        assert!(at(&buf, x, midy) > 0, "horizontal gap at x={x}");
    }
    for y in 0..H {
        assert!(at(&buf, midx, y) > 0, "vertical gap at y={y}");
    }
}

#[test]
fn half_line_stub_only_reaches_one_edge() {
    let buf = cov('\u{2576}'); // ╶ light right
    let midy = H / 2;
    // Right edge inked, left edge clear.
    assert!(at(&buf, W - 1, midy) > 0);
    assert_eq!(at(&buf, 0, midy), 0);
}

#[test]
fn full_block_fills_every_pixel() {
    let buf = cov('\u{2588}'); // █
    assert!(buf.iter().all(|&v| v == 255), "full block should be solid");
}

#[test]
fn upper_half_block_fills_only_the_top() {
    let buf = cov('\u{2580}'); // ▀
    assert!(row_has_ink(&buf, 0));
    assert!(row_has_ink(&buf, H / 2 - 1));
    assert!(!row_has_ink(&buf, H - 1));
}

#[test]
fn lower_eighth_ladder_grows_monotonically() {
    let count = |ch: char| cov(ch).iter().filter(|&&v| v > 0).count();
    let one = count('\u{2581}'); // ▁
    let four = count('\u{2584}'); // ▄ (lower half)
    let full = count('\u{2588}'); // █
    assert!(one < four && four < full);
    assert_eq!(full, (W * H) as usize);
}

#[test]
fn left_half_block_fills_only_the_left() {
    let buf = cov('\u{258C}'); // ▌
    assert!(col_has_ink(&buf, 0));
    assert!(!col_has_ink(&buf, W - 1));
}

#[test]
fn right_half_block_fills_only_the_right() {
    let buf = cov('\u{2590}'); // ▐
    assert!(col_has_ink(&buf, W - 1));
    assert!(!col_has_ink(&buf, 0));
}

#[test]
fn shades_are_constant_partial_coverage() {
    let light = cov('\u{2591}'); // ░
    let medium = cov('\u{2592}'); // ▒
    let dark = cov('\u{2593}'); // ▓
    // Uniform fill at increasing levels, none fully opaque.
    assert!(light.iter().all(|&v| v == light[0]));
    assert!(medium.iter().all(|&v| v == medium[0]));
    assert!(dark.iter().all(|&v| v == dark[0]));
    assert!(light[0] < medium[0] && medium[0] < dark[0] && dark[0] < 255);
}

#[test]
fn quadrant_lower_left_fills_only_that_quarter() {
    let buf = cov('\u{2596}'); // ▖ lower-left
    // A pixel deep in the lower-left quadrant is inked.
    assert!(at(&buf, 1, H - 2) > 0);
    // The other three corners are clear.
    assert_eq!(at(&buf, 1, 1), 0); // upper-left
    assert_eq!(at(&buf, W - 2, 1), 0); // upper-right
    assert_eq!(at(&buf, W - 2, H - 2), 0); // lower-right
}

#[test]
fn braille_blank_is_covered_but_empty() {
    let buf = cov('\u{2800}'); // blank Braille pattern
    assert!(buf.iter().all(|&v| v == 0));
}

#[test]
fn braille_dot_bits_land_in_their_grid_positions() {
    let dot1 = cov('\u{2801}'); // dot 1: upper-left
    assert!(at(&dot1, 2, 2) > 0);
    assert_eq!(at(&dot1, W - 2, H - 2), 0);

    let dot4 = cov('\u{2808}'); // dot 4: upper-right
    assert!(at(&dot4, W - 2, 2) > 0);
    assert_eq!(at(&dot4, 2, 2), 0);

    let dot8 = cov('\u{2880}'); // dot 8: lower-right
    assert!(at(&dot8, W - 2, H - 2) > 0);
    assert_eq!(at(&dot8, 2, H - 2), 0);
}

#[test]
fn braille_full_inks_all_eight_dot_regions() {
    let buf = cov('\u{28FF}'); // all eight dots
    for &(x, y) in &[
        (2, 2),
        (2, H / 4 + 2),
        (2, H / 2 + 2),
        (2, H - 2),
        (W - 2, 2),
        (W - 2, H / 4 + 2),
        (W - 2, H / 2 + 2),
        (W - 2, H - 2),
    ] {
        assert!(at(&buf, x, y) > 0, "missing Braille dot at {x},{y}");
    }
}

#[test]
fn double_horizontal_has_two_separated_rails() {
    let buf = cov('\u{2550}'); // ═
    let midy = H / 2;
    // The midline itself sits in the gap between the two rails.
    assert_eq!(
        at(&buf, W / 2, midy),
        0,
        "double line should be open at center"
    );
    // Two distinct inked rows above and below the midline.
    let inked_rows: Vec<u32> = (0..H).filter(|&y| row_has_ink(&buf, y)).collect();
    assert!(
        inked_rows.len() >= 2,
        "expected at least two rails, got rows {inked_rows:?}"
    );
    assert!(inked_rows.iter().any(|&y| y < midy));
    assert!(inked_rows.iter().any(|&y| y > midy));
    // Both rails span the full width (seamless across cells).
    for &y in &inked_rows {
        assert!(at(&buf, 0, y) > 0 && at(&buf, W - 1, y) > 0);
    }
}

#[test]
fn double_vertical_has_two_separated_rails() {
    let buf = cov('\u{2551}'); // ║
    let midx = W / 2;
    assert_eq!(at(&buf, midx, H / 2), 0);
    let inked_cols: Vec<u32> = (0..W).filter(|&x| col_has_ink(&buf, x)).collect();
    assert!(inked_cols.iter().any(|&x| x < midx));
    assert!(inked_cols.iter().any(|&x| x > midx));
}

#[test]
fn double_corner_reaches_two_edges() {
    let buf = cov('\u{2554}'); // ╔ (down + right)
    // Reaches the right edge (horizontal rails) and the bottom edge (vertical
    // rails); the top and left edges stay clear.
    assert!(col_has_ink(&buf, W - 1), "no ink at right edge");
    assert!(row_has_ink(&buf, H - 1), "no ink at bottom edge");
    assert!(!row_has_ink(&buf, 0), "top edge should be clear");
    assert!(!col_has_ink(&buf, 0), "left edge should be clear");
}

#[test]
fn rounded_corner_arms_reach_edges() {
    let buf = cov('\u{256D}'); // ╭ (down + right, rounded)
    // The straight arms still reach the right and bottom edges so the corner
    // tiles with its neighbors.
    assert!(col_has_ink(&buf, W - 1), "right arm must reach the edge");
    assert!(row_has_ink(&buf, H - 1), "down arm must reach the edge");
}

#[test]
fn diagonals_ink_opposite_corners() {
    let back = cov('\u{2572}'); // ╲ top-left to bottom-right
    assert!(at(&back, 0, 0) > 0);
    assert!(at(&back, W - 1, H - 1) > 0);

    let fwd = cov('\u{2571}'); // ╱ bottom-left to top-right
    assert!(at(&fwd, 0, H - 1) > 0);
    assert!(at(&fwd, W - 1, 0) > 0);
}

#[test]
fn powerline_right_triangle_points_right() {
    let buf = cov('\u{E0B0}'); // right-pointing filled triangle
    let midy = H / 2;
    // Apex reaches the right edge on the mid row.
    assert!(
        at(&buf, W - 1, midy) > 0,
        "apex should reach the right edge"
    );
    // Base fills the left edge across the full height.
    assert!(at(&buf, 0, 0) > 0 && at(&buf, 0, H - 1) > 0);
    // The top-right corner is outside the triangle.
    assert_eq!(at(&buf, W - 1, 0), 0);
}

#[test]
fn powerline_left_triangle_points_left() {
    let buf = cov('\u{E0B2}'); // left-pointing filled triangle
    let midy = H / 2;
    assert!(at(&buf, 0, midy) > 0, "apex should reach the left edge");
    assert!(at(&buf, W - 1, 0) > 0 && at(&buf, W - 1, H - 1) > 0);
    assert_eq!(at(&buf, 0, 0), 0);
}

#[test]
fn box_thickness_default_multiplier_is_byte_identical() {
    // BOXTHICK: the default `1.0` multiplier must reproduce the historical
    // light-line thickness formula exactly. `x * 1.0 == x` in f32, so every
    // cell metric matches `(min(w, h) / 8).round().max(1)`. This is the pure
    // proof behind the unchanged default render path.
    for w in 1u32..=40 {
        for h in 1u32..=40 {
            let legacy = ((w.min(h) as f32 / 8.0).round() as u32).max(1);
            assert_eq!(
                light_thickness_with(w, h, 1.0),
                legacy,
                "default thickness must match the pre-feature formula at {w}x{h}"
            );
        }
    }
}

#[test]
fn box_thickness_multiplier_scales_stroke_weight() {
    // A multiplier above 1.0 produces a heavier light line and below 1.0 a
    // lighter one, always clamped to at least one pixel. Pure: no global state.
    let thin = light_thickness_with(W, H, 0.5);
    let base = light_thickness_with(W, H, 1.0);
    let thick = light_thickness_with(W, H, 3.0);
    assert!(thin >= 1, "thickness never drops below one pixel");
    assert!(thick > base, "3.0x must be heavier than the default weight");
    assert!(thin <= base, "0.5x must not exceed the default weight");
}

#[test]
fn covered_ranges_all_produce_buffers() {
    // Every codepoint the module classifies must also produce a buffer, and the
    // documented block + powerline ranges are fully covered.
    for code in 0x2500u32..=0x257F {
        let ch = char::from_u32(code).unwrap();
        if covers(ch) {
            assert!(coverage(ch, W, H).is_some());
        }
    }
    for code in 0x2580u32..=0x259F {
        let ch = char::from_u32(code).unwrap();
        assert!(covers(ch), "block {code:#x} should be covered");
    }
    for code in 0x2800u32..=0x28FF {
        let ch = char::from_u32(code).unwrap();
        assert!(covers(ch), "Braille pattern {code:#x} should be covered");
        assert!(coverage(ch, W, H).is_some());
    }
    for code in 0xE0B0u32..=0xE0B3 {
        let ch = char::from_u32(code).unwrap();
        assert!(covers(ch), "powerline {code:#x} should be covered");
    }
}

// ---------------------------------------------------------------------------
// Symbols for Legacy Computing: sextants, octants, triangular blocks.
// ---------------------------------------------------------------------------

/// A point deep inside sextant region `r` (1..=6) of a 2-col × 3-row cell.
fn sextant_point(r: u32) -> (u32, u32) {
    // left column top→bottom = 1,2,3; right column = 4,5,6.
    let (col, row) = if r <= 3 { (0u32, r - 1) } else { (1, r - 4) };
    (col * W / 2 + 1, row * H / 3 + H / 6)
}

/// A point deep inside octant region `r` (1..=8) of a 2-col × 4-row cell.
fn octant_point(r: u32) -> (u32, u32) {
    let (col, row) = if r <= 4 { (0u32, r - 1) } else { (1, r - 5) };
    (col * W / 2 + 1, row * H / 4 + H / 8)
}

#[test]
fn sextant_masks_table_is_valid_and_distinct() {
    assert_eq!(SEXTANT_MASKS.len(), 60);
    // Region bits 1..=6 ⇒ masks fit in 6 bits and are non-empty.
    for &m in SEXTANT_MASKS {
        assert!(m != 0 && m <= 0x3F, "sextant mask {m:#x} out of range");
    }
    // All distinct — the offset→mask mapping must be one-to-one.
    let mut sorted = SEXTANT_MASKS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 60, "duplicate sextant masks");
    // Spot-check the offset↔region mapping against the Unicode names.
    assert_eq!(SEXTANT_MASKS[0], 0x01, "1FB00 = SEXTANT-1 (region 1)");
    assert_eq!(
        SEXTANT_MASKS[59], 0x3E,
        "1FB3B = SEXTANT-23456 (regions 2-6)"
    );
}

#[test]
fn octant_masks_table_is_valid_and_distinct() {
    assert_eq!(OCTANT_MASKS.len(), 230);
    for &m in OCTANT_MASKS {
        assert!(m != 0, "octant mask {m:#x} out of range");
    }
    let mut sorted = OCTANT_MASKS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 230, "duplicate octant masks");
    assert_eq!(OCTANT_MASKS[0], 0x04, "1CD00 = OCTANT-3 (region 3)");
}

#[test]
fn sextants_cover_the_full_range_and_render() {
    for code in 0x1FB00u32..=0x1FB3B {
        let ch = char::from_u32(code).unwrap();
        assert!(covers(ch), "sextant {code:#x} should be covered");
        assert_eq!(cov(ch).len(), (W * H) as usize);
    }
}

#[test]
fn octants_cover_the_full_range_and_render() {
    for code in 0x1CD00u32..=0x1CDE5 {
        let ch = char::from_u32(code).unwrap();
        assert!(covers(ch), "octant {code:#x} should be covered");
        assert!(coverage(ch, W, H).is_some());
    }
}

#[test]
fn sextant_single_region_inks_only_that_region() {
    // SEXTANT-1 (1FB00) = region 1 only: top-left sixth inked, the rest clear.
    let buf = cov('\u{1FB00}');
    let (x, y) = sextant_point(1);
    assert!(at(&buf, x, y) > 0, "region 1 should be inked");
    for r in 2..=6 {
        let (x, y) = sextant_point(r);
        assert_eq!(
            at(&buf, x, y),
            0,
            "region {r} should be clear for SEXTANT-1"
        );
    }
    // SEXTANT-6 (1FB1E) = region 6 only: bottom-right sixth inked.
    let buf = cov('\u{1FB1E}');
    let (x, y) = sextant_point(6);
    assert!(at(&buf, x, y) > 0, "region 6 should be inked");
    for r in 1..=5 {
        let (x, y) = sextant_point(r);
        assert_eq!(
            at(&buf, x, y),
            0,
            "region {r} should be clear for SEXTANT-6"
        );
    }
}

#[test]
fn sextant_left_column_matches_left_half_block() {
    // SEXTANT-123 (1FB06) fills the whole left column, so its left-column ink
    // matches the LEFT HALF block (U+258C) and its right column is clear.
    let sext = cov('\u{1FB06}');
    let half = cov('\u{258C}');
    for r in [1u32, 2, 3] {
        let (x, y) = sextant_point(r);
        assert!(at(&sext, x, y) > 0, "left region {r} inked");
    }
    for r in [4u32, 5, 6] {
        let (x, y) = sextant_point(r);
        assert_eq!(at(&sext, x, y), 0, "right region {r} clear");
    }
    // Same left-column sample inked in both glyphs.
    assert_eq!(at(&sext, 1, H / 2) > 0, at(&half, 1, H / 2) > 0);
}

#[test]
fn octant_single_region_inks_only_that_region() {
    // OCTANT-3 (1CD00) = region 3 only: left column, third row down.
    let buf = cov('\u{1CD00}');
    let (x, y) = octant_point(3);
    assert!(at(&buf, x, y) > 0, "octant region 3 should be inked");
    for r in [1u32, 2, 4, 5, 6, 7, 8] {
        let (x, y) = octant_point(r);
        assert_eq!(at(&buf, x, y), 0, "octant region {r} should be clear");
    }
}

#[test]
fn triangular_quarter_block_inks_apex_corner_only() {
    // LEFT TRIANGULAR ONE QUARTER (1FB6C): apex at the left edge, so it inks
    // the left side and leaves the far-right column clear.
    let buf = cov('\u{1FB6C}');
    assert!(at(&buf, 0, H / 2) > 0, "apex (left-center) should be inked");
    assert_eq!(at(&buf, W - 1, H / 2), 0, "right edge should be clear");
    // UPPER (1FB6D): apex at the top.
    let buf = cov('\u{1FB6D}');
    assert!(at(&buf, W / 2, 0) > 0, "apex (top-center) should be inked");
    assert_eq!(at(&buf, W / 2, H - 1), 0, "bottom edge should be clear");
}

#[test]
fn triangular_three_quarters_inverts_the_quarter() {
    // The three-quarters block inks strictly more than its quarter counterpart
    // and reaches the edge opposite the apex.
    let q = cov('\u{1FB6C}'); // LEFT quarter
    let tq = cov('\u{1FB6B}'); // complement of LOWER quarter → broad fill
    let count = |b: &[u8]| b.iter().filter(|&&v| v > 0).count();
    let three_q = cov('\u{1FB68}'); // complement of LEFT quarter
    assert!(
        count(&three_q) > count(&q),
        "three-quarters must ink more than one quarter"
    );
    // Complement of LEFT reaches the right edge.
    assert!(at(&three_q, W - 1, H / 2) > 0, "right edge should be inked");
    let _ = tq;
}

#[test]
fn eighth_ladders_grow_monotonically_and_reach_the_named_edge() {
    let count = |ch: char| cov(ch).iter().filter(|&&v| v > 0).count();
    // Upper-eighth ladder: more eighths ⇒ more ink, all at the top.
    assert!(count('\u{1FB82}') < count('\u{1FB86}')); // 1/8 < 7/8
    let upper = cov('\u{1FB86}'); // upper 7/8
    assert!(at(&upper, W / 2, 0) > 0, "top row inked");
    assert_eq!(
        at(&upper, W / 2, H - 1),
        0,
        "bottom row clear for upper 7/8"
    );
    // Right-eighth ladder reaches the right edge.
    let right = cov('\u{1FB8B}'); // right 7/8
    assert!(at(&right, W - 1, H / 2) > 0, "right column inked");
    assert_eq!(at(&right, 0, H / 2), 0, "left column clear for right 7/8");
}

#[test]
fn vertical_and_horizontal_eighth_strips_are_thin() {
    // A vertical 1/8 strip inks a narrow band and spans the full height.
    let v = cov('\u{1FB73}'); // column 5
    let inked_cols: Vec<u32> = (0..W).filter(|&x| col_has_ink(&v, x)).collect();
    assert!(inked_cols.len() <= 2, "vertical strip should be ~1/8 wide");
    assert!(v.iter().any(|&p| p > 0));
    // A horizontal 1/8 strip spans a narrow band of rows.
    let hbuf = cov('\u{1FB78}'); // row 4
    let inked_rows: Vec<u32> = (0..H).filter(|&y| row_has_ink(&hbuf, y)).collect();
    assert!(
        inked_rows.len() <= 3,
        "horizontal strip should be ~1/8 tall"
    );
}

#[test]
fn half_shade_inks_one_half_at_partial_coverage() {
    let left = cov('\u{1FB8C}'); // LEFT HALF medium shade
    // Left half has mid coverage, right half is clear.
    assert!(
        0 < at(&left, 1, H / 2) && at(&left, 1, H / 2) < 255,
        "partial shade"
    );
    assert_eq!(at(&left, W - 1, H / 2), 0, "right half clear");
    let right = cov('\u{1FB8D}'); // RIGHT HALF medium shade
    assert_eq!(at(&right, 0, H / 2), 0, "left half clear");
    assert!(0 < at(&right, W - 1, H / 2) && at(&right, W - 1, H / 2) < 255);
}

// --- Group A: L-combo eighth blocks + segmented digits (Legacy Computing) ---

#[test]
fn lcombo_eighth_blocks_and_segmented_digits_cover_and_render() {
    // Every newly-closed Legacy Computing codepoint must be geometrically
    // covered and produce a non-empty coverage bitmap at a real cell size.
    for code in (0x1FB7Cu32..=0x1FB81).chain(0x1FBF0..=0x1FBF9) {
        let ch = char::from_u32(code).unwrap();
        assert!(covers(ch), "U+{code:04X} should be covered");
        let buf = coverage(ch, W, H).expect("covered codepoint renders");
        assert_eq!(buf.len(), (W * H) as usize);
        assert!(
            buf.iter().any(|&v| v > 0),
            "U+{code:04X} produced an empty (all-zero) coverage bitmap"
        );
    }
}

#[test]
fn lcombo_left_and_lower_inks_left_and_bottom_edges_only() {
    // U+1FB7C LEFT AND LOWER ONE EIGHTH BLOCK: the left column and bottom row
    // are inked; the top-right interior is clear.
    let buf = cov('\u{1FB7C}');
    assert!(col_has_ink(&buf, 0), "left edge inked");
    assert!(row_has_ink(&buf, H - 1), "bottom edge inked");
    // Top-right interior (well away from the left/bottom strips) is clear.
    assert_eq!(at(&buf, W - 1, 0), 0, "top-right corner clear");
}

#[test]
fn lcombo_right_and_upper_inks_right_and_top_edges_only() {
    // U+1FB7E RIGHT AND UPPER ONE EIGHTH BLOCK: the right column and top row
    // are inked; the bottom-left corner is clear.
    let buf = cov('\u{1FB7E}');
    assert!(col_has_ink(&buf, W - 1), "right edge inked");
    assert!(row_has_ink(&buf, 0), "top edge inked");
    assert_eq!(at(&buf, 0, H - 1), 0, "bottom-left corner clear");
}

#[test]
fn upper_and_lower_eighth_inks_both_horizontal_edges_not_middle() {
    // U+1FB80 UPPER AND LOWER ONE EIGHTH BLOCK: top and bottom rows inked,
    // the vertical middle is clear.
    let buf = cov('\u{1FB80}');
    assert!(row_has_ink(&buf, 0), "top edge inked");
    assert!(row_has_ink(&buf, H - 1), "bottom edge inked");
    assert!(!row_has_ink(&buf, H / 2), "middle row clear");
}

#[test]
fn horizontal_eighth_block_1358_inks_rows_one_three_five_eight() {
    // U+1FB81 HORIZONTAL ONE EIGHTH BLOCK-1358: 1/8-tall rows 1, 3, 5, 8 inked;
    // rows 2, 4, 6, 7 clear. Probe the vertical center of each 1/8 band.
    let buf = cov('\u{1FB81}');
    let band_mid = |row: u32| ((H as f32 * (row as f32 - 0.5) / 8.0) as u32).min(H - 1);
    for row in [1u32, 3, 5, 8] {
        assert!(
            row_has_ink(&buf, band_mid(row)),
            "row {row} should be inked"
        );
    }
    for row in [2u32, 4, 6, 7] {
        assert!(
            !row_has_ink(&buf, band_mid(row)),
            "row {row} should be clear"
        );
    }
}

#[test]
fn segmented_digit_one_inks_only_the_two_right_verticals() {
    // U+1FBF1 SEGMENTED DIGIT ONE = segments b + c (the right-hand verticals).
    // The digit is inset from the cell edges, so check the right *half* has
    // ink and the left third is entirely clear (no a/d/e/f/g segments).
    let buf = cov('\u{1FBF1}');
    assert!(
        (W / 2..W).any(|x| col_has_ink(&buf, x)),
        "right verticals inked"
    );
    assert!(
        !(0..W / 3).any(|x| col_has_ink(&buf, x)),
        "left third clear for digit 1"
    );
}

#[test]
fn segmented_digit_eight_inks_all_three_horizontal_bars() {
    // U+1FBF8 SEGMENTED DIGIT EIGHT lights every segment: the top, middle, and
    // bottom horizontal bars are all inked. The digit is inset, so probe within
    // thirds rather than the exact cell edges.
    let buf = cov('\u{1FBF8}');
    assert!((0..H / 3).any(|y| row_has_ink(&buf, y)), "top bar inked");
    assert!(row_has_ink(&buf, H / 2), "middle bar inked");
    assert!(
        (2 * H / 3..H).any(|y| row_has_ink(&buf, y)),
        "bottom bar inked"
    );
}

#[test]
fn segmented_digit_masks_are_distinct_per_digit() {
    // The 10 digits must map to 10 distinct segment masks (no two digits share
    // a seven-segment encoding).
    let masks: Vec<u8> = (0x1FBF0u32..=0x1FBF9)
        .map(|c| segmented_digit_table(char::from_u32(c).unwrap()).unwrap())
        .collect();
    for (i, a) in masks.iter().enumerate() {
        for b in masks.iter().skip(i + 1) {
            assert_ne!(a, b, "two segmented digits share a mask");
        }
    }
}

#[test]
fn eighth_strips_never_vanish_on_tiny_cells() {
    // On cells narrower/shorter than 8px, a 1/8 strip's two rounded bounds
    // can land on the same pixel; the >=1px floor keeps every strip visible,
    // matching the EighthEdges strips. Sweep all six vertical strip columns
    // and all six horizontal strip rows over degenerate cell sizes.
    for (w, h) in [(3u32, 5u32), (5, 7), (2, 2), (7, 3)] {
        for ch in '\u{1FB70}'..='\u{1FB75}' {
            let buf = coverage(ch, w, h).expect("covered");
            assert!(
                buf.iter().any(|&p| p > 0),
                "vertical strip U+{:04X} vanished at {w}x{h}",
                ch as u32
            );
        }
        for ch in '\u{1FB76}'..='\u{1FB7B}' {
            let buf = coverage(ch, w, h).expect("covered");
            assert!(
                buf.iter().any(|&p| p > 0),
                "horizontal strip U+{:04X} vanished at {w}x{h}",
                ch as u32
            );
        }
    }
}

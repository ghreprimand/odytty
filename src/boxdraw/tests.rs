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
    for code in 0xE0B0u32..=0xE0B3 {
        let ch = char::from_u32(code).unwrap();
        assert!(covers(ch), "powerline {code:#x} should be covered");
    }
}

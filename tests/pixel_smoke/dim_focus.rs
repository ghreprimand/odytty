//! SGR-dim attribute, perceptual-dim confinement, and ID2 focus dimming.

use odytty::core::CursorStyle;

use crate::harness::*;

#[test]
fn dim_attribute_lowers_cell_luminance() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Same glyph, bright vs dim foreground. Dim scales the fg, so the summed
    // luminance over the cell's ink must drop while the background is unchanged.
    let bright = composite(&row_snapshot(1, "\x1b[97mM"), &atlas, CursorStyle::Block);
    let dim = composite(&row_snapshot(1, "\x1b[2;97mM"), &atlas, CursorStyle::Block);

    let sum_lum = |f: &Frame| -> f32 {
        let (x0, y0, x1, y1) = f.cell_bounds(0, 0);
        let mut s = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                s += luminance(f.pixel(x, y));
            }
        }
        s
    };
    let bright_lum = sum_lum(&bright);
    let dim_lum = sum_lum(&dim);
    assert!(
        dim_lum < bright_lum,
        "dim cell luminance {dim_lum} should be below bright {bright_lum}"
    );
}

#[test]
fn perceptual_dim_delta_is_confined_to_dim_cells() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Two-cell row: col0 is a plain glyph, col1 carries the SGR-dim attribute.
    // The reference renders the same two glyphs with no dim anywhere.
    let plain = composite(&row_snapshot(2, "MM"), &atlas, CursorStyle::Block);
    let mixed = composite(&row_snapshot(2, "M\x1b[2mM"), &atlas, CursorStyle::Block);

    // Confinement: the non-dim cell (col0) must be byte-identical between the
    // two frames — the perceptual-dim change touches only cells with the dim
    // attribute, so the plain path stays pixel-identical.
    let (x0, y0, x1, y1) = plain.cell_bounds(0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            assert_eq!(
                plain.pixel(x, y),
                mixed.pixel(x, y),
                "non-dim cell pixel ({x},{y}) must be byte-identical"
            );
        }
    }

    // The dim cell (col1) must visibly change and lose luminance over its ink.
    let (dx0, dy0, dx1, dy1) = plain.cell_bounds(1, 0);
    let mut any_diff = false;
    let (mut plain_lum, mut dim_lum) = (0.0f32, 0.0f32);
    for y in dy0..dy1 {
        for x in dx0..dx1 {
            let (p, d) = (plain.pixel(x, y), mixed.pixel(x, y));
            any_diff |= differs(p, d);
            plain_lum += luminance(p);
            dim_lum += luminance(d);
        }
    }
    assert!(any_diff, "dim cell must differ from the plain render");
    assert!(
        dim_lum < plain_lum,
        "dim cell luminance {dim_lum} should drop below plain {plain_lum}"
    );
}

/// ID2 off-path gate: driving the focus-dim seam at `0.0` (the focused window,
/// and the knob default) is byte-identical to the standard focused render, so a
/// focused frame is pixel-identical to the pre-feature renderer.
#[test]
fn focus_dim_off_is_pixel_identical_to_focused() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(4, "Mix!");
    let focused = composite(&snapshot, &atlas, CursorStyle::Block);
    let off = composite_focus_dim(&snapshot, &atlas, CursorStyle::Block, 0.0);
    assert!(
        frames_match(&focused, &off),
        "focus_dim=0.0 must be byte-identical to the focused render"
    );
}

/// ID2 unfocused baseline: a non-zero focus-dim recedes the whole grid — both
/// the background fill and the glyph ink lose luminance — and the dim is applied
/// before the RV1 floor, so at a raised minimum contrast the text still clears
/// the floor against the dimmed background (legibility wins by construction).
#[test]
fn unfocused_focus_dim_recedes_grid_and_floor_keeps_text_legible() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(4, "Mix!");
    let focused = composite(&snapshot, &atlas, CursorStyle::Block);
    let unfocused = composite_focus_dim(&snapshot, &atlas, CursorStyle::Block, 0.3);

    // (a) The unfocused frame must differ from the focused one, and its total
    // luminance must drop — the whole window (text + background) recedes.
    let mut any_diff = false;
    let (mut focused_lum, mut unfocused_lum) = (0.0f32, 0.0f32);
    for y in 0..focused.height {
        for x in 0..focused.width {
            let (f, u) = (focused.pixel(x, y), unfocused.pixel(x, y));
            any_diff |= differs(f, u);
            focused_lum += luminance(f);
            unfocused_lum += luminance(u);
        }
    }
    assert!(any_diff, "unfocused frame must differ from focused");
    assert!(
        unfocused_lum < focused_lum,
        "unfocused total luminance {unfocused_lum} should drop below focused {focused_lum}"
    );

    // (b) The dim runs before the RV1 floor: dimming both fg and bg perceptually,
    // then enforcing a raised contrast ratio, must still clear that ratio against
    // the dimmed background. Exercised at the color layer (lock-free) so the
    // process-global min_contrast floor is not mutated under parallel tests.
    let amount = 0.3;
    let fg = [0.30f32, 0.30, 0.30]; // low-contrast light grey…
    let bg = [0.05f32, 0.05, 0.05]; // …on a near-black background.
    let dim_fg = odytty::color::dim_perceptual(fg, amount);
    let dim_bg = odytty::color::dim_perceptual(bg, amount);
    let ratio = 4.5; // WCAG AA body-text floor.
    let floored = odytty::color::enforce_min_contrast(dim_fg, dim_bg, ratio);
    let achieved = odytty::color::wcag_contrast(floored, dim_bg);
    assert!(
        achieved + 1e-3 >= ratio,
        "floored contrast {achieved} should clear the raised floor {ratio}"
    );
}

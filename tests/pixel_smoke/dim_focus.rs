// SPDX-License-Identifier: GPL-3.0-only
//! SGR-dim attribute, perceptual-dim confinement, and ID2 focus dimming.

use odytty::core::{Color, CursorStyle, RgbColor, Terminal};
use odytty::grid::{self, VERTS_PER_QUAD};
use odytty::text;

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

/// RV1 floor at the *cursor-block under-glyph* resolve site, proven in
/// composited pixels (not just vertex colors). When a visible block cursor sits
/// over a printable glyph, the under-cursor glyph is redrawn in the cell's
/// background color through `text::enforce_contrast_rgba(bg, block_color)` (the
/// second RV1 resolve site; the body site is the per-cell glyph). At the default
/// floor of 1.0 that is exact passthrough, so after compositing the cell is
/// dominated by the cursor block but the glyph's cut-out paints in the cell
/// background — i.e. the under-glyph stays legible against the block. This is the
/// only pixel-smoke case that composites a *visible* block cursor over real
/// glyph ink; the cursor-site seam is otherwise covered only at the grid vertex
/// layer. Global-free: the process floor stays at its 1.0 default throughout.
#[test]
fn block_cursor_floor_keeps_under_glyph_legible_in_composite() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // The suite runs entirely at the default (identity) floor; assert it so a
    // future global-mutating test added here would trip this guard rather than
    // silently perturb the passthrough claim.
    assert_eq!(text::min_contrast(), 1.0, "suite must run at the 1.0 floor");

    // 1x1 grid, a distinct (green) cursor color so the block fill is clearly
    // separable from both the default background and the under-glyph cut-out.
    let mut term = Terminal::new(1, 1);
    term.set_base_colors(
        RgbColor::new(0xCC, 0xCC, 0xCC),
        RgbColor::new(0x0B, 0x0C, 0x10),
        RgbColor::new(0x4C, 0xD9, 0x9F),
    );
    term.advance(b"M"); // pending-wrap keeps the cursor on the 'M'.
    let snapshot = term.snapshot();
    assert!(
        snapshot.cursor_visible,
        "cursor must be visible for this case"
    );

    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    // The block fill dominates the cell.
    let block_lin = [
        odytty::color::srgb_to_linear(0x4C),
        odytty::color::srgb_to_linear(0xD9),
        odytty::color::srgb_to_linear(0x9F),
    ];
    assert_eq!(
        cell_modal_color(&frame, 0, 0),
        quant3(block_lin),
        "block cursor fill should dominate the cell"
    );

    // The under-glyph cut-out paints in the cell background (the floor's
    // passthrough at the cursor site): at least one fully-covered glyph pixel
    // must read closer to the background than to the block fill.
    let bg = text::background_linear(Color::Default);
    let bg3 = [bg[0], bg[1], bg[2]];
    let dist =
        |a: [f32; 3], b: [f32; 3]| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs();
    let (x0, y0, x1, y1) = frame.cell_bounds(0, 0);
    let mut under_glyph_px = 0usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = frame.pixel(x, y);
            if dist(p, bg3) < dist(p, block_lin) {
                under_glyph_px += 1;
            }
        }
    }
    assert!(
        under_glyph_px > 0,
        "under-cursor glyph must paint background-colored ink (floor passthrough), \
         keeping it legible against the block"
    );

    // Sanity: the cursor really emits the block + under-glyph pair (4 quads:
    // cell bg, cell glyph, cursor block, cursor under-glyph), and the under-glyph
    // vertex color is the background passthrough, not the block color.
    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, CursorStyle::Block);
    assert_eq!(
        verts.len(),
        4 * VERTS_PER_QUAD,
        "expected the block+glyph pair"
    );
    let under_glyph = verts[3 * VERTS_PER_QUAD];
    assert_eq!(under_glyph.is_glyph, 1.0);
    assert_eq!(
        under_glyph.color, bg,
        "cursor under-glyph must be the background passthrough at the 1.0 floor"
    );
}

/// ID2 × RV1 precondition, in live pixels: an unfocused window dims **both** the
/// foreground ink and the background fill, and the dim is *perceptual* (OKLab),
/// so a colored background recedes in luminance while keeping its hue — it does
/// not wash toward gray. This is the load-bearing precondition behind the RV1
/// re-lift claim: the floor only keeps text legible *because* it runs against a
/// genuinely dimmed background, so proving the background actually dims (and
/// stays chromatic) anchors that argument in pixels. Global-free: focus-dim is a
/// per-call parameter, never the process floor.
#[test]
fn focus_dim_dims_background_fill_preserving_hue() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A saturated blue background so hue drift is measurable.
    let snapshot = styled_row_snapshot(1, b"\x1b[48;2;40;70;160m", "X");
    let focused = composite(&snapshot, &atlas, CursorStyle::Block);
    let unfocused = composite_focus_dim(&snapshot, &atlas, CursorStyle::Block, 0.3);

    // Modal color is the background fill (glyph ink is a minority of pixels).
    let to_lin = |c: [u8; 3]| {
        [
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
        ]
    };
    let focused_bg = to_lin(cell_modal_color(&focused, 0, 0));
    let unfocused_bg = to_lin(cell_modal_color(&unfocused, 0, 0));

    // (a) The background recedes in luminance.
    let fl = luminance(focused_bg);
    let ul = luminance(unfocused_bg);
    assert!(
        ul < fl,
        "unfocused background luminance {ul} should drop below focused {fl}"
    );

    // (b) The dim is perceptual: hue is preserved and the background stays
    // chromatic (chroma reduced, not collapsed to gray).
    let f_lch = odytty::color::oklab_to_oklch(odytty::color::linear_to_oklab(focused_bg));
    let u_lch = odytty::color::oklab_to_oklch(odytty::color::linear_to_oklab(unfocused_bg));
    let mut dh = (f_lch.h - u_lch.h).abs();
    if dh > std::f32::consts::PI {
        dh = std::f32::consts::TAU - dh; // shortest angular distance
    }
    // ~3° tolerance absorbs u8 quantization of the darker dimmed fill; a true
    // hue skew (or a gray-wash, which makes hue arbitrary) would be far larger,
    // and the chroma assertion below independently rules out the gray-wash case.
    assert!(
        dh < 0.06,
        "background hue should be preserved; drift {dh} rad"
    );
    assert!(
        u_lch.c < f_lch.c && u_lch.c > 0.4 * f_lch.c,
        "background chroma should reduce but stay chromatic: {} -> {}",
        f_lch.c,
        u_lch.c
    );
    assert!(
        u_lch.l < f_lch.l,
        "background lightness should drop: {} -> {}",
        f_lch.l,
        u_lch.l
    );
}

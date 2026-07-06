// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::core::{Color, Terminal};
use crate::text::{FontStyle, load_font};

fn atlas() -> Option<GlyphAtlas> {
    let font = load_font().ok()?;
    Some(GlyphAtlas::build(&font, 24.0))
}

fn quad_rect(verts: &[Vertex], quad_index: usize) -> [f32; 4] {
    let start = quad_index * VERTS_PER_QUAD;
    let quad = &verts[start..start + VERTS_PER_QUAD];
    let x0 = quad
        .iter()
        .map(|vertex| vertex.pos[0])
        .fold(f32::INFINITY, f32::min);
    let y0 = quad
        .iter()
        .map(|vertex| vertex.pos[1])
        .fold(f32::INFINITY, f32::min);
    let x1 = quad
        .iter()
        .map(|vertex| vertex.pos[0])
        .fold(f32::NEG_INFINITY, f32::max);
    let y1 = quad
        .iter()
        .map(|vertex| vertex.pos[1])
        .fold(f32::NEG_INFINITY, f32::max);
    [x0, y0, x1, y1]
}

#[test]
fn known_grid_vertex_count() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 5x1 grid with "Hi" then three blanks: 5 background quads, plus glyph
    // quads only for the two inked, printable characters. Cursor hidden so
    // this asserts cell geometry alone.
    let mut term = Terminal::new(5, 1);
    term.advance(b"\x1b[?25lHi");
    let snapshot = term.snapshot();
    let verts = build_vertices(&snapshot, &atlas);
    let expected = (5 + 2) * VERTS_PER_QUAD;
    assert_eq!(verts.len(), expected);
}

#[test]
fn blank_cells_emit_background_only() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A fresh terminal is all spaces: every cell is background-only.
    // Cursor hidden so the count is pure cell geometry.
    let mut term = Terminal::new(3, 2);
    term.advance(b"\x1b[?25l");
    let snapshot = term.snapshot();
    let verts = build_vertices(&snapshot, &atlas);
    assert_eq!(verts.len(), 3 * 2 * VERTS_PER_QUAD);
    assert!(verts.iter().all(|v| v.is_glyph == 0.0));
}

#[test]
fn inverse_swaps_foreground_and_background() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Default colors, no inverse: background quad uses the default bg.
    let mut plain = Terminal::new(1, 1);
    plain.advance(b"X");
    let plain_bg = build_vertices(&plain.snapshot(), &atlas)[0].color;

    // Inverse on: the background quad should now carry the default fg color.
    let mut inv = Terminal::new(1, 1);
    inv.advance(b"\x1b[7mX\x1b[0m");
    let inv_verts = build_vertices(&inv.snapshot(), &atlas);
    let inv_bg = inv_verts[0].color; // first quad is the background
    let inv_glyph = inv_verts[VERTS_PER_QUAD].color; // second quad is the glyph

    assert_eq!(inv_bg, text::foreground_linear(Color::Default));
    assert_eq!(inv_glyph, text::background_linear(Color::Default));
    assert_ne!(inv_bg, plain_bg);
}

#[test]
fn dynamic_colors_override_rendered_defaults_and_palette() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(2, 1);
    term.advance(b"\x1b[?25l\x1b]11;rgb:ffff/0000/0000\x1b\\ ");
    let verts = build_vertices(&term.snapshot(), &atlas);
    assert_eq!(verts[0].color, rgb_linear(RgbColor::new(255, 0, 0)));

    let mut term = Terminal::new(2, 1);
    term.advance(b"\x1b[?25l\x1b]4;1;rgb:0000/ffff/0000\x1b\\\x1b[41m ");
    let verts = build_vertices(&term.snapshot(), &atlas);
    assert_eq!(verts[0].color, rgb_linear(RgbColor::new(0, 255, 0)));
}

#[test]
fn unsupported_printable_emits_fallback_glyph_quad() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'é' is printable but outside the atlas's pre-rasterized ASCII block:
    // the renderer now draws the missing-glyph fallback box rather than
    // leaving the cell blank. Cursor hidden so only cell + glyph are counted.
    let mut term = Terminal::new(1, 1);
    term.advance("\x1b[?25l".as_bytes());
    term.advance("é".as_bytes());
    let verts = build_vertices(&term.snapshot(), &atlas);
    // One background quad plus one fallback-box glyph quad.
    assert_eq!(verts.len(), 2 * VERTS_PER_QUAD);
    let glyph = &verts[VERTS_PER_QUAD];
    assert_eq!(glyph.is_glyph, 1.0);
    // The glyph quad uses the shared fallback UV — identical for any other
    // unsupported printable codepoint.
    let fallback_uv = atlas.uv_rect('é').expect("fallback uv");
    assert_eq!(glyph.uv, [fallback_uv[0], fallback_uv[1]]);
    assert_eq!(atlas.uv_rect('é'), atlas.uv_rect('★'));
}

#[test]
fn wide_continuation_spacer_emits_nothing() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A wide character occupies a lead cell + a continuation spacer. The
    // spacer must contribute no quad (no double-draw); the lead emits one
    // background (spanning two columns) and one fallback-box glyph quad.
    let mut term = Terminal::new(4, 1);
    term.advance("\x1b[?25l".as_bytes());
    term.advance("世".as_bytes());
    let snapshot = term.snapshot();
    // Confirm the second column really is a continuation spacer.
    assert!(snapshot.cells[1].wide_continuation);
    let verts = build_vertices(&snapshot, &atlas);
    // lead: bg + fallback glyph; spacer: nothing; two blanks: bg each = 4 quads.
    assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);
}

#[test]
fn cursor_visible_emits_one_block_quad_on_blank_cell() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Fresh 3x2 terminal: cursor visible at (0,0) on a blank cell. The
    // cursor adds exactly one background (block) quad over the cell grid.
    let visible = Terminal::new(3, 2);
    let with_cursor = build_vertices(&visible.snapshot(), &atlas);

    let mut hidden = Terminal::new(3, 2);
    hidden.advance(b"\x1b[?25l");
    let without_cursor = build_vertices(&hidden.snapshot(), &atlas);

    assert_eq!(with_cursor.len() - without_cursor.len(), VERTS_PER_QUAD);
}

#[test]
fn hidden_cursor_emits_no_cursor_quad() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    let verts = build_vertices(&term.snapshot(), &atlas);
    // Four blank cells, cursor hidden: only the four cell backgrounds.
    assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);
}

#[test]
fn cursor_quad_sits_at_cursor_cell() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;

    // Move the cursor to row 1, column 3 (1-based CUP -> 0-based 1,3).
    let mut term = Terminal::new(5, 3);
    term.advance(b"\x1b[2;4H");
    let snapshot = term.snapshot();
    assert_eq!(snapshot.cursor, crate::core::Position { row: 1, column: 3 });

    let verts = build_vertices(&snapshot, &atlas);
    // The cursor cell is blank, so the cursor is the final background quad.
    let cursor_tl = verts[verts.len() - VERTS_PER_QUAD].pos;
    assert_eq!(cursor_tl, [3.0 * cell_w, 1.0 * cell_h]);
}

#[test]
fn cursor_position_is_clamped_to_grid_bounds() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;

    // Hand-build a snapshot whose cursor points past the last cell.
    let dimensions = crate::core::Dimensions::new(2, 2);
    let cells = vec![crate::core::Cell::blank(); 4];
    let snapshot = Snapshot {
        dimensions,
        cursor: crate::core::Position {
            row: 99,
            column: 99,
        },
        cursor_visible: true,
        colors: crate::core::DynamicColors::default(),
        cells,
    };

    // Must not panic, and the clamped cursor lands on the last cell (1,1).
    let verts = build_vertices(&snapshot, &atlas);
    let cursor_tl = verts[verts.len() - VERTS_PER_QUAD].pos;
    assert_eq!(cursor_tl, [1.0 * cell_w, 1.0 * cell_h]);
}

#[test]
fn cursor_over_glyph_redraws_it_inverted() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 1x1 terminal with 'R': pending-wrap keeps the cursor on the 'R'.
    let mut term = Terminal::new(1, 1);
    term.advance(b"R");
    let snapshot = term.snapshot();
    assert_eq!(snapshot.cursor, crate::core::Position { row: 0, column: 0 });

    let verts = build_vertices(&snapshot, &atlas);
    // Cell bg, cell glyph, cursor block, cursor glyph = 4 quads.
    assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);

    let cursor_block = verts[2 * VERTS_PER_QUAD];
    let cursor_glyph = verts[3 * VERTS_PER_QUAD];
    // Block carries the cell's foreground; the redrawn glyph the background.
    assert_eq!(cursor_block.is_glyph, 0.0);
    assert_eq!(cursor_block.color, text::foreground_linear(Color::Default));
    assert_eq!(cursor_glyph.is_glyph, 1.0);
    assert_eq!(cursor_glyph.color, text::background_linear(Color::Default));
}

#[test]
fn colored_row_uses_ansi_palette() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A red 'R' glyph quad should carry the indexed-1 (red) foreground.
    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[31mR\x1b[0m");
    let verts = build_vertices(&term.snapshot(), &atlas);
    let glyph = verts[VERTS_PER_QUAD].color;
    assert_eq!(glyph, text::foreground_linear(Color::Indexed(1)));
}

#[test]
fn attrs_select_expected_font_style() {
    assert_eq!(font_style_for_attrs(&Attrs::default()), FontStyle::Regular);

    let mut bold = Attrs::default();
    bold.set_bold(true);
    assert_eq!(font_style_for_attrs(&bold), FontStyle::Bold);

    let mut italic = Attrs::default();
    italic.set_italic(true);
    assert_eq!(font_style_for_attrs(&italic), FontStyle::Italic);

    let mut bold_italic = Attrs::default();
    bold_italic.set_bold(true);
    bold_italic.set_italic(true);
    assert_eq!(font_style_for_attrs(&bold_italic), FontStyle::BoldItalic);
}

#[test]
fn styled_glyph_uses_styled_uv_rect() {
    let Ok(font) = load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut atlas = GlyphAtlas::build(&font, 24.0);
    atlas
        .ensure_styled(&font, FontStyle::Bold, 'B')
        .expect("bold glyph uv");
    let expected = atlas
        .glyph_quad_styled(FontStyle::Bold, 'B')
        .expect("bold glyph quad");

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[1mB");
    let verts = build_vertices(&term.snapshot(), &atlas);

    let glyph = verts[VERTS_PER_QUAD];
    assert_eq!(glyph.is_glyph, 1.0);
    assert_eq!(glyph.uv, [expected.uv[0], expected.uv[1]]);
}

/// Backgrounds are emitted in a separate pass before any glyph, so a
/// later cell's background can never paint over an earlier cell's overflow
/// ink. With cursor hidden, a 2-cell row of inked glyphs yields both
/// background quads first, then both glyph quads.
#[test]
fn backgrounds_are_batched_before_glyphs() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let mut term = Terminal::new(2, 1);
    term.advance(b"\x1b[?25lHi");
    let verts = build_vertices(&term.snapshot(), &atlas);
    // Two backgrounds, then two glyphs.
    assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);
    assert_eq!(verts[0].is_glyph, 0.0, "cell 0 background first");
    assert_eq!(
        verts[VERTS_PER_QUAD].is_glyph, 0.0,
        "cell 1 background next"
    );
    assert_eq!(
        verts[2 * VERTS_PER_QUAD].is_glyph,
        1.0,
        "glyphs only after all backgrounds"
    );
    assert_eq!(verts[3 * VERTS_PER_QUAD].is_glyph, 1.0);
}

/// A glyph quad is positioned and sized from the atlas's bearing-aware
/// bounds (offset from the cell origin + ink size), not the fixed cell rect.
#[test]
fn glyph_quad_uses_bearing_aware_bounds() {
    let Ok(font) = load_font() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let atlas = GlyphAtlas::build(&font, 28.0);
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    let bounds = atlas.glyph_quad('g').expect("g bounds");

    // Place 'g' at column 2, row 1 so the cell origin is non-zero.
    let mut term = Terminal::new(4, 2);
    term.advance(b"\x1b[?25l\x1b[2;3Hg");
    let snapshot = term.snapshot();
    let verts = build_vertices(&snapshot, &atlas);

    // Find the single glyph quad (is_glyph == 1.0); its top-left vertex must
    // sit at cell_origin + bounds.offset, with the bounds' UV.
    let glyph_tl = verts
        .iter()
        .find(|v| v.is_glyph == 1.0)
        .expect("one glyph quad");
    let x0 = 2.0 * cell_w;
    let y0 = 1.0 * cell_h;
    assert_eq!(
        glyph_tl.pos,
        [x0 + bounds.offset_x as f32, y0 + bounds.offset_y as f32]
    );
    assert_eq!(glyph_tl.uv, [bounds.uv[0], bounds.uv[1]]);
}

#[test]
fn underline_attribute_appends_thin_solid_quad() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[4mU");
    let verts = build_vertices(&term.snapshot(), &atlas);

    assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
    let line = verts[2 * VERTS_PER_QUAD];
    let expected = underline_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    assert_eq!(line.is_glyph, 0.0);
    assert_eq!(line.pos, [expected[0], expected[1]]);
    assert_eq!(
        line.color,
        text::foreground_linear(Color::Default),
        "underline uses the effective foreground"
    );
}

#[test]
fn underline_color_uses_sgr_58_when_set() {
    // Asserts exact passthrough color; serialize against the floor mutators.
    let _guard = crate::test_lock::render_globals_lock();
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[58;5;2;4mU");
    let snapshot = term.snapshot();
    let verts = build_vertices(&snapshot, &atlas);

    assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
    let line = verts[2 * VERTS_PER_QUAD];
    assert_eq!(
        line.color,
        foreground_linear(&snapshot.colors, Color::Indexed(2))
    );
}

/// U1 color-type coverage at the default floor: a *truecolor* SGR-58 underline
/// color is a byte-identical passthrough of its raw resolved color, matching the
/// indexed case above. Non-mutating — it asserts the new enforce call is a no-op
/// at min_contrast = 1.0 without touching the process-global floor (so it cannot
/// interleave with the single owned global-mutator test).
#[test]
fn underline_color_truecolor_passthrough_at_default_floor() {
    // Reads the process-global floor at its 1.0 baseline; serialize against the
    // floor mutators so a concurrent set cannot be observed mid-window.
    let _guard = crate::test_lock::render_globals_lock();
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    assert_eq!(text::min_contrast(), 1.0);

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[58;2;180;90;30;4mU");
    let snapshot = term.snapshot();
    let verts = build_vertices(&snapshot, &atlas);

    assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
    let line = verts[2 * VERTS_PER_QUAD];
    assert_eq!(
        line.color,
        foreground_linear(&snapshot.colors, Color::Rgb(180, 90, 30)),
        "default floor: truecolor underline color must be byte-identical passthrough"
    );
}

#[test]
fn double_underline_appends_two_solid_quads() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[4:2mD");
    let verts = build_vertices(&term.snapshot(), &atlas);

    assert_eq!(verts.len(), 4 * VERTS_PER_QUAD);
    let upper = quad_rect(&verts, 2);
    let lower = quad_rect(&verts, 3);
    let expected_lower = underline_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    assert_eq!(lower, expected_lower);
    assert!(upper[1] < lower[1]);
    assert_eq!(upper[3] - upper[1], lower[3] - lower[1]);
}

#[test]
fn dotted_underline_emits_gapped_dot_quads() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[4:4mO");
    let verts = build_vertices(&term.snapshot(), &atlas);
    let decoration_quads = verts.len() / VERTS_PER_QUAD - 2;

    assert!(decoration_quads >= 2);
    let first = quad_rect(&verts, 2);
    let second = quad_rect(&verts, 3);
    assert!(second[0] > first[2], "dots are separated by a gap");
    assert_eq!(first[3] - first[1], first[2] - first[0]);
}

#[test]
fn dashed_underline_emits_segmented_quads() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(2, 1);
    term.advance("\x1b[?25l\x1b[4:5m表".as_bytes());
    let verts = build_vertices(&term.snapshot(), &atlas);
    let first_dash = quad_rect(&verts, 2);

    assert!(verts.len() > 3 * VERTS_PER_QUAD);
    assert!(first_dash[2] - first_dash[0] < atlas.cell.width as f32 * 2.0);
}

#[test]
fn curly_underline_emits_stepped_quads() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(2, 1);
    term.advance("\x1b[?25l\x1b[4:3m表".as_bytes());
    let verts = build_vertices(&term.snapshot(), &atlas);
    let decoration_quads = verts.len() / VERTS_PER_QUAD - 2;

    assert!(decoration_quads >= 2);
    let first = quad_rect(&verts, 2);
    let second = quad_rect(&verts, 3);
    assert_ne!(first[1], second[1], "curly style alternates y positions");
}

/// Pins the [`DIM_PERCEPTUAL_AMOUNT`] choice: dimming must (a) preserve hue
/// in OKLab and (b) reproduce the perceived brightness of the historical
/// linear ×0.5 halving (matching OKLab lightness within tolerance). A future
/// edit to the constant or the dim model that breaks parity trips here.
#[test]
fn perceptual_dim_matches_old_halving_brightness_and_preserves_hue() {
    // A saturated source so a hue skew would be visible.
    let fg = text::foreground_linear(Color::Rgb(200, 60, 30));
    let rgb = [fg[0], fg[1], fg[2]];
    let dimmed = crate::color::dim_perceptual(rgb, DIM_PERCEPTUAL_AMOUNT);

    let old_halved = [rgb[0] * 0.5, rgb[1] * 0.5, rgb[2] * 0.5];
    let lab_dim = crate::color::linear_to_oklab(dimmed);
    let lab_old = crate::color::linear_to_oklab(old_halved);
    let lab_src = crate::color::linear_to_oklab(rgb);

    // (a) Brightness parity with the old linear halving.
    assert!(
        (lab_dim.l - lab_old.l).abs() < 1e-3,
        "perceptual dim L {} should match old-halving L {}",
        lab_dim.l,
        lab_old.l
    );
    // (b) Hue preserved: the (a, b) chroma vector keeps its direction
    // (scaled by the same factor as L), so atan2(b, a) is unchanged.
    let hue = |lab: crate::color::Oklab| lab.b.atan2(lab.a);
    assert!(
        (hue(lab_dim) - hue(lab_src)).abs() < 1e-4,
        "perceptual dim must preserve OKLab hue"
    );
}

#[test]
fn dim_attribute_scales_effective_foreground() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[2;31mD");
    let verts = build_vertices(&term.snapshot(), &atlas);

    assert_eq!(
        verts[VERTS_PER_QUAD].color,
        dim_color(text::foreground_linear(Color::Indexed(1)))
    );
}

/// RV1 activation across **both** resolve sites and after the dims.
///
/// The per-cell resolve seam passes the foreground through
/// `text::enforce_contrast_rgba`, so a raised minimum-contrast floor actually
/// lifts low-contrast glyph color at the render path (not just in the text.rs
/// unit). This test deliberately owns the only grid-side mutation of the
/// process-global floor — it raises to AAA once, exercises all three cases in
/// that single window, then restores `1.0` before any assertion can unwind —
/// so the suite gains no second unlocked global mutator (which could
/// interleave and flake). The three cases:
/// 1. **Body site** (per-cell glyph): the canonical low-contrast lift.
/// 2. **Cursor-block under-glyph site**: the second floor application
///    (`enforce_contrast_rgba(bg, block)`), proving the floor is live there
///    too, so the two sites agree on honoring the floor.
/// 3. **Combined dim + focus + floor**: a dim cell rendered unfocused, whose
///    contrast the two dims have already eroded below the floor — the floor
///    still lifts it, confirming it runs last and wins by construction.
#[test]
fn min_contrast_floor_lifts_at_both_resolve_sites_and_after_dims() {
    // Mutates the process-global floor; serialize against every other floor test.
    let _guard = crate::test_lock::render_globals_lock();
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    // --- Case 1 inputs: a near-black glyph on a black background (~1.0). ---
    let mut body = Terminal::new(1, 1);
    body.advance(b"\x1b[?25l\x1b[38;2;20;20;20;48;2;0;0;0mX");
    let body = body.snapshot();

    // --- Case 2 inputs: a visible block cursor over a glyph, with a cursor
    // color close to the background so the under-glyph (bg) vs block contrast
    // starts low. No `?25l`, so pending-wrap keeps the cursor on the glyph. ---
    let mut cur = Terminal::new(1, 1);
    cur.set_base_colors(
        crate::core::RgbColor::new(0xCC, 0xCC, 0xCC),
        crate::core::RgbColor::new(0x0B, 0x0C, 0x10),
        crate::core::RgbColor::new(0x22, 0x24, 0x2C),
    );
    cur.advance(b"R");
    let cur = cur.snapshot();
    let block_color = rgb_linear(cur.colors.cursor);

    // --- Case 3 inputs: a dim grey glyph on a darker grey, rendered with a
    // non-zero focus dim. After SGR-dim + focus-dim, fg/bg contrast is low. ---
    let mut combo = Terminal::new(1, 1);
    combo.advance(b"\x1b[?25l\x1b[2;38;2;90;90;90;48;2;30;30;30mX");
    let combo = combo.snapshot();
    let focus = 0.3_f32;
    let build_combo = |out: &mut Vec<Vertex>| {
        build_cell_vertices_with_focus_dim_into(
            out,
            &combo,
            &atlas,
            &[],
            focus,
            BackgroundTreatmentParams::default(),
        );
    };

    // --- Case 4 inputs (U1): an explicit SGR-58 underline color (a dark, but
    // chromatic, purple) on a black background, so the underline ink starts
    // below the floor and the lift can be checked for hue preservation. The
    // underline quad is index 2 (bg, glyph, underline) for this single cell. ---
    let mut uline = Terminal::new(1, 1);
    uline.advance(b"\x1b[?25l\x1b[58;2;40;12;120;4;48;2;0;0;0mU");
    let uline = uline.snapshot();
    let uline_raw = foreground_linear(&uline.colors, Color::Rgb(40, 12, 120));

    // --- Case 5 inputs (U1): an explicit *256-color* (Indexed) underline color
    // below the floor on black, proving the lift is color-type-agnostic — the
    // same single enforce path covers indexed and truecolor identically (the
    // headline U1 finding). Index 18 is a dark, chromatic blue (~1.35 contrast
    // on black), comfortably below the AAA floor. ---
    let mut uidx = Terminal::new(1, 1);
    uidx.advance(b"\x1b[?25l\x1b[58;5;18;4;48;2;0;0;0mU");
    let uidx = uidx.snapshot();
    let uidx_raw = foreground_linear(&uidx.colors, Color::Indexed(18));

    // === Baseline at the default passthrough floor (1.0). ===
    assert_eq!(text::min_contrast(), 1.0);
    let body_unfloored = build_vertices(&body, &atlas)[VERTS_PER_QUAD].color;
    assert_eq!(
        body_unfloored,
        foreground_linear(&body.colors, Color::Rgb(20, 20, 20)),
        "default floor must be byte-identical passthrough"
    );
    let mut cur_base = Vec::new();
    build_vertices_with_cursor_into(&mut cur_base, &cur, &atlas, CursorStyle::Block);
    // 4 quads: cell bg, cell glyph, cursor block, cursor under-glyph.
    assert_eq!(cur_base.len(), 4 * VERTS_PER_QUAD);
    let cur_unfloored = cur_base[3 * VERTS_PER_QUAD].color;
    let mut combo_base = Vec::new();
    build_combo(&mut combo_base);
    let combo_unfloored = combo_base[VERTS_PER_QUAD].color;
    let combo_bg = combo_base[0].color;
    // Case 4 baseline: at the default floor the explicit underline color is an
    // exact passthrough of its raw resolved color (the new U1 enforce call is a
    // verifiable no-op at min_contrast = 1.0).
    let uline_unfloored = build_vertices(&uline, &atlas)[2 * VERTS_PER_QUAD].color;
    assert_eq!(
        uline_unfloored, uline_raw,
        "default floor: explicit underline color must be byte-identical passthrough"
    );
    // Case 5 baseline: same passthrough guarantee for the 256-color underline.
    let uidx_unfloored = build_vertices(&uidx, &atlas)[2 * VERTS_PER_QUAD].color;
    assert_eq!(
        uidx_unfloored, uidx_raw,
        "default floor: 256-color underline color must be byte-identical passthrough"
    );
    // Precondition: the doubly-dimmed pair really is below the AAA floor, so
    // case 3 proves the floor — not the inputs — does the lifting.
    let combo_base_contrast = crate::color::wcag_contrast(
        [combo_unfloored[0], combo_unfloored[1], combo_unfloored[2]],
        [combo_bg[0], combo_bg[1], combo_bg[2]],
    );
    assert!(
        combo_base_contrast < 7.0,
        "combined precondition: dimmed contrast should start below the floor: {combo_base_contrast}"
    );

    // === Raise to AAA (7.0), rebuild all three, then restore. ===
    text::set_min_contrast(7.0);
    let body_floored = build_vertices(&body, &atlas)[VERTS_PER_QUAD].color;
    let mut cur_hi = Vec::new();
    build_vertices_with_cursor_into(&mut cur_hi, &cur, &atlas, CursorStyle::Block);
    let cur_floored = cur_hi[3 * VERTS_PER_QUAD].color;
    let mut combo_hi = Vec::new();
    build_combo(&mut combo_hi);
    let combo_floored = combo_hi[VERTS_PER_QUAD].color;
    let combo_hi_bg = combo_hi[0].color;
    let uline_floored = build_vertices(&uline, &atlas)[2 * VERTS_PER_QUAD].color;
    let uidx_floored = build_vertices(&uidx, &atlas)[2 * VERTS_PER_QUAD].color;
    text::set_min_contrast(1.0); // restore before any assertion can unwind

    // --- Case 1: body site lifted and meets the floor. ---
    let body_bg = background_linear(&body.colors, Color::Rgb(0, 0, 0));
    assert_ne!(
        body_floored, body_unfloored,
        "body: raised floor must change fg"
    );
    let body_ratio = crate::color::wcag_contrast(
        [body_floored[0], body_floored[1], body_floored[2]],
        [body_bg[0], body_bg[1], body_bg[2]],
    );
    assert!(body_ratio >= 7.0 - 1e-3, "body floor not met: {body_ratio}");

    // --- Case 2: cursor under-glyph site lifted and meets the floor against
    // the block color (the second resolve site honors the same floor). ---
    assert_ne!(
        cur_floored, cur_unfloored,
        "cursor under-glyph: raised floor must change the under-glyph color"
    );
    let cur_ratio = crate::color::wcag_contrast(
        [cur_floored[0], cur_floored[1], cur_floored[2]],
        [block_color[0], block_color[1], block_color[2]],
    );
    assert!(
        cur_ratio >= 7.0 - 1e-3,
        "cursor-site floor not met: {cur_ratio}"
    );

    // --- Case 3: combined dim + focus + floor. bg is unchanged by the floor
    // (only fg is lifted), and the lifted fg clears the floor against it. ---
    assert_eq!(combo_hi_bg, combo_bg, "floor must not alter the background");
    assert_ne!(
        combo_floored, combo_unfloored,
        "combined: floor must lift fg"
    );
    let combo_ratio = crate::color::wcag_contrast(
        [combo_floored[0], combo_floored[1], combo_floored[2]],
        [combo_bg[0], combo_bg[1], combo_bg[2]],
    );
    assert!(
        combo_ratio >= 7.0 - 1e-3,
        "combined floor not met after both dims: {combo_ratio}"
    );

    // --- Case 4 (U1): the explicit SGR-58 underline color is floored on the
    // same path as every other foreground ink — lifted to clear the ratio, with
    // hue preserved (enforce_min_contrast moves only OKLab L, holding a/b). ---
    let uline_bg = background_linear(&uline.colors, Color::Rgb(0, 0, 0));
    assert_ne!(
        uline_floored, uline_unfloored,
        "underline color: raised floor must lift the explicit SGR-58 color"
    );
    let uline_ratio = crate::color::wcag_contrast(
        [uline_floored[0], uline_floored[1], uline_floored[2]],
        [uline_bg[0], uline_bg[1], uline_bg[2]],
    );
    assert!(
        uline_ratio >= 7.0 - 1e-3,
        "underline-color floor not met: {uline_ratio}"
    );
    // Hue AND chroma preserved within eps: enforce_min_contrast moves only OKLab
    // L, holding a/b — so both the OKLCH hue and chroma (sqrt(a²+b²)) are carried
    // through the lift. Asserting chroma too (not just hue) pins the "lightness
    // only" guarantee the reviewer expects, distinguishing it from a desaturate.
    let oklch = |c: [f32; 4]| {
        crate::color::oklab_to_oklch(crate::color::linear_to_oklab([c[0], c[1], c[2]]))
    };
    let hue_drift = |a: [f32; 4], b: [f32; 4]| {
        let d = (oklch(a).h - oklch(b).h).abs();
        d.min((std::f32::consts::TAU - d).abs())
    };
    let dh = hue_drift(uline_floored, uline_raw);
    assert!(
        dh < 0.02,
        "underline-color hue drifted under the floor: {dh}"
    );
    let dc = (oklch(uline_floored).c - oklch(uline_raw).c).abs();
    assert!(
        dc < 0.02,
        "underline-color chroma drifted under the floor (not lightness-only): {dc}"
    );
    // Idempotent at the color layer: re-flooring the lifted color is a no-op.
    let refloored = crate::color::enforce_min_contrast(
        [uline_floored[0], uline_floored[1], uline_floored[2]],
        [uline_bg[0], uline_bg[1], uline_bg[2]],
        7.0,
    );
    assert_eq!(
        refloored,
        [uline_floored[0], uline_floored[1], uline_floored[2]],
        "underline-color floor must be idempotent"
    );

    // --- Case 5 (U1): the 256-color underline color is lifted on the identical
    // path — same color-type-agnostic enforce call — clearing the ratio with hue
    // preserved, so indexed and truecolor underline colors behave the same. ---
    let uidx_bg = background_linear(&uidx.colors, Color::Rgb(0, 0, 0));
    assert_ne!(
        uidx_floored, uidx_unfloored,
        "256-color underline: raised floor must lift the indexed color"
    );
    let uidx_ratio = crate::color::wcag_contrast(
        [uidx_floored[0], uidx_floored[1], uidx_floored[2]],
        [uidx_bg[0], uidx_bg[1], uidx_bg[2]],
    );
    assert!(
        uidx_ratio >= 7.0 - 1e-3,
        "256-color underline floor not met: {uidx_ratio}"
    );
    let dh_idx = hue_drift(uidx_floored, uidx_raw);
    assert!(
        dh_idx < 0.02,
        "256-color underline hue drifted under the floor: {dh_idx}"
    );

    // The restore took effect: passthrough again.
    assert_eq!(text::min_contrast(), 1.0);
}

#[test]
fn hidden_attribute_suppresses_glyph_quad() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[8mH");
    let verts = build_vertices(&term.snapshot(), &atlas);

    assert_eq!(verts.len(), VERTS_PER_QUAD);
    assert!(verts.iter().all(|v| v.is_glyph == 0.0));
}

#[test]
fn strikethrough_attribute_appends_thin_solid_quad() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };

    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[9mS");
    let verts = build_vertices(&term.snapshot(), &atlas);

    assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
    let line = verts[2 * VERTS_PER_QUAD];
    let expected = strikethrough_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    assert_eq!(line.is_glyph, 0.0);
    assert_eq!(line.pos, [expected[0], expected[1]]);
    assert_eq!(line.color, text::foreground_linear(Color::Default));
}

#[test]
fn block_cursor_matches_default_build() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Explicit Block cursor is byte-identical to the default build path.
    let term = Terminal::new(3, 2);
    let snapshot = term.snapshot();
    let mut default_path = Vec::new();
    build_vertices_into(&mut default_path, &snapshot, &atlas);
    let mut block_path = Vec::new();
    build_vertices_with_cursor_into(&mut block_path, &snapshot, &atlas, CursorStyle::Block);
    assert_eq!(default_path, block_path);
}

#[test]
fn underline_cursor_emits_single_bottom_bar() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    // Blank cell, visible cursor at (0,0). Underline cursor = one solid quad
    // pinned to the bottom edge, no inverse block, no glyph redraw.
    let term = Terminal::new(2, 1);
    let mut verts = Vec::new();
    build_vertices_with_cursor_into(&mut verts, &term.snapshot(), &atlas, CursorStyle::Underline);
    // Two blank-cell backgrounds + one cursor bar.
    assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
    let bar = verts[verts.len() - VERTS_PER_QUAD];
    let expected = cursor_underline_rect(0.0, 0.0, cell_w, cell_h);
    assert_eq!(bar.is_glyph, 0.0);
    assert_eq!(bar.pos, [expected[0], expected[1]]);
    // The bar hugs the bottom edge of the cell.
    assert!((expected[3] - cell_h).abs() < 1e-6);
    assert!(expected[1] > cell_h * 0.5);
    assert_eq!(bar.color, text::foreground_linear(Color::Default));
}

#[test]
fn bar_cursor_emits_single_left_bar() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    let term = Terminal::new(2, 1);
    let mut verts = Vec::new();
    build_vertices_with_cursor_into(&mut verts, &term.snapshot(), &atlas, CursorStyle::Bar);
    assert_eq!(verts.len(), 3 * VERTS_PER_QUAD);
    let bar = verts[verts.len() - VERTS_PER_QUAD];
    let expected = cursor_bar_rect(0.0, 0.0, cell_w, cell_h);
    assert_eq!(bar.is_glyph, 0.0);
    assert_eq!(bar.pos, [expected[0], expected[1]]);
    // The bar hugs the left edge and spans the full cell height.
    assert!((expected[0]).abs() < 1e-6);
    assert!(expected[2] < cell_w * 0.5);
    assert!((expected[3] - cell_h).abs() < 1e-6);
    assert_eq!(bar.color, text::foreground_linear(Color::Default));
}

#[test]
fn cursor_render_params_default_is_byte_identical() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Wave-15b R1: a default `CursorRenderParams` threaded through the
    // with-origin/with-params path produces vertices byte-identical to the
    // legacy origin-only path, for every cursor style.
    let term = Terminal::new(3, 2);
    let snapshot = term.snapshot();
    for style in [CursorStyle::Block, CursorStyle::Underline, CursorStyle::Bar] {
        let mut legacy = Vec::new();
        append_cursor_vertices(&mut legacy, &snapshot, &atlas, style);
        let mut threaded = Vec::new();
        append_cursor_vertices_with_origin(
            &mut threaded,
            &snapshot,
            &atlas,
            style,
            [0.0, 0.0],
            CursorRenderParams::default(),
        );
        assert_eq!(legacy, threaded, "style {style:?} must be byte-identical");
    }
}

#[test]
fn cursor_render_params_offset_and_alpha_are_live() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Wave-15b R1 (inverse): the threading must actually apply — a guard
    // against a future edit silently dropping `params` (which would leave the
    // default-identity test green while the feature is dead). A blank cell emits
    // exactly one block quad, so vertex 0 is the cursor block.
    let term = Terminal::new(3, 2);
    let snapshot = term.snapshot();

    let mut base = Vec::new();
    append_cursor_vertices_with_origin(
        &mut base,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        [0.0, 0.0],
        CursorRenderParams::default(),
    );
    assert!(
        (base[0].color[3] - 1.0).abs() < 1e-6,
        "default alpha is opaque"
    );

    // alpha=0.5 halves the block color alpha (glyph contrast derived first).
    let mut faded = Vec::new();
    append_cursor_vertices_with_origin(
        &mut faded,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        [0.0, 0.0],
        CursorRenderParams {
            offset: [0.0, 0.0],
            alpha: 0.5,
        },
    );
    assert!(
        (faded[0].color[3] - 0.5).abs() < 1e-6,
        "alpha must multiply the cursor block color alpha"
    );

    // offset shifts the block quad origin by exactly [dx, dy].
    let mut shifted = Vec::new();
    append_cursor_vertices_with_origin(
        &mut shifted,
        &snapshot,
        &atlas,
        CursorStyle::Block,
        [0.0, 0.0],
        CursorRenderParams {
            offset: [5.0, 7.0],
            alpha: 1.0,
        },
    );
    assert!((shifted[0].pos[0] - (base[0].pos[0] + 5.0)).abs() < 1e-6);
    assert!((shifted[0].pos[1] - (base[0].pos[1] + 7.0)).abs() < 1e-6);
}

#[test]
fn hidden_cursor_emits_nothing_for_any_style() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Cursor hidden (also the blink "off" phase): no cursor quad regardless
    // of shape. Four blank cells -> four backgrounds only.
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    let snapshot = term.snapshot();
    for style in [CursorStyle::Block, CursorStyle::Underline, CursorStyle::Bar] {
        let mut verts = Vec::new();
        build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, style);
        assert_eq!(verts.len(), 4 * VERTS_PER_QUAD, "style {style:?}");
    }
}

// --- GRID-RESOLVE-COVERAGE: the SGR-dim × focus-dim × RV1-floor matrix ---
//
// The per-cell resolve closure runs three perceptual steps in a load-bearing
// order: SGR dim → ID2 focus dim (fg *and* bg) → RV1 contrast floor. The
// existing tests cover each step in isolation (dim_attribute_scales…,
// min_contrast_floor_lifts…); these deepen the *interaction* — combined
// application, the load-bearing ordering, the two floor sites, and that the
// dim is the OKLab perceptual path rather than a naive linear halving.

/// Sum of absolute per-channel RGB differences between two resolved colors —
/// a small, dependency-free "visibly different" witness for these tests.
fn rgb_l1(a: [f32; 4], b: [f32; 4]) -> f32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

/// Ordering guard: the floor must run **after** both dims, not before.
///
/// Replays the closure's exact math with the real seam functions
/// (`text::dim_linear_rgba` for focus dim, `color::enforce_min_contrast` for
/// the floor) at an explicit ratio — global-free, so it never perturbs (or is
/// perturbed by) the process `MIN_CONTRAST`. Proves that the live order
/// (dim → floor) meets the ratio against the dimmed background, while the
/// swapped order (floor → dim) drops back **below** the ratio: dimming both
/// fg and bg after the floor pulls their luminances toward the `+0.05`
/// offsets and shrinks the contrast. This is exactly why the floor is last.
#[test]
fn resolve_floor_must_run_after_both_dims() {
    let fg = text::foreground_linear(Color::Rgb(120, 120, 120));
    let bg = text::background_linear(Color::Rgb(20, 20, 20));
    let focus = 0.45_f32;
    let ratio = 5.0_f32;

    // Live order: focus-dim both, then floor the dimmed fg against dimmed bg.
    let fg_dim = text::dim_linear_rgba(fg, focus);
    let bg_dim = text::dim_linear_rgba(bg, focus);
    let live_fg = {
        let [r, g, b] = crate::color::enforce_min_contrast(
            [fg_dim[0], fg_dim[1], fg_dim[2]],
            [bg_dim[0], bg_dim[1], bg_dim[2]],
            ratio,
        );
        [r, g, b, fg_dim[3]]
    };
    let live_contrast = crate::color::wcag_contrast(
        [live_fg[0], live_fg[1], live_fg[2]],
        [bg_dim[0], bg_dim[1], bg_dim[2]],
    );
    assert!(
        live_contrast + 1e-3 >= ratio,
        "live order (dim→floor) must meet the floor: {live_contrast} < {ratio}"
    );

    // Swapped order: floor first (against the undimmed bg), then focus-dim —
    // the dim erodes the contrast the floor had just established.
    let floored_first = {
        let [r, g, b] =
            crate::color::enforce_min_contrast([fg[0], fg[1], fg[2]], [bg[0], bg[1], bg[2]], ratio);
        [r, g, b, fg[3]]
    };
    let swapped_fg = text::dim_linear_rgba(floored_first, focus);
    let swapped_contrast = crate::color::wcag_contrast(
        [swapped_fg[0], swapped_fg[1], swapped_fg[2]],
        [bg_dim[0], bg_dim[1], bg_dim[2]],
    );
    assert!(
        swapped_contrast < ratio - 1e-2,
        "swapped order (floor→dim) should fall below the floor: {swapped_contrast}"
    );
    assert!(
        rgb_l1(live_fg, swapped_fg) > 0.02,
        "the two orders must produce visibly different foregrounds"
    );
}

/// ID2 focus dim in the live closure recedes **both** the foreground and the
/// background, perceptually (OKLab), preserving hue.
///
/// Drives the real `build_cell_vertices_with_focus_dim_into` seam at
/// `focus_dim = 0.0` vs `0.3`. The background quad is the most robust witness:
/// the closure dims bg but never routes it through the floor, so its resolved
/// color is independent of the process `MIN_CONTRAST` — this part holds no
/// matter what a concurrent test does to the global. A saturated bg makes the
/// hue-preservation check meaningful; a high-contrast fg keeps the floor inert
/// so the fg-recede check is robust too.
#[test]
fn focus_dim_recedes_fg_and_bg_perceptually_in_closure() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Saturated blue background, bright foreground glyph (high fg/bg contrast).
    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[38;2;235;235;235;48;2;40;70;160mX");
    let snapshot = term.snapshot();

    let mut focused = Vec::new();
    build_cell_vertices_with_focus_dim_into(
        &mut focused,
        &snapshot,
        &atlas,
        &[],
        0.0,
        BackgroundTreatmentParams::default(),
    );
    let mut unfocused = Vec::new();
    build_cell_vertices_with_focus_dim_into(
        &mut unfocused,
        &snapshot,
        &atlas,
        &[],
        0.3,
        BackgroundTreatmentParams::default(),
    );

    // focus_dim = 0.0 is the off-path gate: an exact no-op.
    assert_eq!(
        focused,
        unfocused_baseline(&snapshot, &atlas),
        "focus_dim=0.0 must be byte-identical to the focus-agnostic build"
    );

    // verts[0] is the pass-1 background quad; verts[VERTS_PER_QUAD] the glyph.
    let bg_f = focused[0].color;
    let bg_u = unfocused[0].color;
    let fg_f = focused[VERTS_PER_QUAD].color;
    let fg_u = unfocused[VERTS_PER_QUAD].color;

    let lum = |c: [f32; 4]| crate::color::relative_luminance([c[0], c[1], c[2]]);
    // Both fg and bg recede in luminance under focus dim.
    assert!(
        lum(bg_u) < lum(bg_f),
        "bg must recede: {} -> {}",
        lum(bg_f),
        lum(bg_u)
    );
    assert!(
        lum(fg_u) < lum(fg_f),
        "fg must recede: {} -> {}",
        lum(fg_f),
        lum(fg_u)
    );

    // The background dim is perceptual: hue is preserved (OKLCH). bg never
    // passes through the floor, so this is fully global-independent.
    let hue = |c: [f32; 4]| {
        crate::color::oklab_to_oklch(crate::color::linear_to_oklab([c[0], c[1], c[2]])).h
    };
    let mut dh = (hue(bg_f) - hue(bg_u)).abs();
    if dh > std::f32::consts::PI {
        dh = std::f32::consts::TAU - dh;
    }
    assert!(
        dh < 0.03,
        "focus dim must preserve background hue; drift {dh} rad"
    );
}

/// Build the same snapshot through the focus-agnostic entry, for the
/// off-path-gate equality check above. Kept as a helper so the gate compares
/// against the real `build_cell_vertices_with_color_glyph_runs_into` path
/// (which forwards `0.0`) rather than a hand-rolled duplicate.
fn unfocused_baseline(snapshot: &Snapshot, atlas: &GlyphAtlas) -> Vec<Vertex> {
    let mut v = Vec::new();
    build_cell_vertices_with_color_glyph_runs_into(&mut v, snapshot, atlas, &[]);
    v
}

/// The live closure routes SGR-dim through `dim_color`, and at
/// `DIM_PERCEPTUAL_AMOUNT` that is *equivalent to* — not merely "as bright
/// as" — the historical naive linear `×0.5` halving.
///
/// This equivalence is exact (within float round-trip error) and is a
/// mathematical identity, not a tuning coincidence: scaling all three OKLab
/// coordinates `(L, a, b)` by a uniform factor `k` is identical to scaling
/// linear RGB by `k³`, because OKLab's only nonlinearity is a per-component
/// cube root that a uniform scale commutes through. `dim_perceptual(c, a)`
/// scales `(L, a, b)` by `1 - a`, so it equals `(1 - a)³ · c`; with
/// `a = 1 - ∛0.5` that factor is exactly `0.5`. (Both paths therefore also
/// preserve hue — a uniform linear scale already keeps chromaticity — so the
/// "perceptual" framing buys hue-stability that naive halving already had;
/// see the report flag.) This test pins the equivalence so a future change to
/// `dim_perceptual` that silently broke the established SGR-dim output would
/// be caught. Global-free: floor stays at its 1.0 passthrough (high-contrast
/// color keeps it inert even under a concurrent raise).
#[test]
fn closure_sgr_dim_equals_naive_half_brightness() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Saturated orange on the default dark bg: high contrast (floor inert),
    // strong chroma so any real divergence from ×0.5 would show.
    let mut term = Terminal::new(1, 1);
    term.advance(b"\x1b[?25l\x1b[2;38;2;220;90;20mX");
    let snapshot = term.snapshot();
    let rendered = build_vertices(&snapshot, &atlas)[VERTS_PER_QUAD].color;

    let undimmed = text::foreground_linear(Color::Rgb(220, 90, 20));
    // The rendered fg is the perceptual operator output …
    assert_eq!(
        rendered,
        dim_color(undimmed),
        "rendered dim fg must be the perceptual operator output"
    );
    // … which equals a naive linear ×0.5 halving within round-trip error.
    let naive_half = [
        undimmed[0] * 0.5,
        undimmed[1] * 0.5,
        undimmed[2] * 0.5,
        undimmed[3],
    ];
    assert!(
        rgb_l1(rendered, naive_half) < 1e-5,
        "SGR-dim at DIM_PERCEPTUAL_AMOUNT must equal a ×0.5 halving: \
             {rendered:?} vs {naive_half:?}"
    );
}

// --- ID3/U5 background treatment (gradient / vignette) ----------------------

/// KILL-SHOT (trap 1): the default params are inactive, and `apply_to` is an
/// exact identity for every cell — so the grid apply block is skipped and the
/// rendered frame is byte-identical to the pre-feature renderer.
#[test]
fn bg_treatment_default_is_identity() {
    let params = BackgroundTreatmentParams::default();
    assert!(!params.active());
    let bg = [0.2, 0.3, 0.4, 1.0];
    for (row, col) in [(0, 0), (3, 7), (23, 79)] {
        assert_eq!(params.apply_to(bg, row, col, 24, 80), bg);
    }
}

/// An explicitly inactive (zero-strength) treatment is also an identity.
#[test]
fn bg_treatment_zero_strength_is_identity() {
    let params = BackgroundTreatmentParams {
        kind: BackgroundTreatment::Vignette,
        strength: 0.0,
    };
    assert!(!params.active());
    let bg = [0.5, 0.5, 0.5, 1.0];
    assert_eq!(params.apply_to(bg, 5, 5, 24, 80), bg);
}

/// Gradient: the top row is unchanged (falloff 0) and the bottom row is
/// darkened the most, monotonically increasing with row; alpha is preserved.
#[test]
fn bg_treatment_gradient_darkens_toward_bottom() {
    let params = BackgroundTreatmentParams {
        kind: BackgroundTreatment::Gradient,
        strength: 1.0,
    };
    let bg = [0.8, 0.8, 0.8, 1.0];
    let rows = 10;
    let cols = 4;
    let top = params.apply_to(bg, 0, 0, rows, cols);
    let mid = params.apply_to(bg, 5, 0, rows, cols);
    let bottom = params.apply_to(bg, rows - 1, 0, rows, cols);
    assert_eq!(top, bg, "top row falloff is 0 ⇒ unchanged");
    assert!(mid[0] < top[0], "middle darker than top");
    assert!(bottom[0] < mid[0], "bottom darker than middle");
    assert_eq!(bottom[3], 1.0, "alpha preserved");
    // The bottom never darkens past the documented cap.
    let min_factor = 1.0 - MAX_BG_TREATMENT_DARKEN;
    assert!(bottom[0] >= bg[0] * min_factor - 1e-6);
}

/// Vignette: the center cell is unchanged (falloff 0) and a corner is darkened;
/// the corner is the farthest point so it carries the maximum attenuation.
#[test]
fn bg_treatment_vignette_darkens_toward_corners() {
    let params = BackgroundTreatmentParams {
        kind: BackgroundTreatment::Vignette,
        strength: 1.0,
    };
    let bg = [0.6, 0.6, 0.6, 1.0];
    let rows = 9;
    let cols = 9;
    let center = params.apply_to(bg, 4, 4, rows, cols);
    let corner = params.apply_to(bg, 0, 0, rows, cols);
    assert_eq!(center, bg, "center falloff is 0 ⇒ unchanged");
    assert!(corner[0] < bg[0], "corner is darkened");
    assert_eq!(corner[3], 1.0, "alpha preserved");
    // Corner is the farthest point ⇒ maximum documented attenuation.
    let expected = bg[0] * (1.0 - MAX_BG_TREATMENT_DARKEN);
    assert!((corner[0] - expected).abs() < 1e-5);
}

/// Degenerate grids never panic and never divide by zero.
#[test]
fn bg_treatment_degenerate_grids_are_total() {
    let g = BackgroundTreatmentParams {
        kind: BackgroundTreatment::Gradient,
        strength: 1.0,
    };
    let v = BackgroundTreatmentParams {
        kind: BackgroundTreatment::Vignette,
        strength: 1.0,
    };
    let bg = [0.3, 0.3, 0.3, 1.0];
    // 1x1 grid: both falloffs are 0 (no extent), so identity.
    assert_eq!(g.apply_to(bg, 0, 0, 1, 1), bg);
    assert_eq!(v.apply_to(bg, 0, 0, 1, 1), bg);
}

/// TRANSPARENCY (MENU-OPACITY): `CellRegion` marks the overlay panel's cell
/// span so the builder holds those backgrounds opaque while the terminal cells
/// around them scale with the window opacity.
#[test]
fn cell_region_contains_covers_its_rect_only() {
    let r = CellRegion {
        left: 2,
        top: 1,
        width: 3,
        height: 2,
    };
    // Inside: [rows 1..=2] x [cols 2..=4].
    assert!(r.contains(1, 2));
    assert!(r.contains(2, 4));
    // Outside on every edge.
    assert!(!r.contains(0, 2), "row above");
    assert!(!r.contains(3, 2), "row below");
    assert!(!r.contains(1, 1), "col left");
    assert!(!r.contains(1, 5), "col right");
}

/// TRANSPARENCY (MENU-OPACITY) core guarantee: with a translucent
/// `cell_bg_opacity`, cells inside the opaque region draw fully opaque while
/// cells outside scale by the opacity — and `None` scales every cell (the
/// byte-identical path). Mirrors the single-pane path where an overlay panel is
/// painted into the translucent snapshot: the panel stays a readable surface,
/// the terminal behind it keeps the window opacity.
#[test]
fn opaque_region_holds_marked_cells_opaque_only() {
    let Some(atlas) = atlas() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Three blank cells in a row => three background quads (pass 1 emits all
    // backgrounds first; blanks emit no glyph and the cursor is hidden).
    let mut term = Terminal::new(3, 1);
    term.advance(b"\x1b[?25l");
    let snapshot = term.snapshot();

    let opacity = 0.5;
    let bg_alpha = |verts: &[Vertex], quad: usize| verts[quad * VERTS_PER_QUAD].color[3];

    // Region covers ONLY the middle cell (col 1).
    let region = CellRegion {
        left: 1,
        top: 0,
        width: 1,
        height: 1,
    };
    let mut with_region = Vec::new();
    build_cell_vertices_with_focus_dim_and_origin_into(
        &mut with_region,
        &snapshot,
        &atlas,
        &[],
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        opacity,
        Some(region),
    );
    let edge = bg_alpha(&with_region, 0);
    let marked = bg_alpha(&with_region, 1);
    let edge_right = bg_alpha(&with_region, 2);
    // The marked (overlay) cell is fully opaque; the edges scale by the opacity.
    assert!(
        marked > edge,
        "marked cell must be more opaque than the edges"
    );
    assert!(
        (edge - marked * opacity).abs() < 1e-6,
        "an edge cell scales the marked cell's opaque alpha by the window opacity"
    );
    assert!(
        (edge_right - edge).abs() < 1e-6,
        "both edge cells scale identically"
    );

    // `None` scales EVERY cell — byte-identical to the pre-region path — so the
    // middle cell now matches the edges (nothing is held opaque).
    let mut no_region = Vec::new();
    build_cell_vertices_with_focus_dim_and_origin_into(
        &mut no_region,
        &snapshot,
        &atlas,
        &[],
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        opacity,
        None,
    );
    assert!(
        (bg_alpha(&no_region, 1) - edge).abs() < 1e-6,
        "with no region the middle cell scales like every other cell"
    );
    assert!(
        (bg_alpha(&no_region, 1) - marked).abs() > 1e-6,
        "the region is what lifts the middle cell to opaque"
    );
}

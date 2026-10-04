// SPDX-License-Identifier: GPL-3.0-only
//! Pixel fixtures for the test-only bidi rendering seam.
//!
//! Every frame is composited on the CPU from the real vertex builder and the
//! real atlas, with the synthetic `tests/fixtures/fonts/bidi-mixed.ttf` face
//! (project-authored, see the fixture README). The main oracle: rendering a
//! logical line in display order must give exactly the pixels the unchanged
//! production path draws for the hand-written visual-order string. Joined
//! Arabic forms have no visual-order string, so that fixture compares cell
//! blocks against the production logical-order frame instead.

use super::*;
use crate::core::Terminal;
use crate::emoji::{ColorGlyphAtlas, ColorGlyphId, ColorGlyphKey};
use crate::ligature::{LigatureFonts, LigatureShaper};
use crate::text::{CellSize, FontHandle, FontStyle};

const PX: f32 = 28.0;

fn font() -> FontHandle {
    FontHandle::try_from_vec(include_bytes!("../../tests/fixtures/fonts/bidi-mixed.ttf").to_vec())
        .expect("bidi fixture font parses")
}

struct Fonts(FontHandle);

impl LigatureFonts for Fonts {
    fn ligature_font(&self, _style: FontStyle) -> &FontHandle {
        &self.0
    }
}

/// Fixture font, its atlas holding every character the fixtures draw, and
/// the shaping font set.
fn setup() -> (GlyphAtlas, Fonts) {
    let font = font();
    let mut atlas = GlyphAtlas::build(&font, PX);
    for ch in
        "\u{05D0}\u{05D1}\u{05D2}\u{05D3}\u{05D4}\u{0628}\u{0644}\u{0627}\u{754C}\u{3001}".chars()
    {
        atlas.ensure(&font, ch);
    }
    (atlas, Fonts(font))
}

fn terminal(text: &str, cols: usize, rows: usize) -> Terminal {
    let mut terminal = Terminal::new(cols, rows);
    // Hide the cursor: the cell build never draws it, but keep frames plain.
    terminal.advance(b"\x1b[?25l");
    terminal.advance(text.as_bytes());
    terminal
}

fn display_map(terminal: &Terminal) -> BidiDisplayMap {
    let wrapped: Vec<bool> = terminal
        .visible_search_rows(0)
        .iter()
        .map(|row| row.wrapped)
        .collect();
    BidiDisplayMap::plan(&terminal.snapshot(), &wrapped)
}

/// Linear-RGB frame of one snapshot, composited with the shader's blend.
#[derive(Debug, PartialEq)]
struct Frame {
    width: usize,
    cell_w: usize,
    cell_h: usize,
    px: Vec<[f32; 3]>,
}

impl Frame {
    /// The pixels of visual columns `columns` of `row`.
    fn block(&self, row: usize, columns: std::ops::Range<usize>) -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        for y in row * self.cell_h..(row + 1) * self.cell_h {
            let start = y * self.width + columns.start * self.cell_w;
            out.extend_from_slice(&self.px[start..start + columns.len() * self.cell_w]);
        }
        out
    }
}

fn composite(snapshot: &Snapshot, atlas: &GlyphAtlas, verts: &[Vertex]) -> Frame {
    let cell_w = atlas.cell.width as usize;
    let cell_h = atlas.cell.height as usize;
    let width = snapshot.dimensions.columns * cell_w;
    let height = snapshot.dimensions.rows * cell_h;
    let mut px = vec![[0.0_f32; 3]; width * height];
    for quad in verts.as_chunks::<INSTANCES_PER_QUAD>().0 {
        let v = &quad[0];
        let [x0, y0] = v.pos;
        let [x1, y1] = v.end_pos;
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        for py in (y0.floor().max(0.0) as usize)..(y1.ceil() as usize).min(height) {
            let cy = py as f32 + 0.5;
            if cy < y0 || cy >= y1 {
                continue;
            }
            for px_x in (x0.floor().max(0.0) as usize)..(x1.ceil() as usize).min(width) {
                let cx = px_x as f32 + 0.5;
                if cx < x0 || cx >= x1 {
                    continue;
                }
                let alpha = if v.is_glyph > 0.5 {
                    let u = v.uv[0] + (cx - x0) / (x1 - x0) * (v.end_uv[0] - v.uv[0]);
                    let t = v.uv[1] + (cy - y0) / (y1 - y0) * (v.end_uv[1] - v.uv[1]);
                    let ax = ((u * atlas.width as f32) as usize).min(atlas.width as usize - 1);
                    let ay = ((t * atlas.height as f32) as usize).min(atlas.height as usize - 1);
                    v.color[3] * f32::from(atlas.data[ay * atlas.width as usize + ax]) / 255.0
                } else {
                    v.color[3]
                };
                let dst = &mut px[py * width + px_x];
                for (out, ink) in dst.iter_mut().zip(v.color) {
                    *out = ink * alpha + *out * (1.0 - alpha);
                }
            }
        }
    }
    Frame {
        width,
        cell_w,
        cell_h,
        px,
    }
}

/// Rasterize every contextual glyph of `runs`, as the live renderer's ensure
/// pass does; a run with a missing slot falls back to scalar glyphs.
fn ensure_runs(atlas: &mut GlyphAtlas, fonts: &Fonts, runs: &[LigatureRun]) {
    for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
        atlas.ensure_shaped(&fonts.0, glyph.key);
        assert!(
            atlas.contains_shaped(glyph.key),
            "{:?} rasterized",
            glyph.key
        );
    }
}

/// The unchanged production path: logical order, live ligature runs when
/// `ligatures` is on.
fn production_frame_with(text: &str, cols: usize, rows: usize, ligatures: bool) -> Frame {
    let (mut atlas, fonts) = setup();
    let snapshot = terminal(text, cols, rows).snapshot();
    let runs = LigatureShaper::new().build_runs(ligatures, &snapshot, &fonts, &[]);
    ensure_runs(&mut atlas, &fonts, &runs);
    let mut verts = Vec::new();
    build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
        &mut verts,
        &snapshot,
        &atlas,
        &[],
        &runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        1.0,
        None,
        ChromePin::NONE,
    );
    composite(&snapshot, &atlas, &verts)
}

fn production_frame(text: &str, cols: usize, rows: usize) -> Frame {
    production_frame_with(text, cols, rows, true)
}

/// The test-only seam: display order, directional ligature runs.
fn bidi_frame(text: &str, cols: usize, rows: usize) -> (Frame, Vec<Vertex>, BidiDisplayMap) {
    let (mut atlas, fonts) = setup();
    let terminal = terminal(text, cols, rows);
    let snapshot = terminal.snapshot();
    let map = display_map(&terminal);
    let runs = LigatureShaper::new().build_runs_bidi(&snapshot, &fonts, &[], &map);
    ensure_runs(&mut atlas, &fonts, &runs);
    let mut verts = Vec::new();
    build_cell_vertices_with_bidi_into(&mut verts, &snapshot, &atlas, &[], &runs, &map);
    (composite(&snapshot, &atlas, &verts), verts, map)
}

/// Assert that `logical` renders in display order exactly as the production
/// path renders the hand-ordered `visual` string.
fn assert_renders_as(logical: &str, visual: &str, cols: usize) {
    let (frame, _, map) = bidi_frame(logical, cols, 1);
    assert!(!map.is_identity(), "{logical:?} must reorder");
    let expected = production_frame(visual, cols, 1);
    assert!(
        frame == expected,
        "{logical:?} must render as the visual string {visual:?}"
    );
}

#[test]
fn ltr_only_text_through_the_seam_is_byte_identical_to_production() {
    let (mut atlas, fonts) = setup();
    for text in [
        "plain ab->c 2x",
        "a\u{754C}b (x) [y] 12",
        "\u{3001}M\u{754C}",
    ] {
        let terminal = terminal(text, 16, 2);
        let snapshot = terminal.snapshot();
        let map = display_map(&terminal);
        assert!(map.is_identity(), "{text:?}");
        let mut shaper = LigatureShaper::new();
        let live = shaper.build_runs(true, &snapshot, &fonts, &[]);
        ensure_runs(&mut atlas, &fonts, &live);
        assert_eq!(
            LigatureShaper::new().build_runs_bidi(&snapshot, &fonts, &[], &map),
            live,
            "{text:?}: level-0 runs shape exactly as the live path"
        );
        let mut production = Vec::new();
        build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
            &mut production,
            &snapshot,
            &atlas,
            &[],
            &live,
            0.0,
            [0.0, 0.0],
            BackgroundTreatmentParams::default(),
            1.0,
            1.0,
            None,
            ChromePin::NONE,
        );
        let mut seam = Vec::new();
        build_cell_vertices_with_bidi_into(&mut seam, &snapshot, &atlas, &[], &live, &map);
        assert_eq!(seam, production, "{text:?}");
    }
}

#[test]
fn rows_left_in_identity_layout_keep_production_pixels() {
    // Row 0 reorders; row 1 is a separate, left-to-right paragraph.
    let text = "ab \u{05D0}\u{05D1}\r\nxy->2x (c)";
    let (frame, _, map) = bidi_frame(text, 12, 2);
    assert!(map.row_is_reordered(0));
    assert!(!map.row_is_reordered(1));
    let production = production_frame(text, 12, 2);
    assert!(
        frame.block(1, 0..12) == production.block(1, 0..12),
        "row 1 keeps production pixels"
    );
    assert!(
        frame.block(0, 0..12) != production.block(0, 0..12),
        "row 0 reorders"
    );
}

#[test]
fn hebrew_latin_and_digits_render_in_display_order() {
    assert_renders_as(
        "ab \u{05D0}\u{05D1} 12 \u{05D2}\u{05D3}.",
        "ab \u{05D3}\u{05D2} 12 \u{05D1}\u{05D0}.",
        16,
    );
}

#[test]
fn mirrored_bracket_pair_presents_mirror_glyphs() {
    let logical = "\u{05D0}\u{05D1} (\u{05D2}\u{05D3}) \u{05D4}";
    assert_renders_as(logical, "\u{05D4} (\u{05D3}\u{05D2}) \u{05D1}\u{05D0}", 12);
    // Without presentation mirroring the reversed brackets would face away.
    let (frame, _, _) = bidi_frame(logical, 12, 1);
    assert!(
        frame != production_frame("\u{05D4} )\u{05D3}\u{05D2}( \u{05D1}\u{05D0}", 12, 1),
        "unmirrored brackets would differ"
    );
}

#[test]
fn wide_cells_beside_and_inside_rtl_keep_two_adjacent_cells() {
    // U+754C is left to right and stays put; the ideographic comma U+3001 is
    // a neutral between two Hebrew letters, so it moves as one two-cell owner.
    assert_renders_as(
        "\u{754C}\u{05D0}\u{3001}\u{05D1}x",
        "\u{754C}\u{05D1}\u{3001}\u{05D0}x",
        10,
    );
    let (_, _, map) = bidi_frame("\u{754C}\u{05D0}\u{3001}\u{05D1}x", 10, 1);
    // Logical: 0-1 U+754C, 2 alef, 3-4 U+3001, 5 bet, 6 x.
    assert_eq!(map.visual_column(0, 3), 3, "lead stays first");
    assert_eq!(map.visual_column(0, 4), 4, "continuation follows its lead");
    assert_eq!(map.visual_column(0, 2), 5);
    assert_eq!(map.visual_column(0, 5), 2);
}

#[test]
fn ligature_crossing_a_direction_boundary_does_not_form() {
    // "2x" is a fixture ligature. Logically adjacent here, but the digits sit
    // at level 2 and x at level 0, so alef separates them on screen.
    let logical = "\u{05D0}12x";
    let production = LigatureShaper::new().build_runs(
        true,
        &terminal(logical, 8, 1).snapshot(),
        &setup().1,
        &[],
    );
    assert!(
        !production.is_empty(),
        "the logical-order path forms the 2x ligature, so the fixture is live"
    );
    assert_renders_as(logical, "12\u{05D0}x", 8);
    // A left-to-right ligature inside right-to-left text is not shaped; the
    // reversed greater-than sign presents mirrored.
    assert_renders_as("\u{05D0}->\u{05D1}", "\u{05D1}<-\u{05D0}", 8);
    // A level-0 ligature on a reordered row still forms in place.
    assert_renders_as("\u{05D0}\u{05D1} a->b", "\u{05D1}\u{05D0} a->b", 10);
    assert!(
        production_frame("a->b", 10, 1) != production_frame_with("a->b", 10, 1, false),
        "the arrow ligature draws, so the in-place case is live"
    );
}

#[test]
fn arabic_joining_run_draws_right_to_left_with_its_lam_alef_ligature() {
    // x, space, beh lam alef, space, c. Logical shaping gives beh.init at
    // column 2 and a two-cell lam-alef final form anchored at column 3.
    let logical = "x \u{0628}\u{0644}\u{0627} c";
    let production = production_frame(logical, 8, 1);
    assert!(
        production != production_frame_with(logical, 8, 1, false),
        "the logical-order path joins the run, so the fixture is live"
    );
    let (frame, _, map) = bidi_frame(logical, 8, 1);
    assert_eq!(
        (2..5).map(|c| map.visual_column(0, c)).collect::<Vec<_>>(),
        [4, 3, 2]
    );
    assert!(
        frame.block(0, 4..5) == production.block(0, 2..3),
        "beh.init"
    );
    assert!(
        frame.block(0, 2..4) == production.block(0, 3..5),
        "lam-alef covers the visual cells of lam and alef"
    );
    assert!(
        frame.block(0, 0..2) == production.block(0, 0..2),
        "block 0, 0..2"
    );
    assert!(
        frame.block(0, 5..8) == production.block(0, 5..8),
        "block 0, 5..8"
    );
    // The run was joined, not drawn as isolated letters.
    assert!(
        frame.block(0, 4..5) != production_frame("\u{0628}", 8, 1).block(0, 0..1),
        "beh is drawn in its initial form"
    );
}

#[test]
fn background_quads_tile_every_row_exactly_once() {
    for text in [
        "ab \u{05D0}\u{05D1} 12 \u{05D2}\u{05D3}.",
        "\u{754C}\u{05D0}\u{3001}\u{05D1}x",
        "x \u{0628}\u{0644}\u{0627} y",
        "\u{05D0}\u{05D1} (\u{05D2}\u{05D3}) \u{05D4}",
    ] {
        let (frame, verts, _) = bidi_frame(text, 10, 1);
        let mut spans: Vec<(f32, f32)> = verts
            .as_chunks::<INSTANCES_PER_QUAD>()
            .0
            .iter()
            .filter(|quad| quad[0].is_glyph == 0.0)
            .map(|quad| (quad[0].pos[0], quad[0].end_pos[0]))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut edge = 0.0;
        for (x0, x1) in spans {
            assert_eq!(
                x0, edge,
                "{text:?}: backgrounds abut without gap or overlap"
            );
            edge = x1;
        }
        assert_eq!(edge, frame.width as f32, "{text:?}: row width unchanged");
    }
}

#[test]
fn glyph_ink_is_clipped_to_its_visual_owner_on_reordered_rows() {
    // The fixture y overflows its advance on both sides.
    let cell_w = setup().0.cell.width as f32;
    let glyph_spans = |verts: &[Vertex]| -> Vec<(f32, f32)> {
        verts
            .as_chunks::<INSTANCES_PER_QUAD>()
            .0
            .iter()
            .filter(|quad| quad[0].is_glyph == 1.0)
            .map(|quad| (quad[0].pos[0], quad[0].end_pos[0]))
            .collect()
    };
    let (_, verts, map) = bidi_frame("\u{05D0}y\u{05D1}", 6, 1);
    assert_eq!(map.visual_column(0, 1), 1);
    let y = glyph_spans(&verts)
        .into_iter()
        .find(|(x0, x1)| *x0 < 2.0 * cell_w && *x1 > cell_w)
        .expect("y glyph");
    assert_eq!(y, (cell_w, 2.0 * cell_w), "clipped to its own cell");
    // A row in identity layout keeps today's unclipped overflow ink.
    let (_, verts, _) = bidi_frame(" y", 6, 1);
    let y = glyph_spans(&verts)[0];
    assert!(y.0 < cell_w && y.1 > 2.0 * cell_w, "{y:?} overflows");
}

#[test]
fn color_glyph_runs_follow_display_order_without_changing_width() {
    let cell = CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    };
    let mut color_atlas = ColorGlyphAtlas::new(cell);
    let narrow = ColorGlyphKey::new(1, ColorGlyphId::Glyph(7), 16.0, 1.0, 1);
    let wide = ColorGlyphKey::new(1, ColorGlyphId::Glyph(8), 16.0, 1.0, 2);
    color_atlas
        .insert_premultiplied(narrow, 1, &vec![255u8; 8 * 16 * 4])
        .expect("narrow slot");
    color_atlas
        .insert_premultiplied(wide, 2, &vec![255u8; 16 * 16 * 4])
        .expect("wide slot");
    // Logical: 0 alef, 1-2 U+3001, 3 bet, 4 gimel, 5 x.
    let terminal = terminal("\u{05D0}\u{3001}\u{05D1}\u{05D2}x", 8, 1);
    let snapshot = terminal.snapshot();
    let map = display_map(&terminal);
    let runs = [
        ColorGlyphRun::cluster(0, 1, wide, 2),
        ColorGlyphRun::new(0, 3, narrow),
    ];
    let mut out = Vec::new();
    glyph_quads::color_glyph_vertices_core(
        &mut out,
        &snapshot,
        &color_atlas,
        &runs,
        [0.0, 0.0],
        ChromePin::NONE,
        RowFade::NONE,
        Some(&map),
    );
    let quads: Vec<(f32, f32)> = out
        .as_chunks::<INSTANCES_PER_QUAD>()
        .0
        .iter()
        .map(|quad| (quad[0].pos[0], quad[0].end_pos[0]))
        .collect();
    // Visual: gimel 0, bet 1, U+3001 2-3, alef 4, x 5.
    assert_eq!(quads, [(16.0, 32.0), (8.0, 16.0)]);
    let mut logical = Vec::new();
    build_color_glyph_vertices_with_origin_into(
        &mut logical,
        &snapshot,
        &color_atlas,
        &runs,
        [0.0, 0.0],
        ChromePin::NONE,
        RowFade::NONE,
    );
    let logical: Vec<(f32, f32)> = logical
        .as_chunks::<INSTANCES_PER_QUAD>()
        .0
        .iter()
        .map(|quad| (quad[0].pos[0], quad[0].end_pos[0]))
        .collect();
    assert_eq!(
        logical,
        [(8.0, 24.0), (24.0, 32.0)],
        "production stays logical"
    );
}

#[test]
fn soft_wrapped_rows_resolve_as_one_paragraph() {
    // "abc" then alef bet wraps onto row 1 at width 4; the first strong
    // character of the paragraph is on row 0, and each row reorders alone.
    let terminal = terminal("abc\u{05D0}\u{05D1}\u{05D2}", 4, 2);
    let map = display_map(&terminal);
    assert!(map.row_is_reordered(0) && map.row_is_reordered(1));
    // Row 0: a b c alef -> alef stays at column 3 (single-owner run).
    assert_eq!(map.visual_column(0, 3), 3);
    // Row 1: bet gimel blank blank -> gimel bet, blanks stay right.
    assert_eq!(map.visual_column(1, 0), 1);
    assert_eq!(map.visual_column(1, 1), 0);
    assert_eq!(map.visual_column(1, 2), 2);
}

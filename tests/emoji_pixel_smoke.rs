// SPDX-License-Identifier: GPL-3.0-only
use odytty::atlas::GlyphAtlas;
use odytty::core::{CursorStyle, Terminal};
use odytty::emoji::{
    ColorGlyphAtlas, ColorGlyphId, EmojiFont, EmojiRasterizer, discover_noto_color_emoji,
};
use odytty::grid::{self, ColorGlyphVertex, Vertex};

#[derive(Clone)]
struct Frame {
    width: usize,
    height: usize,
    rgba: Vec<[u8; 4]>,
}

impl Frame {
    fn new(width: usize, height: usize, color: [u8; 4]) -> Self {
        Self {
            width,
            height,
            rgba: vec![color; width * height],
        }
    }

    fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        self.rgba[y * self.width + x]
    }

    fn set_pixel(&mut self, x: usize, y: usize, color: [u8; 4]) {
        self.rgba[y * self.width + x] = color;
    }
}

fn setup() -> Option<(GlyphAtlas, EmojiRasterizer)> {
    let text_font = odytty::text::load_font().ok()?;
    let found = discover_noto_color_emoji()?;
    let emoji_font = EmojiFont::load(found.path).ok()?;
    Some((
        GlyphAtlas::build(&text_font, 24.0),
        EmojiRasterizer::from_font(emoji_font),
    ))
}

fn snapshot_for(text: &str, columns: usize) -> odytty::core::Snapshot {
    let mut terminal = Terminal::new(columns, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance(text.as_bytes());
    terminal.snapshot()
}

fn assert_one_cluster_run(text: &str, covered_columns: u8) {
    let Some((atlas, mut rasterizer)) = setup() else {
        eprintln!("skipping: text font or Noto Color Emoji unavailable");
        return;
    };
    let snapshot = snapshot_for(text, 16);
    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut color_atlas);

    assert_eq!(runs.len(), 1, "{text:?} should produce one color run");
    assert_eq!(runs[0].row, 0);
    assert_eq!(runs[0].column, 0);
    assert_eq!(runs[0].covered_columns, covered_columns);
    assert!(
        matches!(runs[0].key.glyph_id, ColorGlyphId::Cluster(_)),
        "{text:?} should be keyed by the full cluster"
    );

    let mut color_vertices = Vec::new();
    grid::build_color_glyph_vertices_into(&mut color_vertices, &snapshot, &color_atlas, &runs);
    assert_eq!(
        color_vertices.len(),
        grid::VERTS_PER_QUAD,
        "{text:?} should draw one color glyph quad"
    );
    assert_eq!(color_vertices[0].pos, [0.0, 0.0]);
    assert_eq!(
        color_vertices[5].pos,
        [atlas.cell.width as f32 * 2.0, atlas.cell.height as f32],
        "{text:?} should honor the 2-cell color bitmap contract"
    );
}

fn composite_color_glyphs(
    frame: &mut Frame,
    atlas: &ColorGlyphAtlas,
    vertices: &[ColorGlyphVertex],
) {
    for quad in vertices.chunks_exact(grid::VERTS_PER_QUAD) {
        let x0 = quad.iter().map(|v| v.pos[0]).fold(f32::INFINITY, f32::min) as usize;
        let y0 = quad.iter().map(|v| v.pos[1]).fold(f32::INFINITY, f32::min) as usize;
        let x1 = quad
            .iter()
            .map(|v| v.pos[0])
            .fold(f32::NEG_INFINITY, f32::max) as usize;
        let y1 = quad
            .iter()
            .map(|v| v.pos[1])
            .fold(f32::NEG_INFINITY, f32::max) as usize;
        let u0 = quad.iter().map(|v| v.uv[0]).fold(f32::INFINITY, f32::min);
        let v0 = quad.iter().map(|v| v.uv[1]).fold(f32::INFINITY, f32::min);
        let u1 = quad
            .iter()
            .map(|v| v.uv[0])
            .fold(f32::NEG_INFINITY, f32::max);
        let v1 = quad
            .iter()
            .map(|v| v.uv[1])
            .fold(f32::NEG_INFINITY, f32::max);

        for y in y0..y1.min(frame.height) {
            for x in x0..x1.min(frame.width) {
                let tx = (x - x0) as f32 / (x1 - x0).max(1) as f32;
                let ty = (y - y0) as f32 / (y1 - y0).max(1) as f32;
                let ax = ((u0 + (u1 - u0) * tx) * atlas.width as f32)
                    .floor()
                    .clamp(0.0, (atlas.width - 1) as f32) as usize;
                let ay = ((v0 + (v1 - v0) * ty) * atlas.height as f32)
                    .floor()
                    .clamp(0.0, (atlas.height - 1) as f32) as usize;
                let i = (ay * atlas.width as usize + ax) * 4;
                let src = [
                    atlas.data[i],
                    atlas.data[i + 1],
                    atlas.data[i + 2],
                    atlas.data[i + 3],
                ];
                frame.set_pixel(x, y, over_premul(src, frame.pixel(x, y)));
            }
        }
    }
}

fn over_premul(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let inv = 255 - u16::from(src[3]);
    [
        (u16::from(src[0]) + u16::from(dst[0]) * inv / 255) as u8,
        (u16::from(src[1]) + u16::from(dst[1]) * inv / 255) as u8,
        (u16::from(src[2]) + u16::from(dst[2]) * inv / 255) as u8,
        (u16::from(src[3]) + u16::from(dst[3]) * inv / 255) as u8,
    ]
}

#[test]
fn audit_multi_codepoint_clusters_are_split_in_the_grid_before_color_stitching() {
    let skin = snapshot_for("\u{1F44D}\u{1F3FD}", 8);
    assert_eq!(skin.cells[0].grapheme(), "\u{1F44D}");
    assert!(skin.cells[1].wide_continuation);
    assert_eq!(skin.cells[2].grapheme(), "\u{1F3FD}");
    assert!(skin.cells[3].wide_continuation);

    let flag = snapshot_for("\u{1F1FA}\u{1F1F8}", 8);
    assert_eq!(flag.cells[0].grapheme(), "\u{1F1FA}");
    assert_eq!(flag.cells[1].grapheme(), "\u{1F1F8}");
    assert!(!flag.cells[1].wide_continuation);

    let keycap = snapshot_for("1\u{FE0F}\u{20E3}", 4);
    assert_eq!(keycap.cells[0].ch, '1');
    assert_eq!(keycap.cells[0].combining(), &['\u{FE0F}', '\u{20E3}']);
    assert_eq!(keycap.cells[0].grapheme(), "1\u{FE0F}\u{20E3}");

    let family = snapshot_for(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
        16,
    );
    assert_eq!(family.cells[0].grapheme(), "\u{1F468}\u{200D}");
    assert!(family.cells[1].wide_continuation);
    assert_eq!(family.cells[2].grapheme(), "\u{1F469}\u{200D}");
    assert!(family.cells[3].wide_continuation);
    assert_eq!(family.cells[4].grapheme(), "\u{1F467}\u{200D}");
    assert!(family.cells[5].wide_continuation);
    assert_eq!(family.cells[6].grapheme(), "\u{1F466}");
    assert!(family.cells[7].wide_continuation);
}

#[test]
fn real_emoji_renders_through_color_segment_over_background() {
    let Some((atlas, mut rasterizer)) = setup() else {
        eprintln!("skipping: text font or Noto Color Emoji unavailable");
        return;
    };
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance("\u{1F525}".as_bytes());
    let snapshot = terminal.snapshot();
    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut color_atlas);
    assert_eq!(runs.len(), 1, "fire emoji should produce one color run");

    let mut color_vertices = Vec::new();
    grid::build_color_glyph_vertices_into(&mut color_vertices, &snapshot, &color_atlas, &runs);
    assert_eq!(color_vertices.len(), grid::VERTS_PER_QUAD);

    let mut frame = Frame::new(
        atlas.cell.width as usize * 2,
        atlas.cell.height as usize,
        [8, 16, 24, 255],
    );
    composite_color_glyphs(&mut frame, &color_atlas, &color_vertices);

    assert!(
        frame.rgba.iter().any(|px| *px != [8, 16, 24, 255]),
        "composited frame should contain live emoji pixels"
    );
}

#[test]
fn vs15_stays_coverage_vs16_enters_color_path() {
    let Some((atlas, mut rasterizer)) = setup() else {
        eprintln!("skipping: text font or Noto Color Emoji unavailable");
        return;
    };
    let mut text_terminal = Terminal::new(1, 1);
    text_terminal.advance("\u{2764}\u{FE0E}".as_bytes());
    let mut emoji_terminal = Terminal::new(1, 1);
    emoji_terminal.advance("\u{2764}\u{FE0F}".as_bytes());

    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let text_runs = rasterizer.build_color_glyph_runs(&text_terminal.snapshot(), &mut color_atlas);
    let emoji_runs =
        rasterizer.build_color_glyph_runs(&emoji_terminal.snapshot(), &mut color_atlas);

    assert!(text_runs.is_empty(), "VS15 forces text/coverage path");
    assert_eq!(emoji_runs.len(), 1, "VS16 forces color path");
}

#[test]
fn multi_codepoint_clusters_render_as_one_cluster_keyed_color_glyph() {
    assert_one_cluster_run("\u{1F44D}\u{1F3FD}", 4);
    assert_one_cluster_run("\u{1F1FA}\u{1F1F8}", 2);
    assert_one_cluster_run(
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
        8,
    );
}

#[test]
fn keycap_cluster_is_color_when_resolved_and_visible_fallback_otherwise() {
    let Some((atlas, mut rasterizer)) = setup() else {
        eprintln!("skipping: text font or Noto Color Emoji unavailable");
        return;
    };
    let snapshot = snapshot_for("1\u{FE0F}\u{20E3}", 4);
    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut color_atlas);

    if let Some(run) = runs.first() {
        assert_eq!(runs.len(), 1);
        assert_eq!(run.covered_columns, 1);
        assert!(matches!(run.key.glyph_id, ColorGlyphId::Cluster(_)));
    } else {
        let mut vertices = Vec::<Vertex>::new();
        grid::build_cell_vertices_with_color_glyph_runs_into(
            &mut vertices,
            &snapshot,
            &atlas,
            &runs,
        );
        assert!(
            vertices.iter().any(|v| v.is_glyph == 1.0),
            "unresolved keycap cluster must fall back to visible coverage text"
        );
    }
}

#[test]
fn unresolved_cluster_keeps_coverage_fallback_visible() {
    let Some((atlas, _)) = setup() else {
        eprintln!("skipping: no system text font available");
        return;
    };
    let snapshot = snapshot_for("1\u{FE0F}\u{20E3}", 4);
    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let mut rasterizer = EmojiRasterizer::new(None);
    let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut color_atlas);
    assert!(runs.is_empty());

    let mut vertices = Vec::<Vertex>::new();
    grid::build_cell_vertices_with_color_glyph_runs_into(&mut vertices, &snapshot, &atlas, &runs);
    assert!(
        vertices.iter().any(|v| v.is_glyph == 1.0),
        "fallback coverage glyph for the keycap base must remain visible"
    );
}

#[test]
fn cluster_run_suppresses_all_covered_source_foregrounds() {
    let Some((atlas, _)) = setup() else {
        eprintln!("skipping: no system text font available");
        return;
    };
    let snapshot = snapshot_for("AB", 4);
    let key = odytty::emoji::ColorGlyphKey::new(1, ColorGlyphId::Cluster(9), 24.0, 1.0);
    let runs = [odytty::grid::ColorGlyphRun::cluster(0, 0, key, 2)];

    let mut plain = Vec::<Vertex>::new();
    grid::build_cell_vertices_into(&mut plain, &snapshot, &atlas);
    let mut color_aware = Vec::<Vertex>::new();
    grid::build_cell_vertices_with_color_glyph_runs_into(
        &mut color_aware,
        &snapshot,
        &atlas,
        &runs,
    );

    assert_eq!(plain.iter().filter(|v| v.is_glyph == 1.0).count(), 12);
    assert!(
        color_aware.iter().all(|v| v.is_glyph == 0.0),
        "cluster coverage suppresses every source cell foreground"
    );
}

#[test]
fn resident_color_run_suppresses_coverage_foreground_quad() {
    let Some((atlas, mut rasterizer)) = setup() else {
        eprintln!("skipping: text font or Noto Color Emoji unavailable");
        return;
    };
    let mut terminal = Terminal::new(2, 1);
    terminal.advance(b"\x1b[?25l");
    terminal.advance("\u{1F525}".as_bytes());
    let snapshot = terminal.snapshot();
    let mut color_atlas = ColorGlyphAtlas::new(atlas.cell);
    let runs = rasterizer.build_color_glyph_runs(&snapshot, &mut color_atlas);
    assert_eq!(runs.len(), 1);

    let mut plain_vertices = Vec::<Vertex>::new();
    grid::build_vertices_with_cursor_into(
        &mut plain_vertices,
        &snapshot,
        &atlas,
        CursorStyle::Block,
    );
    let mut color_aware_vertices = Vec::<Vertex>::new();
    grid::build_cell_vertices_with_color_glyph_runs_into(
        &mut color_aware_vertices,
        &snapshot,
        &atlas,
        &runs,
    );

    assert!(plain_vertices.iter().any(|v| v.is_glyph == 1.0));
    assert!(
        color_aware_vertices.iter().all(|v| v.is_glyph == 0.0),
        "color-aware cell pass keeps backgrounds but suppresses coverage glyph"
    );
}

use odytty::atlas::GlyphAtlas;
use odytty::core::{CursorStyle, Terminal};
use odytty::emoji::{ColorGlyphAtlas, EmojiFont, EmojiRasterizer, discover_noto_color_emoji};
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

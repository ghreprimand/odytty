//! Pixel-level smoke checks (Stage 3: "visual regression / pixel-level smoke
//! checks where practical").
//!
//! A **headless CPU compositor** rasterizes a small terminal grid into an RGBA
//! buffer using the *real* geometry path — `grid::build_vertices*` produces the
//! exact quads the GPU draws, and this file composites them on the CPU with the
//! same painter ordering (all backgrounds first, then glyphs/decorations) and
//! the same straight-alpha blend the `cell.wgsl` fragment shader uses on its
//! default path (text gamma `1.0`, ambient effect off): a glyph pixel's alpha is
//! the atlas R8 coverage, background/solid quads are opaque fills. No GPU, no
//! winit, no window — so it runs in the default `cargo test`.
//!
//! ## Why structural assertions, not byte-exact goldens
//!
//! Rendered pixels depend on whichever monospace face the host actually has
//! (the embedded/system font differs across machines and CI), so a byte-hash
//! golden would be brittle and non-portable. These checks instead assert
//! *structural* invariants that hold for any reasonable monospace font: ink
//! presence within expected bounds, decoration-row presence, the inverse/dim
//! color relationships, blank-cell purity, box-drawing seam continuity, and
//! wide-cell single-draw. A hash-golden layer could be added on top (the task
//! permits it as an optional extra) but is deliberately omitted to keep the
//! suite portable; the structural layer is the durable contract.
//!
//! Every case skips gracefully (prints and returns) when no system font is
//! available, matching the rest of the suite's hermeticity.

use odytty::atlas::{GlyphAtlas, SubpixelMode};
use odytty::core::{CursorStyle, Snapshot, Terminal};
use odytty::grid::{self, Vertex};
use odytty::text::{self, foreground_linear};

use ab_glyph::FontVec;

/// Build size for the test atlas. Large enough that decoration rows and glyph
/// ink are several pixels tall (robust thresholds), small enough to stay fast.
const PX: f32 = 28.0;

/// A composited linear-RGB frame: `width * height` pixels, row-major, opaque.
struct Frame {
    width: usize,
    height: usize,
    /// Linear RGB per pixel (alpha is always 1.0 after the opaque clear).
    px: Vec<[f32; 3]>,
    cell_w: usize,
    cell_h: usize,
}

impl Frame {
    fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        self.px[y * self.width + x]
    }

    /// Inclusive-exclusive pixel bounds of cell `(col, row)`.
    fn cell_bounds(&self, col: usize, row: usize) -> (usize, usize, usize, usize) {
        let x0 = col * self.cell_w;
        let y0 = row * self.cell_h;
        (x0, y0, x0 + self.cell_w, y0 + self.cell_h)
    }
}

/// Relative luminance of a linear-RGB pixel (Rec. 709 coefficients).
fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Whether two colors differ enough to count as a visible change.
fn differs(a: [f32; 3], b: [f32; 3]) -> bool {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() > 0.02
}

/// The default background as linear RGB (the surface clear color).
fn default_bg() -> [f32; 3] {
    let b = text::background_linear(odytty::core::Color::Default);
    [b[0], b[1], b[2]]
}

/// Composite a snapshot into a `Frame` using the real grid geometry and the
/// shader's default-path blend. `cursor_style` mirrors the renderer's DECSCUSR
/// handling; pass `Block` for the default path.
fn composite(snapshot: &Snapshot, atlas: &GlyphAtlas, cursor_style: CursorStyle) -> Frame {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as usize;
    let cell_h = atlas.cell.height as usize;
    let width = cols * cell_w;
    let height = rows * cell_h;

    let mut frame = Frame {
        width,
        height,
        px: vec![default_bg(); width * height],
        cell_w,
        cell_h,
    };

    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, snapshot, atlas, cursor_style);

    // Each quad is 6 vertices (tl, bl, tr, tr, bl, br); the axis-aligned rect is
    // recoverable from the first (top-left) and last (bottom-right) vertices.
    for quad in verts.chunks_exact(grid::VERTS_PER_QUAD) {
        composite_quad(&mut frame, atlas, quad);
    }
    frame
}

/// Composite one axis-aligned quad (background, glyph, or solid decoration).
fn composite_quad(frame: &mut Frame, atlas: &GlyphAtlas, quad: &[Vertex]) {
    let tl = &quad[0];
    let br = &quad[5];
    let x0 = tl.pos[0];
    let y0 = tl.pos[1];
    let x1 = br.pos[0];
    let y1 = br.pos[1];
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let color = [tl.color[0], tl.color[1], tl.color[2]];
    let color_a = tl.color[3];
    let is_glyph = tl.is_glyph > 0.5;

    // Pixel range: include pixel p when its center p+0.5 falls inside the rect.
    let px0 = x0.floor().max(0.0) as usize;
    let py0 = y0.floor().max(0.0) as usize;
    let px1 = (x1.ceil() as usize).min(frame.width);
    let py1 = (y1.ceil() as usize).min(frame.height);

    for py in py0..py1 {
        let cy = py as f32 + 0.5;
        if cy < y0 || cy >= y1 {
            continue;
        }
        for px in px0..px1 {
            let cx = px as f32 + 0.5;
            if cx < x0 || cx >= x1 {
                continue;
            }
            // Alpha/coverage: opaque for solids; coverage-modulated for glyphs.
            // The grayscale default returns the same scalar for RGB. Subpixel
            // atlases return independent RGB coverage, matching the dual-source
            // shader's per-channel destination weights.
            let alpha = if is_glyph {
                let u0 = tl.uv[0];
                let v0 = tl.uv[1];
                let u1 = br.uv[0];
                let v1 = br.uv[1];
                let fx = (cx - x0) / (x1 - x0);
                let fy = (cy - y0) / (y1 - y0);
                let u = u0 + fx * (u1 - u0);
                let v = v0 + fy * (v1 - v0);
                let ax =
                    ((u * atlas.width as f32) as i64).clamp(0, atlas.width as i64 - 1) as usize;
                let ay =
                    ((v * atlas.height as f32) as i64).clamp(0, atlas.height as i64 - 1) as usize;
                atlas_coverage_rgb(atlas, ax, ay).map(|coverage| color_a * coverage)
            } else {
                [color_a; 3]
            };
            if alpha.iter().all(|&a| a <= 0.0) {
                continue;
            }
            let idx = py * frame.width + px;
            let dst = frame.px[idx];
            frame.px[idx] = [
                color[0] * alpha[0] + dst[0] * (1.0 - alpha[0]),
                color[1] * alpha[1] + dst[1] * (1.0 - alpha[1]),
                color[2] * alpha[2] + dst[2] * (1.0 - alpha[2]),
            ];
        }
    }
}

fn atlas_coverage_rgb(atlas: &GlyphAtlas, x: usize, y: usize) -> [f32; 3] {
    match atlas.subpixel_mode() {
        SubpixelMode::Off => {
            let coverage = atlas.data[y * atlas.width as usize + x] as f32 / 255.0;
            [coverage; 3]
        }
        SubpixelMode::Rgb | SubpixelMode::Bgr => {
            let idx = (y * atlas.width as usize + x) * 4;
            [
                atlas.data[idx] as f32 / 255.0,
                atlas.data[idx + 1] as f32 / 255.0,
                atlas.data[idx + 2] as f32 / 255.0,
            ]
        }
    }
}

/// Load the system font + a build-size atlas, or `None` to skip the test.
fn setup() -> Option<(FontVec, GlyphAtlas)> {
    let font = text::load_font().ok()?;
    let atlas = GlyphAtlas::build(&font, PX);
    Some((font, atlas))
}

fn setup_subpixel() -> Option<(FontVec, GlyphAtlas)> {
    let font = text::load_font().ok()?;
    let atlas = GlyphAtlas::build_with_subpixel(&font, PX, SubpixelMode::Rgb);
    Some((font, atlas))
}

/// Snapshot for a 1-row grid with `text` typed into it, cursor hidden so only
/// cell geometry is composited.
fn row_snapshot(cols: usize, text: &str) -> Snapshot {
    let mut term = Terminal::new(cols, 1);
    term.advance(b"\x1b[?25l");
    term.advance(text.as_bytes());
    term.snapshot()
}

/// Count inked pixels (differ from the default background) inside a cell.
fn cell_ink_count(frame: &Frame, col: usize, row: usize) -> usize {
    let bg = default_bg();
    let (x0, y0, x1, y1) = frame.cell_bounds(col, row);
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if differs(frame.pixel(x, y), bg) {
                n += 1;
            }
        }
    }
    n
}

/// The modal (most common) quantized color inside a cell — dominated by the
/// background fill, since glyph ink is a minority of cell pixels.
fn cell_modal_color(frame: &Frame, col: usize, row: usize) -> [u8; 3] {
    use std::collections::HashMap;
    let (x0, y0, x1, y1) = frame.cell_bounds(col, row);
    let mut counts: HashMap<[u8; 3], usize> = HashMap::new();
    for y in y0..y1 {
        for x in x0..x1 {
            *counts.entry(quant3(frame.pixel(x, y))).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, n)| n)
        .map(|(c, _)| c)
        .unwrap_or([0, 0, 0])
}

fn quant3(c: [f32; 3]) -> [u8; 3] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn quant(c: [f32; 4]) -> [u8; 3] {
    quant3([c[0], c[1], c[2]])
}

/// Resolve a non-ASCII char into the atlas, returning `false` when the font
/// lacks it (so the caller skips). Detected by comparing the ensured UV against
/// the fallback box that an unmistakably-absent private-use codepoint yields.
fn ensure_real_glyph(atlas: &mut GlyphAtlas, font: &FontVec, ch: char) -> bool {
    let fallback = atlas.uv_rect('\u{E000}'); // private-use: no font ships it
    let got = atlas.ensure(font, ch);
    got.is_some() && got != fallback
}

/// A width-2 (East Asian) codepoint the loaded font has a real outline for, used
/// to exercise the wide-slot raster path. Returns `None` on hosts without a
/// CJK/fullwidth-capable font (the common case here), so the caller skips. Width
/// is decided with the same `unicode-width` rule core uses for cell layout.
fn find_supported_wide_glyph(atlas: &mut GlyphAtlas, font: &FontVec) -> Option<char> {
    use unicode_width::UnicodeWidthChar;
    let ranges = [0x4E00u32..=0x4F00, 0x3040..=0x30FF, 0xFF01..=0xFF60];
    ranges
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
        .find(|&ch| UnicodeWidthChar::width(ch) == Some(2) && ensure_real_glyph(atlas, font, ch))
}

#[test]
fn blank_cell_renders_pure_background() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // All-spaces grid, cursor hidden: every pixel must be the exact background.
    let snapshot = row_snapshot(4, "");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let bg = default_bg();
    for y in 0..frame.height {
        for x in 0..frame.width {
            assert!(
                !differs(frame.pixel(x, y), bg),
                "blank grid pixel ({x},{y}) should be pure background"
            );
        }
    }
}

#[test]
fn known_glyph_inks_within_its_cell() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // 'H' in column 0, blanks elsewhere. 'H' is a plain ASCII glyph with no side
    // bearing overflow, so its ink stays inside cell 0; neighbor cells stay blank.
    let snapshot = row_snapshot(4, "H");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "'H' should leave visible ink in its own cell"
    );
    // A column two cells away must be untouched (no stray ink / no bleed).
    assert_eq!(
        cell_ink_count(&frame, 2, 0),
        0,
        "ink must not bleed two cells away from 'H'"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "rightmost cell must stay blank"
    );
}

#[test]
fn subpixel_atlas_composites_known_glyph() {
    let Some((_font, atlas)) = setup_subpixel() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(4, "H");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    assert!(
        cell_ink_count(&frame, 0, 0) > 0,
        "subpixel atlas should leave visible ink in the glyph cell"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "subpixel glyph coverage must not bleed into distant blank cells"
    );
}

#[test]
fn inverse_swaps_foreground_and_background_fill() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Normal 'X': the cell is dominated by the background fill.
    let normal = composite(&row_snapshot(2, "X"), &atlas, CursorStyle::Block);
    // Inverse 'X': the cell fill becomes the foreground color.
    let inverse = composite(
        &row_snapshot(2, "\x1b[7mX\x1b[0m"),
        &atlas,
        CursorStyle::Block,
    );

    let fg = quant(foreground_linear(odytty::core::Color::Default));
    let bg = quant(text::background_linear(odytty::core::Color::Default));

    assert_eq!(
        cell_modal_color(&normal, 0, 0),
        bg,
        "normal cell fill should be the background color"
    );
    assert_eq!(
        cell_modal_color(&inverse, 0, 0),
        fg,
        "inverse cell fill should be the foreground color"
    );
}

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
fn underline_attribute_inks_a_decoration_row() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(1, "\x1b[4mU");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    // The documented underline rect; assert that its row band is fully inked
    // across the cell width (a continuous foreground bar).
    let rect = grid::underline_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    let row = (((rect[1] + rect[3]) / 2.0) as usize).min(frame.height - 1);
    let bg = default_bg();
    let inked: usize = (0..frame.cell_w)
        .filter(|&x| differs(frame.pixel(x, row), bg))
        .count();
    assert!(
        inked >= frame.cell_w - 1,
        "underline row {row} should be inked across the full cell width (got {inked}/{})",
        frame.cell_w
    );
}

#[test]
fn strikethrough_attribute_inks_a_decoration_row() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let snapshot = row_snapshot(1, "\x1b[9mS");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);

    let rect = grid::strikethrough_rect(
        0.0,
        0.0,
        atlas.cell.width as f32,
        atlas.cell.height as f32,
        atlas.cell.baseline as f32,
    );
    let row = (((rect[1] + rect[3]) / 2.0) as usize).min(frame.height - 1);
    let bg = default_bg();
    let inked: usize = (0..frame.cell_w)
        .filter(|&x| differs(frame.pixel(x, row), bg))
        .count();
    assert!(
        inked >= frame.cell_w - 1,
        "strikethrough row {row} should be inked across the full cell width (got {inked}/{})",
        frame.cell_w
    );
}

#[test]
fn box_drawing_horizontal_joins_across_the_cell_seam() {
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // U+2500 BOX DRAWINGS LIGHT HORIZONTAL in two adjacent cells. The classic
    // seam-continuity check: the horizontal stroke must run unbroken across the
    // boundary between the two cells with no blank column at the join.
    if !ensure_real_glyph(&mut atlas, &font, '\u{2500}') {
        eprintln!("skipping: font lacks U+2500 box-drawing glyph");
        return;
    }
    let snapshot = row_snapshot(2, "\u{2500}\u{2500}");
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let bg = default_bg();

    // Find a row where the stroke inks in the left cell.
    let mut stroke_row = None;
    for y in 0..frame.height {
        if differs(frame.pixel(frame.cell_w / 2, y), bg) {
            stroke_row = Some(y);
            break;
        }
    }
    let Some(row) = stroke_row else {
        panic!("U+2500 produced no horizontal stroke ink");
    };

    // Across the full two-cell width, every column on the stroke row must be
    // inked — no gap, including the two columns straddling the seam.
    let mut gaps = Vec::new();
    for x in 0..frame.width {
        if !differs(frame.pixel(x, row), bg) {
            gaps.push(x);
        }
    }
    assert!(
        gaps.is_empty(),
        "box-drawing stroke row {row} has blank columns {gaps:?} (seam discontinuity)"
    );
}

#[test]
fn wide_char_spans_two_cells_without_double_draw() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // A wide char occupies a lead cell + a continuation spacer. Whether the font
    // has the glyph or renders the fallback box, exactly one glyph quad is
    // emitted for the pair (the spacer draws nothing), and ink stays within the
    // two-cell span. '世' is East-Asian Wide.
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    term.advance("世".as_bytes());
    let snapshot = term.snapshot();
    assert!(
        snapshot.cells[1].wide_continuation,
        "precondition: column 1 is a wide continuation spacer"
    );

    // Vertex-level: exactly one glyph quad across the whole grid (no double-draw
    // from the continuation cell).
    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, CursorStyle::Block);
    let glyph_quads = verts
        .chunks_exact(grid::VERTS_PER_QUAD)
        .filter(|q| q[0].is_glyph > 0.5)
        .count();
    assert_eq!(
        glyph_quads, 1,
        "a wide char must emit exactly one glyph quad, not one per cell"
    );

    // Pixel-level: ink exists in the spanned region and nowhere past it (the two
    // trailing blank cells stay pure background).
    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    let span_ink = cell_ink_count(&frame, 0, 0) + cell_ink_count(&frame, 1, 0);
    assert!(
        span_ink > 0,
        "wide glyph should leave ink across its two cells"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "ink must not extend past the wide char's two-cell span"
    );
}

#[test]
fn wide_glyph_inks_across_the_seam_when_supported() {
    // W1: with a CJK/fullwidth-capable font, a width-2 glyph rasterizes into a
    // two-cell slot and inks across the cell seam — not clipped at the lead
    // cell's right edge. Skips on hosts without such a font (the validation host
    // here), where the always-running atlas unit test
    // `rasterize_clip_width_relieves_wide_glyph_clipping` proves the mechanism.
    let Some((font, mut atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    let Some(wide) = find_supported_wide_glyph(&mut atlas, &font) else {
        eprintln!("skipping: no wide (CJK/fullwidth) glyph in the loaded font");
        return;
    };

    // Grid: wide lead at cols 0-1, a narrow neighbour at col 2, blank at col 3.
    let mut term = Terminal::new(4, 1);
    term.advance(b"\x1b[?25l");
    term.advance(wide.to_string().as_bytes());
    term.advance(b"M");
    let snapshot = term.snapshot();
    assert!(
        snapshot.cells[1].wide_continuation,
        "precondition: column 1 is a wide continuation spacer"
    );
    assert!(!snapshot.cells[2].wide_continuation && snapshot.cells[2].ch == 'M');

    // Exactly one glyph quad for the wide pair (no continuation double-draw),
    // plus one for the narrow neighbour = 2 glyph quads total.
    let mut verts = Vec::new();
    grid::build_vertices_with_cursor_into(&mut verts, &snapshot, &atlas, CursorStyle::Block);
    let glyph_quads = verts
        .chunks_exact(grid::VERTS_PER_QUAD)
        .filter(|q| q[0].is_glyph > 0.5)
        .count();
    assert_eq!(
        glyph_quads, 2,
        "wide char + narrow neighbour must emit exactly two glyph quads"
    );

    let frame = composite(&snapshot, &atlas, CursorStyle::Block);
    // Ink crosses the seam: BOTH the lead cell and the continuation cell carry
    // real glyph ink (a clipped single-cell glyph would leave cell 1 near-empty).
    let lead_ink = cell_ink_count(&frame, 0, 0);
    let cont_ink = cell_ink_count(&frame, 1, 0);
    assert!(lead_ink > 0, "wide glyph lead cell should hold ink");
    assert!(
        cont_ink > 0,
        "wide glyph must ink across the seam into its continuation cell, not clip"
    );
    // Narrow neighbour unaffected: it has its own ink and the wide glyph does not
    // bleed past its two-cell span into the trailing blank.
    assert!(
        cell_ink_count(&frame, 2, 0) > 0,
        "narrow neighbour should render"
    );
    assert_eq!(
        cell_ink_count(&frame, 3, 0),
        0,
        "ink must not extend past the wide span + neighbour"
    );
}

#[test]
fn bar_cursor_inks_only_a_thin_left_stripe() {
    let Some((_font, atlas)) = setup() else {
        eprintln!("skipping: no system font available");
        return;
    };
    // Visible bar cursor on a blank cell at (0,0): a thin vertical stripe at the
    // cell's left edge, not a full-cell block. Assert the left column inks while
    // the right half of the cell stays background.
    let term = Terminal::new(2, 1);
    let snapshot = term.snapshot();
    let frame = composite(&snapshot, &atlas, CursorStyle::Bar);
    let bg = default_bg();

    let mid_y = frame.cell_h / 2;
    assert!(
        differs(frame.pixel(0, mid_y), bg),
        "bar cursor should ink the cell's left edge"
    );
    let right_x = frame.cell_w - 1;
    assert!(
        !differs(frame.pixel(right_x, mid_y), bg),
        "bar cursor must not fill the right side of the cell"
    );
}

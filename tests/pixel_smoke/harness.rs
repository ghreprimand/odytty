// SPDX-License-Identifier: GPL-3.0-only
//! Shared headless CPU compositor and helpers for the `pixel_smoke` suite.
//!
//! The compositor rasterizes a small terminal grid into a linear-RGB buffer
//! using the *real* geometry path — `grid::build_vertices*` produces the exact
//! quads the GPU draws, composited here on the CPU with the same painter
//! ordering (all backgrounds first, then glyphs/decorations) and the same
//! straight-alpha blend the `cell.wgsl` fragment shader uses on its default
//! path. No GPU, no winit, no window — so it runs in the default `cargo test`.

use odytty::atlas::{FontStyle, GlyphAtlas, SubpixelMode};
use odytty::core::{CursorStyle, Snapshot, Terminal};
use odytty::grid::{self, Vertex};
use odytty::text;

use ab_glyph::FontVec;

/// Build size for the test atlas. Large enough that decoration rows and glyph
/// ink are several pixels tall (robust thresholds), small enough to stay fast.
pub(crate) const PX: f32 = 28.0;

/// A composited linear-RGB frame: `width * height` pixels, row-major, opaque.
pub(crate) struct Frame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// Linear RGB per pixel (alpha is always 1.0 after the opaque clear).
    pub(crate) px: Vec<[f32; 3]>,
    pub(crate) cell_w: usize,
    pub(crate) cell_h: usize,
}

impl Frame {
    pub(crate) fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        self.px[y * self.width + x]
    }

    /// Inclusive-exclusive pixel bounds of cell `(col, row)`.
    pub(crate) fn cell_bounds(&self, col: usize, row: usize) -> (usize, usize, usize, usize) {
        let x0 = col * self.cell_w;
        let y0 = row * self.cell_h;
        (x0, y0, x0 + self.cell_w, y0 + self.cell_h)
    }
}

/// Relative luminance of a linear-RGB pixel (Rec. 709 coefficients).
pub(crate) fn luminance(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Whether two colors differ enough to count as a visible change.
pub(crate) fn differs(a: [f32; 3], b: [f32; 3]) -> bool {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() > 0.02
}

/// The default background as linear RGB (the surface clear color).
pub(crate) fn default_bg() -> [f32; 3] {
    let b = text::background_linear(odytty::core::Color::Default);
    [b[0], b[1], b[2]]
}

/// Composite a snapshot into a `Frame` using the real grid geometry and the
/// shader's default-path blend. `cursor_style` mirrors the renderer's DECSCUSR
/// handling; pass `Block` for the default path.
pub(crate) fn composite(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
) -> Frame {
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

/// Composite a frame driving the ID2 focus-dim amount through the real geometry
/// path — the same `build_cell_vertices_with_color_glyph_runs_into` seam the
/// native renderer uses for an unfocused window. `focus_dim == 0.0` reproduces
/// the focused render byte-for-byte (the off-path gate).
pub(crate) fn composite_focus_dim(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    focus_dim: f32,
) -> Frame {
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
    grid::build_cell_vertices_with_focus_dim_into(
        &mut verts,
        snapshot,
        atlas,
        &[],
        focus_dim,
        grid::BackgroundTreatmentParams::default(),
    );
    grid::append_cursor_vertices(&mut verts, snapshot, atlas, cursor_style);

    for quad in verts.chunks_exact(grid::VERTS_PER_QUAD) {
        composite_quad(&mut frame, atlas, quad);
    }
    frame
}

pub(crate) fn composite_with_padding(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    padding_px: usize,
) -> Frame {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as usize;
    let cell_h = atlas.cell.height as usize;
    let width = cols * cell_w + padding_px * 2;
    let height = rows * cell_h + padding_px * 2;

    let mut frame = Frame {
        width,
        height,
        px: vec![default_bg(); width * height],
        cell_w,
        cell_h,
    };

    let origin = [padding_px as f32, padding_px as f32];
    let mut verts = Vec::new();
    grid::build_cell_vertices_with_focus_dim_and_origin_into(
        &mut verts,
        snapshot,
        atlas,
        &[],
        0.0,
        origin,
        grid::BackgroundTreatmentParams::default(),
        // Identity opacity: padding smoke keeps cells fully opaque.
        1.0,
        None,
        grid::ChromePin::NONE,
    );
    grid::append_cursor_vertices_with_origin(
        &mut verts,
        snapshot,
        atlas,
        cursor_style,
        origin,
        grid::CursorRenderParams::default(),
    );

    for quad in verts.chunks_exact(grid::VERTS_PER_QUAD) {
        composite_quad(&mut frame, atlas, quad);
    }
    frame
}

/// Composite a frame driving an ID3/U5 background treatment through the real
/// geometry path — the same `build_cell_vertices_with_focus_dim_into` seam the
/// native renderer uses. The default (inactive) `BackgroundTreatmentParams`
/// reproduces the plain render byte-for-byte (the off-path gate).
pub(crate) fn composite_background_treatment(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    treatment: grid::BackgroundTreatmentParams,
) -> Frame {
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
    grid::build_cell_vertices_with_focus_dim_into(&mut verts, snapshot, atlas, &[], 0.0, treatment);
    grid::append_cursor_vertices(&mut verts, snapshot, atlas, cursor_style);

    for quad in verts.chunks_exact(grid::VERTS_PER_QUAD) {
        composite_quad(&mut frame, atlas, quad);
    }
    frame
}

/// ID3/U5 image-background compositor. Models the native draw order: the
/// scrimmed background image is drawn first (here, a uniform fill of
/// `image_rgb`, standing in for the post-scrim image pass), then the cell
/// background quads composite on top with their alpha scaled by
/// `cell_bg_opacity` — exactly the `bg[3] *= cell_bg_opacity` path in the grid
/// cell-vertex builder. At `cell_bg_opacity = 1.0` the cells are fully opaque
/// and the image is hidden behind them (the off-path identity); below 1.0 the
/// image shows through behind text.
pub(crate) fn composite_background_image(
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    cell_bg_opacity: f32,
    image_rgb: [f32; 3],
) -> Frame {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as usize;
    let cell_h = atlas.cell.height as usize;
    let width = cols * cell_w;
    let height = rows * cell_h;

    // The frame starts as the (scrimmed) background image — the image pass draws
    // first, behind every cell quad.
    let mut frame = Frame {
        width,
        height,
        px: vec![image_rgb; width * height],
        cell_w,
        cell_h,
    };

    let mut verts = Vec::new();
    grid::build_cell_vertices_with_focus_dim_and_origin_into(
        &mut verts,
        snapshot,
        atlas,
        &[],
        0.0,
        [0.0, 0.0],
        grid::BackgroundTreatmentParams::default(),
        cell_bg_opacity,
        None,
        grid::ChromePin::NONE,
    );
    grid::append_cursor_vertices(&mut verts, snapshot, atlas, cursor_style);

    for quad in verts.chunks_exact(grid::VERTS_PER_QUAD) {
        composite_quad(&mut frame, atlas, quad);
    }
    frame
}

/// Composite one axis-aligned quad (background, glyph, or solid decoration).
pub(crate) fn composite_quad(frame: &mut Frame, atlas: &GlyphAtlas, quad: &[Vertex]) {
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

pub(crate) fn atlas_coverage_rgb(atlas: &GlyphAtlas, x: usize, y: usize) -> [f32; 3] {
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
pub(crate) fn setup() -> Option<(FontVec, GlyphAtlas)> {
    let font = text::load_font().ok()?;
    let atlas = GlyphAtlas::build(&font, PX);
    Some((font, atlas))
}

pub(crate) fn setup_subpixel() -> Option<(FontVec, GlyphAtlas)> {
    let font = text::load_font().ok()?;
    let atlas = GlyphAtlas::build_with_subpixel(&font, PX, SubpixelMode::Rgb);
    Some((font, atlas))
}

/// Snapshot for a 1-row grid with `text` typed into it, cursor hidden so only
/// cell geometry is composited.
pub(crate) fn row_snapshot(cols: usize, text: &str) -> Snapshot {
    let mut term = Terminal::new(cols, 1);
    term.advance(b"\x1b[?25l");
    term.advance(text.as_bytes());
    term.snapshot()
}

/// Like [`row_snapshot`] but applies an SGR prefix (e.g. `b"\x1b[1m"` for bold,
/// `b"\x1b[3m"` for italic) so the cells carry the corresponding attribute and
/// the grid resolves them through the matching [`FontStyle`].
pub(crate) fn styled_row_snapshot(cols: usize, sgr: &[u8], text: &str) -> Snapshot {
    let mut term = Terminal::new(cols, 1);
    term.advance(b"\x1b[?25l");
    term.advance(sgr);
    term.advance(text.as_bytes());
    term.snapshot()
}

/// Resolve every char of `text` into the atlas at the given style, so the
/// immutable composite lookup finds resident slots instead of the fallback box.
pub(crate) fn ensure_styled_row(
    atlas: &mut GlyphAtlas,
    font: &FontVec,
    style: FontStyle,
    text: &str,
) {
    for ch in text.chars() {
        let _ = atlas.ensure_styled(font, style, ch);
    }
}

/// Count inked pixels (differ from the default background) inside a cell.
pub(crate) fn cell_ink_count(frame: &Frame, col: usize, row: usize) -> usize {
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

/// Mean inked-pixel x (cell-local) in the top quarter of row 0 minus that in
/// the bottom quarter, aggregated over `cols` cells. Positive means the ink
/// above the baseline sits right of the ink below it — the signature of a
/// right-leaning oblique. `None` if either band has no ink to measure.
pub(crate) fn row_top_minus_bottom_centroid(frame: &Frame, cols: usize) -> Option<f64> {
    let bg = default_bg();
    let (mut top_sum, mut top_n) = (0f64, 0u64);
    let (mut bot_sum, mut bot_n) = (0f64, 0u64);
    for c in 0..cols {
        let (x0, y0, x1, y1) = frame.cell_bounds(c, 0);
        let q = ((y1 - y0) / 4).max(1);
        for y in y0..y0 + q {
            for x in x0..x1 {
                if differs(frame.pixel(x, y), bg) {
                    top_sum += (x - x0) as f64;
                    top_n += 1;
                }
            }
        }
        for y in (y1 - q)..y1 {
            for x in x0..x1 {
                if differs(frame.pixel(x, y), bg) {
                    bot_sum += (x - x0) as f64;
                    bot_n += 1;
                }
            }
        }
    }
    if top_n == 0 || bot_n == 0 {
        return None;
    }
    Some(top_sum / top_n as f64 - bot_sum / bot_n as f64)
}

/// The modal (most common) quantized color inside a cell — dominated by the
/// background fill, since glyph ink is a minority of cell pixels.
pub(crate) fn cell_modal_color(frame: &Frame, col: usize, row: usize) -> [u8; 3] {
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

pub(crate) fn quant3(c: [f32; 3]) -> [u8; 3] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

pub(crate) fn quant(c: [f32; 4]) -> [u8; 3] {
    quant3([c[0], c[1], c[2]])
}

pub(crate) fn frames_match(a: &Frame, b: &Frame) -> bool {
    a.width == b.width && a.height == b.height && a.px == b.px
}

/// Resolve a non-ASCII char into the atlas, returning `false` when the font
/// lacks it (so the caller skips). Detected by comparing the ensured UV against
/// the fallback box that an unmistakably-absent private-use codepoint yields.
pub(crate) fn ensure_real_glyph(atlas: &mut GlyphAtlas, font: &FontVec, ch: char) -> bool {
    let fallback = atlas.uv_rect('\u{E000}'); // private-use: no font ships it
    let got = atlas.ensure(font, ch);
    got.is_some() && got != fallback
}

/// A width-2 (East Asian) codepoint the loaded font has a real outline for, used
/// to exercise the wide-slot raster path. Returns `None` on hosts without a
/// CJK/fullwidth-capable font (the common case here), so the caller skips. Width
/// is decided with the same `unicode-width` rule core uses for cell layout.
pub(crate) fn find_supported_wide_glyph(atlas: &mut GlyphAtlas, font: &FontVec) -> Option<char> {
    use unicode_width::UnicodeWidthChar;
    let ranges = [0x4E00u32..=0x4F00, 0x3040..=0x30FF, 0xFF01..=0xFF60];
    ranges
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
        .find(|&ch| UnicodeWidthChar::width(ch) == Some(2) && ensure_real_glyph(atlas, font, ch))
}

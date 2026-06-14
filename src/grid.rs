// SPDX-License-Identifier: GPL-3.0-only
//! GPU-agnostic cell geometry: turn a terminal [`Snapshot`] into textured quads.
//!
//! This module is the seam between terminal *semantics* (the owned core) and
//! *pixels* (the `wgpu` renderer). It is deliberately free of any GPU types so
//! it can be unit-tested without a window or device: the native renderer
//! (`crate::native`) uploads the vertex buffer this produces and draws it with
//! the shared `src/shaders/cell.wgsl` pipeline.
//!
//! ## What it produces
//!
//! For every non-continuation cell of a snapshot it emits a **background quad**
//! (two triangles) covering the cell's pixel rectangle, and — when the cell
//! holds an inked, printable glyph — a **foreground quad** with the glyph's UV
//! rectangle from the atlas. Foreground quads carry `is_glyph = 1.0` so the
//! fragment shader samples the R8 coverage atlas as alpha; background quads
//! carry `is_glyph = 0.0` and use their solid color directly.
//!
//! Geometry is built in **physical pixel space** using the atlas cell metrics.
//! The vertex shader converts pixels to NDC via the viewport-size uniform, so a
//! window resize only updates that uniform — the geometry here never needs to
//! be rebuilt for a resize (only when the snapshot content changes).

use bytemuck::{Pod, Zeroable};

use crate::atlas::GlyphBounds;
use crate::core::{Attrs, Color, CursorStyle, DynamicColors, RgbColor, Snapshot, UnderlineStyle};
use crate::emoji::{ColorGlyphAtlas, ColorGlyphKey};
use crate::text::{self, FontStyle, GlyphAtlas};

/// One vertex of a cell quad. Matches the `VsIn` layout in `cell.wgsl`.
///
/// `#[repr(C)]` with no implicit padding (8 + 8 + 16 + 4 + 12 = 48 bytes, all
/// 4-byte-aligned `f32`s) so it is `Pod`/`Zeroable` and can be uploaded
/// straight into a GPU buffer. `_pad` rounds the struct to a 16-byte multiple
/// and keeps the layout explicit.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    /// Position in physical pixels, origin top-left.
    pub pos: [f32; 2],
    /// Atlas UV coordinates (only meaningful for glyph quads).
    pub uv: [f32; 2],
    /// Linear-RGBA color (background fill, or glyph tint).
    pub color: [f32; 4],
    /// `1.0` for glyph quads (sample atlas as alpha), `0.0` for backgrounds.
    pub is_glyph: f32,
    /// Padding to a 16-byte stride multiple; never read by the shader.
    pub _pad: [f32; 3],
}

impl Vertex {
    fn new(pos: [f32; 2], uv: [f32; 2], color: [f32; 4], is_glyph: f32) -> Self {
        Self {
            pos,
            uv,
            color,
            is_glyph,
            _pad: [0.0; 3],
        }
    }
}

/// Number of vertices per quad (two triangles).
pub const VERTS_PER_QUAD: usize = 6;
/// OKLab dim amount for the SGR-dim/faint attribute, chosen for perceived
/// parity with the historical linear ×0.5 halving. OKLab lightness scales as
/// the cube root of linear luminance, so the old linear ×0.5 lowered perceived
/// lightness to `0.5^(1/3) ≈ 0.7937` of the original; matching that means
/// scaling OKLab L by the same factor, i.e. an amount of `1 - 0.5^(1/3) ≈
/// 0.2063`. Using [`crate::color::dim_perceptual`] at this amount keeps the
/// established dim *brightness* while upgrading the model to be hue-preserving
/// and chroma-aware (dimmer light desaturates), unlike the old per-channel
/// linear scale which could skew hue.
const DIM_PERCEPTUAL_AMOUNT: f32 = 0.206_299_47;
const LINE_DECORATION_THICKNESS_DIVISOR: f32 = 16.0;

/// A solid pixel-space overlay quad appended after terminal-cell geometry.
///
/// Native uses this for presentation-only overlays that do not need glyph atlas
/// sampling, such as the scrollback position indicator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolidQuad {
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

/// One vertex of a premultiplied-RGBA color glyph quad.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct ColorGlyphVertex {
    /// Position in physical pixels, origin top-left.
    pub pos: [f32; 2],
    /// Color glyph atlas UV coordinates.
    pub uv: [f32; 2],
}

impl ColorGlyphVertex {
    fn new(pos: [f32; 2], uv: [f32; 2]) -> Self {
        Self { pos, uv }
    }
}

/// A shaped color glyph placed on a snapshot lead cell.
///
/// The key comes from shaping/rasterization, not from the cell's `char`; EM3
/// tests supply synthetic keys and EM4 will supply real swash glyph/cluster ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorGlyphRun {
    pub row: usize,
    pub column: usize,
    pub key: ColorGlyphKey,
    /// Number of source grid columns whose foreground glyphs are replaced by
    /// this run. Multi-codepoint emoji clusters can be stored across several
    /// cells even though they rasterize to one 1- or 2-cell color bitmap.
    pub covered_columns: u8,
}

impl ColorGlyphRun {
    pub fn new(row: usize, column: usize, key: ColorGlyphKey) -> Self {
        Self {
            row,
            column,
            key,
            covered_columns: 1,
        }
    }

    pub fn cluster(row: usize, column: usize, key: ColorGlyphKey, covered_columns: u8) -> Self {
        Self {
            row,
            column,
            key,
            covered_columns: covered_columns.max(1),
        }
    }

    pub fn covers(self, row: usize, column: usize) -> bool {
        self.row == row
            && column >= self.column
            && column < self.column + self.covered_columns as usize
    }
}

/// Push a pixel-space rectangle as two triangles into `out`.
///
/// `rect` is `[x0, y0, x1, y1]` in pixels; `uv` is `[u0, v0, u1, v1]`. For
/// background quads `uv` is ignored by the shader but still written so every
/// vertex has a defined value. Triangles are emitted with no particular winding
/// because the pipeline disables face culling.
fn push_quad(out: &mut Vec<Vertex>, rect: [f32; 4], uv: [f32; 4], color: [f32; 4], is_glyph: f32) {
    let [x0, y0, x1, y1] = rect;
    let [u0, v0, u1, v1] = uv;
    let tl = Vertex::new([x0, y0], [u0, v0], color, is_glyph);
    let tr = Vertex::new([x1, y0], [u1, v0], color, is_glyph);
    let bl = Vertex::new([x0, y1], [u0, v1], color, is_glyph);
    let br = Vertex::new([x1, y1], [u1, v1], color, is_glyph);
    out.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
}

/// Append one solid, non-glyph quad to an existing vertex list.
pub fn push_solid_quad(out: &mut Vec<Vertex>, quad: SolidQuad) {
    push_quad(out, quad.rect, [0.0, 0.0, 0.0, 0.0], quad.color, 0.0);
}

pub fn push_solid_quad_with_origin(out: &mut Vec<Vertex>, quad: SolidQuad, origin: [f32; 2]) {
    let rect = [
        quad.rect[0] + origin[0],
        quad.rect[1] + origin[1],
        quad.rect[2] + origin[0],
        quad.rect[3] + origin[1],
    ];
    push_quad(out, rect, [0.0, 0.0, 0.0, 0.0], quad.color, 0.0);
}

fn push_color_glyph_quad(out: &mut Vec<ColorGlyphVertex>, rect: [f32; 4], uv: [f32; 4]) {
    let [x0, y0, x1, y1] = rect;
    let [u0, v0, u1, v1] = uv;
    let tl = ColorGlyphVertex::new([x0, y0], [u0, v0]);
    let tr = ColorGlyphVertex::new([x1, y0], [u1, v0]);
    let bl = ColorGlyphVertex::new([x0, y1], [u0, v1]);
    let br = ColorGlyphVertex::new([x1, y1], [u1, v1]);
    out.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
}

/// Build the dedicated color-glyph vertex segment for shaped runs.
///
/// Color glyphs draw after coverage glyphs/decorations and before cursor/
/// overlays. Selection and search backgrounds are therefore already painted
/// under the unchanged premultiplied RGBA pixels. A 2-cell color glyph emits
/// exactly one quad from the lead cell; a run pointing at a continuation spacer
/// emits nothing.
pub fn build_color_glyph_vertices_into(
    out: &mut Vec<ColorGlyphVertex>,
    snapshot: &Snapshot,
    atlas: &ColorGlyphAtlas,
    runs: &[ColorGlyphRun],
) {
    build_color_glyph_vertices_with_origin_into(out, snapshot, atlas, runs, [0.0, 0.0]);
}

pub fn build_color_glyph_vertices_with_origin_into(
    out: &mut Vec<ColorGlyphVertex>,
    snapshot: &Snapshot,
    atlas: &ColorGlyphAtlas,
    runs: &[ColorGlyphRun],
    origin: [f32; 2],
) {
    out.clear();
    out.reserve(runs.len() * VERTS_PER_QUAD);

    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;

    for run in runs {
        if run.row >= rows || run.column >= cols {
            continue;
        }
        let idx = run.row * cols + run.column;
        let cell = &snapshot.cells[idx];
        if cell.wide_continuation || cell.attrs.hidden() {
            continue;
        }

        let Some(bounds) = atlas.lookup(run.key) else {
            continue;
        };
        let width_cells = bounds.width_cells as usize;
        if width_cells == 0 || run.column + width_cells > cols {
            continue;
        }
        if width_cells > run.covered_columns as usize {
            continue;
        }

        let x0 = origin[0] + run.column as f32 * cell_w;
        let y0 = origin[1] + run.row as f32 * cell_h;
        push_color_glyph_quad(
            out,
            [
                x0,
                y0,
                x0 + bounds.pixel_width as f32,
                y0 + bounds.pixel_height as f32,
            ],
            bounds.uv,
        );
    }
}

/// Pick the atlas style requested by terminal attributes.
pub fn font_style_for_attrs(attrs: &Attrs) -> FontStyle {
    match (attrs.bold(), attrs.italic()) {
        (true, true) => FontStyle::BoldItalic,
        (true, false) => FontStyle::Bold,
        (false, true) => FontStyle::Italic,
        (false, false) => FontStyle::Regular,
    }
}

/// Apply SGR dim/faint to an effective foreground color.
///
/// Dims perceptually in OKLab via [`crate::color::dim_perceptual`] at
/// [`DIM_PERCEPTUAL_AMOUNT`] (hue-preserving, chroma-aware), preserving the
/// alpha channel. The amount is calibrated to the perceived brightness of the
/// historical linear ×0.5 halving, so dim text stays as legible as before while
/// no longer skewing hue.
pub fn dim_color(color: [f32; 4]) -> [f32; 4] {
    let dimmed =
        crate::color::dim_perceptual([color[0], color[1], color[2]], DIM_PERCEPTUAL_AMOUNT);
    [dimmed[0], dimmed[1], dimmed[2], color[3]]
}

fn line_decoration_thickness(cell_h: f32) -> f32 {
    (cell_h / LINE_DECORATION_THICKNESS_DIVISOR)
        .round()
        .max(1.0)
}

/// Pixel-space underline rectangle for one cell.
pub fn underline_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32, baseline: f32) -> [f32; 4] {
    let thickness = line_decoration_thickness(cell_h);
    let y = (y0 + baseline + thickness).min(y0 + cell_h - thickness);
    [x0, y, x0 + cell_w, y + thickness]
}

fn push_segmented_line(
    out: &mut Vec<Vertex>,
    rect: [f32; 4],
    color: [f32; 4],
    painted: f32,
    gap: f32,
) {
    let [x0, y0, x1, y1] = rect;
    let painted = painted.max(1.0);
    let gap = gap.max(1.0);
    let mut x = x0;
    while x < x1 {
        let end = (x + painted).min(x1);
        if end > x {
            push_solid_quad(
                out,
                SolidQuad {
                    rect: [x, y0, end, y1],
                    color,
                },
            );
        }
        x += painted + gap;
    }
}

fn push_double_underline(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    cell_w: f32,
    cell_h: f32,
    baseline: f32,
    color: [f32; 4],
) {
    let lower = underline_rect(x0, y0, cell_w, cell_h, baseline);
    let thickness = lower[3] - lower[1];
    let upper_y = (lower[1] - thickness * 2.0).max(y0);
    push_solid_quad(
        out,
        SolidQuad {
            rect: [lower[0], upper_y, lower[2], upper_y + thickness],
            color,
        },
    );
    push_solid_quad(out, SolidQuad { rect: lower, color });
}

fn push_curly_underline(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    cell_w: f32,
    cell_h: f32,
    baseline: f32,
    color: [f32; 4],
) {
    let base = underline_rect(x0, y0, cell_w, cell_h, baseline);
    let thickness = base[3] - base[1];
    let step = (thickness * 2.0).max(2.0);
    let upper_y = (base[1] - thickness).max(y0);
    let lower_y = (base[1] + thickness).min(y0 + cell_h - thickness);
    let mut x = base[0];
    let mut high = true;

    // Curly underline uses a small stepped square-wave approximation. It stays
    // in the existing solid-quad path while keeping the visual distinct from
    // straight, dashed, and dotted styles.
    while x < base[2] {
        let end = (x + step).min(base[2]);
        let y = if high { upper_y } else { lower_y };
        push_solid_quad(
            out,
            SolidQuad {
                rect: [x, y, end, y + thickness],
                color,
            },
        );
        x = end;
        high = !high;
    }
}

fn push_underline_decoration(
    out: &mut Vec<Vertex>,
    style: UnderlineStyle,
    x0: f32,
    y0: f32,
    cell_w: f32,
    cell_h: f32,
    baseline: f32,
    color: [f32; 4],
) {
    let rect = underline_rect(x0, y0, cell_w, cell_h, baseline);
    let thickness = rect[3] - rect[1];
    match style {
        UnderlineStyle::None => {}
        UnderlineStyle::Straight => push_solid_quad(out, SolidQuad { rect, color }),
        UnderlineStyle::Double => {
            push_double_underline(out, x0, y0, cell_w, cell_h, baseline, color)
        }
        UnderlineStyle::Curly => push_curly_underline(out, x0, y0, cell_w, cell_h, baseline, color),
        UnderlineStyle::Dotted => {
            // One painted square, one square gap. The dot size follows line
            // thickness so small fonts stay crisp.
            push_segmented_line(out, rect, color, thickness, thickness)
        }
        UnderlineStyle::Dashed => {
            // Six thickness units painted, three units gap. This is long enough
            // to read as a dash on small cells without spanning whole words.
            push_segmented_line(out, rect, color, thickness * 6.0, thickness * 3.0)
        }
    }
}

/// Pixel-space strikethrough rectangle for one cell.
///
/// The line sits near the visual midline, derived from the atlas baseline so it
/// stays stable across font sizes and faces without needing per-glyph metrics.
pub fn strikethrough_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32, baseline: f32) -> [f32; 4] {
    let thickness = line_decoration_thickness(cell_h);
    let y = (y0 + baseline * 0.6)
        .round()
        .clamp(y0, y0 + cell_h - thickness);
    [x0, y, x0 + cell_w, y + thickness]
}

/// Build the full vertex list for a snapshot against a glyph atlas.
///
/// Pure and GPU-free: the same input always yields the same vertices. Cell
/// pixel size comes from `atlas.cell`, so cells map 1:1 onto atlas cells.
///
/// Rules:
/// - `wide_continuation` spacer cells are skipped; a wide lead cell's
///   background spans both columns so there is no gap.
/// - `attrs.inverse()` swaps foreground and background before emitting.
/// - A foreground quad is emitted only for a printable, inked glyph: the
///   character is not a space and the atlas has a UV rect for it (printable
///   ASCII). Control/non-ASCII cells emit background only.
pub fn build_vertices(snapshot: &Snapshot, atlas: &GlyphAtlas) -> Vec<Vertex> {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let mut out = Vec::with_capacity(rows * cols * VERTS_PER_QUAD * 2);
    build_vertices_into(&mut out, snapshot, atlas);
    out
}

/// Rebuild the full vertex list into an existing allocation.
///
/// This is the allocation-reuse path used by the native renderer: callers keep
/// a grow-only `Vec<Vertex>`, then clear and refill it for each rebuilt frame.
/// The cursor is drawn as a block; callers that honor DECSCUSR cursor shapes use
/// [`build_vertices_with_cursor_into`].
pub fn build_vertices_into(out: &mut Vec<Vertex>, snapshot: &Snapshot, atlas: &GlyphAtlas) {
    build_vertices_with_cursor_into(out, snapshot, atlas, CursorStyle::Block);
}

/// Rebuild the full vertex list, drawing the cursor in the given DECSCUSR shape.
pub fn build_vertices_with_cursor_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
) {
    build_cell_vertices_into(out, snapshot, atlas);
    append_cursor_vertices(out, snapshot, atlas, cursor_style);
}

/// Rebuild only the terminal cell geometry, excluding cursor and overlays.
///
/// Native uses this for retained-buffer rendering: the cell segment is rebuilt
/// only when terminal/UI content changes, while cursor blink can refresh the
/// bounded cursor tail without walking every cell.
pub fn build_cell_vertices_into(out: &mut Vec<Vertex>, snapshot: &Snapshot, atlas: &GlyphAtlas) {
    build_cell_vertices_with_color_glyph_runs_into(out, snapshot, atlas, &[]);
}

/// Build terminal-cell vertices while suppressing the monochrome foreground
/// glyph for cells that will be covered by a live color glyph run.
///
/// Backgrounds and text decorations are still emitted. This keeps selection,
/// search, underline, and strikethrough layers below/around the color bitmap
/// while preventing fallback boxes from showing through transparent emoji
/// pixels.
///
/// This is the focus-agnostic entry (the focused window). Callers that render an
/// unfocused window with ID2 focus dimming use
/// [`build_cell_vertices_with_focus_dim_into`]; this wrapper forwards a `0.0`
/// amount, which is an exact no-op, so the focused path stays byte-identical.
pub fn build_cell_vertices_with_color_glyph_runs_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
) {
    build_cell_vertices_with_focus_dim_into(out, snapshot, atlas, color_runs, 0.0);
}

/// Build terminal-cell vertices (as
/// [`build_cell_vertices_with_color_glyph_runs_into`]) while applying the ID2
/// focus-dimming amount.
///
/// `focus_dim` is the perceptual amount applied to every cell's foreground *and*
/// background before the RV1 minimum-contrast floor, so the whole window recedes
/// while it is unfocused without losing legibility. The native layer passes
/// `0.0` while the window is focused (and always when the `focus_dim` knob is
/// off), which short-circuits to an exact no-op so focused frames stay
/// byte-identical to the pre-feature renderer.
pub fn build_cell_vertices_with_focus_dim_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    focus_dim: f32,
) {
    build_cell_vertices_with_focus_dim_and_origin_into(
        out,
        snapshot,
        atlas,
        color_runs,
        focus_dim,
        [0.0, 0.0],
    );
}

pub fn build_cell_vertices_with_focus_dim_and_origin_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    focus_dim: f32,
    origin: [f32; 2],
) {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    let baseline = atlas.cell.baseline as f32;

    let needed = rows * cols * VERTS_PER_QUAD * 2;
    out.clear();
    if out.capacity() < needed {
        out.reserve(needed - out.capacity());
    }

    // Effective foreground/background after inverse + dim, and the column span of
    // a wide lead cell. Computed identically in both passes.
    let resolve = |cell: &crate::core::Cell| -> ([f32; 4], [f32; 4]) {
        let mut fg = foreground_linear(&snapshot.colors, cell.attrs.foreground);
        let mut bg = background_linear(&snapshot.colors, cell.attrs.background);
        if cell.attrs.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.attrs.dim() {
            fg = dim_color(fg);
        }
        // ID2 focus dimming: while the window is unfocused, recede the whole
        // cell — both foreground and background — perceptually in OKLab so hue
        // is preserved and relative contrast stays roughly stable. Applied after
        // the SGR-dim attribute and before the RV1 floor so legibility wins by
        // construction (the floor sees the dimmed background and re-lifts text
        // above the configured ratio). `focus_dim == 0.0` (focused, or the knob
        // off) is skipped entirely, keeping focused frames byte-identical.
        if focus_dim > 0.0 {
            fg = text::dim_linear_rgba(fg, focus_dim);
            bg = text::dim_linear_rgba(bg, focus_dim);
        }
        // RV1 minimum-contrast floor: lift the foreground until it meets the
        // configured WCAG ratio against this cell's background. Applied last so
        // it sees the post-inverse, post-dim color. Exact passthrough at the
        // default floor of 1.0, so the plain path is byte-identical.
        fg = text::enforce_contrast_rgba(fg, bg);
        (fg, bg)
    };
    let span_of = |row: usize, col: usize| -> f32 {
        if col + 1 < cols && snapshot.cells[row * cols + col + 1].wide_continuation {
            2.0
        } else {
            1.0
        }
    };

    // Pass 1: full-cell background quads only. Emitting every background before
    // any glyph guarantees a later column's background can never paint over an
    // earlier glyph's beyond-cell overflow ink.
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }
            let (_, bg) = resolve(cell);
            let span = span_of(row, col);
            let x0 = origin[0] + col as f32 * cell_w;
            let y0 = origin[1] + row as f32 * cell_h;
            push_quad(
                out,
                [x0, y0, x0 + cell_w * span, y0 + cell_h],
                [0.0, 0.0, 0.0, 0.0],
                bg,
                0.0,
            );
        }
    }

    // Pass 2: glyph quads (sized from bearing-aware atlas bounds, so overflow ink
    // renders uncropped) plus underline/strikethrough decorations, all drawn over
    // every background from pass 1.
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }
            let (fg, _) = resolve(cell);
            let span = span_of(row, col);
            let x0 = origin[0] + col as f32 * cell_w;
            let y0 = origin[1] + row as f32 * cell_h;

            if !cell.attrs.hidden()
                && cell.ch != ' '
                && !has_color_glyph_run(color_runs, row, col)
                && let Some(bounds) =
                    atlas.glyph_quad_styled(font_style_for_attrs(&cell.attrs), cell.ch)
            {
                push_glyph_quad(out, x0, y0, bounds, fg);
            }

            let underline_style = cell.attrs.effective_underline_style();
            if underline_style != UnderlineStyle::None {
                let underline_color = cell
                    .attrs
                    .underline_color
                    .map_or(fg, |color| foreground_linear(&snapshot.colors, color));
                push_underline_decoration(
                    out,
                    underline_style,
                    x0,
                    y0,
                    cell_w * span,
                    cell_h,
                    baseline,
                    underline_color,
                );
            }

            if cell.attrs.strikethrough() {
                push_solid_quad(
                    out,
                    SolidQuad {
                        rect: strikethrough_rect(x0, y0, cell_w * span, cell_h, baseline),
                        color: fg,
                    },
                );
            }
        }
    }
}

fn has_color_glyph_run(runs: &[ColorGlyphRun], row: usize, column: usize) -> bool {
    runs.iter().any(|run| run.covers(row, column))
}

/// Append only cursor geometry for `snapshot` and `cursor_style`.
///
/// The cursor emits at most two quads (block over a printable glyph) or one quad
/// for underline/bar styles, so callers can update this segment without a full
/// cell rebuild when only blink phase changes.
pub fn append_cursor_vertices(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
) {
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    push_cursor(
        out,
        snapshot,
        atlas,
        cell_w,
        cell_h,
        cursor_style,
        [0.0, 0.0],
    );
}

pub fn append_cursor_vertices_with_origin(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    origin: [f32; 2],
) {
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    push_cursor(out, snapshot, atlas, cell_w, cell_h, cursor_style, origin);
}

/// Push a glyph quad sized and positioned from bearing-aware atlas bounds.
///
/// The cell's on-screen origin is `(x0, y0)`; the quad is offset and sized by the
/// glyph's inked extent (1 atlas pixel == 1 physical screen pixel), so ink that
/// overflows the cell box is drawn uncropped while backgrounds stay full-cell.
fn push_glyph_quad(out: &mut Vec<Vertex>, x0: f32, y0: f32, bounds: GlyphBounds, color: [f32; 4]) {
    let gx0 = x0 + bounds.offset_x as f32;
    let gy0 = y0 + bounds.offset_y as f32;
    let gx1 = gx0 + bounds.width as f32;
    let gy1 = gy0 + bounds.height as f32;
    push_quad(out, [gx0, gy0, gx1, gy1], bounds.uv, color, 1.0);
}

/// Rebuild the full vertex list and append presentation-only solid overlays.
/// The cursor is drawn as a block; see
/// [`build_vertices_with_overlays_and_cursor_into`] for shaped cursors.
pub fn build_vertices_with_overlays_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    overlays: &[SolidQuad],
) {
    build_vertices_with_overlays_and_cursor_into(
        out,
        snapshot,
        atlas,
        CursorStyle::Block,
        overlays,
    );
}

/// Rebuild the full vertex list with a DECSCUSR-shaped cursor and append
/// presentation-only solid overlays.
pub fn build_vertices_with_overlays_and_cursor_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    overlays: &[SolidQuad],
) {
    build_vertices_with_cursor_into(out, snapshot, atlas, cursor_style);
    out.reserve(overlays.len() * VERTS_PER_QUAD);
    for &overlay in overlays {
        push_solid_quad(out, overlay);
    }
}

/// Pixel thickness of a non-block cursor (underline bar / vertical bar).
fn cursor_bar_thickness(extent: f32) -> f32 {
    (extent / 8.0).round().clamp(1.0, extent.max(1.0))
}

/// Pixel-space rectangle for an underline cursor: a thin horizontal bar pinned
/// to the bottom edge of the cell.
pub fn cursor_underline_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32) -> [f32; 4] {
    let thickness = cursor_bar_thickness(cell_h);
    [x0, y0 + cell_h - thickness, x0 + cell_w, y0 + cell_h]
}

/// Pixel-space rectangle for a bar cursor: a thin vertical bar at the cell's
/// left edge.
pub fn cursor_bar_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32) -> [f32; 4] {
    let thickness = cursor_bar_thickness(cell_w);
    [x0, y0, x0 + thickness, y0 + cell_h]
}

/// Emit the cursor for the snapshot in the given shape, if one should be drawn.
///
/// - **Block**: an **inverse** block — a background quad in the cell's
///   foreground color with the cell's glyph (if any) redrawn on top in the
///   cell's background color, keeping the character readable under the cursor.
/// - **Underline**: a thin foreground-colored bar at the cell's bottom edge,
///   drawn over the cell's existing glyph (no inversion).
/// - **Bar**: a thin foreground-colored vertical bar at the cell's left edge,
///   drawn over the cell's existing glyph (no inversion).
///
/// A hidden cursor (`cursor_visible == false`, which the renderer also uses for
/// the blink "off" phase) emits nothing. The position is clamped to the grid so
/// a stale snapshot can never index out of bounds. Reflects only the live
/// snapshot cursor — no scrollback/viewport offset is applied here.
fn push_cursor(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cell_w: f32,
    cell_h: f32,
    style: CursorStyle,
    origin: [f32; 2],
) {
    if !snapshot.cursor_visible {
        return;
    }

    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if cols == 0 || rows == 0 {
        return;
    }

    // Defensive clamp: a stale snapshot could carry a cursor past the grid.
    let row = snapshot.cursor.row.min(rows - 1);
    let col = snapshot.cursor.column.min(cols - 1);

    let cell = &snapshot.cells[row * cols + col];

    // Effective colors after the cell's own inverse attribute. For the block
    // cursor these are swapped again so the block reads as an inversion of the
    // cell; the bar/underline shapes draw in the effective foreground.
    let mut fg = foreground_linear(&snapshot.colors, cell.attrs.foreground);
    let mut bg = background_linear(&snapshot.colors, cell.attrs.background);
    if cell.attrs.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }

    let x0 = origin[0] + col as f32 * cell_w;
    let y0 = origin[1] + row as f32 * cell_h;

    match style {
        CursorStyle::Block => {
            let block_color = rgb_linear(snapshot.colors.cursor);
            // The under-cursor glyph is drawn in the cell's background color over
            // the cursor block; apply the RV1 floor so it stays legible against
            // the block (the relevant pair here is glyph-vs-block, since `fg` is
            // not drawn in this path). Passthrough at the default floor of 1.0.
            let glyph_color = text::enforce_contrast_rgba(bg, block_color);
            push_quad(
                out,
                [x0, y0, x0 + cell_w, y0 + cell_h],
                [0.0, 0.0, 0.0, 0.0],
                block_color,
                0.0,
            );
            if !cell.attrs.hidden()
                && cell.ch != ' '
                && let Some(bounds) =
                    atlas.glyph_quad_styled(font_style_for_attrs(&cell.attrs), cell.ch)
            {
                push_glyph_quad(out, x0, y0, bounds, glyph_color);
            }
        }
        CursorStyle::Underline => {
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_underline_rect(x0, y0, cell_w, cell_h),
                    color: rgb_linear(snapshot.colors.cursor),
                },
            );
        }
        CursorStyle::Bar => {
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_bar_rect(x0, y0, cell_w, cell_h),
                    color: rgb_linear(snapshot.colors.cursor),
                },
            );
        }
    }
}

fn foreground_linear(colors: &DynamicColors, color: Color) -> [f32; 4] {
    match color {
        Color::Default => rgb_linear(colors.foreground),
        Color::Indexed(index) => rgb_linear(
            colors
                .palette_color(index)
                .unwrap_or_else(|| rgb_from_tuple(text::indexed_srgb(index))),
        ),
        Color::Rgb(red, green, blue) => rgb_linear(RgbColor::new(red, green, blue)),
    }
}

fn background_linear(colors: &DynamicColors, color: Color) -> [f32; 4] {
    match color {
        Color::Default => rgb_linear(colors.background),
        Color::Indexed(index) => rgb_linear(
            colors
                .palette_color(index)
                .unwrap_or_else(|| rgb_from_tuple(text::indexed_srgb(index))),
        ),
        Color::Rgb(red, green, blue) => rgb_linear(RgbColor::new(red, green, blue)),
    }
}

fn rgb_linear(color: RgbColor) -> [f32; 4] {
    [
        text::srgb_to_linear(color.red),
        text::srgb_to_linear(color.green),
        text::srgb_to_linear(color.blue),
        1.0,
    ]
}

fn rgb_from_tuple(color: (u8, u8, u8)) -> RgbColor {
    RgbColor::new(color.0, color.1, color.2)
}

#[cfg(test)]
mod tests;

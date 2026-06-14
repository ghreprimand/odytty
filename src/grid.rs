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

        let x0 = run.column as f32 * cell_w;
        let y0 = run.row as f32 * cell_h;
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
            let x0 = col as f32 * cell_w;
            let y0 = row as f32 * cell_h;
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
            let x0 = col as f32 * cell_w;
            let y0 = row as f32 * cell_h;

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
    push_cursor(out, snapshot, atlas, cell_w, cell_h, cursor_style);
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

    let x0 = col as f32 * cell_w;
    let y0 = row as f32 * cell_h;

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
mod tests {
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
            build_cell_vertices_with_focus_dim_into(out, &combo, &atlas, &[], focus);
        };

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
        build_vertices_with_cursor_into(
            &mut verts,
            &term.snapshot(),
            &atlas,
            CursorStyle::Underline,
        );
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
            let [r, g, b] = crate::color::enforce_min_contrast(
                [fg[0], fg[1], fg[2]],
                [bg[0], bg[1], bg[2]],
                ratio,
            );
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
        build_cell_vertices_with_focus_dim_into(&mut focused, &snapshot, &atlas, &[], 0.0);
        let mut unfocused = Vec::new();
        build_cell_vertices_with_focus_dim_into(&mut unfocused, &snapshot, &atlas, &[], 0.3);

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
}

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
use crate::ligature::LigatureRun;
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

/// Per-cell color-run coverage mask.
///
/// Every render-preparation consumer (atlas warm-up, ligature shaping, cell
/// vertex build) needs the same predicate — "is this cell covered by a color
/// glyph run?" — for every visible cell. Answering it by scanning the run
/// list per cell is O(cells x runs), which explodes on emoji-heavy screens
/// where the run count grows with the cell count. This mask is built once per
/// consumer pass in O(cells / 64 + runs) and answers each query in O(1),
/// producing exactly the same answers as
/// `runs.iter().any(|run| run.covers(row, column))` for every in-grid cell
/// (pinned by an equivalence test).
pub struct ColorRunCoverage {
    columns: usize,
    rows: usize,
    /// One bit per cell, row-major. Left empty when there are no runs so the
    /// common emoji-free frame skips the allocation entirely.
    bits: Vec<u64>,
}

impl ColorRunCoverage {
    pub fn new(runs: &[ColorGlyphRun], columns: usize, rows: usize) -> Self {
        if runs.is_empty() || columns == 0 || rows == 0 {
            return Self {
                columns,
                rows,
                bits: Vec::new(),
            };
        }
        let mut bits = vec![0u64; (columns * rows).div_ceil(64)];
        for run in runs {
            if run.row >= rows {
                continue;
            }
            let start = run.column.min(columns);
            let end = run
                .column
                .saturating_add(run.covered_columns as usize)
                .min(columns);
            for column in start..end {
                let index = run.row * columns + column;
                bits[index / 64] |= 1 << (index % 64);
            }
        }
        Self {
            columns,
            rows,
            bits,
        }
    }

    /// Whether no run covers any cell (the common emoji-free frame).
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }

    /// Whether any run covers cell `(row, column)`. Out-of-grid coordinates
    /// answer `false`; runs are grid-derived, so no in-grid query differs from
    /// the linear scan.
    #[inline]
    pub fn covers(&self, row: usize, column: usize) -> bool {
        if self.bits.is_empty() || row >= self.rows || column >= self.columns {
            return false;
        }
        let index = row * self.columns + column;
        self.bits[index / 64] & (1 << (index % 64)) != 0
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
    build_color_glyph_vertices_with_origin_into(
        out,
        snapshot,
        atlas,
        runs,
        [0.0, 0.0],
        ChromePin::NONE,
    );
}

pub fn build_color_glyph_vertices_with_origin_into(
    out: &mut Vec<ColorGlyphVertex>,
    snapshot: &Snapshot,
    atlas: &ColorGlyphAtlas,
    runs: &[ColorGlyphRun],
    origin: [f32; 2],
    // SCROLL-CHROME-BOUNCE: crop content color glyphs at the tab-bar seam.
    chrome_pin: ChromePin,
) {
    out.clear();
    out.reserve(runs.len() * VERTS_PER_QUAD);

    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    // SCROLL-CHROME-BOUNCE: color glyphs are always content; crop any that glide
    // up under the pinned tab bar at the seam (inert unless a glide is running).
    let chrome_seam_y = chrome_pin.seam_y(origin[1], cell_h);

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

        // CHROME-GAP: color glyphs ride the same per-cell chrome-gap shifts as
        // the mono builder (content past a left rail / below the bar; the rail
        // band past a right rail). Zero-gap pins leave both terms at 0.0.
        let x0 = origin[0] + run.column as f32 * cell_w + chrome_pin.cell_dx(run.column);
        let y0 = origin[1] + run.row as f32 * cell_h + chrome_pin.cell_dy(run.row, run.column);
        let x1 = x0 + bounds.pixel_width as f32;
        if chrome_pin.active() && chrome_pin.top_rows > 0 {
            push_color_glyph_quad_clipped_top(
                out,
                x0,
                y0,
                x1,
                bounds.pixel_height as f32,
                bounds.uv,
                chrome_seam_y,
            );
        } else {
            // TAB-LABEL-CENTERING: an emoji tab/rail label rides the same sub-cell
            // shift the mono path uses, so a color label centers identically.
            // `0.0` (content, single-row / odd-height bands) is byte-identical.
            let glyph_y0 = y0 + chrome_pin.glyph_center_dy(run.row, run.column, cell_h);
            push_color_glyph_quad(
                out,
                [x0, glyph_y0, x1, glyph_y0 + bounds.pixel_height as f32],
                bounds.uv,
            );
        }
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

#[allow(clippy::too_many_arguments)]
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
    build_cell_vertices_with_focus_dim_into(
        out,
        snapshot,
        atlas,
        color_runs,
        0.0,
        BackgroundTreatmentParams::default(),
    );
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
    treatment: BackgroundTreatmentParams,
) {
    build_cell_vertices_with_focus_dim_and_origin_into(
        out,
        snapshot,
        atlas,
        color_runs,
        focus_dim,
        [0.0, 0.0],
        treatment,
        // Identity opacity (literal 1.0): this focus-dim/color-glyph entry never
        // carries the image treatment, so it keeps cells fully opaque
        // (byte-identical). Pinned to 1.0 rather than the default `cell_bg_opacity`
        // — the shipped default is 0.8 since v0.6.0, but this seam's contract is
        // opaque cells regardless of that default.
        1.0,
        // No overlay panel rides this seam, so no cell is force-opaque.
        None,
        // Off-screen `[0.0, 0.0]` origin build: no chrome to pin.
        ChromePin::NONE,
    );
}

/// TRANSPARENCY (MENU-OPACITY): a rectangular span of grid cells whose
/// background quads must stay **fully opaque** regardless of the frame's
/// `cell_bg_opacity`. In the single-pane path an open overlay panel (context
/// menu / settings / picker) is painted directly into the terminal snapshot, so
/// when the window is translucent the whole snapshot — including the panel — is
/// built at the window alpha. That resealed nothing but sank the panel to the
/// window opacity, letting the desktop bleed through the menu. Marking the
/// panel's cell span keeps the overlay SURFACE opaque (the ruled readability
/// boundary) while the terminal cells outside it still scale with the window
/// opacity. Coordinates are cell indices in the snapshot being built (i.e. after
/// any tab-chrome decoration); `None` at the call site is the byte-identical
/// path (multi-pane draws its overlay as a separate opaque layer, and the opaque
/// window path never sets a region).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRegion {
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
}

impl CellRegion {
    /// Whether cell `(row, col)` falls inside this region.
    #[inline]
    pub fn contains(&self, row: usize, col: usize) -> bool {
        col >= self.left
            && col < self.left + self.width
            && row >= self.top
            && row < self.top + self.height
    }
}

/// TAB-LABEL-CENTERING: the sub-row vertical offset (in cell-height units) that
/// recenters a single label line placed at integer `label_row` within a
/// `band_rows`-tall band onto the band's true geometric center. Multiply by the
/// cell height for the pixel shift (see [`ChromePin::band_glyph_dy_rows`]).
///
/// Returns `0.0` for a one-row band and whenever the label already sits on the
/// exact center (odd bands under either placement convention), so it is inert on
/// the classic single-row chrome and every odd height. A row-snapped label can
/// never be pixel-centered on an EVEN band (no integer row is the center), which
/// is exactly the residual this offset removes: e.g. a 4-row top bar snaps its
/// label to row 2 (center of {0,1,2,3} is 1.5), leaving it half a cell low; this
/// returns `-0.5` to lift it back.
pub fn band_label_center_dy_rows(band_rows: usize, label_row: usize) -> f32 {
    if band_rows <= 1 {
        return 0.0;
    }
    band_rows as f32 / 2.0 - label_row as f32 - 0.5
}

/// Center a chrome label while reserving two physical pixels beneath its ink
/// for font descenders. The row-level placement stays unchanged; only the
/// bearing-aware glyph quad moves upward by the guard amount. Two pixels keep a
/// full clear sample at fractional physical origins on the single-row bar.
pub fn band_label_descender_safe_dy_rows(
    band_rows: usize,
    label_row: usize,
    cell_height_px: u32,
) -> f32 {
    let centered = band_label_center_dy_rows(band_rows, label_row);
    if cell_height_px == 0 {
        centered
    } else {
        centered - 2.0 / cell_height_px as f32
    }
}

/// Rail labels use the same two-pixel descender clearance. Their even-height
/// slot centering additionally moves the label into the breathing row below.
pub fn rail_label_descender_safe_dy_rows(
    band_rows: usize,
    label_row: usize,
    cell_height_px: u32,
) -> f32 {
    let centered = band_label_center_dy_rows(band_rows, label_row);
    if cell_height_px == 0 {
        centered
    } else {
        centered - 2.0 / cell_height_px as f32
    }
}

/// SCROLL-CHROME-BOUNCE: pins composited chrome (the top tab-bar band and any
/// side rail band) against the sub-row smooth-scroll offset that `content_origin`
/// folds into the vertex Y. Without it the whole decorated single-pane snapshot
/// — chrome rows included — glides with the scrollback, so the tab bar visibly
/// drifts. Chrome cells subtract `scroll_offset_y` (landing back at the
/// un-shifted pad-y) while terminal content keeps it (so content still glides).
/// `NONE` / `scroll_offset_y == 0.0` makes every branch inert, so the plain and
/// at-rest paths stay byte-identical.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromePin {
    /// The sub-row vertical offset already folded into `origin[1]`. Chrome cells
    /// subtract it to stay pinned; `0.0` disables the whole mechanism.
    pub scroll_offset_y: f32,
    /// Composited tab-bar rows at the top of the snapshot (pinned; also the seam
    /// below which content is clamped so a sub-row overshoot can neither bleed
    /// into the bar nor open a gap under it).
    pub top_rows: usize,
    /// Pinned side-rail column band `[rail_col_start, rail_col_end)` spanning
    /// every row. Empty (`start == end`) when no rail is composited.
    pub rail_col_start: usize,
    /// End (exclusive) of the pinned rail column band.
    pub rail_col_end: usize,
    /// TAB-LABEL-CENTERING: sub-row glyph shift (in cell-height units) applied to
    /// glyph quads in the top tab band (rows `< top_rows`), recentering its
    /// single label row onto the band's true pixel center with descender space.
    /// Backgrounds are unaffected, so the full-height active fill and gap-free
    /// band are intact. `0.0` on content builds is inert. Independent of the
    /// scroll glide: it applies even at rest.
    pub band_glyph_dy_rows: f32,
    /// TAB-LABEL-CENTERING: the rail analog of `band_glyph_dy_rows`, applied to
    /// glyph quads in the rail column band (`rail_col_start..rail_col_end`). The
    /// rail places its slot label at `(slot_rows - 1) / 2`, biased HIGH on even
    /// slots, so this offset is the opposite sign of the top band's. `0.0` is
    /// inert.
    pub rail_glyph_dy_rows: f32,
    /// CHROME-GAP: pixels inserted at the rail↔content seam ("content never
    /// touches chrome" — the window padding value, applied between the pinned
    /// rail band and the content columns). Cells at/right of the seam column
    /// shift right by this: past a LEFT rail that is the content (and the top
    /// bar above it, keeping one uniform column basis); past the content of a
    /// RIGHT rail it is the rail band itself. `0.0` (no rail / zero padding) is
    /// byte-identical.
    pub gap_x: f32,
    /// CHROME-GAP: pixels inserted below the pinned top tab-bar band. Content
    /// cells (rows at/below `top_rows`, outside the rail band) shift down by
    /// this; the full-height rail band and the bar itself stay put. `0.0` is
    /// byte-identical.
    pub gap_y: f32,
}

impl ChromePin {
    /// Inert pin: no chrome, no offset. Every call site that is not the
    /// single-pane gliding content path passes this, keeping the frame
    /// byte-identical.
    pub const NONE: Self = Self {
        scroll_offset_y: 0.0,
        top_rows: 0,
        rail_col_start: 0,
        rail_col_end: 0,
        band_glyph_dy_rows: 0.0,
        rail_glyph_dy_rows: 0.0,
        gap_x: 0.0,
        gap_y: 0.0,
    };

    /// Whether the pin is doing anything this frame (a glide is in flight).
    #[inline]
    fn active(&self) -> bool {
        self.scroll_offset_y != 0.0
    }

    /// Whether cell `(row, col)` is composited chrome (top bar or rail band),
    /// which must stay pinned rather than glide with the content.
    #[inline]
    fn is_chrome(&self, row: usize, col: usize) -> bool {
        row < self.top_rows || self.in_rail_band(col)
    }

    /// Whether `col` lies inside the composited rail column band.
    #[inline]
    fn in_rail_band(&self, col: usize) -> bool {
        col >= self.rail_col_start && col < self.rail_col_end
    }

    /// CHROME-GAP: the horizontal shift of cell column `col`. Columns at/right
    /// of the rail↔content seam shift by `gap_x`: past a LEFT rail band
    /// (`rail_col_start == 0`) that is every non-rail column — content and the
    /// top bar share one shifted column basis; with a RIGHT rail
    /// (`rail_col_start > 0`) it is the rail band itself that moves off the
    /// content. `0.0` whenever no rail band exists or the gap is zero.
    #[inline]
    fn cell_dx(&self, col: usize) -> f32 {
        if self.gap_x == 0.0 || self.rail_col_start == self.rail_col_end {
            return 0.0;
        }
        let seam_col = if self.rail_col_start == 0 {
            self.rail_col_end
        } else {
            self.rail_col_start
        };
        if col >= seam_col { self.gap_x } else { 0.0 }
    }

    /// CHROME-GAP: the vertical shift of cell `(row, col)` — content rows below
    /// the top band shift down by `gap_y`; the bar itself and the full-height
    /// rail band stay put. `0.0` whenever there is no top band or no gap.
    #[inline]
    fn cell_dy(&self, row: usize, col: usize) -> f32 {
        if self.gap_y != 0.0 && row >= self.top_rows && !self.in_rail_band(col) {
            self.gap_y
        } else {
            0.0
        }
    }

    /// CHROME-GAP: the horizontal shift the CONTENT columns carry — `gap_x`
    /// past a pinned LEFT rail, `0.0` otherwise (a right rail shifts the band,
    /// not the content). For cursor-anchored geometry built without per-cell
    /// dispatch.
    #[inline]
    pub fn content_dx(&self) -> f32 {
        if self.rail_col_start == 0 && self.rail_col_end > 0 {
            self.gap_x
        } else {
            0.0
        }
    }

    /// CHROME-GAP: the vertical shift the CONTENT rows carry below a pinned top
    /// band (`gap_y`; `0.0` without one).
    #[inline]
    pub fn content_dy(&self) -> f32 {
        self.gap_y
    }

    /// The un-shifted top-of-content Y (the seam below the pinned top bar) for a
    /// build whose `origin[1]` already carries `scroll_offset_y`. CHROME-GAP:
    /// includes `gap_y`, so gliding content clamps at the gap-inset content top
    /// and the gap strip below the bar stays clean, exactly like the padding
    /// band.
    #[inline]
    fn seam_y(&self, shifted_origin_y: f32, cell_h: f32) -> f32 {
        (shifted_origin_y - self.scroll_offset_y) + self.top_rows as f32 * cell_h + self.gap_y
    }

    /// The top-left Y of cell `(row, col)`: pinned (un-shifted) for chrome,
    /// glide-shifted for content. Inert (plain shifted origin) when `!active()`
    /// and no chrome gap is in play. CHROME-GAP: content cells additionally
    /// carry `cell_dy` (the below-band gap); chrome cells never do.
    #[inline]
    fn cell_top_y(&self, shifted_origin_y: f32, cell_h: f32, row: usize, col: usize) -> f32 {
        let base = if self.active() && self.is_chrome(row, col) {
            (shifted_origin_y - self.scroll_offset_y) + row as f32 * cell_h
        } else {
            shifted_origin_y + row as f32 * cell_h
        };
        base + self.cell_dy(row, col)
    }

    /// TAB-LABEL-CENTERING: the sub-cell glyph Y shift (pixels) for a chrome
    /// label cell, recentering a multi-row band's single label row on the band's
    /// true center. The rail column band takes precedence over the top band in
    /// the shared top-left corner, matching the chrome hit-test (which resolves
    /// the rail first). `0.0` for every content cell and every inert (single-row
    /// / odd-height) band, so the plain and single-row-chrome paths stay
    /// byte-identical.
    ///
    /// PIXEL-SNAP: the composed shift is rounded to a whole physical pixel.
    /// Even-height bands center their label by half a cell, so an ODD physical
    /// cell height lands the shift on a half-pixel origin; texture sampling then
    /// bleeds every label glyph's bottom ink row 50/50 into the row below, which
    /// visibly thins baseline strokes (a digit's flat bottom) on the dim rail
    /// band. Cell-height parity follows `font_size x monitor_scale`, so the
    /// artifact was per-monitor: 20px cells (scale 1.0) and 34px cells (1.67)
    /// were clean while 25px cells (scale 1.25) clipped. Rounding moves the
    /// label by at most half a pixel, so at least 1.5px of the two-pixel
    /// descender guard always survives; whole-pixel shifts (even cell heights,
    /// single-row bands) round to themselves, so clean configurations render
    /// identically. Content cells keep the exact `0.0` arm.
    #[inline]
    fn glyph_center_dy(&self, row: usize, col: usize, cell_h: f32) -> f32 {
        if self.rail_glyph_dy_rows != 0.0 && col >= self.rail_col_start && col < self.rail_col_end {
            (self.rail_glyph_dy_rows * cell_h).round()
        } else if self.band_glyph_dy_rows != 0.0 && row < self.top_rows {
            (self.band_glyph_dy_rows * cell_h).round()
        } else {
            0.0
        }
    }
}

/// SCROLL-CHROME-BOUNCE: the vertical span of a content cell's background quad,
/// clamped at the tab-bar seam so a gliding sub-row offset neither bleeds the
/// content background up into the pinned bar nor opens a gap beneath it. The
/// first content row is pulled flush to the seam (filling a downward-glide gap
/// or cropping an upward overshoot); lower rows keep their natural top (already
/// below the seam). Chrome cells and the inert path return the natural span.
#[inline]
fn content_bg_span(
    pin: &ChromePin,
    seam_y: f32,
    y0: f32,
    cell_h: f32,
    row: usize,
    col: usize,
) -> (f32, f32) {
    let bottom = y0 + cell_h;
    if pin.active() && pin.top_rows > 0 && !pin.is_chrome(row, col) {
        let top = if row == pin.top_rows {
            seam_y
        } else {
            y0.max(seam_y)
        };
        (top, bottom)
    } else {
        (y0, bottom)
    }
}

// ID3/U5 adds `cell_bg_opacity` as the 8th argument; the existing inputs
// (geometry, focus dim, treatment) are already discrete render parameters and
// bundling them into a struct would obscure the two live call sites more than
// it helps. Matches `push_cursor`'s identical pragma.
#[allow(clippy::too_many_arguments)]
pub fn build_cell_vertices_with_focus_dim_and_origin_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    focus_dim: f32,
    origin: [f32; 2],
    treatment: BackgroundTreatmentParams,
    cell_bg_opacity: f32,
    // TRANSPARENCY (MENU-OPACITY): cells inside this span draw their background
    // fully opaque, ignoring `cell_bg_opacity`, so an overlay panel painted into
    // a translucent snapshot stays a readable opaque surface. `None` is the
    // byte-identical path (every cell uses `cell_bg_opacity`).
    opaque_region: Option<CellRegion>,
    // SCROLL-CHROME-BOUNCE: pins composited chrome against the sub-row glide.
    // `ChromePin::NONE` (every non-single-pane-content caller) is byte-identical.
    chrome_pin: ChromePin,
) {
    build_cells_core(
        out,
        snapshot,
        atlas,
        color_runs,
        &[],
        focus_dim,
        origin,
        treatment,
        cell_bg_opacity,
        opaque_region,
        chrome_pin,
        // No selection opacity threaded: selected cells (none on this seam)
        // take the full-strength selection tint. Byte-identical for the
        // no-selection callers.
        1.0,
    );
}

/// Ligature-aware counterpart to
/// [`build_cell_vertices_with_focus_dim_and_origin_into`]. An empty run slice
/// takes the same scalar-glyph branches as the legacy entry point. Selection
/// cells take the full-strength selection tint here (no color blend); the
/// selection-opacity-aware render path uses
/// [`build_cell_vertices_with_ligatures_and_selection_into`].
#[allow(clippy::too_many_arguments)]
pub fn build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    ligature_runs: &[LigatureRun],
    focus_dim: f32,
    origin: [f32; 2],
    treatment: BackgroundTreatmentParams,
    cell_bg_opacity: f32,
    opaque_region: Option<CellRegion>,
    chrome_pin: ChromePin,
) {
    build_cells_core(
        out,
        snapshot,
        atlas,
        color_runs,
        ligature_runs,
        focus_dim,
        origin,
        treatment,
        cell_bg_opacity,
        opaque_region,
        chrome_pin,
        1.0,
    );
}

/// SELECTION-OPACITY: ligature-aware build that threads `selection_opacity` as a
/// COLOR-space tint strength for selected cells. A selected cell composites at
/// the same surface alpha as its unselected neighbors (`cell_bg_opacity`), so
/// selection and content share one transparency plane and the selection never
/// couples inversely to window opacity; `selection_opacity` blends the selection
/// fill toward the unselected background (`composite_over`) instead of scaling
/// the surface alpha. `selection_opacity == 1.0` keeps the selection at full
/// strength; lower values let the cell's own background show through the tint.
/// Frames with no selected cell are byte-identical to
/// [`build_cell_vertices_with_focus_dim_origin_and_ligatures_into`].
#[allow(clippy::too_many_arguments)]
pub fn build_cell_vertices_with_ligatures_and_selection_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    ligature_runs: &[LigatureRun],
    focus_dim: f32,
    origin: [f32; 2],
    treatment: BackgroundTreatmentParams,
    cell_bg_opacity: f32,
    opaque_region: Option<CellRegion>,
    chrome_pin: ChromePin,
    selection_opacity: f32,
) {
    build_cells_core(
        out,
        snapshot,
        atlas,
        color_runs,
        ligature_runs,
        focus_dim,
        origin,
        treatment,
        cell_bg_opacity,
        opaque_region,
        chrome_pin,
        selection_opacity,
    );
}

/// Private core of the cell-vertex build. Carries every render parameter,
/// including `selection_opacity` (see [`crate::core::Attrs::selected`]), which
/// drives BOTH a selected cell's COLOR-space tint strength AND its background
/// surface alpha. The surface alpha is lerped from the surrounding content
/// opacity up to fully opaque as the knob rises —
/// `A_sel = cell_bg_opacity + selection_opacity * (1.0 - cell_bg_opacity)` —
/// so the selection PUNCHES THROUGH window transparency and stays visible, and
/// is never weaker than its surround (`A_sel >= cell_bg_opacity` always). The
/// color tint recedes toward the unselected fill as the knob falls, in lockstep
/// with the surface alpha. All public entry points delegate here; the ones that
/// do not thread a selection opacity pass `1.0`, the full-strength selection
/// contract: `A_sel == 1.0` at every window opacity, so a selected cell is
/// byte-identical to the original fully-opaque selection at ANY window opacity.
/// Frames with no selected cell are byte-identical for any scalar, since both
/// the tint and the alpha lerp are only taken when a cell carries the marker.
#[allow(clippy::too_many_arguments)]
fn build_cells_core(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    ligature_runs: &[LigatureRun],
    focus_dim: f32,
    origin: [f32; 2],
    treatment: BackgroundTreatmentParams,
    cell_bg_opacity: f32,
    opaque_region: Option<CellRegion>,
    chrome_pin: ChromePin,
    selection_opacity: f32,
) {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    let baseline = atlas.cell.baseline as f32;
    // SCROLL-CHROME-BOUNCE: the pinned-chrome seam for this build (inert unless a
    // glide is in flight — then the top bar/rail stay put while content glides).
    let chrome_seam_y = chrome_pin.seam_y(origin[1], cell_h);

    let needed = rows * cols * VERTS_PER_QUAD * 2;
    out.clear();
    if out.capacity() < needed {
        out.reserve(needed - out.capacity());
    }

    // Effective foreground/background after inverse + dim, and the column span of
    // a wide lead cell. Computed identically in both passes. `row`/`col` are the
    // cell's grid position, needed by the ID3/U5 background treatment (gradient /
    // vignette) which modulates the background by position.
    let resolve = |cell: &crate::core::Cell, row: usize, col: usize| -> ([f32; 4], [f32; 4]) {
        let mut fg = foreground_linear(&snapshot.colors, cell.attrs.foreground);
        let mut bg = background_linear(&snapshot.colors, cell.attrs.background);
        if cell.attrs.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        // SELECTION-OPACITY (default/inverse path): a selected cell painted via
        // the historical inverse swap blends its selection COLORS toward the
        // UNSELECTED appearance in linear color space, so `selection_opacity`
        // controls the selection's tint STRENGTH. The selected cell's surface
        // alpha is handled separately in Pass 1 (lerped from content opacity up
        // to fully opaque by the same knob), so tint and surface alpha recede
        // toward the unselected look together as the knob falls, and the
        // selection punches through window transparency as the knob rises.
        // After the swap `bg` is the selection fill (the cell's original
        // foreground) and `fg` is the backdrop (the cell's original background),
        // so compositing the fill over the backdrop at `selection_opacity` — and
        // the text symmetrically back toward the unselected foreground — recedes
        // the whole cell toward its unselected look as the knob falls. At 1.0 the
        // `composite_over` endpoints are exact, so a fully-opaque selection is
        // byte-identical to the plain swap; the themed path pre-composites its
        // fill upstream (`themed_selection_style`) and needs no blend here.
        if selection_opacity < 1.0 && cell.attrs.selected() && cell.attrs.inverse() {
            let fill = [bg[0], bg[1], bg[2]];
            let backdrop = [fg[0], fg[1], fg[2]];
            let blended_bg = crate::color::composite_over(fill, backdrop, selection_opacity);
            let blended_fg = crate::color::composite_over(backdrop, fill, selection_opacity);
            bg = [blended_bg[0], blended_bg[1], blended_bg[2], bg[3]];
            fg = [blended_fg[0], blended_fg[1], blended_fg[2], fg[3]];
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
        // ID3/U5 background treatment (gradient / vignette): modulate the cell
        // background by its grid position. Applied AFTER focus dimming and
        // BEFORE the RV1 floor, so the floor sees the treated per-cell
        // background and re-lifts the foreground to keep contrast — readability
        // is preserved by construction, per cell. `treatment.active() == false`
        // (kind None or zero strength, the default) skips this entirely, so the
        // plain/fast path stays byte-identical.
        if treatment.active() && !chrome_pin.is_chrome(row, col) {
            bg = treatment.apply_to(bg, row, col, rows, cols);
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
    let mut ligature_index = 0;
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }
            let (_, bg) = resolve(cell, row, col);
            // ID3/U5 image background: scale ONLY the background-quad alpha by
            // `cell_bg_opacity` so a background image shows through behind text.
            // `1.0` (the default) yields `bg[3] * 1.0 == bg[3]` — byte-identical.
            // The floor reference inside `resolve` keeps the OPAQUE bg (alpha
            // untouched there), so `enforce_contrast_rgba` still floors against
            // the theme background `l_bg`; the readability scrim guarantees the
            // composited luminance stays on the safe side of `l_bg`.
            // TRANSPARENCY (MENU-OPACITY): a cell inside `opaque_region`
            // (an overlay panel painted into a translucent snapshot) forces its
            // background fully opaque so the panel reads as a solid surface; every
            // other cell scales by `cell_bg_opacity` exactly as before. `None`
            // (the default) leaves `bg[3] * cell_bg_opacity` untouched.
            //
            // SELECTION-OPACITY: a selected cell's background surface alpha is
            // lerped from the surrounding content opacity up to fully opaque,
            // driven by the knob:
            //   A_sel = cell_bg_opacity + selection_opacity * (1 - cell_bg_opacity)
            // so the selection PUNCHES THROUGH window transparency and stays
            // visible against a translucent/busy backdrop. It is monotonic in the
            // knob and `A_sel >= cell_bg_opacity` always, so the selection is
            // NEVER weaker than its surround; the excess over the surround
            // (`k * (1 - cell_bg_opacity)`) grows as the window gets more
            // transparent and is zero at an opaque window (equal-plane solid
            // highlight) — no inverse feel in either direction. At k == 1.0
            // A_sel == 1.0 at EVERY window opacity, so the cell is byte-identical
            // to the original fully-opaque selection. Applies to BOTH the inverse
            // and themed paths (both carry the `selected()` marker). The color
            // tint recedes in lockstep (see `resolve` for the inverse path and
            // `themed_selection_style` for the themed path). An `opaque_region`
            // overlay cell still forces fully opaque, unchanged.
            let cell_opacity = if opaque_region.is_some_and(|r| r.contains(row, col)) {
                1.0
            } else if cell.attrs.selected() {
                cell_bg_opacity + selection_opacity * (1.0 - cell_bg_opacity)
            } else {
                cell_bg_opacity
            };
            let bg = [bg[0], bg[1], bg[2], bg[3] * cell_opacity];
            let span = span_of(row, col);
            let x0 = origin[0] + col as f32 * cell_w + chrome_pin.cell_dx(col);
            let y0 = chrome_pin.cell_top_y(origin[1], cell_h, row, col);
            let (bg_top, bg_bottom) =
                content_bg_span(&chrome_pin, chrome_seam_y, y0, cell_h, row, col);
            push_quad(
                out,
                [x0, bg_top, x0 + cell_w * span, bg_bottom],
                [0.0, 0.0, 0.0, 0.0],
                bg,
                0.0,
            );
        }
    }

    // Pass 2: glyph quads (sized from bearing-aware atlas bounds, so overflow ink
    // renders uncropped) plus underline/strikethrough decorations, all drawn over
    // every background from pass 1.
    //
    // Color-run coverage is answered from a per-cell mask built once in
    // O(cells / 64 + runs) instead of scanning the run list for every cell;
    // the answers, and therefore the emitted vertices, are identical.
    let color_coverage = ColorRunCoverage::new(color_runs, cols, rows);
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }
            let (fg, bg) = resolve(cell, row, col);
            let span = span_of(row, col);
            let x0 = origin[0] + col as f32 * cell_w + chrome_pin.cell_dx(col);
            let y0 = chrome_pin.cell_top_y(origin[1], cell_h, row, col);
            let decoration_y0 = y0 + chrome_pin.glyph_center_dy(row, col, cell_h);

            while ligature_runs
                .get(ligature_index)
                .is_some_and(|run| run.row < row || (run.row == row && run.end <= col))
            {
                ligature_index += 1;
            }
            let ligature = ligature_runs
                .get(ligature_index)
                .filter(|run| run.covers(row, col))
                .filter(|run| {
                    run.glyphs
                        .iter()
                        .all(|glyph| atlas.contains_shaped(glyph.key))
                });
            if !cell.attrs.hidden()
                && cell.ch != ' '
                && !color_coverage.covers(row, col)
                && ligature.is_none()
                && let Some(bounds) =
                    atlas.glyph_quad_styled(font_style_for_attrs(&cell.attrs), cell.ch)
            {
                // SCROLL-CHROME-BOUNCE: a content glyph gliding up under the
                // pinned bar is cropped at the seam (UV, not squash); chrome
                // glyphs and the inert path draw uncropped (byte-identical).
                if chrome_pin.active() && chrome_pin.top_rows > 0 && !chrome_pin.is_chrome(row, col)
                {
                    push_glyph_quad_clipped_top(out, x0, y0, bounds, fg, chrome_seam_y);
                } else {
                    // TAB-LABEL-CENTERING: a chrome band label rides a sub-cell Y
                    // shift so a multi-row bar's/slot's single label line lands on
                    // the band's true pixel center. `0.0` (content cells, single-
                    // row / odd-height bands) leaves the glyph exactly where the
                    // row-snap placed it, so the plain path is byte-identical.
                    push_glyph_quad(out, x0, decoration_y0, bounds, fg);
                }
            }

            // Zero-width combining marks stored on the cell draw over the base
            // glyph at the same cell origin: each mark rasterized with a
            // one-cell pen anchor so its (left-hanging) ink lands on the base
            // cell (`combining_mark_quad`, which never yields the tofu box).
            // Marks draw even over a space base (a mark can attach to one) and
            // follow the base glyph's seam-clipping rule. Wide bases and
            // stacked multi-mark clusters render at the same single-cell
            // anchor — a bounded approximation, not full mark positioning.
            if !cell.attrs.hidden() && !color_coverage.covers(row, col) && ligature.is_none() {
                for &mark in cell.combining() {
                    let Some(bounds) =
                        atlas.combining_mark_quad(font_style_for_attrs(&cell.attrs), mark)
                    else {
                        continue;
                    };
                    if chrome_pin.active()
                        && chrome_pin.top_rows > 0
                        && !chrome_pin.is_chrome(row, col)
                    {
                        push_glyph_quad_clipped_top(out, x0, y0, bounds, fg, chrome_seam_y);
                    } else {
                        push_glyph_quad(out, x0, decoration_y0, bounds, fg);
                    }
                }
            }

            // Emit a contextual span once from its first source column. Its
            // atlas keys carry only source span/anchor data; shaped advances
            // never move grid columns. Horizontal clipping prevents ink from
            // reaching the following logical cell or an adjacent pane.
            if let Some(run) = ligature.filter(|run| run.start == col) {
                // CHROME-GAP: the run shares its lead cell's horizontal shift
                // (a shaping run never crosses the rail↔content seam — the band
                // and content carry distinct attrs — so one dx spans the run).
                let run_dx = chrome_pin.cell_dx(run.start);
                let span_x0 = origin[0] + run.start as f32 * cell_w + run_dx;
                let span_x1 = origin[0] + run.end as f32 * cell_w + run_dx;
                let grid_top = if chrome_pin.active()
                    && chrome_pin.top_rows > 0
                    && !chrome_pin.is_chrome(row, col)
                {
                    chrome_seam_y
                } else {
                    origin[1]
                };
                // CHROME-GAP: the content grid's pixel bottom rides the same
                // below-band shift its cells do (0.0 for chrome and no-gap).
                let grid_bottom = origin[1] + rows as f32 * cell_h + chrome_pin.cell_dy(row, col);
                for glyph in run.glyphs.iter() {
                    if let Some(bounds) = atlas.shaped_glyph_quad(glyph.key) {
                        push_glyph_quad_clipped_rect(
                            out,
                            span_x0,
                            decoration_y0,
                            bounds,
                            fg,
                            [span_x0, grid_top, span_x1, grid_bottom],
                        );
                    }
                }
            }

            let underline_style = cell.attrs.effective_underline_style();
            if underline_style != UnderlineStyle::None {
                // U1: explicit SGR underline color must clear the same RV1
                // minimum-contrast floor as every other foreground ink. The
                // `None` case already maps to the floored cell `fg`; the explicit
                // case is routed through the identical `enforce_contrast_rgba`
                // (OKLab-L bisect, hue+chroma preserved, exact passthrough at the
                // default floor of 1.0 so plain frames stay byte-identical).
                let underline_color = cell.attrs.underline_color.map_or(fg, |color| {
                    text::enforce_contrast_rgba(foreground_linear(&snapshot.colors, color), bg)
                });
                push_underline_decoration(
                    out,
                    underline_style,
                    x0,
                    decoration_y0,
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

/// Maximum background-luminance attenuation an ID3/U5 treatment may apply, at
/// full strength and the farthest falloff point. The cap keeps the effect
/// subtle by construction; the RV1 floor (applied immediately after, on the
/// treated background) is the hard readability guarantee regardless.
pub const MAX_BG_TREATMENT_DARKEN: f32 = 0.55;

/// Which ID3/U5 background treatment is active. [`BackgroundTreatment::None`]
/// (the default) is the identity — the treatment block is skipped entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundTreatment {
    /// No treatment — background drawn exactly as resolved (default).
    #[default]
    None,
    /// Vertical gradient: the background darkens smoothly toward the bottom rows.
    Gradient,
    /// Radial vignette: the background darkens toward the edges and corners.
    Vignette,
}

/// ID3/U5 readability-safe background-treatment parameters, applied per cell to
/// the resolved background color in [`build_cell_vertices_with_focus_dim_and_origin_into`].
///
/// The treatment runs **before** the RV1 minimum-contrast floor, so the floor
/// sees the treated per-cell background and re-lifts the foreground to keep
/// contrast above the configured ratio — readability is preserved by
/// construction, per cell. The [`Default`] is the identity (`kind = None`,
/// `strength = 0.0`), for which [`Self::active`] is `false`, the apply block is
/// skipped, and the rendered frame is byte-identical to the pre-feature
/// renderer. Lives here (not in the native overlay registry) because
/// [`build_cell_vertices_with_focus_dim_and_origin_into`] — a `crate::grid`
/// function — must name it to apply the fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundTreatmentParams {
    /// The spatial treatment function.
    pub kind: BackgroundTreatment,
    /// Treatment strength in `0.0..=1.0`. Scales the maximum attenuation
    /// ([`MAX_BG_TREATMENT_DARKEN`]). `0.0` ⇒ inactive (identity).
    pub strength: f32,
}

impl Default for BackgroundTreatmentParams {
    fn default() -> Self {
        Self {
            kind: BackgroundTreatment::None,
            strength: 0.0,
        }
    }
}

impl BackgroundTreatmentParams {
    /// True when the treatment will actually modify some cell. The identity
    /// (`None` kind, or zero/negative strength) is `false`, so the per-cell
    /// apply block is skipped and the frame stays byte-identical.
    pub fn active(&self) -> bool {
        !matches!(self.kind, BackgroundTreatment::None) && self.strength > 0.0
    }

    /// Per-cell attenuation factor in `0.0..=1.0` (0 = unchanged, 1 = farthest
    /// point of the treatment). Pure and total.
    fn falloff(&self, row: usize, col: usize, rows: usize, cols: usize) -> f32 {
        match self.kind {
            BackgroundTreatment::None => 0.0,
            BackgroundTreatment::Gradient => {
                if rows <= 1 {
                    0.0
                } else {
                    (row as f32 / (rows - 1) as f32).clamp(0.0, 1.0)
                }
            }
            BackgroundTreatment::Vignette => {
                // Normalized radial distance from the grid center: 0 at the
                // center cell, 1 at the farthest corner.
                let cx = (cols as f32 - 1.0) * 0.5;
                let cy = (rows as f32 - 1.0) * 0.5;
                let dx = col as f32 - cx;
                let dy = row as f32 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let maxd = (cx * cx + cy * cy).sqrt();
                if maxd <= 0.0 {
                    0.0
                } else {
                    (dist / maxd).clamp(0.0, 1.0)
                }
            }
        }
    }

    /// Apply the treatment to a linear-RGBA background color at grid position
    /// `(row, col)`. Pure; returns `bg` unchanged when inactive or at a
    /// zero-falloff cell. Only luminance is attenuated; alpha is preserved.
    pub fn apply_to(
        &self,
        bg: [f32; 4],
        row: usize,
        col: usize,
        rows: usize,
        cols: usize,
    ) -> [f32; 4] {
        if !self.active() {
            return bg;
        }
        let f = self.falloff(row, col, rows, cols);
        if f <= 0.0 {
            return bg;
        }
        let atten = 1.0 - (self.strength.clamp(0.0, 1.0) * MAX_BG_TREATMENT_DARKEN * f);
        [bg[0] * atten, bg[1] * atten, bg[2] * atten, bg[3]]
    }
}

/// Visual-only parameters applied to cursor geometry. The [`Default`] is the
/// focused, fully opaque, zero-offset identity.
///
/// - `offset` — sub-cell pixel shift added to the cursor's cell origin
///   (VE4-slide). Default `[0.0, 0.0]` ⇒ unchanged `x0`/`y0`.
/// - `alpha` — multiplier on the cursor quad's color alpha (ID1-easing).
///   Default `1.0` ⇒ unchanged opacity. Polarity: `1.0` = fully opaque (today),
///   `0.0` = invisible — never default to `0.0`.
/// - `focused` — whether the window owns keyboard focus. An unfocused Block
///   cursor becomes a hollow outline; underline and bar styles are unchanged.
///
/// The type lives here (not in the native overlay registry) because
/// [`push_cursor`] — a `crate::grid` function — must name it to apply the
/// fields, and a `pub(in crate::native)` type is not visible from `crate::grid`.
/// The native layer re-exports it; this is the contained grid-level change the
/// foundation scope anticipated for alpha application.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorRenderParams {
    pub offset: [f32; 2],
    pub alpha: f32,
    pub focused: bool,
    /// Suppress the ordinary destination cursor while the large-jump follower
    /// owns the presentation. Terminal state and glyph content remain live.
    pub follower_active: bool,
}

impl Default for CursorRenderParams {
    fn default() -> Self {
        Self {
            offset: [0.0, 0.0],
            alpha: 1.0,
            focused: true,
            follower_active: false,
        }
    }
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
        CursorRenderParams::default(),
    );
}

pub fn append_cursor_vertices_with_origin(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    origin: [f32; 2],
    params: CursorRenderParams,
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
        origin,
        params,
    );
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

/// Crop a coverage glyph to a pixel rectangle by adjusting UVs, never by
/// squashing geometry. Used by multi-cell contextual glyphs so their ink stays
/// inside the logical source span and pane/grid bounds.
fn push_glyph_quad_clipped_rect(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    bounds: GlyphBounds,
    color: [f32; 4],
    clip: [f32; 4],
) {
    let mut gx0 = x0 + bounds.offset_x as f32;
    let mut gy0 = y0 + bounds.offset_y as f32;
    let mut gx1 = gx0 + bounds.width as f32;
    let mut gy1 = gy0 + bounds.height as f32;
    let [mut u0, mut v0, mut u1, mut v1] = bounds.uv;
    let original_uv = bounds.uv;
    let original_w = gx1 - gx0;
    let original_h = gy1 - gy0;
    if original_w <= 0.0
        || original_h <= 0.0
        || gx1 <= clip[0]
        || gx0 >= clip[2]
        || gy1 <= clip[1]
        || gy0 >= clip[3]
    {
        return;
    }
    if gx0 < clip[0] {
        let t = (clip[0] - gx0) / original_w;
        u0 = original_uv[0] + t * (original_uv[2] - original_uv[0]);
        gx0 = clip[0];
    }
    if gx1 > clip[2] {
        let t = (gx1 - clip[2]) / original_w;
        u1 = original_uv[2] - t * (original_uv[2] - original_uv[0]);
        gx1 = clip[2];
    }
    if gy0 < clip[1] {
        let t = (clip[1] - gy0) / original_h;
        v0 = original_uv[1] + t * (original_uv[3] - original_uv[1]);
        gy0 = clip[1];
    }
    if gy1 > clip[3] {
        let t = (gy1 - clip[3]) / original_h;
        v1 = original_uv[3] - t * (original_uv[3] - original_uv[1]);
        gy1 = clip[3];
    }
    push_quad(out, [gx0, gy0, gx1, gy1], [u0, v0, u1, v1], color, 1.0);
}

/// SCROLL-CHROME-BOUNCE: push a coverage glyph whose top is cropped at
/// `clip_top_y` via a UV adjustment (never a squash), so a content glyph gliding
/// up under the pinned tab bar cannot paint into the chrome band. Glyphs entirely
/// above the seam are dropped.
fn push_glyph_quad_clipped_top(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    bounds: GlyphBounds,
    color: [f32; 4],
    clip_top_y: f32,
) {
    let gx0 = x0 + bounds.offset_x as f32;
    let mut gy0 = y0 + bounds.offset_y as f32;
    let gx1 = gx0 + bounds.width as f32;
    let gy1 = gy0 + bounds.height as f32;
    if gy1 <= clip_top_y {
        return;
    }
    let [u0, mut v0, u1, v1] = bounds.uv;
    if gy0 < clip_top_y {
        let t = (clip_top_y - gy0) / (gy1 - gy0);
        v0 += t * (v1 - v0);
        gy0 = clip_top_y;
    }
    push_quad(out, [gx0, gy0, gx1, gy1], [u0, v0, u1, v1], color, 1.0);
}

/// SCROLL-CHROME-BOUNCE: color-glyph analogue of [`push_glyph_quad_clipped_top`]
/// — crop the emoji quad's top at the seam via UV so a gliding color glyph never
/// paints into the pinned tab bar.
fn push_color_glyph_quad_clipped_top(
    out: &mut Vec<ColorGlyphVertex>,
    x0: f32,
    y0: f32,
    x1: f32,
    pixel_height: f32,
    uv: [f32; 4],
    clip_top_y: f32,
) {
    let mut gy0 = y0;
    let gy1 = y0 + pixel_height;
    if gy1 <= clip_top_y {
        return;
    }
    let [u0, mut v0, u1, v1] = uv;
    if gy0 < clip_top_y {
        let t = (clip_top_y - gy0) / (gy1 - gy0);
        v0 += t * (v1 - v0);
        gy0 = clip_top_y;
    }
    push_color_glyph_quad(out, [x0, gy0, x1, gy1], [u0, v0, u1, v1]);
}

/// PANE-SUBCELL-CLIP: a vertical clip band `[top_y, bottom_y)` in physical
/// pixels, applied to a pane's already-built vertex quads so a sub-cell scroll
/// glide baked into the pane origin cannot smear the partial top/bottom row past
/// the pane's own content rect into a neighbouring pane across the 1px divider.
///
/// [`Self::NONE`] (an infinite band) is the inert value every non-gliding and
/// single-pane caller passes: [`clip_quads_vertical`] returns immediately, so
/// the at-rest and single-pane frames stay byte-identical. This is the analogue
/// of — but larger than — the chrome-seam clamp ([`content_bg_span`] /
/// [`push_glyph_quad_clipped_top`]), which only pins whole chrome rows at the
/// top seam; this clamps arbitrary partial rows at either edge of a pane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VClip {
    /// Top of the visible band; quad geometry above it is cropped (UV-adjusted,
    /// never squashed). `f32::NEG_INFINITY` disables the top clamp.
    pub top_y: f32,
    /// Bottom of the visible band (exclusive edge); quad geometry below it is
    /// cropped. `f32::INFINITY` disables the bottom clamp.
    pub bottom_y: f32,
}

impl VClip {
    /// Inert band (covers everything): [`clip_quads_vertical`] is a no-op, so
    /// every non-gliding / single-pane caller keeps its frame byte-identical.
    pub const NONE: Self = Self {
        top_y: f32::NEG_INFINITY,
        bottom_y: f32::INFINITY,
    };

    /// Whether the band actually clips anything this frame (a sub-cell glide is
    /// in flight). `NONE` reports `false`, gating the whole clamp off.
    #[inline]
    pub fn active(&self) -> bool {
        self.top_y != f32::NEG_INFINITY || self.bottom_y != f32::INFINITY
    }
}

/// A quad vertex whose vertical position and vertical UV can be clamped by
/// [`clip_quads_vertical`]. Abstracts over the mono [`Vertex`] (atlas coverage +
/// solid backgrounds) and the [`ColorGlyphVertex`] (emoji), so one clip routine
/// serves both pane vertex streams. The horizontal axis and color are never
/// touched — the clip is purely vertical.
pub(crate) trait ClipQuadVertex {
    fn clip_y(&self) -> f32;
    fn set_clip_y(&mut self, y: f32);
    fn clip_v(&self) -> f32;
    fn set_clip_v(&mut self, v: f32);
}

impl ClipQuadVertex for Vertex {
    #[inline]
    fn clip_y(&self) -> f32 {
        self.pos[1]
    }
    #[inline]
    fn set_clip_y(&mut self, y: f32) {
        self.pos[1] = y;
    }
    #[inline]
    fn clip_v(&self) -> f32 {
        self.uv[1]
    }
    #[inline]
    fn set_clip_v(&mut self, v: f32) {
        self.uv[1] = v;
    }
}

impl ClipQuadVertex for ColorGlyphVertex {
    #[inline]
    fn clip_y(&self) -> f32 {
        self.pos[1]
    }
    #[inline]
    fn set_clip_y(&mut self, y: f32) {
        self.pos[1] = y;
    }
    #[inline]
    fn clip_v(&self) -> f32 {
        self.uv[1]
    }
    #[inline]
    fn set_clip_v(&mut self, v: f32) {
        self.uv[1] = v;
    }
}

/// PANE-SUBCELL-CLIP: clamp every axis-aligned quad in `verts` to the vertical
/// band `clip`, cropping a partial row's overhang via a UV adjustment (never a
/// squash) exactly like [`push_glyph_quad_clipped_top`] does at the chrome seam.
/// A quad entirely outside the band collapses to zero height (emits no
/// fragments) rather than being removed, so the vertex COUNT is preserved — the
/// caller's background/glyph segment split (keyed on a fixed background vertex
/// count) stays valid.
///
/// Inert when `clip` is [`VClip::NONE`] (the fast-path return), so at-rest and
/// single-pane frames are byte-identical. Operates on the emitted geometry, so
/// one pass serves backgrounds, coverage glyphs, colour glyphs, the cursor, and
/// per-pane overlays uniformly. The quad vertex order is the fixed
/// `[tl, bl, tr, tr, bl, br]` [`push_quad`] emits: indices 0/2/3 are the top
/// edge, 1/4/5 the bottom.
pub(crate) fn clip_quads_vertical<V: ClipQuadVertex>(verts: &mut [V], clip: VClip) {
    if !clip.active() {
        return;
    }
    for quad in verts.chunks_exact_mut(VERTS_PER_QUAD) {
        let y0 = quad[0].clip_y();
        let y1 = quad[1].clip_y();
        let height = y1 - y0;
        if height <= 0.0 {
            // Already degenerate (or malformed) — nothing to clamp.
            continue;
        }
        // Entirely below the band: collapse to the bottom edge (zero height).
        if y0 >= clip.bottom_y {
            for v in quad.iter_mut() {
                v.set_clip_y(clip.bottom_y);
            }
            continue;
        }
        // Entirely above the band: collapse to the top edge (zero height).
        if y1 <= clip.top_y {
            for v in quad.iter_mut() {
                v.set_clip_y(clip.top_y);
            }
            continue;
        }
        let v0 = quad[0].clip_v();
        let v1 = quad[1].clip_v();
        // Crop the overhang above the band, pulling the top edge down to the
        // seam and advancing its UV so the glyph is cropped, not scaled.
        if y0 < clip.top_y {
            let t = (clip.top_y - y0) / height;
            let nv = v0 + t * (v1 - v0);
            for &i in &[0usize, 2, 3] {
                quad[i].set_clip_y(clip.top_y);
                quad[i].set_clip_v(nv);
            }
        }
        // Crop the overhang below the band (the sub-cell smear that would cross
        // the divider), pulling the bottom edge up to the seam.
        if y1 > clip.bottom_y {
            let t = (clip.bottom_y - y0) / height;
            let nv = v0 + t * (v1 - v0);
            for &i in &[1usize, 4, 5] {
                quad[i].set_clip_y(clip.bottom_y);
                quad[i].set_clip_v(nv);
            }
        }
    }
}

/// PANE-SUBCELL-CLIP: fill the sub-cell gap a downward glide opens at the top of
/// a pane by pulling the FIRST rendered row's background quads up to `top_y`,
/// mirroring the chrome-seam [`content_bg_span`] first-row flush. When a pane's
/// origin is shifted down by a sub-cell `frac`, its top row starts `frac` px
/// below the pane's content top, exposing a thin strip; extending the first
/// row's backgrounds (not glyphs) up to the content top paints that strip in the
/// row's own background instead of the clear/wallpaper colour.
///
/// `bg_verts` is the background segment only (glyphs excluded); `row0_quads` is
/// the count of non-continuation cells in the snapshot's first row (its
/// background quads lead the segment in row-major order). Only the top edge is
/// moved (indices 0/2/3), and only upward, so a quad already at or above `top_y`
/// is untouched and the at-rest / single-pane path (never called) is unaffected.
pub(crate) fn extend_first_row_bg_to_top(bg_verts: &mut [Vertex], row0_quads: usize, top_y: f32) {
    let end = (row0_quads * VERTS_PER_QUAD).min(bg_verts.len());
    for quad in bg_verts[..end].chunks_exact_mut(VERTS_PER_QUAD) {
        for &i in &[0usize, 2, 3] {
            if quad[i].pos[1] > top_y {
                quad[i].pos[1] = top_y;
            }
        }
    }
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
/// - **Block, focused**: an **inverse** block — a background quad in the cell's
///   foreground color with the cell's glyph (if any) redrawn on top in the
///   cell's background color, keeping the character readable under the cursor.
/// - **Block, unfocused**: four one-pixel cursor-color border quads. The cell's
///   existing glyph remains untouched in its normal foreground color.
/// - **Underline**: a thin foreground-colored bar at the cell's bottom edge,
///   drawn over the cell's existing glyph (no inversion).
/// - **Bar**: a thin foreground-colored vertical bar at the cell's left edge,
///   drawn over the cell's existing glyph (no inversion).
///
/// A hidden cursor (`cursor_visible == false`, which the renderer also uses for
/// the blink "off" phase) emits nothing. The position is clamped to the grid so
/// a stale snapshot can never index out of bounds. Reflects only the live
/// snapshot cursor — no scrollback/viewport offset is applied here.
// Wave-15b adds `params` as the 8th argument; the geometry inputs (dimensions,
// style, origin) are already discrete and bundling them would obscure the
// call sites more than it would help. Matches `push_underline_decoration`.
#[allow(clippy::too_many_arguments)]
fn push_cursor(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cell_w: f32,
    cell_h: f32,
    style: CursorStyle,
    origin: [f32; 2],
    params: CursorRenderParams,
) {
    if !snapshot.cursor_visible || params.follower_active {
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

    // VE4-slide: additive sub-cell shift. Default `[0.0, 0.0]` ⇒ identity.
    let x0 = origin[0] + col as f32 * cell_w + params.offset[0];
    let y0 = origin[1] + row as f32 * cell_h + params.offset[1];

    match style {
        CursorStyle::Block => {
            let mut block_color = rgb_linear(snapshot.colors.cursor);
            block_color[3] *= params.alpha;
            if !params.focused {
                let thickness = 1.0_f32.min(cell_w).min(cell_h);
                let x1 = x0 + cell_w;
                let y1 = y0 + cell_h;
                for rect in [
                    [x0, y0, x1, y0 + thickness],
                    [x0, y1 - thickness, x1, y1],
                    [x0, y0, x0 + thickness, y1],
                    [x1 - thickness, y0, x1, y1],
                ] {
                    push_solid_quad(
                        out,
                        SolidQuad {
                            rect,
                            color: block_color,
                        },
                    );
                }
                return;
            }
            // The under-cursor glyph is drawn in the cell's background color over
            // the cursor block; apply the RV1 floor so it stays legible against
            // the block (the relevant pair here is glyph-vs-block, since `fg` is
            // not drawn in this path). Passthrough at the default floor of 1.0.
            //
            // R6 ordering: derive the glyph color from the OPAQUE block color
            // BEFORE the ID1-easing alpha fade, so a fading cursor never drags
            // the under-glyph through a transient contrast violation. The glyph
            // itself is NEVER alpha-faded — only the block is.
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
            // The under-cursor glyph keeps its combining marks too, drawn in
            // the same contrast-derived color as the base (see the content
            // pass for the anchoring rule).
            if !cell.attrs.hidden() {
                for &mark in cell.combining() {
                    if let Some(bounds) =
                        atlas.combining_mark_quad(font_style_for_attrs(&cell.attrs), mark)
                    {
                        push_glyph_quad(out, x0, y0, bounds, glyph_color);
                    }
                }
            }
        }
        CursorStyle::Underline => {
            let mut color = rgb_linear(snapshot.colors.cursor);
            color[3] *= params.alpha;
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_underline_rect(x0, y0, cell_w, cell_h),
                    color,
                },
            );
        }
        CursorStyle::Bar => {
            let mut color = rgb_linear(snapshot.colors.cursor);
            color[3] *= params.alpha;
            push_solid_quad(
                out,
                SolidQuad {
                    rect: cursor_bar_rect(x0, y0, cell_w, cell_h),
                    color,
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

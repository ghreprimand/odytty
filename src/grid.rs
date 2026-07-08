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

        let x0 = origin[0] + run.column as f32 * cell_w;
        let y0 = origin[1] + run.row as f32 * cell_h;
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
            push_color_glyph_quad(
                out,
                [x0, y0, x1, y0 + bounds.pixel_height as f32],
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
        row < self.top_rows || (col >= self.rail_col_start && col < self.rail_col_end)
    }

    /// The un-shifted top-of-content Y (the seam below the pinned top bar) for a
    /// build whose `origin[1]` already carries `scroll_offset_y`.
    #[inline]
    fn seam_y(&self, shifted_origin_y: f32, cell_h: f32) -> f32 {
        (shifted_origin_y - self.scroll_offset_y) + self.top_rows as f32 * cell_h
    }

    /// The top-left Y of cell `(row, col)`: pinned (un-shifted) for chrome,
    /// glide-shifted for content. Inert (plain shifted origin) when `!active()`.
    #[inline]
    fn cell_top_y(&self, shifted_origin_y: f32, cell_h: f32, row: usize, col: usize) -> f32 {
        if self.active() && self.is_chrome(row, col) {
            (shifted_origin_y - self.scroll_offset_y) + row as f32 * cell_h
        } else {
            shifted_origin_y + row as f32 * cell_h
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
        if treatment.active() {
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
            let cell_opacity = if opaque_region.is_some_and(|r| r.contains(row, col)) {
                1.0
            } else {
                cell_bg_opacity
            };
            let bg = [bg[0], bg[1], bg[2], bg[3] * cell_opacity];
            let span = span_of(row, col);
            let x0 = origin[0] + col as f32 * cell_w;
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
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                continue;
            }
            let (fg, bg) = resolve(cell, row, col);
            let span = span_of(row, col);
            let x0 = origin[0] + col as f32 * cell_w;
            let y0 = chrome_pin.cell_top_y(origin[1], cell_h, row, col);

            if !cell.attrs.hidden()
                && cell.ch != ' '
                && !has_color_glyph_run(color_runs, row, col)
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
                    push_glyph_quad(out, x0, y0, bounds, fg);
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

/// Visual-only animation parameters applied to the cursor quad (Wave-15b
/// foundation). Each field is owned by exactly one Phase-4 cursor feature; the
/// [`Default`] is the identity, so today's render is byte-identical.
///
/// - `offset` — sub-cell pixel shift added to the cursor's cell origin
///   (VE4-slide). Default `[0.0, 0.0]` ⇒ unchanged `x0`/`y0`.
/// - `alpha` — multiplier on the cursor quad's color alpha (ID1-easing).
///   Default `1.0` ⇒ unchanged opacity. Polarity: `1.0` = fully opaque (today),
///   `0.0` = invisible — never default to `0.0`.
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
}

impl Default for CursorRenderParams {
    fn default() -> Self {
        Self {
            offset: [0.0, 0.0],
            alpha: 1.0,
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

    // VE4-slide: additive sub-cell shift. Default `[0.0, 0.0]` ⇒ identity.
    let x0 = origin[0] + col as f32 * cell_w + params.offset[0];
    let y0 = origin[1] + row as f32 * cell_h + params.offset[1];

    match style {
        CursorStyle::Block => {
            let mut block_color = rgb_linear(snapshot.colors.cursor);
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
            block_color[3] *= params.alpha;
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

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
//! For every non-continuation cell of a snapshot it emits a compact
//! **background-quad instance** covering the cell's pixel rectangle, and - when the cell
//! holds an inked, printable glyph - a **foreground quad** with the glyph's UV
//! rectangle from the atlas. Foreground quads carry `is_glyph = 1.0` so the
//! fragment shader samples the R8 coverage atlas as alpha; background quads
//! carry `is_glyph = 0.0` and use their solid color directly.
//!
//! Geometry is built in **physical pixel space** using the atlas cell metrics.
//! The vertex shader converts pixels to NDC via the viewport-size uniform, so a
//! window resize with unchanged cell metrics and layout only updates that
//! uniform - the geometry here is rebuilt only when the snapshot content,
//! metrics, or layout change.

mod background;
mod bidi;
mod clipping;
mod glyph_quads;
mod model;

pub use background::{BackgroundTreatment, BackgroundTreatmentParams, MAX_BG_TREATMENT_DARKEN};
pub use bidi::{BidiDisplayMap, BidiParagraphContext, max_bidi_context_rows};
pub use clipping::VClip;
pub(crate) use clipping::{
    clip_quads_to_rect, clip_quads_vertical, extend_first_row_bg_to_top, subtract_rects_from_quads,
};
#[cfg(test)]
use glyph_quads::push_color_glyph_quad;
pub use glyph_quads::{
    build_color_glyph_vertices_into, build_color_glyph_vertices_with_origin_into,
};
use glyph_quads::{push_cell_glyph, push_glyph_quad_clipped_rect};
pub use model::{
    ColorGlyphRun, ColorGlyphVertex, ColorRunCoverage, INSTANCES_PER_QUAD, RowFade, SolidQuad,
    VERTS_PER_QUAD, Vertex,
};
use model::{DIM_PERCEPTUAL_AMOUNT, LINE_DECORATION_THICKNESS_DIVISOR, push_quad};

use crate::core::{
    Attrs, Color, CursorStyle, DynamicColors, PLACEHOLDER_CHAR, RgbColor, Snapshot, UnderlineStyle,
};
#[cfg(test)]
use crate::emoji::ColorGlyphAtlas;
use crate::emoji::ColorGlyphKey;
use crate::ligature::LigatureRun;
use crate::text::{self, FontStyle, GlyphAtlas};

#[cfg(test)]
thread_local! {
    /// Resolutions of cell foreground/background during vertex rebuilds on
    /// this thread. Tests assert one call per lead cell per rebuild (both
    /// passes share it); per-thread counting keeps builds in concurrently
    /// running tests out of the count.
    pub(crate) static CELL_COLOR_RESOLVE_CALLS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Whether a cell's base character emits a coverage glyph quad.
///
/// Spaces never do. Kitty Unicode placeholders ([`PLACEHOLDER_CHAR`], U+10EEEE)
/// also never do: the char stays in the Snapshot so copy/paste, plain text, and
/// transcripts remain honest, but the atlas has no real outline for it - drawing
/// it would paint tofu under (or through) image tiles. Unresolved placeholders
/// therefore render as blank cells (background + decorations only), which is
/// preferable to tofu soup. Platform-neutral shared path.
#[inline]
fn cell_draws_base_glyph(ch: char) -> bool {
    ch != ' ' && ch != PLACEHOLDER_CHAR
}

/// Whether combining marks on this base should emit coverage quads.
///
/// Placeholder diacritics encode Kitty image/placement ids - metadata, not ink -
/// so they are suppressed along with the base. Ordinary cells (including space
/// bases that carry marks) still draw their combining marks.
#[inline]
fn cell_draws_combining_marks(ch: char) -> bool {
    ch != PLACEHOLDER_CHAR
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
/// [`DIM_PERCEPTUAL_AMOUNT`], preserving the alpha channel. The amount makes the
/// result equal the historical linear ×0.5 halving (a uniform OKLab scale is a
/// uniform linear scale, so hue is kept exactly), so dim text is as legible as
/// before.
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

/// Push one decoration rectangle, cropped to start at the chrome `seam` when
/// content is gliding under a pinned bar (the same crop glyph ink gets). A
/// rectangle wholly above the seam is dropped; `None` pushes it unchanged.
fn push_decoration_quad(out: &mut Vec<Vertex>, rect: [f32; 4], color: [f32; 4], seam: Option<f32>) {
    let [x0, y0, x1, y1] = rect;
    let rect = match seam {
        Some(seam) if y0 < seam => {
            if y1 <= seam {
                return;
            }
            [x0, seam, x1, y1]
        }
        _ => rect,
    };
    push_solid_quad(out, SolidQuad { rect, color });
}

/// The most segments one dashed, dotted or curly decoration may emit. A cell is
/// a few dozen pixels wide, so the bound is never reached by a real frame; it
/// keeps a non-finite or absurd width from looping or filling memory.
const MAX_DECORATION_SEGMENTS: usize = 4096;

fn push_segmented_line(
    out: &mut Vec<Vertex>,
    rect: [f32; 4],
    color: [f32; 4],
    painted: f32,
    gap: f32,
    seam: Option<f32>,
) {
    if !rect.iter().all(|value| value.is_finite()) {
        return;
    }
    let [x0, y0, x1, y1] = rect;
    let painted = painted.max(1.0);
    let gap = gap.max(1.0);
    let mut x = x0;
    for _ in 0..MAX_DECORATION_SEGMENTS {
        if x >= x1 {
            break;
        }
        let end = (x + painted).min(x1);
        if end > x {
            push_decoration_quad(out, [x, y0, end, y1], color, seam);
        }
        x += painted + gap;
    }
}

#[allow(clippy::too_many_arguments)]
fn push_double_underline(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    cell_w: f32,
    cell_h: f32,
    baseline: f32,
    color: [f32; 4],
    seam: Option<f32>,
) {
    let lower = underline_rect(x0, y0, cell_w, cell_h, baseline);
    let thickness = lower[3] - lower[1];
    let upper_y = (lower[1] - thickness * 2.0).max(y0);
    push_decoration_quad(
        out,
        [lower[0], upper_y, lower[2], upper_y + thickness],
        color,
        seam,
    );
    push_decoration_quad(out, lower, color, seam);
}

#[allow(clippy::too_many_arguments)]
fn push_curly_underline(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    cell_w: f32,
    cell_h: f32,
    baseline: f32,
    color: [f32; 4],
    seam: Option<f32>,
) {
    let base = underline_rect(x0, y0, cell_w, cell_h, baseline);
    let thickness = base[3] - base[1];
    let step = (thickness * 2.0).max(2.0);
    let upper_y = (base[1] - thickness).max(y0);
    let lower_y = (base[1] + thickness).min(y0 + cell_h - thickness);
    if !base.iter().all(|value| value.is_finite()) {
        return;
    }
    let mut x = base[0];
    let mut high = true;

    // Curly underline uses a small stepped square-wave approximation. It stays
    // in the existing solid-quad path while keeping the visual distinct from
    // straight, dashed, and dotted styles.
    for _ in 0..MAX_DECORATION_SEGMENTS {
        if x >= base[2] {
            break;
        }
        let end = (x + step).min(base[2]);
        let y = if high { upper_y } else { lower_y };
        push_decoration_quad(out, [x, y, end, y + thickness], color, seam);
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
    seam: Option<f32>,
) {
    let rect = underline_rect(x0, y0, cell_w, cell_h, baseline);
    let thickness = rect[3] - rect[1];
    match style {
        UnderlineStyle::None => {}
        UnderlineStyle::Straight => push_decoration_quad(out, rect, color, seam),
        UnderlineStyle::Double => {
            push_double_underline(out, x0, y0, cell_w, cell_h, baseline, color, seam)
        }
        UnderlineStyle::Curly => {
            push_curly_underline(out, x0, y0, cell_w, cell_h, baseline, color, seam)
        }
        UnderlineStyle::Dotted => {
            // One painted square, one square gap. The dot size follows line
            // thickness so small fonts stay crisp.
            push_segmented_line(out, rect, color, thickness, thickness, seam)
        }
        UnderlineStyle::Dashed => {
            // Six thickness units painted, three units gap. This is long enough
            // to read as a dash on small cells without spanning whole words.
            push_segmented_line(out, rect, color, thickness * 6.0, thickness * 3.0, seam)
        }
    }
}

/// Pixel-space strikethrough rectangle for one cell.
///
/// The line sits near the visual midline, derived from the atlas baseline so it
/// stays stable across font sizes and faces without needing per-glyph metrics.
pub fn strikethrough_rect(x0: f32, y0: f32, cell_w: f32, cell_h: f32, baseline: f32) -> [f32; 4] {
    let thickness = line_decoration_thickness(cell_h);
    // `min` then `max`, not `clamp`: a cell shorter than the line makes the
    // upper bound fall below the lower one, and `clamp` panics on that.
    let y = (y0 + baseline * 0.6)
        .round()
        .min(y0 + cell_h - thickness)
        .max(y0);
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
/// - A foreground quad is emitted for a printable resident glyph or its
///   fallback box. Spaces and controls emit no glyph. Non-ASCII and styled
///   glyphs use dynamically ensured atlas slots; combining marks draw at the
///   base-cell anchor.
pub fn build_vertices(snapshot: &Snapshot, atlas: &GlyphAtlas) -> Vec<Vertex> {
    // Admission comes first: nothing is reserved for a snapshot that cannot
    // be drawn, whatever its dimensions claim.
    let Some(reserve) = vertex_reserve(snapshot) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(reserve);
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
        // - the shipped default is 0.8 since v0.6.0, but this seam's contract is
        // opaque cells regardless of that default.
        1.0,
        // TEXT-BRIGHTNESS identity: this seam never carries the lift.
        1.0,
        // No overlay panel rides this seam, so no cell is force-opaque.
        None,
        // Off-screen `[0.0, 0.0]` origin build: no chrome to pin.
        ChromePin::NONE,
    );
}

/// TRANSPARENCY (MENU-OPACITY): force opaque backgrounds within this cell
/// region. In the single-pane path an open overlay panel is painted into the
/// snapshot, so marking its span keeps the panel surface opaque while other
/// cells scale with `cell_bg_opacity`. Coordinates are snapshot cell indices
/// after any tab-chrome decoration; `None` at the call site leaves every cell
/// at `cell_bg_opacity`.
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
            && col < self.left.saturating_add(self.width)
            && row >= self.top
            && row < self.top.saturating_add(self.height)
    }
}

/// Whether `snapshot` holds a cell for every position its dimensions claim.
/// Every public builder checks it once at admission and draws nothing for a
/// snapshot that does not (the dimension product is checked, so it cannot wrap),
/// instead of indexing past the cells. A snapshot with no columns or no rows
/// claims no grid at all: `Dimensions::new` never produces one, so it is
/// refused rather than walked as a number of empty rows.
pub(crate) fn snapshot_shape_is_valid(snapshot: &Snapshot) -> bool {
    let columns = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    columns > 0
        && rows > 0
        && columns
            .checked_mul(rows)
            .is_some_and(|cells| snapshot.cells.len() >= cells)
}

/// How many vertices a full build of `snapshot` can need, or `None` when the
/// snapshot is not admitted. Checked arithmetic, and only a snapshot that
/// passed [`snapshot_shape_is_valid`] reaches it, so the figure is backed by
/// cells that exist.
fn vertex_reserve(snapshot: &Snapshot) -> Option<usize> {
    if !snapshot_shape_is_valid(snapshot) {
        return None;
    }
    snapshot
        .dimensions
        .rows
        .checked_mul(snapshot.dimensions.columns)?
        .checked_mul(INSTANCES_PER_QUAD * 2)
}

/// TAB-LABEL-CENTERING: offset a label at integer `label_row` from its row
/// center to the center of a `band_rows`-tall band, in cell-height units
/// (multiply by the cell height for pixels; see
/// [`ChromePin::band_glyph_dy_rows`]). Zero for one-row and odd bands; a
/// 4-row band labelled at row 2 returns `-0.5`.
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
/// folds into the vertex Y. Without it the whole decorated single-pane snapshot - chrome rows included - glides with the scrollback, so the tab bar visibly
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
    /// touches chrome" - the window padding value, applied between the pinned
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
    /// (`rail_col_start == 0`) that is every non-rail column - content and the
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

    /// CHROME-GAP: the vertical shift of cell `(row, col)` - content rows below
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

    /// CHROME-GAP: the horizontal shift the CONTENT columns carry - `gap_x`
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
    /// PIXEL-SNAP: the composed shift is rounded to a whole physical pixel, so
    /// an odd cell height cannot put the label on a half-pixel origin and blur
    /// its baseline. Rounding moves it by at most half a pixel, leaving at
    /// least 1.5px of the two-pixel descender guard. Content cells keep the
    /// exact `0.0` arm.
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
    // TEXT-BRIGHTNESS: glyph-foreground lift toward white (`1.0` = exact
    // identity). Uniform across all mono-glyph ink this builder emits.
    text_brightness: f32,
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
        // COLORED-BG-FLOOR EXEMPT: this entry point serves chrome surfaces (the
        // floating rail strip) and fixed startup/overlay builds, whose effective
        // opacity is owned by `tab_panel_strength` / their own contract - the
        // floor rides only the content entry points below. Equal values are the
        // exact inert path in `build_cells_core`.
        cell_bg_opacity,
        text_brightness,
        opaque_region,
        chrome_pin,
        // No selection opacity threaded: selected cells (none on this seam)
        // take the full-strength selection tint. Byte-identical for the
        // no-selection callers.
        1.0,
        RowFade::NONE,
        None,
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
    // TEXT-BRIGHTNESS: glyph-foreground lift (`1.0` = exact identity).
    text_brightness: f32,
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
        // COLORED-BG-FLOOR EXEMPT: legacy/no-selection seam (initial blank
        // buffer, ligature harnesses) - equal values are the exact inert path.
        cell_bg_opacity,
        text_brightness,
        opaque_region,
        chrome_pin,
        1.0,
        RowFade::NONE,
        None,
    );
}

/// Ligature-aware build that threads `selection_opacity` for selected cells.
/// The knob drives both the selection fill's tint toward the unselected
/// background and the selected cell's surface alpha, which is lerped from
/// `cell_bg_opacity` up to fully opaque (see `build_cells_core`), so a
/// selection is never more transparent than its surround.
/// `selection_opacity == 1.0` keeps the selection at full strength and fully
/// opaque; lower values let the cell's own background show through the tint.
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
    // COLORED-BG-FLOOR: the surface alpha for cells whose RESOLVED background
    // differs from the theme default (see `build_cells_core`). Passing the same
    // value as `cell_bg_opacity` is the exact inert path (chrome strips do).
    colored_bg_opacity: f32,
    // TEXT-BRIGHTNESS: glyph-foreground lift (`1.0` = exact identity).
    text_brightness: f32,
    opaque_region: Option<CellRegion>,
    chrome_pin: ChromePin,
    selection_opacity: f32,
    // BIDI: display-order placement while `bidi_reorder` is on; `None` is the
    // unchanged logical-column path.
    bidi: Option<&BidiDisplayMap>,
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
        colored_bg_opacity,
        text_brightness,
        opaque_region,
        chrome_pin,
        selection_opacity,
        RowFade::NONE,
        bidi,
    );
}

/// VE4 new-output fade: the full-parameter build used by the single-pane GPU
/// path - [`build_cell_vertices_with_ligatures_and_selection_into`] plus a
/// per-row FOREGROUND alpha ramp for freshly arrived rows. `row_fade` scales
/// only pass-2 ink (glyphs, combining marks, ligature runs, underline and
/// strikethrough decorations); pass-1 cell backgrounds are untouched, so a
/// fading row's backdrop renders exactly as normal from its first frame.
/// [`RowFade::NONE`] is byte-identical to the selection entry point.
#[allow(clippy::too_many_arguments)]
pub fn build_cell_vertices_with_ligatures_selection_and_row_fade_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    ligature_runs: &[LigatureRun],
    focus_dim: f32,
    origin: [f32; 2],
    treatment: BackgroundTreatmentParams,
    cell_bg_opacity: f32,
    // COLORED-BG-FLOOR: surface alpha for resolved-non-default backgrounds;
    // `== cell_bg_opacity` is the exact inert path.
    colored_bg_opacity: f32,
    // TEXT-BRIGHTNESS: glyph-foreground lift (`1.0` = exact identity).
    text_brightness: f32,
    opaque_region: Option<CellRegion>,
    chrome_pin: ChromePin,
    selection_opacity: f32,
    row_fade: RowFade,
    // BIDI: display-order placement while `bidi_reorder` is on; `None` is the
    // unchanged logical-column path.
    bidi: Option<&BidiDisplayMap>,
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
        colored_bg_opacity,
        text_brightness,
        opaque_region,
        chrome_pin,
        selection_opacity,
        row_fade,
        bidi,
    );
}

/// Private core of the cell-vertex build. Carries every render parameter,
/// including `selection_opacity` (see [`crate::core::Attrs::selected`]), which
/// drives BOTH a selected cell's COLOR-space tint strength AND its background
/// surface alpha. The surface alpha is lerped from the surrounding content
/// opacity up to fully opaque as the knob rises -
/// `A_sel = cell_bg_opacity + selection_opacity * (1.0 - cell_bg_opacity)` -
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
    // COLORED-BG-FLOOR: the surface alpha for a cell whose resolved background
    // (post-inverse, pre-dim/treatment) differs from the theme's default
    // background. The GPU layer computes it as
    // `cell_bg_opacity_setting * max(window_bg_alpha, colored_bg_opacity)`, so
    // it can only be >= `cell_bg_opacity` (the floor strengthens, never
    // weakens) and equals it exactly when the knob is 0.0 or the window is
    // opaque - both byte-identical inert paths, as is passing equal values.
    colored_bg_opacity: f32,
    // TEXT-BRIGHTNESS: soft-knee lift of every glyph foreground toward white
    // (`text::lift_brightness_rgba`), applied in `resolve` AFTER the RV1
    // min-contrast floor so a floor-corrected color is the lift's input and
    // the lift does not re-run the floor (on light backgrounds it can reduce
    // the floored contrast; see `text/color.rs`). Uniform across all mono ink this builder
    // emits - glyphs, combining marks, ligature runs, and the underline/
    // strikethrough decorations that reuse `fg`. Color emoji ride the separate
    // color-glyph pipeline with their intrinsic palette (exempt by design).
    // Operates on COLOR channels only, so it composes with the VE4 fade's
    // per-row ALPHA ramp independently and deterministically in either order.
    // `1.0` is an exact identity (early return), keeping the plain path
    // byte-identical.
    text_brightness: f32,
    opaque_region: Option<CellRegion>,
    chrome_pin: ChromePin,
    selection_opacity: f32,
    row_fade: RowFade,
    // BIDI: display-order placement while `bidi_reorder` is on. `None`, and
    // every row the map leaves in identity layout, take the unchanged
    // logical-column path.
    bidi: Option<&BidiDisplayMap>,
) {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let cell_w = atlas.cell.width as f32;
    let cell_h = atlas.cell.height as f32;
    let baseline = atlas.cell.baseline as f32;
    // SCROLL-CHROME-BOUNCE: the pinned-chrome seam for this build (inert unless a
    // glide is in flight - then the top bar/rail stay put while content glides).
    let chrome_seam_y = chrome_pin.seam_y(origin[1], cell_h);

    out.clear();
    let Some(needed) = vertex_reserve(snapshot) else {
        return;
    };
    // `out` is empty here, so this guarantees `needed` capacity.
    out.reserve(needed);

    // Effective foreground/background after inverse + dim, and the column span of
    // a wide lead cell. Computed identically in both passes. `row`/`col` are the
    // cell's grid position, needed by the ID3/U5 background treatment (gradient /
    // vignette) which modulates the background by position.
    // COLORED-BG-FLOOR: the reference the "non-default background" test compares
    // against - the theme's default background AFTER theme resolution (the same
    // `DynamicColors` the cells resolve through), so an SGR background that
    // happens to resolve to the exact default colour (e.g. an app repainting
    // the screen in the theme's own background) still composites as default and
    // keeps the glassy treatment.
    let default_bg = rgb_linear(snapshot.colors.background);
    let resolve =
        |cell: &crate::core::Cell, row: usize, col: usize| -> ([f32; 4], [f32; 4], bool) {
            #[cfg(test)]
            CELL_COLOR_RESOLVE_CALLS.with(|calls| calls.set(calls.get() + 1));
            let mut fg = foreground_linear(&snapshot.colors, cell.attrs.foreground);
            let mut bg = background_linear(&snapshot.colors, cell.attrs.background);
            if cell.attrs.inverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            // COLORED-BG-FLOOR: classified at the resolution seam - post-inverse
            // (an inverse cell's visible backdrop is its resolved foreground),
            // pre-dim/pre-treatment (those modulate default cells too; the
            // classification must not flip when the window loses focus or a
            // gradient is active). Exact float compare is sound: both sides come
            // through the same `rgb_linear` conversion, so an equal source
            // colour yields bit-equal channels.
            let colored_bg = bg != default_bg;
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
            // so compositing the fill over the backdrop at `selection_opacity` - and
            // the text symmetrically back toward the unselected foreground - recedes
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
            } else if selection_opacity > 1.0 && cell.attrs.selected() && cell.attrs.inverse() {
                // SELECTION-STRENGTH boost: the surface stays fully opaque (Pass 1
                // saturates its alpha at 1.0); the selection COLOR is pushed
                // further along the backdrop->fill vector so the highlight reads
                // stronger. The text keeps its authored backdrop color -- the RV1
                // floor below corrects it toward the configured ratio over the
                // strengthened fill (best effort). Parity with the themed
                // path, which boosts the same vector in `effective_selection_fill`.
                let fill = [bg[0], bg[1], bg[2]];
                let backdrop = [fg[0], fg[1], fg[2]];
                let boosted = crate::color::boost_over(fill, backdrop, selection_opacity);
                bg = [boosted[0], boosted[1], boosted[2], bg[3]];
            }
            if cell.attrs.dim() {
                fg = dim_color(fg);
            }
            // ID2 focus dimming: while the window is unfocused, recede the whole
            // cell - both foreground and background - perceptually in OKLab so hue
            // is preserved and relative contrast stays roughly stable. Applied after
            // the SGR-dim attribute and before the RV1 floor, so the floor sees the
            // dimmed background and corrects the text toward the configured ratio
            // (best effort). `focus_dim == 0.0` (focused, or the knob
            // off) is skipped entirely, keeping focused frames byte-identical.
            if focus_dim > 0.0 {
                fg = text::dim_linear_rgba(fg, focus_dim);
                bg = text::dim_linear_rgba(bg, focus_dim);
            }
            // Background treatment (gradient / vignette): modulate the cell
            // background by its grid position. Applied AFTER focus dimming and
            // BEFORE the RV1 floor, so the floor sees the treated per-cell
            // background and corrects the foreground toward the configured ratio
            // (best effort). `treatment.active() == false`
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
            // TEXT-BRIGHTNESS: lift the final ink color toward white, after the
            // RV1 floor (see the parameter note). `1.0` is an exact identity.
            if text_brightness > 1.0 {
                fg = text::lift_brightness_rgba(fg, text_brightness);
            }
            (fg, bg, colored_bg)
        };
    let span_of = |row: usize, col: usize| -> f32 {
        if col + 1 < cols && snapshot.cells[row * cols + col + 1].wide_continuation {
            2.0
        } else {
            1.0
        }
    };
    // BIDI: the visual column of a cell and, on a reordered row, the pixel
    // span of its owner that clips every glyph drawn for it.
    let reordered = |row: usize| bidi.filter(|map| map.row_is_reordered(row));
    let visual_col =
        |row: usize, col: usize| reordered(row).map_or(col, |map| map.visual_column(row, col));
    let owner_clip =
        |row: usize, x0: f32, span: f32| reordered(row).map(|_| [x0, x0 + cell_w * span]);

    // One resolution per lead cell for this rebuild. Pass 1 and pass 2 read the
    // same tuple, so a cell's colors cannot diverge between the background and
    // the glyph. Wide continuation spacers are unused placeholders and are
    // skipped by both passes. The cursor painter does not read this table: its
    // under-glyph is floored against the cursor color, not this foreground, and
    // sharing the table would change those pixels.
    let mut resolved = Vec::with_capacity(rows.saturating_mul(cols));
    for row in 0..rows {
        for col in 0..cols {
            let cell = &snapshot.cells[row * cols + col];
            if cell.wide_continuation {
                resolved.push(([0.0_f32; 4], [0.0_f32; 4], false));
            } else {
                resolved.push(resolve(cell, row, col));
            }
        }
    }

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
            let (_, bg, colored_bg) = resolved[row * cols + col];
            // ID3/U5 image background: scale ONLY the background-quad alpha by
            // `cell_bg_opacity` so a background image shows through behind text.
            // `1.0` (the default) yields `bg[3] * 1.0 == bg[3]` - byte-identical.
            // The floor reference inside `resolve` keeps the OPAQUE bg (alpha
            // untouched there), so `enforce_contrast_rgba` still floors against
            // the theme background `l_bg`; the readability scrim guarantees the
            // composited luminance stays on the safe side of `l_bg`.
            //
            // Background alpha precedence:
            // 1. TRANSPARENCY (MENU-OPACITY): a cell inside `opaque_region` is
            //    fully opaque.
            // 2. SELECTION-OPACITY: a selected cell (inverse or themed path)
            //    lerps from the content opacity toward opaque,
            //      A_sel = cell_bg_opacity + selection_opacity * (1 - cell_bg_opacity),
            //    so it is never weaker than its surround and is 1.0 at full
            //    strength; its color tint recedes with it (`resolve`,
            //    `themed_selection_style`).
            // 3. COLORED-BG-FLOOR: a non-default resolved background takes
            //    `colored_bg_opacity` so app-painted blocks stay strong as
            //    window opacity drops; chrome cells are exempt (their opacity
            //    is owned by `tab_panel_strength`).
            // Otherwise `cell_bg_opacity`. Equal alphas make every branch
            // identical, so the knob-off and opaque-window paths are unchanged.
            let cell_opacity = if opaque_region.is_some_and(|r| r.contains(row, col)) {
                1.0
            } else if cell.attrs.selected() {
                // SELECTION-OPACITY surface alpha, saturated at 1.0: at strength
                // <= 1.0 the lerp is already <= 1.0 (byte-identical), and at
                // strength > 1.0 it clamps here so the boost never drives a
                // background alpha above 1.0 into GPU blending -- the extra
                // strength lands on COLOR (see the inverse boost above and the
                // themed `effective_selection_fill`), not surface alpha.
                (cell_bg_opacity + selection_opacity * (1.0 - cell_bg_opacity)).min(1.0)
            } else if colored_bg && !chrome_pin.is_chrome(row, col) {
                colored_bg_opacity
            } else {
                cell_bg_opacity
            };
            let bg = [bg[0], bg[1], bg[2], bg[3] * cell_opacity];
            let span = span_of(row, col);
            let x0 = origin[0] + visual_col(row, col) as f32 * cell_w + chrome_pin.cell_dx(col);
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
            // Pass 2 draws ink only; the colored-background classification is a
            // pass-1 (surface alpha) concern.
            let (fg, bg, _) = resolved[row * cols + col];
            // VE4 new-output fade: freshly arrived rows ramp their FOREGROUND
            // ink in (glyphs, marks, ligatures, decorations below) while the
            // pass-1 background stays untouched. Applied after the RV1 floor:
            // the floored color is the ramp's destination, and the ramp starts
            // at the caller's floor multiplier (never 0), so the transient is a
            // bounded, user-opted dip below steady-state contrast that
            // resolves to the exact floored color. `1.0` (inert / settled /
            // chrome / cursor row) skips the blend and is byte-identical.
            let fade_mul = row_fade.multiplier(row, col);
            let fg = if fade_mul < 1.0 {
                [fg[0], fg[1], fg[2], fg[3] * fade_mul]
            } else {
                fg
            };
            let span = span_of(row, col);
            let x0 = origin[0] + visual_col(row, col) as f32 * cell_w + chrome_pin.cell_dx(col);
            let y0 = chrome_pin.cell_top_y(origin[1], cell_h, row, col);
            let decoration_y0 = y0 + chrome_pin.glyph_center_dy(row, col, cell_h);
            // SCROLL-CHROME-BOUNCE: content glyphs gliding up under the pinned
            // bar are cropped at the seam; chrome glyphs and the inert path are not.
            let seam =
                (chrome_pin.active() && chrome_pin.top_rows > 0 && !chrome_pin.is_chrome(row, col))
                    .then_some(chrome_seam_y);
            let clip = owner_clip(row, x0, span);

            while ligature_runs
                .get(ligature_index)
                .is_some_and(|run| run.row < row || (run.row == row && run.end <= col))
            {
                ligature_index += 1;
            }
            // BIDI: on a reordered row a run draws only when its cells share
            // one level (adjacent on screen, one direction); otherwise its
            // cells fall back to scalar glyphs.
            let ligature_box = |run: &LigatureRun| match reordered(row) {
                Some(map) => map.uniform_visual_span(row, run.start..run.end),
                None => Some(run.start..run.end),
            };
            let ligature = ligature_runs
                .get(ligature_index)
                .filter(|run| run.covers(row, col))
                .filter(|run| {
                    run.glyphs
                        .iter()
                        .all(|glyph| atlas.contains_shaped(glyph.key))
                })
                .filter(|run| ligature_box(run).is_some());
            let style = font_style_for_attrs(&cell.attrs);
            // BIDI: a mirrored cell presents its Bidi_Mirroring_Glyph, falling
            // back to its own glyph when that character is not in the atlas.
            let mirrored = bidi.and_then(|map| map.mirrored_char(row, col));
            if !cell.attrs.hidden()
                && cell_draws_base_glyph(cell.ch)
                && !color_coverage.covers(row, col)
                && ligature.is_none()
                && let Some(bounds) = mirrored
                    .and_then(|ch| atlas.glyph_quad_styled(style, ch))
                    .or_else(|| atlas.glyph_quad_styled(style, cell.ch))
            {
                // TAB-LABEL-CENTERING: a chrome band label rides a sub-cell Y
                // shift (`decoration_y0`); `0.0` for content cells keeps the
                // plain path byte-identical.
                push_cell_glyph(out, [x0, y0, decoration_y0], bounds, fg, seam, clip);
            }

            // Retained extension scalars stored on the cell draw over the base
            // glyph at the same cell origin: each mark rasterized with a
            // one-cell pen anchor so its (left-hanging) ink lands on the base
            // cell (`combining_mark_quad`, which never yields the tofu box).
            // Marks draw even over a space base (a mark can attach to one) and
            // follow the base glyph's seam-clipping rule. Wide bases and
            // stacked multi-mark clusters render at the same single-cell
            // anchor, a bounded approximation rather than full mark positioning.
            if !cell.attrs.hidden()
                && cell_draws_combining_marks(cell.ch)
                && !color_coverage.covers(row, col)
                && ligature.is_none()
            {
                for &mark in cell.combining() {
                    if let Some(bounds) = atlas.combining_mark_quad(style, mark) {
                        push_cell_glyph(out, [x0, y0, decoration_y0], bounds, fg, seam, clip);
                    }
                }
            }

            // Emit a contextual span once from its first source column. Its
            // atlas keys carry only source span/anchor data; shaped advances
            // never move grid columns. Horizontal clipping prevents ink from
            // reaching the following logical cell or an adjacent pane.
            if let Some(run) = ligature.filter(|run| run.start == col) {
                // CHROME-GAP: the run shares its lead cell's horizontal shift
                // (a shaping run never crosses the rail↔content seam - the band
                // and content carry distinct attrs - so one dx spans the run).
                let run_dx = chrome_pin.cell_dx(run.start);
                let visual = ligature_box(run).unwrap_or(run.start..run.end);
                let span_x0 = origin[0] + visual.start as f32 * cell_w + run_dx;
                let span_x1 = origin[0] + visual.end as f32 * cell_w + run_dx;
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
                        // BIDI: the atlas drew the glyph with its pen at
                        // `anchor_cell`; shift the slot so that pen lands on
                        // the leftmost visual cell of the glyph's source
                        // cluster. Identity placement keeps `span_x0`.
                        let glyph_x0 = match reordered(row) {
                            Some(map) => {
                                let anchor = usize::from(glyph.key.anchor_cell);
                                let source = run.start + anchor;
                                let end =
                                    (source + usize::from(glyph.source_cells.max(1))).min(run.end);
                                let pen = (source..end.max(source + 1))
                                    .map(|column| map.visual_column(row, column))
                                    .min()
                                    .unwrap_or(visual.start);
                                origin[0] + (pen as f32 - anchor as f32) * cell_w + run_dx
                            }
                            None => span_x0,
                        };
                        push_glyph_quad_clipped_rect(
                            out,
                            glyph_x0,
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
                    let mut floored =
                        text::enforce_contrast_rgba(foreground_linear(&snapshot.colors, color), bg);
                    // TEXT-BRIGHTNESS: mono ink is lifted uniformly, so an
                    // explicit underline color rides the same lift as the cell
                    // `fg` it replaces (after the floor, as in `resolve`).
                    if text_brightness > 1.0 {
                        floored = text::lift_brightness_rgba(floored, text_brightness);
                    }
                    // VE4 new-output fade: an explicit SGR underline color rides
                    // the same foreground ramp as the cell `fg` default above.
                    if fade_mul < 1.0 {
                        [floored[0], floored[1], floored[2], floored[3] * fade_mul]
                    } else {
                        floored
                    }
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
                    seam,
                );
            }

            if cell.attrs.strikethrough() {
                // Same origin and seam crop as the underline and the glyph, so
                // a label recentred in its band is struck at its own midline.
                push_decoration_quad(
                    out,
                    strikethrough_rect(x0, decoration_y0, cell_w * span, cell_h, baseline),
                    fg,
                    seam,
                );
            }
        }
    }
}

/// BIDI test seam: the cell build in display order under `bidi`, at the
/// identity origin with every other effect at its inert value.
#[cfg(test)]
pub(crate) fn build_cell_vertices_with_bidi_into(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    color_runs: &[ColorGlyphRun],
    ligature_runs: &[LigatureRun],
    bidi: &BidiDisplayMap,
) {
    build_cells_core(
        out,
        snapshot,
        atlas,
        color_runs,
        ligature_runs,
        0.0,
        [0.0, 0.0],
        BackgroundTreatmentParams::default(),
        1.0,
        1.0,
        1.0,
        None,
        ChromePin::NONE,
        1.0,
        RowFade::NONE,
        Some(bidi),
    );
}

/// Visual-only parameters applied to cursor geometry. The [`Default`] is the
/// focused, fully opaque, zero-offset identity.
///
/// - `offset` - sub-cell pixel shift added to the cursor's cell origin
///   (VE4-slide). Default `[0.0, 0.0]` ⇒ unchanged `x0`/`y0`.
/// - `alpha` - multiplier on the cursor quad's color alpha.
///   Default `1.0` ⇒ unchanged opacity. Polarity: `1.0` = fully opaque (today),
///   `0.0` = invisible - never default to `0.0`.
/// - `focused` - whether the window owns keyboard focus. An unfocused Block
///   cursor becomes a hollow outline; underline and bar styles are unchanged.
///
/// The type lives here (not in the native overlay registry) because
/// [`push_cursor`] - a `crate::grid` function - must name it to apply the
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
        None,
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
    append_cursor_vertices_with_origin_and_bidi(
        out,
        snapshot,
        atlas,
        cursor_style,
        origin,
        params,
        None,
    );
}

/// [`append_cursor_vertices_with_origin`] under an optional bidi display map:
/// the cursor stays on its logical cell and draws at that cell's visual
/// column, with a mirrored cell's presentation glyph under a block cursor.
/// A map is passed only while `bidi_reorder` is on; `None` is the unchanged
/// path.
pub fn append_cursor_vertices_with_origin_and_bidi(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    origin: [f32; 2],
    params: CursorRenderParams,
    bidi: Option<&BidiDisplayMap>,
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
        bidi,
    );
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
    out.reserve(overlays.len() * INSTANCES_PER_QUAD);
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
/// - **Block, focused**: a background quad in the theme cursor color with the
///   cell's glyph (if any) redrawn on top in the cell's background color,
///   floored for contrast against the cursor color, keeping the character
///   readable under the cursor.
/// - **Block, unfocused**: four one-pixel cursor-color border quads. The cell's
///   existing glyph remains untouched in its normal foreground color.
/// - **Underline**: a thin cursor-color bar at the cell's bottom edge, drawn
///   over the cell's existing glyph.
/// - **Bar**: a thin cursor-color vertical bar at the cell's left edge, drawn
///   over the cell's existing glyph.
///
/// A hidden cursor (`cursor_visible == false`, which the renderer also uses for
/// the blink "off" phase) emits nothing. The position is clamped to the grid so
/// a stale snapshot can never index out of bounds. Reflects only the live
/// snapshot cursor - no scrollback/viewport offset is applied here.
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
    bidi: Option<&BidiDisplayMap>,
) {
    if !snapshot.cursor_visible || params.follower_active {
        return;
    }

    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if cols == 0 || rows == 0 || !snapshot_shape_is_valid(snapshot) {
        return;
    }

    // Defensive clamp: a stale snapshot could carry a cursor past the grid.
    let row = snapshot.cursor.row.min(rows - 1);
    let col = snapshot.cursor.column.min(cols - 1);

    let cell = &snapshot.cells[row * cols + col];

    // Effective colors after the cell's own inverse attribute. The block
    // cursor redraws the glyph in the effective background over a cursor-color
    // block; the bar and underline shapes draw in the cursor color.
    let mut fg = foreground_linear(&snapshot.colors, cell.attrs.foreground);
    let mut bg = background_linear(&snapshot.colors, cell.attrs.background);
    if cell.attrs.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }

    // BIDI: the cursor addresses its logical cell and draws at that cell's
    // visual column. `None` keeps the logical column.
    let visual_col = bidi.map_or(col, |map| map.visual_column(row, col));
    // A mirrored cell presents its mirror glyph, as in the content pass; a
    // mirror missing from the atlas falls back to the stored character.
    let glyph_ch = bidi
        .and_then(|map| map.mirrored_char(row, col))
        .filter(|&mirror| {
            atlas
                .glyph_quad_styled(font_style_for_attrs(&cell.attrs), mirror)
                .is_some()
        })
        .unwrap_or(cell.ch);

    // VE4-slide: additive sub-cell shift. Default `[0.0, 0.0]` ⇒ identity.
    let x0 = origin[0] + visual_col as f32 * cell_w + params.offset[0];
    let y0 = origin[1] + row as f32 * cell_h + params.offset[1];
    // BIDI: on a reordered row the under-cursor glyph is cropped to its
    // owner's visual span, as every glyph of the content pass is, so overflow
    // ink cannot cover a neighbouring visual owner. Elsewhere it is uncropped.
    let owner_span = if col + 1 < cols && snapshot.cells[row * cols + col + 1].wide_continuation {
        2.0
    } else {
        1.0
    };
    let owner_clip = bidi
        .filter(|map| map.row_is_reordered(row))
        .map(|_| [x0, x0 + cell_w * owner_span]);

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
            // itself is NEVER alpha-faded - only the block is.
            let glyph_color = text::enforce_contrast_rgba(bg, block_color);
            push_quad(
                out,
                [x0, y0, x0 + cell_w, y0 + cell_h],
                [0.0, 0.0, 0.0, 0.0],
                block_color,
                0.0,
            );
            if !cell.attrs.hidden()
                && cell_draws_base_glyph(cell.ch)
                && let Some(bounds) =
                    atlas.glyph_quad_styled(font_style_for_attrs(&cell.attrs), glyph_ch)
            {
                push_cell_glyph(out, [x0, y0, y0], bounds, glyph_color, None, owner_clip);
            }
            // The under-cursor glyph keeps its combining marks too, drawn in
            // the same contrast-derived color as the base (see the content
            // pass for the anchoring rule). Placeholder cells suppress marks
            // here as well - same sibling rule as the content pass.
            if !cell.attrs.hidden() && cell_draws_combining_marks(cell.ch) {
                for &mark in cell.combining() {
                    if let Some(bounds) =
                        atlas.combining_mark_quad(font_style_for_attrs(&cell.attrs), mark)
                    {
                        push_cell_glyph(out, [x0, y0, y0], bounds, glyph_color, None, owner_clip);
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
mod bidi_tests;
#[cfg(test)]
mod clipping_tests;
#[cfg(test)]
mod tests;

// SPDX-License-Identifier: GPL-3.0-only
//! Glyph quad emission: the color-glyph vertex segment and the coverage-glyph
//! quad helpers (uncropped, seam-cropped, and rectangle-cropped) that the cell
//! builder uses.

use super::model::push_quad;
use super::{
    BidiDisplayMap, ChromePin, ColorGlyphRun, ColorGlyphVertex, INSTANCES_PER_QUAD, RowFade, Vertex,
};
use crate::atlas::GlyphBounds;
use crate::core::Snapshot;
use crate::emoji::ColorGlyphAtlas;

pub(super) fn push_color_glyph_quad(
    out: &mut Vec<ColorGlyphVertex>,
    rect: [f32; 4],
    uv: [f32; 4],
    alpha: f32,
) {
    let [x0, y0, x1, y1] = rect;
    let [u0, v0, u1, v1] = uv;
    out.push(ColorGlyphVertex::new(
        [x0, y0],
        [x1, y1],
        [u0, v0],
        [u1, v1],
        alpha,
    ));
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
        RowFade::NONE,
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
    // VE4 new-output fade: a color glyph on a fading row rides the same
    // foreground alpha ramp as mono ink (`RowFade::NONE` = every alpha 1.0).
    row_fade: RowFade,
) {
    color_glyph_vertices_core(
        out, snapshot, atlas, runs, origin, chrome_pin, row_fade, None,
    );
}

/// Shared color-glyph build. `bidi` (set while `bidi_reorder` is on) moves each run to
/// the visual columns of the cells it covers; `None` keeps logical columns.
#[allow(clippy::too_many_arguments)]
pub(super) fn color_glyph_vertices_core(
    out: &mut Vec<ColorGlyphVertex>,
    snapshot: &Snapshot,
    atlas: &ColorGlyphAtlas,
    runs: &[ColorGlyphRun],
    origin: [f32; 2],
    chrome_pin: ChromePin,
    row_fade: RowFade,
    bidi: Option<&BidiDisplayMap>,
) {
    out.clear();
    out.reserve(runs.len() * INSTANCES_PER_QUAD);

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
        // BIDI: a cluster whose covered cells share one level draws from the
        // leftmost of their visual columns; otherwise from its lead's.
        let column = bidi
            .filter(|map| map.row_is_reordered(run.row))
            .map_or(run.column, |map| {
                let covered =
                    run.column..(run.column + usize::from(run.covered_columns.max(1))).min(cols);
                map.uniform_visual_span(run.row, covered)
                    .map_or_else(|| map.visual_column(run.row, run.column), |span| span.start)
            });
        let x0 = origin[0] + column as f32 * cell_w + chrome_pin.cell_dx(run.column);
        let y0 = origin[1] + run.row as f32 * cell_h + chrome_pin.cell_dy(run.row, run.column);
        let x1 = x0 + bounds.pixel_width as f32;
        let fade_alpha = row_fade.multiplier(run.row, run.column);
        if chrome_pin.active() && chrome_pin.top_rows > 0 {
            push_color_glyph_quad_clipped_top(
                out,
                x0,
                y0,
                x1,
                bounds.pixel_height as f32,
                bounds.uv,
                chrome_seam_y,
                fade_alpha,
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
                fade_alpha,
            );
        }
    }
}

/// Push a glyph quad sized and positioned from bearing-aware atlas bounds.
///
/// The cell's on-screen origin is `(x0, y0)`; the quad is offset and sized by the
/// glyph's inked extent (1 atlas pixel == 1 physical screen pixel), so ink that
/// overflows the cell box is drawn uncropped while backgrounds stay full-cell.
pub(super) fn push_glyph_quad(
    out: &mut Vec<Vertex>,
    x0: f32,
    y0: f32,
    bounds: GlyphBounds,
    color: [f32; 4],
) {
    let gx0 = x0 + bounds.offset_x as f32;
    let gy0 = y0 + bounds.offset_y as f32;
    let gx1 = gx0 + bounds.width as f32;
    let gy1 = gy0 + bounds.height as f32;
    push_quad(out, [gx0, gy0, gx1, gy1], bounds.uv, color, 1.0);
}

/// Push a cell's scalar glyph or combining mark. `y` is the cell top and the
/// label-centered glyph top. `seam` crops a content glyph at the pinned-chrome
/// seam (drawn from the cell top); `owner_clip` (bidi reordered rows only)
/// crops ink horizontally to the owner's visual span. Without either, the
/// glyph draws uncropped from the centered top, as it always has.
pub(super) fn push_cell_glyph(
    out: &mut Vec<Vertex>,
    [x0, y0, centered_y0]: [f32; 3],
    bounds: GlyphBounds,
    color: [f32; 4],
    seam: Option<f32>,
    owner_clip: Option<[f32; 2]>,
) {
    match (owner_clip, seam) {
        (None, None) => push_glyph_quad(out, x0, centered_y0, bounds, color),
        (None, Some(seam)) => push_glyph_quad_clipped_top(out, x0, y0, bounds, color, seam),
        (Some([left, right]), seam) => {
            let (y, top) = seam.map_or((centered_y0, f32::NEG_INFINITY), |seam| (y0, seam));
            push_glyph_quad_clipped_rect(
                out,
                x0,
                y,
                bounds,
                color,
                [left, top, right, f32::INFINITY],
            );
        }
    }
}

/// Crop a coverage glyph to a pixel rectangle by adjusting UVs, never by
/// squashing geometry. Used by multi-cell contextual glyphs so their ink stays
/// inside the logical source span and pane/grid bounds.
pub(super) fn push_glyph_quad_clipped_rect(
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
pub(super) fn push_glyph_quad_clipped_top(
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

/// SCROLL-CHROME-BOUNCE: color-glyph analogue of [`push_glyph_quad_clipped_top`]:
/// crops the emoji quad's top at the seam via UV so a gliding color glyph never
/// paints into the pinned tab bar.
#[allow(clippy::too_many_arguments)]
fn push_color_glyph_quad_clipped_top(
    out: &mut Vec<ColorGlyphVertex>,
    x0: f32,
    y0: f32,
    x1: f32,
    pixel_height: f32,
    uv: [f32; 4],
    clip_top_y: f32,
    alpha: f32,
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
    push_color_glyph_quad(out, [x0, gy0, x1, gy1], [u0, v0, u1, v1], alpha);
}

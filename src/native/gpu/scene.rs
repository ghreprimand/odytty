// SPDX-License-Identifier: GPL-3.0-only
//! Snapshot, pane, cell, image, cursor, and overlay vertex construction.
//!
//! Everything here turns terminal state plus the frame's input contracts into
//! CPU-side vertex data and uploads it. Segment counts and their order in the
//! shared vertex buffer are the contract `frame` draws against.

use std::collections::BTreeMap;

use crate::atlas;
use crate::core::{CursorStyle, RgbColor, Snapshot};
use crate::graphics::{StoredImageId, VisiblePlacement};
use crate::grid::{self, ColorGlyphRun, ColorGlyphVertex, CursorRenderParams, SolidQuad, Vertex};
use crate::ligature::LigatureRun;
use crate::text::{self, GlyphAtlas};

use crate::native::image_layer::{ImageUpload, PaneImageInput, PaneImageUpload};

use super::fonts::StyleFonts;
use super::pipeline_policy::{
    colored_content_build_opacity, content_build_opacity, scene_clear_color,
};
use super::resources::GpuState;
use super::resources::{
    ViewportUniform, create_color_glyph_vertex_buffer, create_vertex_buffer,
    grow_vertex_buffer_capacity,
};
use super::types::{
    CursorGlowInstance, CursorGlowRequest, CursorStreakInstance, CursorStreakRequest, OverlayTop,
    PaneRender, PanelFrameQuads, RailOverlay,
};
use super::types::{
    accumulate_pane_color_glyphs, append_cursor_glow_vertices, append_cursor_streak_vertices,
    build_cursor_glow_instance, build_cursor_streak_instance, pane_chrome_pin, quads_excluding,
    rail_overlay_chrome_pin, retained_cursor_effects, row_fade_view,
};

/// Apply the synthetic-styles kill switch to a font set's natural synthesis
/// mask. When synthesis is `enabled`, returns [`StyleFonts::synthetic_mask`]
/// unchanged (each style true only when it has no real face). When disabled,
/// every bit is forced off so [`GlyphAtlas::set_synthetic_styles`] performs no
/// emboldening or shear and styled cells fall back to plain regular glyphs.
pub(super) fn masked_synthetic(fonts: &StyleFonts, enabled: bool) -> (bool, bool, bool) {
    if enabled {
        fonts.synthetic_mask()
    } else {
        (false, false, false)
    }
}

pub(in crate::native) fn ensure_snapshot_glyphs(
    atlas: &mut GlyphAtlas,
    fonts: &StyleFonts,
    snapshot: &Snapshot,
) {
    ensure_snapshot_glyphs_excluding_color_runs(atlas, fonts, snapshot, &[]);
}

pub(in crate::native) fn ensure_snapshot_glyphs_excluding_color_runs(
    atlas: &mut GlyphAtlas,
    fonts: &StyleFonts,
    snapshot: &Snapshot,
    color_runs: &[ColorGlyphRun],
) {
    let cols = snapshot.dimensions.columns;
    // O(cells / 64 + runs) coverage mask instead of a per-cell scan of the
    // run list; the skip decisions are identical.
    let coverage = grid::ColorRunCoverage::new(color_runs, cols, snapshot.dimensions.rows);
    for (idx, cell) in snapshot.cells.iter().enumerate() {
        let row = idx / cols;
        let column = idx % cols;
        if cell.wide_continuation || cell.attrs.hidden() {
            continue;
        }
        if coverage.covers(row, column) {
            continue;
        }
        let style = grid::font_style_for_attrs(&cell.attrs);
        let _ = atlas.ensure_styled(fonts.font_for(style), style, cell.ch);
        // Zero-width combining marks stored on the cell rasterize as their own
        // dynamic glyphs (anchored so their ink lands over the base cell); a
        // mark the font lacks caches the fallback decision and simply does not
        // draw (`combining_mark_quad` filters the fallback slot).
        for &mark in cell.combining() {
            let _ = atlas.ensure_styled(fonts.font_for(style), style, mark);
        }
    }
}

pub(super) fn vertex_bytes_len(vertices: &[Vertex]) -> u64 {
    std::mem::size_of_val(vertices) as u64
}

pub(super) fn background_vertex_count(snapshot: &Snapshot) -> u32 {
    let cells = snapshot
        .cells
        .iter()
        .filter(|cell| !cell.wide_continuation)
        .count();
    (cells * grid::INSTANCES_PER_QUAD) as u32
}

/// Append the overlays and live cursor parameters shared by Full and
/// CursorOnly rebuilds. Keeping this layer in one builder prevents the two GPU
/// paths from diverging when cursor animation parameters change.
pub(in crate::native) fn append_cursor_layer_vertices(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    origin: [f32; 2],
    overlays: &[SolidQuad],
    params: CursorRenderParams,
) {
    out.reserve(overlays.len() * grid::INSTANCES_PER_QUAD);
    for &overlay in overlays {
        grid::push_solid_quad(out, overlay);
    }
    grid::append_cursor_vertices_with_origin(out, snapshot, atlas, cursor_style, origin, params);
}

/// PANE-SUBCELL-CLIP: the number of background quads the snapshot's FIRST row
/// contributes — the non-continuation cells in row 0. Background quads are
/// emitted in row-major order, so these lead the background segment, and
/// [`grid::extend_first_row_bg_to_top`] uses the count to flush exactly the top
/// row's backgrounds into the sub-cell gap a downward glide opens. Wide
/// continuation cells emit no quad (they are merged into their lead), matching
/// [`background_vertex_count`]'s own filter.
fn pane_row0_bg_quads(snapshot: &Snapshot) -> usize {
    let cols = snapshot.dimensions.columns;
    snapshot
        .cells
        .iter()
        .take(cols)
        .filter(|cell| !cell.wide_continuation)
        .count()
}

fn linear_rgba(color: RgbColor, alpha: f32) -> [f32; 4] {
    [
        text::srgb_to_linear(color.red),
        text::srgb_to_linear(color.green),
        text::srgb_to_linear(color.blue),
        alpha.clamp(0.0, 1.0),
    ]
}

/// CHROME-GAP: test-only legacy entry — the production wash routes through
/// [`wallpaper_edge_wash_quads_with_pin`]; a `ChromePin::NONE` pin reproduces
/// this four-strip wash byte-for-byte (pinned by the gpu tests).
#[cfg(test)]
pub(in crate::native) fn wallpaper_edge_wash_quads(
    snapshot: &Snapshot,
    cell: atlas::CellSize,
    origin: [f32; 2],
    surface_size: [u32; 2],
    opacity: f32,
) -> Vec<SolidQuad> {
    wallpaper_edge_wash_quads_with_pin(
        snapshot,
        cell,
        origin,
        surface_size,
        opacity,
        grid::ChromePin::NONE,
    )
}

/// CHROME-GAP-aware edge wash. With a zero-gap pin this is exactly the legacy
/// four-strip wash (byte-identical). With a chrome-facing gap in play the
/// decorated frame's true pixel extent grows by the gap on each affected axis
/// (so the outer strips cannot overlap the shifted cells), and the interior gap
/// strips — the rail↔content column and the below-bar row that now read as
/// padding — are washed too, so a translucent window shows the themed wash
/// there instead of raw wallpaper.
pub(in crate::native) fn wallpaper_edge_wash_quads_with_pin(
    snapshot: &Snapshot,
    cell: atlas::CellSize,
    origin: [f32; 2],
    surface_size: [u32; 2],
    opacity: f32,
    pin: grid::ChromePin,
) -> Vec<SolidQuad> {
    let color = linear_rgba(snapshot.colors.background, opacity);
    let surface_w = surface_size[0] as f32;
    let surface_h = surface_size[1] as f32;
    let cell_w = cell.width as f32;
    let cell_h = cell.height as f32;
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    let has_rail = pin.rail_col_start != pin.rail_col_end;
    let gap_x = if has_rail { pin.gap_x } else { 0.0 };
    let gap_y = if pin.top_rows > 0 { pin.gap_y } else { 0.0 };
    let grid_x0 = origin[0].clamp(0.0, surface_w);
    let grid_y0 = origin[1].clamp(0.0, surface_h);
    let grid_x1 = (origin[0] + cols as f32 * cell_w + gap_x).clamp(0.0, surface_w);
    let grid_y1 = (origin[1] + rows as f32 * cell_h + gap_y).clamp(0.0, surface_h);

    let mut quads = Vec::with_capacity(6);
    let mut push = |rect: [f32; 4]| {
        if rect[2] > rect[0] && rect[3] > rect[1] {
            quads.push(SolidQuad { rect, color });
        }
    };

    push([0.0, 0.0, surface_w, grid_y0]);
    push([0.0, grid_y0, grid_x0, grid_y1]);
    push([grid_x1, grid_y0, surface_w, grid_y1]);
    push([0.0, grid_y1, surface_w, surface_h]);

    if gap_x > 0.0 {
        // The full-height rail↔content gap column: right of a LEFT rail band,
        // left of a RIGHT rail band (the seam column is where the shift begins).
        let seam_col = if pin.rail_col_start == 0 {
            pin.rail_col_end
        } else {
            pin.rail_col_start
        };
        let seam_x = origin[0] + seam_col as f32 * cell_w;
        push([
            seam_x.clamp(0.0, surface_w),
            grid_y0,
            (seam_x + gap_x).clamp(0.0, surface_w),
            grid_y1,
        ]);
    }
    if gap_y > 0.0 {
        // The below-bar gap row, spanning the CONTENT columns only: the
        // full-height rail band (unshifted in y) bounds it on its side.
        let content_x0 = if has_rail && pin.rail_col_start == 0 {
            origin[0] + pin.rail_col_end as f32 * cell_w + gap_x
        } else {
            grid_x0
        };
        let content_x1 = if has_rail && pin.rail_col_start > 0 {
            origin[0] + pin.rail_col_start as f32 * cell_w
        } else {
            grid_x1
        };
        let band_bottom = origin[1] + pin.top_rows as f32 * cell_h;
        push([
            content_x0.clamp(0.0, surface_w),
            band_bottom.clamp(0.0, surface_h),
            content_x1.clamp(0.0, surface_w),
            (band_bottom + gap_y).clamp(0.0, surface_h),
        ]);
    }
    quads
}

/// NF11: wash quads for a multi-pane composite. Every surface pixel NOT
/// covered by a pane's cell grid gets a wash quad in the snapshot background
/// color, exactly like the single-pane [`wallpaper_edge_wash_quads`]: with a
/// background image and translucent cell backgrounds, the window-padding band
/// and each pane's sub-cell remainder strips (pooled at the window margins by
/// `layout::pane_grid_origin`) would otherwise show raw wallpaper, visibly
/// insetting the washed region from the tile edge. Grids must be disjoint
/// (pane rects tile the content area); wash quads never overlap a grid, so
/// translucent cell backgrounds are never double-tinted. Divider gaps are
/// washed too — themed divider quads draw opaquely on top in a later segment.
///
/// Horizontal band sweep: the surface is split at every grid edge, and each
/// band emits quads for its x-gaps. For a single grid this degenerates to the
/// same four quads (top / left / right / bottom) as the single-pane function.
pub(in crate::native) fn multi_pane_wallpaper_edge_wash_quads(
    grid_rects: &[[f32; 4]],
    surface_size: [u32; 2],
    color: [f32; 4],
) -> Vec<SolidQuad> {
    let surface_w = surface_size[0] as f32;
    let surface_h = surface_size[1] as f32;
    let grids: Vec<[f32; 4]> = grid_rects
        .iter()
        .filter_map(|r| {
            let x0 = r[0].clamp(0.0, surface_w);
            let y0 = r[1].clamp(0.0, surface_h);
            let x1 = r[2].clamp(0.0, surface_w);
            let y1 = r[3].clamp(0.0, surface_h);
            (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
        })
        .collect();

    let mut ys: Vec<f32> = Vec::with_capacity(grids.len() * 2 + 2);
    ys.push(0.0);
    ys.push(surface_h);
    for grid in &grids {
        ys.push(grid[1]);
        ys.push(grid[3]);
    }
    ys.retain(|y| (0.0..=surface_h).contains(y));
    ys.sort_by(f32::total_cmp);
    ys.dedup();

    let mut quads = Vec::new();
    for band in ys.windows(2) {
        let (band_y0, band_y1) = (band[0], band[1]);
        if band_y1 <= band_y0 {
            continue;
        }
        // Bands are split at every grid edge, so a grid intersecting a band
        // spans it fully; only its x-interval matters within the band.
        let mut spans: Vec<(f32, f32)> = grids
            .iter()
            .filter(|grid| grid[1] < band_y1 && grid[3] > band_y0)
            .map(|grid| (grid[0], grid[2]))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut cursor_x = 0.0;
        for (gx0, gx1) in spans {
            if gx0 > cursor_x {
                quads.push(SolidQuad {
                    rect: [cursor_x, band_y0, gx0, band_y1],
                    color,
                });
            }
            cursor_x = cursor_x.max(gx1);
        }
        if cursor_x < surface_w {
            quads.push(SolidQuad {
                rect: [cursor_x, band_y0, surface_w, band_y1],
                color,
            });
        }
    }
    quads
}

impl GpuState {
    /// TRANSPARENCY: the opacity fed to the cell-vertex builder for terminal
    /// CONTENT. The wallpaper softening and window transparency compose: the
    /// surface alpha is `cell_bg_opacity * window_bg_alpha`, continuous across
    /// the 100% boundary and byte-identical (`== cell_bg_opacity`) when opaque.
    fn content_build_opacity(&self) -> f32 {
        content_build_opacity(self.window_bg_alpha, self.cell_bg_opacity)
    }

    /// COLORED-BG-FLOOR: the effective surface alpha for content cells with a
    /// resolved non-default background this frame. Equals
    /// [`Self::content_build_opacity`] exactly when the knob is `0.0` or the
    /// window is opaque (both inert identities); otherwise `>=` it.
    fn colored_content_build_opacity(&self) -> f32 {
        colored_content_build_opacity(
            self.window_bg_alpha,
            self.cell_bg_opacity,
            self.colored_bg_opacity,
        )
    }

    /// SELECTION-OPACITY: the independent selection strength fed to the cell
    /// builder for selected cells, unaffected by window transparency or
    /// `cell_bg_opacity`. Carries the full `0.0..=1.5` knob range; the builder
    /// saturates the surface ALPHA at `1.0` and shapes only COLOR above `1.0`,
    /// so no alpha above 1 ever reaches GPU blending.
    fn selection_build_opacity(&self) -> f32 {
        self.selection_opacity
    }

    /// TRANSPARENCY: the color the scene pass clears to. Fully transparent
    /// (premultiplied zero) while the window is translucent, so padding/gaps
    /// show the desktop and cell background quads blend to premultiplied
    /// `(rgb·a, a)` over it. Otherwise the opaque theme clear (unchanged).
    pub(super) fn scene_clear_color(&self) -> wgpu::Color {
        scene_clear_color(self.window_bg_alpha, self.clear_color)
    }

    /// Whether the window-padding / pane-gap edge wash must be emitted this
    /// frame: either a background image with translucent cells (NF11) or a
    /// translucent window (TRANSPARENCY, where the scene clear is transparent
    /// so padding would otherwise show raw desktop instead of the themed
    /// background at the window alpha). Neither → no wash (byte-identical).
    fn needs_edge_wash(&self) -> bool {
        self.window_bg_alpha < 1.0 || (self.bg_image.is_some() && self.cell_bg_opacity < 1.0)
    }

    fn content_origin(&self) -> [f32; 2] {
        // RV4: the vertical origin is shifted by the smooth-scroll sub-row
        // offset (`0.0` at rest / on the off path, so this is byte-identical to
        // `[pad, pad]` unless a glide is in flight). Shifting the origin glides
        // the whole rendered viewport — cells, cursor, and overlays — uniformly.
        [
            self.window_padding.as_f32(),
            self.window_padding.as_f32() + self.scroll_frac_offset,
        ]
    }

    /// Assemble the frame's [`grid::ChromePin`] from the pinned-chrome geometry
    /// and the live sub-row offset. Inert (`ChromePin::NONE`) whenever there is
    /// no chrome to pin or no glide is in flight, so the plain / at-rest path
    /// stays byte-identical.
    fn chrome_pin(&self) -> grid::ChromePin {
        let Some(geom) = self.chrome_pin_geom else {
            return grid::ChromePin::NONE;
        };
        // Preserve chrome geometry even at rest: the grid builder also uses the
        // descriptor to keep spatial background treatments off chrome cells.
        grid::ChromePin {
            scroll_offset_y: self.scroll_frac_offset,
            top_rows: geom.top_rows,
            rail_col_start: geom.rail_col_start,
            rail_col_end: geom.rail_col_end,
            band_glyph_dy_rows: geom.band_glyph_dy_rows,
            rail_glyph_dy_rows: geom.rail_glyph_dy_rows,
            gap_x: geom.gap_x,
            gap_y: geom.gap_y,
        }
    }

    /// CHROME-GAP: the origin for CONTENT-anchored cursor geometry — the
    /// content origin plus the chrome-facing gap shifts the content cells carry
    /// in the composited single-pane frame. The decorated snapshot's cursor is
    /// always a content cell, so one uniform offset (no per-cell dispatch)
    /// keeps the cursor block, glow, and streak registered with the shifted
    /// cells. Identical to [`Self::content_origin`] when no gap is in play.
    fn cursor_content_origin(&self) -> [f32; 2] {
        let origin = self.content_origin();
        let pin = self.chrome_pin();
        [origin[0] + pin.content_dx(), origin[1] + pin.content_dy()]
    }

    /// Rebuild the compact cell-instance buffer from a fresh terminal snapshot.
    ///
    /// Called on the UI thread after the pump thread signals new PTY output.
    /// The CPU-side instance data is rebuilt into reusable storage (cleared and
    /// refilled, not reallocated) and the GPU vertex buffer only grows when
    /// the rebuilt data exceeds its current capacity, so a coalesced update
    /// never recreates the buffer. The caller must already hold the snapshot
    /// by value — the terminal mutex is dropped before this runs so the lock
    /// is never held across GPU calls.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn update_from_snapshot(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        cursor_glow: Option<CursorGlowRequest>,
        cursor_streak: Option<CursorStreakRequest>,
        cursor_params: CursorRenderParams,
        focus_dim: f32,
        treatment: grid::BackgroundTreatmentParams,
    ) {
        self.update_from_snapshot_with_overlays(
            snapshot,
            cursor_style,
            &[],
            cursor_glow,
            cursor_streak,
            cursor_params,
            focus_dim,
            treatment,
            PanelFrameQuads {
                base_gaps: &[],
                overlays: &[],
            },
            None,
        );
    }

    /// Rebuild the vertex buffers from **several panes** drawn into one window,
    /// each at its own pixel origin, plus the themed divider quads between them
    /// (design doc §3.2). This is the multi-pane analogue of
    /// [`Self::update_from_snapshot_with_overlays`]; the single-pane path never
    /// calls it, so the byte-identical fast path is untouched.
    ///
    /// Buffer layout matches the single path so `draw_scene` is unchanged: all
    /// panes' background quads accumulate first (`[0..background_vertex_count]`),
    /// then all panes' coverage glyphs (`..cell_vertex_count`), then the
    /// dividers + per-pane overlays + the focused pane's cursor + the optional
    /// topmost window overlay (`..vertex_count`). Color glyphs accumulate into
    /// the dedicated buffer.
    ///
    /// `overlay_top` is an open window-level overlay (context menu / settings /
    /// palette / connections / replay) painted in window space. Its full cell
    /// vertices (background **and** glyphs) are appended last so the panel draws
    /// opaquely over every pane — a `PaneRender` could not, since its background
    /// quads would land in the shared background segment behind other panes'
    /// glyphs. `None` leaves the multi-pane frame unchanged.
    ///
    /// Glyph caching is done in two passes: every pane's glyphs are ensured in
    /// the atlas *before* any pane's vertices are built, so a later pane growing
    /// the atlas can never invalidate an earlier pane's UVs (the single path
    /// gets this for free with one snapshot; multi-pane must order it
    /// explicitly).
    ///
    /// Called by the Phase 1c-3 App render dispatch (`app::panes`); the
    /// single-pane path keeps using `update_from_snapshot*`.
    pub(in crate::native) fn update_from_panes(
        &mut self,
        panes: &[PaneRender],
        cursor_params: CursorRenderParams,
        dividers: &[SolidQuad],
        overlay_top: Option<OverlayTop>,
        panel: PanelFrameQuads,
        rail_overlay: Option<RailOverlay>,
    ) {
        // Pass A: ensure all panes' glyphs in both atlases, capturing each
        // pane's color-glyph runs for the build pass.
        let mut pane_runs: Vec<Vec<ColorGlyphRun>> = Vec::with_capacity(panes.len());
        let mut pane_ligature_runs: Vec<Vec<LigatureRun>> = Vec::with_capacity(panes.len());
        for pane in panes {
            let runs = self
                .emoji_rasterizer
                .build_color_glyph_runs(pane.snapshot, &mut self.color_glyph_atlas);
            let mut ligature_runs = self.build_ligature_runs(pane.snapshot, &runs);
            ensure_snapshot_glyphs_excluding_color_runs(
                &mut self.atlas,
                &self.fonts,
                pane.snapshot,
                &runs,
            );
            self.ensure_ligature_glyphs(&mut ligature_runs);
            pane_runs.push(runs);
            pane_ligature_runs.push(ligature_runs);
        }
        // The topmost overlay panel is text-only (borders, labels, values); its
        // mono glyphs must be in the atlas before any vertices are built. It
        // carries no color glyphs (overlays never render emoji), so it is
        // excluded with an empty run list.
        if let Some(overlay) = overlay_top.as_ref() {
            ensure_snapshot_glyphs_excluding_color_runs(
                &mut self.atlas,
                &self.fonts,
                overlay.snapshot,
                &[],
            );
        }
        // F4-P3: the revealed rail overlay strip's mono glyphs join the atlas in
        // the same ensure pass.
        if let Some(rail) = rail_overlay.as_ref() {
            self.ensure_rail_overlay_glyphs(rail);
        }
        if self.atlas.take_dirty() {
            self.refresh_atlas_texture();
        }
        if self.color_glyph_atlas.take_dirty() {
            self.refresh_color_glyph_atlas_texture();
        }

        // Pass B: build vertices. Backgrounds accumulate into `self.vertices`
        // directly; glyphs into `glyph_segment`; dividers + overlays + cursor
        // into `tail`. Color glyphs accumulate straight into their buffer.
        self.vertices.clear();
        self.color_glyph_vertices.clear();
        let mut glyph_segment: Vec<Vertex> = Vec::new();
        let mut tail: Vec<Vertex> = Vec::new();
        let mut pane_buf: Vec<Vertex> = Vec::new();
        // Per-pane color-glyph scratch. `build_color_glyph_vertices_with_origin_into`
        // clears its `out` (the single-pane callers rebuild the whole buffer), so
        // the multi-pane loop must build into a scratch buffer and *extend* the
        // shared accumulator — mirroring the `pane_buf` -> `glyph_segment` pattern
        // below. Building straight into `self.color_glyph_vertices` would wipe
        // earlier panes' emoji and desync the `color_start` offset (a slice
        // `[color_start..]` on the now-empty buffer panicked in multi-pane render).
        let mut pane_color_buf: Vec<ColorGlyphVertex> = Vec::new();
        let mut cursor_glow_instance = None;
        let mut cursor_streak_instance = None;
        let mut retained_cursor_overlays = Vec::new();
        let mut retained_cursor_glow = None;
        let mut retained_cursor_streak = None;
        for ((pane, runs), ligature_runs) in panes
            .iter()
            .zip(pane_runs.iter())
            .zip(pane_ligature_runs.iter())
        {
            pane_buf.clear();
            grid::build_cell_vertices_with_ligatures_and_selection_into(
                &mut pane_buf,
                pane.snapshot,
                &self.atlas,
                runs,
                ligature_runs,
                pane.focus_dim,
                pane.origin,
                pane.treatment,
                self.content_build_opacity(),
                // COLORED-BG-FLOOR: content panes float colored backgrounds to
                // the knob's alpha; chrome strips pass the plain content alpha —
                // the exact inert path — so band fills stay under
                // `tab_panel_strength`'s contract.
                if pane.chrome {
                    self.content_build_opacity()
                } else {
                    self.colored_content_build_opacity()
                },
                // TEXT-BRIGHTNESS: uniform lift across content panes and chrome
                // strip labels alike (`1.0` = identity).
                self.text_brightness,
                // Multi-pane panes never carry an overlay panel: it is composited
                // as a separate opaque `OverlayTop` layer, so no cell is forced.
                None,
                // Sub-cell glide is expressed via `pane.origin[1]` + the vertical
                // clip below, not the single-pane chrome-seam pin. TAB-LABEL-
                // CENTERING: a chrome strip carries a band/rail label offset here
                // (0.0 on every content pane, so this is `ChromePin::NONE` for
                // them and the split content frame is byte-identical).
                pane_chrome_pin(pane),
                // SELECTION-OPACITY: this pane's selected cells draw at the
                // independent selection strength (`1.0` = fully opaque default).
                self.selection_build_opacity(),
            );
            let bg = background_vertex_count(pane.snapshot).min(pane_buf.len() as u32) as usize;
            // PANE-SUBCELL-CLIP: when this pane is mid sub-cell glide, its origin
            // is shifted down by a fractional row. Fill the thin gap that opens
            // at the pane's content top with the first row's own backgrounds
            // (mirrors the single-pane chrome-seam first-row flush), then clamp
            // every quad to the pane's content band so the partial bottom row
            // cannot smear across the divider into the neighbour. Inert
            // (`VClip::NONE`) at rest and for single-pane, so the split-at-rest
            // frame is byte-identical.
            if pane.clip.active() {
                let row0_quads = pane_row0_bg_quads(pane.snapshot);
                grid::extend_first_row_bg_to_top(&mut pane_buf[..bg], row0_quads, pane.clip.top_y);
                grid::clip_quads_vertical(&mut pane_buf, pane.clip);
            }
            if let Some(clip) = pane.content_clip {
                grid::clip_quads_to_rect(&mut pane_buf, clip);
            }
            self.vertices.extend_from_slice(&pane_buf[..bg]);
            glyph_segment.extend_from_slice(&pane_buf[bg..]);

            // Build this pane's color glyphs into scratch, clip, then extend the
            // shared accumulator. Extracted so the accumulation is unit-tested
            // without a GPU device (see `accumulate_pane_color_glyphs`): the
            // builder clears its output, so the multi-pane loop must extend
            // rather than write into the shared buffer at a captured offset.
            accumulate_pane_color_glyphs(
                &mut self.color_glyph_vertices,
                &mut pane_color_buf,
                &self.color_glyph_atlas,
                pane.snapshot,
                runs,
                pane.origin,
                pane_chrome_pin(pane),
                pane.clip,
                pane.content_clip,
            );

            let tail_start = tail.len();
            tail.reserve(pane.overlays.len() * grid::INSTANCES_PER_QUAD);
            for &overlay in pane.overlays {
                grid::push_solid_quad(&mut tail, overlay);
            }
            if pane.focused {
                cursor_glow_instance = pane.cursor_glow.and_then(|request| {
                    build_cursor_glow_instance(
                        pane.snapshot,
                        self.atlas.cell,
                        pane.cursor_style,
                        pane.origin,
                        cursor_params,
                        self.scale,
                        self.window_bg_alpha,
                        request,
                        pane.cursor_streak,
                    )
                });
                cursor_streak_instance = pane.cursor_streak.and_then(|request| {
                    build_cursor_streak_instance(
                        pane.snapshot,
                        self.atlas.cell,
                        pane.origin,
                        request,
                    )
                });
                retained_cursor_overlays.extend_from_slice(pane.overlays);
                retained_cursor_glow = pane.cursor_glow;
                retained_cursor_streak = pane.cursor_streak;
                grid::append_cursor_vertices_with_origin(
                    &mut tail,
                    pane.snapshot,
                    &self.atlas,
                    pane.cursor_style,
                    pane.origin,
                    cursor_params,
                );
            }
            // The pane's own overlays (selection / search) and cursor ride its
            // glide, so they clamp to the same band — a selection highlight or
            // cursor on the partial edge row cannot bleed past the divider.
            grid::clip_quads_vertical(&mut tail[tail_start..], pane.clip);
            if let Some(clip) = pane.content_clip {
                grid::clip_quads_to_rect(&mut tail[tail_start..], clip);
            }
        }
        self.write_cursor_glow_instance(cursor_glow_instance);
        self.write_cursor_streak_instance(cursor_streak_instance);
        self.retained_cursor_overlays = retained_cursor_overlays;
        self.retained_cursor_glow = retained_cursor_glow;
        self.retained_cursor_streak = retained_cursor_streak;

        // NF11: wash the wallpaper wherever no pane grid covers it (padding
        // band, sub-cell remainder strips, divider gaps) — same gate, color
        // source, and opacity as the single-pane splice in
        // `update_from_snapshot_with_overlays`. Appended at the end of the
        // background segment: wash quads never overlap a grid (no double-tint
        // under translucent cell backgrounds), and glyphs / dividers /
        // overlays draw in later segments on top. Without a background image
        // or with opaque cells, nothing is emitted — byte-identical frames.
        if self.needs_edge_wash()
            && let Some(first) = panes.first()
        {
            let cell_w = self.atlas.cell.width as f32;
            let cell_h = self.atlas.cell.height as f32;
            let grid_rects: Vec<[f32; 4]> = panes
                .iter()
                .map(|pane| {
                    [
                        pane.origin[0],
                        pane.origin[1],
                        pane.origin[0] + pane.snapshot.dimensions.columns as f32 * cell_w,
                        pane.origin[1] + pane.snapshot.dimensions.rows as f32 * cell_h,
                    ]
                })
                .collect();
            // COLORED-BG-FLOOR EXEMPT: the edge wash paints the theme DEFAULT
            // background into padding/gaps — by definition never a colored cell,
            // so it stays on the plain content product.
            let color = linear_rgba(
                first.snapshot.colors.background,
                self.content_build_opacity(),
            );
            let edge_quads = multi_pane_wallpaper_edge_wash_quads(
                &grid_rects,
                [self.config.width, self.config.height],
                color,
            );
            let edge_quads = quads_excluding(&edge_quads, panel.base_gaps);
            self.vertices
                .reserve(edge_quads.len() * grid::INSTANCES_PER_QUAD);
            for quad in edge_quads {
                grid::push_solid_quad(&mut self.vertices, quad);
            }
        }

        for &quad in panel.base_gaps {
            grid::push_solid_quad(&mut self.vertices, quad);
        }

        // F4-P1: tab-panel wash + seam quads close out the background segment,
        // after the NF11 edge wash — same layer as the single-pane splice in
        // `update_from_snapshot_with_overlays`. Empty when no chrome / panel off
        // / seam off, so the multi-pane frame stays byte-identical.
        if !panel.overlays.is_empty() {
            self.vertices
                .reserve(panel.overlays.len() * grid::INSTANCES_PER_QUAD);
            for &quad in panel.overlays {
                grid::push_solid_quad(&mut self.vertices, quad);
            }
        }

        // Assemble the single buffer in draw order: bg | glyph | dividers+tail.
        self.background_vertex_count = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&glyph_segment);
        self.cell_vertex_count = self.vertices.len() as u32;
        // Dividers are themed solid quads in the pane gaps; they live in the
        // overlay segment (after glyphs) and never overlap glyph ink.
        self.vertices
            .reserve(dividers.len() * grid::INSTANCES_PER_QUAD + tail.len());
        for &divider in dividers {
            grid::push_solid_quad(&mut self.vertices, divider);
        }
        self.vertices.extend_from_slice(&tail);
        // Topmost window-level overlay: its full cell vertices (background +
        // glyphs) are appended LAST, after dividers and per-pane overlays, so
        // the panel draws opaquely over every pane in the final
        // `[cell_vertex_count..vertex_count]` segment. The panel snapshot fills
        // its whole rect (no transparent cells), so this is a clean opaque box.
        if let Some(overlay) = overlay_top.as_ref() {
            let mut overlay_buf: Vec<Vertex> = Vec::new();
            grid::build_cell_vertices_with_focus_dim_and_origin_into(
                &mut overlay_buf,
                overlay.snapshot,
                &self.atlas,
                &[],
                0.0,
                overlay.origin,
                overlay.treatment,
                // MENU-OPACITY PARITY: a window-level overlay panel is chrome, not
                // terminal content, so its background is fully opaque regardless of
                // `cell_bg_opacity` or window transparency -- exactly what the
                // single-pane path guarantees by forcing `overlay_opaque_region`
                // to `1.0`. Building at `self.cell_bg_opacity` (default `0.8`) let
                // the panes behind the panel bleed through only in the multi-pane
                // path; `1.0` restores single/multi parity while the surrounding
                // panes keep their own translucency.
                1.0,
                // TEXT-BRIGHTNESS: overlay panel text lifts with the rest of
                // the window's ink (`1.0` = identity).
                self.text_brightness,
                // The overlay-top snapshot IS the panel; its own opaque layer is
                // composited last, so no per-cell force is needed here.
                None,
                grid::ChromePin::NONE,
            );
            self.vertices.extend_from_slice(&overlay_buf);
        }
        // F4-P3: the revealed rail overlay strip is the very last thing drawn —
        // over the panes, dividers, per-pane overlays, and any window overlay —
        // so the floating rail sits atop the live multi-pane content.
        if let Some(rail) = rail_overlay.as_ref() {
            self.push_rail_overlay(rail);
        }
        self.vertex_count = self.vertices.len() as u32;
        self.background_vertex_count = self.background_vertex_count.min(self.vertex_count);
        self.color_glyph_vertex_count = self.color_glyph_vertices.len() as u32;

        // Upload the cell/overlay buffer.
        let needed = vertex_bytes_len(&self.vertices);
        let capacity = grow_vertex_buffer_capacity(self.vertex_buf_capacity_bytes, needed);
        if capacity != self.vertex_buf_capacity_bytes {
            self.vertex_buf = create_vertex_buffer(&self.device, capacity);
            self.vertex_buf_capacity_bytes = capacity;
        }
        if self.vertex_count > 0 {
            self.queue
                .write_buffer(&self.vertex_buf, 0, bytemuck::cast_slice(&self.vertices));
        }

        // Upload the color-glyph buffer (mirrors `rebuild_color_glyph_segment`).
        let cg_needed = std::mem::size_of_val(self.color_glyph_vertices.as_slice()) as u64;
        if cg_needed > self.color_glyph_vertex_buf_capacity_bytes {
            self.color_glyph_vertex_buf_capacity_bytes = cg_needed.next_power_of_two();
            self.color_glyph_vertex_buf = create_color_glyph_vertex_buffer(
                &self.device,
                self.color_glyph_vertex_buf_capacity_bytes,
            );
        }
        if !self.color_glyph_vertices.is_empty() {
            self.queue.write_buffer(
                &self.color_glyph_vertex_buf,
                0,
                bytemuck::cast_slice(&self.color_glyph_vertices),
            );
        }
    }

    /// Resident image textures with the generation each was uploaded from, for
    /// the upload collector's staleness check (animation frame flips).
    pub(in crate::native) fn cached_image_generations(&self) -> BTreeMap<StoredImageId, u64> {
        self.image_layer.cached_generations()
    }

    pub(in crate::native) fn update_image_layer(
        &mut self,
        placements: &[VisiblePlacement],
        uploads: &[ImageUpload],
        row_offset: usize,
        col_offset: usize,
    ) {
        // CHROME-GAP: single-pane inline graphics anchor at CONTENT cells, so
        // their quads carry the same content gap shifts the cell vertices do
        // ([0.0, 0.0] with no gap — byte-identical). The pin geometry is set
        // before the image layer updates on every Full rebuild.
        let pin = self.chrome_pin();
        let content_gap_px = [pin.content_dx(), pin.content_dy()];
        self.image_layer.update_with_padding(
            &self.device,
            &self.queue,
            &self.viewport_buf,
            placements,
            uploads,
            self.atlas.cell,
            self.window_padding,
            row_offset,
            col_offset,
            content_gap_px,
        );
    }

    /// The multipane image cache keys currently resident, as `(namespace, id)`.
    /// The split render path passes each pane's cached subset to the upload
    /// collector so already-resident image bytes are not re-fetched per frame.
    /// The multipane equivalent of [`Self::cached_image_generations`].
    pub(in crate::native) fn cached_pane_image_generations(
        &self,
    ) -> BTreeMap<(u64, StoredImageId), u64> {
        self.image_layer.cached_pane_generations()
    }

    /// MULTIPANE image update: render each visible pane's graphics into its own
    /// sub-rect, clipped by a per-pane scissor so nothing bleeds across a
    /// divider. See [`ImageLayer::update_panes`].
    pub(in crate::native) fn update_pane_image_layers(
        &mut self,
        panes: &[PaneImageInput],
        uploads: &[PaneImageUpload],
    ) {
        self.image_layer.update_panes(
            &self.device,
            &self.queue,
            &self.viewport_buf,
            panes,
            uploads,
            self.atlas.cell,
            [self.config.width, self.config.height],
        );
    }

    /// Set (or clear) the C4 in-terminal image-viewer overlay (Phase 9). The
    /// image is `(rgba, width, height)` of a decoded, tightly-packed RGBA8
    /// buffer; `None` clears it. The fit-rect is computed for the current
    /// surface size, so the image stays centered across resizes. Drawn as the
    /// final scene step — presentation-only, byte-identical when cleared.
    pub(in crate::native) fn set_overlay_image(&mut self, image: Option<(&[u8], u32, u32)>) {
        let viewport_w = self.config.width as f32;
        let viewport_h = self.config.height as f32;
        self.image_layer.set_overlay_image(
            &self.device,
            &self.queue,
            &self.viewport_buf,
            image,
            viewport_w,
            viewport_h,
        );
    }

    /// The centered fit-rect (surface PIXELS, `[x0,y0,x1,y1]`) of the current C4
    /// viewer image, or `None` when no overlay image is set. Delegates to the
    /// image layer — the rect is the one actually drawn, so the App's
    /// click-outside-to-dismiss hit-test (Phase 13d) is pixel-exact.
    pub(in crate::native) fn overlay_image_fit_rect(&self) -> Option<[f32; 4]> {
        self.image_layer.overlay_image_fit_rect()
    }

    /// Rebuild the cell vertex buffer from a fresh terminal snapshot plus
    /// presentation-only solid overlays, drawing the cursor in `cursor_style`.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn update_from_snapshot_with_overlays(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        overlays: &[SolidQuad],
        cursor_glow: Option<CursorGlowRequest>,
        cursor_streak: Option<CursorStreakRequest>,
        cursor_params: CursorRenderParams,
        focus_dim: f32,
        treatment: grid::BackgroundTreatmentParams,
        panel: PanelFrameQuads,
        rail_overlay: Option<RailOverlay>,
    ) {
        let mut color_glyph_runs = std::mem::take(&mut self.color_glyph_runs);
        self.emoji_rasterizer.build_color_glyph_runs_into(
            snapshot,
            &mut self.color_glyph_atlas,
            &mut color_glyph_runs,
        );
        let mut ligature_runs = self.build_ligature_runs(snapshot, &color_glyph_runs);
        ensure_snapshot_glyphs_excluding_color_runs(
            &mut self.atlas,
            &self.fonts,
            snapshot,
            &color_glyph_runs,
        );
        self.ensure_ligature_glyphs(&mut ligature_runs);
        // F4-P3: the revealed rail overlay strip's mono glyphs must join the
        // atlas before any texture refresh, alongside the terminal snapshot's.
        if let Some(rail) = rail_overlay.as_ref() {
            self.ensure_rail_overlay_glyphs(rail);
        }
        if self.atlas.take_dirty() {
            self.refresh_atlas_texture();
        }
        self.rebuild_color_glyph_segment(snapshot, &color_glyph_runs);
        let origin = self.content_origin();
        let content_opacity = self.content_build_opacity();
        // COLORED-BG-FLOOR: composited chrome cells in this decorated snapshot
        // (tab-bar rows / rail columns) are exempted per-cell inside the builder
        // via the chrome pin, which preserves its geometry even at rest.
        let colored_opacity = self.colored_content_build_opacity();
        let selection_opacity = self.selection_build_opacity();
        // VE4 new-output fade: cheap Option clone (None off-path / settled) so
        // the borrow is scoped to a local, away from `&mut self.vertices`.
        let row_fade_spec = self.row_fade.clone();
        // SCROLL-CHROME-BOUNCE: hold composited chrome still while content glides.
        let chrome_pin = self.chrome_pin();
        grid::build_cell_vertices_with_ligatures_selection_and_row_fade_into(
            &mut self.vertices,
            snapshot,
            &self.atlas,
            &color_glyph_runs,
            &ligature_runs,
            focus_dim,
            origin,
            treatment,
            content_opacity,
            colored_opacity,
            // TEXT-BRIGHTNESS: uniform lift across content and composited
            // chrome ink (`1.0` = identity).
            self.text_brightness,
            // TRANSPARENCY (MENU-OPACITY): while the window is translucent an open
            // overlay panel is painted into this very snapshot; its cell span is
            // forced opaque so the panel stays readable while the terminal cells
            // around it keep the window opacity. `None` on the opaque window path
            // (set by the caller) is byte-identical.
            self.overlay_opaque_region,
            chrome_pin,
            // SELECTION-OPACITY: selected cells in this content snapshot draw at
            // the independent selection strength (`1.0` = fully opaque default).
            selection_opacity,
            // VE4 new-output fade: freshly arrived rows ramp their text ink in;
            // `RowFade::NONE` (off / settled) is the byte-identical plain path.
            row_fade_view(row_fade_spec.as_ref()),
        );
        self.color_glyph_runs = color_glyph_runs;
        let background_vertices = background_vertex_count(snapshot).min(self.vertices.len() as u32);
        if self.needs_edge_wash() {
            // CHROME-GAP: the pin-aware wash covers the gap strips between the
            // chrome bands and the shifted content, and widens the washed frame
            // extent to match; a zero-gap pin is byte-identical to the legacy
            // four-strip wash.
            // COLORED-BG-FLOOR EXEMPT: theme-default background wash (see the
            // multi-pane edge wash note).
            let edge_quads = wallpaper_edge_wash_quads_with_pin(
                snapshot,
                self.atlas.cell,
                origin,
                [self.config.width, self.config.height],
                self.content_build_opacity(),
                chrome_pin,
            );
            let edge_quads = quads_excluding(&edge_quads, panel.base_gaps);
            if !edge_quads.is_empty() {
                let insert_at = background_vertices as usize;
                let mut edge_vertices =
                    Vec::with_capacity(edge_quads.len() * grid::INSTANCES_PER_QUAD);
                for quad in edge_quads {
                    grid::push_solid_quad(&mut edge_vertices, quad);
                }
                let added = edge_vertices.len() as u32;
                self.vertices.splice(insert_at..insert_at, edge_vertices);
                self.background_vertex_count = background_vertices.saturating_add(added);
            } else {
                self.background_vertex_count = background_vertices;
            }
        } else {
            self.background_vertex_count = background_vertices;
        }
        if !panel.base_gaps.is_empty() {
            let insert_at = self.background_vertex_count as usize;
            let mut base_vertices =
                Vec::with_capacity(panel.base_gaps.len() * grid::INSTANCES_PER_QUAD);
            for &quad in panel.base_gaps {
                grid::push_solid_quad(&mut base_vertices, quad);
            }
            let added = base_vertices.len() as u32;
            self.vertices.splice(insert_at..insert_at, base_vertices);
            self.background_vertex_count = self.background_vertex_count.saturating_add(added);
        }
        // F4-P1: tab-panel wash + seam quads land at the END of the background
        // segment (after the NF11 edge wash), so the panel re-tints the padding
        // strips + veils the fills and the seam draws over the panel — both
        // still under every glyph. Empty when no chrome / panel off / seam off,
        // leaving the frame byte-identical.
        if !panel.overlays.is_empty() {
            let insert_at = self.background_vertex_count as usize;
            let mut panel_vertices =
                Vec::with_capacity(panel.overlays.len() * grid::INSTANCES_PER_QUAD);
            for &quad in panel.overlays {
                grid::push_solid_quad(&mut panel_vertices, quad);
            }
            let added = panel_vertices.len() as u32;
            self.vertices.splice(insert_at..insert_at, panel_vertices);
            self.background_vertex_count = self.background_vertex_count.saturating_add(added);
        }
        self.cell_vertex_count = self.vertices.len() as u32;
        // Cursor-layer solid overlays (including the motion trail) are appended
        // before the cursor block. The analytic aura uses its dedicated
        // below-glyph pass and is rebuilt from the same cursor inputs below.
        // CHROME-GAP: the cursor anchors at a CONTENT cell of the decorated
        // snapshot, so its origin carries the content gap shifts (identity with
        // no gap). Overlay quads arrive pre-shifted in absolute pixels and are
        // unaffected by this origin.
        let cursor_origin = self.cursor_content_origin();
        append_cursor_layer_vertices(
            &mut self.vertices,
            snapshot,
            &self.atlas,
            cursor_style,
            cursor_origin,
            overlays,
            cursor_params,
        );
        self.rebuild_cursor_glow(
            snapshot,
            cursor_style,
            cursor_origin,
            cursor_params,
            cursor_glow,
            cursor_streak,
        );
        self.rebuild_cursor_streak(snapshot, cursor_origin, cursor_streak);
        self.retained_cursor_overlays.clear();
        self.retained_cursor_overlays.extend_from_slice(overlays);
        self.retained_cursor_glow = cursor_glow;
        self.retained_cursor_streak = cursor_streak;
        // F4-P3: the revealed rail overlay strip draws topmost — after the
        // cursor and every overlay — so the floating band sits over the live
        // content it reveals atop. `None` leaves the frame byte-identical.
        if let Some(rail) = rail_overlay.as_ref() {
            self.push_rail_overlay(rail);
        }
        self.vertex_count = self.vertices.len() as u32;
        self.background_vertex_count = self.background_vertex_count.min(self.vertex_count);
        let needed = vertex_bytes_len(&self.vertices);
        let capacity = grow_vertex_buffer_capacity(self.vertex_buf_capacity_bytes, needed);
        if capacity != self.vertex_buf_capacity_bytes {
            self.vertex_buf = create_vertex_buffer(&self.device, capacity);
            self.vertex_buf_capacity_bytes = capacity;
        }
        if self.vertex_count > 0 {
            self.queue
                .write_buffer(&self.vertex_buf, 0, bytemuck::cast_slice(&self.vertices));
        }
    }

    fn build_ligature_runs(
        &mut self,
        snapshot: &Snapshot,
        color_runs: &[ColorGlyphRun],
    ) -> Vec<LigatureRun> {
        self.ligature_shaper.build_runs_with_features(
            self.ligatures_enabled,
            snapshot,
            &self.fonts,
            color_runs,
            crate::ligature::LatinShapingFeatures {
                ss01: self.ligature_ss01,
                ss02: self.ligature_ss02,
            },
        )
    }

    fn ensure_ligature_glyphs(&mut self, runs: &mut Vec<LigatureRun>) {
        for glyph in runs.iter().flat_map(|run| run.glyphs.iter()) {
            let font = self.fonts.font_for(glyph.key.style);
            let _ = self.atlas.ensure_shaped(font, glyph.key);
        }
        runs.retain(|run| {
            run.glyphs
                .iter()
                .all(|glyph| self.atlas.contains_shaped(glyph.key))
        });
    }

    /// Ensure the F4-P3 rail auto-hide overlay strip's mono glyphs are in the
    /// atlas. Called in the glyph-ensure pass (before any atlas texture refresh)
    /// so the strip's UVs are valid when its vertices are built. The strip is
    /// mono-only (rail labels never render emoji), so it excludes color runs.
    fn ensure_rail_overlay_glyphs(&mut self, rail: &RailOverlay) {
        ensure_snapshot_glyphs_excluding_color_runs(
            &mut self.atlas,
            &self.fonts,
            rail.snapshot,
            &[],
        );
    }

    /// Composite the F4-P3 rail auto-hide overlay strip **topmost**: backgrounds,
    /// outer remainder fills, wash, glyphs, indicators, then seam. Appended to
    /// `self.vertices` after every other segment so the floating rail draws over
    /// live content. The caller must have ensured its glyphs.
    fn push_rail_overlay(&mut self, rail: &RailOverlay) {
        let mut strip: Vec<Vertex> = Vec::new();
        grid::build_cell_vertices_with_focus_dim_and_origin_into(
            &mut strip,
            rail.snapshot,
            &self.atlas,
            &[],
            0.0,
            rail.origin,
            rail.treatment,
            // CHROME-ALPHA: the strip's cell backgrounds compose the window's
            // translucency exactly like the pinned band cells (and every other
            // chrome/content cell), so toggling auto-hide cannot change the
            // band's effective opacity. The raw `cell_bg_opacity` here made the
            // floating rail ignore window transparency entirely.
            // COLORED-BG-FLOOR EXEMPT: chrome strip — this entry point passes
            // the plain alpha for colored cells too, keeping the floating rail
            // identical to the pinned rail band under `tab_panel_strength`.
            self.content_build_opacity(),
            // TEXT-BRIGHTNESS: the floating rail's labels lift with every other
            // glyph so autohide cannot change label ink (`1.0` = identity).
            self.text_brightness,
            // The rail strip is its own floating overlay; no merged panel to
            // force.
            None,
            rail_overlay_chrome_pin(rail.snapshot.dimensions.columns, rail.rail_glyph_dy_rows),
        );
        let bg = background_vertex_count(rail.snapshot).min(strip.len() as u32) as usize;
        self.vertices.extend_from_slice(&strip[..bg]);
        for &quad in rail.base_gaps {
            grid::push_solid_quad(&mut self.vertices, quad);
        }
        if let Some(wash) = rail.wash {
            for quad in quads_excluding(&[wash], rail.base_gaps) {
                grid::push_solid_quad(&mut self.vertices, quad);
            }
        }
        self.vertices.extend_from_slice(&strip[bg..]);
        for &quad in rail.widget_quads {
            grid::push_solid_quad(&mut self.vertices, quad);
        }
        if let Some(seam) = rail.seam {
            grid::push_solid_quad(&mut self.vertices, seam);
        }
    }

    fn rebuild_color_glyph_segment(&mut self, snapshot: &Snapshot, runs: &[ColorGlyphRun]) {
        if self.color_glyph_atlas.take_dirty() {
            self.refresh_color_glyph_atlas_texture();
        }
        let origin = self.content_origin();
        // SCROLL-CHROME-BOUNCE: crop content color glyphs at the tab-bar seam.
        let chrome_pin = self.chrome_pin();
        // VE4 new-output fade: cheap Option clone, borrow scoped to the local.
        let row_fade_spec = self.row_fade.clone();
        grid::build_color_glyph_vertices_with_origin_into(
            &mut self.color_glyph_vertices,
            snapshot,
            &self.color_glyph_atlas,
            runs,
            origin,
            chrome_pin,
            // VE4 new-output fade: emoji on a fading row ramp in with mono ink.
            row_fade_view(row_fade_spec.as_ref()),
        );
        self.color_glyph_vertex_count = self.color_glyph_vertices.len() as u32;

        let needed = std::mem::size_of_val(self.color_glyph_vertices.as_slice()) as u64;
        if needed > self.color_glyph_vertex_buf_capacity_bytes {
            self.color_glyph_vertex_buf_capacity_bytes = needed.next_power_of_two();
            self.color_glyph_vertex_buf = create_color_glyph_vertex_buffer(
                &self.device,
                self.color_glyph_vertex_buf_capacity_bytes,
            );
        }
        if !self.color_glyph_vertices.is_empty() {
            self.queue.write_buffer(
                &self.color_glyph_vertex_buf,
                0,
                bytemuck::cast_slice(&self.color_glyph_vertices),
            );
        }
    }

    pub(in crate::native) fn update_cursor_and_overlays(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        overlays: &[SolidQuad],
        cursor_glow: Option<CursorGlowRequest>,
        cursor_streak: Option<CursorStreakRequest>,
        params: CursorRenderParams,
    ) {
        self.retained_cursor_overlays.clear();
        self.retained_cursor_overlays.extend_from_slice(overlays);
        self.retained_cursor_glow = cursor_glow;
        self.retained_cursor_streak = cursor_streak;
        self.update_cursor_and_overlays_inner(
            snapshot,
            cursor_style,
            overlays,
            cursor_glow,
            cursor_streak,
            params,
        );
    }

    /// Rebuild a held synchronized-output cursor frame with the exact solid
    /// overlays and analytic-aura request retained from the last presented
    /// frame. Blink and easing parameters remain live while trail, glow, and a
    /// frozen large-jump follower stay present until synchronized content releases.
    pub(in crate::native) fn update_cursor_with_retained_overlays(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        params: CursorRenderParams,
    ) {
        let (overlays, cursor_glow, cursor_streak) = retained_cursor_effects(
            &self.retained_cursor_overlays,
            self.retained_cursor_glow,
            self.retained_cursor_streak,
        );
        self.update_cursor_and_overlays_inner(
            snapshot,
            cursor_style,
            &overlays,
            cursor_glow,
            cursor_streak,
            params,
        );
    }

    fn update_cursor_and_overlays_inner(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        overlays: &[SolidQuad],
        cursor_glow: Option<CursorGlowRequest>,
        cursor_streak: Option<CursorStreakRequest>,
        params: CursorRenderParams,
    ) {
        self.cursor_vertices.clear();
        // CHROME-GAP: same content-anchored cursor origin as the Full rebuild
        // (identity with no gap), so a CursorOnly blink frame cannot desync the
        // cursor from the gap-shifted content cells.
        let origin = self.cursor_content_origin();
        // Cursor-layer solid overlays precede the cursor block in
        // `cursor_vertices`. The analytic aura is rebuilt independently into
        // its dedicated below-glyph buffer from these same live inputs.
        append_cursor_layer_vertices(
            &mut self.cursor_vertices,
            snapshot,
            &self.atlas,
            cursor_style,
            origin,
            overlays,
            params,
        );
        self.rebuild_cursor_glow(
            snapshot,
            cursor_style,
            origin,
            params,
            cursor_glow,
            cursor_streak,
        );
        self.rebuild_cursor_streak(snapshot, origin, cursor_streak);

        let cell_vertices = self.cell_vertex_count as usize;
        let needed_vertices = cell_vertices + self.cursor_vertices.len();
        let needed = (needed_vertices * std::mem::size_of::<Vertex>()) as u64;
        let capacity = grow_vertex_buffer_capacity(self.vertex_buf_capacity_bytes, needed);
        if capacity != self.vertex_buf_capacity_bytes {
            self.vertex_buf = create_vertex_buffer(&self.device, capacity);
            self.vertex_buf_capacity_bytes = capacity;
            if cell_vertices > 0 {
                self.queue.write_buffer(
                    &self.vertex_buf,
                    0,
                    bytemuck::cast_slice(&self.vertices[..cell_vertices]),
                );
            }
        }

        self.vertices.truncate(cell_vertices);
        self.vertices.extend_from_slice(&self.cursor_vertices);
        self.vertex_count = self.vertices.len() as u32;
        if !self.cursor_vertices.is_empty() {
            let offset = (cell_vertices * std::mem::size_of::<Vertex>()) as u64;
            self.queue.write_buffer(
                &self.vertex_buf,
                offset,
                bytemuck::cast_slice(&self.cursor_vertices),
            );
        }
    }

    fn rebuild_cursor_glow(
        &mut self,
        snapshot: &Snapshot,
        cursor_style: CursorStyle,
        origin: [f32; 2],
        params: CursorRenderParams,
        request: Option<CursorGlowRequest>,
        follower: Option<CursorStreakRequest>,
    ) {
        let instance = request.and_then(|request| {
            build_cursor_glow_instance(
                snapshot,
                self.atlas.cell,
                cursor_style,
                origin,
                params,
                self.scale,
                self.window_bg_alpha,
                request,
                follower,
            )
        });
        self.write_cursor_glow_instance(instance);
    }

    fn rebuild_cursor_streak(
        &mut self,
        snapshot: &Snapshot,
        origin: [f32; 2],
        request: Option<CursorStreakRequest>,
    ) {
        let instance = request.and_then(|request| {
            build_cursor_streak_instance(snapshot, self.atlas.cell, origin, request)
        });
        self.write_cursor_streak_instance(instance);
    }

    fn write_cursor_streak_instance(&mut self, instance: Option<CursorStreakInstance>) {
        self.cursor_streak_vertices.clear();
        if let Some(instance) = instance {
            append_cursor_streak_vertices(&mut self.cursor_streak_vertices, instance);
        }
        self.cursor_streak_vertex_count = self.cursor_streak_vertices.len() as u32;
        if !self.cursor_streak_vertices.is_empty() {
            self.queue.write_buffer(
                &self.cursor_streak_vertex_buf,
                0,
                bytemuck::cast_slice(&self.cursor_streak_vertices),
            );
        }
    }

    fn write_cursor_glow_instance(&mut self, instance: Option<CursorGlowInstance>) {
        self.cursor_glow_vertices.clear();
        if let Some(instance) = instance {
            append_cursor_glow_vertices(&mut self.cursor_glow_vertices, instance);
        }
        self.cursor_glow_vertex_count = self.cursor_glow_vertices.len() as u32;
        if !self.cursor_glow_vertices.is_empty() {
            self.queue.write_buffer(
                &self.cursor_glow_vertex_buf,
                0,
                bytemuck::cast_slice(&self.cursor_glow_vertices),
            );
        }
    }

    /// Write the current physical surface size into the viewport uniform so the
    /// vertex shader maps pixel-space geometry to NDC correctly after a resize.
    pub(super) fn update_viewport(&self) {
        self.queue.write_buffer(
            &self.viewport_buf,
            0,
            bytemuck::bytes_of(&ViewportUniform {
                size: [self.config.width as f32, self.config.height as f32],
                effect: self.effect,
                text: self.text,
            }),
        );
    }
}

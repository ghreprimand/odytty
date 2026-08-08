// SPDX-License-Identifier: GPL-3.0-only
//! Pane, cursor, overlay, and frame input contracts for the native GPU
//! renderer.
//!
//! These are the value types the application layer hands the renderer each
//! frame plus the pure geometry helpers that turn them into vertex data. They
//! own no GPU resources and never touch the device or surface.

use crate::atlas;
use crate::core::{CursorStyle, Snapshot};
use crate::emoji::ColorGlyphAtlas;
use crate::grid::{self, ColorGlyphRun, ColorGlyphVertex, CursorRenderParams, SolidQuad};
use crate::text;

/// SCROLL-CHROME-BOUNCE: the composited-chrome geometry the App hands the GPU
/// each single-pane frame so [`GpuState::chrome_pin`] can hold the tab bar / rail
/// still while the terminal content glides. Column indices are in the decorated
/// snapshot's coordinate space (post tab-chrome decoration).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native) struct ChromePinGeom {
    pub(in crate::native) top_rows: usize,
    pub(in crate::native) rail_col_start: usize,
    pub(in crate::native) rail_col_end: usize,
    /// TAB-LABEL-CENTERING: sub-row glyph shift (cell-height units) for the top
    /// tab band's label row, including its descender guard. Computed by
    /// the App layer (which knows the bar height + label convention) and copied
    /// straight into [`grid::ChromePin`].
    pub(in crate::native) band_glyph_dy_rows: f32,
    /// TAB-LABEL-CENTERING: the rail analog for the side workspace rail's slot
    /// label, including its descender guard.
    pub(in crate::native) rail_glyph_dy_rows: f32,
    /// CHROME-GAP: pixels between the pinned rail band and the content columns
    /// (the window padding value; 0.0 with no rail or zero padding). See
    /// [`grid::ChromePin::gap_x`] for which cells carry the shift.
    pub(in crate::native) gap_x: f32,
    /// CHROME-GAP: pixels between the pinned top band and the content rows
    /// below it (0.0 with no bar or zero padding).
    pub(in crate::native) gap_y: f32,
}

/// VE4 new-output fade: the App-computed per-content-row FOREGROUND alpha ramp
/// for one single-pane frame, plus the decorated-snapshot chrome offsets that
/// map decorated rows/columns back to content cells (chrome band + rail cells
/// never fade). Stored on [`GpuState`] via [`GpuState::set_row_fade`] and
/// consumed as a [`grid::RowFade`] by the cell + color-glyph vertex builds.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::native) struct RowFadeSpec {
    /// Per-content-row foreground alpha multipliers (index = viewport row;
    /// `1.0` = not fading, floor..1.0 = mid-ramp).
    pub(in crate::native) multipliers: Vec<f32>,
    /// Decorated-snapshot rows above the first content row (tab bar band).
    pub(in crate::native) row_offset: usize,
    /// First decorated-snapshot column carrying content (left rail width).
    pub(in crate::native) col_start: usize,
    /// One past the last content column.
    pub(in crate::native) col_end: usize,
}

/// The `grid::RowFade` view of a stored [`RowFadeSpec`] for a frame's builds.
/// Free function (not a `GpuState` method) so the borrow is scoped to the spec
/// local, not `*self`, at the vertex-build call sites.
pub(super) fn row_fade_view(spec: Option<&RowFadeSpec>) -> grid::RowFade<'_> {
    match spec {
        Some(spec) => grid::RowFade {
            multipliers: &spec.multipliers,
            row_offset: spec.row_offset,
            col_start: spec.col_start,
            col_end: spec.col_end,
        },
        None => grid::RowFade::NONE,
    }
}

/// One pane's render inputs for [`GpuState::update_from_panes`] (design doc
/// §3.2). All geometry is physical pixels in the same origin-top-left basis as
/// `grid.rs` vertices. The caller (the App multi-pane render dispatch) owns
/// layout: `origin` is the pane rect's top-left already folded with that pane's
/// own `scroll_frac_offset` on the y axis, and `overlays` are already shifted
/// into the pane's coordinate space.
///
/// Constructed by the App multi-pane render dispatch (`app::panes`); the
/// single-pane path never touches this type.
pub(in crate::native) struct PaneRender<'a> {
    /// This pane's terminal snapshot (its own grid, scrollback viewport, cursor).
    pub(in crate::native) snapshot: &'a Snapshot,
    /// Pane top-left in physical px, with this pane's scroll glide folded into y.
    pub(in crate::native) origin: [f32; 2],
    /// Whether this pane has keyboard focus — only the focused pane draws a
    /// live cursor (unfocused panes draw none yet; hollow/dim is a
    /// later refinement per §3.3).
    pub(in crate::native) focused: bool,
    /// Cursor style for the focused pane (ignored when `focused` is false).
    pub(in crate::native) cursor_style: CursorStyle,
    /// Inactive-pane dim applied to this pane's cells (0.0 = none); reuses the
    /// existing focus-dim path. Single-pane never sets this.
    pub(in crate::native) focus_dim: f32,
    /// Presentation-only solid overlays (selection/search/hints) already shifted
    /// into this pane's origin space.
    pub(in crate::native) overlays: &'a [SolidQuad],
    /// Background treatment params for this pane's cells.
    pub(in crate::native) treatment: grid::BackgroundTreatmentParams,
    /// PANE-SUBCELL-CLIP: the vertical band this pane's vertices are clamped to,
    /// so a sub-cell scroll glide baked into `origin[1]` cannot smear the partial
    /// top/bottom row past the pane's content rect into a neighbour across the
    /// divider. [`grid::VClip::NONE`] (chrome strips, single-pane, at-rest panes)
    /// is inert, leaving the frame byte-identical.
    pub(in crate::native) clip: grid::VClip,
    /// Actual padded inner rectangle for a content pane. When present, every
    /// emitted background, coverage glyph, colour glyph, selection/search
    /// overlay, and cursor quad is clipped on both axes before batching. `None`
    /// marks chrome and padding-zero panes, preserving those vertex streams.
    pub(in crate::native) content_clip: Option<[f32; 4]>,
    /// TAB-LABEL-CENTERING: sub-row glyph shift (cell-height units) for a top
    /// tab-bar chrome strip, recentering its label row on the band's true center.
    /// `0.0` (every content pane and the rail strip) is inert.
    pub(in crate::native) band_glyph_dy_rows: f32,
    /// TAB-LABEL-CENTERING: sub-row glyph shift for a workspace-rail chrome strip.
    /// `0.0` (every content pane and the top-bar strip) is inert.
    pub(in crate::native) rail_glyph_dy_rows: f32,
    /// Shape-aware cursor aura clip for the focused content pane. `None` is the
    /// exact off path for background panes, chrome strips, reduced motion, and
    /// the default-off `cursor_glow` setting.
    pub(in crate::native) cursor_glow: Option<CursorGlowRequest>,
    /// Large-jump cursor follower for the focused pane. `None` for chrome,
    /// background panes, reduced motion, or an idle/disabled trail.
    pub(in crate::native) cursor_streak: Option<CursorStreakRequest>,
    /// COLORED-BG-FLOOR: whether this pane is a composited chrome strip (top
    /// tab bar / workspace rail) rather than terminal content. Chrome strips
    /// are exempt from the colored-background opacity floor — their effective
    /// opacity is owned by `tab_panel_strength`, whose wash math tops up from
    /// the plain content alpha these cells composite at. The strip's label
    /// offsets cannot stand in for this flag: they are `0.0` for single-row
    /// bands, which would silently un-mark the strip.
    pub(in crate::native) chrome: bool,
}

/// Per-frame request for the analytic cursor aura. Geometry is rebuilt from the
/// same snapshot, origin, style, and [`CursorRenderParams`] as the cursor, so
/// Full and CursorOnly paths cannot diverge. The clip is the terminal content
/// rectangle in physical window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native) struct CursorGlowRequest {
    pub(in crate::native) clip_rect: [f32; 4],
    /// User-facing normalized aura strength (`cursor_glow_intensity`, 0.0..=1.0).
    /// Resolved from settings at the single overlay-request choke point so both
    /// GPU update paths scale the peak alpha identically. Folded into the
    /// overlay cache signature so a live change cannot retain a stale aura.
    pub(in crate::native) intensity: f32,
}

/// Per-frame large-jump cursor-follower request. The rectangle is expressed in
/// undecorated content pixels; the shared instance builder adds the pane origin
/// and any single-pane chrome offset used by the real cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native) struct CursorStreakRequest {
    pub(in crate::native) destination: crate::core::Position,
    pub(in crate::native) rect: [f32; 4],
    pub(in crate::native) alpha: f32,
    pub(in crate::native) clip_rect: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native) struct CursorStreakInstance {
    pub(in crate::native) quad_rect: [f32; 4],
    pub(in crate::native) source_rect: [f32; 4],
    pub(in crate::native) color: [f32; 4],
    pub(in crate::native) peak_alpha: f32,
    pub(in crate::native) clip_rect: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(in crate::native) struct CursorStreakVertex {
    pub(in crate::native) pos: [f32; 2],
    pub(in crate::native) source_rect: [f32; 4],
    /// x = peak alpha. Remaining lanes are reserved for shape evolution.
    pub(in crate::native) follower: [f32; 4],
    pub(in crate::native) color: [f32; 4],
    pub(in crate::native) clip_rect: [f32; 4],
}

fn cursor_streak_source_rect(
    snapshot: &Snapshot,
    cell: atlas::CellSize,
    origin: [f32; 2],
    request: CursorStreakRequest,
) -> Option<[f32; 4]> {
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if cols == 0 || rows == 0 || cell.width == 0 || cell.height == 0 {
        return None;
    }
    let decoration_col = snapshot
        .cursor
        .column
        .saturating_sub(request.destination.column) as f32;
    let decoration_row = snapshot.cursor.row.saturating_sub(request.destination.row) as f32;
    let dx = origin[0] + decoration_col * cell.width as f32;
    let dy = origin[1] + decoration_row * cell.height as f32;
    Some([
        request.rect[0] + dx,
        request.rect[1] + dy,
        request.rect[2] + dx,
        request.rect[3] + dy,
    ])
}

pub(in crate::native) fn build_cursor_streak_instance(
    snapshot: &Snapshot,
    cell: atlas::CellSize,
    origin: [f32; 2],
    request: CursorStreakRequest,
) -> Option<CursorStreakInstance> {
    if !snapshot.cursor_visible {
        return None;
    }
    let source_rect = cursor_streak_source_rect(snapshot, cell, origin, request)?;
    if source_rect[0] >= source_rect[2] || source_rect[1] >= source_rect[3] {
        return None;
    }
    let peak_alpha = request.alpha.clamp(0.0, 1.0);
    if peak_alpha <= 0.0 {
        return None;
    }
    let extent = 1.0;
    let quad_rect = intersect_rect(
        [
            source_rect[0] - extent,
            source_rect[1] - extent,
            source_rect[2] + extent,
            source_rect[3] + extent,
        ],
        request.clip_rect,
    )?;
    let cursor = snapshot.colors.cursor;
    Some(CursorStreakInstance {
        quad_rect,
        source_rect,
        color: [
            text::srgb_to_linear(cursor.red),
            text::srgb_to_linear(cursor.green),
            text::srgb_to_linear(cursor.blue),
            1.0,
        ],
        peak_alpha,
        clip_rect: request.clip_rect,
    })
}

pub(in crate::native) fn append_cursor_streak_vertices(
    out: &mut Vec<CursorStreakVertex>,
    instance: CursorStreakInstance,
) {
    let shared = |pos| CursorStreakVertex {
        pos,
        source_rect: instance.source_rect,
        follower: [instance.peak_alpha, 0.0, 0.0, 0.0],
        color: instance.color,
        clip_rect: instance.clip_rect,
    };
    let [x0, y0, x1, y1] = instance.quad_rect;
    let [a, b, c, d] = [[x0, y0], [x0, y1], [x1, y0], [x1, y1]];
    out.extend([
        shared(a),
        shared(b),
        shared(c),
        shared(c),
        shared(b),
        shared(d),
    ]);
}

/// Fully resolved analytic aura instance consumed by the six-vertex GPU pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::native) struct CursorGlowInstance {
    pub(in crate::native) quad_rect: [f32; 4],
    pub(in crate::native) source_rect: [f32; 4],
    pub(in crate::native) radius: f32,
    pub(in crate::native) corner_radius: f32,
    pub(in crate::native) color: [f32; 4],
    pub(in crate::native) peak_alpha: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(in crate::native) struct CursorGlowVertex {
    pub(in crate::native) pos: [f32; 2],
    pub(in crate::native) source_rect: [f32; 4],
    /// x = falloff radius, y = source corner radius, z = peak alpha.
    pub(in crate::native) aura: [f32; 4],
    pub(in crate::native) color: [f32; 4],
}

const CURSOR_GLOW_BLOCK_ALPHA: f32 = 0.08;
const CURSOR_GLOW_THIN_ALPHA: f32 = 0.10;
const CURSOR_GLOW_ALPHA_LIFT: f32 = 0.02;

/// Map the user-facing normalized `cursor_glow_intensity` (0.0..=1.0) to a peak
/// multiplier. The default intensity reproduces the historical fixed peaks
/// exactly (multiplier `1.0`); `0.0` yields no aura; the maximum doubles the
/// peak while the translucency lift cap scales by the same factor so a
/// translucent background never receives an excessive alpha lift.
fn cursor_glow_intensity_multiplier(intensity: f32) -> f32 {
    let clamped = intensity.clamp(
        crate::settings::MIN_CURSOR_GLOW_INTENSITY,
        crate::settings::MAX_CURSOR_GLOW_INTENSITY,
    );
    (clamped / crate::settings::DEFAULT_CURSOR_GLOW_INTENSITY).max(0.0)
}

fn cursor_glow_peak_alpha(
    style: CursorStyle,
    cursor_alpha: f32,
    content_alpha: f32,
    intensity: f32,
) -> f32 {
    let multiplier = cursor_glow_intensity_multiplier(intensity);
    if multiplier <= 0.0 {
        return 0.0;
    }
    let base = match style {
        CursorStyle::Block => CURSOR_GLOW_BLOCK_ALPHA,
        CursorStyle::Underline | CursorStyle::Bar => CURSOR_GLOW_THIN_ALPHA,
    } * multiplier;
    let eased = base * cursor_alpha.clamp(0.0, 1.0).powi(2);
    let transparency_cap =
        (CURSOR_GLOW_ALPHA_LIFT * multiplier) / (1.0 - content_alpha.clamp(0.0, 1.0)).max(0.001);
    eased.min(transparency_cap).clamp(0.0, 1.0)
}

fn intersect_rect(rect: [f32; 4], clip: [f32; 4]) -> Option<[f32; 4]> {
    let intersection = [
        rect[0].max(clip[0]),
        rect[1].max(clip[1]),
        rect[2].min(clip[2]),
        rect[3].min(clip[3]),
    ];
    (intersection[0] < intersection[2] && intersection[1] < intersection[3]).then_some(intersection)
}

/// Resolve one cursor aura from the exact live cursor inputs. This is the only
/// instance builder used by Full, CursorOnly, and multi-pane updates.
#[allow(clippy::too_many_arguments)]
pub(in crate::native) fn build_cursor_glow_instance(
    snapshot: &Snapshot,
    cell: atlas::CellSize,
    cursor_style: CursorStyle,
    origin: [f32; 2],
    params: CursorRenderParams,
    scale: f32,
    content_alpha: f32,
    request: CursorGlowRequest,
    follower: Option<CursorStreakRequest>,
) -> Option<CursorGlowInstance> {
    let cursor_alpha = follower.map_or(params.alpha, |follower| follower.alpha);
    if !snapshot.cursor_visible || !params.focused || cursor_alpha <= 0.0 {
        return None;
    }
    let cols = snapshot.dimensions.columns;
    let rows = snapshot.dimensions.rows;
    if cols == 0 || rows == 0 {
        return None;
    }
    let cell_w = cell.width as f32;
    let cell_h = cell.height as f32;
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return None;
    }

    let col = snapshot.cursor.column.min(cols - 1) as f32;
    let row = snapshot.cursor.row.min(rows - 1) as f32;
    let x0 = origin[0] + col * cell_w + params.offset[0];
    let y0 = origin[1] + row * cell_h + params.offset[1];
    let source_rect = follower
        .and_then(|follower| cursor_streak_source_rect(snapshot, cell, origin, follower))
        .unwrap_or_else(|| match cursor_style {
            CursorStyle::Block => [x0, y0, x0 + cell_w, y0 + cell_h],
            CursorStyle::Underline => grid::cursor_underline_rect(x0, y0, cell_w, cell_h),
            CursorStyle::Bar => grid::cursor_bar_rect(x0, y0, cell_w, cell_h),
        });

    let scale = scale.max(0.001);
    let source_w = source_rect[2] - source_rect[0];
    let source_h = source_rect[3] - source_rect[1];
    let radius = (0.35 * cell_w.min(cell_h)).clamp(3.0 * scale, 5.0 * scale);
    let corner_radius = match cursor_style {
        CursorStyle::Block => (1.0 * scale).min(0.15 * source_w.min(source_h)),
        CursorStyle::Underline | CursorStyle::Bar => 0.5 * source_w.min(source_h),
    };
    let extent = (1.25 * radius + 1.0).ceil();
    let quad_rect = intersect_rect(
        [
            source_rect[0] - extent,
            source_rect[1] - extent,
            source_rect[2] + extent,
            source_rect[3] + extent,
        ],
        request.clip_rect,
    )?;
    let peak_alpha =
        cursor_glow_peak_alpha(cursor_style, cursor_alpha, content_alpha, request.intensity);
    if peak_alpha <= 0.0 {
        return None;
    }
    let cursor = snapshot.colors.cursor;
    Some(CursorGlowInstance {
        quad_rect,
        source_rect,
        radius,
        corner_radius,
        color: [
            text::srgb_to_linear(cursor.red),
            text::srgb_to_linear(cursor.green),
            text::srgb_to_linear(cursor.blue),
            1.0,
        ],
        peak_alpha,
    })
}

pub(in crate::native) fn append_cursor_glow_vertices(
    out: &mut Vec<CursorGlowVertex>,
    instance: CursorGlowInstance,
) {
    let [x0, y0, x1, y1] = instance.quad_rect;
    let shared = |pos| CursorGlowVertex {
        pos,
        source_rect: instance.source_rect,
        aura: [
            instance.radius,
            instance.corner_radius,
            instance.peak_alpha,
            0.0,
        ],
        color: instance.color,
    };
    out.extend([
        shared([x0, y0]),
        shared([x0, y1]),
        shared([x1, y0]),
        shared([x1, y0]),
        shared([x0, y1]),
        shared([x1, y1]),
    ]);
}

pub(in crate::native) fn retained_cursor_effects(
    overlays: &[SolidQuad],
    cursor_glow: Option<CursorGlowRequest>,
    cursor_streak: Option<CursorStreakRequest>,
) -> (
    Vec<SolidQuad>,
    Option<CursorGlowRequest>,
    Option<CursorStreakRequest>,
) {
    (overlays.to_vec(), cursor_glow, cursor_streak)
}

#[cfg(test)]
pub(in crate::native) fn cursor_glow_falloff(outside: f32, radius: f32) -> f32 {
    let normalized = outside.max(0.0) / radius.max(0.001);
    2.0_f32.powf(-4.0 * normalized * normalized)
}

/// A window-level overlay drawn **topmost** over a multi-pane composite
/// (context menu / settings / palette / connections / replay). Unlike a
/// [`PaneRender`] — whose background quads land in the shared background segment
/// and so would be overdrawn by other panes' glyphs — an `OverlayTop`'s full
/// cell vertices (background **and** glyphs) are appended after the dividers and
/// per-pane overlays, so the panel composites opaquely on top of everything.
/// The snapshot is sized and positioned in window space by the caller; in the
/// single-pane path the overlay is painted into the terminal snapshot instead,
/// so this type is never used there.
pub(in crate::native) struct OverlayTop<'a> {
    /// The overlay panel snapshot (its own grid, fully opaque within its rect).
    pub(in crate::native) snapshot: &'a Snapshot,
    /// Panel top-left in physical px (window space).
    pub(in crate::native) origin: [f32; 2],
    /// Background treatment params (matches the panes so any global treatment
    /// is consistent across the frame).
    pub(in crate::native) treatment: grid::BackgroundTreatmentParams,
}

/// The F4-P3 rail **auto-hide overlay**: the revealed rail band drawn as a
/// floating strip at its window edge, over live content, without any content
/// reflow. Composited topmost like an [`OverlayTop`], but with a layered stack
/// around the strip snapshot so the band reads over the terminal beneath it
/// (CHROME-ALPHA: cells and wash compose the same effective translucency as
/// the pinned bands, so autohide state never changes the band's opacity):
/// 1. strip cell backgrounds and panel-colored outer remainder strips.
/// 2. `wash` over cell backgrounds, excluding the already-filled remainders.
/// 3. strip glyphs and `widget_quads` reorder indicators.
/// 4. `seam` on the content-facing edge.
///
/// Emitted only while the rail is revealed; `None` leaves every frame unchanged.
pub(in crate::native) struct RailOverlay<'a> {
    /// The `rail_cols × rows` strip snapshot (rail glyphs + baked panel tint).
    pub(in crate::native) snapshot: &'a Snapshot,
    /// Strip top-left in physical px (window space).
    pub(in crate::native) origin: [f32; 2],
    /// Background treatment params (matches the frame).
    pub(in crate::native) treatment: grid::BackgroundTreatmentParams,
    /// Descender-safe slot-centering offset shared with pinned rail strips.
    pub(in crate::native) rail_glyph_dy_rows: f32,
    /// Active and reorder indicators in window pixel geometry.
    pub(in crate::native) widget_quads: &'a [SolidQuad],
    /// Panel-colored outer padding and sub-cell remainder strips.
    pub(in crate::native) base_gaps: &'a [SolidQuad],
    /// Occluding wash quad drawn under the strip, or `None`.
    pub(in crate::native) wash: Option<SolidQuad>,
    /// Content-facing seam quad drawn over the strip, or `None`.
    pub(in crate::native) seam: Option<SolidQuad>,
}

#[derive(Clone, Copy)]
pub(in crate::native) struct PanelFrameQuads<'a> {
    pub(in crate::native) base_gaps: &'a [SolidQuad],
    pub(in crate::native) overlays: &'a [SolidQuad],
}

pub(in crate::native) fn quads_excluding(
    quads: &[SolidQuad],
    exclusions: &[SolidQuad],
) -> Vec<SolidQuad> {
    let mut current = quads.to_vec();
    for exclusion in exclusions {
        let mut next = Vec::new();
        for quad in current {
            next.extend(
                crate::native::app::tab_panel::rect_without(quad.rect, exclusion.rect)
                    .into_iter()
                    .map(|rect| SolidQuad {
                        rect,
                        color: quad.color,
                    }),
            );
        }
        current = next;
    }
    current
}

/// TAB-LABEL-CENTERING: the [`grid::ChromePin`] for one multi-pane render input.
/// A content pane (and a chrome strip with no label offset) yields
/// `ChromePin::NONE` — byte-identical to the pre-feature split path. A top tab
/// bar or workspace-rail chrome strip yields a pin carrying only its sub-row
/// label offset: the strip snapshot IS the band, so the whole snapshot's rows
/// (top bar) or columns (rail) are the band the offset applies to. Sub-cell
/// scroll glide on a chrome strip is never in play (`scroll_offset_y == 0`), so
/// the scroll-pin branches stay inert; only the label offset fires.
pub(super) fn pane_chrome_pin(pane: &PaneRender) -> grid::ChromePin {
    if pane.band_glyph_dy_rows == 0.0 && pane.rail_glyph_dy_rows == 0.0 {
        return grid::ChromePin::NONE;
    }
    grid::ChromePin {
        scroll_offset_y: 0.0,
        top_rows: if pane.band_glyph_dy_rows != 0.0 {
            pane.snapshot.dimensions.rows
        } else {
            0
        },
        rail_col_start: 0,
        rail_col_end: if pane.rail_glyph_dy_rows != 0.0 {
            pane.snapshot.dimensions.columns
        } else {
            0
        },
        band_glyph_dy_rows: pane.band_glyph_dy_rows,
        rail_glyph_dy_rows: pane.rail_glyph_dy_rows,
        // CHROME-GAP: multi-pane chrome strips render at their own gap-aware
        // pixel origins; no per-cell shift is composited into a strip snapshot.
        gap_x: 0.0,
        gap_y: 0.0,
    }
}

/// Accumulate one pane's color glyphs (emoji) into the shared per-frame buffer.
///
/// `build_color_glyph_vertices_with_origin_into` clears its output on entry —
/// the two single-pane callers rebuild the whole buffer, so that clear is their
/// contract. The multi-pane loop therefore builds each pane into `scratch` and
/// then *extends* `shared`; writing straight into `shared` would clear away
/// every earlier pane's glyphs and desync the frame. A pane whose emoji count
/// is lower than an earlier pane's used to leave a captured `shared[start..]`
/// offset pointing past the end of the emptied buffer, which panicked and — via
/// the abort-on-panic hook — took the window down.
///
/// This is a free function so the accumulation can be exercised without a GPU
/// device or window: the panic only reproduced across panes with uneven emoji
/// counts, which needs no rendering to trigger. It takes the per-pane geometry
/// as primitives rather than a `&PaneRender` for that reason — a test can drive
/// it from a bare snapshot and run list without building a full pane record.
#[allow(clippy::too_many_arguments)]
pub(in crate::native) fn accumulate_pane_color_glyphs(
    shared: &mut Vec<ColorGlyphVertex>,
    scratch: &mut Vec<ColorGlyphVertex>,
    atlas: &ColorGlyphAtlas,
    snapshot: &Snapshot,
    runs: &[ColorGlyphRun],
    origin: [f32; 2],
    // TAB-LABEL-CENTERING: a chrome strip's emoji label centers with the same
    // offset as its mono glyphs; `ChromePin::NONE` for content panes.
    chrome_pin: grid::ChromePin,
    clip: grid::VClip,
    content_clip: Option<[f32; 4]>,
) {
    grid::build_color_glyph_vertices_with_origin_into(
        scratch,
        snapshot,
        atlas,
        runs,
        origin,
        chrome_pin,
        // VE4 new-output fade: single-pane only (parity with the prior overlay
        // mechanism); split panes never fade.
        grid::RowFade::NONE,
    );
    // Colour glyphs obey the same per-pane clip so a gliding emoji's partial row
    // is cropped, not smeared across the divider. Clip the scratch, then extend
    // the shared accumulator — the builder clears `scratch`, so writing into
    // `shared` directly would wipe earlier panes' glyphs.
    grid::clip_quads_vertical(scratch, clip);
    if let Some(rect) = content_clip {
        grid::clip_quads_to_rect(scratch, rect);
    }
    shared.extend_from_slice(scratch);
}

pub(in crate::native) fn rail_overlay_chrome_pin(
    columns: usize,
    rail_glyph_dy_rows: f32,
) -> grid::ChromePin {
    if rail_glyph_dy_rows == 0.0 {
        return grid::ChromePin::NONE;
    }
    grid::ChromePin {
        scroll_offset_y: 0.0,
        top_rows: 0,
        rail_col_start: 0,
        rail_col_end: columns,
        band_glyph_dy_rows: 0.0,
        rail_glyph_dy_rows,
        // CHROME-GAP: the floating auto-hide overlay is full-bleed by design
        // (no reflow, no gap) — it never composites a chrome-facing gap.
        gap_x: 0.0,
        gap_y: 0.0,
    }
}

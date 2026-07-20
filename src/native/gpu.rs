// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ab_glyph::FontVec;
use wgpu::util::DeviceExt;

use crate::atlas;
use crate::core::{CursorStyle, RgbColor, Snapshot};
use crate::emoji::{ColorGlyphAtlas, EmojiRasterizer};
use crate::graphics::{StoredImageId, VisiblePlacement};
use crate::grid::{self, ColorGlyphRun, ColorGlyphVertex, CursorRenderParams, SolidQuad, Vertex};
use crate::ligature::{LigatureRun, LigatureShaper};
use crate::text::{self, GlyphAtlas, SubpixelMode};
use crate::theme::{Theme, VisualEffect};

use winit::window::Window;

use super::image_layer::{ImageLayer, ImageUpload, PaneImageInput, PaneImageUpload};
use super::options::{NativeError, NativeOptions};
use super::pty::UserEvent;
use super::session::SessionToken;
use super::viewport::WindowPadding;

pub(super) mod default_background;
pub(super) mod fonts;
pub(super) mod image;
pub(super) mod post;

pub(super) use fonts::StyleFonts;
use fonts::{
    effective_symbol_fallback_enabled, effective_symbol_font_path, install_runtime_symbol_resolver,
    resolve_symbol_fallback, resolve_symbol_map_fonts,
};
use image::BgImageGpu;
pub(super) use post::{BloomOptions, CrtOptions};
use post::{PostProcessOptions, PostProcessResources};

fn report_uncaptured_gpu_error(error: wgpu::Error) {
    tracing::error!("uncaptured GPU error: {error}");
}

fn uncaptured_error_handler() -> Arc<dyn wgpu::UncapturedErrorHandler> {
    Arc::new(report_uncaptured_gpu_error)
}

fn install_gpu_error_handlers(
    device: &wgpu::Device,
    device_lost: Arc<AtomicBool>,
    event_proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
    session: SessionToken,
) {
    device.on_uncaptured_error(uncaptured_error_handler());
    device.set_device_lost_callback(move |reason, message| {
        tracing::error!("GPU device lost ({reason:?}): {message}");
        device_lost.store(true, Ordering::Release);
        if let Some(proxy) = event_proxy.as_ref() {
            let _ = proxy.send_event(UserEvent::Redraw { session });
        }
    });
}

#[cfg(test)]
mod availability_tests {
    use super::*;

    #[test]
    fn uncaptured_validation_error_is_reported_without_panicking() {
        let error = wgpu::Error::Validation {
            source: Box::new(std::io::Error::other("test validation source")),
            description: "test validation error".to_owned(),
        };
        uncaptured_error_handler()(error);
    }
}

/// SCROLL-CHROME-BOUNCE: the composited-chrome geometry the App hands the GPU
/// each single-pane frame so [`GpuState::chrome_pin`] can hold the tab bar / rail
/// still while the terminal content glides. Column indices are in the decorated
/// snapshot's coordinate space (post tab-chrome decoration).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChromePinGeom {
    pub(super) top_rows: usize,
    pub(super) rail_col_start: usize,
    pub(super) rail_col_end: usize,
    /// TAB-LABEL-CENTERING: sub-row glyph shift (cell-height units) for the top
    /// tab band's label row, including its descender guard. Computed by
    /// the App layer (which knows the bar height + label convention) and copied
    /// straight into [`grid::ChromePin`].
    pub(super) band_glyph_dy_rows: f32,
    /// TAB-LABEL-CENTERING: the rail analog for the side workspace rail's slot
    /// label, including its descender guard.
    pub(super) rail_glyph_dy_rows: f32,
    /// CHROME-GAP: pixels between the pinned rail band and the content columns
    /// (the window padding value; 0.0 with no rail or zero padding). See
    /// [`grid::ChromePin::gap_x`] for which cells carry the shift.
    pub(super) gap_x: f32,
    /// CHROME-GAP: pixels between the pinned top band and the content rows
    /// below it (0.0 with no bar or zero padding).
    pub(super) gap_y: f32,
}

/// VE4 new-output fade: the App-computed per-content-row FOREGROUND alpha ramp
/// for one single-pane frame, plus the decorated-snapshot chrome offsets that
/// map decorated rows/columns back to content cells (chrome band + rail cells
/// never fade). Stored on [`GpuState`] via [`GpuState::set_row_fade`] and
/// consumed as a [`grid::RowFade`] by the cell + color-glyph vertex builds.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct RowFadeSpec {
    /// Per-content-row foreground alpha multipliers (index = viewport row;
    /// `1.0` = not fading, floor..1.0 = mid-ramp).
    pub(super) multipliers: Vec<f32>,
    /// Decorated-snapshot rows above the first content row (tab bar band).
    pub(super) row_offset: usize,
    /// First decorated-snapshot column carrying content (left rail width).
    pub(super) col_start: usize,
    /// One past the last content column.
    pub(super) col_end: usize,
}

/// The `grid::RowFade` view of a stored [`RowFadeSpec`] for a frame's builds.
/// Free function (not a `GpuState` method) so the borrow is scoped to the spec
/// local, not `*self`, at the vertex-build call sites.
fn row_fade_view(spec: Option<&RowFadeSpec>) -> grid::RowFade<'_> {
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
pub(super) struct PaneRender<'a> {
    /// This pane's terminal snapshot (its own grid, scrollback viewport, cursor).
    pub(super) snapshot: &'a Snapshot,
    /// Pane top-left in physical px, with this pane's scroll glide folded into y.
    pub(super) origin: [f32; 2],
    /// Whether this pane has keyboard focus — only the focused pane draws a
    /// live cursor (unfocused panes draw none yet; hollow/dim is a
    /// later refinement per §3.3).
    pub(super) focused: bool,
    /// Cursor style for the focused pane (ignored when `focused` is false).
    pub(super) cursor_style: CursorStyle,
    /// Inactive-pane dim applied to this pane's cells (0.0 = none); reuses the
    /// existing focus-dim path. Single-pane never sets this.
    pub(super) focus_dim: f32,
    /// Presentation-only solid overlays (selection/search/hints) already shifted
    /// into this pane's origin space.
    pub(super) overlays: &'a [SolidQuad],
    /// Background treatment params for this pane's cells.
    pub(super) treatment: grid::BackgroundTreatmentParams,
    /// PANE-SUBCELL-CLIP: the vertical band this pane's vertices are clamped to,
    /// so a sub-cell scroll glide baked into `origin[1]` cannot smear the partial
    /// top/bottom row past the pane's content rect into a neighbour across the
    /// divider. [`grid::VClip::NONE`] (chrome strips, single-pane, at-rest panes)
    /// is inert, leaving the frame byte-identical.
    pub(super) clip: grid::VClip,
    /// TAB-LABEL-CENTERING: sub-row glyph shift (cell-height units) for a top
    /// tab-bar chrome strip, recentering its label row on the band's true center.
    /// `0.0` (every content pane and the rail strip) is inert.
    pub(super) band_glyph_dy_rows: f32,
    /// TAB-LABEL-CENTERING: sub-row glyph shift for a workspace-rail chrome strip.
    /// `0.0` (every content pane and the top-bar strip) is inert.
    pub(super) rail_glyph_dy_rows: f32,
    /// Shape-aware cursor aura clip for the focused content pane. `None` is the
    /// exact off path for background panes, chrome strips, reduced motion, and
    /// the default-off `cursor_glow` setting.
    pub(super) cursor_glow: Option<CursorGlowRequest>,
    /// Large-jump cursor follower for the focused pane. `None` for chrome,
    /// background panes, reduced motion, or an idle/disabled trail.
    pub(super) cursor_streak: Option<CursorStreakRequest>,
    /// COLORED-BG-FLOOR: whether this pane is a composited chrome strip (top
    /// tab bar / workspace rail) rather than terminal content. Chrome strips
    /// are exempt from the colored-background opacity floor — their effective
    /// opacity is owned by `tab_panel_strength`, whose wash math tops up from
    /// the plain content alpha these cells composite at. The strip's label
    /// offsets cannot stand in for this flag: they are `0.0` for single-row
    /// bands, which would silently un-mark the strip.
    pub(super) chrome: bool,
}

/// Per-frame request for the analytic cursor aura. Geometry is rebuilt from the
/// same snapshot, origin, style, and [`CursorRenderParams`] as the cursor, so
/// Full and CursorOnly paths cannot diverge. The clip is the terminal content
/// rectangle in physical window pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CursorGlowRequest {
    pub(super) clip_rect: [f32; 4],
    /// User-facing normalized aura strength (`cursor_glow_intensity`, 0.0..=1.0).
    /// Resolved from settings at the single overlay-request choke point so both
    /// GPU update paths scale the peak alpha identically. Folded into the
    /// overlay cache signature so a live change cannot retain a stale aura.
    pub(super) intensity: f32,
}

/// Per-frame large-jump cursor-follower request. The rectangle is expressed in
/// undecorated content pixels; the shared instance builder adds the pane origin
/// and any single-pane chrome offset used by the real cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CursorStreakRequest {
    pub(super) destination: crate::core::Position,
    pub(super) rect: [f32; 4],
    pub(super) alpha: f32,
    pub(super) clip_rect: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CursorStreakInstance {
    pub(super) quad_rect: [f32; 4],
    pub(super) source_rect: [f32; 4],
    pub(super) color: [f32; 4],
    pub(super) peak_alpha: f32,
    pub(super) clip_rect: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CursorStreakVertex {
    pub(super) pos: [f32; 2],
    pub(super) source_rect: [f32; 4],
    /// x = peak alpha. Remaining lanes are reserved for shape evolution.
    pub(super) follower: [f32; 4],
    pub(super) color: [f32; 4],
    pub(super) clip_rect: [f32; 4],
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

pub(super) fn build_cursor_streak_instance(
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

pub(super) fn append_cursor_streak_vertices(
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
pub(super) struct CursorGlowInstance {
    pub(super) quad_rect: [f32; 4],
    pub(super) source_rect: [f32; 4],
    pub(super) radius: f32,
    pub(super) corner_radius: f32,
    pub(super) color: [f32; 4],
    pub(super) peak_alpha: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct CursorGlowVertex {
    pub(super) pos: [f32; 2],
    pub(super) source_rect: [f32; 4],
    /// x = falloff radius, y = source corner radius, z = peak alpha.
    pub(super) aura: [f32; 4],
    pub(super) color: [f32; 4],
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
pub(super) fn build_cursor_glow_instance(
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

pub(super) fn append_cursor_glow_vertices(
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

pub(super) fn retained_cursor_effects(
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
pub(super) fn cursor_glow_falloff(outside: f32, radius: f32) -> f32 {
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
pub(super) struct OverlayTop<'a> {
    /// The overlay panel snapshot (its own grid, fully opaque within its rect).
    pub(super) snapshot: &'a Snapshot,
    /// Panel top-left in physical px (window space).
    pub(super) origin: [f32; 2],
    /// Background treatment params (matches the panes so any global treatment
    /// is consistent across the frame).
    pub(super) treatment: grid::BackgroundTreatmentParams,
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
pub(super) struct RailOverlay<'a> {
    /// The `rail_cols × rows` strip snapshot (rail glyphs + baked panel tint).
    pub(super) snapshot: &'a Snapshot,
    /// Strip top-left in physical px (window space).
    pub(super) origin: [f32; 2],
    /// Background treatment params (matches the frame).
    pub(super) treatment: grid::BackgroundTreatmentParams,
    /// Descender-safe slot-centering offset shared with pinned rail strips.
    pub(super) rail_glyph_dy_rows: f32,
    /// Active and reorder indicators in window pixel geometry.
    pub(super) widget_quads: &'a [SolidQuad],
    /// Panel-colored outer padding and sub-cell remainder strips.
    pub(super) base_gaps: &'a [SolidQuad],
    /// Occluding wash quad drawn under the strip, or `None`.
    pub(super) wash: Option<SolidQuad>,
    /// Content-facing seam quad drawn over the strip, or `None`.
    pub(super) seam: Option<SolidQuad>,
}

#[derive(Clone, Copy)]
pub(super) struct PanelFrameQuads<'a> {
    pub(super) base_gaps: &'a [SolidQuad],
    pub(super) overlays: &'a [SolidQuad],
}

pub(super) fn quads_excluding(quads: &[SolidQuad], exclusions: &[SolidQuad]) -> Vec<SolidQuad> {
    let mut current = quads.to_vec();
    for exclusion in exclusions {
        let mut next = Vec::new();
        for quad in current {
            next.extend(
                super::app::tab_panel::rect_without(quad.rect, exclusion.rect)
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
fn pane_chrome_pin(pane: &PaneRender) -> grid::ChromePin {
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

pub(super) fn rail_overlay_chrome_pin(columns: usize, rail_glyph_dy_rows: f32) -> grid::ChromePin {
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

pub(super) fn theme_clear_color(theme: &Theme) -> wgpu::Color {
    let (r, g, b) = theme.clear;
    wgpu::Color {
        r: text::srgb_to_linear(r) as f64,
        g: text::srgb_to_linear(g) as f64,
        b: text::srgb_to_linear(b) as f64,
        a: 1.0,
    }
}

/// TRANSPARENCY: choose the swapchain composite-alpha mode. A transparent
/// window needs the compositor to blend the surface alpha, so prefer a
/// transparency-capable mode — `PreMultiplied` first (the framebuffer this
/// renderer produces over a zero clear is premultiplied), then
/// `PostMultiplied`, then `Opaque` (transparency silently unavailable). Falls
/// back to the first advertised mode if the caps list is exotic; the list is
/// never empty in practice (`Opaque` is universally offered).
pub(super) fn select_alpha_mode(modes: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
    use wgpu::CompositeAlphaMode::{Opaque, PostMultiplied, PreMultiplied};
    for preferred in [PreMultiplied, PostMultiplied, Opaque] {
        if modes.contains(&preferred) {
            return preferred;
        }
    }
    modes.first().copied().unwrap_or(Opaque)
}

/// TRANSPARENCY: the opacity fed to the CONTENT cell-vertex builder. The
/// wallpaper softening and window transparency COMPOSE — the content surface
/// alpha is their product, `cell_bg_opacity * window_bg_alpha`, rather than one
/// regime replacing the other at the 100% boundary.
///
/// At the opaque default (`window_bg_alpha == 1.0`) this is exactly
/// `cell_bg_opacity`, so the opaque path is byte-identical. Below 1.0 the cells
/// stay at the same softening fraction OF the window alpha, so the wallpaper
/// layer (a separate pass carrying its own `window_bg_alpha` uniform) keeps
/// showing through the cells instead of being occluded by a near-opaque cell
/// quad — the earlier hard branch snapped the cell alpha up to `window_bg_alpha`
/// (~0.99 just below 100%), which hid the wallpaper entirely one step off the
/// boundary. The product is continuous as `window_bg_alpha -> 1.0` (the limit
/// from below, `cell_bg_opacity`, equals the value at 1.0) and monotonic in the
/// window alpha, so window opacity now scales one continuous stack — wallpaper
/// treatment fading over the desktop-through — instead of toggling between two
/// background models.
pub(super) fn content_build_opacity(window_bg_alpha: f32, cell_bg_opacity: f32) -> f32 {
    cell_bg_opacity * window_bg_alpha
}

/// COLORED-BG-FLOOR: the opacity fed to the cell-vertex builder for content
/// cells whose RESOLVED background differs from the theme default (powerline
/// segments, button chips, app-painted blocks). The knob floors the WINDOW
/// factor of the [`content_build_opacity`] product — not the product itself —
/// so the wallpaper-softening factor (`cell_bg_opacity`) is preserved and every
/// identity holds by construction:
///
/// * opaque window: `max(1.0, floor) == 1.0` → exactly `cell_bg_opacity`,
///   byte-identical at EVERY knob value (including with a background image's
///   `cell_bg_opacity < 1.0`, where a naive `max(floor, product)` would lift);
/// * knob `0.0`: `max(alpha, 0.0) == alpha` → exactly the content product,
///   byte-identical at every window opacity;
/// * monotonic in the knob, and always `>= content_build_opacity` — the floor
///   strengthens colored cells, never weakens them.
pub(super) fn colored_content_build_opacity(
    window_bg_alpha: f32,
    cell_bg_opacity: f32,
    colored_bg_opacity: f32,
) -> f32 {
    cell_bg_opacity * window_bg_alpha.max(colored_bg_opacity)
}

/// TRANSPARENCY: the color the scene pass clears to. Fully transparent
/// (premultiplied zero) while the window is translucent, so the desktop shows
/// through and the whole treatment-over-desktop stack — wallpaper layer then
/// cell background quads, each premultiplied `(rgb·a, a)` — composites over it
/// correctly; otherwise the opaque theme clear (byte-identical off path). The
/// clear stays transparent-when-translucent by design: window transparency is
/// meant to reveal the DESKTOP, and the wallpaper's own faded contribution now
/// rides the continuous cell-alpha product (see `content_build_opacity`), so the
/// clear does not need to carry the theme color while translucent.
pub(super) fn scene_clear_color(window_bg_alpha: f32, clear_color: wgpu::Color) -> wgpu::Color {
    if window_bg_alpha < 1.0 {
        wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }
    } else {
        clear_color
    }
}

/// Pack a [`VisualEffect`] into the shader uniform's `effect` slot:
/// `[scanline_strength, scanline_period_px]`. When off, strength is `0.0`, which
/// makes the shader's scanline term vanish (pixel-identical to no effect).
pub(super) fn effect_params(visual: VisualEffect) -> [f32; 2] {
    [visual.scanline_strength(), visual.scanline_period_px()]
}

/// Pack the glyph coverage correction into the shader uniform. A gamma of
/// `1.0` makes `pow(coverage, 1.0 / gamma)` exactly the old linear coverage.
pub(super) fn text_params(text_gamma: f32) -> [f32; 4] {
    [text_gamma, 0.0, 0.0, 0.0]
}

pub(super) fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> (wgpu::TextureFormat, bool) {
    let format = formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
        .unwrap_or(formats[0]);
    (format, format.is_srgb())
}

/// Choose device limits that preserve the WebGPU defaults when the adapter can
/// satisfy them and fall back to the GLES-compatible floor when it cannot.
/// The surface-sized texture limit always follows the selected adapter.
pub(super) fn required_limits_for_adapter(adapter_limits: &wgpu::Limits) -> (wgpu::Limits, bool) {
    let preferred = wgpu::Limits {
        max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
        ..wgpu::Limits::default()
    };
    if preferred.check_limits(adapter_limits) {
        return (preferred, false);
    }

    (
        wgpu::Limits {
            max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
            ..wgpu::Limits::downlevel_defaults()
        },
        true,
    )
}

/// Whether an adapter is a software rasterizer rather than an accelerated GPU.
/// `Cpu` is unambiguous, while the name checks cover software implementations
/// that report another device class.
pub(super) fn adapter_is_software(info: &wgpu::AdapterInfo) -> bool {
    if info.device_type == wgpu::DeviceType::Cpu {
        return true;
    }
    let name = info.name.to_ascii_lowercase();
    const SOFTWARE_MARKERS: [&str; 4] = [
        "llvmpipe",
        "lavapipe",
        "swiftshader",
        // Windows WARP software rasterizer.
        "microsoft basic render driver",
    ];
    SOFTWARE_MARKERS.iter().any(|marker| name.contains(marker))
}

/// Select an accelerated, presentable adapter from enumeration order. Device
/// class is the primary key and enumeration order breaks ties, preserving a
/// deterministic choice without changing the normal `request_adapter` path.
pub(super) fn rescue_adapter_index(candidates: &[(wgpu::AdapterInfo, bool)]) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(index, (info, surface_supported))| {
            if !surface_supported || adapter_is_software(info) {
                return None;
            }
            let tier = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::Other => 2,
                wgpu::DeviceType::VirtualGpu => 3,
                wgpu::DeviceType::Cpu => return None,
            };
            Some((tier, index))
        })
        .min()
        .map(|(_, index)| index)
}

/// Viewport uniform mirroring `Viewport` in `cell.wgsl`: physical surface size
/// in pixels plus presentation-only params. `effect` is `[0.0, _]` when the
/// visual treatment is off, which makes the shader a no-op. `text.x` is glyph
/// coverage gamma; `1.0` preserves the legacy linear blend exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct ViewportUniform {
    pub(in crate::native) size: [f32; 2],
    pub(in crate::native) effect: [f32; 2],
    pub(in crate::native) text: [f32; 4],
}

fn create_atlas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &GlyphAtlas,
) -> wgpu::Texture {
    let format = match atlas.subpixel_mode() {
        SubpixelMode::Off => wgpu::TextureFormat::R8Unorm,
        SubpixelMode::Rgb | SubpixelMode::Bgr => wgpu::TextureFormat::Rgba8Unorm,
    };
    let extent = super::texture_limits::extent_2d(device, atlas.width, atlas.height);
    debug_assert_eq!((extent.width, extent.height), (atlas.width, atlas.height));
    let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-atlas"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &atlas_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(atlas.bytes_per_row()),
            rows_per_image: Some(atlas.height),
        },
        extent,
    );
    atlas_texture
}

fn create_color_atlas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &ColorGlyphAtlas,
) -> wgpu::Texture {
    let extent = super::texture_limits::extent_2d(device, atlas.width, atlas.height);
    debug_assert_eq!((extent.width, extent.height), (atlas.width, atlas.height));
    let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("odytty-color-glyph-atlas"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &atlas_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas.data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(atlas.width * 4),
            rows_per_image: Some(atlas.height),
        },
        extent,
    );
    atlas_texture
}

pub(super) fn effective_subpixel_mode(
    requested: SubpixelMode,
    features: wgpu::Features,
) -> SubpixelMode {
    if requested.enabled() && features.contains(wgpu::Features::DUAL_SOURCE_BLENDING) {
        requested
    } else {
        SubpixelMode::Off
    }
}

pub(super) fn blend_state_for_subpixel(mode: SubpixelMode) -> wgpu::BlendState {
    if mode.enabled() {
        wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Src1,
                dst_factor: wgpu::BlendFactor::OneMinusSrc1,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        }
    } else {
        wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        }
    }
}

pub(super) fn blend_state_for_color_glyphs() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        },
    }
}

pub(super) fn scene_target_format(
    surface_format: wgpu::TextureFormat,
    post_process_format: Option<wgpu::TextureFormat>,
    post_process: PostProcessOptions,
) -> wgpu::TextureFormat {
    if post_process.active() {
        post_process_format.unwrap_or(surface_format)
    } else {
        surface_format
    }
}

pub(super) fn create_atlas_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    viewport_buf: &wgpu::Buffer,
    atlas_texture: &wgpu::Texture,
    atlas_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-cell-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(atlas_sampler),
            },
        ],
    })
}

pub(super) fn create_color_atlas_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    viewport_buf: &wgpu::Buffer,
    atlas_texture: &wgpu::Texture,
    atlas_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("odytty-color-glyph-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: viewport_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(atlas_sampler),
            },
        ],
    })
}

/// Apply the synthetic-styles kill switch to a font set's natural synthesis
/// mask. When synthesis is `enabled`, returns [`StyleFonts::synthetic_mask`]
/// unchanged (each style true only when it has no real face). When disabled,
/// every bit is forced off so [`GlyphAtlas::set_synthetic_styles`] performs no
/// emboldening or shear and styled cells fall back to plain regular glyphs.
fn masked_synthetic(fonts: &StyleFonts, enabled: bool) -> (bool, bool, bool) {
    if enabled {
        fonts.synthetic_mask()
    } else {
        (false, false, false)
    }
}

pub(super) fn ensure_snapshot_glyphs(
    atlas: &mut GlyphAtlas,
    fonts: &StyleFonts,
    snapshot: &Snapshot,
) {
    ensure_snapshot_glyphs_excluding_color_runs(atlas, fonts, snapshot, &[]);
}

pub(super) fn ensure_snapshot_glyphs_excluding_color_runs(
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

fn vertex_bytes_len(vertices: &[Vertex]) -> u64 {
    std::mem::size_of_val(vertices) as u64
}

fn background_vertex_count(snapshot: &Snapshot) -> u32 {
    let cells = snapshot
        .cells
        .iter()
        .filter(|cell| !cell.wide_continuation)
        .count();
    (cells * grid::VERTS_PER_QUAD) as u32
}

/// Append the overlays and live cursor parameters shared by Full and
/// CursorOnly rebuilds. Keeping this layer in one builder prevents the two GPU
/// paths from diverging when cursor animation parameters change.
pub(super) fn append_cursor_layer_vertices(
    out: &mut Vec<Vertex>,
    snapshot: &Snapshot,
    atlas: &GlyphAtlas,
    cursor_style: CursorStyle,
    origin: [f32; 2],
    overlays: &[SolidQuad],
    params: CursorRenderParams,
) {
    out.reserve(overlays.len() * grid::VERTS_PER_QUAD);
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
pub(super) fn wallpaper_edge_wash_quads(
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
pub(super) fn wallpaper_edge_wash_quads_with_pin(
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
pub(super) fn multi_pane_wallpaper_edge_wash_quads(
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

pub(super) fn grow_vertex_buffer_capacity(current: u64, needed: u64) -> u64 {
    if needed <= current {
        return current;
    }

    let minimum = std::mem::size_of::<Vertex>() as u64;
    needed.max(minimum).next_power_of_two()
}

/// Fold the window scale factor into the logical font pixel size to get the
/// physical size the glyph atlas is rasterized at.
///
/// The scale is clamped to `>= 1.0`: a sub-1.0 factor (rare fractional
/// downscale output) would rasterize glyphs *below* their logical size and
/// harm legibility, so the atlas is never built under 1x. The surface still
/// maps to the display's real pixels via [`GpuState::resize`]; only the glyph
/// rasterization density is floored. This is the documented HiDPI clamp (H1):
/// keep-and-document was chosen over honoring sub-1.0 scales. The result is
/// also floored at 1.0 px so a degenerate font size never yields a zero atlas.
pub(super) fn physical_font_px(font_size_px: f32, scale: f32) -> f32 {
    (font_size_px * scale.max(1.0)).max(1.0)
}

fn create_vertex_buffer(device: &wgpu::Device, capacity_bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-cell-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<Vertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_color_glyph_vertex_buffer(device: &wgpu::Device, capacity_bytes: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-color-glyph-vertices"),
        size: capacity_bytes.max(std::mem::size_of::<ColorGlyphVertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_cursor_glow_vertex_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-cursor-glow-vertices"),
        size: (grid::VERTS_PER_QUAD * std::mem::size_of::<CursorGlowVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_cursor_streak_vertex_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("odytty-cursor-streak-vertices"),
        size: (grid::VERTS_PER_QUAD * std::mem::size_of::<CursorStreakVertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

pub(super) fn create_cell_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
    subpixel: SubpixelMode,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-cell-shader"),
        source: wgpu::ShaderSource::Wgsl(
            if subpixel.enabled() {
                include_str!("../shaders/cell_subpixel.wgsl")
            } else {
                include_str!("../shaders/cell.wgsl")
            }
            .into(),
        ),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-cell-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2, // pos_px
        1 => Float32x2, // uv
        2 => Float32x4, // color
        3 => Float32,   // is_glyph
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-cell-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // No culling: quad winding is not normalized in the geometry.
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Grayscale uses straight alpha. Subpixel uses dual-source
                // blending so RGB coverage can modulate each color channel
                // independently while the destination contributes
                // `1.0 - coverage`.
                blend: Some(blend_state_for_subpixel(subpixel)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub(super) fn create_cursor_glow_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-cursor-glow-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/cursor_glow.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-cursor-glow-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2, // expanded quad position
        1 => Float32x4, // source cursor rectangle
        2 => Float32x4, // falloff radius, corner radius, peak alpha
        3 => Float32x4, // resolved linear cursor color
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-cursor-glow-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<CursorGlowVertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend_state_for_subpixel(SubpixelMode::Off)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub(super) fn create_cursor_streak_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-cursor-streak-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/cursor_streak.wgsl").into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-cursor-streak-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2,
        1 => Float32x4,
        2 => Float32x4,
        3 => Float32x4,
        4 => Float32x4,
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-cursor-streak-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<CursorStreakVertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend_state_for_subpixel(SubpixelMode::Off)),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

pub(super) fn create_color_glyph_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("odytty-color-glyph-shader"),
        source: wgpu::ShaderSource::Wgsl(COLOR_GLYPH_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("odytty-color-glyph-pl"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    let vertex_attrs = wgpu::vertex_attr_array![
        0 => Float32x2, // pos_px
        1 => Float32x2, // uv
        2 => Float32,   // fade alpha (VE4 new-output fade; 1.0 off-path)
    ];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("odytty-color-glyph-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<ColorGlyphVertex>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &vertex_attrs,
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend_state_for_color_glyphs()),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Plain, owned snapshot of the active GPU adapter for the About panel's
/// renderer diagnostics. Captured once at init from `adapter.get_info()`; holds
/// no `wgpu` types so it can be cloned and read far from the render path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct AdapterDiagnostics {
    /// Adapter name, e.g. "NVIDIA GeForce RTX 4080".
    pub(in crate::native) name: String,
    /// Graphics backend, e.g. "Vulkan", "Metal", "GL".
    pub(in crate::native) backend: String,
    /// Device class, e.g. "DiscreteGpu", "IntegratedGpu", "Cpu".
    pub(in crate::native) device_type: String,
    /// Driver name, e.g. "NVIDIA" (may be empty on some backends).
    pub(in crate::native) driver: String,
    /// Driver detail/version string (may be empty on some backends).
    pub(in crate::native) driver_info: String,
}

impl AdapterDiagnostics {
    fn from_wgpu(info: &wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
        }
    }

    /// A one-line identity for the startup log: `<name> (<backend>, <device>)`.
    pub(in crate::native) fn summary(&self) -> String {
        format!("{} ({}, {})", self.name, self.backend, self.device_type)
    }
}

pub(super) struct GpuState {
    instance: wgpu::Instance,
    window: Arc<Window>,
    adapter: wgpu::Adapter,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    device_lost: Arc<AtomicBool>,
    queue: wgpu::Queue,
    /// Owned adapter info for the About panel's renderer diagnostics. Captured
    /// once at init; read-only thereafter. Not used by any render path.
    adapter_diagnostics: AdapterDiagnostics,
    enabled_features: wgpu::Features,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    cursor_glow_pipeline: wgpu::RenderPipeline,
    cursor_streak_pipeline: wgpu::RenderPipeline,
    color_glyph_pipeline: wgpu::RenderPipeline,
    scene_target_format: wgpu::TextureFormat,
    post_process_format: Option<wgpu::TextureFormat>,
    post_process: Option<PostProcessResources>,
    bloom: BloomOptions,
    crt: CrtOptions,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    color_glyph_bind_group_layout: wgpu::BindGroupLayout,
    color_glyph_bind_group: wgpu::BindGroup,
    viewport_buf: wgpu::Buffer,
    vertex_buf: wgpu::Buffer,
    vertex_buf_capacity_bytes: u64,
    cursor_glow_vertex_buf: wgpu::Buffer,
    cursor_streak_vertex_buf: wgpu::Buffer,
    color_glyph_vertex_buf: wgpu::Buffer,
    color_glyph_vertex_buf_capacity_bytes: u64,
    vertices: Vec<Vertex>,
    cursor_vertices: Vec<Vertex>,
    cursor_glow_vertices: Vec<CursorGlowVertex>,
    cursor_glow_vertex_count: u32,
    cursor_streak_vertices: Vec<CursorStreakVertex>,
    cursor_streak_vertex_count: u32,
    retained_cursor_overlays: Vec<SolidQuad>,
    retained_cursor_glow: Option<CursorGlowRequest>,
    retained_cursor_streak: Option<CursorStreakRequest>,
    color_glyph_vertices: Vec<ColorGlyphVertex>,
    color_glyph_runs: Vec<ColorGlyphRun>,
    vertex_count: u32,
    cell_vertex_count: u32,
    background_vertex_count: u32,
    color_glyph_vertex_count: u32,
    image_layer: ImageLayer,
    /// ID3/U5 background-image pass: a full-window textured quad drawn behind
    /// the grid with a readability scrim. `None` (the default and the off path)
    /// skips the draw entirely, so the rendered frame is byte-identical to the
    /// no-image path. Built/refreshed via [`Self::set_background_image`].
    bg_image: Option<BgImageGpu>,
    /// ID3/U5 cell background opacity multiplier fed to the cell-vertex builder.
    /// `1.0` (the default) keeps cells fully opaque — byte-identical output.
    cell_bg_opacity: f32,
    /// COLORED-BG-FLOOR: the `colored_bg_opacity` knob — minimum window-alpha
    /// contribution for content cells whose resolved background differs from
    /// the theme default. `0.0` disables the floor (inert); an opaque window is
    /// byte-identical at any value. See [`colored_content_build_opacity`].
    colored_bg_opacity: f32,
    /// Selection-highlight opacity fed to the cell-vertex builder for cells
    /// carrying the selection marker. Independent of `cell_bg_opacity` and
    /// `window_bg_alpha`, so the selection strength is tuned separately and does
    /// not wash out under window transparency. `1.0` (the default) keeps the
    /// selection fully opaque; frames with no selected cell are byte-identical
    /// regardless of this value.
    selection_opacity: f32,
    /// TEXT-BRIGHTNESS: glyph-foreground lift toward white fed to the
    /// cell-vertex builder. `1.0` (the default) is an exact identity —
    /// byte-identical vertex output. Applied uniformly to all mono-glyph ink
    /// (content and chrome labels alike); color emoji are exempt by pipeline.
    text_brightness: f32,
    /// TRANSPARENCY: effective window background alpha this frame. `1.0`
    /// (the default, and whenever the window-transparency setting is off or the
    /// compositor offers no alpha mode) keeps the opaque render path
    /// byte-identical. Below `1.0` the scene clears to fully transparent and the
    /// terminal background is drawn at this alpha so the desktop shows through;
    /// text/cursor/overlays stay opaque. An open overlay panel no longer forces
    /// this to `1.0` — the window stays translucent and only the panel's own
    /// cell span is held opaque (see `overlay_opaque_region`).
    window_bg_alpha: f32,
    /// TRANSPARENCY (MENU-OPACITY): while the window is translucent AND an
    /// overlay panel is merged into the single-pane snapshot, the panel's cell
    /// span (in the built snapshot's coordinates, after tab-chrome decoration).
    /// The cell-vertex builder forces these cells' backgrounds fully opaque so
    /// the panel stays a readable surface while the terminal cells around it keep
    /// the window opacity. `None` (the default, the opaque window path, and every
    /// multi-pane frame — where the overlay is a separate opaque layer) is the
    /// byte-identical path.
    overlay_opaque_region: Option<grid::CellRegion>,
    /// VE4 new-output fade: per-content-row FOREGROUND alpha multipliers plus
    /// the decorated-snapshot chrome offsets, set by the single-pane render
    /// dispatch each frame (`None` = inert, the off path and every settled
    /// frame). Consumed by the single-pane cell + color-glyph builds only; the
    /// multi-pane path passes `RowFade::NONE` (parity with the prior overlay
    /// mechanism, which was single-pane only).
    row_fade: Option<RowFadeSpec>,
    /// The glyph atlas, kept so vertices can be rebuilt from new snapshots as
    /// live PTY output arrives.
    pub(super) atlas: GlyphAtlas,
    color_glyph_atlas: ColorGlyphAtlas,
    emoji_rasterizer: EmojiRasterizer,
    /// Bounded row-plan cache for ASCII contextual shaping.
    ligature_shaper: LigatureShaper,
    /// Fonts used to populate the atlas dynamic region for regular and styled
    /// glyphs. Missing style faces intentionally fall back to the regular font.
    fonts: StyleFonts,
    // The three fields below back the live HiDPI rescale seam. `ScaleFactorChanged`
    // updates `scale`, derives a new physical atlas size from `font_size_px`, and
    // keeps `physical_px` idempotent across repeated events.
    /// Logical (unscaled) font size in pixels. Retained so a scale-factor change
    /// can re-derive the physical rasterization size; a future live
    /// `ODYTTY_FONT_SIZE` reload would update this then call [`Self::set_font_px`].
    font_size_px: f32,
    /// Current window scale factor, clamped to `>= 1.0` (see [`physical_font_px`]).
    /// Retained so a repeated `ScaleFactorChanged` carrying an unchanged value is
    /// a cheap no-op instead of a needless atlas rebuild.
    scale: f32,
    /// Physical pixel size the atlas is currently rasterized at
    /// (`physical_font_px(font_size_px, scale)`). Tracked so [`Self::set_font_px`]
    /// is idempotent on an unchanged size.
    physical_px: f32,
    /// Logical window padding from settings plus its current physical-pixel
    /// realization at [`Self::scale`].
    window_padding_px: f32,
    window_padding: WindowPadding,
    /// Surface clear color from the active theme (linear RGBA).
    clear_color: wgpu::Color,
    /// Ambient-effect uniform params `[strength, period_px]` ([0,_] == off).
    /// Re-written into the viewport uniform on every resize/reconfigure.
    effect: [f32; 2],
    /// Glyph coverage gamma uniform. `1.0` is the exact legacy output path.
    text: [f32; 4],
    /// Effective coverage path after adapter capability checks.
    subpixel: SubpixelMode,
    /// Last-applied RV5 stem-darkening strength. This is baked into atlas
    /// coverage at raster time, so a live setting change rebuilds the atlas.
    stem_darken: f32,
    /// Last-applied line-height multiplier (LINEHEIGHT). The leading is baked
    /// into the atlas cell geometry, so a live change rebuilds the atlas. `1.0`
    /// is the byte-identical historical cell.
    line_height: f32,
    /// Last-applied box-drawing thickness multiplier (BOXTHICK). The stroke
    /// weight is baked into geometric box-drawing slots at raster time, so a
    /// live change rebuilds the atlas. `1.0` reproduces the historical weights.
    box_thickness: f32,
    /// Last-applied value of the process-wide synthetic-styles kill switch
    /// ([`crate::settings::synthetic_styles_enabled`]). Retained so
    /// [`Self::apply_text_options`] can detect a live toggle and rebuild the
    /// atlas through the existing font-change seam; when `false`, the atlas
    /// synthetic mask is forced off so styled cells render as plain regular
    /// glyphs.
    synthetic_enabled: bool,
    /// Last-applied value of the process-wide geometric box-drawing switch.
    /// Retained so [`Self::apply_text_options`] can detect a live toggle and
    /// rebuild the atlas; geometry slots are atlas-owned, so flipping the setting
    /// must not wait for unrelated font changes.
    geometric_enabled: bool,
    /// Last-applied programming-ligature switch.
    ligatures_enabled: bool,
    /// Last-applied effective symbol / Nerd-font fallback switch. The setting is
    /// published process-wide and the legacy env var may override it; retaining
    /// the effective value lets live toggles rebuild the atlas.
    symbol_fallback_enabled: bool,
    /// Last-applied effective explicit fallback path, after env override
    /// precedence. A change requires re-resolving and rebuilding the atlas.
    symbol_font_path: Option<PathBuf>,
    /// Symbol / Nerd-font fallback **chain** for PUA prompt icons (RV6),
    /// resolved when the effective switch is enabled (order explicit > bundled
    /// v3,v2 > host); empty otherwise. The atlas walks it per glyph so coverage
    /// is the union of all faces. Reinstalled whenever the glyph atlas is rebuilt.
    symbol_fallback: Vec<Arc<FontVec>>,
    /// Last-applied SYMMAP override map (raw rules), retained for change
    /// detection: when the live map differs the atlas is rebuilt with freshly
    /// resolved override faces. Empty (the default) keeps the no-override path.
    symbol_map: crate::text::SymbolMap,
    /// SYMMAP override faces resolved from `symbol_map`'s family names
    /// (`(start, end, face)` ranges). Reinstalled whenever the atlas is rebuilt.
    symbol_map_fonts: Vec<(u32, u32, Arc<FontVec>)>,
    font_path: Option<PathBuf>,
    font_family: String,
    /// Last-applied RV7 font-weight variant suffix, retained for change
    /// detection. Empty (the default) keeps the regular-face load path, so the
    /// stored value stays `""` and never triggers a weight-driven rebuild.
    font_weight: String,
    /// RV4 smooth-scroll sub-row vertical offset (pixels) added to
    /// [`Self::content_origin`]. `0.0` at rest / on the off path keeps the
    /// origin byte-identical. Updated each animating frame via
    /// [`Self::set_scroll_frac_offset`].
    scroll_frac_offset: f32,
    /// SCROLL-CHROME-BOUNCE: the composited-chrome geometry (top-bar rows + rail
    /// column band) to pin against `scroll_frac_offset` this frame. `None` (no
    /// chrome, or the multi-pane path) leaves the pin inert / byte-identical.
    chrome_pin_geom: Option<ChromePinGeom>,
    /// FREEZE-HARDEN (b): monotonically increasing count of frames actually
    /// presented (`frame.present()` reached). Read by the freeze watchdog to
    /// distinguish "work pending, frames flowing" from "work pending, render
    /// path dead". Never reset.
    frames_presented: u64,
    // Kept alive for the lifetime of the bind group; never read directly.
    atlas_texture: wgpu::Texture,
    atlas_sampler: wgpu::Sampler,
    color_glyph_atlas_texture: wgpu::Texture,
    color_glyph_atlas_sampler: wgpu::Sampler,
}

impl GpuState {
    /// Read-only GPU adapter diagnostics for the About panel (name, backend,
    /// device type, driver). Captured once at init.
    pub(super) fn adapter_diagnostics(&self) -> &AdapterDiagnostics {
        &self.adapter_diagnostics
    }

    /// Bring up the GPU surface for `window`.
    ///
    /// Synchronous from the caller's perspective: the async adapter/device
    /// requests are driven to completion with `pollster`, since `winit`'s
    /// handler callbacks are synchronous.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        window: Arc<Window>,
        options: &NativeOptions,
        initial_snapshot: &Snapshot,
        theme: Theme,
        visual: VisualEffect,
        stem_darken: f32,
        bloom: BloomOptions,
        crt: CrtOptions,
        event_proxy: Option<winit::event_loop::EventLoopProxy<UserEvent>>,
        session: SessionToken,
    ) -> Result<Self, NativeError> {
        let effect = effect_params(visual);
        let text = text_params(options.text_gamma);
        let size = window.inner_size();
        let scale = (window.scale_factor() as f32).max(1.0);
        let physical_px = physical_font_px(options.font_size_px, scale);
        // GL/GLES requires the window's display handle to create a presentable
        // surface on both Wayland and X11. Vulkan, Metal, and DX12 ignore this
        // field, so their existing adapter and rendering paths are unchanged.
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())),
        );

        let surface = instance
            .create_surface(window.clone())
            .map_err(|err| NativeError::SurfaceCreation(err.to_string()))?;

        let mut adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|err| {
            NativeError::NoAdapter(format!(
                "{err}; install a Vulkan driver or accelerated GL stack; if WGPU_BACKEND is set, ensure it selects an installed backend; see the \"Slow rendering / software adapter\" section of docs/install.md"
            ))
        })?;

        let initial_adapter_info = adapter.get_info();
        let adapter_info = if adapter_is_software(&initial_adapter_info) {
            let mut adapters =
                pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
            let candidates = adapters
                .iter()
                .map(|candidate| {
                    (
                        candidate.get_info(),
                        candidate.is_surface_supported(&surface),
                    )
                })
                .collect::<Vec<_>>();
            if let Some(index) = rescue_adapter_index(&candidates) {
                let replacement_info = candidates[index].0.clone();
                tracing::warn!(
                    "odytty: replacing software GPU adapter {} ({:?}, {:?}) with accelerated adapter {} ({:?}, {:?})",
                    initial_adapter_info.name,
                    initial_adapter_info.backend,
                    initial_adapter_info.device_type,
                    replacement_info.name,
                    replacement_info.backend,
                    replacement_info.device_type
                );
                adapter = adapters.swap_remove(index);
                replacement_info
            } else {
                initial_adapter_info
            }
        } else {
            initial_adapter_info
        };

        // Capture adapter identity for the About panel before any device work.
        // Read-only diagnostics; does not influence rendering.
        let adapter_diagnostics = AdapterDiagnostics::from_wgpu(&adapter_info);
        // Name the selected adapter once at startup so a performance report can
        // be diagnosed from the log alone. Routed through `tracing` (not
        // stderr) so it lands in the rotated `odytty.log`: stderr may be
        // redirected to /dev/null (the scenario `logging.rs` was built for),
        // and on Windows the GUI subsystem has no visible stderr at all, so the
        // log is the only place the adapter identity survives. Emitted at WARN
        // because the log's default floor is WARN — an INFO line would be
        // dropped by default, leaving "diagnosed from the log alone" untrue.
        // The summary is device metadata only (backend/name/limits), never
        // terminal content. A software rasterizer — a silent
        // llvmpipe/lavapipe/SwiftShader or Windows WARP fallback — is the usual
        // cause of a "very slow even with effects off" report, so it earns a
        // louder warning pointing at the docs.
        tracing::warn!("odytty: GPU adapter: {}", adapter_diagnostics.summary());
        if adapter_is_software(&adapter_info) {
            tracing::warn!(
                "odytty: WARNING: rendering in software ({}); expect low performance. \
                 See the \"Slow rendering / software adapter\" section of docs/install.md",
                adapter_diagnostics.name
            );
        }

        let adapter_features = adapter.features();
        let enabled_features = adapter_features & wgpu::Features::DUAL_SOURCE_BLENDING;
        let subpixel = effective_subpixel_mode(options.subpixel, enabled_features);
        if options.subpixel.enabled() && !subpixel.enabled() {
            tracing::warn!(
                "odytty: ODYTTY_SUBPIXEL requested but the GPU adapter lacks dual-source blending; using grayscale text"
            );
        }
        let post_process_format = post::supported_format(&adapter);
        if post_process_format.is_none() {
            tracing::warn!(
                "odytty: GPU adapter lacks filterable Rgba16Float render targets; post-process effects disabled"
            );
        }

        let adapter_limits = adapter.limits();
        let (required_limits, uses_downlevel_limits) = required_limits_for_adapter(&adapter_limits);
        if uses_downlevel_limits {
            tracing::warn!(
                "odytty: GPU adapter is below WebGPU default limits; using downlevel-compatible limits"
            );
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("odytty-device"),
            required_features: enabled_features,
            required_limits,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::Off,
        }))
        .map_err(|err| NativeError::DeviceRequest(err.to_string()))?;
        let device_lost = Arc::new(AtomicBool::new(false));
        install_gpu_error_handlers(&device, Arc::clone(&device_lost), event_proxy, session);

        let caps = surface.get_capabilities(&adapter);
        let (format, surface_is_srgb) = choose_surface_format(&caps.formats);
        if !surface_is_srgb {
            tracing::warn!(
                "odytty: GPU surface offered no sRGB format; using {format:?}; text and colors may render darker than intended"
            );
        }

        let (surface_width, surface_height) = super::texture_limits::clamp_dimensions(
            size.width,
            size.height,
            device.limits().max_texture_dimension_2d,
        );
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: surface_width,
            height: surface_height,
            // Fifo (vsync) is universally supported and avoids tearing; the
            // present mode can become a setting once frames carry real content.
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: select_alpha_mode(&caps.alpha_modes),
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // --- Glyph atlas: rasterize at physical pixels for crisp HiDPI text.
        let fonts = StyleFonts::load(options)?;
        let window_padding = WindowPadding::from_logical(options.window_padding_px, scale);
        let origin = [window_padding.as_f32(), window_padding.as_f32()];
        atlas::set_stem_darken(stem_darken);
        let line_height = options.line_height;
        let box_thickness = options.box_thickness;
        crate::boxdraw::set_box_thickness(box_thickness);
        let mut atlas = GlyphAtlas::build_with_options(
            fonts.regular_font(),
            physical_px,
            subpixel,
            line_height,
        );
        atlas.set_texture_dimension_limit(device.limits().max_texture_dimension_2d);
        let synthetic_enabled = crate::settings::synthetic_styles_enabled();
        let (synth_bold, synth_italic, synth_bold_italic) =
            masked_synthetic(&fonts, synthetic_enabled);
        atlas.set_synthetic_styles(synth_bold, synth_italic, synth_bold_italic);
        let geometric_enabled = crate::settings::geometric_boxdraw_enabled();
        atlas.set_geometric_boxdraw(geometric_enabled);
        let symbol_fallback_enabled = effective_symbol_fallback_enabled();
        let symbol_font_path = effective_symbol_font_path();
        let symbol_fallback =
            resolve_symbol_fallback(symbol_fallback_enabled, symbol_font_path.as_deref());
        atlas.set_fallback_fonts(symbol_fallback.clone());
        install_runtime_symbol_resolver(&mut atlas, symbol_fallback_enabled);
        let symbol_map = crate::settings::symbol_map();
        let symbol_map_fonts = resolve_symbol_map_fonts(&symbol_map);
        atlas.set_symbol_map_fonts(symbol_map_fonts.clone());
        ensure_snapshot_glyphs(&mut atlas, &fonts, initial_snapshot);
        let mut atlas_texture = create_atlas_texture(&device, &queue, &atlas);
        // Nearest + clamp: glyph cells map 1:1 to pixels, so no filtering.
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // --- Viewport uniform (physical surface size), updated on resize.
        let viewport_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("odytty-viewport"),
            contents: bytemuck::bytes_of(&ViewportUniform {
                size: [config.width as f32, config.height as f32],
                effect,
                text,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // --- Bind group layout / group: uniform + atlas texture + sampler.
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("odytty-cell-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // VERTEX uses size for NDC mapping; FRAGMENT reads the
                    // effect params for the ambient scanline wash.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let mut bind_group = create_atlas_bind_group(
            &device,
            &bind_group_layout,
            &viewport_buf,
            &atlas_texture,
            &atlas_sampler,
        );
        let mut color_glyph_atlas = ColorGlyphAtlas::new(atlas.cell);
        color_glyph_atlas.set_texture_dimension_limit(device.limits().max_texture_dimension_2d);
        let mut emoji_rasterizer = EmojiRasterizer::discover();
        let initial_color_glyph_runs =
            emoji_rasterizer.build_color_glyph_runs(initial_snapshot, &mut color_glyph_atlas);
        let ligatures_enabled = crate::settings::ligatures_enabled();
        let mut ligature_shaper = LigatureShaper::new();
        let mut initial_ligature_runs = ligature_shaper.build_runs(
            ligatures_enabled,
            initial_snapshot,
            &fonts,
            &initial_color_glyph_runs,
        );
        for glyph in initial_ligature_runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
        {
            let _ = atlas.ensure_shaped(fonts.font_for(glyph.key.style), glyph.key);
        }
        initial_ligature_runs.retain(|run| {
            run.glyphs
                .iter()
                .all(|glyph| atlas.contains_shaped(glyph.key))
        });
        if atlas.take_dirty() {
            atlas_texture = create_atlas_texture(&device, &queue, &atlas);
            bind_group = create_atlas_bind_group(
                &device,
                &bind_group_layout,
                &viewport_buf,
                &atlas_texture,
                &atlas_sampler,
            );
        }
        let color_glyph_atlas_texture =
            create_color_atlas_texture(&device, &queue, &color_glyph_atlas);
        let color_glyph_atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("odytty-color-glyph-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let color_glyph_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("odytty-color-glyph-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
        let color_glyph_bind_group = create_color_atlas_bind_group(
            &device,
            &color_glyph_bind_group_layout,
            &viewport_buf,
            &color_glyph_atlas_texture,
            &color_glyph_atlas_sampler,
        );
        let _ = atlas.take_dirty();
        let scene_target_format = scene_target_format(
            config.format,
            post_process_format,
            PostProcessOptions { bloom, crt },
        );
        // The viewer overlay is composited after post directly onto the
        // swapchain, so the layer also needs the surface format for its overlay
        // + backing pipelines (distinct from the scene-target format above).
        let image_layer = ImageLayer::new(&device, scene_target_format, config.format);

        // --- Render pipeline from the shared cell shader.
        let pipeline =
            create_cell_pipeline(&device, scene_target_format, &bind_group_layout, subpixel);
        let cursor_glow_pipeline =
            create_cursor_glow_pipeline(&device, scene_target_format, &bind_group_layout);
        let cursor_streak_pipeline =
            create_cursor_streak_pipeline(&device, scene_target_format, &bind_group_layout);
        let color_glyph_pipeline = create_color_glyph_pipeline(
            &device,
            scene_target_format,
            &color_glyph_bind_group_layout,
        );
        // Build the first vertex buffer from the initial (blank) snapshot. Live
        // PTY output replaces this content via `update_from_snapshot` as the
        // pump thread advances the shared terminal. A >=1x1 grid always emits at
        // least one background quad, so this buffer is never zero-sized.
        let mut vertices = Vec::new();
        grid::build_cell_vertices_with_focus_dim_origin_and_ligatures_into(
            &mut vertices,
            initial_snapshot,
            &atlas,
            &initial_color_glyph_runs,
            &initial_ligature_runs,
            0.0,
            origin,
            grid::BackgroundTreatmentParams::default(),
            // Initial buffer is the blank snapshot; cells stay fully opaque until
            // a live `set_cell_bg_opacity` arrives (identity / byte-identical).
            crate::settings::DEFAULT_CELL_BG_OPACITY,
            // TEXT-BRIGHTNESS identity for the blank initial snapshot; live
            // values arrive with the first real frame's `set_text_brightness`.
            crate::settings::DEFAULT_TEXT_BRIGHTNESS,
            // No overlay panel is merged into the initial snapshot.
            None,
            grid::ChromePin::NONE,
        );
        let cell_vertex_count = vertices.len() as u32;
        grid::append_cursor_vertices_with_origin(
            &mut vertices,
            initial_snapshot,
            &atlas,
            CursorStyle::Block,
            origin,
            CursorRenderParams::default(),
        );
        let vertex_count = vertices.len() as u32;
        let background_vertex_count = background_vertex_count(initial_snapshot);
        let vertex_buf_capacity_bytes = grow_vertex_buffer_capacity(0, vertex_bytes_len(&vertices));
        let vertex_buf = create_vertex_buffer(&device, vertex_buf_capacity_bytes);
        let cursor_glow_vertex_buf = create_cursor_glow_vertex_buffer(&device);
        let cursor_streak_vertex_buf = create_cursor_streak_vertex_buffer(&device);
        if vertex_count > 0 {
            queue.write_buffer(&vertex_buf, 0, bytemuck::cast_slice(&vertices));
        }
        let mut color_glyph_vertices = Vec::new();
        grid::build_color_glyph_vertices_with_origin_into(
            &mut color_glyph_vertices,
            initial_snapshot,
            &color_glyph_atlas,
            &initial_color_glyph_runs,
            origin,
            grid::ChromePin::NONE,
            grid::RowFade::NONE,
        );
        let color_glyph_vertex_count = color_glyph_vertices.len() as u32;
        let initial_color_glyph_bytes =
            std::mem::size_of_val(color_glyph_vertices.as_slice()) as u64;
        let color_glyph_vertex_buf_capacity_bytes = initial_color_glyph_bytes
            .next_power_of_two()
            .max(std::mem::size_of::<ColorGlyphVertex>() as u64);
        let color_glyph_vertex_buf =
            create_color_glyph_vertex_buffer(&device, color_glyph_vertex_buf_capacity_bytes);
        if color_glyph_vertex_count > 0 {
            queue.write_buffer(
                &color_glyph_vertex_buf,
                0,
                bytemuck::cast_slice(&color_glyph_vertices),
            );
        }

        Ok(Self {
            instance,
            window,
            adapter,
            surface,
            device,
            device_lost,
            queue,
            adapter_diagnostics,
            enabled_features,
            config,
            pipeline,
            cursor_glow_pipeline,
            cursor_streak_pipeline,
            color_glyph_pipeline,
            scene_target_format,
            post_process_format,
            post_process: None,
            bloom,
            crt,
            bind_group_layout,
            bind_group,
            color_glyph_bind_group_layout,
            color_glyph_bind_group,
            viewport_buf,
            vertex_buf,
            vertex_buf_capacity_bytes,
            cursor_glow_vertex_buf,
            cursor_streak_vertex_buf,
            color_glyph_vertex_buf,
            color_glyph_vertex_buf_capacity_bytes,
            vertices,
            cursor_vertices: Vec::new(),
            cursor_glow_vertices: Vec::with_capacity(grid::VERTS_PER_QUAD),
            cursor_glow_vertex_count: 0,
            cursor_streak_vertices: Vec::with_capacity(grid::VERTS_PER_QUAD),
            cursor_streak_vertex_count: 0,
            retained_cursor_overlays: Vec::new(),
            retained_cursor_glow: None,
            retained_cursor_streak: None,
            color_glyph_vertices,
            color_glyph_runs: initial_color_glyph_runs,
            vertex_count,
            cell_vertex_count,
            background_vertex_count,
            color_glyph_vertex_count,
            image_layer,
            // ID3/U5: no image until the App pushes settings via
            // `set_background_image`; cells start fully opaque (identity).
            bg_image: None,
            cell_bg_opacity: crate::settings::DEFAULT_CELL_BG_OPACITY,
            colored_bg_opacity: crate::settings::DEFAULT_COLORED_BG_OPACITY,
            selection_opacity: crate::settings::DEFAULT_SELECTION_OPACITY,
            text_brightness: crate::settings::DEFAULT_TEXT_BRIGHTNESS,
            window_bg_alpha: 1.0,
            overlay_opaque_region: None,
            row_fade: None,
            atlas,
            color_glyph_atlas,
            emoji_rasterizer,
            ligature_shaper,
            fonts,
            font_size_px: options.font_size_px,
            scale,
            physical_px,
            window_padding_px: options.window_padding_px,
            window_padding,
            clear_color: theme_clear_color(&theme),
            effect,
            text,
            subpixel,
            stem_darken,
            line_height,
            box_thickness,
            synthetic_enabled,
            geometric_enabled,
            ligatures_enabled,
            symbol_fallback_enabled,
            symbol_font_path,
            symbol_fallback,
            symbol_map,
            symbol_map_fonts,
            font_path: options.font_path.clone(),
            font_family: options.font_family.clone(),
            font_weight: options.font_weight.clone(),
            scroll_frac_offset: 0.0,
            chrome_pin_geom: None,
            frames_presented: 0,
            atlas_texture,
            atlas_sampler,
            color_glyph_atlas_texture,
            color_glyph_atlas_sampler,
        })
    }

    fn refresh_atlas_texture(&mut self) {
        self.atlas_texture = create_atlas_texture(&self.device, &self.queue, &self.atlas);
        self.bind_group = create_atlas_bind_group(
            &self.device,
            &self.bind_group_layout,
            &self.viewport_buf,
            &self.atlas_texture,
            &self.atlas_sampler,
        );
    }

    fn refresh_color_glyph_atlas_texture(&mut self) {
        self.color_glyph_atlas_texture =
            create_color_atlas_texture(&self.device, &self.queue, &self.color_glyph_atlas);
        self.color_glyph_bind_group = create_color_atlas_bind_group(
            &self.device,
            &self.color_glyph_bind_group_layout,
            &self.viewport_buf,
            &self.color_glyph_atlas_texture,
            &self.color_glyph_atlas_sampler,
        );
    }

    fn rebuild_atlas(&mut self) {
        // BOXTHICK weight is read from the process-global atomic at raster time;
        // re-publish it here so a scale-driven rebuild keeps the active multiplier.
        crate::boxdraw::set_box_thickness(self.box_thickness);
        let mut atlas = GlyphAtlas::build_with_options(
            self.fonts.regular_font(),
            self.physical_px,
            self.subpixel,
            self.line_height,
        );
        atlas.set_texture_dimension_limit(self.device.limits().max_texture_dimension_2d);
        let (synth_bold, synth_italic, synth_bold_italic) =
            masked_synthetic(&self.fonts, self.synthetic_enabled);
        atlas.set_synthetic_styles(synth_bold, synth_italic, synth_bold_italic);
        atlas.set_geometric_boxdraw(self.geometric_enabled);
        atlas.set_fallback_fonts(self.symbol_fallback.clone());
        install_runtime_symbol_resolver(&mut atlas, self.symbol_fallback_enabled);
        atlas.set_symbol_map_fonts(self.symbol_map_fonts.clone());
        let _ = atlas.take_dirty();
        self.atlas = atlas;
        self.ligature_shaper.clear();
        self.refresh_atlas_texture();
        self.color_glyph_atlas = ColorGlyphAtlas::new(self.atlas.cell);
        self.color_glyph_atlas
            .set_texture_dimension_limit(self.device.limits().max_texture_dimension_2d);
        self.refresh_color_glyph_atlas_texture();
    }

    /// The current per-cell pixel metrics. These change when the atlas is
    /// rebuilt at a new scale, so callers that derive grid dimensions from the
    /// cell size must re-read this after [`Self::set_scale`] reports a rebuild.
    pub(super) fn cell(&self) -> crate::atlas::CellSize {
        self.atlas.cell
    }

    /// FREEZE-HARDEN (b): frames that reached `present()` since GPU init.
    pub(super) fn frames_presented(&self) -> u64 {
        self.frames_presented
    }

    /// The clamped scale factor the atlas is currently rasterized for.
    pub(super) fn scale(&self) -> f32 {
        self.scale
    }

    pub(super) fn window_padding(&self) -> WindowPadding {
        self.window_padding
    }

    /// Physical surface size in pixels `(width, height)` — the basis the
    /// multi-pane render dispatch lays pane rects out within.
    pub(super) fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// The active theme background clear color in linear RGB with alpha `1.0`,
    /// matching the [`SolidQuad`] color basis (VE4 new-output fade paints a
    /// quad of this color over each fading row). Sourced from the same
    /// `clear_color` used for the frame clear, so an opaque fade quad is
    /// pixel-seamless against the surrounding background.
    pub(super) fn clear_color_linear(&self) -> [f32; 4] {
        [
            self.clear_color.r as f32,
            self.clear_color.g as f32,
            self.clear_color.b as f32,
            1.0,
        ]
    }

    /// TRANSPARENCY: set the effective window background alpha for upcoming
    /// frames. `1.0` restores the fully-opaque path; values below `1.0` (only
    /// meaningful when the compositor offers a transparent alpha mode) draw the
    /// terminal background translucent. The App recomputes this each frame from
    /// the settings, so the mutation is a cheap store — geometry is rebuilt from
    /// it on the next update.
    pub(super) fn set_window_bg_alpha(&mut self, alpha: f32) {
        self.window_bg_alpha = alpha.clamp(0.0, 1.0);
        // TRANSPARENCY: the wallpaper pass is a separate GPU layer, so the
        // window alpha must reach its uniform too — otherwise the image draws
        // fully opaque over the transparent clear and no desktop shows through.
        // `set_window_alpha` is a no-op when the value is unchanged, so the
        // opaque path issues no extra GPU write (command stream unchanged).
        if let Some(bg) = self.bg_image.as_mut() {
            bg.set_window_alpha(&self.queue, self.window_bg_alpha);
        }
    }

    /// TRANSPARENCY (MENU-OPACITY): set the overlay panel's opaque cell span for
    /// the upcoming single-pane frame, or `None` to force no cells. Only
    /// meaningful while the window is translucent and an overlay is merged into
    /// the snapshot; the App passes `None` on the opaque window path and on every
    /// multi-pane frame (there the overlay is a separate opaque layer), keeping
    /// those paths byte-identical. A cheap store — the builder reads it on the
    /// next update.
    pub(super) fn set_overlay_opaque_region(&mut self, region: Option<grid::CellRegion>) {
        self.overlay_opaque_region = region;
    }

    /// VE4 new-output fade: set the per-row foreground alpha ramp for the
    /// upcoming single-pane frame, or `None` when no row is mid-fade (the off
    /// path and every settled frame — the builders then take the exact inert
    /// `RowFade::NONE` path). A cheap store — read on the next update.
    pub(super) fn set_row_fade(&mut self, fade: Option<RowFadeSpec>) {
        self.row_fade = fade;
    }

    /// TRANSPARENCY: whether the configured swapchain can present a transparent
    /// window at all. `Opaque` composite-alpha means the display server offers
    /// no alpha blending, so the setting has no visible effect and the App
    /// keeps the opaque path.
    pub(super) fn transparency_capable(&self) -> bool {
        self.config.alpha_mode != wgpu::CompositeAlphaMode::Opaque
    }

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

    /// COLORED-BG-FLOOR: live-update the colored-background opacity floor
    /// (settings panel / config reload). Clamped to `[0,1]`; the next rebuild
    /// repaints colored blocks at the new floor.
    pub(super) fn set_colored_bg_opacity(&mut self, opacity: f32) {
        self.colored_bg_opacity = opacity.clamp(0.0, 1.0);
    }

    /// TEXT-BRIGHTNESS: live-update the glyph-foreground lift (settings panel /
    /// config reload). Clamped to `[1.0, 1.5]`; the next rebuild repaints ink
    /// at the new lift. `1.0` is the exact-identity plain path.
    pub(super) fn set_text_brightness(&mut self, brightness: f32) {
        self.text_brightness = brightness.clamp(1.0, 1.5);
    }

    /// SELECTION-OPACITY: the independent alpha applied to selected cells'
    /// background quads, unaffected by window transparency or `cell_bg_opacity`.
    fn selection_build_opacity(&self) -> f32 {
        self.selection_opacity
    }

    /// SELECTION-OPACITY: live-update the selection highlight opacity (settings
    /// panel / config reload). Clamped to `[0,1]`; a change re-keys the frame so
    /// an on-screen selection repaints at the new strength.
    pub(super) fn set_selection_opacity(&mut self, opacity: f32) {
        self.selection_opacity = opacity.clamp(0.0, 1.0);
    }

    /// TRANSPARENCY: the color the scene pass clears to. Fully transparent
    /// (premultiplied zero) while the window is translucent, so padding/gaps
    /// show the desktop and cell background quads blend to premultiplied
    /// `(rgb·a, a)` over it. Otherwise the opaque theme clear (unchanged).
    fn scene_clear_color(&self) -> wgpu::Color {
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

    /// RV4: set the smooth-scroll sub-row vertical offset (pixels) applied to
    /// [`Self::content_origin`]. `0.0` (the default / settled / off-path value)
    /// leaves the origin byte-identical to before this feature existed.
    pub(super) fn set_scroll_frac_offset(&mut self, offset_px: f32) {
        self.scroll_frac_offset = offset_px;
    }

    /// SCROLL-CHROME-BOUNCE: the composited-chrome geometry to pin against the
    /// sub-row scroll offset this frame (top-bar rows + rail column band). `None`
    /// (no chrome, or the multi-pane path) leaves the pin inert.
    pub(super) fn set_chrome_pin_geom(&mut self, geom: Option<ChromePinGeom>) {
        self.chrome_pin_geom = geom;
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

    fn refresh_window_padding(&mut self) -> bool {
        let next = WindowPadding::from_logical(self.window_padding_px, self.scale);
        if next == self.window_padding {
            return false;
        }
        self.window_padding = next;
        true
    }

    pub(super) fn set_window_padding_px(&mut self, logical_px: f32) -> bool {
        let logical_px = logical_px.max(0.0);
        let logical_changed = (self.window_padding_px - logical_px).abs() >= f32::EPSILON;
        self.window_padding_px = logical_px;
        self.refresh_window_padding() || logical_changed
    }

    /// Update the window scale factor, re-rasterizing the glyph atlas at the new
    /// physical pixel size only when the clamped value actually changes.
    ///
    /// Idempotent on a repeated scale: `winit` emits `ScaleFactorChanged` for
    /// some unrelated transitions, so an unchanged value returns `false`
    /// immediately without touching the GPU. Sub-1.0 factors are clamped (see
    /// [`physical_font_px`]). Returns `true` when a rebuild occurred so the
    /// caller can republish [`Self::cell`] (cell metrics scale with density) and
    /// rebuild its grid geometry.
    pub(super) fn set_scale(&mut self, scale: f32) -> bool {
        let clamped = scale.max(1.0);
        if (clamped - self.scale).abs() < f32::EPSILON {
            return false;
        }
        self.scale = clamped;
        let font_changed = self.set_font_px(physical_font_px(self.font_size_px, clamped));
        let padding_changed = self.refresh_window_padding();
        font_changed || padding_changed
    }

    /// Re-rasterize the glyph atlas at a new physical pixel size and recreate the
    /// atlas texture + bind group, republishing [`Self::cell`].
    ///
    /// Scale-agnostic by design — the caller folds window scale into `px` (via
    /// [`physical_font_px`]). Built deliberately reusable for a future live
    /// `ODYTTY_FONT_SIZE` reload, which would update `font_size_px` and call this
    /// directly. Idempotent on an unchanged size (returns `false`).
    ///
    /// Invalidation is by construction: a fresh [`GlyphAtlas::build`] has an
    /// empty dynamic region, so no old-density slot can survive into the new
    /// atlas (R1 invalidation requirement). Live non-ASCII glyphs repopulate at
    /// the new size on the next [`Self::update_from_snapshot`] via
    /// `ensure_snapshot_glyphs`. Returns `true` when a rebuild occurred.
    pub(super) fn set_font_px(&mut self, px: f32) -> bool {
        let px = px.max(1.0);
        if (px - self.physical_px).abs() < f32::EPSILON {
            return false;
        }
        self.physical_px = px;
        self.rebuild_atlas();
        true
    }

    pub(super) fn apply_text_options(
        &mut self,
        options: &NativeOptions,
        stem_darken: f32,
    ) -> Result<bool, NativeError> {
        let next_subpixel = effective_subpixel_mode(options.subpixel, self.enabled_features);
        if options.subpixel.enabled() && !next_subpixel.enabled() {
            tracing::warn!(
                "odytty: ODYTTY_SUBPIXEL requested but the GPU adapter lacks dual-source blending; using grayscale text"
            );
        }

        let font_changed = self.font_path != options.font_path
            || self.font_family != options.font_family
            || self.font_weight != options.font_weight;
        let subpixel_changed = self.subpixel != next_subpixel;
        let font_size_changed = (self.font_size_px - options.font_size_px).abs() >= f32::EPSILON;
        let stem_darken_changed = (self.stem_darken - stem_darken).abs() >= f32::EPSILON;
        let line_height_changed = (self.line_height - options.line_height).abs() >= f32::EPSILON;
        let box_thickness_changed =
            (self.box_thickness - options.box_thickness).abs() >= f32::EPSILON;
        // The synthetic-styles kill switch is published process-wide (it cannot
        // ride `NativeOptions`, whose construction literals live in fenced
        // files). A live toggle reuses this font-change rebuild seam: the synth
        // mask is baked into atlas slots, so a redraw alone cannot un-bake it —
        // the atlas must rebuild.
        let synthetic_now = crate::settings::synthetic_styles_enabled();
        let synthetic_changed = synthetic_now != self.synthetic_enabled;
        let geometric_now = crate::settings::geometric_boxdraw_enabled();
        let geometric_changed = geometric_now != self.geometric_enabled;
        let ligatures_now = crate::settings::ligatures_enabled();
        let ligatures_changed = ligatures_now != self.ligatures_enabled;
        let symbol_fallback_now = effective_symbol_fallback_enabled();
        let symbol_font_path_now = effective_symbol_font_path();
        let symbol_fallback_changed = symbol_fallback_now != self.symbol_fallback_enabled
            || symbol_font_path_now != self.symbol_font_path;
        // SYMMAP: the override map is published process-wide; a live change
        // requires re-resolving the override faces and rebuilding the atlas
        // (override glyphs are baked into atlas slots, like the fallback face).
        let symbol_map_now = crate::settings::symbol_map();
        let symbol_map_changed = symbol_map_now != self.symbol_map;
        if !font_changed
            && !subpixel_changed
            && !font_size_changed
            && !stem_darken_changed
            && !line_height_changed
            && !box_thickness_changed
            && !synthetic_changed
            && !geometric_changed
            && !ligatures_changed
            && !symbol_fallback_changed
            && !symbol_map_changed
        {
            return Ok(false);
        }

        let next_fonts = if font_changed {
            Some(StyleFonts::load_from(
                options.font_path.as_deref(),
                &options.font_family,
                &options.font_weight,
            )?)
        } else {
            None
        };

        if let Some(fonts) = next_fonts {
            self.fonts = fonts;
            self.font_path = options.font_path.clone();
            self.font_family = options.font_family.clone();
            self.font_weight = options.font_weight.clone();
        }
        if subpixel_changed {
            self.subpixel = next_subpixel;
            self.pipeline = create_cell_pipeline(
                &self.device,
                self.scene_target_format,
                &self.bind_group_layout,
                self.subpixel,
            );
        }
        if font_size_changed {
            self.font_size_px = options.font_size_px;
            self.physical_px = physical_font_px(self.font_size_px, self.scale);
        }
        if stem_darken_changed {
            self.stem_darken = stem_darken;
        }
        if line_height_changed {
            self.line_height = options.line_height;
        }
        if box_thickness_changed {
            self.box_thickness = options.box_thickness;
        }
        if synthetic_changed {
            self.synthetic_enabled = synthetic_now;
        }
        if geometric_changed {
            self.geometric_enabled = geometric_now;
        }
        if ligatures_changed {
            self.ligatures_enabled = ligatures_now;
        }
        if symbol_fallback_changed {
            self.symbol_fallback_enabled = symbol_fallback_now;
            self.symbol_font_path = symbol_font_path_now;
            self.symbol_fallback = resolve_symbol_fallback(
                self.symbol_fallback_enabled,
                self.symbol_font_path.as_deref(),
            );
        }
        if symbol_map_changed {
            self.symbol_map_fonts = resolve_symbol_map_fonts(&symbol_map_now);
            self.symbol_map = symbol_map_now;
        }
        atlas::set_stem_darken(stem_darken);
        self.rebuild_atlas();
        Ok(true)
    }

    pub(super) fn set_theme(&mut self, theme: Theme) {
        self.clear_color = theme_clear_color(&theme);
        // T10: a theme change moves `l_bg` and may flip the scrim polarity, so
        // recompute the background-image scrim against the new theme (reusing
        // the stored explicit override). No re-decode — a cheap uniform write.
        if let Some(bg) = self.bg_image.as_mut() {
            bg.refresh_for_theme(&self.queue, &theme, self.cell_bg_opacity);
        }
    }

    /// ID3/U5: apply the background-image settings. The `treatment_is_image`
    /// gate AND a configured `path` are both required for an image to exist;
    /// otherwise the pass is cleared (off-path identity). A path/blur change
    /// re-decodes; a theme/opacity/scrim-only change just refreshes the scrim
    /// uniform (T6). `cell_bg_opacity` is stored for the cell-vertex builder.
    pub(super) fn set_background_image(
        &mut self,
        treatment_is_image: bool,
        path: Option<&Path>,
        blur_radius: u32,
        scrim_override: Option<f32>,
        cell_bg_opacity: f32,
        theme: Theme,
    ) {
        self.cell_bg_opacity = cell_bg_opacity;
        let wanted = if treatment_is_image { path } else { None };
        let Some(path) = wanted else {
            self.bg_image = None;
            return;
        };
        let clamped_blur = blur_radius.min(crate::settings::MAX_BACKGROUND_BLUR_RADIUS);
        let needs_reload = match self.bg_image.as_ref() {
            Some(bg) => {
                let (cur_path, cur_blur) = bg.source();
                cur_path != path || cur_blur != clamped_blur
            }
            None => true,
        };
        if needs_reload {
            self.bg_image = BgImageGpu::load(
                &self.device,
                &self.queue,
                self.scene_target_format,
                path,
                blur_radius,
                scrim_override,
                &theme,
                cell_bg_opacity,
            );
        } else if let Some(bg) = self.bg_image.as_mut() {
            bg.refresh_scrim(&self.queue, &theme, cell_bg_opacity, scrim_override);
        }
        // TRANSPARENCY: a freshly-loaded image starts opaque, so re-seed the
        // live window alpha (a no-op when already opaque) — the wallpaper must
        // pick up an in-effect translucent window on load, not only on the next
        // opacity change.
        if let Some(bg) = self.bg_image.as_mut() {
            bg.set_window_alpha(&self.queue, self.window_bg_alpha);
        }
    }

    /// Retired (UX5): the legacy ambient scanline path is folded into the
    /// unified CRT post-process, so the cell shader no longer reads a scanline
    /// term from the `effect` uniform. `visual=ambient` now aliases to `crt=on`
    /// in settings and the scanline look is produced by the CRT post-process.
    /// This shell remains so the settings-apply path keeps a stable call site;
    /// the `effect` uniform stays at its (now vestigial but valid) init value,
    /// so the uniform layout is unchanged and nothing is left in an invalid
    /// state. It intentionally does nothing.
    pub(super) fn set_visual(&mut self, _visual: VisualEffect) {}

    pub(super) fn set_text_gamma(&mut self, text_gamma: f32) {
        self.text = text_params(text_gamma);
        self.update_viewport();
    }

    pub(super) fn set_bloom(&mut self, bloom: BloomOptions) {
        let old_target = self.scene_target_format;
        self.bloom = bloom;
        let new_target = scene_target_format(
            self.config.format,
            self.post_process_format,
            self.post_options(),
        );
        if new_target != old_target {
            self.rebuild_scene_pipelines(new_target);
        }
    }

    pub(super) fn set_crt(&mut self, crt: CrtOptions) {
        let old_target = self.scene_target_format;
        self.crt = crt;
        let new_target = scene_target_format(
            self.config.format,
            self.post_process_format,
            self.post_options(),
        );
        if new_target != old_target {
            self.rebuild_scene_pipelines(new_target);
        }
    }

    fn rebuild_scene_pipelines(&mut self, target_format: wgpu::TextureFormat) {
        self.pipeline = create_cell_pipeline(
            &self.device,
            target_format,
            &self.bind_group_layout,
            self.subpixel,
        );
        self.cursor_glow_pipeline =
            create_cursor_glow_pipeline(&self.device, target_format, &self.bind_group_layout);
        self.cursor_streak_pipeline =
            create_cursor_streak_pipeline(&self.device, target_format, &self.bind_group_layout);
        self.color_glyph_pipeline = create_color_glyph_pipeline(
            &self.device,
            target_format,
            &self.color_glyph_bind_group_layout,
        );
        self.image_layer
            .rebuild_pipeline(&self.device, target_format);
        // C1: the background-image pipeline also draws inside the scene pass,
        // so it must retarget with the rest — `set_background_image` skips the
        // rebuild when path/blur are unchanged (needs_reload == false), which
        // is exactly the live CRT/bloom-toggle path.
        if let Some(bg) = self.bg_image.as_mut() {
            bg.rebuild_pipeline(&self.device, target_format);
        }
        self.scene_target_format = target_format;
    }

    fn ensure_scene_target_format(&mut self) {
        let target = scene_target_format(
            self.config.format,
            self.post_process_format,
            self.post_options(),
        );
        if target != self.scene_target_format {
            self.rebuild_scene_pipelines(target);
        }
    }

    /// Rebuild the cell vertex buffer from a fresh terminal snapshot.
    ///
    /// Called on the UI thread after the pump thread signals new PTY output.
    /// The grid is small (e.g. 80×24 → a few thousand vertices), so recreating
    /// the buffer per coalesced update is cheap and avoids tracking capacity.
    /// The caller must already hold the snapshot by value — the terminal mutex
    /// is dropped before this runs so the lock is never held across GPU calls.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_from_snapshot(
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
    pub(super) fn update_from_panes(
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
            self.vertices.extend_from_slice(&pane_buf[..bg]);
            glyph_segment.extend_from_slice(&pane_buf[bg..]);

            let color_start = self.color_glyph_vertices.len();
            grid::build_color_glyph_vertices_with_origin_into(
                &mut self.color_glyph_vertices,
                pane.snapshot,
                &self.color_glyph_atlas,
                runs,
                pane.origin,
                // TAB-LABEL-CENTERING: a chrome strip's emoji label centers with
                // the same offset as its mono glyphs; `ChromePin::NONE` for panes.
                pane_chrome_pin(pane),
                // VE4 new-output fade: single-pane only (parity with the prior
                // overlay mechanism); split panes never fade.
                grid::RowFade::NONE,
            );
            // Colour glyphs (emoji) obey the same per-pane clip so a gliding
            // emoji's partial row is cropped, not smeared across the divider.
            grid::clip_quads_vertical(&mut self.color_glyph_vertices[color_start..], pane.clip);

            let tail_start = tail.len();
            tail.reserve(pane.overlays.len() * grid::VERTS_PER_QUAD);
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
                .reserve(edge_quads.len() * grid::VERTS_PER_QUAD);
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
                .reserve(panel.overlays.len() * grid::VERTS_PER_QUAD);
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
            .reserve(dividers.len() * grid::VERTS_PER_QUAD + tail.len());
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
                self.cell_bg_opacity,
                // TEXT-BRIGHTNESS: overlay panel text lifts with the rest of
                // the window's ink (`1.0` = identity).
                self.text_brightness,
                // The overlay-top snapshot IS the panel; it is already built at
                // the opaque `cell_bg_opacity` on its own layer.
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

    pub(super) fn cached_image_ids(&self) -> BTreeSet<StoredImageId> {
        self.image_layer.cached_ids()
    }

    pub(super) fn update_image_layer(
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
    pub(super) fn cached_pane_image_ids(&self) -> BTreeSet<(u64, StoredImageId)> {
        self.image_layer.cached_pane_ids()
    }

    /// MULTIPANE image update: render each visible pane's graphics into its own
    /// sub-rect, clipped by a per-pane scissor so nothing bleeds across a
    /// divider. See [`ImageLayer::update_panes`].
    pub(super) fn update_pane_image_layers(
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
    pub(super) fn set_overlay_image(&mut self, image: Option<(&[u8], u32, u32)>) {
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
    pub(super) fn overlay_image_fit_rect(&self) -> Option<[f32; 4]> {
        self.image_layer.overlay_image_fit_rect()
    }

    /// Rebuild the cell vertex buffer from a fresh terminal snapshot plus
    /// presentation-only solid overlays, drawing the cursor in `cursor_style`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_from_snapshot_with_overlays(
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
                let mut edge_vertices = Vec::with_capacity(edge_quads.len() * grid::VERTS_PER_QUAD);
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
                Vec::with_capacity(panel.base_gaps.len() * grid::VERTS_PER_QUAD);
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
                Vec::with_capacity(panel.overlays.len() * grid::VERTS_PER_QUAD);
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
        self.ligature_shaper
            .build_runs(self.ligatures_enabled, snapshot, &self.fonts, color_runs)
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

    pub(super) fn update_cursor_and_overlays(
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
    pub(super) fn update_cursor_with_retained_overlays(
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
    fn update_viewport(&self) {
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

    /// Reconfigure the surface for a new physical size. No-op for zero extents
    /// (e.g. a minimized window), which the swap chain rejects.
    pub(super) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let (width, height) = super::texture_limits::clamp_dimensions(
            width,
            height,
            self.device.limits().max_texture_dimension_2d,
        );
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        // Geometry is pixel-space and stable across resize; only the viewport
        // uniform needs the new physical size.
        self.update_viewport();
        if let Some(post_process) = &mut self.post_process
            && let Some(format) = self.post_process_format
        {
            post_process.resize(&self.device, &self.config, format);
        }
    }

    /// Reapply the current configuration for an outdated surface.
    pub(super) fn reconfigure(&self) {
        self.surface.configure(&self.device, &self.config);
    }

    /// Recreate a backend surface after `CurrentSurfaceTexture::Lost`.
    ///
    /// Vulkan, Metal, and DX12 can invalidate the platform surface independently
    /// of the logical window. Reconfiguring the invalid surface is insufficient;
    /// a fresh surface must be created from the retained instance and window.
    pub(super) fn recreate_surface(&mut self) -> Result<(), NativeError> {
        let surface = self
            .instance
            .create_surface(Arc::clone(&self.window))
            .map_err(|err| NativeError::SurfaceCreation(err.to_string()))?;
        let caps = surface.get_capabilities(&self.adapter);
        if !caps.formats.contains(&self.config.format) {
            return Err(NativeError::SurfaceCreation(format!(
                "recreated GPU surface no longer supports {:?}",
                self.config.format
            )));
        }
        if !caps.alpha_modes.contains(&self.config.alpha_mode) {
            self.config.alpha_mode = caps
                .alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Opaque);
        }
        surface.configure(&self.device, &self.config);
        self.surface = surface;
        Ok(())
    }

    fn post_active(&self) -> bool {
        self.post_options().active() && self.post_process_format.is_some()
    }

    fn post_options(&self) -> PostProcessOptions {
        PostProcessOptions {
            bloom: self.bloom,
            crt: self.crt,
        }
    }

    fn draw_scene<'pass>(&'pass self, pass: &mut wgpu::RenderPass<'pass>) {
        if self.vertex_count == 0 {
            return;
        }

        // ID3/U5: the background image is drawn FIRST (over the clear colour,
        // behind every cell quad), with its readability scrim baked in. The
        // translucent cell layer (at `cell_bg_opacity`) composites on top, so
        // the image shows through behind text. `None` (off path) is skipped.
        if let Some(bg) = self.bg_image.as_ref() {
            bg.draw(pass);
        }

        let background_count = self.background_vertex_count.min(self.vertex_count);
        let cell_count = self.cell_vertex_count.min(self.vertex_count);
        // Canonical Kitty render order: background cell quads -> negative-z
        // images -> analytic cursor aura -> coverage glyphs/decorations ->
        // color glyphs -> cursor/overlays -> non-negative-z images. Keeping the
        // aura below both glyph lanes preserves text pixels exactly.
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
        if background_count > 0 {
            pass.draw(0..background_count, 0..1);
        }
        self.image_layer.draw_below(pass);
        if self.cursor_glow_vertex_count > 0 {
            pass.set_pipeline(&self.cursor_glow_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.cursor_glow_vertex_buf.slice(..));
            pass.draw(0..self.cursor_glow_vertex_count, 0..1);
        }
        if self.cursor_streak_vertex_count > 0 {
            pass.set_pipeline(&self.cursor_streak_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.cursor_streak_vertex_buf.slice(..));
            pass.draw(0..self.cursor_streak_vertex_count, 0..1);
        }
        if background_count < cell_count {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.draw(background_count..cell_count, 0..1);
        }
        if self.color_glyph_vertex_count > 0 {
            pass.set_pipeline(&self.color_glyph_pipeline);
            pass.set_bind_group(0, &self.color_glyph_bind_group, &[]);
            pass.set_vertex_buffer(0, self.color_glyph_vertex_buf.slice(..));
            pass.draw(0..self.color_glyph_vertex_count, 0..1);
        }
        if cell_count < self.vertex_count {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
            pass.draw(cell_count..self.vertex_count, 0..1);
        }
        self.image_layer.draw_above(pass);
        // NOTE: the C4 viewer overlay is intentionally NOT drawn here. It is
        // composited in a dedicated pass on the swapchain AFTER the CRT/bloom
        // post pass (see `encode_overlay_pass` / `render`) so the photo is never
        // touched by effects. Drawing it inside the scene pass would route it
        // through the HDR offscreen and the post shaders.
    }

    /// Composite the C4 viewer overlay onto the swapchain AFTER post-processing.
    ///
    /// Opened with `LoadOp::Load` so the post-processed frame is preserved and
    /// the viewer (backing + image, both in surface format) draws crisply on
    /// top, untouched by CRT/bloom. The whole pass is gated on
    /// `has_overlay_image()`: with no viewer image set, no pass is encoded and
    /// the command buffer is byte-for-byte identical to the no-viewer path.
    fn encode_overlay_pass<'pass>(
        &'pass self,
        encoder: &'pass mut wgpu::CommandEncoder,
        view: &'pass wgpu::TextureView,
    ) {
        if !self.image_layer.has_overlay_image() {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("odytty-overlay-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Preserve the post-processed frame, then draw the viewer.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.image_layer.draw_overlay(&mut pass);
    }

    fn encode_scene_pass<'pass>(
        &'pass self,
        encoder: &'pass mut wgpu::CommandEncoder,
        view: &'pass wgpu::TextureView,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("odytty-cell-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Keep the neutral clear, then draw cell quads over it.
                    // TRANSPARENCY: a fully-transparent clear when the window is
                    // translucent (opaque theme clear otherwise — byte-identical).
                    load: wgpu::LoadOp::Clear(self.scene_clear_color()),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw_scene(&mut pass);
    }

    /// Clear the surface to the active theme's clear color and present one frame.
    ///
    /// Returns a [`FrameOutcome`] so the event loop can decide whether to
    /// reconfigure the surface or simply skip the frame. `wgpu` 29 reports
    /// acquisition status through [`wgpu::CurrentSurfaceTexture`] rather than a
    /// `Result`, so there is no fatal out-of-memory path here.
    pub(super) fn render(&mut self) -> FrameOutcome {
        if self.device_lost.swap(false, Ordering::AcqRel) {
            return FrameOutcome::RecreateDevice;
        }
        self.ensure_scene_target_format();
        let (frame, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            // Acquired, but the surface no longer matches: draw this frame, then
            // reconfigure for the next one.
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            // An outdated surface or validation error can reuse the surface.
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Validation => {
                return FrameOutcome::Reconfigure;
            }
            wgpu::CurrentSurfaceTexture::Lost => return FrameOutcome::RecreateSurface,
            // Transient: drop this frame and try again later. The two arms are
            // reported separately so the event loop's escalation policy can
            // tell a chronic acquire timeout (candidate for a bounded surface
            // recreate) from a legitimately occluded window (never recreated).
            wgpu::CurrentSurfaceTexture::Timeout => {
                return FrameOutcome::Skipped { occluded: false };
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return FrameOutcome::Skipped { occluded: true };
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("odytty-clear-encoder"),
            });
        if self.post_active() {
            if let Some(format) = self.post_process_format {
                if self.post_process.is_none() {
                    self.post_process = Some(PostProcessResources::new(
                        &self.device,
                        &self.config,
                        format,
                    ));
                }
                let post_process = self.post_process.as_ref().expect("post process resources");
                self.encode_scene_pass(&mut encoder, &post_process.offscreen_view);
                post_process.encode_post_process(
                    &mut encoder,
                    &self.queue,
                    &view,
                    self.post_options(),
                );
                // Viewer draws over the post-processed frame (effects-free).
                self.encode_overlay_pass(&mut encoder, &view);
            } else {
                self.encode_scene_pass(&mut encoder, &view);
                self.encode_overlay_pass(&mut encoder, &view);
            }
        } else {
            self.encode_scene_pass(&mut encoder, &view);
            self.encode_overlay_pass(&mut encoder, &view);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        // FREEZE-HARDEN (b): count only frames that actually reached
        // present(); skipped/failed acquires above never get here.
        self.frames_presented = self.frames_presented.wrapping_add(1);

        if suboptimal {
            FrameOutcome::Reconfigure
        } else {
            FrameOutcome::Presented
        }
    }
}

const COLOR_GLYPH_SHADER: &str = r#"
struct Viewport {
    size: vec2<f32>,
    effect: vec2<f32>,
    text: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> viewport: Viewport;
@group(0) @binding(1)
var color_glyph_tex: texture_2d<f32>;
@group(0) @binding(2)
var color_glyph_sampler: sampler;

struct VsIn {
    @location(0) pos_px: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) alpha: f32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

@vertex
fn vs_main(input: VsIn) -> VsOut {
    var out: VsOut;
    let ndc = vec2<f32>(
        (input.pos_px.x / viewport.size.x) * 2.0 - 1.0,
        1.0 - (input.pos_px.y / viewport.size.y) * 2.0,
    );
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = input.uv;
    out.alpha = input.alpha;
    return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    // VE4 new-output fade: the texel is premultiplied RGBA, so one uniform
    // multiply on all four channels fades the glyph without fringing.
    // alpha = 1.0 (everywhere off the fade path) is the exact identity.
    return textureSample(color_glyph_tex, color_glyph_sampler, input.uv) * input.alpha;
}
"#;

/// What the event loop should do after a frame attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FrameOutcome {
    /// A frame was presented successfully.
    Presented,
    /// The surface needs reconfiguring before the next frame.
    Reconfigure,
    /// The platform surface was invalidated and must be recreated.
    RecreateSurface,
    /// The device-lost callback signalled the event-loop thread.
    RecreateDevice,
    /// The frame was intentionally skipped (transient surface state).
    /// `occluded` distinguishes an occluded surface (platform reports the
    /// window as not visible; retrying is all that is ever appropriate) from an
    /// acquire timeout (which, when chronic, escalates to a surface recreate).
    Skipped { occluded: bool },
}

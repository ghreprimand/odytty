// SPDX-License-Identifier: GPL-3.0-only
use std::time::Duration;

use crate::core::Dimensions;
use crate::grid::SolidQuad;
use crate::text::CellSize;

use winit::event::MouseScrollDelta;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WindowPadding {
    physical_px: u32,
}

impl WindowPadding {
    pub(crate) const ZERO: Self = Self { physical_px: 0 };

    pub(crate) fn from_logical(logical_px: f32, scale: f32) -> Self {
        let physical_px = (logical_px.max(0.0) * scale.max(1.0)).round() as u32;
        Self { physical_px }
    }

    pub(crate) fn physical_px(self) -> u32 {
        self.physical_px
    }

    pub(crate) fn as_f32(self) -> f32 {
        self.physical_px as f32
    }

    fn content_extent(self, extent: u32) -> u32 {
        extent.saturating_sub(self.physical_px.saturating_mul(2))
    }
}

#[cfg(test)]
pub(super) fn grid_dimensions_for(width_px: u32, height_px: u32, cell: CellSize) -> Dimensions {
    grid_dimensions_for_with_padding(width_px, height_px, cell, WindowPadding::ZERO)
}

pub(super) fn grid_dimensions_for_with_padding(
    width_px: u32,
    height_px: u32,
    cell: CellSize,
    padding: WindowPadding,
) -> Dimensions {
    let cols = padding.content_extent(width_px) / cell.width.max(1);
    let rows = padding.content_extent(height_px) / cell.height.max(1);
    Dimensions::new(cols as usize, rows as usize)
}

const WHEEL_STEP_LINES: usize = 3;
const SCROLL_INDICATOR_WIDTH_PX: f32 = 3.0;
const SCROLL_INDICATOR_MIN_HEIGHT_PX: f32 = 8.0;
/// Width (physical px) of the grab band for the draggable scroll thumb
/// (MOUSE-SCROLLBAR). Wider than the 3px visual thumb so the thumb is actually
/// grabbable with a pointer; the drawn thumb is unchanged. The band only ever
/// captures a press while scrolled back into history (`offset > 0`) and within
/// the thumb's vertical extent, so it never steals right-margin clicks at the
/// live tail.
const SCROLLBAR_HIT_WIDTH_PX: f32 = 14.0;
/// Minimum delay between drag-edge autoscroll steps.
pub(super) const SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(80);

/// Native-side scrollback viewport state.
///
/// Tracks how many rows the rendered viewport is paged upward from the live
/// bottom (`offset == 0` is the live screen). Every mutation clamps against the
/// supplied scrollback length, so the offset can never address rows that do not
/// exist. The core `snapshot_with_scrollback` clamps too; tracking the bound
/// here keeps the UX honest (no "dead" scrolling past the oldest row) and lets
/// the offset logic be unit-tested without a GPU/window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Viewport {
    offset: usize,
}

impl Viewport {
    pub(super) fn offset(self) -> usize {
        self.offset
    }

    /// Whether the viewport is at the live bottom (offset 0).
    #[cfg(test)]
    pub(super) fn is_live(self) -> bool {
        self.offset == 0
    }

    /// Page `lines` rows upward into history, clamped to `scrollback_len`.
    /// Returns whether the offset changed.
    pub(super) fn scroll_up(&mut self, lines: usize, scrollback_len: usize) -> bool {
        let next = self.offset.saturating_add(lines).min(scrollback_len);
        self.set(next)
    }

    /// Page `lines` rows downward toward the live bottom. Returns whether the
    /// offset changed.
    pub(super) fn scroll_down(&mut self, lines: usize) -> bool {
        let next = self.offset.saturating_sub(lines);
        self.set(next)
    }

    /// Snap back to the live bottom. Returns whether the offset changed.
    pub(super) fn reset_to_live(&mut self) -> bool {
        self.set(0)
    }

    /// Keep the scrolled-back view anchored to the same absolute rows when
    /// `added` new rows entered scrollback. This is the "stay scrolled" policy:
    /// fresh PTY output does not scroll the user's view out from under them.
    /// No-op at the live bottom (where new output should appear immediately).
    pub(super) fn anchor_after_growth(&mut self, added: usize, scrollback_len: usize) {
        if self.offset == 0 || added == 0 {
            return;
        }
        self.offset = self.offset.saturating_add(added).min(scrollback_len);
    }

    /// Re-clamp after the available history may have shrunk (resize reflow,
    /// alternate-screen entry clearing primary scrollback).
    pub(super) fn clamp(&mut self, scrollback_len: usize) {
        self.offset = self.offset.min(scrollback_len);
    }

    /// Jump directly to a computed scrollback offset. Used by search result
    /// navigation, which already works in absolute row coordinates.
    pub(super) fn jump_to(&mut self, offset: usize, scrollback_len: usize) -> bool {
        self.set(offset.min(scrollback_len))
    }

    fn set(&mut self, next: usize) -> bool {
        if next == self.offset {
            return false;
        }
        self.offset = next;
        true
    }
}

/// Convert a mouse-wheel delta into a signed row count at the default step:
/// positive scrolls up into history, negative scrolls toward the live bottom.
/// Line deltas map each notch to [`WHEEL_STEP_LINES`] rows; pixel deltas convert
/// by the cell height. Thin wrapper over [`wheel_lines_scaled`] at the default
/// step, used by the direction-only reporting path and the overlay free-scroll
/// (which deliberately ignore the user's local-scroll multiplier).
pub(super) fn wheel_lines(delta: MouseScrollDelta, cell_height: u32) -> isize {
    wheel_lines_scaled(delta, cell_height, WHEEL_STEP_LINES)
}

/// As [`wheel_lines`], but with a caller-supplied notch multiplier
/// (`step_lines`, floored at 1) so the local viewport scroll path can honor the
/// `scroll_wheel_lines` setting (MOUSE-WHEEL-SPEED). Only discrete wheel-notch
/// (`LineDelta`) input is multiplied; continuous (touchpad `PixelDelta`) input
/// is already row-accurate and is never scaled. `step_lines == WHEEL_STEP_LINES`
/// is byte-identical to the historical behavior.
pub(super) fn wheel_lines_scaled(
    delta: MouseScrollDelta,
    cell_height: u32,
    step_lines: usize,
) -> isize {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => {
            if y == 0.0 {
                return 0;
            }
            let step = step_lines.max(1) as isize;
            let notches = y.abs().ceil().max(1.0) as isize;
            y.signum() as isize * notches * step
        }
        MouseScrollDelta::PixelDelta(pos) => {
            let height = (cell_height.max(1)) as f64;
            (pos.y / height).round() as isize
        }
    }
}

/// Offset-independent geometry of the right-edge scroll thumb, plus the maps
/// between a scrollback offset and the thumb's track position.
///
/// This is the single source of truth shared by the render path
/// ([`scroll_indicator_quad_with_padding`]), the drag hit-test
/// ([`scroll_indicator_hit_with_padding`]), and the drag inverse
/// ([`scrollbar_offset_for_drag_with_padding`]) so the draggable thumb can never
/// diverge from the drawn thumb. The thumb height / travel / x-band depend only
/// on `scrollback_len`, the grid dimensions, the cell size, and padding — never
/// on the live `viewport_offset` — so an in-progress drag stays valid even as
/// the offset passes through 0 (the live tail) on the way back up.
struct ScrollbarGeometry {
    scrollback_len: usize,
    track_h: f32,
    thumb_h: f32,
    travel: f32,
    pad: f32,
    /// Right edge of the track (== `pad + track_w`); both the visual thumb and
    /// the grab band hug this edge.
    x1: f32,
    /// Visual thumb width (the drawn quad), preserved byte-identical.
    visual_w: f32,
    /// Grab-band width (>= `visual_w`) used only by the hit-test.
    hit_w: f32,
}

impl ScrollbarGeometry {
    /// Resolve the offset-independent thumb geometry, or `None` when there is no
    /// scrollback / the track has zero area. Does **not** consult
    /// `viewport_offset`: callers that must hide the thumb at the live tail gate
    /// on `viewport_offset == 0` themselves (the render + hit-test paths do; the
    /// drag-update path deliberately does not).
    fn resolve(
        scrollback_len: usize,
        dimensions: Dimensions,
        cell: CellSize,
        padding: WindowPadding,
    ) -> Option<Self> {
        if scrollback_len == 0 {
            return None;
        }
        let cols = dimensions.columns;
        let rows = dimensions.rows;
        if cols == 0 || rows == 0 {
            return None;
        }
        let track_w = cols as f32 * cell.width.max(1) as f32;
        let track_h = rows as f32 * cell.height.max(1) as f32;
        if track_w <= 0.0 || track_h <= 0.0 {
            return None;
        }
        let total_rows = scrollback_len.saturating_add(rows).max(1);
        let proportional_h = track_h * (rows as f32 / total_rows as f32);
        let thumb_h = proportional_h
            .max(SCROLL_INDICATOR_MIN_HEIGHT_PX.min(track_h))
            .min(track_h);
        let travel = (track_h - thumb_h).max(0.0);
        let visual_w = SCROLL_INDICATOR_WIDTH_PX.min(track_w);
        let hit_w = SCROLLBAR_HIT_WIDTH_PX.min(track_w).max(visual_w);
        let pad = padding.as_f32();
        let x1 = pad + track_w;
        Some(Self {
            scrollback_len,
            track_h,
            thumb_h,
            travel,
            pad,
            x1,
            visual_w,
            hit_w,
        })
    }

    /// Track-local top of the thumb (before padding) for a scrollback offset.
    /// Oldest history (`offset == scrollback_len`) sits at the top (`0.0`);
    /// the smallest visible offset sits near `travel`.
    fn thumb_top(&self, viewport_offset: usize) -> f32 {
        let offset = viewport_offset.min(self.scrollback_len);
        let position = (self.scrollback_len - offset) as f32 / self.scrollback_len as f32;
        self.travel * position
    }

    /// The drawn thumb quad for a scrollback offset (byte-identical to the
    /// historical inline computation).
    fn thumb_quad(&self, viewport_offset: usize, color: [f32; 4]) -> SolidQuad {
        let y0 = self.thumb_top(viewport_offset);
        let y1 = (y0 + self.thumb_h).min(self.track_h);
        let x0 = self.x1 - self.visual_w;
        SolidQuad {
            rect: [x0, self.pad + y0, self.x1, self.pad + y1],
            color,
        }
    }

    /// If the pointer (screen px) is over the thumb's grab band, return the
    /// grab offset within the thumb (`y - thumb_top_screen`) so the subsequent
    /// drag keeps the cursor anchored to the same point on the thumb rather than
    /// jumping the thumb to the cursor. `None` otherwise.
    fn grab_dy_at(&self, viewport_offset: usize, x: f32, y: f32) -> Option<f32> {
        let x0 = self.x1 - self.hit_w;
        if x < x0 || x > self.x1 {
            return None;
        }
        let y_top = self.pad + self.thumb_top(viewport_offset);
        let y_bot = y_top + self.thumb_h;
        if y < y_top || y > y_bot {
            return None;
        }
        Some(y - y_top)
    }

    /// Inverse of [`thumb_top`]: map a track-local thumb-top back to a scrollback
    /// offset. Rounds to the nearest row, so it round-trips `thumb_top` exactly.
    /// A degenerate zero-travel track (sub-thumb-height window) maps to the
    /// oldest offset.
    fn offset_at_thumb_top(&self, y0: f32) -> usize {
        if self.travel <= 0.0 {
            return self.scrollback_len;
        }
        let position = (y0 / self.travel).clamp(0.0, 1.0);
        let raw = (self.scrollback_len as f32 * (1.0 - position)).round();
        raw.max(0.0).min(self.scrollback_len as f32) as usize
    }

    /// Map a live cursor-y (screen px) plus the grab offset to a scrollback
    /// offset, clamping the thumb-top to `[0, travel]` so dragging past either
    /// end pins to the oldest row / the live tail.
    fn offset_for_drag(&self, cursor_y: f32, grab_dy: f32) -> usize {
        let y0 = (cursor_y - grab_dy - self.pad).clamp(0.0, self.travel);
        self.offset_at_thumb_top(y0)
    }
}

/// Build the right-edge scrollback indicator overlay for the current viewport.
///
/// `viewport_offset == 0` is the live tail, where the indicator is hidden. In
/// alternate screen the core exposes no active scrollback, so the same rule
/// hides the bar there without native needing a core-specific alt-screen query.
#[cfg(test)]
pub(super) fn scroll_indicator_quad(
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
    color: [f32; 4],
) -> Option<SolidQuad> {
    scroll_indicator_quad_with_padding(
        viewport_offset,
        scrollback_len,
        dimensions,
        cell,
        color,
        WindowPadding::ZERO,
    )
}

pub(super) fn scroll_indicator_quad_with_padding(
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
    color: [f32; 4],
    padding: WindowPadding,
) -> Option<SolidQuad> {
    if viewport_offset == 0 {
        return None;
    }
    ScrollbarGeometry::resolve(scrollback_len, dimensions, cell, padding)
        .map(|geometry| geometry.thumb_quad(viewport_offset, color))
}

/// Hit-test a pointer press (screen px) against the draggable scroll thumb
/// (MOUSE-SCROLLBAR). Returns the grab offset within the thumb when the press
/// lands on the visible thumb's grab band, or `None` otherwise.
///
/// Mirrors the render gate: the thumb is hidden (and therefore not grabbable) at
/// the live tail (`viewport_offset == 0`) and without scrollback, so a press
/// there is never captured — keeping the plain/fast press path byte-identical.
#[cfg(test)]
pub(super) fn scroll_indicator_hit(
    x: f32,
    y: f32,
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
) -> Option<f32> {
    scroll_indicator_hit_with_padding(
        x,
        y,
        viewport_offset,
        scrollback_len,
        dimensions,
        cell,
        WindowPadding::ZERO,
    )
}

pub(super) fn scroll_indicator_hit_with_padding(
    x: f32,
    y: f32,
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
    padding: WindowPadding,
) -> Option<f32> {
    if viewport_offset == 0 {
        return None;
    }
    ScrollbarGeometry::resolve(scrollback_len, dimensions, cell, padding)?.grab_dy_at(
        viewport_offset,
        x,
        y,
    )
}

/// Map an in-progress scroll-thumb drag to a target scrollback offset
/// (MOUSE-SCROLLBAR). `grab_dy` is the value returned by
/// [`scroll_indicator_hit_with_padding`] when the drag began. Returns `None`
/// only when there is no scrollback / zero-area track; unlike the render and
/// hit-test paths it does **not** gate on `viewport_offset`, so a drag stays
/// live as the offset passes through 0 on the way back up.
#[cfg(test)]
pub(super) fn scrollbar_offset_for_drag(
    cursor_y: f32,
    grab_dy: f32,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
) -> Option<usize> {
    scrollbar_offset_for_drag_with_padding(
        cursor_y,
        grab_dy,
        scrollback_len,
        dimensions,
        cell,
        WindowPadding::ZERO,
    )
}

pub(super) fn scrollbar_offset_for_drag_with_padding(
    cursor_y: f32,
    grab_dy: f32,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
    padding: WindowPadding,
) -> Option<usize> {
    ScrollbarGeometry::resolve(scrollback_len, dimensions, cell, padding)
        .map(|geometry| geometry.offset_for_drag(cursor_y, grab_dy))
}

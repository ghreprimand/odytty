// SPDX-License-Identifier: GPL-3.0-only
//! Render-facing quad clipping and pane-edge fill geometry.

use super::*;

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

/// A compact quad instance whose rectangle and UV bounds can be clamped.
pub(crate) trait ClipQuadInstance {
    fn rect(&self) -> [f32; 4];
    fn set_rect(&mut self, rect: [f32; 4]);
    fn uv_rect(&self) -> [f32; 4];
    fn set_uv_rect(&mut self, uv: [f32; 4]);
}

impl ClipQuadInstance for Vertex {
    fn rect(&self) -> [f32; 4] {
        [self.pos[0], self.pos[1], self.end_pos[0], self.end_pos[1]]
    }

    fn set_rect(&mut self, rect: [f32; 4]) {
        self.pos = [rect[0], rect[1]];
        self.end_pos = [rect[2], rect[3]];
    }

    fn uv_rect(&self) -> [f32; 4] {
        [self.uv[0], self.uv[1], self.end_uv[0], self.end_uv[1]]
    }

    fn set_uv_rect(&mut self, uv: [f32; 4]) {
        self.uv = [uv[0], uv[1]];
        self.end_uv = [uv[2], uv[3]];
    }
}

impl ClipQuadInstance for ColorGlyphVertex {
    fn rect(&self) -> [f32; 4] {
        [self.pos[0], self.pos[1], self.end_pos[0], self.end_pos[1]]
    }

    fn set_rect(&mut self, rect: [f32; 4]) {
        self.pos = [rect[0], rect[1]];
        self.end_pos = [rect[2], rect[3]];
    }

    fn uv_rect(&self) -> [f32; 4] {
        [self.uv[0], self.uv[1], self.end_uv[0], self.end_uv[1]]
    }

    fn set_uv_rect(&mut self, uv: [f32; 4]) {
        self.uv = [uv[0], uv[1]];
        self.end_uv = [uv[2], uv[3]];
    }
}

/// PANE-SUBCELL-CLIP: clamp every axis-aligned quad in `verts` to the vertical
/// band `clip`, cropping a partial row's overhang via a UV adjustment (never a
/// squash) exactly like [`push_glyph_quad_clipped_top`] does at the chrome seam.
/// A quad entirely outside the band collapses to zero height rather than being
/// removed, so the instance count and segment split stay valid.
///
/// Inert when `clip` is [`VClip::NONE`] (the fast-path return), so at-rest and
/// single-pane frames are byte-identical. One pass serves both compact streams.
pub(crate) fn clip_quads_vertical<V: ClipQuadInstance>(instances: &mut [V], clip: VClip) {
    if !clip.active() {
        return;
    }
    for instance in instances {
        let [x0, y0, x1, y1] = instance.rect();
        let height = y1 - y0;
        if height <= 0.0 {
            continue;
        }
        if y0 >= clip.bottom_y {
            instance.set_rect([x0, clip.bottom_y, x1, clip.bottom_y]);
            continue;
        }
        if y1 <= clip.top_y {
            instance.set_rect([x0, clip.top_y, x1, clip.top_y]);
            continue;
        }
        let [u0, v0, u1, v1] = instance.uv_rect();
        let clipped_y0 = y0.max(clip.top_y);
        let clipped_y1 = y1.min(clip.bottom_y);
        let clipped_v0 = v0 + ((clipped_y0 - y0) / height) * (v1 - v0);
        let clipped_v1 = v0 + ((clipped_y1 - y0) / height) * (v1 - v0);
        instance.set_rect([x0, clipped_y0, x1, clipped_y1]);
        instance.set_uv_rect([u0, clipped_v0, u1, clipped_v1]);
    }
}

/// Clamp every axis-aligned quad to a physical-pixel rectangle
/// `[left, top, right, bottom]`. Coverage and colour-glyph UVs advance with a
/// cropped edge, so ink is cut rather than rescaled; solid-cell vertices carry
/// inert UVs and therefore follow the same path safely. Quads wholly outside
/// collapse to zero area instead of being removed, preserving the fixed
/// background/glyph segment counts used by the GPU batching path.
///
/// The multi-pane renderer applies this only to padded content panes. Chrome,
/// single-pane rendering, and the padding-zero path never call it, preserving
/// their established vertex streams exactly.
pub(crate) fn clip_quads_to_rect<V: ClipQuadInstance>(instances: &mut [V], clip: [f32; 4]) {
    for instance in instances {
        let [x0, y0, x1, y1] = instance.rect();
        let width = x1 - x0;
        let height = y1 - y0;
        if width <= 0.0 || height <= 0.0 {
            continue;
        }

        if x0 >= clip[2] {
            instance.set_rect([clip[2], y0, clip[2], y1]);
            continue;
        }
        if x1 <= clip[0] {
            instance.set_rect([clip[0], y0, clip[0], y1]);
            continue;
        }
        if y0 >= clip[3] {
            instance.set_rect([x0, clip[3], x1, clip[3]]);
            continue;
        }
        if y1 <= clip[1] {
            instance.set_rect([x0, clip[1], x1, clip[1]]);
            continue;
        }

        let [u0, v0, u1, v1] = instance.uv_rect();
        let clipped_x0 = x0.max(clip[0]);
        let clipped_x1 = x1.min(clip[2]);
        let clipped_y0 = y0.max(clip[1]);
        let clipped_y1 = y1.min(clip[3]);
        let clipped_u0 = u0 + ((clipped_x0 - x0) / width) * (u1 - u0);
        let clipped_u1 = u0 + ((clipped_x1 - x0) / width) * (u1 - u0);
        let clipped_v0 = v0 + ((clipped_y0 - y0) / height) * (v1 - v0);
        let clipped_v1 = v0 + ((clipped_y1 - y0) / height) * (v1 - v0);
        instance.set_rect([clipped_x0, clipped_y0, clipped_x1, clipped_y1]);
        instance.set_uv_rect([clipped_u0, clipped_v0, clipped_u1, clipped_v1]);
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
/// `bg_instances` is the background segment only (glyphs excluded); `row0_quads` is
/// the count of non-continuation cells in the snapshot's first row (its
/// background quads lead the segment in row-major order). Only the top edge is
/// moved (indices 0/2/3), and only upward, so a quad already at or above `top_y`
/// is untouched and the at-rest / single-pane path (never called) is unaffected.
pub(crate) fn extend_first_row_bg_to_top(
    bg_instances: &mut [Vertex],
    row0_quads: usize,
    top_y: f32,
) {
    let end = row0_quads.min(bg_instances.len());
    for instance in &mut bg_instances[..end] {
        if instance.pos[1] > top_y {
            instance.pos[1] = top_y;
        }
    }
}

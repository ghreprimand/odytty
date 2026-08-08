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

/// A quad vertex whose vertical position and vertical UV can be clamped by
/// [`clip_quads_vertical`]. Abstracts over the mono [`Vertex`] (atlas coverage +
/// solid backgrounds) and the [`ColorGlyphVertex`] (emoji), so one clip routine
/// serves both pane vertex streams. The horizontal axis and color are never
/// touched — the clip is purely vertical.
pub(crate) trait ClipQuadVertex {
    fn clip_x(&self) -> f32;
    fn set_clip_x(&mut self, x: f32);
    fn clip_y(&self) -> f32;
    fn set_clip_y(&mut self, y: f32);
    fn clip_u(&self) -> f32;
    fn set_clip_u(&mut self, u: f32);
    fn clip_v(&self) -> f32;
    fn set_clip_v(&mut self, v: f32);
}

impl ClipQuadVertex for Vertex {
    #[inline]
    fn clip_x(&self) -> f32 {
        self.pos[0]
    }
    #[inline]
    fn set_clip_x(&mut self, x: f32) {
        self.pos[0] = x;
    }
    #[inline]
    fn clip_y(&self) -> f32 {
        self.pos[1]
    }
    #[inline]
    fn set_clip_y(&mut self, y: f32) {
        self.pos[1] = y;
    }
    #[inline]
    fn clip_u(&self) -> f32 {
        self.uv[0]
    }
    #[inline]
    fn set_clip_u(&mut self, u: f32) {
        self.uv[0] = u;
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
    fn clip_x(&self) -> f32 {
        self.pos[0]
    }
    #[inline]
    fn set_clip_x(&mut self, x: f32) {
        self.pos[0] = x;
    }
    #[inline]
    fn clip_y(&self) -> f32 {
        self.pos[1]
    }
    #[inline]
    fn set_clip_y(&mut self, y: f32) {
        self.pos[1] = y;
    }
    #[inline]
    fn clip_u(&self) -> f32 {
        self.uv[0]
    }
    #[inline]
    fn set_clip_u(&mut self, u: f32) {
        self.uv[0] = u;
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
pub(crate) fn clip_quads_to_rect<V: ClipQuadVertex>(verts: &mut [V], clip: [f32; 4]) {
    for quad in verts.chunks_exact_mut(VERTS_PER_QUAD) {
        let x0 = quad[0].clip_x();
        let x1 = quad[2].clip_x();
        let y0 = quad[0].clip_y();
        let y1 = quad[1].clip_y();
        let width = x1 - x0;
        let height = y1 - y0;
        if width <= 0.0 || height <= 0.0 {
            continue;
        }

        if x0 >= clip[2] {
            for vertex in quad.iter_mut() {
                vertex.set_clip_x(clip[2]);
            }
            continue;
        }
        if x1 <= clip[0] {
            for vertex in quad.iter_mut() {
                vertex.set_clip_x(clip[0]);
            }
            continue;
        }
        if y0 >= clip[3] {
            for vertex in quad.iter_mut() {
                vertex.set_clip_y(clip[3]);
            }
            continue;
        }
        if y1 <= clip[1] {
            for vertex in quad.iter_mut() {
                vertex.set_clip_y(clip[1]);
            }
            continue;
        }

        let u0 = quad[0].clip_u();
        let u1 = quad[2].clip_u();
        if x0 < clip[0] {
            let t = (clip[0] - x0) / width;
            let cropped_u = u0 + t * (u1 - u0);
            for &i in &[0usize, 1, 4] {
                quad[i].set_clip_x(clip[0]);
                quad[i].set_clip_u(cropped_u);
            }
        }
        if x1 > clip[2] {
            let t = (clip[2] - x0) / width;
            let cropped_u = u0 + t * (u1 - u0);
            for &i in &[2usize, 3, 5] {
                quad[i].set_clip_x(clip[2]);
                quad[i].set_clip_u(cropped_u);
            }
        }

        let v0 = quad[0].clip_v();
        let v1 = quad[1].clip_v();
        if y0 < clip[1] {
            let t = (clip[1] - y0) / height;
            let cropped_v = v0 + t * (v1 - v0);
            for &i in &[0usize, 2, 3] {
                quad[i].set_clip_y(clip[1]);
                quad[i].set_clip_v(cropped_v);
            }
        }
        if y1 > clip[3] {
            let t = (clip[3] - y0) / height;
            let cropped_v = v0 + t * (v1 - v0);
            for &i in &[1usize, 4, 5] {
                quad[i].set_clip_y(clip[3]);
                quad[i].set_clip_v(cropped_v);
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

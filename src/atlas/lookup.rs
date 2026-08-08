// SPDX-License-Identifier: GPL-3.0-only
//! Immutable UV and bearing-aware glyph lookup.

use super::*;

impl GlyphAtlas {
    /// Immutable UV lookup used by the per-frame geometry builder.
    ///
    /// Returns the atlas cell for printable ASCII, the resident cell for a
    /// non-ASCII codepoint already inserted via [`Self::ensure`], and the
    /// **fallback box** for any other printable codepoint (so missing glyphs
    /// render a visible box rather than blank). Spaces and control characters
    /// return `None` (nothing is drawn).
    pub fn uv_rect(&self, ch: char) -> Option<[f32; 4]> {
        self.uv_rect_styled(FontStyle::Regular, ch)
    }

    /// Style-aware immutable UV lookup (groundwork for attribute-driven
    /// rendering). Identical to [`Self::uv_rect`] for [`FontStyle::Regular`].
    ///
    /// The prebuilt printable-ASCII block belongs to `Regular`; for any other
    /// style, ASCII resolves through the dynamic region like every other glyph,
    /// returning the resident `(style, ch)` slot if present and the fallback box
    /// otherwise. Spaces and control characters return `None`.
    pub fn uv_rect_styled(&self, style: FontStyle, ch: char) -> Option<[f32; 4]> {
        let code = ch as u32;
        if style == FontStyle::Regular && (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_uv(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&(style, ch)) {
            return Some(self.slot_uv(slot));
        }
        if wants_glyph(ch) {
            return Some(self.slot_uv(FALLBACK_SLOT));
        }
        None
    }

    /// Normalized UV rectangle `[u0, v0, u1, v1]` for an atlas slot index.
    ///
    /// Returns the slot's **inner** `cell.width × cell.height` rectangle (the
    /// glyph area), inset past the [`ATLAS_PAD`] gutter on every side, so the
    /// per-cell quad samples only this glyph and the surrounding gutter shields
    /// it from neighbor bleed.
    pub(super) fn slot_uv(&self, slot: u32) -> [f32; 4] {
        let (ox, oy) = slot_offset(slot, self.cols, self.cell);
        let border = slot_border(self.cell);
        let ix = ox + border;
        let iy = oy + border;
        // Wide (East Asian) lead slots span two cells; their inner region is
        // `span * cell.width` wide. `slot_offset` already places consecutive
        // slots contiguously, so the reserved second cell sits immediately right.
        let span = self.slot_span[slot as usize] as u32;
        [
            ix as f32 / self.width as f32,
            iy as f32 / self.height as f32,
            (ix + span * self.cell.width) as f32 / self.width as f32,
            (iy + self.cell.height) as f32 / self.height as f32,
        ]
    }

    /// Bearing-aware quad geometry for a printable codepoint (Regular style).
    ///
    /// The geometry counterpart to [`Self::uv_rect`]: where `uv_rect` returns the
    /// fixed inner-cell rectangle, this returns the glyph's real inked extent
    /// (offset + size relative to the cell, plus the matching UV), so the
    /// renderer can draw ink that overflows the cell box uncropped. Resolution
    /// matches `uv_rect`: ASCII and resident codepoints resolve to their slot,
    /// other printables to the fallback box (full-cell bounds), spaces/controls
    /// to `None`.
    pub fn glyph_quad(&self, ch: char) -> Option<GlyphBounds> {
        self.glyph_quad_styled(FontStyle::Regular, ch)
    }

    /// Style-aware bearing-aware quad geometry. The geometry counterpart to
    /// [`Self::uv_rect_styled`]; see [`Self::glyph_quad`].
    pub fn glyph_quad_styled(&self, style: FontStyle, ch: char) -> Option<GlyphBounds> {
        let code = ch as u32;
        if style == FontStyle::Regular && (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_glyph_bounds(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&(style, ch)) {
            return Some(self.slot_glyph_bounds(slot));
        }
        if wants_glyph(ch) {
            return Some(self.slot_glyph_bounds(FALLBACK_SLOT));
        }
        None
    }

    /// Bearing-aware quad geometry for a zero-width combining mark, or `None`
    /// when the mark is not resident as a real glyph. Unlike
    /// [`Self::glyph_quad_styled`] this NEVER resolves to the hollow-box
    /// fallback: a mark the font lacks must simply not draw, because its quad
    /// is composited OVER an already-drawn base glyph and a tofu box there
    /// would obscure the base. Marks rasterize with a one-cell pen anchor (see
    /// `ensure_styled`), so the returned offsets place the ink on the base
    /// cell when the quad is anchored at that cell's origin.
    pub fn combining_mark_quad(&self, style: FontStyle, ch: char) -> Option<GlyphBounds> {
        let &slot = self.dynamic.get(&(style, ch))?;
        if slot == FALLBACK_SLOT {
            return None;
        }
        Some(self.slot_glyph_bounds(slot))
    }

    /// Build the public [`GlyphBounds`] for a slot from its stored ink extent,
    /// normalizing the UV against the current atlas dimensions (the V denominator
    /// changes as the atlas grows, so this is computed on demand, never cached).
    pub(super) fn slot_glyph_bounds(&self, slot: u32) -> GlyphBounds {
        let ink = self.slot_ink[slot as usize];
        let (ox, oy) = slot_offset(slot, self.cols, self.cell);
        let border = slot_border(self.cell) as i32;
        let cell_x = ox as i32 + border;
        let cell_y = oy as i32 + border;
        let ix = cell_x + ink.offset_x;
        let iy = cell_y + ink.offset_y;
        GlyphBounds {
            offset_x: ink.offset_x,
            offset_y: ink.offset_y,
            width: ink.width,
            height: ink.height,
            uv: [
                ix as f32 / self.width as f32,
                iy as f32 / self.height as f32,
                (ix + ink.width as i32) as f32 / self.width as f32,
                (iy + ink.height as i32) as f32 / self.height as f32,
            ],
        }
    }
}

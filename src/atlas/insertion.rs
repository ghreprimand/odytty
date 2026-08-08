// SPDX-License-Identifier: GPL-3.0-only
//! Scalar and shaped-glyph insertion into the dynamic atlas region.

use super::*;

impl GlyphAtlas {
    /// Resolve `ch` to a UV rect, rasterizing a real glyph into the dynamic
    /// region on first use (the **mutable** counterpart to [`Self::uv_rect`]).
    ///
    /// - Printable ASCII and already-resident codepoints resolve immediately.
    /// - A non-ASCII codepoint the font provides is rasterized into the next
    ///   free slot, growing the atlas by a page of rows if the region is full;
    ///   [`Self::take_dirty`] then reports the texture needs re-uploading.
    /// - A codepoint the font lacks (or one that would exceed [`MAX_ATLAS_SLOTS`])
    ///   resolves to the fallback box and is cached so the decision is made once.
    /// - Spaces and control characters return `None`.
    pub fn ensure(&mut self, font: &FontVec, ch: char) -> Option<[f32; 4]> {
        self.ensure_styled(font, FontStyle::Regular, ch)
    }

    /// Style-aware mutable insertion (groundwork for attribute-driven
    /// rendering). Identical to [`Self::ensure`] for [`FontStyle::Regular`].
    ///
    /// A non-`Regular` style rasterizes `ch` from the supplied `font` into a
    /// slot keyed by `(style, ch)`, so styled glyphs never collide with regular
    /// ones. Until a future change supplies a true bold/italic face, callers
    /// pass the regular font, so a styled slot holds the regular outline; the
    /// keying is what matters for groundwork. Growth, fallback, and the hard
    /// slot cap behave exactly as in [`Self::ensure`].
    pub fn ensure_styled(
        &mut self,
        font: &FontVec,
        style: FontStyle,
        ch: char,
    ) -> Option<[f32; 4]> {
        let code = ch as u32;
        if style == FontStyle::Regular && (FIRST_CHAR..=LAST_CHAR).contains(&code) {
            return Some(self.slot_uv(code - FIRST_CHAR + 1));
        }
        if let Some(&slot) = self.dynamic.get(&(style, ch)) {
            return Some(self.slot_uv(slot));
        }
        if !wants_glyph(ch) {
            return None;
        }
        // SYMMAP (extends RV6): a user-configured codepoint→font-family override
        // takes priority over every other glyph source. `None` (the default,
        // empty-map state) means no override and the scan is skipped, so the
        // path below is byte-identical to the no-SYMMAP renderer.
        let override_arc = self.symbol_map_font_for(ch);
        // Geometric box-drawing (RV2): when enabled, recognized line/block/
        // Powerline codepoints are rasterized from cell-aligned geometry instead
        // of the font glyph — and render even if the font lacks the codepoint.
        // A SYMMAP override suppresses geometric rendering: if the user
        // explicitly remapped a box-drawing range, their font glyph wins.
        let geometric = self.geometric && override_arc.is_none() && crate::boxdraw::covers(ch);
        // The raster face for this glyph, in precedence order:
        //   SYMMAP override > geometric (handled above) > RV6 glyph fallback >
        //   primary font > hollow-box tofu.
        // RV6: when the primary font lacks a printable spacing glyph, a fallback
        // font (if configured) rasterizes it instead of tofu.
        // `None` here means either no fallback is set, the codepoint is not a
        // standalone glyph candidate, or the fallback also lacks it -- all of
        // which fall through to the historical hollow-box slot.
        let mut symbol_font: Option<Arc<FontVec>> = None;
        if let Some(ov) = override_arc {
            // SYMMAP: rasterize from the override face directly (no synthetic
            // transform — icon faces are not emboldened/sheared), bypassing the
            // primary-font glyph-presence check and the fallback chain.
            symbol_font = Some(ov);
        } else if !geometric && !font_has_glyph(font, ch) {
            match self.symbol_fallback(ch) {
                Some(fb) => symbol_font = Some(fb),
                None => {
                    // Font lacks the glyph and no fallback applies: cache the
                    // fallback decision, draw nothing new.
                    self.dynamic.insert((style, ch), FALLBACK_SLOT);
                    return Some(self.slot_uv(FALLBACK_SLOT));
                }
            }
        }
        // Geometric glyphs are always single-cell (box/block/Powerline).
        let cells = if geometric { 1 } else { glyph_cells(ch) };
        let Some(slot) = self.allocate_slots(cells) else {
            // Atlas is at its hard cap: degrade to the fallback box.
            self.dynamic.insert((style, ch), FALLBACK_SLOT);
            return Some(self.slot_uv(FALLBACK_SLOT));
        };
        let origin = slot_offset(slot, self.cols, self.cell);
        let ink = if geometric {
            rasterize_geometric(
                ch,
                &mut self.data,
                self.width,
                self.subpixel,
                SlotRegion {
                    origin,
                    cell: self.cell,
                    outer_w: slot_w(self.cell),
                },
            )
            .unwrap_or_else(|| GlyphInk::cell(self.cell))
        } else {
            // A fallback glyph renders from the fallback face with no synthetic
            // transform (icons are not emboldened/sheared); otherwise
            // the primary font and the style's synthetic mask apply as usual.
            let (raster_font, synth): (&FontVec, SynthTransform) = match symbol_font.as_deref() {
                Some(fb) => (fb, SynthTransform::none()),
                None => (font, self.synth_for(style)),
            };
            // Symbol-fallback and SYMMAP-override faces are icon faces: fit each
            // glyph into its cell box (aspect-preserving) and center it, so they
            // form an even column instead of rendering at the body em-size with
            // natural bearing (overflow/clip + ragged x). Primary-font glyphs
            // (`symbol_font` is `None`) pass `None` → byte-identical placement.
            let fit = symbol_font.as_ref().map(|_| {
                let pad =
                    (SYMBOL_CELL_INSET * self.cell.width.min(self.cell.height) as f32).round();
                CellFit {
                    box_w: (cells * self.cell.width) as f32,
                    box_h: self.cell.height as f32,
                    pad,
                }
            });
            // Combining marks carry a zero advance and hang their ink LEFT of
            // the pen (they are typeset after their base advances). Anchoring
            // the pen one cell to the right places that ink over the slot's
            // cell box — the same anchor mechanism `ensure_shaped` uses — so
            // the renderer can draw the mark quad at the base cell's origin
            // and the recorded `GlyphInk` offsets land the ink on the base.
            let anchor_x = if is_combining_mark(ch) {
                self.cell.width as f32
            } else {
                0.0
            };
            rasterize_glyph(
                raster_font,
                Pen {
                    px: self.px,
                    baseline: self.cell.baseline as f32,
                },
                ch,
                anchor_x,
                &mut self.data,
                self.width,
                self.subpixel,
                SlotRegion {
                    origin,
                    cell: self.cell,
                    // Wide glyphs draw across `cells` contiguous slots; the clip
                    // extends over all of them so ink is never cropped at the edge.
                    outer_w: cells * slot_w(self.cell),
                },
                synth,
                fit,
            )
            .unwrap_or_else(|| GlyphInk::cell(self.cell))
        };
        // `allocate_slots` already pushed a dense placeholder for the lead (and
        // every reserved/filler slot); overwrite the lead with the real ink.
        self.slot_ink[slot as usize] = ink;
        self.dynamic.insert((style, ch), slot);
        self.revision += 1;
        self.dirty = true;
        Some(self.slot_uv(slot))
    }

    /// Rasterize a contextual OpenType glyph into a source-column-anchored
    /// multi-cell slot. The glyph ID is the shared OpenType index used by swash
    /// and ab_glyph. Shaped advances are intentionally absent from this API.
    pub fn ensure_shaped(&mut self, font: &FontVec, key: ShapedGlyphKey) -> Option<GlyphBounds> {
        if key.span_cells == 0 || key.anchor_cell >= key.span_cells {
            return None;
        }
        // A span wider than one atlas row cannot be stored: slots are
        // row-major, so the reserved cells would wrap onto later rows and the
        // rasterized ink strip would overwrite other glyphs' coverage while
        // `slot_uv` hands out u1 > 1.0. Refuse the allocation and let the
        // caller degrade to scalar per-cell fallback, exactly as it does on
        // atlas exhaustion (`contains_shaped` stays false for the key).
        if u32::from(key.span_cells) > self.cols {
            return None;
        }
        if let Some(&slot) = self.shaped.get(&key) {
            return self.shaped_bounds(slot);
        }
        let span = u32::from(key.span_cells);
        let slot = self.allocate_slots(span)?;
        let origin = slot_offset(slot, self.cols, self.cell);
        let synth = self.synth_for(key.style);
        let ink = rasterize_glyph_id(
            font,
            Pen {
                px: self.px,
                baseline: self.cell.baseline as f32,
            },
            GlyphId(key.glyph_id),
            f32::from(key.anchor_cell) * self.cell.width as f32,
            &mut self.data,
            self.width,
            self.subpixel,
            SlotRegion {
                origin,
                cell: self.cell,
                outer_w: span * slot_w(self.cell),
            },
            synth,
            None,
        )
        .unwrap_or(GlyphInk {
            offset_x: 0,
            offset_y: 0,
            width: 0,
            height: 0,
        });
        self.slot_ink[slot as usize] = ink;
        self.shaped.insert(key, slot);
        self.revision += 1;
        self.dirty = true;
        self.shaped_bounds(slot)
    }

    /// Immutable contextual-glyph lookup used after the ensure pass.
    pub fn shaped_glyph_quad(&self, key: ShapedGlyphKey) -> Option<GlyphBounds> {
        let slot = *self.shaped.get(&key)?;
        self.shaped_bounds(slot)
    }

    fn shaped_bounds(&self, slot: u32) -> Option<GlyphBounds> {
        let bounds = self.slot_glyph_bounds(slot);
        (bounds.width > 0 && bounds.height > 0).then_some(bounds)
    }

    /// Number of contextual glyph identities resident in the atlas.
    pub fn shaped_slot_count(&self) -> usize {
        self.shaped.len()
    }

    /// Whether a contextual identity owns an atlas slot. This is distinct from
    /// [`Self::shaped_glyph_quad`]: contextual fonts may intentionally replace
    /// one source glyph with a zero-ink spacer, which is resident but emits no
    /// quad. Callers use residency to distinguish that valid spacer from atlas
    /// exhaustion and retain scalar fallback for an unallocated span.
    pub fn contains_shaped(&self, key: ShapedGlyphKey) -> bool {
        self.shaped.contains_key(&key)
    }
}

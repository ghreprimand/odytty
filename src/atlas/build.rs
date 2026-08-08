// SPDX-License-Identifier: GPL-3.0-only
//! Atlas construction and initial printable-ASCII population.

use super::*;

impl GlyphAtlas {
    /// Rasterize printable ASCII at `px` pixels into a new atlas.
    ///
    /// `px` is the physical pixel size to rasterize at (caller multiplies the
    /// logical font size by the window scale factor for crisp HiDPI text).
    pub fn build(font: &FontVec, px: f32) -> Self {
        Self::build_with_subpixel(font, px, SubpixelMode::Off)
    }

    /// Rasterize printable ASCII into a new atlas with the requested coverage
    /// storage. Subpixel modes keep the same atlas dimensions and slot geometry
    /// as grayscale but store RGB stripe coverage in an RGBA8 bitmap.
    pub fn build_with_subpixel(font: &FontVec, px: f32, subpixel: SubpixelMode) -> Self {
        Self::build_with_options(font, px, subpixel, 1.0)
    }

    /// Rasterize printable ASCII with an explicit `line_height` multiplier
    /// (LINEHEIGHT). `1.0` is the historical cell geometry, byte-identical to
    /// [`Self::build_with_subpixel`]; values above `1.0` add vertical leading,
    /// split symmetrically (half above, half below) so the baseline stays
    /// centered and glyph rasterization is unchanged — only the cell box grows
    /// and the baseline shifts down by the top half. The added rows are
    /// transparent gutter, so default `1.0` produces identical coverage.
    pub fn build_with_options(
        font: &FontVec,
        px: f32,
        subpixel: SubpixelMode,
        line_height: f32,
    ) -> Self {
        let px = px.max(1.0);
        let scale = PxScale::from(px);
        let scaled = font.as_scaled(scale);

        // Monospace: every glyph shares the advance of a representative glyph.
        let advance = scaled.h_advance(font.glyph_id('M'));
        let ascent = scaled.ascent();
        let descent = scaled.descent(); // negative (below baseline)

        // Single documented baseline: the font ascent rounded to the nearest
        // whole pixel. Every glyph — ASCII, accents, box-drawing — is positioned
        // with its baseline on this one integer row, so mixed glyphs sit on a
        // common line and horizontal stems land on pixel boundaries for crisp
        // coverage. The cell height spans this baseline plus the descent so
        // descenders fit within the cell box.
        let baseline = ascent.round().max(0.0);
        let cell_w = advance.ceil().max(1.0) as u32;
        let cell_h = (ascent - descent).ceil().max(1.0) as u32;

        // LINEHEIGHT leading: extra rows added around the natural cell. At the
        // default `1.0` the leading is exactly 0, so `cell_h`/`baseline` are
        // unchanged and the atlas is byte-identical. The leading is split with
        // the larger half on top (`lead_top`) so the baseline moves down by that
        // amount; glyphs still rasterize against the same metrics, just lower in
        // a taller slot, which keeps every glyph's shape pixel-for-pixel.
        let leading = (((line_height.max(1.0) - 1.0) * cell_h as f32).round() as u32).min(cell_h);
        let lead_top = leading.div_ceil(2);
        let cell_h = cell_h + leading;
        // Shift the baseline down by the top leading so every glyph rasterizes
        // lower within the taller slot. Kept as `f32` for the rasterizer Pen;
        // the cell stores the rounded integer row.
        let baseline = baseline + lead_top as f32;
        let cell = CellSize {
            width: cell_w,
            height: cell_h,
            baseline: baseline as u32,
        };

        // Base region: fallback box (slot 0) + printable ASCII (slots 1..=95).
        // Each slot carries a transparent gutter (see `slot_offset`/`slot_uv`).
        let cols = ATLAS_COLS;
        let base_slots = FIRST_DYNAMIC_SLOT;
        let capacity_rows = base_slots.div_ceil(cols);
        let width = cols * slot_w(cell);
        let height = capacity_rows * slot_h(cell);
        let mut data = vec![0u8; atlas_byte_len(width, height, subpixel.bytes_per_pixel())];

        // Per-slot inked extent (index == slot). The fallback box keeps full-cell
        // bounds so a missing glyph renders exactly as before; ASCII slots record
        // their real ink as they are rasterized.
        let mut slot_ink = vec![GlyphInk::cell(cell); base_slots as usize];
        // Base region is all single-cell (fallback box + printable ASCII).
        let slot_span = vec![1u8; base_slots as usize];

        // Slot 0: synthesized hollow-box fallback, drawn the same for any font.
        let border = slot_border(cell);
        let (fox, foy) = slot_offset(FALLBACK_SLOT, cols, cell);
        draw_fallback_box(&mut data, width, subpixel, fox + border, foy + border, cell);

        // Slots 1..=95: printable ASCII at the build pixel size.
        for code in FIRST_CHAR..=LAST_CHAR {
            let ch = char::from_u32(code).unwrap_or(' ');
            let slot = code - FIRST_CHAR + 1;
            let origin = slot_offset(slot, cols, cell);
            if let Some(ink) = rasterize_glyph(
                font,
                Pen { px, baseline },
                ch,
                0.0, // printable ASCII: never a combining mark
                &mut data,
                width,
                subpixel,
                SlotRegion {
                    origin,
                    cell,
                    outer_w: slot_w(cell),
                },
                SynthTransform::none(),
                None, // ASCII: natural-bearing text, never fitted
            ) {
                slot_ink[slot as usize] = ink;
            }
        }

        Self {
            width,
            height,
            data,
            cell,
            slot_ink,
            slot_span,
            cols,
            capacity_rows,
            next_slot: base_slots,
            max_slots: MAX_ATLAS_SLOTS,
            dynamic: HashMap::new(),
            shaped: HashMap::new(),
            px,
            revision: 0,
            dirty: false,
            subpixel,
            synthetic: 0,
            geometric: false,
            fallback_chain: Vec::new(),
            symbol_map_fonts: Vec::new(),
            runtime_symbol_resolver: None,
            runtime_symbol_cache: HashMap::new(),
        }
    }
}

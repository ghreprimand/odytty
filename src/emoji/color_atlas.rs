//! Color glyph atlas groundwork for emoji rendering.
//!
//! This module deliberately stops before font decoding. EM4 will feed shaped
//! swash glyph/cluster ids plus premultiplied RGBA pixels into this atlas. EM3
//! only establishes the storage, keying, dirty tracking, and geometry contract
//! using synthetic RGBA images.

use std::collections::HashMap;

use crate::atlas::CellSize;

const ATLAS_COLS: u32 = 16;
const ATLAS_GROW_ROWS: u32 = 4;
const MAX_COLOR_GLYPH_SLOTS: u32 = 4096;

/// Stable identity for a shaped color glyph or cluster.
///
/// The key intentionally does not include a Unicode scalar or `char`. Real emoji
/// rendering is shaped text: a displayed color image may be a single glyph id
/// or a multi-codepoint cluster such as a ZWJ family, flag, keycap, or
/// variation-selector sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorGlyphKey {
    pub font_id: u64,
    pub glyph_id: ColorGlyphId,
    pub px_bits: u32,
    pub scale_bits: u32,
}

impl ColorGlyphKey {
    pub fn new(font_id: u64, glyph_id: ColorGlyphId, px_size: f32, scale: f32) -> Self {
        Self {
            font_id,
            glyph_id,
            px_bits: px_size.to_bits(),
            scale_bits: scale.to_bits(),
        }
    }
}

/// Shaped glyph identity. `Cluster` is for ligatures or sequences whose final
/// image is not represented by a single font glyph id at the render seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorGlyphId {
    Glyph(u32),
    Cluster(u64),
}

/// Atlas lookup result for one resident color glyph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorGlyphBounds {
    pub width_cells: u8,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub uv: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ColorGlyphSlot {
    slot: u32,
    width_cells: u8,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ColorGlyphAtlasError {
    #[error("color glyph width must span 1 or 2 cells, got {0}")]
    InvalidCellSpan(u8),
    #[error("premultiplied RGBA length mismatch: expected {expected} bytes, got {actual}")]
    Length { expected: usize, actual: usize },
    #[error("premultiplied source has RGB greater than alpha at byte {0}")]
    NotPremultiplied(usize),
    #[error("color glyph atlas slot cap reached")]
    Full,
}

/// A grow-only RGBA8 atlas for premultiplied color glyph images.
#[derive(Debug, Clone)]
pub struct ColorGlyphAtlas {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub cell: CellSize,
    cols: u32,
    capacity_rows: u32,
    next_slot: u32,
    slots: HashMap<ColorGlyphKey, ColorGlyphSlot>,
    revision: u64,
    dirty: bool,
}

impl ColorGlyphAtlas {
    pub fn new(cell: CellSize) -> Self {
        let width = ATLAS_COLS * max_slot_width(cell);
        let height = ATLAS_GROW_ROWS * cell.height.max(1);
        Self {
            width,
            height,
            data: vec![0; (width * height * 4) as usize],
            cell,
            cols: ATLAS_COLS,
            capacity_rows: ATLAS_GROW_ROWS,
            next_slot: 0,
            slots: HashMap::new(),
            revision: 0,
            dirty: false,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }

    pub fn lookup(&self, key: ColorGlyphKey) -> Option<ColorGlyphBounds> {
        let slot = self.slots.get(&key)?;
        Some(self.slot_bounds(*slot))
    }

    /// Insert one synthetic or decoded glyph image.
    ///
    /// `rgba` is `Rgba8Unorm` and must already be premultiplied. Its dimensions
    /// are implied by `width_cells * cell.width` by `cell.height`.
    pub fn insert_premultiplied(
        &mut self,
        key: ColorGlyphKey,
        width_cells: u8,
        rgba: &[u8],
    ) -> Result<ColorGlyphBounds, ColorGlyphAtlasError> {
        if let Some(bounds) = self.lookup(key) {
            return Ok(bounds);
        }
        if !(1..=2).contains(&width_cells) {
            return Err(ColorGlyphAtlasError::InvalidCellSpan(width_cells));
        }
        let pixel_width = self.cell.width as usize * width_cells as usize;
        let pixel_height = self.cell.height as usize;
        let expected = pixel_width * pixel_height * 4;
        if rgba.len() != expected {
            return Err(ColorGlyphAtlasError::Length {
                expected,
                actual: rgba.len(),
            });
        }
        validate_premultiplied(rgba)?;
        if self.next_slot >= MAX_COLOR_GLYPH_SLOTS {
            return Err(ColorGlyphAtlasError::Full);
        }

        self.grow_for_slot(self.next_slot);
        let slot = ColorGlyphSlot {
            slot: self.next_slot,
            width_cells,
        };
        self.next_slot += 1;
        self.copy_slot_pixels(slot, rgba);
        self.slots.insert(key, slot);
        self.revision = self.revision.wrapping_add(1);
        self.dirty = true;
        Ok(self.slot_bounds(slot))
    }

    fn grow_for_slot(&mut self, slot: u32) {
        let needed_rows = slot / self.cols + 1;
        if needed_rows <= self.capacity_rows {
            return;
        }
        while self.capacity_rows < needed_rows {
            self.capacity_rows += ATLAS_GROW_ROWS;
        }
        self.height = self.capacity_rows * self.cell.height.max(1);
        self.data.resize((self.width * self.height * 4) as usize, 0);
    }

    fn copy_slot_pixels(&mut self, slot: ColorGlyphSlot, rgba: &[u8]) {
        let (x0, y0) = self.slot_origin(slot.slot);
        let row_bytes = slot.width_cells as usize * self.cell.width as usize * 4;
        for row in 0..self.cell.height as usize {
            let dst = (((y0 as usize + row) * self.width as usize + x0 as usize) * 4) as usize;
            let src = row * row_bytes;
            self.data[dst..dst + row_bytes].copy_from_slice(&rgba[src..src + row_bytes]);
        }
    }

    fn slot_bounds(&self, slot: ColorGlyphSlot) -> ColorGlyphBounds {
        let (x0, y0) = self.slot_origin(slot.slot);
        let pixel_width = slot.width_cells as u32 * self.cell.width;
        let pixel_height = self.cell.height;
        ColorGlyphBounds {
            width_cells: slot.width_cells,
            pixel_width,
            pixel_height,
            uv: [
                x0 as f32 / self.width as f32,
                y0 as f32 / self.height as f32,
                (x0 + pixel_width) as f32 / self.width as f32,
                (y0 + pixel_height) as f32 / self.height as f32,
            ],
        }
    }

    fn slot_origin(&self, slot: u32) -> (u32, u32) {
        (
            (slot % self.cols) * max_slot_width(self.cell),
            (slot / self.cols) * self.cell.height.max(1),
        )
    }
}

fn max_slot_width(cell: CellSize) -> u32 {
    cell.width.max(1) * 2
}

fn validate_premultiplied(rgba: &[u8]) -> Result<(), ColorGlyphAtlasError> {
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        let alpha = px[3];
        if px[0] > alpha || px[1] > alpha || px[2] > alpha {
            return Err(ColorGlyphAtlasError::NotPremultiplied(i * 4));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> CellSize {
        CellSize {
            width: 4,
            height: 3,
            baseline: 2,
        }
    }

    fn key(id: u32) -> ColorGlyphKey {
        ColorGlyphKey::new(7, ColorGlyphId::Glyph(id), 16.0, 1.0)
    }

    fn rgba(width_cells: u8, color: [u8; 4]) -> Vec<u8> {
        let len = cell().width as usize * width_cells as usize * cell().height as usize;
        std::iter::repeat_n(color, len).flatten().collect()
    }

    #[test]
    fn insert_tracks_dirty_revision_and_uv_for_one_cell_slot() {
        let mut atlas = ColorGlyphAtlas::new(cell());
        assert_eq!(atlas.revision(), 0);
        assert!(!atlas.take_dirty());

        let bounds = atlas
            .insert_premultiplied(key(1), 1, &rgba(1, [20, 10, 5, 80]))
            .expect("insert");
        assert_eq!(bounds.width_cells, 1);
        assert_eq!(bounds.pixel_width, 4);
        assert_eq!(bounds.pixel_height, 3);
        assert_eq!(bounds.uv, [0.0, 0.0, 0.03125, 0.25]);
        assert_eq!(atlas.revision(), 1);
        assert!(atlas.take_dirty());
        assert!(!atlas.take_dirty());
    }

    #[test]
    fn two_cell_slot_uses_double_width_uv_without_char_keying() {
        let mut atlas = ColorGlyphAtlas::new(cell());
        let cluster = ColorGlyphKey::new(9, ColorGlyphId::Cluster(55), 18.0, 2.0);
        let bounds = atlas
            .insert_premultiplied(cluster, 2, &rgba(2, [4, 8, 12, 16]))
            .expect("insert");

        assert_eq!(atlas.lookup(cluster), Some(bounds));
        assert_eq!(bounds.width_cells, 2);
        assert_eq!(bounds.pixel_width, 8);
        assert_eq!(bounds.uv[2] - bounds.uv[0], 0.0625);
    }

    #[test]
    fn rejects_straight_alpha_source_pixels() {
        let mut atlas = ColorGlyphAtlas::new(cell());
        let err = atlas
            .insert_premultiplied(key(2), 1, &rgba(1, [100, 0, 0, 99]))
            .expect_err("straight alpha rejected");
        assert_eq!(err, ColorGlyphAtlasError::NotPremultiplied(0));
    }

    #[test]
    fn duplicate_insert_reuses_existing_slot_without_dirtying() {
        let mut atlas = ColorGlyphAtlas::new(cell());
        let first = atlas
            .insert_premultiplied(key(3), 1, &rgba(1, [1, 2, 3, 4]))
            .expect("first");
        assert!(atlas.take_dirty());

        let second = atlas
            .insert_premultiplied(key(3), 1, &rgba(1, [4, 3, 2, 4]))
            .expect("second");
        assert_eq!(first, second);
        assert_eq!(atlas.revision(), 1);
        assert!(!atlas.take_dirty());
    }
}

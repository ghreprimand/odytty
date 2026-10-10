// SPDX-License-Identifier: GPL-3.0-only
//! OdyTTY-owned atlas for shaped color glyphs and clusters.
//!
//! The emoji rasterizer supplies premultiplied RGBA pixels keyed by
//! font/glyph-or-cluster identity, and the GPU renderer uploads and
//! composites the live color-glyph segment (`src/native/gpu/scene.rs`).

use std::collections::HashMap;

use crate::atlas::CellSize;

const ATLAS_COLS: u32 = 16;
const ATLAS_GROW_ROWS: u32 = 4;
const MAX_COLOR_GLYPH_SLOTS: u32 = 16384;
// Separate RGBA bitmap budget; GPU texture and temporary uploads add memory.
// Retain color residency independently of the monochrome coverage budget.
const MAX_COLOR_BITMAP_BYTES: usize = 256 * 1024 * 1024;

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
    /// Cell span the bitmap was rendered for. Part of the identity: the same
    /// glyph id requested at a different span must rasterize its own slot:
    /// without this, whichever width rendered first would be returned for
    /// every later width (a one- vs two-cell presentation collision).
    pub width_cells: u8,
}

impl ColorGlyphKey {
    pub fn new(
        font_id: u64,
        glyph_id: ColorGlyphId,
        px_size: f32,
        scale: f32,
        width_cells: u8,
    ) -> Self {
        Self {
            font_id,
            glyph_id,
            px_bits: px_size.to_bits(),
            scale_bits: scale.to_bits(),
            width_cells,
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
    #[error("color glyph key span {key} disagrees with bitmap span {bitmap}")]
    WidthMismatch { key: u8, bitmap: u8 },
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
    enabled: bool,
    cols: u32,
    capacity_rows: u32,
    next_slot: u32,
    max_slots: u32,
    max_texture_dimension: u32,
    slots: HashMap<ColorGlyphKey, ColorGlyphSlot>,
    revision: u64,
    dirty: bool,
}

impl ColorGlyphAtlas {
    pub fn new(cell: CellSize) -> Self {
        Self::with_texture_dimension_limit(cell, u32::MAX)
    }

    /// Allocate only a complete, bounded initial page that fits the device.
    /// Refused geometry retains the cell metrics and a transparent 1x1 texture;
    /// every lookup/insertion declines, leaving monochrome fallback visible.
    pub fn with_texture_dimension_limit(cell: CellSize, max_dimension: u32) -> Self {
        let layout = initial_layout(cell, max_dimension);
        let mut data = Vec::new();
        let layout = layout.filter(|(_, _, bytes)| data.try_reserve_exact(*bytes).is_ok());
        let (width, height, enabled) = match layout {
            Some((width, height, bytes)) => {
                data.resize(bytes, 0);
                (width, height, true)
            }
            None => {
                data = vec![0; 4];
                (1, 1, false)
            }
        };
        let mut atlas = Self {
            width,
            height,
            data,
            cell,
            enabled,
            cols: ATLAS_COLS,
            capacity_rows: if enabled { ATLAS_GROW_ROWS } else { 0 },
            next_slot: 0,
            max_slots: 0,
            max_texture_dimension: max_dimension,
            slots: HashMap::new(),
            revision: 0,
            dirty: false,
        };
        atlas.set_texture_dimension_limit(max_dimension);
        atlas
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn take_dirty(&mut self) -> bool {
        let dirty = self.dirty;
        self.dirty = false;
        dirty
    }

    /// Heap bytes the CPU-side RGBA bitmap currently holds, for memory
    /// attribution. Reports the allocation's capacity for the same reason the
    /// monochrome atlas does: capacity is what the process is resident for.
    pub fn cpu_bitmap_bytes(&self) -> u64 {
        self.data.capacity() as u64
    }

    /// Bytes the GPU colour-glyph texture occupies at the current dimensions.
    /// The atlas is always RGBA8, so four bytes per pixel.
    pub fn gpu_texture_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4
    }

    /// Bind both atlas dimensions to the active device's 2D texture limit.
    /// Oversized atlases decline lookups and inserts without discarding slots.
    /// Inserts return [`ColorGlyphAtlasError::Full`] when another complete
    /// growth page would exceed the limit.
    pub fn set_texture_dimension_limit(&mut self, max_dimension: u32) {
        self.max_texture_dimension = max_dimension;
        if !self.enabled {
            self.max_slots = 0;
            return;
        }
        let row_bytes = self.width as usize * self.cell.height as usize * 4;
        let bitmap_rows = (MAX_COLOR_BITMAP_BYTES / row_bytes) as u32;
        let rows = (max_dimension / self.cell.height).min(bitmap_rows);
        let reachable_rows = rows / ATLAS_GROW_ROWS * ATLAS_GROW_ROWS;
        self.max_slots = reachable_rows
            .saturating_mul(self.cols)
            .min(MAX_COLOR_GLYPH_SLOTS)
            .max(self.next_slot);
    }

    pub fn lookup(&self, key: ColorGlyphKey) -> Option<ColorGlyphBounds> {
        if !self.enabled
            || self.width > self.max_texture_dimension
            || self.height > self.max_texture_dimension
        {
            return None;
        }
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
        if !(1..=2).contains(&width_cells) {
            return Err(ColorGlyphAtlasError::InvalidCellSpan(width_cells));
        }
        if key.width_cells != width_cells {
            return Err(ColorGlyphAtlasError::WidthMismatch {
                key: key.width_cells,
                bitmap: width_cells,
            });
        }
        if !self.enabled
            || self.width > self.max_texture_dimension
            || self.height > self.max_texture_dimension
        {
            return Err(ColorGlyphAtlasError::Full);
        }
        if let Some(bounds) = self.lookup(key) {
            return Ok(bounds);
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
        if self.next_slot >= self.max_slots {
            return Err(ColorGlyphAtlasError::Full);
        }

        if !self.grow_for_slot(self.next_slot) {
            return Err(ColorGlyphAtlasError::Full);
        }
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

    fn grow_for_slot(&mut self, slot: u32) -> bool {
        let needed_rows = slot / self.cols + 1;
        if needed_rows <= self.capacity_rows {
            return true;
        }
        let capacity_rows = needed_rows.div_ceil(ATLAS_GROW_ROWS) * ATLAS_GROW_ROWS;
        let Some(height) = capacity_rows.checked_mul(self.cell.height) else {
            return false;
        };
        if height > self.max_texture_dimension {
            return false;
        }
        let Some(bytes) = bitmap_byte_len(self.width, height) else {
            return false;
        };
        if self
            .data
            .try_reserve_exact(bytes - self.data.len())
            .is_err()
        {
            return false;
        }
        self.data.resize(bytes, 0);
        self.height = height;
        self.capacity_rows = capacity_rows;
        true
    }

    fn copy_slot_pixels(&mut self, slot: ColorGlyphSlot, rgba: &[u8]) {
        let (x0, y0) = self.slot_origin(slot.slot);
        let row_bytes = slot.width_cells as usize * self.cell.width as usize * 4;
        for row in 0..self.cell.height as usize {
            let dst = ((y0 as usize + row) * self.width as usize + x0 as usize) * 4;
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

fn bitmap_byte_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
        .filter(|bytes| *bytes <= MAX_COLOR_BITMAP_BYTES)
}

fn initial_layout(cell: CellSize, max_dimension: u32) -> Option<(u32, u32, usize)> {
    if cell.width == 0 || cell.height == 0 {
        return None;
    }
    let width = cell.width.checked_mul(2)?.checked_mul(ATLAS_COLS)?;
    let height = cell.height.checked_mul(ATLAS_GROW_ROWS)?;
    if width > max_dimension || height > max_dimension {
        return None;
    }
    Some((width, height, bitmap_byte_len(width, height)?))
}

fn max_slot_width(cell: CellSize) -> u32 {
    cell.width.max(1) * 2
}

fn validate_premultiplied(rgba: &[u8]) -> Result<(), ColorGlyphAtlasError> {
    for (i, px) in rgba.as_chunks::<4>().0.iter().enumerate() {
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
        ColorGlyphKey::new(7, ColorGlyphId::Glyph(id), 16.0, 1.0, 1)
    }

    fn cluster_key(id: u32) -> ColorGlyphKey {
        ColorGlyphKey::new(7, ColorGlyphId::Cluster(u64::from(id)), 16.0, 1.0, 1)
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
    fn same_glyph_at_different_widths_gets_distinct_slots() {
        // width_cells is part of the key identity: the same glyph id rendered
        // for a one-cell and a two-cell presentation must occupy separate
        // slots, not return whichever width happened to rasterize first.
        let mut atlas = ColorGlyphAtlas::new(cell());
        let narrow = ColorGlyphKey::new(7, ColorGlyphId::Glyph(11), 16.0, 1.0, 1);
        let wide = ColorGlyphKey::new(7, ColorGlyphId::Glyph(11), 16.0, 1.0, 2);
        assert_ne!(narrow, wide, "width_cells must differentiate the keys");
        let nb = atlas
            .insert_premultiplied(narrow, 1, &rgba(1, [1, 2, 3, 4]))
            .expect("narrow insert");
        let wb = atlas
            .insert_premultiplied(wide, 2, &rgba(2, [5, 6, 7, 8]))
            .expect("wide insert");
        assert_eq!(atlas.lookup(narrow), Some(nb));
        assert_eq!(atlas.lookup(wide), Some(wb));
        assert_eq!(nb.width_cells, 1);
        assert_eq!(wb.width_cells, 2);
        assert_ne!(nb.uv, wb.uv, "distinct slots, distinct UVs");
    }

    #[test]
    fn two_cell_slot_uses_double_width_uv_without_char_keying() {
        let mut atlas = ColorGlyphAtlas::new(cell());
        let cluster = ColorGlyphKey::new(9, ColorGlyphId::Cluster(55), 18.0, 2.0, 2);
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

    #[test]
    fn capacity_is_bounded_and_full_does_not_overwrite_existing_slots() {
        let mut atlas = ColorGlyphAtlas::new(cell());
        let first_key = cluster_key(0);
        atlas
            .insert_premultiplied(first_key, 1, &rgba(1, [10, 20, 30, 255]))
            .expect("first slot");

        for id in 1..MAX_COLOR_GLYPH_SLOTS {
            atlas
                .insert_premultiplied(cluster_key(id), 1, &rgba(1, [1, 2, 3, 255]))
                .expect("slot before cap");
        }

        let final_key = cluster_key(MAX_COLOR_GLYPH_SLOTS - 1);
        let final_bounds = atlas.lookup(final_key).expect("final slot lookup");
        assert_eq!(atlas.slots.len(), MAX_COLOR_GLYPH_SLOTS as usize);
        assert_eq!(atlas.next_slot, MAX_COLOR_GLYPH_SLOTS);
        assert_eq!(atlas.capacity_rows, MAX_COLOR_GLYPH_SLOTS / ATLAS_COLS);
        assert_eq!(atlas.height, atlas.capacity_rows * cell().height);
        assert_eq!(atlas.data.len(), (atlas.width * atlas.height * 4) as usize);
        assert_eq!(atlas.revision(), u64::from(MAX_COLOR_GLYPH_SLOTS));
        let first_after_growth = atlas.lookup(first_key).expect("first slot lookup");
        assert_eq!(first_after_growth.pixel_width, cell().width);
        assert_eq!(&atlas.data[..4], &[10, 20, 30, 255]);
        assert_eq!(final_bounds.pixel_width, cell().width);
        assert!(final_bounds.uv[2] <= 1.0);
        assert!(final_bounds.uv[3] <= 1.0);

        assert!(atlas.take_dirty());
        let overflow_key = cluster_key(MAX_COLOR_GLYPH_SLOTS);
        let err = atlas
            .insert_premultiplied(overflow_key, 1, &rgba(1, [4, 5, 6, 255]))
            .expect_err("cap returns Full");
        assert_eq!(err, ColorGlyphAtlasError::Full);
        assert_eq!(atlas.lookup(overflow_key), None);
        assert_eq!(atlas.lookup(first_key), Some(first_after_growth));
        assert_eq!(&atlas.data[..4], &[10, 20, 30, 255]);
        assert_eq!(atlas.lookup(final_key), Some(final_bounds));
        assert_eq!(atlas.slots.len(), MAX_COLOR_GLYPH_SLOTS as usize);
        assert_eq!(atlas.next_slot, MAX_COLOR_GLYPH_SLOTS);
        assert_eq!(atlas.revision(), u64::from(MAX_COLOR_GLYPH_SLOTS));
        assert!(!atlas.take_dirty());
    }

    #[test]
    fn device_height_limit_stops_before_an_oversized_growth_page() {
        // The initial width also fits the height-bound device limit.
        let mut atlas = ColorGlyphAtlas::new(CellSize {
            width: 1,
            height: 8,
            baseline: 6,
        });
        let pixels = [255; 32];
        let initial_height = atlas.height;
        atlas.set_texture_dimension_limit(initial_height);
        let slots_in_page = ATLAS_COLS * ATLAS_GROW_ROWS;
        for id in 0..slots_in_page {
            atlas
                .insert_premultiplied(cluster_key(id), 1, &pixels)
                .expect("slot within device limit");
        }
        assert_eq!(atlas.height, initial_height);
        assert_eq!(
            atlas.insert_premultiplied(cluster_key(slots_in_page), 1, &pixels,),
            Err(ColorGlyphAtlasError::Full)
        );
        assert_eq!(atlas.height, initial_height);
    }
}

#[cfg(test)]
mod capacity_policy_tests;

//! Cell-anchored graphics placement scene.
//!
//! Placements are terminal state, not renderer state: they scroll with text,
//! clear with terminal erase/reset operations, and stay isolated between the
//! primary and alternate buffers. Coordinates are physical cell anchors. G2.1
//! deliberately avoids protocol decoding; it only stores decoded-image records,
//! placement records, and raw Kitty/Sixel payloads for later protocol packets.

use std::collections::VecDeque;

use super::store::{ImageInsert, ImageStore, ImageStoreError, ImageStoreLimits, StoredImageId};

pub const MAX_RAW_GRAPHICS_COMMANDS: usize = 64;
pub const MAX_RAW_GRAPHICS_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlacementId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphicsProtocol {
    Kitty,
    Sixel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ScreenBuffer {
    Primary,
    Alternate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellAnchor {
    pub row: isize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementRequest {
    pub image_id: StoredImageId,
    pub protocol: GraphicsProtocol,
    pub anchor: CellAnchor,
    pub source: SourceRect,
    pub display_columns: usize,
    pub display_rows: usize,
    pub pixel_offset_x: i32,
    pub pixel_offset_y: i32,
    pub z_index: i32,
}

impl PlacementRequest {
    pub fn new(
        image_id: StoredImageId,
        protocol: GraphicsProtocol,
        row: usize,
        column: usize,
        display_columns: usize,
        display_rows: usize,
    ) -> Self {
        Self {
            image_id,
            protocol,
            anchor: CellAnchor {
                row: row as isize,
                column,
            },
            source: SourceRect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            display_columns,
            display_rows,
            pixel_offset_x: 0,
            pixel_offset_y: 0,
            z_index: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePlacement {
    pub id: PlacementId,
    pub image_id: StoredImageId,
    pub protocol: GraphicsProtocol,
    pub anchor: CellAnchor,
    pub source: SourceRect,
    pub display_columns: usize,
    pub display_rows: usize,
    pub pixel_offset_x: i32,
    pub pixel_offset_y: i32,
    pub z_index: i32,
    pub generation: u64,
    buffer: ScreenBuffer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisiblePlacement {
    pub id: PlacementId,
    pub image_id: StoredImageId,
    pub protocol: GraphicsProtocol,
    pub row: usize,
    pub column: usize,
    pub source: SourceRect,
    pub display_columns: usize,
    pub display_rows: usize,
    pub pixel_offset_x: i32,
    pub pixel_offset_y: i32,
    pub z_index: i32,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphicsCommand {
    KittyApc {
        payload: Vec<u8>,
    },
    SixelDcs {
        raw_body: Vec<u8>,
        payload_start: usize,
        p2: Option<u16>,
    },
}

#[derive(Debug, Clone)]
pub struct ImageScene {
    store: ImageStore,
    placements: Vec<ImagePlacement>,
    raw_commands: VecDeque<GraphicsCommand>,
    next_placement_id: u64,
    next_generation: u64,
    active: ScreenBuffer,
}

impl Default for ImageScene {
    fn default() -> Self {
        Self::new(ImageStoreLimits::default())
    }
}

impl ImageScene {
    pub fn new(store_limits: ImageStoreLimits) -> Self {
        Self {
            store: ImageStore::new(store_limits),
            placements: Vec::new(),
            raw_commands: VecDeque::new(),
            next_placement_id: 1,
            next_generation: 1,
            active: ScreenBuffer::Primary,
        }
    }

    pub fn store(&self) -> &ImageStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut ImageStore {
        &mut self.store
    }

    pub fn insert_rgba(
        &mut self,
        protocol_id: Option<u32>,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<ImageInsert, ImageStoreError> {
        let inserted = self.store.insert_rgba(protocol_id, width, height, rgba)?;
        self.remove_placements_for_images(&inserted.evicted);
        Ok(inserted)
    }

    pub fn place(&mut self, request: PlacementRequest) -> Option<PlacementId> {
        if !self.store.contains(request.image_id)
            || request.display_columns == 0
            || request.display_rows == 0
        {
            return None;
        }

        self.store.touch(request.image_id);
        let id = PlacementId(self.next_placement_id);
        self.next_placement_id += 1;
        let generation = self.next_generation;
        self.next_generation += 1;
        self.placements.push(ImagePlacement {
            id,
            image_id: request.image_id,
            protocol: request.protocol,
            anchor: request.anchor,
            source: request.source,
            display_columns: request.display_columns,
            display_rows: request.display_rows,
            pixel_offset_x: request.pixel_offset_x,
            pixel_offset_y: request.pixel_offset_y,
            z_index: request.z_index,
            generation,
            buffer: self.active,
        });
        Some(id)
    }

    pub fn placements(&self) -> &[ImagePlacement] {
        &self.placements
    }

    pub fn raw_commands(&self) -> &VecDeque<GraphicsCommand> {
        &self.raw_commands
    }

    pub fn record_kitty_apc(&mut self, payload: &[u8]) -> bool {
        if !payload.starts_with(b"G") {
            return false;
        }
        self.push_raw(GraphicsCommand::KittyApc {
            payload: capped_bytes(payload),
        });
        true
    }

    pub fn record_sixel_dcs(
        &mut self,
        raw_body: &[u8],
        payload_start: usize,
        p2: Option<u16>,
    ) -> bool {
        if payload_start > raw_body.len() || !raw_body[..payload_start].contains(&b'q') {
            return false;
        }
        self.push_raw(GraphicsCommand::SixelDcs {
            raw_body: capped_bytes(raw_body),
            payload_start,
            p2,
        });
        true
    }

    pub fn enter_alternate(&mut self, clear: bool) {
        self.active = ScreenBuffer::Alternate;
        if clear {
            self.placements
                .retain(|placement| placement.buffer != ScreenBuffer::Alternate);
        }
    }

    pub fn leave_alternate(&mut self) {
        self.placements
            .retain(|placement| placement.buffer != ScreenBuffer::Alternate);
        self.active = ScreenBuffer::Primary;
    }

    pub fn clear_active(&mut self) {
        let active = self.active;
        self.placements
            .retain(|placement| placement.buffer != active);
    }

    pub fn hard_reset(&mut self) {
        self.clear_active();
        self.raw_commands.clear();
        self.active = ScreenBuffer::Primary;
    }

    // -----------------------------------------------------------------------
    // Kitty graphics protocol delete actions (a=d)
    // -----------------------------------------------------------------------

    /// `d=a` — delete all visible placements in the active buffer.
    pub fn delete_all_placements(&mut self) {
        self.clear_active();
    }

    /// `d=A` — delete all visible placements AND free images with no remaining
    /// placements.
    pub fn delete_all_placements_and_free(&mut self) {
        self.clear_active();
        self.gc_unreferenced_images();
    }

    /// `d=i` — delete placements referencing `image_id` (protocol id) in the
    /// active buffer. If `placement_id` is `Some`, delete only that specific
    /// placement.
    pub fn delete_by_image_id(&mut self, image_id: u32, placement_id: Option<u32>) {
        let active = self.active;
        self.placements.retain(|p| {
            if p.buffer != active {
                return true;
            }
            let img_match = self
                .store
                .get(p.image_id)
                .map_or(false, |img| img.protocol_id == Some(image_id));
            if !img_match {
                return true;
            }
            if let Some(pid) = placement_id {
                p.id.0 != pid as u64
            } else {
                false
            }
        });
    }

    /// `d=I` — like `d=i` but also free image data when no placements remain.
    pub fn delete_by_image_id_and_free(&mut self, image_id: u32, placement_id: Option<u32>) {
        self.delete_by_image_id(image_id, placement_id);
        self.gc_unreferenced_images();
    }

    /// `d=c` / `d=C` — delete placements whose anchor is at (`row`, `col`) in
    /// the active buffer.
    pub fn delete_at_cursor(&mut self, row: usize, col: usize, free_images: bool) {
        let active = self.active;
        self.placements.retain(|p| {
            if p.buffer != active {
                return true;
            }
            !(p.anchor.row == row as isize && p.anchor.column == col)
        });
        if free_images {
            self.gc_unreferenced_images();
        }
    }

    /// `d=p` / `d=P` — delete placements that intersect cell (`row`, `col`) in
    /// the active buffer.
    pub fn delete_at_position(&mut self, row: usize, col: usize, free_images: bool) {
        let active = self.active;
        self.placements.retain(|p| {
            if p.buffer != active {
                return true;
            }
            let r = p.anchor.row;
            let c = p.anchor.column;
            let intersects = row as isize >= r
                && (row as isize) < r + p.display_rows as isize
                && col >= c
                && col < c + p.display_columns;
            !intersects
        });
        if free_images {
            self.gc_unreferenced_images();
        }
    }

    /// Remove images from the store that have no placements referencing them.
    fn gc_unreferenced_images(&mut self) {
        let referenced: std::collections::HashSet<StoredImageId> =
            self.placements.iter().map(|p| p.image_id).collect();
        let all_ids: Vec<StoredImageId> = self
            .store
            .iter_ids()
            .filter(|id| !referenced.contains(id))
            .collect();
        for id in all_ids {
            self.store.remove(id);
        }
    }

    pub fn scroll_full_up(&mut self, count: usize, scrollback_rows: usize) {
        self.shift_rows(0, None, -(count as isize), true);
        self.evict_above_scrollback(scrollback_rows);
    }

    pub fn scroll_region_up(&mut self, top: usize, bottom: usize, count: usize) {
        self.shift_rows(
            top as isize,
            Some(bottom as isize),
            -(count as isize),
            false,
        );
        self.drop_outside_region(top as isize, bottom as isize);
    }

    pub fn scroll_region_down(&mut self, top: usize, bottom: usize, count: usize) {
        self.shift_rows(top as isize, Some(bottom as isize), count as isize, false);
        self.drop_outside_region(top as isize, bottom as isize);
    }

    pub fn erase_display(
        &mut self,
        mode: usize,
        cursor_row: usize,
        cursor_column: usize,
        rows: usize,
        columns: usize,
    ) {
        let active = self.active;
        self.placements.retain(|placement| {
            if placement.buffer != active {
                return true;
            }
            match mode {
                0 => !intersects_range(placement, cursor_row, cursor_column, rows, columns),
                1 => !intersects_range(placement, 0, 0, cursor_row + 1, cursor_column + 1),
                2 | 3 => false,
                _ => true,
            }
        });
    }

    pub fn resize(&mut self, rows: usize, columns: usize) {
        let active = self.active;
        self.placements.retain(|placement| {
            if placement.buffer != active {
                return true;
            }
            placement.anchor.column < columns && placement.anchor.row < rows as isize
        });
    }

    pub fn visible_placements(
        &self,
        offset_rows: usize,
        viewport_rows: usize,
        viewport_columns: usize,
    ) -> Vec<VisiblePlacement> {
        let offset = offset_rows as isize;
        let active = self.active;
        let mut visible = Vec::new();
        for placement in self
            .placements
            .iter()
            .filter(|placement| placement.buffer == active)
        {
            let projected_row = placement.anchor.row + offset;
            if projected_row + placement.display_rows as isize <= 0
                || projected_row >= viewport_rows as isize
                || placement.anchor.column >= viewport_columns
            {
                continue;
            }
            visible.push(VisiblePlacement {
                id: placement.id,
                image_id: placement.image_id,
                protocol: placement.protocol,
                row: projected_row.max(0) as usize,
                column: placement.anchor.column,
                source: placement.source,
                display_columns: placement
                    .display_columns
                    .min(viewport_columns - placement.anchor.column),
                display_rows: placement
                    .display_rows
                    .min(viewport_rows - projected_row.max(0) as usize),
                pixel_offset_x: placement.pixel_offset_x,
                pixel_offset_y: placement.pixel_offset_y,
                z_index: placement.z_index,
                generation: placement.generation,
            });
        }
        visible.sort_by_key(|placement| (placement.z_index, placement.generation));
        visible
    }

    fn push_raw(&mut self, command: GraphicsCommand) {
        self.raw_commands.push_back(command);
        while self.raw_commands.len() > MAX_RAW_GRAPHICS_COMMANDS {
            self.raw_commands.pop_front();
        }
    }

    fn shift_rows(
        &mut self,
        top: isize,
        bottom: Option<isize>,
        delta: isize,
        allow_negative: bool,
    ) {
        let active = self.active;
        for placement in self
            .placements
            .iter_mut()
            .filter(|placement| placement.buffer == active)
        {
            if placement.anchor.row < top {
                continue;
            }
            if let Some(bottom) = bottom {
                if placement.anchor.row > bottom {
                    continue;
                }
            }
            placement.anchor.row += delta;
            if !allow_negative && placement.anchor.row < top {
                placement.anchor.row = top - placement.display_rows as isize;
            }
        }
    }

    fn drop_outside_region(&mut self, top: isize, bottom: isize) {
        let active = self.active;
        self.placements.retain(|placement| {
            if placement.buffer != active {
                return true;
            }
            let start = placement.anchor.row;
            let end = placement.anchor.row + placement.display_rows as isize - 1;
            !(start < top || end > bottom)
        });
    }

    fn evict_above_scrollback(&mut self, scrollback_rows: usize) {
        let oldest = -(scrollback_rows as isize);
        let active = self.active;
        self.placements.retain(|placement| {
            if placement.buffer != active {
                return true;
            }
            placement.anchor.row + placement.display_rows as isize > oldest
        });
    }

    fn remove_placements_for_images(&mut self, evicted: &[StoredImageId]) {
        if evicted.is_empty() {
            return;
        }
        self.placements
            .retain(|placement| !evicted.contains(&placement.image_id));
    }
}

fn capped_bytes(payload: &[u8]) -> Vec<u8> {
    payload
        .iter()
        .take(MAX_RAW_GRAPHICS_BYTES)
        .copied()
        .collect()
}

fn intersects_range(
    placement: &ImagePlacement,
    start_row: usize,
    start_column: usize,
    end_row_exclusive: usize,
    end_column_exclusive: usize,
) -> bool {
    let place_start_row = placement.anchor.row.max(0) as usize;
    let place_end_row = place_start_row + placement.display_rows;
    let place_start_column = placement.anchor.column;
    let place_end_column = place_start_column + placement.display_columns;

    let row_overlap = place_start_row < end_row_exclusive && place_end_row > start_row;
    let column_overlap =
        place_start_column < end_column_exclusive && place_end_column > start_column;
    row_overlap && column_overlap
}

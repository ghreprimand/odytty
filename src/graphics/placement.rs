// SPDX-License-Identifier: GPL-3.0-only
//! Cell-anchored graphics placement scene.
//!
//! Placements are terminal state, not renderer state: they scroll with text,
//! clear with terminal erase/reset operations, and stay isolated between the
//! primary and alternate buffers. Coordinates are physical cell anchors. G2.1
//! deliberately avoids protocol decoding; it only stores decoded-image records,
//! placement records, and raw Kitty/Sixel payloads for later protocol decoding.

use std::collections::VecDeque;

use super::frames::{AnimationControl, FrameComposition, FrameError, FrameUpdate};
use super::store::{ImageInsert, ImageStore, ImageStoreError, ImageStoreLimits, StoredImageId};

pub const MAX_RAW_GRAPHICS_COMMANDS: usize = 64;
pub const MAX_RAW_GRAPHICS_BYTES: usize = 1024 * 1024;
pub const MAX_IMAGE_PLACEMENTS_PER_BUFFER: usize = 64;
/// Live cap on virtual (Unicode-placeholder) placements. Virtual placements
/// carry no screen location, so the per-buffer placement cap does not bound
/// them; this does. Oldest-first eviction, same shape as the placement cap.
pub const MAX_VIRTUAL_PLACEMENTS: usize = 64;
/// Upper bound on a virtual placement's cell extent per axis. The extent comes
/// from client-supplied `c=`/`r=` (untrusted), and unlike a real placement it
/// is not clamped by the screen at creation time, so it is clamped here.
pub const MAX_VIRTUAL_EXTENT: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlacementId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphicsProtocol {
    Kitty,
    Sixel,
    /// iTerm2 inline image (`OSC 1337 ; File=`).
    Iterm2,
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
    /// Protocol-level image id (Kitty `i=`); `None` for protocols without one
    /// (e.g. Sixel). Used together with `protocol_placement_id` to identify a
    /// placement for replacement and delete-by-placement semantics.
    pub protocol_image_id: Option<u32>,
    /// Protocol-level placement id (Kitty `p=`); `None` when unspecified. A new
    /// placement with the same `(protocol_image_id, protocol_placement_id)` in
    /// the active buffer replaces the existing one (Kitty spec behavior).
    pub protocol_placement_id: Option<u32>,
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
            protocol_image_id: None,
            protocol_placement_id: None,
        }
    }

    /// Set the source crop rectangle (Kitty `x/y/w/h`, pixels).
    pub fn with_source(mut self, source: SourceRect) -> Self {
        self.source = source;
        self
    }

    /// Set the pixel offset within the anchor cell (Kitty `X/Y`).
    pub fn with_pixel_offset(mut self, x: i32, y: i32) -> Self {
        self.pixel_offset_x = x;
        self.pixel_offset_y = y;
        self
    }

    /// Set the placement z-index (Kitty `z=`).
    pub fn with_z_index(mut self, z_index: i32) -> Self {
        self.z_index = z_index;
        self
    }

    /// Set the protocol-level image and placement ids (Kitty `i=`/`p=`).
    pub fn with_protocol_ids(
        mut self,
        protocol_image_id: Option<u32>,
        protocol_placement_id: Option<u32>,
    ) -> Self {
        self.protocol_image_id = protocol_image_id;
        self.protocol_placement_id = protocol_placement_id;
        self
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
    pub protocol_image_id: Option<u32>,
    pub protocol_placement_id: Option<u32>,
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

/// A Kitty *virtual* placement (`U=1`): the prototype for images displayed via
/// Unicode placeholder cells. It has no screen anchor — the placeholder cells
/// in the text grid supply the position, so it scrolls, reflows, and is erased
/// exactly as the text carrying it does, with no placement bookkeeping at all.
///
/// It is addressed by the protocol image id (encoded in a placeholder cell's
/// foreground color) and optionally the protocol placement id (encoded in the
/// underline color). `columns` / `rows` are the cell grid the image is split
/// across; each placeholder cell names one tile of that grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualPlacement {
    pub image_id: StoredImageId,
    pub protocol_image_id: u32,
    pub protocol_placement_id: Option<u32>,
    pub columns: usize,
    pub rows: usize,
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
    virtual_placements: Vec<VirtualPlacement>,
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
            virtual_placements: Vec::new(),
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

        // Kitty replacement semantics: a new placement with the same
        // (protocol image id, protocol placement id) in the active buffer
        // replaces the previous one. Only applies when a placement id was
        // explicitly given; un-numbered placements always accumulate.
        if let Some(placement_id) = request.protocol_placement_id {
            let active = self.active;
            let image_id = request.protocol_image_id;
            self.placements.retain(|placement| {
                !(placement.buffer == active
                    && placement.protocol_placement_id == Some(placement_id)
                    && placement.protocol_image_id == image_id)
            });
        }

        // Bound un-numbered `a=p` display commands as well as numbered ones.
        // The two screen buffers have independent lifetimes, so evict only the
        // oldest placement in the active buffer when its live cap is full.
        if self
            .placements
            .iter()
            .filter(|placement| placement.buffer == self.active)
            .count()
            >= MAX_IMAGE_PLACEMENTS_PER_BUFFER
            && let Some(oldest) = self
                .placements
                .iter()
                .position(|placement| placement.buffer == self.active)
        {
            self.placements.remove(oldest);
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
            protocol_image_id: request.protocol_image_id,
            protocol_placement_id: request.protocol_placement_id,
            generation,
            buffer: self.active,
        });
        Some(id)
    }

    /// Create a Kitty virtual placement (`U=1`) for an already-stored image.
    /// Returns `false` when the image is unknown or the extent is empty.
    ///
    /// Replacement follows the same rule as real placements: a new virtual
    /// placement with the same `(protocol image id, protocol placement id)`
    /// replaces the previous one, so a client can resize a placeholder image
    /// without accumulating prototypes. The extent is clamped to
    /// [`MAX_VIRTUAL_EXTENT`] per axis because `c=`/`r=` are untrusted and no
    /// screen bound applies to a placement with no screen position.
    pub fn place_virtual(
        &mut self,
        image_id: StoredImageId,
        protocol_image_id: u32,
        protocol_placement_id: Option<u32>,
        columns: usize,
        rows: usize,
        z_index: i32,
    ) -> bool {
        if !self.store.contains(image_id) || columns == 0 || rows == 0 {
            return false;
        }
        let columns = columns.min(MAX_VIRTUAL_EXTENT);
        let rows = rows.min(MAX_VIRTUAL_EXTENT);

        self.virtual_placements.retain(|existing| {
            !(existing.protocol_image_id == protocol_image_id
                && existing.protocol_placement_id == protocol_placement_id)
        });
        if self.virtual_placements.len() >= MAX_VIRTUAL_PLACEMENTS {
            self.virtual_placements.remove(0);
        }

        self.store.touch(image_id);
        let generation = self.next_generation;
        self.next_generation += 1;
        self.virtual_placements.push(VirtualPlacement {
            image_id,
            protocol_image_id,
            protocol_placement_id,
            columns,
            rows,
            z_index,
            generation,
        });
        true
    }

    pub fn virtual_placements(&self) -> &[VirtualPlacement] {
        &self.virtual_placements
    }

    /// Whether any virtual placement exists. The placeholder scan over the
    /// visible grid is gated on this: with no virtual placements there is
    /// nothing a placeholder cell could resolve to, so the render path does
    /// zero extra work and frames stay byte-identical.
    pub fn has_virtual_placements(&self) -> bool {
        !self.virtual_placements.is_empty()
    }

    /// Resolve the virtual placement a placeholder cell refers to. `placement_id`
    /// comes from the cell's underline color; when it is absent (or zero) the
    /// spec lets the terminal choose any virtual placement of that image, and
    /// the most recently created one is chosen here.
    pub fn find_virtual_placement(
        &self,
        protocol_image_id: u32,
        placement_id: Option<u32>,
    ) -> Option<&VirtualPlacement> {
        self.virtual_placements
            .iter()
            .filter(|candidate| candidate.protocol_image_id == protocol_image_id)
            .filter(|candidate| match placement_id {
                Some(id) => candidate.protocol_placement_id == Some(id),
                None => true,
            })
            .max_by_key(|candidate| candidate.generation)
    }

    /// Resolve a stored image by its protocol-level image id (Kitty `i=`),
    /// preferring the most recently inserted match. Used by `a=p` to display a
    /// previously transmitted image without re-sending pixel data.
    pub fn find_by_protocol_id(&self, protocol_id: u32) -> Option<StoredImageId> {
        self.store
            .iter_ids()
            .filter(|id| {
                self.store
                    .get(*id)
                    .is_some_and(|image| image.protocol_id == Some(protocol_id))
            })
            .max_by_key(|id| self.store.get(*id).map(|image| image.generation))
    }

    pub fn placements(&self) -> &[ImagePlacement] {
        &self.placements
    }

    // -----------------------------------------------------------------------
    // Kitty graphics animation (a=f / a=a / a=c)
    // -----------------------------------------------------------------------

    /// Create or edit an animation frame of a stored image (`a=f`).
    ///
    /// The frame's byte cost is checked against the store's remaining budget
    /// before anything is written, so a frame flood is refused rather than
    /// evicting the images a session is currently showing. Frames and still
    /// images share the one decoded-byte quota.
    pub fn animation_transmit_frame(
        &mut self,
        image_id: StoredImageId,
        update: FrameUpdate<'_>,
    ) -> Result<(u32, bool), FrameError> {
        let budget = self.store.budget_remaining();
        let Some(mut guard) = self.store.frames_mut(image_id) else {
            return Err(FrameError::FrameNotFound);
        };
        let (width, height) = guard.canvas_dimensions();
        let canvas_bytes = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        // Editing an existing frame rewrites pixels in place and so costs
        // nothing. The first `r=1` edit captures one root canvas; the first new
        // frame costs that root plus the appended canvas. Invalid initial edit
        // numbers are rejected later without mutation and therefore cost zero.
        let added_bytes = match (guard.frames().is_empty(), update.edit_frame) {
            (true, Some(1)) => canvas_bytes,
            (true, Some(_)) => 0,
            (_, Some(_)) => 0,
            (_, None) => guard.frames().added_bytes_for(canvas_bytes),
        };
        if added_bytes > budget {
            return Err(FrameError::Quota);
        }
        let canvas = guard.canvas().to_vec();
        let frame = guard
            .frames_mut()
            .transmit_frame(&canvas, width, height, update)?;
        // A frame command can land on the frame currently displayed (r= edit of
        // the current frame), so republish before dropping the guard.
        let changed = guard.publish_current_frame();
        Ok((frame, changed))
    }

    /// Compose a rectangle of one animation frame onto another (`a=c`).
    pub fn animation_compose(
        &mut self,
        image_id: StoredImageId,
        composition: FrameComposition,
    ) -> Result<bool, FrameError> {
        let Some(mut guard) = self.store.frames_mut(image_id) else {
            return Err(FrameError::FrameNotFound);
        };
        let (width, height) = guard.canvas_dimensions();
        guard.frames_mut().compose(width, height, composition)?;
        Ok(guard.publish_current_frame())
    }

    /// Apply an animation control command (`a=a`). Returns whether the
    /// displayed pixels changed.
    pub fn animation_control(
        &mut self,
        image_id: StoredImageId,
        control: AnimationControl,
    ) -> Result<bool, FrameError> {
        let Some(mut guard) = self.store.frames_mut(image_id) else {
            return Err(FrameError::FrameNotFound);
        };
        if guard.frames().is_empty() {
            return Err(FrameError::FrameNotFound);
        }
        // Validate every referenced frame before applying any of the command's
        // other fields. An invalid compound control command is rejected as a
        // unit instead of changing loop or playback state on its way to the
        // missing-frame error.
        if control
            .gap_frame
            .is_some_and(|frame| guard.frames().gap_ms(frame).is_none())
            || control
                .current_frame
                .is_some_and(|frame| guard.frames().gap_ms(frame).is_none())
        {
            return Err(FrameError::FrameNotFound);
        }
        if let Some(loops) = control.loops {
            guard.frames_mut().set_loops(loops);
        }
        if let Some(frame) = control.gap_frame {
            let gap = control.gap_ms.unwrap_or(0);
            guard.frames_mut().set_gap(frame, gap)?;
        }
        if let Some(state) = control.state {
            guard.frames_mut().set_state(state);
        }
        let mut changed = false;
        if let Some(frame) = control.current_frame {
            changed = guard.frames_mut().set_current(frame)?;
        }
        if changed {
            guard.publish_current_frame();
        }
        Ok(changed)
    }

    /// Delete one animation frame (`d=f` / `d=F`). Frame numbers are 1-based,
    /// default to the root, and clamp to the last existing frame. Deleting the
    /// root promotes frame 2. If no extra frame exists, lowercase is a no-op;
    /// uppercase removes the entire image and all of its placements.
    pub fn animation_delete_frame(
        &mut self,
        image_id: StoredImageId,
        frame: u32,
        free_when_exhausted: bool,
    ) -> Result<bool, FrameError> {
        let frame_count = self
            .store
            .get(image_id)
            .ok_or(FrameError::FrameNotFound)?
            .frames
            .frame_count();
        if frame_count <= 1 {
            if !free_when_exhausted {
                return Ok(false);
            }
            self.placements
                .retain(|placement| placement.image_id != image_id);
            self.virtual_placements
                .retain(|placement| placement.image_id != image_id);
            return Ok(self.store.remove(image_id).is_some());
        }

        let mut guard = self
            .store
            .frames_mut(image_id)
            .ok_or(FrameError::FrameNotFound)?;
        let (removed, displayed_changed) = guard.frames_mut().delete_frame(frame);
        if displayed_changed {
            guard.publish_current_frame();
        }
        Ok(removed)
    }

    /// Whether any stored image holds animation frames. Every animation code
    /// path in the render loop is gated on this, so a session with no animated
    /// image does no animation work at all.
    pub fn has_animations(&self) -> bool {
        self.store.has_animations()
    }

    /// Clock reading at which some animation referenced by `visible` needs its
    /// next frame, or `None` when nothing visible is animating. `None` is the
    /// answer for a still terminal, an animation that is stopped, and an
    /// animated image no visible placement refers to - the render loop turns
    /// `None` into "schedule no wake".
    pub fn next_animation_deadline_ms(&self, visible: &[VisiblePlacement]) -> Option<u64> {
        if !self.store.has_animations() {
            return None;
        }
        visible
            .iter()
            .filter(|placement| self.store.animated_ids().contains(&placement.image_id))
            .filter_map(|placement| {
                self.store
                    .get(placement.image_id)
                    .and_then(|image| image.frames.next_deadline_ms())
            })
            .min()
    }

    /// Advance every animation referenced by `visible` to the frame due at
    /// `now_ms`, republishing displayed pixels. Returns whether any image
    /// changed, which the caller treats as "this frame must be repainted".
    ///
    /// Only visible placements are advanced: an animation nothing shows holds
    /// its position and resumes from the current clock when it becomes visible
    /// again, rather than burning frames off-screen.
    pub fn advance_animations(&mut self, now_ms: u64, visible: &[VisiblePlacement]) -> bool {
        if !self.store.has_animations() {
            return false;
        }
        let mut targets: Vec<StoredImageId> = visible
            .iter()
            .map(|placement| placement.image_id)
            .filter(|id| self.store.animated_ids().contains(id))
            .collect();
        targets.sort_unstable();
        targets.dedup();
        let mut changed = false;
        for id in targets {
            let Some(mut guard) = self.store.frames_mut(id) else {
                continue;
            };
            if guard.frames_mut().advance(now_ms) {
                changed |= guard.publish_current_frame();
            }
        }
        changed
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
        self.virtual_placements.clear();
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

    /// `d=i` — delete placements referencing `image_id` (Kitty protocol id) in
    /// the active buffer. If `placement_id` is `Some`, delete only the placement
    /// with that protocol-level placement id (Kitty `p=`); otherwise delete all
    /// placements of the image.
    ///
    /// Virtual (Unicode-placeholder) placements are deleted here too: `d=i`/`d=I`
    /// are among the specifiers the graphics protocol says DO affect virtual
    /// placements. The specifiers that address a screen location — `a`, `c`,
    /// `p` and their capital forms — deliberately leave them alone, because a
    /// virtual placement has no screen location to intersect.
    pub fn delete_by_image_id(&mut self, image_id: u32, placement_id: Option<u32>) {
        self.virtual_placements.retain(|candidate| {
            if candidate.protocol_image_id != image_id {
                return true;
            }
            match placement_id {
                Some(pid) => candidate.protocol_placement_id != Some(pid),
                None => false,
            }
        });
        let active = self.active;
        self.placements.retain(|p| {
            if p.buffer != active {
                return true;
            }
            if p.protocol_image_id != Some(image_id) {
                return true;
            }
            // Keep placements whose protocol placement id differs from the
            // requested one; with no placement id all matches are removed.
            match placement_id {
                Some(pid) => p.protocol_placement_id != Some(pid),
                None => false,
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
    /// Virtual placements count as references: an image kept alive only as a
    /// Unicode-placeholder prototype must survive a `d=A` that clears the
    /// screen's real placements, or every placeholder on screen would blank.
    fn gc_unreferenced_images(&mut self) {
        let referenced: std::collections::HashSet<StoredImageId> = self
            .placements
            .iter()
            .map(|p| p.image_id)
            .chain(self.virtual_placements.iter().map(|p| p.image_id))
            .collect();
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

    /// Scroll a TOP-ANCHORED region (top row 0) up by `count`, feeding the rows
    /// that leave the top into scrollback exactly as [`Self::scroll_full_up`]
    /// does, while leaving the footer below `bottom` fixed as
    /// [`Self::scroll_region_up`] would. Used by the linefeed-at-region-bottom
    /// path when a full-screen TUI reserves a bottom input composer via a
    /// top-anchored DECSTBM region: the content above the margin is real
    /// history, so placements scrolling off the top are retained into
    /// scrollback rather than dropped.
    pub fn scroll_region_up_into_scrollback(
        &mut self,
        bottom: usize,
        count: usize,
        scrollback_rows: usize,
    ) {
        self.shift_rows(0, Some(bottom as isize), -(count as isize), true);
        self.evict_above_scrollback(scrollback_rows);
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
        cell_height_px: u32,
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
            // C21: a placement partially scrolled above the viewport top must
            // show its LOWER portion, not re-anchor its top rows at row 0.
            // Advance the source rect by the clipped pixel rows (placements
            // render 1:1, so one display row == one cell height of source
            // pixels). `height == 0` means "to the image bottom" and needs no
            // reduction — the advanced `y` shrinks it implicitly.
            let clipped_rows = usize::try_from(-projected_row).unwrap_or(0);
            let mut source = placement.source;
            if clipped_rows > 0 {
                let clip_px = (clipped_rows as u32).saturating_mul(cell_height_px);
                source.y = source.y.saturating_add(clip_px);
                if source.height != 0 {
                    source.height = source.height.saturating_sub(clip_px);
                }
            }
            let row = projected_row.max(0) as usize;
            visible.push(VisiblePlacement {
                id: placement.id,
                image_id: placement.image_id,
                protocol: placement.protocol,
                row,
                column: placement.anchor.column,
                source,
                display_columns: placement
                    .display_columns
                    .min(viewport_columns - placement.anchor.column),
                display_rows: placement
                    .display_rows
                    .saturating_sub(clipped_rows)
                    .min(viewport_rows - row),
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
            if let Some(bottom) = bottom
                && placement.anchor.row > bottom
            {
                continue;
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
        // Sibling path: a store eviction invalidates virtual placements exactly
        // as it invalidates real ones — a prototype pointing at freed pixels
        // would resolve every placeholder cell to a missing image.
        self.virtual_placements
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

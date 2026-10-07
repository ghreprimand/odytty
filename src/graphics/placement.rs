// SPDX-License-Identifier: GPL-3.0-only
//! Cell-anchored graphics placement scene.
//!
//! Placements are terminal state, not renderer state: they scroll with text,
//! clear with terminal erase/reset operations, and stay isolated between the
//! primary and alternate buffers. Coordinates are physical cell anchors. G2.1
//! deliberately avoids protocol decoding; it stores decoded-image records and
//! placement records, and exposes validity gates for Kitty APC / Sixel DCS
//! framing without retaining the raw payload bytes.

use super::frames::{AnimationControl, FrameComposition, FrameError, FrameUpdate};
use super::store::{ImageInsert, ImageStore, ImageStoreError, ImageStoreLimits, StoredImageId};

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

#[derive(Debug, Clone)]
pub struct ImageScene {
    store: ImageStore,
    placements: Vec<ImagePlacement>,
    virtual_placements: Vec<VirtualPlacement>,
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
            next_placement_id: 1,
            next_generation: 1,
            active: ScreenBuffer::Primary,
        }
    }

    /// Continue image and placement counters past `previous`, so a scene
    /// rebuilt on snapshot restore never reissues an id or generation the old
    /// scene handed out (see [`ImageStore::continue_counters_from`]).
    pub fn continue_counters_from(&mut self, previous: &ImageScene) {
        self.store.continue_counters_from(&previous.store);
        self.next_placement_id = self.next_placement_id.max(previous.next_placement_id);
        self.next_generation = self.next_generation.max(previous.next_generation);
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
        self.insert_rgba_numbered(protocol_id, None, width, height, rgba)
    }

    /// Insert an image that may carry a client-chosen image number (Kitty
    /// `I=`). Shares the eviction bookkeeping with [`ImageScene::insert_rgba`]
    /// rather than duplicating it, so a numbered image drops its placements on
    /// eviction exactly as an unnumbered one does.
    pub fn insert_rgba_numbered(
        &mut self,
        protocol_id: Option<u32>,
        protocol_number: Option<u32>,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<ImageInsert, ImageStoreError> {
        let inserted =
            self.store
                .insert_rgba_numbered(protocol_id, protocol_number, width, height, rgba)?;
        self.remove_placements_for_images(&inserted.evicted);
        // Same-id replacement drops the old image's placements, real and
        // virtual, through the same path as eviction.
        self.remove_placements_for_images(&inserted.replaced);
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

    /// Resolve a stored image by its client-chosen image number (Kitty `I=`),
    /// taking the newest match.
    ///
    /// Newest-match is the protocol's rule, not a tie-break of convenience:
    /// numbers are explicitly not unique, and transmitting with a number
    /// creates a new image rather than replacing the previous one, so several
    /// images can legitimately share a number at once. Resolution by generation
    /// — the same ordering [`ImageScene::find_by_protocol_id`] uses — means a
    /// client's follow-up commands act on the image it most recently sent.
    pub fn find_by_image_number(&self, protocol_number: u32) -> Option<StoredImageId> {
        self.store
            .iter_ids()
            .filter(|id| {
                self.store
                    .get(*id)
                    .is_some_and(|image| image.protocol_number == Some(protocol_number))
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

    /// Validity gate for a Kitty APC payload: true when it is a graphics
    /// command (`G` introducer). The boolean is load-bearing (it gates whether
    /// the caller marks the scene dirty and dispatches the command); the payload
    /// bytes themselves are not retained, since no production consumer ever read
    /// them back.
    pub fn accepts_kitty_apc(&self, payload: &[u8]) -> bool {
        payload.starts_with(b"G")
    }

    /// Validity gate for a Sixel DCS: true when the framing is well-formed (a
    /// `q` introducer precedes the payload). As with [`Self::accepts_kitty_apc`]
    /// the boolean is load-bearing while the raw bytes are not retained.
    pub fn accepts_sixel_dcs(&self, raw_body: &[u8], payload_start: usize) -> bool {
        payload_start <= raw_body.len() && raw_body[..payload_start].contains(&b'q')
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

    /// RIS discards both screens, including a saved primary screen that
    /// would otherwise reappear on leaving the alternate screen, so every
    /// placement goes. Stored image data stays available to later commands.
    pub fn hard_reset(&mut self) {
        self.placements.clear();
        self.virtual_placements.clear();
        self.active = ScreenBuffer::Primary;
    }

    // -----------------------------------------------------------------------
    // Kitty graphics protocol delete actions (a=d)
    // -----------------------------------------------------------------------

    /// `d=a`: delete the active buffer's placements that reach the screen.
    /// Placements wholly in scrollback history are not visible on screen and
    /// stay, as in kitty.
    pub fn delete_all_placements(&mut self) {
        self.delete_active_where(reaches_screen);
    }

    /// `d=A`: like `d=a`, then free the images those placements showed when
    /// nothing else references them. Images that were transmitted but never
    /// placed, or are placed only elsewhere, are untouched.
    pub fn delete_all_placements_and_free(&mut self) {
        let affected = self.delete_active_where(reaches_screen);
        self.free_unreferenced(affected);
    }

    /// `d=i`: delete placements referencing `image_id` (Kitty protocol id) in
    /// the active buffer. If `placement_id` is `Some`, delete only the placement
    /// with that protocol-level placement id (Kitty `p=`); otherwise delete all
    /// placements of the image.
    ///
    /// Virtual (Unicode-placeholder) placements are deleted here too: `d=i`/`d=I`
    /// are among the specifiers the graphics protocol says DO affect virtual
    /// placements. The specifiers that address a screen location (`a`, `c`,
    /// `p` and their capital forms) deliberately leave them alone, because a
    /// virtual placement has no screen location to intersect.
    pub fn delete_by_image_id(&mut self, image_id: u32, placement_id: Option<u32>) {
        self.delete_by_image_id_affected(image_id, placement_id);
    }

    fn delete_by_image_id_affected(
        &mut self,
        image_id: u32,
        placement_id: Option<u32>,
    ) -> Vec<StoredImageId> {
        let matches_placement = |id: Option<u32>| match placement_id {
            Some(pid) => id == Some(pid),
            None => true,
        };
        let mut affected = Vec::new();
        self.virtual_placements.retain(|candidate| {
            let matched = candidate.protocol_image_id == image_id
                && matches_placement(candidate.protocol_placement_id);
            if matched {
                affected.push(candidate.image_id);
            }
            !matched
        });
        affected.extend(self.delete_active_where(|placement| {
            placement.protocol_image_id == Some(image_id)
                && matches_placement(placement.protocol_placement_id)
        }));
        affected
    }

    /// `d=I`: like `d=i`, then free the image's data when no placement
    /// references it. Without a placement id the image is the target, so it is
    /// freed even when it had no placements; with one, only an image that lost
    /// that placement is a candidate. Other images are untouched.
    pub fn delete_by_image_id_and_free(&mut self, image_id: u32, placement_id: Option<u32>) {
        let mut affected = self.delete_by_image_id_affected(image_id, placement_id);
        if placement_id.is_none() {
            affected.extend(self.store.iter_ids().filter(|id| {
                self.store
                    .get(*id)
                    .is_some_and(|image| image.protocol_id == Some(image_id))
            }));
        }
        self.free_unreferenced(affected);
    }

    /// `d=c` / `d=C`: delete the active buffer's placements that cover the
    /// cursor cell (`row`, `col`), not only those anchored there.
    pub fn delete_at_cursor(&mut self, row: usize, col: usize, free_images: bool) {
        self.delete_at_position(row, col, free_images);
    }

    /// `d=p` / `d=P`: delete the active buffer's placements that cover cell
    /// (`row`, `col`). The capital form then frees the images those placements
    /// showed when nothing else references them.
    pub fn delete_at_position(&mut self, row: usize, col: usize, free_images: bool) {
        let affected = self.delete_active_where(|placement| covers_cell(placement, row, col));
        if free_images {
            self.free_unreferenced(affected);
        }
    }

    /// Remove the active buffer's placements matching `matches`, returning
    /// the image each removed placement showed.
    fn delete_active_where(
        &mut self,
        matches: impl Fn(&ImagePlacement) -> bool,
    ) -> Vec<StoredImageId> {
        let active = self.active;
        let mut affected = Vec::new();
        self.placements.retain(|placement| {
            if placement.buffer != active || !matches(placement) {
                return true;
            }
            affected.push(placement.image_id);
            false
        });
        affected
    }

    /// Remove each of `candidates` from the store when no placement references
    /// it. Virtual placements count as references: an image kept alive only as
    /// a Unicode-placeholder prototype must survive, or every placeholder on
    /// screen would blank.
    fn free_unreferenced(&mut self, candidates: Vec<StoredImageId>) {
        if candidates.is_empty() {
            return;
        }
        let referenced: std::collections::HashSet<StoredImageId> = self
            .placements
            .iter()
            .map(|p| p.image_id)
            .chain(self.virtual_placements.iter().map(|p| p.image_id))
            .collect();
        for id in candidates {
            if !referenced.contains(&id) {
                self.store.remove(id);
            }
        }
    }

    /// Full-screen scroll up: every active placement moves, including those
    /// already in scrollback history, so history ages and evicts uniformly.
    pub fn scroll_full_up(&mut self, count: usize, scrollback_rows: usize) {
        self.shift_into_history(None, -(count as isize));
        self.evict_above_scrollback(scrollback_rows);
    }

    pub fn scroll_region_up(&mut self, top: usize, bottom: usize, count: usize) {
        self.scroll_region(top as isize, bottom as isize, -(count as isize));
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
        self.shift_into_history(Some(bottom as isize), -(count as isize));
        self.evict_above_scrollback(scrollback_rows);
    }

    pub fn scroll_region_down(&mut self, top: usize, bottom: usize, count: usize) {
        self.scroll_region(top as isize, bottom as isize, count as isize);
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
            // ED0 erases the cursor row from the cursor on plus every later
            // row; ED1 every earlier row plus the cursor row through the
            // cursor. Rows are compared signed, so a placement wholly in
            // scrollback history never overlaps the screen.
            let row = cursor_row as isize;
            match mode {
                0 => {
                    !(overlaps(placement, row..row + 1, cursor_column..columns)
                        || overlaps(placement, row + 1..rows as isize, 0..columns))
                }
                1 => {
                    !(overlaps(placement, 0..row, 0..columns)
                        || overlaps(placement, row..row + 1, 0..cursor_column.saturating_add(1)))
                }
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

    /// Move active placements anchored at or above `bottom` (all of them when
    /// `None`) by `delta` rows, into scrollback history for a negative delta.
    /// Placements already in history move too; a footer below `bottom` stays.
    fn shift_into_history(&mut self, bottom: Option<isize>, delta: isize) {
        let active = self.active;
        for placement in self
            .placements
            .iter_mut()
            .filter(|placement| placement.buffer == active)
        {
            if bottom.is_some_and(|bottom| placement.anchor.row > bottom) {
                continue;
            }
            placement.anchor.row += delta;
        }
    }

    /// Scroll the region `top..=bottom` by `delta` rows. Placements wholly
    /// outside the region, such as a header or footer, neither move nor go.
    /// A placement inside it moves and is removed once any part leaves the
    /// region; one crossing a margin before the scroll is removed, because the
    /// rows it covered inside the region moved away beneath it.
    fn scroll_region(&mut self, top: isize, bottom: isize, delta: isize) {
        let active = self.active;
        self.placements.retain_mut(|placement| {
            if placement.buffer != active {
                return true;
            }
            let (start, end) = row_span(placement);
            if end < top || start > bottom {
                return true;
            }
            if start < top || end > bottom {
                return false;
            }
            placement.anchor.row += delta;
            let (start, end) = row_span(placement);
            start >= top && end <= bottom
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

/// First and last row a placement covers, signed so scrollback history is
/// negative. A zero-row placement is treated as covering its anchor row.
fn row_span(placement: &ImagePlacement) -> (isize, isize) {
    let start = placement.anchor.row;
    let rows = isize::try_from(placement.display_rows.max(1)).unwrap_or(isize::MAX);
    (start, start.saturating_add(rows - 1))
}

/// Whether `placement` covers any cell in `rows` x `columns`.
fn overlaps(
    placement: &ImagePlacement,
    rows: std::ops::Range<isize>,
    columns: std::ops::Range<usize>,
) -> bool {
    let (start, end) = row_span(placement);
    let column_end = placement
        .anchor
        .column
        .saturating_add(placement.display_columns);
    start < rows.end
        && end >= rows.start
        && placement.anchor.column < columns.end
        && column_end > columns.start
}

/// Whether `placement` covers screen cell (`row`, `col`).
fn covers_cell(placement: &ImagePlacement, row: usize, col: usize) -> bool {
    let row = isize::try_from(row).unwrap_or(isize::MAX);
    overlaps(
        placement,
        row..row.saturating_add(1),
        col..col.saturating_add(1),
    )
}

/// Whether any row of `placement` is on the screen or below it, rather than
/// wholly in scrollback history.
fn reaches_screen(placement: &ImagePlacement) -> bool {
    row_span(placement).1 >= 0
}

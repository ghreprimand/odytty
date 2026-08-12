// SPDX-License-Identifier: GPL-3.0-only
//! Animation frames for stored images: frame storage, composition, and
//! playback timing.
//!
//! An animated image is one stored image with a list of full-canvas frames.
//! Frame 1 (the *root* frame) is a copy of the image's transmitted pixels,
//! taken the first time a frame command arrives; later frames are built by
//! compositing transmitted rectangles onto a background canvas (a previous
//! frame, or a solid color). Every frame therefore holds a complete canvas, so
//! displaying a frame is a byte copy and never a replay of stored operations.
//!
//! Playback is a pure function of the frame list plus a monotonic millisecond
//! clock supplied by the caller: this module never reads a clock itself, which
//! keeps the terminal core clockless and the timing behavior directly testable.
//! The render loop asks for [`ImageFrames::next_deadline_ms`] and calls
//! [`ImageFrames::advance`] when that deadline arrives; a stopped animation, an
//! animation with a single frame, and a `RunLoading` animation parked on its
//! last frame all report no deadline, so an idle terminal schedules no wake.

/// Live frames per image. Bounds the per-image frame list independently of the
/// byte budget so a flood of tiny frames cannot grow bookkeeping without end.
pub const MAX_FRAMES_PER_IMAGE: usize = 64;

/// Gap applied to a newly created frame when the client sends no usable `z=`
/// (absent, or `z=0` which the protocol says to ignore). Matches the kitty
/// protocol's documented 40ms frame default.
pub const DEFAULT_FRAME_GAP_MS: i32 = 40;

/// Floor on a displayed frame's gap. Client gaps are untrusted and a 1ms
/// animation would pin the render loop at its frame cap for as long as the
/// animation runs; clamping here keeps a hostile or careless gap from turning
/// the event-driven loop into a spin.
pub const MIN_PLAYBACK_GAP_MS: u64 = 10;

/// Ceiling on a displayed frame's gap (one minute). Bounds how far ahead a
/// wake can be scheduled from one untrusted value.
pub const MAX_PLAYBACK_GAP_MS: u64 = 60_000;

/// Playback state of one animation, set by the `a=a` control command's `s=` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationState {
    /// `s=1` - stopped. The current frame stays displayed and no wake is
    /// scheduled. Also the state of every image before any control command.
    #[default]
    Stopped,
    /// `s=2` - running, but on reaching the last frame the terminal waits for
    /// more frames to arrive instead of looping.
    RunLoading,
    /// `s=3` - running, looping back to the first frame after the last, subject
    /// to the loop count.
    Running,
}

/// Why a frame command was rejected. Each maps to one kitty protocol error
/// code at the protocol boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The addressed frame number does not exist (`ENOENT`).
    FrameNotFound,
    /// A rectangle falls outside the image canvas, or the payload length does
    /// not match the declared rectangle (`EINVAL`).
    OutOfBounds,
    /// Source and destination frames are the same and the rectangles overlap
    /// (`EINVAL`, required by the protocol).
    Overlap,
    /// The per-image frame cap is reached (`ENOSPC`).
    TooManyFrames,
    /// The new frame does not fit the image store's byte budget (`ENOSPC`).
    Quota,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnimationFrame {
    /// Full canvas pixels for this frame, always `canvas_w * canvas_h * 4`.
    rgba: Vec<u8>,
    /// Gap in milliseconds to the next frame. Negative means *gapless*: the
    /// frame is never shown to the user, it exists only as base data for the
    /// frames composed from it.
    gap_ms: i32,
}

/// One image's animation: its frames plus playback position and state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageFrames {
    frames: Vec<AnimationFrame>,
    /// Zero-based index of the frame currently displayed.
    current: usize,
    state: AnimationState,
    /// Remaining loops after the current one, or `None` for "loop forever".
    /// `v=1` (the protocol default) is infinite; `v=n` is `n - 1` loops.
    loops_remaining: Option<u32>,
    /// Clock reading at which the current frame started being displayed.
    frame_started_ms: Option<u64>,
}

/// A rectangle of transmitted frame pixels plus the composition parameters that
/// place it onto a background canvas (`a=f`).
#[derive(Debug, Clone, Copy)]
pub struct FrameUpdate<'a> {
    /// RGBA8 pixels of the rectangle, exactly `width * height * 4` bytes.
    pub data: &'a [u8],
    /// Destination rectangle within the image canvas (`x=`, `y=`, `s=`, `v=`).
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// 1-based frame whose pixels are the background canvas (`c=`). `None`
    /// means the canvas is filled with [`FrameUpdate::background`].
    pub base_frame: Option<u32>,
    /// 1-based frame to edit (`r=`). `None` creates a new frame; when set, the
    /// existing frame's own pixels are the background canvas.
    pub edit_frame: Option<u32>,
    /// Gap for the frame (`z=`). `None` or `Some(0)` means the default gap for
    /// a new frame, and leaves an edited frame's gap unchanged.
    pub gap_ms: Option<i32>,
    /// `X=1` - replace destination pixels instead of alpha-blending onto them.
    pub overwrite: bool,
    /// `Y=` - background color as 32-bit RGBA, used when no base frame is given.
    pub background: u32,
}

/// Parameters of a frame-composition command (`a=c`).
#[derive(Debug, Clone, Copy)]
pub struct FrameComposition {
    /// 1-based source frame (`r=`), whose pixels are read.
    pub source_frame: u32,
    /// 1-based destination frame (`c=`), whose pixels are written.
    pub destination_frame: u32,
    /// Rectangle size in pixels, shared by source and destination (`w=`, `h=`).
    pub width: u32,
    pub height: u32,
    /// Destination rectangle origin (`x=`, `y=`).
    pub destination_x: u32,
    pub destination_y: u32,
    /// Source rectangle origin (`X=`, `Y=`).
    pub source_x: u32,
    pub source_y: u32,
    /// `C=1` - replace destination pixels instead of alpha-blending.
    pub overwrite: bool,
}

/// Parameters of an animation control command (`a=a`). Every field is optional
/// because one command may set any subset of them. The order they are applied
/// in is fixed: loop count and per-frame gap are properties of the animation,
/// the state change re-phases the frame clock, and a requested current frame is
/// applied last so `a=a,c=N,s=3` starts running from frame `N`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnimationControl {
    /// `s=` - new playback state.
    pub state: Option<AnimationState>,
    /// `c=` - 1-based frame to make current.
    pub current_frame: Option<u32>,
    /// `r=` - 1-based frame whose gap `gap_ms` applies to.
    pub gap_frame: Option<u32>,
    /// `z=` - gap in milliseconds for `gap_frame`.
    pub gap_ms: Option<i32>,
    /// `v=` - loop count.
    pub loops: Option<u32>,
}

impl ImageFrames {
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn state(&self) -> AnimationState {
        self.state
    }

    /// 1-based number of the frame currently displayed, or `None` when the
    /// image has no frames.
    pub fn current_frame(&self) -> Option<u32> {
        (!self.frames.is_empty()).then(|| self.current as u32 + 1)
    }

    /// Pixels of the currently displayed frame, or `None` when the image has no
    /// frames (in which case its transmitted pixels are what is displayed).
    pub fn current_rgba(&self) -> Option<&[u8]> {
        self.frames.get(self.current).map(|frame| &frame.rgba[..])
    }

    /// Bytes held by the frame list. Counted into the image store's single
    /// decoded-byte budget, so frames and still images compete for one quota.
    pub fn bytes(&self) -> usize {
        self.frames
            .iter()
            .map(|frame| frame.rgba.len())
            .fold(0usize, |total, bytes| total.saturating_add(bytes))
    }

    /// Gap of a 1-based frame, or `None` when it does not exist.
    pub fn gap_ms(&self, frame: u32) -> Option<i32> {
        self.frame_index(frame)
            .and_then(|index| self.frames.get(index))
            .map(|frame| frame.gap_ms)
    }

    /// Drop every frame and reset playback (`a=d, d=f`). The image's own
    /// transmitted pixels are left untouched, so the still image survives its
    /// animation.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.current = 0;
        self.state = AnimationState::Stopped;
        self.loops_remaining = None;
        self.frame_started_ms = None;
    }

    /// Byte cost of adding one frame for a `canvas_bytes`-sized canvas,
    /// including the root-frame copy taken on the first frame command. The
    /// caller checks this against the store budget *before* mutating, so a
    /// frame that does not fit is rejected instead of evicting live images.
    pub fn added_bytes_for(&self, canvas_bytes: usize) -> usize {
        if self.frames.is_empty() {
            canvas_bytes.saturating_mul(2)
        } else {
            canvas_bytes
        }
    }

    /// Create or edit a frame from transmitted pixels. Returns the 1-based
    /// number of the frame written.
    pub fn transmit_frame(
        &mut self,
        canvas: &[u8],
        canvas_width: u32,
        canvas_height: u32,
        update: FrameUpdate<'_>,
    ) -> Result<u32, FrameError> {
        let canvas_bytes = canvas_bytes(canvas_width, canvas_height)?;
        if canvas.len() != canvas_bytes {
            return Err(FrameError::OutOfBounds);
        }
        validate_rect(
            canvas_width,
            canvas_height,
            update.x,
            update.y,
            update.width,
            update.height,
        )?;
        let expected = (update.width as usize)
            .checked_mul(update.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(FrameError::OutOfBounds)?;
        if update.data.len() != expected {
            return Err(FrameError::OutOfBounds);
        }

        // Validate every frame reference before capturing the root frame. A
        // rejected command must not turn a still image into an animation or
        // consume quota as a side effect. On the first valid create, `c=1`
        // refers to the root frame that this command is about to capture. The
        // same applies to `r=1`: clients may edit the root before appending any
        // later frame.
        if let Some(edited) = update.edit_frame {
            let exists = if self.frames.is_empty() {
                edited == 1
            } else {
                self.frame_index(edited).is_some()
            };
            if !exists {
                return Err(FrameError::FrameNotFound);
            }
        } else {
            if self.frames.len() >= MAX_FRAMES_PER_IMAGE {
                return Err(FrameError::TooManyFrames);
            }
            if let Some(base) = update.base_frame {
                let exists = if self.frames.is_empty() {
                    base == 1
                } else {
                    self.frame_index(base).is_some()
                };
                if !exists {
                    return Err(FrameError::FrameNotFound);
                }
            }
        }

        self.ensure_root(canvas);

        if let Some(edited) = update.edit_frame {
            let index = self.frame_index(edited).ok_or(FrameError::FrameNotFound)?;
            if index >= self.frames.len() {
                return Err(FrameError::FrameNotFound);
            }
            let mut pixels = std::mem::take(&mut self.frames[index].rgba);
            blit(
                &mut pixels,
                canvas_width,
                update.x,
                update.y,
                update.width,
                update.height,
                update.data,
                update.overwrite,
            );
            self.frames[index].rgba = pixels;
            if let Some(gap) = update.gap_ms.filter(|gap| *gap != 0) {
                self.frames[index].gap_ms = gap;
            }
            return Ok(index as u32 + 1);
        }

        let mut pixels = match update.base_frame {
            Some(base) => {
                let index = self.frame_index(base).ok_or(FrameError::FrameNotFound)?;
                self.frames
                    .get(index)
                    .ok_or(FrameError::FrameNotFound)?
                    .rgba
                    .clone()
            }
            None => background_canvas(canvas_bytes, update.background),
        };
        blit(
            &mut pixels,
            canvas_width,
            update.x,
            update.y,
            update.width,
            update.height,
            update.data,
            update.overwrite,
        );
        let gap_ms = match update.gap_ms {
            Some(0) | None => DEFAULT_FRAME_GAP_MS,
            Some(gap) => gap,
        };
        self.frames.push(AnimationFrame {
            rgba: pixels,
            gap_ms,
        });
        Ok(self.frames.len() as u32)
    }

    /// Compose a rectangle of one frame onto another (`a=c`).
    pub fn compose(
        &mut self,
        canvas_width: u32,
        canvas_height: u32,
        composition: FrameComposition,
    ) -> Result<(), FrameError> {
        let source = self
            .frame_index(composition.source_frame)
            .ok_or(FrameError::FrameNotFound)?;
        let destination = self
            .frame_index(composition.destination_frame)
            .ok_or(FrameError::FrameNotFound)?;
        if source >= self.frames.len() || destination >= self.frames.len() {
            return Err(FrameError::FrameNotFound);
        }
        let width = if composition.width == 0 {
            canvas_width
        } else {
            composition.width
        };
        let height = if composition.height == 0 {
            canvas_height
        } else {
            composition.height
        };
        validate_rect(
            canvas_width,
            canvas_height,
            composition.destination_x,
            composition.destination_y,
            width,
            height,
        )?;
        validate_rect(
            canvas_width,
            canvas_height,
            composition.source_x,
            composition.source_y,
            width,
            height,
        )?;
        if source == destination
            && rects_overlap(
                composition.source_x,
                composition.source_y,
                composition.destination_x,
                composition.destination_y,
                width,
                height,
            )
        {
            return Err(FrameError::Overlap);
        }

        let patch = extract_rect(
            &self.frames[source].rgba,
            canvas_width,
            composition.source_x,
            composition.source_y,
            width,
            height,
        );
        let mut pixels = std::mem::take(&mut self.frames[destination].rgba);
        blit(
            &mut pixels,
            canvas_width,
            composition.destination_x,
            composition.destination_y,
            width,
            height,
            &patch,
            composition.overwrite,
        );
        self.frames[destination].rgba = pixels;
        Ok(())
    }

    /// Set a 1-based frame's gap (`a=a` with `r=`/`z=`). A zero gap is ignored,
    /// as the protocol specifies.
    pub fn set_gap(&mut self, frame: u32, gap_ms: i32) -> Result<(), FrameError> {
        let index = self.frame_index(frame).ok_or(FrameError::FrameNotFound)?;
        let entry = self
            .frames
            .get_mut(index)
            .ok_or(FrameError::FrameNotFound)?;
        if gap_ms != 0 {
            entry.gap_ms = gap_ms;
        }
        Ok(())
    }

    /// Make a 1-based frame the current frame (`a=a` with `c=`). Returns
    /// whether the displayed frame changed. The frame clock is cleared rather
    /// than set, so the next playback tick re-phases from its own reading -
    /// which is what lets the protocol layer stay clockless.
    pub fn set_current(&mut self, frame: u32) -> Result<bool, FrameError> {
        let index = self.frame_index(frame).ok_or(FrameError::FrameNotFound)?;
        if index >= self.frames.len() {
            return Err(FrameError::FrameNotFound);
        }
        let changed = index != self.current;
        self.current = index;
        self.frame_started_ms = None;
        Ok(changed)
    }

    /// Apply a playback state (`a=a` with `s=`). Stopping resets the loop
    /// counter, as the protocol requires. Starting clears the frame clock so
    /// the first playback tick phases the animation from its own reading; the
    /// protocol layer therefore never needs a clock of its own.
    pub fn set_state(&mut self, state: AnimationState) {
        self.state = state;
        self.frame_started_ms = None;
        if state == AnimationState::Stopped {
            self.loops_remaining = None;
        }
    }

    /// Apply a loop count (`a=a` with `v=`). `v=0` is ignored, `v=1` is
    /// infinite, and `v=n` plays `n - 1` further loops.
    pub fn set_loops(&mut self, loops: u32) {
        match loops {
            0 => {}
            1 => self.loops_remaining = None,
            count => self.loops_remaining = Some(count.saturating_sub(1)),
        }
    }

    /// Whether playback is running and can still reach another frame.
    pub fn is_animating(&self) -> bool {
        if self.frames.len() < 2 {
            return false;
        }
        match self.state {
            AnimationState::Stopped => false,
            AnimationState::RunLoading => self.current + 1 < self.frames.len(),
            AnimationState::Running => true,
        }
    }

    /// Clock reading at which the current frame should be replaced, or `None`
    /// when nothing is scheduled (stopped, single-frame, or a loading animation
    /// parked on its last frame waiting for more frames).
    pub fn next_deadline_ms(&self) -> Option<u64> {
        if !self.is_animating() {
            return None;
        }
        let started = self.frame_started_ms?;
        Some(started.saturating_add(self.playback_gap_ms(self.current).unwrap_or(0)))
    }

    /// Advance playback to the frame due at `now_ms`. Returns whether the
    /// displayed frame changed. Gapless frames (negative gap) are skipped
    /// without being displayed; the walk is bounded by the frame count so a
    /// list of only gapless frames stops the animation instead of spinning.
    pub fn advance(&mut self, now_ms: u64) -> bool {
        if !self.is_animating() {
            return false;
        }
        if self.frame_started_ms.is_none() {
            self.frame_started_ms = Some(now_ms);
            return false;
        }
        let mut changed = false;
        let budget = self.frames.len();
        let mut exhausted = true;
        for _ in 0..budget {
            let started = self.frame_started_ms.unwrap_or(now_ms);
            let gap = self.playback_gap_ms(self.current).unwrap_or(0);
            if now_ms.saturating_sub(started) < gap {
                exhausted = false;
                break;
            }
            if !self.step_frame() {
                exhausted = false;
                break;
            }
            self.frame_started_ms = Some(started.saturating_add(gap));
            changed = true;
        }
        if exhausted {
            // The loop ran out of budget: either the animation is far behind
            // its clock or every frame is gapless. Re-phase to now, and if the
            // frame we landed on is gapless (never displayable) stop rather
            // than schedule an immediate wake forever.
            self.frame_started_ms = Some(now_ms);
            if self.playback_gap_ms(self.current).is_none() {
                self.state = AnimationState::Stopped;
                self.frame_started_ms = None;
            }
        }
        changed
    }

    /// Take one step forward in the frame list, applying end-of-list policy.
    /// Returns whether the position moved.
    fn step_frame(&mut self) -> bool {
        if self.frames.is_empty() {
            return false;
        }
        if self.current + 1 < self.frames.len() {
            self.current += 1;
            return true;
        }
        match self.state {
            // Loading mode waits at the last frame for more frames to arrive.
            AnimationState::RunLoading | AnimationState::Stopped => false,
            AnimationState::Running => {
                if self.consume_loop() {
                    self.current = 0;
                    true
                } else {
                    self.state = AnimationState::Stopped;
                    self.frame_started_ms = None;
                    false
                }
            }
        }
    }

    /// Consume one loop of the loop budget. `true` when looping may continue.
    fn consume_loop(&mut self) -> bool {
        match self.loops_remaining {
            None => true,
            Some(0) => false,
            Some(remaining) => {
                self.loops_remaining = Some(remaining - 1);
                true
            }
        }
    }

    /// Display duration of a frame, or `None` when the frame is *gapless* and
    /// must never be shown. A stored gap of zero (the root frame's default)
    /// plays at the protocol's default gap rather than instantly, so a client
    /// that never sets the root gap cannot drive a zero-delay animation.
    fn playback_gap_ms(&self, index: usize) -> Option<u64> {
        let gap = self.frames.get(index)?.gap_ms;
        if gap < 0 {
            return None;
        }
        let gap = if gap == 0 {
            DEFAULT_FRAME_GAP_MS as u64
        } else {
            gap as u64
        };
        Some(gap.clamp(MIN_PLAYBACK_GAP_MS, MAX_PLAYBACK_GAP_MS))
    }

    /// Take the root frame from the image's transmitted pixels the first time a
    /// frame command touches this image. The root frame has a zero gap, which
    /// the protocol says a client must set explicitly via the control command.
    fn ensure_root(&mut self, canvas: &[u8]) {
        if self.frames.is_empty() {
            self.frames.push(AnimationFrame {
                rgba: canvas.to_vec(),
                gap_ms: 0,
            });
        }
    }

    /// Map a 1-based protocol frame number onto a list index. Frame `0` is not
    /// a frame: the protocol numbers frames from one.
    fn frame_index(&self, frame: u32) -> Option<usize> {
        if frame == 0 {
            return None;
        }
        let index = frame as usize - 1;
        (index < self.frames.len()).then_some(index)
    }
}

fn canvas_bytes(width: u32, height: u32) -> Result<usize, FrameError> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes > 0)
        .ok_or(FrameError::OutOfBounds)
}

/// Reject a rectangle that is empty or reaches past the canvas. Frame
/// rectangles come straight from client control data, so the checked adds here
/// are the parse-boundary clamp for every later index computation.
fn validate_rect(
    canvas_width: u32,
    canvas_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), FrameError> {
    if width == 0 || height == 0 {
        return Err(FrameError::OutOfBounds);
    }
    let right = x.checked_add(width).ok_or(FrameError::OutOfBounds)?;
    let bottom = y.checked_add(height).ok_or(FrameError::OutOfBounds)?;
    if right > canvas_width || bottom > canvas_height {
        return Err(FrameError::OutOfBounds);
    }
    Ok(())
}

fn rects_overlap(
    source_x: u32,
    source_y: u32,
    destination_x: u32,
    destination_y: u32,
    width: u32,
    height: u32,
) -> bool {
    let horizontal = source_x < destination_x.saturating_add(width)
        && destination_x < source_x.saturating_add(width);
    let vertical = source_y < destination_y.saturating_add(height)
        && destination_y < source_y.saturating_add(height);
    horizontal && vertical
}

/// A canvas filled with one 32-bit RGBA color (the `Y=` key). The protocol's
/// default of zero is a transparent black pixel.
fn background_canvas(bytes: usize, color: u32) -> Vec<u8> {
    let pixel = color.to_be_bytes();
    let mut canvas = Vec::with_capacity(bytes);
    for _ in 0..bytes / 4 {
        canvas.extend_from_slice(&pixel);
    }
    canvas.resize(bytes, 0);
    canvas
}

/// Copy a rectangle out of a canvas into a tightly packed RGBA buffer.
fn extract_rect(
    canvas: &[u8],
    canvas_width: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let stride = canvas_width as usize * 4;
    let row_bytes = width as usize * 4;
    let mut out = Vec::with_capacity(row_bytes.saturating_mul(height as usize));
    for row in 0..height as usize {
        let start = (y as usize + row) * stride + x as usize * 4;
        let end = start + row_bytes;
        if end <= canvas.len() {
            out.extend_from_slice(&canvas[start..end]);
        } else {
            out.resize(out.len() + row_bytes, 0);
        }
    }
    out
}

/// Write `patch` into `canvas` at `(x, y)`, either replacing pixels or
/// alpha-blending them (the protocol's default). Rows past the end of either
/// buffer are skipped rather than panicking: the rectangle is validated by
/// [`validate_rect`] before this runs, and this stays defensive so a future
/// caller cannot turn a bad rectangle into an out-of-bounds slice.
#[allow(clippy::too_many_arguments)]
fn blit(
    canvas: &mut [u8],
    canvas_width: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    patch: &[u8],
    overwrite: bool,
) {
    let stride = canvas_width as usize * 4;
    let row_bytes = width as usize * 4;
    for row in 0..height as usize {
        let dst_start = (y as usize + row) * stride + x as usize * 4;
        let src_start = row * row_bytes;
        let Some(dst) = canvas.get_mut(dst_start..dst_start + row_bytes) else {
            break;
        };
        let Some(src) = patch.get(src_start..src_start + row_bytes) else {
            break;
        };
        if overwrite {
            dst.copy_from_slice(src);
            continue;
        }
        for (destination, source) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            alpha_blend(destination, source);
        }
    }
}

/// Alpha-composite one source pixel over one destination pixel, 8-bit
/// straight-alpha in and out. Integer math only: `(a * b + 127) / 255`
/// rounding keeps repeated composition from drifting the way a truncating
/// divide does.
fn alpha_blend(destination: &mut [u8], source: &[u8]) {
    let src_alpha = source[3] as u32;
    if src_alpha == 0 {
        return;
    }
    if src_alpha == 255 {
        destination.copy_from_slice(source);
        return;
    }
    let dst_alpha = destination[3] as u32;
    let out_alpha = src_alpha + mul255(dst_alpha, 255 - src_alpha);
    for channel in 0..3 {
        let src = source[channel] as u32;
        let dst = destination[channel] as u32;
        let weighted = mul255(src, src_alpha) + mul255(mul255(dst, dst_alpha), 255 - src_alpha);
        destination[channel] = ((weighted * 255 + out_alpha / 2) / out_alpha).min(255) as u8;
    }
    destination[3] = out_alpha.min(255) as u8;
}

fn mul255(a: u32, b: u32) -> u32 {
    (a * b + 127) / 255
}

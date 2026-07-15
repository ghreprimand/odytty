// SPDX-License-Identifier: GPL-3.0-only
//! Cursor blink-hold frame update + VE4 cursor-slide motion for the native app.
//!
//! Two responsibilities, both kept here so `app/mod.rs` stays under the
//! source-size cap:
//!
//! 1. [`App::update_held_cursor_frame`] re-presents the last frame with only the
//!    cursor's blink visibility toggled, reusing the retained render signature
//!    so a blink tick does not force a full content rebuild.
//! 2. The VE4 cursor-slide contributor: [`App::cursor_motion_offset`] /
//!    [`App::cursor_motion_deadline`] expose the precomputed slide offset + wake
//!    that the [`App::cursor_render_params`] / [`App::animation_deadline`]
//!    aggregators fold in, and [`App::update_cursor_motion`] refreshes them once
//!    per rebuild from the injected `now` and the cursor's logical move.
//!
//! Static-path contract (`cursor_motion = false` or reduced motion): the offset
//! is held at `[0.0, 0.0]` and no wake is armed, so the cursor sits at its exact
//! cell origin. Discontinuities (first frame,
//! resize/reflow, scrollback, large jump, unfocused, hidden cursor) always snap
//! rather than use this short glide. Large jumps may be presented by the
//! independent bounded follower in `cursor_streak.rs`.
//!
//! This module reaches `App`'s private fields directly; the parent reaches these
//! methods through `pub(super)`.

use super::*;

/// Total duration of one cursor glide. Short enough to read as responsive, long
/// enough to register as motion.
const CURSOR_SLIDE_DURATION: Duration = Duration::from_millis(55);

/// Animation frame cadence while a slide is in flight (~60fps). Bounded: the
/// slide settles after [`CURSOR_SLIDE_DURATION`] and then arms no further wake.
const CURSOR_MOTION_FRAME: Duration = Duration::from_millis(16);

/// Manhattan cell distance beyond which a cursor move snaps instead of sliding.
/// A large jump bypasses this nearby-motion glide; only short adjacent steps
/// use it. The optional trail follower owns the separate large-jump treatment.
pub(super) const MAX_SLIDE_CELLS: f32 = 6.0;

/// Ease-out cubic: fast departure, gentle arrival. Maps `0.0..=1.0` to itself.
fn ease_out_cubic(p: f32) -> f32 {
    let inv = 1.0 - p;
    1.0 - inv * inv * inv
}

impl App {
    /// Sub-cell pixel offset added to the cursor's cell origin (VE4 slide).
    ///
    /// Returns the value [`App::update_cursor_motion`] precomputed for the
    /// current frame: `[0.0, 0.0]` whenever motion is off or the cursor is at
    /// rest, or the decaying displacement while a glide is in flight. First-frame
    /// snap is guaranteed by the updater (a `None` prior snapshot snaps), so this
    /// never glides from a stale position.
    pub(super) fn cursor_motion_offset(&self) -> [f32; 2] {
        if self.settings.reduced_motion {
            [0.0, 0.0]
        } else {
            self.cursor_anim_offset
        }
    }

    /// Next wake instant while a cursor slide is in flight, or `None` once it
    /// settles (the bounded-wake contract: a finished glide arms no further
    /// wake, so an idle terminal returns to zero extra wakeups).
    pub(super) fn cursor_motion_deadline(&self) -> Option<Instant> {
        (!self.settings.reduced_motion)
            .then_some(self.cursor_slide_deadline)
            .flatten()
    }

    /// Recompute the cursor slide offset + the next slide wake for `now`.
    ///
    /// Called once per rebuild after the blink poll, so the `&self` accessors
    /// above return a value consistent with this frame. The destination is the
    /// fresh `snapshot.cursor`; the origin is the prior undecorated content
    /// cursor (`last_cursor_comparison_snapshot.cursor`), which still holds the
    /// prior frame's position at this point in the rebuild.
    ///
    /// Identity when off: with `cursor_motion == false` this pins
    /// `offset = [0.0, 0.0]` and clears the deadline, so
    /// [`App::cursor_render_params`] stays at the identity and
    /// [`App::animation_deadline`] contributes nothing.
    ///
    /// Snap (no slide) on any discontinuity: first frame, a dimension change
    /// (resize/reflow), scrolled-back viewport, an unfocused window, a hidden
    /// cursor, or a jump longer than [`MAX_SLIDE_CELLS`].
    pub(in crate::native) fn update_cursor_motion(
        &mut self,
        now: Instant,
        snapshot: &Snapshot,
        cell: CellSize,
    ) {
        if self.settings.reduced_motion || !self.settings.cursor_motion {
            self.cursor_anim_offset = [0.0, 0.0];
            self.cursor_slide_deadline = None;
            self.cursor_slide_start = None;
            return;
        }
        let to = snapshot.cursor;
        let prior = self
            .last_cursor_comparison_snapshot
            .as_ref()
            .map(|s| (s.cursor, s.dimensions));
        // Discontinuities that must teleport rather than glide.
        let snap = match prior {
            None => true,
            Some((_, dims)) => dims != snapshot.dimensions,
        } || !snapshot.cursor_visible
            || !self.focused
            || self.viewport.offset() != 0;
        if snap {
            self.cursor_slide_start = None;
        } else if let Some((from, _)) = prior
            && to != from
        {
            // A fresh logical move: arm a glide from the prior cell unless it is
            // a large jump, which the separate follower may present.
            let dcol = from.column as f32 - to.column as f32;
            let drow = from.row as f32 - to.row as f32;
            if dcol.abs() + drow.abs() <= MAX_SLIDE_CELLS {
                self.cursor_slide_from_px = [dcol * cell.width as f32, drow * cell.height as f32];
                self.cursor_slide_start = Some(now);
            } else {
                self.cursor_slide_start = None;
            }
        }
        match self.cursor_slide_start {
            Some(start) => {
                let progress = (now.saturating_duration_since(start).as_secs_f32()
                    / CURSOR_SLIDE_DURATION.as_secs_f32())
                .clamp(0.0, 1.0);
                if progress >= 1.0 {
                    self.cursor_anim_offset = [0.0, 0.0];
                    self.cursor_slide_deadline = None;
                    self.cursor_slide_start = None;
                } else {
                    let remain = 1.0 - ease_out_cubic(progress);
                    self.cursor_anim_offset = [
                        self.cursor_slide_from_px[0] * remain,
                        self.cursor_slide_from_px[1] * remain,
                    ];
                    self.cursor_slide_deadline = Some(now + CURSOR_MOTION_FRAME);
                }
            }
            None => {
                self.cursor_anim_offset = [0.0, 0.0];
                self.cursor_slide_deadline = None;
            }
        }
    }

    /// Test seam: set the prior presented snapshot so [`App::update_cursor_motion`]
    /// can be driven across simulated frames (the real path updates this at frame
    /// end).
    #[cfg(test)]
    pub(in crate::native) fn set_last_presented_snapshot_for_test(&mut self, snapshot: Snapshot) {
        self.last_presented_snapshot = Some(snapshot.clone());
        self.last_cursor_comparison_snapshot = Some(snapshot);
    }

    pub(super) fn update_held_cursor_frame(&mut self, now: Instant) -> bool {
        let Some(mut snapshot) = self.last_presented_snapshot.clone() else {
            return false;
        };
        let Some(previous_signature) = self.last_render_signature.clone() else {
            return false;
        };

        let last_presented_cursor_blinking = self.last_presented_cursor_blinking;
        let focused = self.focused;
        let cursor_on = self
            .cursor_blink
            .poll(now, last_presented_cursor_blinking, focused);
        // Easing keeps the cursor visible through the blink off-phase (the
        // precomputed alpha carries the fade); the hard-hide applies only when
        // easing is off, matching the main rebuild path so the two stay in sync.
        if !cursor_on && (!self.settings.cursor_easing || self.settings.reduced_motion) {
            snapshot.cursor_visible = false;
        }

        // R3 call-site parity: this blink-frame path and the normal paint path
        // (`app/mod.rs` CursorOnly arm) MUST pass the same `cursor_render_params()`
        // source, or the cursor would render differently between a blink tick and
        // a content repaint. This includes the window-focus bit that selects a
        // filled or hollow Block. The signature's `anim` key is derived from the
        // same params so the cache observes every geometry change here too.
        let params = self.cursor_render_params();
        let signature = RenderSignature {
            content: previous_signature.content,
            cursor: CursorRenderSignature {
                visible: snapshot.cursor_visible,
                style: self.last_presented_cursor_style,
                anim: CursorAnimKey::from_params(&params),
                streak_epoch: self.cursor_streak_epoch(),
            },
        };
        let update = RenderSignature::update_from(self.last_render_signature.as_ref(), &signature);
        let last_presented_cursor_style = self.last_presented_cursor_style;
        if let Some(gpu) = self.gpu.as_mut() {
            match update {
                GeometryUpdate::Full | GeometryUpdate::CursorOnly => {
                    gpu.update_cursor_with_retained_overlays(
                        &snapshot,
                        last_presented_cursor_style,
                        params,
                    );
                }
                GeometryUpdate::Retained => {}
            }
        }
        self.last_render_signature = Some(signature);
        true
    }
}

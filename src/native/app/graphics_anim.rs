// SPDX-License-Identifier: GPL-3.0-only
//! Render-loop driver for Kitty graphics animations (`a=f` / `a=a`).
//!
//! The terminal core owns frames, composition, and playback policy but keeps no
//! clock; this module is the one place that reads a clock for animated graphics
//! and feeds it in. It contributes to the event-driven loop exactly the way the
//! cursor, bell, and scroll-glide timers do: the maintenance pass advances the
//! animation and republishes the deadline, and
//! [`App::next_wake_deadline`](crate::native::app::App) sources that deadline so
//! the loop sleeps until the next frame is actually due. There is no polling and
//! no per-frame timer when nothing is animating.
//!
//! Scope decisions, both deliberate:
//!
//! - **Active pane only.** The deadline source and the advancing consumer are
//!   the same pane, so no wake is scheduled for an animation nobody is looking
//!   at. A background tab or pane holds its current frame and resumes from the
//!   live clock when it is brought forward, rather than burning frames off-screen.
//! - **Reduced motion does not stop animations.** `reduced_motion` governs
//!   OdyTTY's own decorative motion - cursor easing, trails, fades. An animated
//!   image is program output, like text: a client that transmits frames is
//!   showing content, not chrome, so suppressing it would corrupt what the
//!   program is displaying rather than calm the interface. The effects layer's
//!   rules are unchanged by this module.

use std::time::{Duration, Instant};

use super::App;

impl App {
    /// Advance the active pane's visible animated images to the frame due at
    /// `now` and refresh [`App::animated_graphics_deadline`]. A frame flip marks
    /// the terminal dirty in the core (so the frame gate repaints) and requests a
    /// redraw here.
    pub(in crate::native) fn advance_graphics_animations(&mut self, now: Instant) {
        let now_ms = self.graphics_clock_ms(now);
        let offset = self.viewport.offset();
        let (changed, deadline_ms) = {
            let mut terminal = crate::native::lock_recover(&self.terminal);
            let changed = terminal.advance_graphics_animations(now_ms, offset);
            let deadline = terminal.graphics_animation_deadline_ms(offset);
            (changed, deadline)
        };
        self.animated_graphics_deadline = deadline_ms.map(|ms| self.instant_for_graphics_ms(ms));
        if changed {
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Milliseconds from the graphics-clock origin, saturating so a clock that
    /// somehow precedes the origin reads as zero rather than wrapping.
    fn graphics_clock_ms(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.graphics_clock_epoch)
            .as_millis()
            .min(u64::MAX as u128) as u64
    }

    /// Inverse of [`Self::graphics_clock_ms`]: the instant a core-side
    /// millisecond deadline falls at.
    fn instant_for_graphics_ms(&self, ms: u64) -> Instant {
        self.graphics_clock_epoch + Duration::from_millis(ms)
    }

    /// Test seam: the animation wake this pane currently schedules.
    #[cfg(test)]
    pub(in crate::native) fn animated_graphics_deadline_for_test(&self) -> Option<Instant> {
        self.animated_graphics_deadline
    }
}

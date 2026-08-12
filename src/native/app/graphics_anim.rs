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
//! - **Visible panes only.** Every pane in the active tab contributes its next
//!   deadline and advances from the same clock. Background tabs and panes hidden
//!   by zoom hold their current frame until they become visible, so animations
//!   never burn frames off-screen.
//! - **Reduced motion does not stop animations.** `reduced_motion` governs
//!   OdyTTY's own decorative motion - cursor easing, trails, fades. An animated
//!   image is program output, like text: a client that transmits frames is
//!   showing content, not chrome, so suppressing it would corrupt what the
//!   program is displaying rather than calm the interface. The effects layer's
//!   rules are unchanged by this module.

use std::time::{Duration, Instant};

use super::App;

impl App {
    /// Advance every visible pane's animated images to the frame due at `now`
    /// and refresh [`App::animated_graphics_deadline`] with their minimum wake.
    /// A frame flip marks that pane dirty so the tab-wide frame gate repaints.
    pub(in crate::native) fn advance_graphics_animations(&mut self, now: Instant) {
        let now_ms = self.graphics_clock_ms(now);
        let mut changed = false;
        let mut deadline_ms: Option<u64> = None;
        for token in self.sessions.active_visible_tokens() {
            let Some(session) = self.sessions.get_mut(token) else {
                continue;
            };
            let offset = session.viewport.offset();
            let terminal = std::sync::Arc::clone(&session.terminal);
            let mut terminal = crate::native::lock_recover(&terminal);
            let pane_changed = terminal.advance_graphics_animations(now_ms, offset);
            let pane_deadline = terminal.graphics_animation_deadline_ms(offset);
            drop(terminal);
            if pane_changed {
                session.needs_rebuild = true;
                changed = true;
            }
            if let Some(pane_deadline) = pane_deadline {
                deadline_ms =
                    Some(deadline_ms.map_or(pane_deadline, |current| current.min(pane_deadline)));
            }
        }
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

    /// Test seam: the minimum animation wake across currently visible panes.
    #[cfg(test)]
    pub(in crate::native) fn animated_graphics_deadline_for_test(&self) -> Option<Instant> {
        self.animated_graphics_deadline
    }
}

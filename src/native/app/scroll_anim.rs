// SPDX-License-Identifier: GPL-3.0-only
//! RV4 smooth scrolling — optional eased scrollback animation.
//!
//! The core scroll model stays integer-row: [`Viewport::offset`] snaps to the
//! new row immediately (the scroll TARGET updates with zero added latency), and
//! only the VISUAL position eases toward it. The easing is a single sub-row
//! pixel offset ([`App::scroll_frac_offset`]) decaying to zero over a short,
//! hard-capped duration; it is pushed to the GPU `content_origin` Y so the whole
//! rendered viewport — cells, cursor, and overlays — glides uniformly, then
//! settles. At rest the offset is `0.0` and no wake is scheduled.
//!
//! Off-path contract (D-RV4-6 / §6): with `smooth_scroll` off, the
//! `scroll_viewport` trigger never calls [`App::begin_scroll_anim`], so
//! [`App::scroll_anim`] is always `None`, [`App::scroll_frac_offset`] stays
//! `0.0` (the GPU `content_origin` is unshifted), [`App::scroll_anim_deadline`]
//! is `None` (zero extra wakes), and [`App::scroll_frac_bits`] is a constant `0`
//! in the content render signature — the default render path is byte-identical
//! to before this feature existed.
//!
//! Snap-by-default (D-RV4-2/D-RV4-8): every viewport change clears the glide via
//! [`App::clear_scroll_anim`] (called from `on_viewport_changed`); only a
//! user-initiated `scroll_viewport` re-arms it afterwards, and only when not in
//! a selection drag-autoscroll (which would otherwise nest easing). So
//! programmatic jumps — return-to-live, search navigation, scrollbar-thumb
//! drag, resize — and active drag-autoscroll always snap.

use super::*;

/// Ease-out cubic (matches the cursor-slide curve): fast departure, gentle
/// arrival. Maps `0.0..=1.0` to itself. Local copy so the feature lives in its
/// own lane.
fn ease_out_cubic(p: f32) -> f32 {
    let inv = 1.0 - p;
    1.0 - inv * inv * inv
}

/// Active eased scrollback glide (RV4). Records the start instant and the full
/// sub-row pixel displacement at `t = 0`, which decays to `0.0` over
/// [`crate::settings::SMOOTH_SCROLL_DURATION`].
#[derive(Debug, Clone, Copy)]
pub(in crate::native) struct ScrollAnimState {
    start: Instant,
    /// Pixel displacement at `t = 0`. Positive = content shifted DOWN (a
    /// scroll-up, where new content enters from the top); negative = up.
    from_px: f32,
}

impl App {
    /// Begin (or replace) a smooth-scroll glide for a user scroll of `delta`
    /// rows. Captures a one-cell-height displacement in the direction the
    /// content came from, decaying to zero — the magnitude is always one cell
    /// (a catch-up ease), never proportional to `delta`, so a fast/large scroll
    /// never stacks offset (T3). A new scroll always REPLACES the prior glide.
    /// Only reached from the `scroll_viewport` trigger when `smooth_scroll` is on
    /// and the change is user-initiated (not a drag-autoscroll).
    #[cfg(test)]
    pub(super) fn begin_scroll_anim(&mut self, delta: isize) {
        self.begin_scroll_anim_of(self.sessions.active_id(), delta);
    }

    pub(super) fn begin_scroll_anim_of(&mut self, token: SessionToken, delta: isize) {
        let cell_h = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height) as f32;
        let Some(session) = self.sessions.get_mut(token) else {
            return;
        };
        if cell_h <= 0.0 || delta == 0 {
            // No metrics yet (pre-GPU) or a no-op scroll: snap rather than glide.
            session.scroll_anim = None;
            session.scroll_frac_offset = 0.0;
            return;
        }
        // scroll-up (delta > 0, viewing older content) → content enters from
        // above → start shifted DOWN (positive) and ease up to rest.
        let sign = if delta > 0 { 1.0 } else { -1.0 };
        let from_px = sign * cell_h;
        session.scroll_anim = Some(ScrollAnimState {
            start: Instant::now(),
            from_px,
        });
        session.scroll_frac_offset = from_px;
    }

    /// Snap: clear any glide and zero the offset. The default for every viewport
    /// change (`on_viewport_changed`); the user `scroll_viewport` path re-arms
    /// after it when appropriate, so all programmatic jumps snap.
    #[cfg(test)]
    pub(super) fn clear_scroll_anim(&mut self) {
        self.clear_scroll_anim_of(self.sessions.active_id());
    }

    /// Test seam: seed an in-flight glide on the active session directly, so a
    /// headless test (no GPU cell metrics) can exercise the wake-scheduling /
    /// maintenance-step wiring that `begin_scroll_anim_of` would set up on a real
    /// scroll. Constructs the module-private `ScrollAnimState` from within its
    /// own module.
    #[cfg(test)]
    pub(in crate::native) fn seed_scroll_glide_for_test(&mut self, from_px: f32) {
        let token = self.sessions.active_id();
        if let Some(session) = self.sessions.get_mut(token) {
            session.scroll_anim = Some(ScrollAnimState {
                start: Instant::now(),
                from_px,
            });
            session.scroll_frac_offset = from_px;
        }
    }

    pub(super) fn clear_scroll_anim_of(&mut self, token: SessionToken) {
        if let Some(session) = self.sessions.get_mut(token) {
            session.scroll_anim = None;
            session.scroll_frac_offset = 0.0;
        }
    }

    /// Advance the glide for the current frame: recompute
    /// [`App::scroll_frac_offset`] and settle (clear) once the bounded duration
    /// elapses. No-op while idle / off, leaving the offset `0.0`
    /// (byte-identical). Called once per rebuild, before the render signature is
    /// built and the offset is pushed to the GPU.
    pub(super) fn update_scroll_anim(&mut self, now: Instant) {
        let Some(state) = self.scroll_anim else {
            self.scroll_frac_offset = 0.0;
            return;
        };
        let duration = crate::settings::SMOOTH_SCROLL_DURATION;
        let elapsed = now.saturating_duration_since(state.start);
        if elapsed >= duration {
            self.scroll_anim = None;
            self.scroll_frac_offset = 0.0;
            return;
        }
        let p = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
        self.scroll_frac_offset = state.from_px * (1.0 - ease_out_cubic(p));
    }

    /// Next glide wake, or `None` once the glide settles (always `None` on the
    /// off path, where `scroll_anim` is `None`). Folded into
    /// [`App::animation_deadline`] as the fourth contributor — the soonest of
    /// the next frame and the settle instant.
    pub(super) fn scroll_anim_deadline(&self) -> Option<Instant> {
        self.scroll_anim.as_ref().map(|state| {
            let settle = state.start + crate::settings::SMOOTH_SCROLL_DURATION;
            let next_frame = Instant::now() + crate::settings::SMOOTH_SCROLL_FRAME;
            settle.min(next_frame)
        })
    }

    /// `f32::to_bits()` of the current scroll offset, for the content render
    /// signature. Constant `0` on the off path / at rest (so the cache decision
    /// is unchanged), and changes every animating frame so the cache
    /// reclassifies and the GPU rebuilds the shifted vertices.
    pub(super) fn scroll_frac_bits(&self) -> u32 {
        self.scroll_frac_offset.to_bits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Terminal;
    use std::sync::Mutex;

    // --- pure curve properties (no App) -------------------------------------

    #[test]
    fn ease_out_cubic_endpoints_and_monotonic() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        let mut prev = ease_out_cubic(0.0);
        for i in 1..=10 {
            let v = ease_out_cubic(i as f32 / 10.0);
            assert!(v > prev, "ease must increase: {v} <= {prev}");
            prev = v;
        }
    }

    fn build_app() -> Option<App> {
        let d = Dimensions::new(40, 6);
        let session = crate::native::test_support::spawn_test_pause_shell(d).ok()?;
        let writer: crate::native::pty::PtyWriter =
            Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(d.columns, d.rows)));
        let pty = Arc::new(Mutex::new(session));
        Some(App::new(
            crate::native::options::NativeOptions::default(),
            terminal,
            writer,
            pty,
            Settings::default(),
            crate::settings::SettingsReloader::for_current_process(Instant::now()),
        ))
    }

    // --- T1: off-path identity ----------------------------------------------

    #[test]
    fn off_path_is_idle_and_byte_identical() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert!(!app.settings.smooth_scroll, "off by default");
        // No GPU metrics in a headless build; begin must snap (clear), never arm.
        app.begin_scroll_anim(3);
        assert!(
            app.scroll_anim.is_none(),
            "no glide on the off/headless path"
        );
        assert_eq!(app.scroll_frac_offset, 0.0);
        assert_eq!(app.scroll_anim_deadline(), None, "no wake while idle");
        assert_eq!(app.scroll_frac_bits(), 0, "constant 0 in the signature");
        app.update_scroll_anim(Instant::now());
        assert_eq!(app.scroll_frac_offset, 0.0, "update is a no-op while idle");
    }

    // --- begin / settle / deadline ------------------------------------------

    #[test]
    fn glide_settles_to_zero_within_the_bounded_duration() {
        let Some(mut app) = build_app() else {
            return;
        };
        // Drive the state machine directly (no GPU): seed an explicit glide.
        let t0 = Instant::now();
        app.scroll_anim = Some(ScrollAnimState {
            start: t0,
            from_px: 16.0,
        });
        app.scroll_frac_offset = 16.0;
        // Mid-glide: offset strictly between 0 and the initial magnitude, and a
        // wake is scheduled.
        let mid = t0 + crate::settings::SMOOTH_SCROLL_DURATION / 2;
        app.update_scroll_anim(mid);
        assert!(
            app.scroll_frac_offset > 0.0 && app.scroll_frac_offset < 16.0,
            "eases toward rest: {}",
            app.scroll_frac_offset
        );
        assert!(app.scroll_anim_deadline().is_some(), "wake while gliding");
        assert_ne!(app.scroll_frac_bits(), 0, "signature changes while gliding");
        // Past the hard cap: settled, idle, zero — no perpetual wake.
        let after = t0 + crate::settings::SMOOTH_SCROLL_DURATION + Duration::from_millis(1);
        app.update_scroll_anim(after);
        assert_eq!(app.scroll_frac_offset, 0.0, "settles to zero");
        assert!(app.scroll_anim.is_none(), "glide cleared at the cap");
        assert_eq!(app.scroll_anim_deadline(), None, "no wake once settled");
    }

    // --- snap clears the glide ----------------------------------------------

    #[test]
    fn clear_snaps_immediately() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.scroll_anim = Some(ScrollAnimState {
            start: Instant::now(),
            from_px: -16.0,
        });
        app.scroll_frac_offset = -16.0;
        app.clear_scroll_anim();
        assert!(app.scroll_anim.is_none());
        assert_eq!(app.scroll_frac_offset, 0.0);
        assert_eq!(app.scroll_anim_deadline(), None);
    }

    // --- aggregator integration ---------------------------------------------

    #[test]
    fn deadline_joins_animation_aggregator() {
        let Some(mut app) = build_app() else {
            return;
        };
        // Cursor animation knobs off ⇒ the aggregate equals the scroll deadline.
        assert_eq!(app.animation_deadline(), None, "idle ⇒ no aggregate wake");
        app.scroll_anim = Some(ScrollAnimState {
            start: Instant::now(),
            from_px: 16.0,
        });
        assert!(
            app.animation_deadline().is_some(),
            "an in-flight glide contributes to the single animation timer"
        );
    }
}

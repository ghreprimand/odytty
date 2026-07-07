// SPDX-License-Identifier: GPL-3.0-only
//! Continuous fractional scrolling — sub-row pixel-precise scrollback for
//! high-resolution wheels/touchpads (`PixelDelta`).
//!
//! The scroll model stays integer-row: [`Viewport::offset`] carries whole rows,
//! and a per-session sub-row remainder (`Session::scroll_frac_rows`, unit =
//! rows, invariant `(-1.0, 1.0)`) tracks the fractional position between two
//! integer offsets. The remainder is turned into a pixel displacement
//! (`Session::scroll_frac_offset`) pushed to the GPU `content_origin` Y
//! (`gpu.rs`), so the whole rendered viewport — cells, cursor, and overlays —
//! shifts uniformly by the sub-row amount.
//!
//! Sign convention: positive = content shifted DOWN (a scroll-up, older content
//! entering from the top); negative = up. At rest (`scroll_frac_rows == 0.0`)
//! the offset is `0.0` and the render-signature bit ([`App::scroll_frac_bits`])
//! is a constant `0`, so the default render path is byte-identical to an
//! integer-only scroll.
//!
//! Only high-resolution `PixelDelta` input on the continuous lane drives this
//! (see `handle_mouse_wheel`); discrete `LineDelta` notches move the integer
//! offset directly and never touch the remainder. The pixel travel maps 1:1 to
//! rows (× `scroll_pixel_speed`, identity by default), so a burst sums to
//! exactly the physical travel — runaway-proof by construction, no notch
//! multiplier. The lane is single-pane only in v1 (the multipane render path
//! hard-zeros the shared GPU offset at `panes.rs`).

use super::*;
use std::time::Duration;

/// Apply `d_rows` of continuous scroll travel to a running sub-row remainder,
/// returning the whole-row carry (to move the integer viewport offset) and the
/// new fractional remainder in `(-1.0, 1.0)`. Pure so the carry arithmetic is
/// unit-testable without GPU cell metrics. `trunc` carries toward zero, so the
/// remainder never flips sign spuriously and whole rows always move the integer
/// offset by an exact row count — the sub-row part below one row is *kept*, not
/// discarded (the old notch coalescer truncated it, so sub-notch pixel travel
/// produced no motion at all).
fn carry_scroll_frac(frac: f32, d_rows: f32) -> (isize, f32) {
    let total = frac + d_rows;
    let whole = total.trunc();
    (whole as isize, total - whole)
}

/// Time constant for the SCROLL-GLIDE follower's exponential approach. Larger =
/// slower, longer glide; smaller = snappier. A tuning const so the feel can be
/// retuned without touching the stepping math.
const SCROLL_GLIDE_TAU: Duration = Duration::from_millis(80);

/// Frame pacing for the glide animation wake (matches the other frame-paced
/// animations), and the assumed dt for the first frame after arming.
const SCROLL_GLIDE_FRAME: Duration = Duration::from_millis(16);

/// The follower settles (snaps to the logical offset and ends the glide) once it
/// is within this many rows of the target — well under a pixel at any cell size.
const GLIDE_SETTLE_ROWS: f32 = 0.01;

/// One frame of the SCROLL-GLIDE forward-chase follower: ease `visual` toward
/// `logical` by an exponential factor derived from the real frame `dt` and the
/// time constant `tau`, so the motion is frame-rate independent. The factor
/// `1 - e^(-dt/tau)` is in `[0, 1)` for any positive `dt`, so the result never
/// overshoots or reverses past `logical`. With `logical` only ever moving in the
/// scroll direction, that makes the chase structurally sawtooth-proof (unlike
/// the removed catch-up ease, which re-armed a backward displacement per notch).
/// Pure, so the approach behavior is unit-testable without a clock or GPU.
fn glide_step(visual: f32, logical: f32, dt: Duration, tau: Duration) -> f32 {
    let dt_s = dt.as_secs_f32();
    let tau_s = tau.as_secs_f32().max(1e-4);
    let alpha = (1.0 - (-dt_s / tau_s).exp()).clamp(0.0, 1.0);
    visual + (logical - visual) * alpha
}

impl App {
    /// Whether an eligible `PixelDelta` wheel event should drive the continuous
    /// fractional scroll lane rather than the discrete notch lane. All must
    /// hold: the `pixel_scroll` knob is on; the active tab is a single pane (the
    /// multipane render path can't express per-pane sub-row offsets); the active
    /// screen is the primary screen (the alternate screen has no scrollback to
    /// render fractionally); Ctrl is not held (a zoom gesture); and the pointer
    /// is not mid selection-drag (drag-autoscroll steps whole rows). The
    /// mouse-reporting and overlay-open cases are already excluded by earlier
    /// returns in `handle_mouse_wheel`.
    pub(super) fn continuous_scroll_eligible(&self) -> bool {
        self.settings.pixel_scroll
            && self.sessions.active_is_single_pane()
            && !self.modifiers.ctrl
            && !self.pointer_drag.is_selecting()
            && self.on_primary_screen()
    }

    /// Whether the active terminal is showing its primary screen (not an
    /// alternate-screen application like a pager or full-screen TUI). Continuous
    /// fractional scroll is meaningful only where there is scrollback to glide
    /// through.
    fn on_primary_screen(&self) -> bool {
        self.terminal
            .lock()
            .map(|t| !t.on_alternate_screen())
            .unwrap_or(true)
    }

    /// Drive the continuous fractional scroll lane for one `PixelDelta` event of
    /// `pos_y` pixels on session `token`. Maps pixels 1:1 to rows (×
    /// `scroll_pixel_speed`, identity by default) so a burst sums to exactly the
    /// physical travel, carries whole rows into the integer [`Viewport::offset`],
    /// and keeps the sub-row remainder as the visual offset. It never arms an
    /// ease — there is none; the sub-row position is set directly. The remainder
    /// is clamped at the scrollback bounds so momentum past either end deposits
    /// nothing (no drift, no rubber-band).
    pub(super) fn drive_continuous_scroll(
        &mut self,
        token: SessionToken,
        pos_y: f64,
        cell_height: u32,
    ) {
        let cell_h = cell_height as f32;
        if cell_h <= 0.0 {
            return;
        }
        // 1:1 physical: one cell-height of finger travel = one row. `pos_y > 0`
        // = scroll up toward history (matches `wheel_delta_notches`'s sign).
        let mut d_rows = (pos_y as f32 / cell_h) * self.settings.scroll_pixel_speed;
        if d_rows == 0.0 {
            return;
        }
        // Defensive: cap a single malformed giant PixelDelta at one viewport
        // height so a driver glitch cannot leap across all of scrollback. Never
        // reached in normal input.
        let vp_rows = (self.grid.rows.max(1)) as f32;
        d_rows = d_rows.clamp(-vp_rows, vp_rows);

        let scrollback_len = self.scrollback_len_of(token);
        let (whole, mut new_frac) = {
            let Some(session) = self.sessions.get_mut(token) else {
                return;
            };
            carry_scroll_frac(session.scroll_frac_rows, d_rows)
        };
        // Carry whole rows into the integer offset (clamped by the viewport).
        if whole != 0
            && let Some(session) = self.sessions.get_mut(token)
        {
            match whole.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    session.viewport.scroll_up(whole as usize, scrollback_len);
                }
                std::cmp::Ordering::Less => {
                    session.viewport.scroll_down((-whole) as usize);
                }
                std::cmp::Ordering::Equal => {}
            }
        }
        // Boundary clamp: once the integer offset saturates, drop the leftover
        // remainder that would push further past the end. Positive = content
        // shifted down (older); at the oldest row (`offset >= scrollback_len`) a
        // positive remainder is impossible. Negative = content shifted up
        // (newer); at the live bottom (`offset == 0`) a negative remainder is
        // impossible.
        let offset = self
            .sessions
            .get(token)
            .map(|session| session.viewport.offset())
            .unwrap_or(0);
        if offset == 0 && new_frac < 0.0 {
            new_frac = 0.0;
        }
        if offset >= scrollback_len && new_frac > 0.0 {
            new_frac = 0.0;
        }
        // Snap-clears the remainder (`on_viewport_changed_of` →
        // `clear_scroll_frac_of`), marks a rebuild, and requests a redraw; then
        // write the sub-row offset AFTER the clear so the continuous position
        // survives this frame (mirrors how the discrete path re-armed the old
        // ease after the same snap).
        self.on_viewport_changed_of(token);
        if let Some(session) = self.sessions.get_mut(token) {
            session.scroll_frac_rows = new_frac;
            session.scroll_frac_offset = new_frac * cell_h;
        }
    }

    /// Clear any sub-row scroll remainder for `token` and zero its pixel offset
    /// — the snap-by-default seam invoked on every viewport change
    /// (`on_viewport_changed_of`: return-to-live, search nav, scrollbar-thumb
    /// drag, resize). A no-op when already at rest, so the off path stays
    /// byte-identical.
    pub(super) fn clear_scroll_frac_of(&mut self, token: SessionToken) {
        if let Some(session) = self.sessions.get_mut(token) {
            session.scroll_frac_rows = 0.0;
            session.scroll_frac_offset = 0.0;
        }
    }

    /// `f32::to_bits()` of the current sub-row pixel offset, folded into the
    /// content render signature. Constant `0` at rest (so the cache decision is
    /// unchanged), and changes whenever the fractional position moves so the
    /// cache reclassifies and the GPU rebuilds the shifted vertices.
    pub(super) fn scroll_frac_bits(&self) -> u32 {
        self.scroll_frac_offset.to_bits()
    }

    /// Whether a discrete wheel/keyboard scroll should engage the SCROLL-GLIDE
    /// follower: the `scroll_glide` knob is on, the active tab is a single pane
    /// (v1 renders the sub-row shift only on that path), the primary screen is
    /// showing (no scrollback to glide on the alternate screen), and the pointer
    /// is not mid selection-drag (drag-autoscroll steps whole rows).
    fn scroll_glide_eligible(&self) -> bool {
        self.settings.scroll_glide
            && self.sessions.active_is_single_pane()
            && !self.pointer_drag.is_selecting()
            && self.on_primary_screen()
    }

    /// The follower's rendered position just before a scroll jump, for
    /// [`Self::arm_scroll_glide_of`]: the lagging visual of an in-flight glide,
    /// else the current integer offset. Read BEFORE the offset moves so a notch
    /// stream keeps chasing from where it currently renders.
    pub(super) fn scroll_glide_start_visual(&self, token: SessionToken) -> f32 {
        self.sessions
            .get(token)
            .map(|session| {
                if session.glide_active {
                    session.glide_visual
                } else {
                    session.viewport.offset() as f32
                }
            })
            .unwrap_or(0.0)
    }

    /// Arm (or re-arm) the glide follower for `token` after a user-initiated
    /// wheel/keyboard scroll moved the integer offset. `start_visual` is where
    /// the follower was rendering before the jump, so a notch stream keeps
    /// chasing without a reset. The lag is clamped to one viewport height so a
    /// rapid burst cannot leave a long laggy crawl. No-op when the glide is
    /// ineligible (off / multipane / alt-screen / selecting) or the lag is
    /// already negligible, leaving the byte-identical instant-scroll path.
    pub(super) fn arm_scroll_glide_of(&mut self, token: SessionToken, start_visual: f32) {
        if !self.scroll_glide_eligible() {
            return;
        }
        let vp_rows = self.grid.rows.max(1) as f32;
        if let Some(session) = self.sessions.get_mut(token) {
            let logical = session.viewport.offset();
            let logical_f = logical as f32;
            let visual = start_visual.clamp(logical_f - vp_rows, logical_f + vp_rows);
            if (visual - logical_f).abs() < GLIDE_SETTLE_ROWS {
                return;
            }
            session.glide_visual = visual;
            session.glide_active = true;
            session.glide_target = logical;
            session.glide_last_tick = None;
        }
    }

    /// Snap the glide follower for `token` to rest (visual == logical, no wake) —
    /// the snap-by-default seam alongside [`Self::clear_scroll_frac_of`]. Zeroes
    /// the sub-row pixel offset only when a glide was actually in flight, so a
    /// snap on an idle session leaves any continuous-lane remainder untouched.
    pub(super) fn snap_scroll_glide_of(&mut self, token: SessionToken) {
        if let Some(session) = self.sessions.get_mut(token) {
            if session.glide_active {
                session.glide_active = false;
                session.scroll_frac_offset = 0.0;
            }
            session.glide_last_tick = None;
            session.glide_visual = session.viewport.offset() as f32;
        }
    }

    /// Advance the active session's glide follower one frame toward `logical`
    /// (the just-anchored integer offset), setting the sub-row pixel offset the
    /// render reads. A no-op unless a glide is in flight, so the off path costs
    /// one branch. Snaps immediately when the target moved between frames (output
    /// growth re-anchored the offset) or once the follower settles within
    /// [`GLIDE_SETTLE_ROWS`] of the target.
    pub(super) fn update_scroll_glide(&mut self, now: Instant, cell_height: u32, logical: usize) {
        let cell_h = cell_height as f32;
        let token = self.sessions.active_id();
        let Some(session) = self.sessions.get_mut(token) else {
            return;
        };
        if !session.glide_active {
            return;
        }
        // A between-frame target change (output growth re-anchored the scrolled
        // viewport) is a snap site: land at the new logical offset immediately
        // rather than gliding across the re-anchoring.
        if session.glide_target != logical {
            session.glide_active = false;
            session.glide_last_tick = None;
            session.glide_visual = logical as f32;
            session.scroll_frac_offset = 0.0;
            return;
        }
        let dt = match session.glide_last_tick {
            Some(prev) => now.saturating_duration_since(prev),
            None => SCROLL_GLIDE_FRAME,
        };
        session.glide_last_tick = Some(now);
        let logical_f = logical as f32;
        let next = glide_step(session.glide_visual, logical_f, dt, SCROLL_GLIDE_TAU);
        if (next - logical_f).abs() < GLIDE_SETTLE_ROWS {
            session.glide_active = false;
            session.glide_last_tick = None;
            session.glide_visual = logical_f;
            session.scroll_frac_offset = 0.0;
        } else {
            session.glide_visual = next;
            session.scroll_frac_offset = (next - next.floor()) * cell_h;
        }
    }

    /// The scrollback offset the active session should SNAPSHOT at this frame:
    /// the glide follower's floored row while a glide is in flight (so the
    /// content_origin sub-row shift is always under one cell — no multi-row edge
    /// gap), else the passed-through logical offset, clamped to a valid offset.
    /// The logical `viewport` offset is unchanged — selection, scrollbar, and
    /// "at live bottom" all still read it; only the render snapshot follows the
    /// glide.
    pub(super) fn glide_render_offset(&self, logical: usize, scrollback_len: usize) -> usize {
        let token = self.sessions.active_id();
        match self.sessions.get(token) {
            Some(session) if session.glide_active => {
                (session.glide_visual.floor().max(0.0) as usize).min(scrollback_len)
            }
            _ => logical,
        }
    }

    /// Frame-paced animation wake while a glide is in flight (`None` at rest / on
    /// the off path), folded into [`App::animation_deadline`] so the maintenance
    /// pass repaints every frame until the follower settles.
    pub(super) fn scroll_glide_deadline(&self) -> Option<Instant> {
        let token = self.sessions.active_id();
        self.sessions
            .get(token)
            .filter(|session| session.glide_active)
            .map(|_| Instant::now() + SCROLL_GLIDE_FRAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Terminal;
    use std::sync::Mutex;

    // --- pure carry arithmetic (no App / no GPU) ----------------------------

    #[test]
    fn carry_keeps_the_sub_row_remainder_and_moves_whole_rows() {
        // 2.5 rows of travel from rest: two whole rows carry, half a row is kept
        // as the sub-row remainder (the notch coalescer used to truncate this).
        let (whole, frac) = carry_scroll_frac(0.0, 2.5);
        assert_eq!(whole, 2);
        assert!((frac - 0.5).abs() < 1e-6, "remainder kept: {frac}");

        // A further sub-row event crosses the next whole row and keeps the rest.
        let (whole, frac) = carry_scroll_frac(0.5, 0.6);
        assert_eq!(whole, 1);
        assert!((frac - 0.1).abs() < 1e-6, "remainder carried: {frac}");

        // Pure sub-notch travel moves the remainder with NO whole-row carry —
        // the visual glides by a fraction of a row while the integer offset is
        // unchanged. This is the sub-notch-produces-no-motion bug, gone.
        let (whole, frac) = carry_scroll_frac(0.0, 0.3);
        assert_eq!(whole, 0, "no whole-row move for sub-row travel");
        assert!((frac - 0.3).abs() < 1e-6);

        // Sign is preserved (negative = toward live).
        let (whole, frac) = carry_scroll_frac(0.0, -1.4);
        assert_eq!(whole, -1);
        assert!((frac + 0.4).abs() < 1e-6, "signed remainder: {frac}");

        // Zero travel is inert.
        assert_eq!(carry_scroll_frac(0.25, 0.0), (0, 0.25));
    }

    // --- App-level integration (real PTY, synthetic cell height) ------------

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

    /// Feed enough line feeds to push rows into scrollback so the viewport has
    /// history to glide through.
    fn seed_scrollback(app: &App) -> usize {
        if let Ok(mut t) = app.terminal.lock() {
            for _ in 0..40 {
                t.advance(b"line\r\n");
            }
            t.screen().scrollback_len()
        } else {
            0
        }
    }

    #[test]
    fn continuous_scroll_carries_whole_rows_and_keeps_the_fraction() {
        let Some(mut app) = build_app() else {
            return;
        };
        let sb = seed_scrollback(&app);
        assert!(sb >= 4, "need scrollback to glide through: {sb}");
        let token = app.sessions.active_id();
        // 2.5 rows up at a 16 px cell: two rows carry into the integer offset,
        // half a row remains as the sub-row pixel offset (0.5 * 16 = 8 px).
        app.drive_continuous_scroll(token, 40.0, 16);
        assert_eq!(app.viewport.offset(), 2, "two whole rows moved the offset");
        assert!(
            (app.scroll_frac_offset - 8.0).abs() < 1e-3,
            "half a row of sub-row offset: {}",
            app.scroll_frac_offset
        );
        assert_ne!(
            app.scroll_frac_bits(),
            0,
            "signature reflects the sub-row shift"
        );

        // A sub-row follow-up (0.25 row) moves ONLY the visual — the integer
        // offset does not change, but the pixel offset advances. Sub-notch pixel
        // travel now produces motion (it was truncated to nothing before).
        app.drive_continuous_scroll(token, 4.0, 16);
        assert_eq!(app.viewport.offset(), 2, "sub-row travel keeps the offset");
        assert!(
            (app.scroll_frac_offset - 12.0).abs() < 1e-3,
            "sub-row glide advanced: {}",
            app.scroll_frac_offset
        );
    }

    #[test]
    fn continuous_scroll_deposits_nothing_past_the_live_bottom() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed_scrollback(&app);
        let token = app.sessions.active_id();
        // Already at the live bottom (offset 0). Scrolling toward live (negative
        // travel) deposits nothing: no drift, no rubber-band.
        app.drive_continuous_scroll(token, -32.0, 16);
        assert_eq!(app.viewport.offset(), 0, "cannot move below the live row");
        assert_eq!(app.scroll_frac_offset, 0.0, "no residual sub-row offset");
        assert_eq!(app.scroll_frac_rows, 0.0);
    }

    #[test]
    fn continuous_scroll_arms_no_animation_wake() {
        // Fight-avoidance made structural: the ease is gone, so a continuous
        // scroll can never schedule an animation wake (the old bounce came from
        // re-arming an 80 ms ease on every notch). An idle app driven by the
        // continuous lane still reports no animation deadline.
        let Some(mut app) = build_app() else {
            return;
        };
        seed_scrollback(&app);
        let token = app.sessions.active_id();
        app.drive_continuous_scroll(token, 24.0, 16);
        assert_eq!(
            app.animation_deadline(),
            None,
            "the continuous lane sets the offset directly and schedules no ease wake"
        );
    }

    #[test]
    fn clear_scroll_frac_snaps_immediately() {
        let Some(mut app) = build_app() else {
            return;
        };
        seed_scrollback(&app);
        let token = app.sessions.active_id();
        app.drive_continuous_scroll(token, 40.0, 16);
        assert_ne!(
            app.scroll_frac_offset, 0.0,
            "precondition: mid sub-row glide"
        );
        app.clear_scroll_frac_of(token);
        assert_eq!(app.scroll_frac_offset, 0.0, "snap zeroes the pixel offset");
        assert_eq!(app.scroll_frac_rows, 0.0, "and the row remainder");
        assert_eq!(
            app.scroll_frac_bits(),
            0,
            "constant 0 in the signature at rest"
        );
    }

    #[test]
    fn pixel_scroll_off_makes_the_continuous_lane_ineligible() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert!(app.settings.pixel_scroll, "on by default");
        assert!(
            app.continuous_scroll_eligible(),
            "single-pane primary screen with the knob on is eligible"
        );
        app.settings.pixel_scroll = false;
        assert!(
            !app.continuous_scroll_eligible(),
            "with the knob off, PixelDelta falls back to the notch path"
        );
    }

    // --- SCROLL-GLIDE pure follower (no App / no GPU) -----------------------

    #[test]
    fn glide_step_is_a_monotonic_forward_chase() {
        let tau = Duration::from_millis(80);
        let dt = Duration::from_millis(16);
        // Chasing upward (logical > visual): each step advances toward 5 and
        // never overshoots or reverses. This is the anti-sawtooth invariant.
        let mut v = 0.0f32;
        for _ in 0..200 {
            let next = glide_step(v, 5.0, dt, tau);
            assert!(next > v - 1e-6, "never reverses: {next} vs {v}");
            assert!(next <= 5.0 + 1e-6, "never overshoots the target: {next}");
            v = next;
        }
        assert!((v - 5.0).abs() < 0.01, "converges to the target: {v}");

        // Chasing downward is symmetric (logical < visual): monotone decrease,
        // no undershoot past the target.
        let mut v = 5.0f32;
        for _ in 0..200 {
            let next = glide_step(v, 0.0, dt, tau);
            assert!(next < v + 1e-6, "never reverses downward: {next} vs {v}");
            assert!(next >= -1e-6, "never undershoots the target: {next}");
            v = next;
        }
        assert!(v.abs() < 0.01, "converges downward: {v}");
    }

    #[test]
    fn glide_step_bigger_dt_moves_further_but_never_past_target() {
        let tau = Duration::from_millis(80);
        // A large dt (a dropped-frame catch-up) moves most of the way in one
        // step, but the exponential factor stays < 1, so it never overshoots.
        let small = glide_step(0.0, 10.0, Duration::from_millis(8), tau);
        let big = glide_step(0.0, 10.0, Duration::from_millis(64), tau);
        assert!(
            big > small,
            "a longer frame advances further: {big} vs {small}"
        );
        assert!(big < 10.0, "even a long frame does not overshoot: {big}");
    }

    // --- SCROLL-GLIDE App integration (real PTY, synthetic cell height) -----

    #[test]
    fn scroll_glide_off_is_byte_identical() {
        let Some(mut app) = build_app() else {
            return;
        };
        let sb = seed_scrollback(&app);
        assert!(sb >= 5);
        // Exercise the OFF path explicitly (the default is now on).
        app.settings.scroll_glide = false;
        let token = app.sessions.active_id();
        app.scroll_viewport_of(token, 5);
        assert_eq!(app.viewport.offset(), 5, "the integer offset still jumps");
        assert!(!app.glide_active, "no glide armed on the off path");
        assert_eq!(
            app.scroll_frac_offset, 0.0,
            "no sub-row shift on the off path"
        );
        assert_eq!(app.scroll_frac_bits(), 0, "render signature bit stays 0");
        assert_eq!(
            app.glide_render_offset(5, sb),
            5,
            "render snapshots at the logical offset when no glide is armed"
        );
    }

    #[test]
    fn scroll_glide_on_arms_a_lagging_follower() {
        let Some(mut app) = build_app() else {
            return;
        };
        let sb = seed_scrollback(&app);
        assert!(sb >= 5);
        app.settings.scroll_glide = true;
        let token = app.sessions.active_id();
        app.scroll_viewport_of(token, 5);
        // The logical offset jumps instantly; the follower lags at the old
        // position and the render snapshots there until it eases up.
        assert_eq!(app.viewport.offset(), 5, "logical offset is instant");
        assert!(app.glide_active, "the glide is armed");
        assert!((app.glide_visual - 0.0).abs() < 1e-6, "follower lags at 0");
        assert_eq!(
            app.glide_render_offset(5, sb),
            0,
            "render snapshots at the follower's floored row, not the logical row"
        );
    }

    #[test]
    fn glide_follower_eases_toward_logical_and_settles() {
        let Some(mut app) = build_app() else {
            return;
        };
        let sb = seed_scrollback(&app);
        assert!(sb >= 5);
        app.settings.scroll_glide = true;
        let token = app.sessions.active_id();
        app.scroll_viewport_of(token, 5);
        assert!(app.glide_active);

        let base = Instant::now();
        let mut prev = app.glide_visual;
        let mut settled = false;
        for i in 1..=400u32 {
            app.update_scroll_glide(base + Duration::from_millis(16 * i as u64), 16, 5);
            if !app.glide_active {
                settled = true;
                break;
            }
            assert!(
                app.glide_visual >= prev - 1e-6,
                "follower only moves toward the target: {} vs {prev}",
                app.glide_visual
            );
            assert!(app.glide_visual <= 5.0 + 1e-6, "never overshoots");
            prev = app.glide_visual;
        }
        assert!(settled, "the glide settles in bounded time");
        assert_eq!(
            app.viewport.offset(),
            5,
            "logical offset unchanged by the glide"
        );
        assert!(
            (app.glide_visual - 5.0).abs() < 1e-6,
            "follower lands on the target"
        );
        assert_eq!(
            app.scroll_frac_offset, 0.0,
            "no residual sub-row shift at rest"
        );
        assert_eq!(app.scroll_glide_deadline(), None, "no wake once settled");
    }

    #[test]
    fn viewport_change_snaps_the_glide() {
        let Some(mut app) = build_app() else {
            return;
        };
        let sb = seed_scrollback(&app);
        assert!(sb >= 5);
        app.settings.scroll_glide = true;
        let token = app.sessions.active_id();
        app.scroll_viewport_of(token, 5);
        assert!(app.glide_active, "precondition: glide armed");
        // Any non-scroll viewport change (here, return-to-live on input) snaps
        // the follower to the exact offset — no gliding a programmatic jump.
        app.return_to_live();
        assert!(!app.glide_active, "return-to-live snaps the glide");
        assert_eq!(app.scroll_frac_offset, 0.0, "snap zeroes the sub-row shift");
        assert_eq!(app.viewport.offset(), 0, "back at the live tail");
    }

    #[test]
    fn glide_lag_is_clamped_to_one_viewport_height() {
        let Some(mut app) = build_app() else {
            return;
        };
        let sb = seed_scrollback(&app);
        assert!(sb >= 5);
        app.settings.scroll_glide = true;
        let token = app.sessions.active_id();
        app.scroll_viewport_of(token, 5);
        let vp = app.grid.rows.max(1) as f32;
        let logical = app.viewport.offset() as f32;
        // A start far beyond one viewport of lag: the clamp caps the follower's
        // starting lag at one viewport height so a rapid burst cannot leave a
        // long laggy crawl. visual starts exactly `logical - vp` behind.
        app.arm_scroll_glide_of(token, logical - vp * 10.0);
        assert!(app.glide_active);
        assert!(
            (app.glide_visual - (logical - vp)).abs() < 1e-6,
            "lag clamped to one viewport height ({vp}): {}",
            app.glide_visual
        );
    }
}

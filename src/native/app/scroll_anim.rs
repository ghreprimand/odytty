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
}

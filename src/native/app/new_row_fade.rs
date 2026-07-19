// SPDX-License-Identifier: GPL-3.0-only
//! VE4 new-output fade-in — rows of freshly arrived output at the live tail
//! fade their TEXT in over a short ease-out ramp instead of appearing
//! instantly. Cell backgrounds render exactly as normal from the first frame;
//! only the foreground ink (glyphs, combining marks, ligatures, underline and
//! strikethrough decorations, color glyphs) ramps. The original mechanism — an
//! opaque background-color veil quad decaying over each new row — read as a
//! dark from-black flash on translucent windows (the veil started at alpha 1.0
//! while the surrounding cell backgrounds composed the window opacity), so it
//! was replaced by this per-row foreground alpha ramp.
//!
//! Row tracking is app-side scrollback-delta (D-VE4F-1): at the live tail
//! (`viewport_offset == 0`) every new line of output grows `scrollback_len` by
//! exactly one as the oldest visible row scrolls into scrollback, so the
//! `scrollback_len` delta since the previous rebuild (capped at `grid.rows`) is
//! the count of newly arrived rows entering the bottom of the viewport. The
//! bottom `delta` entries of [`App::row_fade_starts`] are stamped `Some(now)`;
//! older entries shift upward to follow their rows.
//!
//! RV1 interaction (D-VE4F-4 revised): the veil satisfied the readability
//! floor by construction (fully rendered ink underneath, merely obscured). The
//! text ramp instead starts at [`NEW_ROW_FADE_MIN_ALPHA`] — never 0 — and
//! resolves to the exact RV1-floored color within the configured ramp
//! (`new_output_fade_ms`, 1s max). A short, bounded, operator-opt-in ramp from
//! a visible floor to the floored steady state satisfies the RV1 contract's
//! intent: steady-state readability is untouched, and no frame ever renders
//! invisible ink.
//!
//! Off-path contract (D-VE4F-6 / §7): with `new_output_fade` off,
//! [`App::update_row_fade`] clears `row_fade_starts` and returns immediately, so
//! the vector is always empty, [`App::new_row_fade_deadline`] is `None` (no
//! extra wakes), [`App::new_row_fade_text_multipliers`] answers `None` (the
//! vertex builders take their exact inert path), and
//! [`App::new_row_fade_overlay_signature`] is constant `Inert` — the default
//! render path is byte-identical to before this feature existed.

use super::*;

/// Animation cadence (~60 fps): the wake interval while a fade is in flight.
const FADE_FRAME: Duration = Duration::from_millis(16);

/// The foreground alpha a freshly arrived row starts at. The ramp runs
/// `floor -> 1.0` on the ease-out curve — never from 0 — so new text is
/// visible from its very first frame and the fade reads as ink developing,
/// not content blinking into existence (RV1 interaction, module docs).
pub(in crate::native) const NEW_ROW_FADE_MIN_ALPHA: f32 = 0.25;

/// Ease-out cubic: fast departure, gentle arrival. Maps `0.0..=1.0` to itself.
/// Local copy (the cursor-slide module's identical helper is module-private) so
/// this feature lives entirely in its own lane.
fn ease_out_cubic(p: f32) -> f32 {
    let inv = 1.0 - p;
    1.0 - inv * inv * inv
}

impl App {
    /// Fade ramp length from the live `new_output_fade_ms` setting (D-VE4F-5): a
    /// freshly arrived row is fully revealed this long after it appears. Single
    /// source for the settle test, the wake deadline, and the quad alpha math.
    /// Changing the slider mid-fade retimes any in-flight fades on the next
    /// rebuild, which is accepted: the ramp is short and the retime is harmless.
    fn new_output_fade_duration(&self) -> Duration {
        Duration::from_millis(self.settings.new_output_fade_ms as u64)
    }

    /// Refresh the per-row fade-start instants once per rebuild, before the
    /// overlay-quad emission. Off / scrolled-back / geometry-changed all snap
    /// (clear, no fade); only the live tail with the feature on stamps new
    /// rows. Bumps [`App::row_fade_epoch`] whenever a fade is still active so
    /// each animation frame reclassifies the render cache (the quad alphas move
    /// while the cell content does not).
    pub(in crate::native) fn update_row_fade(&mut self, now: Instant, scrollback_len: usize) {
        // Off path or scrolled back into history (D-VE4F-3): never populate;
        // snap to the live content and record the baseline length.
        if self.settings.reduced_motion
            || !self.settings.new_output_fade
            || self.viewport.offset() > 0
        {
            self.row_fade_starts.clear();
            self.last_scrollback_len_for_fade = scrollback_len;
            return;
        }
        let rows = self.grid.rows;
        // Geometry discontinuity (resize / first frame, D-VE4F-11): resize the
        // tracking vector and snap — a new grid is not a stream of new output.
        if self.row_fade_starts.len() != rows {
            self.row_fade_starts.clear();
            self.row_fade_starts.resize(rows, None);
            self.last_scrollback_len_for_fade = scrollback_len;
            return;
        }
        // New rows = the scrollback growth since the previous rebuild, capped at
        // the grid height (a burst larger than the viewport fades at most every
        // visible row once). D-VE4F-2.
        let delta = scrollback_len
            .saturating_sub(self.last_scrollback_len_for_fade)
            .min(rows);
        if delta > 0 {
            // Shift existing fades up to follow their rows, then stamp the new
            // bottom `delta` rows.
            self.row_fade_starts.rotate_left(delta);
            let len = self.row_fade_starts.len();
            for entry in &mut self.row_fade_starts[len - delta..] {
                *entry = Some(now);
            }
        }
        self.last_scrollback_len_for_fade = scrollback_len;
        // Settle finished rows so the deadline + signature go idle once every
        // fade completes (bounded wake: the loop stops waking when nothing is
        // animating).
        let fade = self.new_output_fade_duration();
        let mut any_active = false;
        for entry in self.row_fade_starts.iter_mut() {
            if let Some(start) = *entry {
                if now.saturating_duration_since(start) >= fade {
                    *entry = None;
                } else {
                    any_active = true;
                }
            }
        }
        if any_active {
            self.row_fade_epoch = self.row_fade_epoch.wrapping_add(1);
        }
    }

    /// Next fade wake, or `None` once every row settles (and always `None` on
    /// the off path, where `row_fade_starts` is empty). Folded into
    /// [`App::animation_deadline`] as the fourth contributor — it joins the
    /// existing single control-flow timer rather than adding a second.
    pub(super) fn new_row_fade_deadline(&self) -> Option<Instant> {
        if self.settings.reduced_motion || self.row_fade_starts.is_empty() {
            return None;
        }
        let fade = self.new_output_fade_duration();
        self.row_fade_starts
            .iter()
            .flatten()
            .map(|&start| (start + fade).min(Instant::now() + FADE_FRAME))
            .min()
    }

    /// Render-cache fragment. `NewRowFade { epoch }` while any row is mid-fade
    /// (the per-rebuild epoch bump makes every animation frame reclassify),
    /// `Inert` otherwise — so the off path is a frame-to-frame constant and the
    /// geometry-update decision is unchanged from before this field existed.
    pub(super) fn new_row_fade_overlay_signature(&self) -> OverlayFragment {
        if !self.settings.reduced_motion && self.row_fade_starts.iter().any(Option::is_some) {
            OverlayFragment::NewRowFade {
                epoch: self.row_fade_epoch,
            }
        } else {
            OverlayFragment::Inert
        }
    }

    /// Per-content-row FOREGROUND alpha multipliers for this frame, or `None`
    /// when no row is mid-fade (the off path, reduced motion, and every
    /// settled frame — the vertex builders then take their exact inert path).
    ///
    /// A fading row's multiplier ramps [`NEW_ROW_FADE_MIN_ALPHA`]` -> 1.0` on
    /// the ease-out cubic over the `new_output_fade_ms` ramp; non-fading rows
    /// answer `1.0`. The cursor's row is never faded (D-VE4F-9) so the live
    /// prompt renders at full strength. Index = viewport content row; the
    /// render dispatch maps it into decorated-snapshot coordinates via the
    /// chrome row/column offsets.
    pub(in crate::native) fn new_row_fade_text_multipliers(
        &self,
        now: Instant,
        cursor_row: usize,
    ) -> Option<Vec<f32>> {
        if self.settings.reduced_motion || self.row_fade_starts.is_empty() {
            return None;
        }
        let fade = self.new_output_fade_duration();
        let mut multipliers = vec![1.0_f32; self.row_fade_starts.len()];
        let mut any_active = false;
        for (row, &start_opt) in self.row_fade_starts.iter().enumerate() {
            let Some(start) = start_opt else { continue };
            // Cursor-row exception (D-VE4F-9).
            if row == cursor_row {
                continue;
            }
            let elapsed = now.saturating_duration_since(start);
            if elapsed >= fade {
                continue;
            }
            let p = (elapsed.as_secs_f32() / fade.as_secs_f32()).min(1.0);
            multipliers[row] =
                NEW_ROW_FADE_MIN_ALPHA + (1.0 - NEW_ROW_FADE_MIN_ALPHA) * ease_out_cubic(p);
            any_active = true;
        }
        any_active.then_some(multipliers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::app::overlay_registry::OverlayCtx;

    const CELL_W: u32 = 8;
    const CELL_H: u32 = 16;
    const ROWS: usize = 6;
    const COLS: usize = 40;

    // --- pure curve properties (no App) -------------------------------------

    #[test]
    fn ease_out_cubic_endpoints_and_monotonic() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        // Strictly increasing across the ramp.
        let mut prev = ease_out_cubic(0.0);
        for i in 1..=10 {
            let v = ease_out_cubic(i as f32 / 10.0);
            assert!(v > prev, "ease must increase: {v} <= {prev}");
            prev = v;
        }
    }

    // --- App-level (headless, no real PTY) ----------------------------------

    fn build_app() -> Option<App> {
        let d = Dimensions::new(COLS, ROWS);
        let (mut app, _terminal) = crate::native::test_support::headless_app_with(
            crate::native::options::NativeOptions::default(),
            d,
            Settings::default(),
        );
        // Pin a deterministic grid + cell so row geometry is known.
        app.grid = d;
        app.set_test_cell_for_test(CellSize {
            width: CELL_W,
            height: CELL_H,
            baseline: 0,
        });
        Some(app)
    }

    fn ctx_at(app: &App, cursor_row: usize, now: Instant) -> OverlayCtx {
        app.overlay_ctx(
            app.scrollback_len(),
            CellSize {
                width: CELL_W,
                height: CELL_H,
                baseline: 0,
            },
            crate::core::Position {
                row: cursor_row,
                column: 0,
            },
            true,
            now,
        )
    }

    // --- T1: off-path identity ----------------------------------------------

    #[test]
    fn off_path_never_populates_and_is_inert() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert!(!app.settings.new_output_fade, "off by default");
        // Even a large scrollback advance must not stamp any fade while off.
        app.update_row_fade(Instant::now(), 50);
        assert!(
            app.row_fade_starts.is_empty(),
            "off path leaves the tracker empty"
        );
        assert_eq!(app.new_row_fade_deadline(), None, "no wake while off");
        assert_eq!(
            app.new_row_fade_overlay_signature(),
            OverlayFragment::Inert,
            "signature constant-Inert while off"
        );
        assert_eq!(
            app.new_row_fade_text_multipliers(Instant::now(), 0),
            None,
            "off path exposes no text-ramp multipliers"
        );
    }

    #[test]
    fn reduced_motion_snaps_output_fade_and_suppresses_wakes() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        app.update_row_fade(t0 + Duration::from_millis(1), 2);
        assert!(
            app.new_row_fade_deadline().is_some(),
            "fade is initially live"
        );

        app.settings.reduced_motion = true;
        app.update_row_fade(t0 + Duration::from_millis(2), 2);

        assert!(
            app.settings.new_output_fade,
            "stored preference is unchanged"
        );
        assert!(
            app.row_fade_starts.is_empty(),
            "reduced motion clears the fade"
        );
        assert_eq!(
            app.new_row_fade_deadline(),
            None,
            "reduced motion adds no wake"
        );
        assert_eq!(
            app.new_row_fade_overlay_signature(),
            OverlayFragment::Inert,
            "reduced motion makes the overlay inert"
        );
        assert_eq!(
            app.new_row_fade_text_multipliers(t0 + Duration::from_millis(3), 0),
            None,
            "reduced motion exposes no text-ramp multipliers"
        );
    }

    #[test]
    fn reduced_motion_makes_cursor_effects_static_and_inert() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_motion = true;
        app.settings.cursor_trail = true;
        app.settings.cursor_glow = true;
        app.settings.cursor_easing = true;
        app.cursor_anim_alpha = 0.5;
        app.cursor_ease_deadline = Some(Instant::now() + Duration::from_millis(16));
        app.cursor_anim_offset = [4.0, 0.0];
        app.cursor_slide_from_px = [8.0, 0.0];
        app.cursor_slide_start = Some(Instant::now());
        app.cursor_slide_deadline = Some(Instant::now() + Duration::from_millis(16));

        app.settings.reduced_motion = true;

        let params = app.cursor_render_params();
        assert_eq!(params.offset, [0.0, 0.0], "reduced motion snaps the cursor");
        assert_eq!(params.alpha, 1.0, "reduced motion disables blink fading");
        assert_eq!(app.cursor_blink_fade_deadline(), None, "no easing wake");
        assert_eq!(app.cursor_motion_deadline(), None, "no slide wake");
        assert_eq!(
            app.focused_cursor_animation_deadline(),
            None,
            "no focused split-pane wake"
        );
        assert_eq!(
            app.cursor_trail_overlay_signature(),
            OverlayFragment::Inert,
            "trail is inert"
        );
        assert_eq!(
            app.cursor_glow_overlay_signature(),
            OverlayFragment::Inert,
            "glow is inert"
        );
        let mut effects = Vec::new();
        let ctx = ctx_at(&app, 0, Instant::now());
        app.paint_cursor_trail_quads(&ctx, &mut effects);
        assert!(effects.is_empty(), "reduced motion emits no cursor effects");
        assert!(
            app.cursor_glow_request([0.0, 0.0, 100.0, 100.0]).is_none(),
            "reduced motion emits no aura request"
        );

        let now = Instant::now();
        app.update_cursor_easing(now, false, true);
        let snapshot = crate::core::Snapshot {
            dimensions: Dimensions::new(COLS, ROWS),
            cursor: crate::core::Position { row: 0, column: 1 },
            cursor_visible: true,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); COLS * ROWS],
        };
        app.update_cursor_motion(
            now,
            &snapshot,
            CellSize {
                width: CELL_W,
                height: CELL_H,
                baseline: 0,
            },
        );
        assert_eq!(app.cursor_anim_alpha, 1.0, "easing state resets");
        assert_eq!(app.cursor_ease_deadline, None, "easing timer clears");
        assert_eq!(app.cursor_anim_offset, [0.0, 0.0], "slide state resets");
        assert_eq!(app.cursor_slide_deadline, None, "slide timer clears");
        assert_eq!(app.cursor_slide_start, None, "slide start clears");
        assert!(app.settings.cursor_motion);
        assert!(app.settings.cursor_trail);
        assert!(app.settings.cursor_glow);
        assert!(app.settings.cursor_easing);
    }

    // --- T5: scrollback-delta tracking correctness --------------------------

    #[test]
    fn delta_marks_exactly_bottom_rows_and_rotates() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        // First call with a mismatched length resizes + snaps (no fade).
        app.update_row_fade(t0, 0);
        assert_eq!(app.row_fade_starts.len(), ROWS);
        assert!(app.row_fade_starts.iter().all(Option::is_none));

        // +3 rows: exactly the bottom 3 are stamped.
        let t1 = t0 + Duration::from_millis(1);
        app.update_row_fade(t1, 3);
        let marked: Vec<bool> = app.row_fade_starts.iter().map(Option::is_some).collect();
        assert_eq!(marked, vec![false, false, false, true, true, true]);

        // +2 more rows: older fades shift up by 2, the new bottom 2 stamped now.
        let t2 = t1 + Duration::from_millis(1);
        app.update_row_fade(t2, 5);
        let marked: Vec<bool> = app.row_fade_starts.iter().map(Option::is_some).collect();
        assert_eq!(marked, vec![false, true, true, true, true, true]);
        // The two newest rows carry the newer instant; the three older carry t1.
        assert_eq!(app.row_fade_starts[4], Some(t2));
        assert_eq!(app.row_fade_starts[5], Some(t2));
        assert_eq!(app.row_fade_starts[1], Some(t1));
    }

    #[test]
    fn delta_is_capped_at_grid_rows() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        // A burst larger than the viewport fades at most every visible row once.
        let t1 = t0 + Duration::from_millis(1);
        app.update_row_fade(t1, 1000);
        assert!(
            app.row_fade_starts.iter().all(|e| *e == Some(t1)),
            "every row fades, none double-stamped"
        );
    }

    // --- settle + deadline + signature epoch --------------------------------

    #[test]
    fn settle_clears_and_returns_to_idle() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        let t1 = t0 + Duration::from_millis(1);
        app.update_row_fade(t1, 2);
        assert!(app.new_row_fade_deadline().is_some(), "wake while fading");
        assert!(matches!(
            app.new_row_fade_overlay_signature(),
            OverlayFragment::NewRowFade { .. }
        ));
        let epoch_a = app.row_fade_epoch;
        // A frame still mid-fade bumps the epoch (forces reclassification).
        app.update_row_fade(t1 + Duration::from_millis(16), 2);
        assert!(app.row_fade_epoch > epoch_a, "epoch advances while fading");
        // Past the fade ramp (default 250 ms) every row settles → idle.
        app.update_row_fade(t1 + Duration::from_millis(300), 2);
        assert!(
            app.row_fade_starts.iter().all(Option::is_none),
            "settled rows clear"
        );
        assert_eq!(app.new_row_fade_deadline(), None, "idle: no wake");
        assert_eq!(
            app.new_row_fade_overlay_signature(),
            OverlayFragment::Inert,
            "idle: signature back to Inert"
        );
        // Settle-to-identical: once every row settles the render dispatch hands
        // the builders `None`, whose vertex output is byte-identical to a
        // never-faded frame (pinned at the grid layer).
        assert_eq!(
            app.new_row_fade_text_multipliers(t1 + Duration::from_millis(300), 0),
            None,
            "settled: no multipliers, builders take the inert path"
        );
    }

    #[test]
    fn fade_duration_setting_drives_the_settle_and_deadline_boundary() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        // A non-default ramp: the settle boundary and the deadline must track it,
        // not the old fixed 120 ms.
        app.settings.new_output_fade_ms = 500.0;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        let t1 = t0 + Duration::from_millis(1);
        app.update_row_fade(t1, 2);
        assert!(app.new_row_fade_deadline().is_some(), "fade live at 500 ms");

        // 300 ms in is still mid-ramp for a 500 ms fade: rows stay active.
        app.update_row_fade(t1 + Duration::from_millis(300), 2);
        assert!(
            app.row_fade_starts.iter().any(Option::is_some),
            "rows still fading before the 500 ms ramp ends"
        );

        // Past 500 ms every row settles.
        app.update_row_fade(t1 + Duration::from_millis(600), 2);
        assert!(
            app.row_fade_starts.iter().all(Option::is_none),
            "rows settle once the configured ramp elapses"
        );
        assert_eq!(app.new_row_fade_deadline(), None, "idle after the ramp");
    }

    #[test]
    fn fade_duration_setting_scales_the_text_multiplier() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        app.settings.new_output_fade_ms = 500.0;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        app.update_row_fade(t0, 1); // one fresh row stamped t0 (bottom row)
        // At 120 ms the row is 24% through a 500 ms ramp, so its text is still
        // clearly mid-ramp; under the old fixed 120 ms it would already be at
        // full strength. Cursor parked on row 0 so the fading row is not exempt.
        let m = app
            .new_row_fade_text_multipliers(t0 + Duration::from_millis(120), 0)
            .expect("fade in flight");
        let fading = m[ROWS - 1];
        assert!(
            fading < 0.9,
            "still substantially mid-ramp partway through the longer ramp: {fading}"
        );
        assert!(
            fading >= NEW_ROW_FADE_MIN_ALPHA,
            "never below the readability floor: {fading}"
        );
    }

    #[test]
    fn fade_deadline_joins_animation_aggregator() {
        let Some(mut app) = build_app() else {
            return;
        };
        // Cursor animation knobs off ⇒ the aggregate equals the fade deadline.
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        assert_eq!(
            app.animation_deadline(),
            None,
            "no fade in flight ⇒ no aggregate wake"
        );
        app.update_row_fade(t0 + Duration::from_millis(1), 2);
        assert!(
            app.animation_deadline().is_some(),
            "fade in flight contributes to the single animation timer"
        );
    }

    // --- T3: viewport-offset + resize snap ----------------------------------

    #[test]
    fn scrolled_back_clears_fade() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        app.update_row_fade(t0 + Duration::from_millis(1), 3);
        assert!(app.row_fade_starts.iter().any(Option::is_some));
        // Scroll back into history: the next update must snap (clear) — no fade
        // painted over historical scrollback.
        app.viewport.scroll_up(5, 100);
        assert!(app.viewport.offset() > 0);
        app.update_row_fade(t0 + Duration::from_millis(2), 100);
        assert!(
            app.row_fade_starts.is_empty(),
            "scrolled-back snaps the tracker"
        );
        assert_eq!(app.new_row_fade_deadline(), None);
    }

    #[test]
    fn geometry_change_snaps() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        app.update_row_fade(t0 + Duration::from_millis(1), 3);
        assert_eq!(app.row_fade_starts.len(), ROWS);
        // A resize is a discontinuity: the tracker resizes and snaps (no fade).
        app.grid = Dimensions::new(COLS, ROWS + 4);
        app.update_row_fade(t0 + Duration::from_millis(2), 3);
        assert_eq!(app.row_fade_starts.len(), ROWS + 4);
        assert!(
            app.row_fade_starts.iter().all(Option::is_none),
            "new geometry starts un-faded"
        );
    }

    // --- T4: cursor-row exception -------------------------------------------

    #[test]
    fn cursor_row_is_never_faded() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        // Fade every row.
        app.update_row_fade(t0 + Duration::from_millis(1), 1000);
        let cursor_row = 4;
        let now = t0 + Duration::from_millis(2);
        let m = app
            .new_row_fade_text_multipliers(now, cursor_row)
            .expect("fade in flight");
        assert_eq!(m.len(), ROWS);
        assert_eq!(m[cursor_row], 1.0, "cursor row renders at full strength");
        for (row, &mul) in m.iter().enumerate() {
            if row != cursor_row {
                assert!(mul < 1.0, "row {row} is mid-ramp: {mul}");
            }
        }
    }

    // --- T2/T7: floor-start + monotonic ramp (RV1 interaction) --------------

    #[test]
    fn text_ramp_starts_at_floor_and_rises_monotonically() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        app.update_row_fade(t0, 1); // one fresh row at the bottom, stamped t0
        let row = ROWS - 1;
        // Fresh (t == now): the multiplier starts AT the floor — never 0, so
        // new text is visible from its very first frame.
        let m0 = app
            .new_row_fade_text_multipliers(t0, 0)
            .expect("fade in flight")[row];
        assert!(
            (m0 - NEW_ROW_FADE_MIN_ALPHA).abs() < 1e-3,
            "starts at floor"
        );
        // The ramp rises monotonically toward 1.0 and never dips below floor.
        let mut prev = m0;
        for ms in [30_u64, 60, 90, 120, 180, 240] {
            let Some(m) = app.new_row_fade_text_multipliers(t0 + Duration::from_millis(ms), 0)
            else {
                break; // past the ramp: settled to the identical frame
            };
            let cur = m[row];
            assert!(cur >= prev - 1e-4, "ramp rises: {cur} >= {prev}");
            assert!(
                (NEW_ROW_FADE_MIN_ALPHA..=1.0).contains(&cur),
                "within [floor, 1.0]: {cur}"
            );
            prev = cur;
        }
    }

    // --- NF21-P4: switch-back / multipane viewport bookkeeping --------------

    // NF21-10: the shared per-pane render helper anchors a scrolled-back pane
    // across output growth and keeps its growth baseline current, so a split /
    // background pane stays pinned to its rows (the single-pane and multipane
    // paths now share this exact bookkeeping). Fails before the helper wired the
    // multipane loop into the same anchoring the single-pane path always had.
    #[test]
    fn anchor_viewport_for_render_stays_scrolled_and_tracks_baseline() {
        let Some(mut app) = build_app() else {
            return;
        };
        // Scroll the active pane back into history and record its baseline.
        app.viewport.scroll_up(5, 100);
        app.last_scrollback_len = 100;
        assert_eq!(app.viewport.offset(), 5);
        // 10 rows of new output arrive while scrolled back: the pane stays pinned
        // to the same absolute rows (offset += delta) and the baseline advances.
        let offset = app.anchor_viewport_for_render(110);
        assert_eq!(offset, 15, "stayed scrolled: 5 + 10 new rows");
        assert_eq!(app.last_scrollback_len, 110, "baseline advanced");
        // No further growth on the next rebuild: no movement, no accrued jump.
        let offset2 = app.anchor_viewport_for_render(110);
        assert_eq!(offset2, 15);
        assert_eq!(app.last_scrollback_len, 110);
        // Back at the live tail, fresh output is NOT anchored (appears at the
        // bottom immediately), but the baseline still tracks.
        app.viewport.reset_to_live();
        let live = app.anchor_viewport_for_render(130);
        assert_eq!(live, 0, "live tail: new output is not anchored");
        assert_eq!(app.last_scrollback_len, 130);
    }

    // NF21-12: switching to a session that grew while backgrounded must not fade
    // in a whole viewport — an activation snaps the new-output fade tracker like
    // a resize does. Fails before `on_active_session_changed` cleared the fade.
    #[test]
    fn activation_snaps_new_output_fade() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        // Simulate an in-flight fade left on the session being (re)activated.
        let rows = app.grid.rows.max(1);
        app.row_fade_starts = vec![Some(Instant::now()); rows];
        assert!(app.row_fade_starts.iter().any(Option::is_some));
        app.on_active_session_changed();
        assert!(
            app.row_fade_starts.is_empty(),
            "activation snaps (clears) the fade tracker"
        );
    }
}

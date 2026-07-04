// SPDX-License-Identifier: GPL-3.0-only
//! VE4 new-output fade-in — rows of freshly arrived output at the live tail
//! fade in over a short ease-out ramp instead of appearing instantly.
//!
//! Row tracking is app-side scrollback-delta (D-VE4F-1): at the live tail
//! (`viewport_offset == 0`) every new line of output grows `scrollback_len` by
//! exactly one as the oldest visible row scrolls into scrollback, so the
//! `scrollback_len` delta since the previous rebuild (capped at `grid.rows`) is
//! the count of newly arrived rows entering the bottom of the viewport. The
//! bottom `delta` entries of [`App::row_fade_starts`] are stamped `Some(now)`;
//! older entries shift upward to follow their rows.
//!
//! RV1 safety is by construction (D-VE4F-4 / T7): the fade is painted as a
//! background-color [`SolidQuad`] overlay that decays from opaque to
//! transparent over each fading row. The underlying cell content is always
//! rendered at full opacity (already RV1-floored); the quad merely *obscures
//! then reveals* it, so no intermediate frame ever drops the foreground below
//! the floor.
//!
//! Off-path contract (D-VE4F-6 / §7): with `new_output_fade` off,
//! [`App::update_row_fade`] clears `row_fade_starts` and returns immediately, so
//! the vector is always empty, [`App::new_row_fade_deadline`] is `None` (no
//! extra wakes), [`App::paint_new_row_fade_quads`] emits nothing, and
//! [`App::new_row_fade_overlay_signature`] is constant `Inert` — the default
//! render path is byte-identical to before this feature existed.

use super::overlay_registry::OverlayCtx;
use super::*;

/// Hard cap on the fade ramp (D-VE4F-5): a freshly arrived row is fully revealed
/// 120 ms after it appears.
pub(super) const FADE_DURATION: Duration = Duration::from_millis(120);
/// Animation cadence (~60 fps): the wake interval while a fade is in flight.
const FADE_FRAME: Duration = Duration::from_millis(16);

/// Ease-out cubic: fast departure, gentle arrival. Maps `0.0..=1.0` to itself.
/// Local copy (the cursor-slide module's identical helper is module-private) so
/// this feature lives entirely in its own lane.
fn ease_out_cubic(p: f32) -> f32 {
    let inv = 1.0 - p;
    1.0 - inv * inv * inv
}

impl App {
    /// Refresh the per-row fade-start instants once per rebuild, before the
    /// overlay-quad emission. Off / scrolled-back / geometry-changed all snap
    /// (clear, no fade); only the live tail with the feature on stamps new
    /// rows. Bumps [`App::row_fade_epoch`] whenever a fade is still active so
    /// each animation frame reclassifies the render cache (the quad alphas move
    /// while the cell content does not).
    pub(in crate::native) fn update_row_fade(&mut self, now: Instant, scrollback_len: usize) {
        // Off path or scrolled back into history (D-VE4F-3): never populate;
        // snap to the live content and record the baseline length.
        if !self.settings.new_output_fade || self.viewport.offset() > 0 {
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
        let mut any_active = false;
        for entry in self.row_fade_starts.iter_mut() {
            if let Some(start) = *entry {
                if now.saturating_duration_since(start) >= FADE_DURATION {
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
        if self.row_fade_starts.is_empty() {
            return None;
        }
        self.row_fade_starts
            .iter()
            .flatten()
            .map(|&start| (start + FADE_DURATION).min(Instant::now() + FADE_FRAME))
            .min()
    }

    /// Render-cache fragment. `NewRowFade { epoch }` while any row is mid-fade
    /// (the per-rebuild epoch bump makes every animation frame reclassify),
    /// `Inert` otherwise — so the off path is a frame-to-frame constant and the
    /// geometry-update decision is unchanged from before this field existed.
    pub(super) fn new_row_fade_overlay_signature(&self) -> OverlayFragment {
        if self.row_fade_starts.iter().any(Option::is_some) {
            OverlayFragment::NewRowFade {
                epoch: self.row_fade_epoch,
            }
        } else {
            OverlayFragment::Inert
        }
    }

    /// Emit one background-color [`SolidQuad`] per fading row, alpha decaying
    /// from opaque (just arrived) to transparent (settled) over [`FADE_DURATION`]
    /// on an ease-out cubic curve. The cursor's row is never obscured
    /// (D-VE4F-9) so the live prompt stays visible. No-op while
    /// `row_fade_starts` is empty (the off path), so the default render path
    /// emits zero quads.
    pub(in crate::native) fn paint_new_row_fade_quads(
        &self,
        ctx: &OverlayCtx,
        out: &mut Vec<SolidQuad>,
    ) {
        if self.row_fade_starts.is_empty() {
            return;
        }
        let cols = ctx.grid.columns;
        let rows = ctx.grid.rows;
        if cols == 0 || rows == 0 {
            return;
        }
        let pad = ctx.window_padding.as_f32();
        let cell_w = ctx.cell.width as f32;
        let cell_h = ctx.cell.height as f32;
        let content_w = cols as f32 * cell_w;
        let cursor_row = ctx.cursor.row;
        for (row, &start_opt) in self.row_fade_starts.iter().enumerate() {
            let Some(start) = start_opt else { continue };
            // Defensive clamp + cursor-row exception.
            if row >= rows || row == cursor_row {
                continue;
            }
            let elapsed = ctx.now.saturating_duration_since(start);
            if elapsed >= FADE_DURATION {
                continue;
            }
            let p = (elapsed.as_secs_f32() / FADE_DURATION.as_secs_f32()).min(1.0);
            let alpha = 1.0 - ease_out_cubic(p);
            let mut color = ctx.clear_color;
            color[3] = alpha;
            let y0 = pad + row as f32 * cell_h;
            out.push(SolidQuad {
                rect: [pad, y0, pad + content_w, y0 + cell_h],
                color,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Terminal;

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

    // --- App-level (skips when no PTY is available) -------------------------

    fn build_app() -> Option<App> {
        let d = Dimensions::new(COLS, ROWS);
        let session = crate::native::test_support::spawn_test_pause_shell(d).ok()?;
        let writer: crate::native::pty::PtyWriter =
            Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(d.columns, d.rows)));
        let pty = Arc::new(Mutex::new(session));
        let mut app = App::new(
            crate::native::options::NativeOptions::default(),
            terminal,
            writer,
            pty,
            Settings::default(),
            crate::settings::SettingsReloader::for_current_process(Instant::now()),
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
        let mut quads = Vec::new();
        app.paint_new_row_fade_quads(&ctx_at(&app, 0, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "off path emits zero quads");
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
        // Past the 120 ms cap every row settles → idle.
        app.update_row_fade(t1 + Duration::from_millis(200), 2);
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
    fn cursor_row_is_never_obscured() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        // Fade every row.
        app.update_row_fade(t0 + Duration::from_millis(1), 1000);
        let cursor_row = 4;
        let mut quads = Vec::new();
        let now = t0 + Duration::from_millis(2);
        app.paint_new_row_fade_quads(&ctx_at(&app, cursor_row, now), &mut quads);
        // One quad per fading row except the cursor's row.
        assert_eq!(quads.len(), ROWS - 1, "cursor row skipped");
        let pad = 0.0_f32;
        let cursor_y0 = pad + cursor_row as f32 * CELL_H as f32;
        assert!(
            quads.iter().all(|q| (q.rect[1] - cursor_y0).abs() > 0.5),
            "no quad sits on the cursor row"
        );
    }

    // --- T2/T7: RV1-safe by construction ------------------------------------

    #[test]
    fn fade_quad_is_background_color_with_decaying_alpha() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.new_output_fade = true;
        let t0 = Instant::now();
        app.update_row_fade(t0, 0);
        app.update_row_fade(t0, 1); // one fresh row at the bottom, stamped t0
        let ctx = ctx_at(&app, 0, t0);
        let clear = ctx.clear_color;
        // Fresh (t == now): fully opaque obscuring quad.
        let mut quads = Vec::new();
        app.paint_new_row_fade_quads(&ctx, &mut quads);
        assert_eq!(quads.len(), 1);
        assert_eq!(
            [quads[0].color[0], quads[0].color[1], quads[0].color[2]],
            [clear[0], clear[1], clear[2]]
        );
        assert!((quads[0].color[3] - 1.0).abs() < 1e-3, "starts opaque");

        // Later in the ramp: alpha strictly lower (revealing the content).
        let mut quads_mid = Vec::new();
        let ctx_mid = ctx_at(&app, 0, t0 + Duration::from_millis(60));
        app.paint_new_row_fade_quads(&ctx_mid, &mut quads_mid);
        assert_eq!(quads_mid.len(), 1);
        assert!(
            quads_mid[0].color[3] < quads[0].color[3],
            "alpha decays toward transparent"
        );
        assert!((0.0..=1.0).contains(&quads_mid[0].color[3]));
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

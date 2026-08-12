// SPDX-License-Identifier: GPL-3.0-only
//! Native window lifecycle and event-loop control flow.
//!
//! Owns resume and window creation, close-request and shell-exit handling, user
//! events, resize and scale changes, focus and active-session transitions,
//! wake-deadline calculation, and the about-to-wait maintenance pass.
//!
//! The `ApplicationHandler` match in the parent module remains the stable event
//! ingress; the arms here are the same bodies reached through the same order.
//! `App` stays the single state owner.

use super::*;

/// Native presenter policy for DECSET 2026 synchronized output.
///
/// The terminal core owns the mode bit. The native layer owns the safety policy:
/// once a hold is observed, grid-content uploads are deferred for at most 150 ms
/// so a crashed application that never sends DECRST 2026 cannot leave the
/// display frozen indefinitely. After the timeout, presentation is released
/// until the application resets the mode and starts a later synchronized batch.
#[derive(Debug, Default)]
pub(in crate::native) struct SynchronizedOutputHold {
    pub(super) active_since: Option<Instant>,
    pub(super) timed_out: bool,
}

impl SynchronizedOutputHold {
    pub(in crate::native) fn should_hold(&mut self, enabled: bool, now: Instant) -> bool {
        if !enabled {
            self.active_since = None;
            self.timed_out = false;
            return false;
        }

        let active_since = *self.active_since.get_or_insert(now);
        if self.timed_out {
            return false;
        }
        if now.saturating_duration_since(active_since) >= SYNCHRONIZED_OUTPUT_TIMEOUT {
            self.timed_out = true;
            return false;
        }
        true
    }

    pub(in crate::native) fn deadline(&self) -> Option<Instant> {
        (!self.timed_out)
            .then_some(self.active_since?)
            .map(|active_since| active_since + SYNCHRONIZED_OUTPUT_TIMEOUT)
    }

    pub(in crate::native) fn is_due(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }

    pub(in crate::native) fn is_holding(&self) -> bool {
        self.active_since.is_some() && !self.timed_out
    }

    /// Release the hold with no scheduled wake (the `enabled = false` rest
    /// state). Used to settle a deactivated session's hold: a background tab is
    /// never rendered, so its hold deadline must not linger as a wake source that
    /// nothing consumes (NF20-B). A later synchronized batch on that session,
    /// once active again, re-arms the hold via [`Self::should_hold`].
    pub(in crate::native) fn clear(&mut self) {
        self.active_since = None;
        self.timed_out = false;
    }
}

/// Whether the first-run onboarding card should open at startup (ONBOARD).
/// `env_override` forces it on (the `ODYTTY_ONBOARDING` escape hatch / CI).
/// Otherwise it is a first launch iff the resolved `config_path` does not yet
/// exist. An unresolvable path (no writable config dir) returns `false` —
/// fail-safe to NOT nagging, since dismissal could not be persisted (D-OB-2).
pub(super) fn should_show_onboarding(
    env_override: bool,
    config_path: Option<&std::path::Path>,
) -> bool {
    env_override || config_path.map(|path| !path.exists()).unwrap_or(false)
}

impl App {
    pub(super) fn resize_grid_with_padding(
        &mut self,
        cell: CellSize,
        padding: WindowPadding,
        width_px: u32,
        height_px: u32,
    ) -> bool {
        // Reserve the tab chrome off the grid: rows off the top for the
        // horizontal bar, or columns off the side for the vertical rail (F4-V2).
        // `reserve` is `NONE` when the bar is hidden, so the plain path is
        // byte-identical; the resize path and the snapshot-grow path
        // (`decorate_snapshot_with_tab_bar` / `..._rail`) read the SAME reserve so
        // the grid, cursor, and pointer can never desync (ODP-8).
        //
        // CHROME-GAP: derive the grid straight from `pane_content_rect`, the one
        // seam that folds in window padding, the chrome reservation, AND the
        // chrome-facing padding gap. With no gap this is exactly the historical
        // cell arithmetic (`floor((extent - 2*pad)/cell) - reserved`, since the
        // reservation is a whole-cell multiple), so the plain and gap-free paths
        // are byte-identical; with a pinned band and nonzero padding the grid
        // loses whatever whole cells the gap displaces, so text is never clipped
        // against a chrome edge.
        let reserve = self.tab_reserve();
        let content = pane_content_rect(width_px, height_px, cell, padding, reserve);
        let mut new_grid = Dimensions::new(
            (content.w.max(0.0) as u32 / cell.width.max(1)) as usize,
            (content.h.max(0.0) as u32 / cell.height.max(1)) as usize,
        );
        // Historical clamp parity: the reserve paths never let the content grid
        // collapse to zero; the no-chrome path keeps its legacy unclamped value.
        if reserve.top_rows > 0 {
            new_grid.rows = new_grid.rows.max(1);
        }
        if reserve.left_reserved_cols() + reserve.right_reserved_cols() > 0 {
            new_grid.columns = new_grid.columns.max(1);
        }
        let grid_changed = new_grid != self.grid;
        if grid_changed {
            self.grid = new_grid;
        }

        // Size every pane of every tab to its laid-out sub-rect. For an all-
        // single-pane world each tab's lone leaf spans the whole content rect,
        // so this resizes each session to exactly `new_grid` — byte-identical to
        // the old per-session loop. Multi-pane tabs get per-pane sizing (#1).
        // Reconcile even when the aggregate grid did not cross a cell boundary:
        // a final one-pixel surface configure can move a ratio-derived divider
        // far enough for one child grid to change while the window grid stays
        // constant. Per-session dimension and metric guards keep unchanged
        // panes inert.
        let panes_changed = self.sessions.reconcile_all_panes_for_surface(
            content,
            cell.width,
            cell.height,
            PANE_DIVIDER_PX,
            padding.as_f32(),
        );
        grid_changed || panes_changed
    }

    /// Reconcile every pane with the current window content geometry even when
    /// the window grid itself has not changed. A newly attached mirror starts at
    /// the host's dimensions, so it must not depend on a later window resize to
    /// reach the window's render and hit-test basis. Per-pane dimension guards
    /// keep an already-correct layout model- and transport-neutral.
    #[cfg(unix)]
    pub(super) fn reconcile_pane_dims_to_window(&mut self) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let Some((width_px, height_px, padding)) = self.resolved_surface() else {
            return;
        };
        let content = pane_content_rect(width_px, height_px, cell, padding, self.tab_reserve());
        self.sessions.resize_all_panes(
            content,
            cell.width,
            cell.height,
            PANE_DIVIDER_PX,
            padding.as_f32(),
        );
    }

    pub(super) fn apply_grid_resize(&mut self, resize: PendingResize) {
        // A minimized window can report a 0x0 drawable surface. The GPU surface
        // ignores that size, and the terminal model must do the same: passing
        // zero through grid fitting clamps to 1x1 and destructively reflows the
        // live screen while there is no drawable area.
        if resize.width_px == 0 || resize.height_px == 0 {
            return;
        }
        if self.resize_grid_with_padding(
            resize.cell,
            resize.padding,
            resize.width_px,
            resize.height_px,
        ) {
            // `resize_all_panes` invalidates each session at the exact
            // dimension-change guard that performs its model reflow. Panes whose
            // grid did not change retain their coordinate-bound UI state.
            self.needs_rebuild = true;
        }
    }

    pub(super) fn record_pending_resize(&mut self, resize: PendingResize, now: Instant) {
        if let Some(due) = self.resize_debounce.record(resize, now) {
            self.apply_grid_resize(due);
            self.finish_resize_for_hud(now);
        }
    }

    /// Pure computation of the next timer wake instant: the minimum over every
    /// scheduled wake source, or `None` when nothing is pending (the zero-wake
    /// idle case → `ControlFlow::Wait`). Split out from
    /// [`Self::update_control_flow_deadline`] so it is testable without an
    /// `ActiveEventLoop` (which cannot be constructed in a unit test). The
    /// caller maps `Some`/`None` onto `WaitUntil`/`Wait`.
    pub(super) fn next_wake_deadline(&self) -> Option<Instant> {
        [
            self.deadline,
            self.resize_debounce.deadline(),
            // BLACK-SCREEN-ON-RESTORE: bounded retry for a transiently-skipped
            // frame. `None` at rest, so the idle wake set is unchanged; when a
            // frame was skipped this wakes the loop to repaint the recovered
            // surface instead of leaving it black until an unrelated event.
            self.skipped_frame_retry_deadline,
            // §7: wake when a pending multiplexer prefix times out, so the
            // pending state clears promptly even with no further input. `None`
            // (the at-rest case) leaves the min unchanged.
            self.prefix_engine.pending_deadline(),
            // NF20-B: the cursor blink of the ACTIVE pane only. `self.cursor_blink`
            // Derefs to the active session — the SAME pane the maintenance
            // consumer (`self.cursor_blink.is_due`) and the frame poll advance.
            // A background pane is never rendered, so its blink is never polled;
            // sourcing its stale deadline here (as the old `sessions.iter()` did)
            // left a wake with no consumer → `WaitUntil(<past>)` busy-spin after a
            // tab switch. Background panes are parked in maintenance, so this
            // active-only source is the whole live set.
            self.cursor_blink.deadline(),
            // Config-file live-reload poll. Only schedule its timer wake while
            // the window is focused: a backgrounded terminal that nobody is
            // editing config *and watching* has no reason to stat the file once
            // a second, so suppressing this drops idle-unfocused self-wakes to
            // zero (the only remaining wake source at rest). Edits made while
            // away still apply on focus regain — the `Focused(true)` redraw
            // walks `run_about_to_wait_maintenance` -> `poll_config_reload`,
            // which fires immediately because `next_poll` is by then in the
            // past — so live reload stays correct, it just defers the stat to
            // the moment you look at the window again.
            self.focused
                .then(|| self.settings_reloader.deadline())
                .flatten(),
            // NF20-B: the synchronized-output hold of the ACTIVE pane only, for
            // the same fan-out reason as the blink above. The maintenance
            // consumer (`self.synchronized_output_hold.is_due`) advances the
            // active pane; background panes are parked, so an active-only source
            // matches the consumer and cannot strand a stale hold in the wake set.
            self.synchronized_output_hold.deadline(),
            // Cursor-animation wake source, ACTIVE focused pane only (NF20-B).
            // Both single-pane and split render paths advance this consumer;
            // background panes stay parked and never fan wakes into this set.
            self.focused_cursor_animation_deadline(),
            // F4-P3: wake at the next rail auto-hide boundary (show debounce /
            // hide grace / flash expiry). `None` at rest — steady Hidden, or
            // Revealed with the pointer parked — so the idle wake set is
            // unchanged when nothing is animating.
            self.rail_autohide.wake_deadline(Instant::now()),
            // One-shot centered HUD expiry. The surface paints in both the
            // single-pane snapshot and the focused split pane, so unlike the
            // single-pane animation aggregator it is safe to source globally.
            self.transient_hud_deadline(),
            // NF21-2: the overlay/scroll/bell/fade animation aggregator
            // (`animation_deadline()` — smooth-scroll glide, bell flash, new-row
            // fade, open-notice + click-hint auto-expiry, and the cursor
            // ease/slide it already folds). This entry was dropped when the
            // multi-session refactor replaced it with the cursor-only fan-out
            // above, stranding those five with a maintenance CONSUMER but no
            // wake SOURCE — they only advanced when an unrelated wake (a blink
            // toggle) happened to fire, so they froze outright when the cursor
            // was steady/unfocused/blink-off. Restored here, gated to the
            // single-pane active render path: that path (the `update_*` calls in
            // the single-pane rebuild) is the ONLY consumer that advances these
            // timers, so — per the NF20-B "a source must not fan wider than its
            // consumer" rule — sourcing a wake while multipane would be a wake
            // with no consumer (a spin). NF21-1/7 restores the multipane
            // advancement and widens this gate. `None` at rest (every
            // contributor `None`), so the idle wake set is unchanged.
            self.sessions
                .active_is_single_pane()
                .then(|| self.animation_deadline())
                .flatten(),
            // In a split, source ONLY the per-pane glide wake here.
            // `rebuild_multipane` advances each visible pane's glide follower,
            // while the focused cursor deadline above drives only the focused
            // pane. Other animation timers (bell / notice / hint) remain
            // single-pane-only consumers, so widening all of
            // `animation_deadline()` would fan wakes beyond their consumers.
            // `None` at rest / single-pane, so the idle wake set is unchanged.
            self.multipane_glide_deadline(),
            // Kitty graphics animation: wake when a visible animated image is due
            // for its next frame. Sourced from the ACTIVE pane (the same pane the
            // maintenance consumer advances), so - per the "a source must not fan
            // wider than its consumer" rule - no wake is scheduled for an
            // animation nobody is looking at. `None` unless an animated placement
            // is visible and running, so the idle wake set is unchanged.
            self.animated_graphics_deadline,
            // WP2: wake to flush the debounced workspace-shape autosave. `None`
            // at rest (nothing pending), so the idle wake set is unchanged; when
            // a shape mutation is pending this fires the one write ~1.5s later.
            self.autosave_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(super) fn update_control_flow_deadline(&self, event_loop: &ActiveEventLoop) {
        match self.next_wake_deadline() {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    /// Record a fatal startup error and ask the loop to exit.
    pub(super) fn fail(&mut self, event_loop: &ActiveEventLoop, err: NativeError) {
        self.startup_error = Some(err);
        event_loop.exit();
    }

    /// Complete the ordinary local-shell exit policy after reconnect/hold have
    /// declined or a held pane has been dismissed. Keeping this tail shared
    /// makes `--hold` a delay in teardown, not a different pane/workspace/app
    /// close policy.
    pub(super) fn finish_shell_exit(&mut self, session: SessionToken) -> bool {
        if self.sessions.position_of_token(session).is_some() && self.sessions.iter().count() <= 1 {
            self.pending_exit = true;
            return true;
        }
        if self.settings.shell_exit_closes == crate::settings::ShellExitCloses::App
            && self.sessions.shell_exit_closes_workspace(session)
        {
            if self.settings.confirm_close
                && self.sessions.any_foreground_job_running_except(session)
            {
                self.overlay.open_confirm_close();
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                return false;
            }
            self.pending_exit = true;
            return true;
        }
        let is_last = self.sessions.close_shell_exited(session);
        if is_last {
            self.pending_exit = true;
            true
        } else {
            self.on_active_session_changed();
            false
        }
    }

    pub(super) fn on_active_session_changed(&mut self) {
        let incoming = self.sessions.active_id();
        // Some state-maintenance callers invoke this seam without changing the
        // active identity. In that case the original layout is still current,
        // so settling here is safe. Real tab/workspace mutations must settle at
        // their entry point, before `incoming` changes.
        if incoming == self.last_active_session {
            self.finish_divider_drag();
        }
        debug_assert!(
            self.divider_drag.is_none(),
            "active-session mutation must settle divider ownership first"
        );
        self.cancel_osc52_prompt();
        let outgoing = self.last_active_session;
        if self.focused && outgoing != incoming {
            self.send_focus_report_to(outgoing, false);
            self.send_focus_report_to(incoming, true);
        }
        self.last_active_session = incoming;

        // A pane activation gives its cursor the same visible hold as keyboard
        // activity. Background panes are parked, so only the newly focused
        // session receives a bounded blink deadline.
        self.note_cursor_keyboard_activity(Instant::now());

        // NF21-8/9/11: an active-session change (tab OR workspace switch post-W1)
        // must not carry window/session input latches across the boundary. Drop
        // the pointer drag + hover cell on every session so the outgoing one
        // sheds a mid-drag `Selecting` latch and the incoming one starts with no
        // phantom hover, clear the grid held-button flag so a lost release can't
        // resume the drag, and drop any in-flight IME preedit so a composition
        // begun on the previous surface cannot paint at (or commit into) the new
        // one. Selection/viewport state is deliberately preserved.
        self.sessions.clear_all_input_latches();
        self.grid_left_held = false;
        // Release-build safety for a future missed caller: never let stale
        // ownership resume against the newly-active layout. The debug assertion
        // above keeps such a caller visible in tests; correctness requires its
        // mutation entry point to perform the real settlement first.
        self.divider_drag = None;
        self.rail_seam_drag = false;
        self.tab_bar_seam_drag = false;
        self.rail_ws_drag = None;
        self.top_tab_drag = None;
        self.prefix_engine.cancel();
        // The confirmation prompt belongs to the old pane. Switching away is
        // an implicit cancel so Enter in the new pane cannot authorize upload.
        self.pending_image_paste = None;
        // Drop any in-flight IME preedit directly (the field is repainted via the
        // `needs_rebuild` set below); a composition begun on the previous surface
        // must not paint at, or commit into, the new one.
        self.ime_preedit.clear();
        // NF21-12: an activation is a viewport discontinuity for the new-output
        // fade, exactly like a resize. Scrollback the incoming session grew
        // while it was backgrounded is not "fresh output" and must not fade in
        // on switch-back. Clear the incoming session's fade tracker so its next
        // rebuild re-baselines (snaps) instead of fading up to a full viewport.
        // No-op when `new_output_fade` is off (the tracker is already empty).
        self.row_fade_starts.clear();

        // An activation is likewise a viewport-anchor discontinuity. A tab keeps
        // producing output while backgrounded but is not rendered, so its
        // scrollback-growth baseline (`last_scrollback_len`) freezes; without
        // this reconcile the first render after switch-back would treat all the
        // backgrounded growth as one huge `added` and `anchor_after_growth`
        // would yank a scrolled-up viewport to the top of scrollback, leaving
        // fresh output stranded offscreen below (the "returned to a tab stuck
        // scrolled up, new output invisible" report). Re-baseline every pane of
        // the incoming tab so backgrounded growth counts as already-past: a pane
        // at the live bottom stays live and a scrolled-up pane keeps its offset
        // relative to the current bottom. A no-op for a tab that was never
        // backgrounded.
        self.sessions.reconcile_active_tab_scroll_baselines();
        // SCROLL-GLIDE: a switch is a viewport discontinuity — settle the
        // incoming session's follower so it renders at its exact offset rather
        // than a stale lagging position.
        self.snap_scroll_glide_of(self.sessions.active_id());
        self.recompute_grid_for_tab_bar();
        self.tab_bar.set_hover(None);
        self.last_render_signature = None;
        self.needs_rebuild = true;
        self.sync_active_window_title();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    pub(super) fn send_focus_report_to(&mut self, token: SessionToken, focused: bool) {
        let Some(session) = self.sessions.get(token) else {
            return;
        };
        let Some(bytes) = session
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| encode_native_focus_report(&terminal, focused))
        else {
            return;
        };
        #[cfg(test)]
        self.focus_reports_for_test.push((token, focused));
        if let Ok(mut writer) = session.writer.lock() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    }

    pub(super) fn apply_user_event(&mut self, event: UserEvent) -> bool {
        match event {
            UserEvent::Redraw { session } => {
                if let Some(target) = self.sessions.get_mut(session) {
                    target.needs_rebuild = true;
                    target.refresh_tab_title();
                }
                // Redraw suppression (design doc §2.5 audit row #4): wake the
                // window when the session is *any visible pane of the active
                // tab*, not only the focused one — a background pane producing
                // output must repaint in a split. For a single-pane tab
                // `is_visible_pane` is exactly `active_id() == session`, so the
                // single-pane wake decision is unchanged.
                if self.sessions.is_visible_pane(session)
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
                false
            }
            UserEvent::ShellExited { session } => {
                // Closing or reconnecting any session may collapse a pane, tab,
                // or workspace. Settle an active divider before that layout can
                // be removed or focus can move to a survivor.
                self.settle_divider_for_surface_change();
                // F6-i4: a remote session whose link dropped (`ssh` exit 255)
                // holds its tab open with an in-pane reconnect prompt instead of
                // closing. Checked BEFORE the last-session exit test so a lone
                // remote tab dropping offers reconnect rather than exiting the
                // app. Local shells and clean exits fall through unchanged.
                if self.sessions.try_arm_reconnect(session) {
                    self.on_active_session_changed();
                    return false;
                }
                // `--hold` is launch-scoped and applies only to the initial
                // local session. Reconnect wins above; later sessions have no
                // hold marker and keep their historical teardown behavior.
                if self.hold_session == Some(session)
                    && self.sessions.hold_after_shell_exit(session)
                {
                    self.hold_session = None;
                    self.held_exit = Some(session);
                    if self.sessions.is_visible_pane(session)
                        && let Some(window) = self.window.as_ref()
                    {
                        window.request_redraw();
                    }
                    return false;
                }
                // SHELL-EXIT-CLOSES (App mode): a shell exit that would close its
                // whole workspace quits OdyTTY instead of closing just that
                // workspace, even when other workspaces survive. Take the SAME
                // "set pending_exit WITHOUT reaping" path the last-workspace exit
                // uses, so the shutdown snapshot still captures every workspace
                // (including this one) for layout restore. Exits that only close
                // a pane or tab are unaffected (the predicate is false there).
                // Windows: the ConPTY shell exit flows through this same
                // UserEvent path, so App mode behaves identically there.
                self.finish_shell_exit(session)
            }
            UserEvent::ImageUploaded {
                session,
                remote_path,
            } => {
                // F6-i7 post-upload UX: the background upload finished. The
                // remote path is NOT typed into the shell -- a bare path sitting
                // on an empty prompt runs on the next Enter and errors (the PNG
                // is not executable). Instead post a self-explaining notice into
                // the originating pane and copy the path to the local clipboard,
                // so it can be pasted as an argument wherever it is wanted.
                if let Some(target) = self.sessions.get_mut(session) {
                    let banner = format!(
                        "\r\n\x1b[1;32m image uploaded \x1b[0m {remote_path} \u{b7} copied to clipboard\r\n"
                    );
                    crate::native::lock_recover(&target.terminal).advance(banner.as_bytes());
                    target.needs_rebuild = true;
                    target.last_render_signature = None;
                }
                // Overwriting the clipboard image is intended: the image is
                // uploaded now, so the remote path is the deliverable.
                let _ = self.clipboard.write_text(&remote_path);
                if self.sessions.is_visible_pane(session)
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
                false
            }
        }
    }

    pub(super) fn run_about_to_wait_maintenance(&mut self, now: Instant) {
        // REMOTE-UX P4 / ODP-8: drain a finished Test Connection probe into the
        // open form. Idle when no probe is in flight.
        self.poll_connection_probe();
        // NF20-B: settle the cursor-animation / render-hold timers of every
        // non-active pane. Background panes are never rendered, so their timers
        // have no consumer; parking them here — the one place that runs before
        // every `next_wake_deadline` recompute in the loop — keeps them out of
        // the wake set and guarantees a pane switched back to starts from a clean
        // (non-stale) timer state. Idempotent and cheap. Paired with the
        // active-only deadline sources in `next_wake_deadline`.
        self.sessions.park_background_timers();

        // NF21-6: drain the bell + prompt-marks-changed latches of EVERY
        // session over the flat arena, so a bell in a background tab / pane /
        // workspace — or in any multipane tab — is serviced here instead of
        // stranding until that surface becomes the active single-pane one
        // (where it fired spuriously at switch-time). The active-visible focused
        // pane keeps today's viewport flash + prompt-marks epoch (the single-
        // pane render fast path no longer drains, so it stays byte-identical);
        // any other session that rang pings cross-platform window urgency and
        // latches its tab's activity flag (the rollup input, ODP-6 v2 — the flag
        // is landed and maintained here, but no rollup UI reads it yet). Kept in
        // maintenance, NOT in `rebuild_multipane`, to avoid colliding with the
        // multipane viewport-bookkeeping work.
        let bell_sweep = self
            .sessions
            .drain_bells(self.settings.command_status_gutter);
        if bell_sweep.focused_bell {
            let window = self.window.clone();
            self.note_bell(now, window.as_deref());
        }
        if bell_sweep.background_bell {
            let window = self.window.clone();
            self.request_bell_attention(window.as_deref());
            // The same sweep latched a tab/workspace activity badge. Rebuild
            // immediately so the visible chrome reflects that state even when
            // the OS urgency request itself produces no draw event.
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        if bell_sweep.focused_prompt_changed {
            self.prompt_marks_epoch = self.prompt_marks_epoch.wrapping_add(1);
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // WP2 sub-ODP 8c: debounced workspace-shape autosave. Cheap and fully
        // idle on non-primary instances / when nothing changed.
        self.run_shape_autosave(now);

        if let Some(resize) = self.resize_debounce.take_due(now) {
            self.apply_grid_resize(resize);
            self.finish_resize_for_hud(now);
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // Static, one-shot HUD expiry. It is not a frame-paced animation and
        // therefore clears exactly once on its deadline in either pane mode.
        if self.expire_transient_hud(now) {
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // Advance a due cursor-blink boundary before recomputing the wake set.
        // This prevents a delayed redraw from leaving `WaitUntil` pointed at a
        // past blink instant; the rebuild below consumes the already-resolved
        // phase for easing and rendering.
        if self.cursor_blink.is_due(now) {
            let blinking = self
                .terminal
                .lock()
                .map(|terminal| terminal.cursor_blinking())
                .unwrap_or(false);
            let focused = self.focused;
            let _ = self.cursor_blink.poll(now, blinking, focused);
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // An animation tick (cursor ease/slide, smooth-scroll glide, bell flash,
        // new-row fade, open-notice / click-hint expiry) rebuilds once so the
        // frame advances. NF21-2: the predicate is "an animation is in flight",
        // NOT "now >= deadline". Three of the frame-paced contributors
        // (new_row_fade / bell embed `Instant::now() + FRAME`), as
        // does the cursor ease/slide, so `now >= deadline` is essentially never
        // satisfied mid-flight — the old equality check silently never fired for
        // them and the animation only stepped when an unrelated wake (a blink
        // toggle) happened to rebuild. Treating "woken while animating" as
        // "request a frame" closes that: the collector schedules the wake at the
        // next frame boundary (`animation_deadline()` = now+FRAME), this repaint
        // advances the timer in the rebuild, and when it settles
        // `animation_deadline()` -> `None` ends the loop — bounded, so the
        // terminal returns to zero-wake idle with no wake and no redraw at rest.
        // Gated to the single-pane render path for the same reason the collector
        // source is (that path is the only consumer that advances these timers;
        // multipane advancement is NF21-1/7). The real-instant contributors
        // (open-notice / click-hint) still fire exactly once — the collector
        // wakes only at their expiry, so `is_some()` sees them due on that one
        // pass and the rebuild clears them.
        if self.sessions.active_is_single_pane() && self.animation_deadline().is_some() {
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        } else if self.multipane_glide_deadline().is_some()
            || self.focused_cursor_animation_deadline().is_some()
        {
            // A split with a live per-pane glide or focused cursor animation
            // repaints until its matching consumer settles. Setting the focused
            // pane's flag opens the tab-wide rebuild gate; the rebuild advances
            // every visible glide and only the focused cursor, then clears the
            // visible flags. Background cursor timers remain parked, so the wake
            // set cannot fan out or storm at rest.
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // Kitty graphics animation: advance visible animated images to the frame
        // due now and refresh the wake this pass will be judged against. Fully
        // gated inside the core on "some image has frames", so a session with no
        // animated graphics does no work here beyond the terminal lock the pass
        // already takes for the blink check.
        self.advance_graphics_animations(now);

        if self.synchronized_output_hold.is_due(now) {
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // F4-P3: advance the rail auto-hide timers (show debounce / hide grace /
        // flash expiry). A due boundary flips the overlay's visibility; repaint
        // so the reveal appears or the hide takes effect. Returns `false` at rest
        // (no autohide, or steady visible/hidden), so this is inert on the plain
        // path and while the rail is parked open under the pointer. Keep the
        // suspend latch current so a menu closing lets the grace run again.
        // RAIL-PIN: a rail-anchored menu or a workspace rename also suspends it.
        self.rail_autohide
            .set_suspend(self.overlay.is_open() || self.rail_pinned_open());
        if self.rail_autohide.poll(now) {
            // A timer boundary that flips the rail's visibility (show debounce
            // elapsing, hide grace / flash expiring) must rebuild the frame so
            // the overlay is (re)assembled or dropped — `build_rail_overlay` runs
            // only inside the `should_rebuild_frame` gate, which reads
            // `needs_rebuild`. Requesting a redraw alone lets the rebuild be
            // skipped and the reveal never paints until an unrelated dirty frame.
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // §7 multiplexer prefix: forget a pending prefix that has timed out.
        // `pending_deadline()` is a `next_wake_deadline` source, so a prefix left
        // pending after its timeout would keep the loop scheduling
        // `WaitUntil(<past instant>)` — a 0-timeout poll that returns immediately
        // every iteration and busy-spins a core — until the next key or focus
        // loss cleared it. Expiring it here on the timer (the same instant the
        // loop is woken at) breaks that spin. No repaint: the pending state has
        // no frame-path affordance yet; if one ships, request a redraw here.
        self.prefix_engine.expire_pending(now);

        // BLACK-SCREEN-ON-RESTORE: a due skipped-frame retry. Clear the pending
        // deadline and request a redraw so the next `RedrawRequested` re-attempts
        // the frame; if it skips again (and the guards still allow it) the
        // RedrawRequested arm re-arms a fresh bounded retry. This is a timed,
        // budget-capped retry — not a busy-poll.
        if let Some(deadline) = self.skipped_frame_retry_deadline
            && now >= deadline
        {
            self.skipped_frame_retry_deadline = None;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        self.poll_config_reload(now);
    }
}

// Window-event lifecycle arms moved verbatim from the `ApplicationHandler`
// match; the match itself remains the stable ingress in `mod.rs`.
impl App {
    pub(super) fn on_resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let (w, h) = self.options.window_logical_size();
        // WIN-DECOR: request decorations per config at creation. Default `true`
        // matches `WindowAttributes::default()`, so the startup chain is
        // byte-identical when unset.
        let attributes = Window::default_attributes()
            .with_title(self.options.title.clone())
            .with_inner_size(LogicalSize::new(w, h))
            .with_decorations(self.settings.window_decorations)
            // Runtime window/title-bar icon (Windows + X11; a no-op on macOS and
            // Wayland). `None` on any decode failure, so a bad icon can never
            // block window creation. The `.exe` file icon is embedded separately
            // at build time (build.rs / winresource).
            .with_window_icon(crate::native::window_icon::load())
            // TRANSPARENCY: request a transparency-CAPABLE surface unconditionally.
            // With `window_transparency` off the presented output is fully opaque
            // (the background alpha stays 1.0); the flag only lets the compositor
            // honor a translucent background when the setting is enabled, which it
            // is by default. A no-op where the display server offers no alpha
            // compositing.
            .with_transparent(true);
        #[cfg(all(unix, not(target_os = "macos")))]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name(linux_window_app_id(&self.options), "odytty")
        };

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                self.fail(event_loop, NativeError::WindowCreation(err.to_string()));
                return;
            }
        };

        // IME: allow composition input (CJK input methods, compose/dead-key
        // accents) to deliver `Ime::Preedit`/`Ime::Commit` events. Without this
        // winit suppresses IME and composed text never reaches the terminal.
        window.set_ime_allowed(true);

        // Seed the first buffer from the current shared-terminal snapshot (any
        // PTY output already pumped is picked up by the first redraw below).
        let initial_snapshot = crate::native::lock_recover(&self.terminal).snapshot();
        match GpuState::new(
            window.clone(),
            &self.options,
            &initial_snapshot,
            self.effective_theme,
            self.visual,
            self.settings.effective_stem_darken(),
            bloom_options(&self.settings),
            crt_options(&self.settings),
            self.sessions.event_proxy(),
            self.sessions.active_id(),
        ) {
            Ok(mut gpu) => {
                // Push live cell pixel metrics to the terminal core so graphics
                // placements (sixel/kitty) compute the correct cell extent.
                let cell = gpu.cell();
                if let Ok(mut term) = self.terminal.lock() {
                    term.set_cell_metrics(cell.width, cell.height);
                }
                self.last_cursor_comparison_snapshot = Some(
                    crate::native::session::CursorComparison::of(&initial_snapshot),
                );
                self.last_presented_snapshot = Some(initial_snapshot.clone());
                // ID3/U5: seed the background-image pass from the launch config
                // so the very first frame already reflects an `image` treatment
                // (no-op / off path when no image is configured).
                gpu.set_background_image(
                    self.settings.effective_background_treatment()
                        == crate::settings::BackgroundTreatment::Image,
                    self.settings.background_image.as_deref(),
                    self.settings.background_blur_radius,
                    self.settings.background_image_scrim,
                    self.settings.cell_bg_opacity,
                    self.effective_theme,
                );
                // SELECTION-OPACITY: seed the selection strength for the first
                // frame from the launch config (identity / off path at 1.0).
                gpu.set_selection_opacity(self.settings.selection_opacity);
                // COLORED-BG-FLOOR: seed the colored-background opacity floor
                // from the launch config so the first frame already floors.
                gpu.set_colored_bg_opacity(self.settings.colored_bg_opacity);
                // TEXT-BRIGHTNESS: seed the glyph-foreground lift from the
                // launch config (identity / off path at 1.0).
                gpu.set_text_brightness(self.settings.text_brightness);
                self.gpu = Some(gpu);
            }
            Err(err) => {
                self.fail(event_loop, err);
                return;
            }
        }

        self.needs_rebuild = true;
        window.request_redraw();
        self.window = Some(window);

        // OS-THEME: seed the OS appearance from the window (Wayland delivers a
        // value here; X11 returns `None`) or the `ODYTTY_APPEARANCE` env
        // fallback, then apply the override so the very first frame already
        // reflects the OS preference. No-op when following is off (the resolve
        // returns the authored theme and the apply early-returns on equality),
        // so the default startup path is unchanged.
        if self.settings.follow_os_theme {
            self.os_theme = self
                .window
                .as_ref()
                .and_then(|window| window.theme())
                .or_else(os_theme::env_appearance_override);
            self.apply_os_theme_override();
        }

        if let Some(delay) = self.autoclose {
            let deadline = Instant::now() + delay;
            self.deadline = Some(deadline);
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }
    }

    pub(super) fn on_close_requested(&mut self, event_loop: &ActiveEventLoop) {
        // CLOSE-CONFIRM: when enabled and a foreground job is actually
        // running, intercept the close and raise the confirmation dialog
        // instead of exiting. Only `ForegroundJob::Running` prompts —
        // `None` (idle shell) and `Unknown` (query error / dead PTY) and
        // a poisoned lock all fall through to the immediate exit, so the
        // off path and the common idle-close path are unchanged (TRAP-1,
        // TRAP-5). An attached session reports not-running (the job lives
        // in the remote host), so closing an attached window detaches
        // immediately without prompting.
        if self.settings.confirm_close && self.foreground_job_running() {
            self.overlay.open_confirm_close();
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        } else {
            event_loop.exit();
        }
    }

    pub(super) fn on_os_theme_changed(&mut self, os_theme: winit::window::Theme) {
        // OS-THEME: the compositor reported a dark/light preference
        // change (Wayland). Record it always; re-resolve the active
        // theme only while following is on. `apply_os_theme_override`
        // bumps the presentation epoch and requests a redraw itself.
        self.os_theme = Some(os_theme);
        if self.settings.follow_os_theme {
            self.apply_os_theme_override();
        }
    }

    pub(super) fn on_window_resized(
        &mut self,
        size: PhysicalSize<u32>,
        event_loop: &ActiveEventLoop,
    ) {
        // BLACK-SCREEN-ON-RESTORE: track minimize (a 0x0 surface) so the
        // skipped-frame retry is suppressed while there is nothing to
        // paint. A restore (non-zero size) clears it AND resets the
        // skipped-frame retry budget so the recovering surface gets a
        // fresh set of bounded retries if its first acquire skips.
        self.window_minimized = size.width == 0 || size.height == 0;
        if !self.window_minimized {
            self.consecutive_skipped_frames = 0;
        }
        // Reconfigure the GPU surface (pixel size) and read the real
        // per-cell metric so the grid fit matches what is drawn. The
        // surface updates immediately; the terminal model + PTY winsize
        // are debounced so drag bursts do not reflow on every event.
        let resize = self.gpu.as_mut().map(|gpu| {
            gpu.resize(size.width, size.height);
            pending_resize_for_surface(gpu.cell(), gpu.window_padding(), size)
        });

        // A surface configure invalidates the pixel basis of an active
        // divider grab. This also closes the Wayland path where the
        // compositor ends the grab without a button-release event.
        self.finish_divider_drag();
        if let Some(resize) = resize {
            let now = Instant::now();
            self.note_window_resize_for_hud(resize.width_px, resize.height_px);
            self.record_pending_resize(resize, now);
        }
        // C4: re-center the image viewer for the new surface size.
        self.refresh_image_overlay_on_resize();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        self.update_control_flow_deadline(event_loop);
    }

    pub(super) fn on_scale_factor_changed(
        &mut self,
        scale_factor: f64,
        mut inner_size_writer: winit::event::InnerSizeWriter,
        event_loop: &ActiveEventLoop,
    ) {
        let size = self
            .window
            .as_ref()
            .map(|window| window.inner_size())
            .unwrap_or_else(|| PhysicalSize::new(0, 0));
        let _ = inner_size_writer.request_inner_size(size);

        let resize = self.gpu.as_mut().and_then(|gpu| {
            gpu.resize(size.width, size.height);
            let scale = scale_factor as f32;
            if !scale_factor_changed(gpu.scale(), scale) || !gpu.set_scale(scale) {
                return None;
            }
            Some(pending_resize_for_surface(
                gpu.cell(),
                gpu.window_padding(),
                size,
            ))
        });

        // The cell metric and padding lattice may change even when the
        // outer pixel size does not. Complete the old-basis gesture
        // before applying those metrics or queuing reconciliation.
        self.settle_divider_for_surface_change();

        if let Some(resize) = resize {
            self.needs_rebuild = true;
            self.last_render_signature = None;
            self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
            // C18: apply the new per-cell metrics to every pane now. The
            // debounced grid resize is a model no-op when the scale change
            // keeps the same cols/rows, which would strand pixel-space
            // consumers (SGR-pixel mouse, inline-image sizing) on the old
            // scale. Metrics-only: no reflow, no SIGWINCH.
            self.sessions
                .apply_cell_metrics_all(resize.cell.width, resize.cell.height);
            self.record_pending_resize(resize, Instant::now());
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        self.update_control_flow_deadline(event_loop);
    }
}

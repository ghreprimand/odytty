// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for the native app facade and its extracted handlers.

use super::*;

fn blink() -> CursorBlinkState {
    CursorBlinkState::new(Duration::from_millis(500))
}

#[test]
fn skip_episode_starts_once_and_emits_once_with_true_totals() {
    let start = Instant::now();
    let mut episode = SkipEpisode::default();
    assert_eq!(episode.note_presented(start), None);

    episode.note_skipped(start);
    episode.note_skipped(start + Duration::from_millis(7));
    assert!(episode.is_active());

    assert_eq!(
        episode.note_presented(start + Duration::from_millis(23)),
        Some((Duration::from_millis(23), 2))
    );
    assert!(!episode.is_active());
    assert_eq!(episode.note_presented(start + Duration::from_secs(1)), None);
}

#[test]
fn skip_episode_log_level_escalates_at_freeze_threshold() {
    assert_eq!(
        episode_log_level(Duration::from_millis(9_999)),
        tracing::Level::DEBUG
    );
    assert_eq!(
        episode_log_level(Duration::from_secs(10)),
        tracing::Level::WARN
    );
}

#[test]
fn skip_episode_record_is_state_only() {
    let record = format_skip_episode_record(4_321, 7, true, false);
    assert!(record.starts_with("skip_episode_end "), "got: {record}");
    let body = &record["skip_episode_end ".len()..];
    for token in body.split_whitespace() {
        let (key, value) = token.split_once('=').expect("key=value tokens only");
        assert!(
            key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "unexpected key charset: {key}"
        );
        assert!(
            value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "unexpected value charset: {value} (free-form strings are banned here)"
        );
    }
}

#[test]
fn pending_surface_reconfigure_is_consumed_once() {
    let mut pending = true;
    assert!(take_pending_reconfigure(&mut pending));
    assert!(!pending);
    assert!(!take_pending_reconfigure(&mut pending));
}

/// TRANSPARENCY: the pure window-background-alpha decision. Opaque (`1.0`)
/// whenever the setting is off or the compositor can't composite alpha;
/// otherwise the configured percent as a fraction. An open overlay panel no
/// longer forces opacity (MENU-OPACITY) — the window stays translucent and
/// the panel is held opaque per-surface elsewhere.
#[test]
fn window_bg_alpha_gates_on_setting_and_capability() {
    // Transparency off => fully opaque regardless of the opacity percent.
    assert_eq!(window_bg_alpha_for(false, true, 85.0), 1.0);
    // On + capable => the configured percent as a 0..=1 fraction.
    assert!((window_bg_alpha_for(true, true, 85.0) - 0.85).abs() < 1e-6);
    // Not capable (Opaque-only compositor) => stays opaque.
    assert_eq!(window_bg_alpha_for(true, false, 85.0), 1.0);
    // The percent is clamped to a valid 0..=1 fraction.
    assert_eq!(window_bg_alpha_for(true, true, 150.0), 1.0);
    assert!((window_bg_alpha_for(true, true, 30.0) - 0.30).abs() < 1e-6);
}

/// Pins the black-screen-on-restore recovery policy at the pure seam, with
/// zero GPU/winit. Two failure modes are guarded:
///
/// - `Reconfigure` ⇒ reconfigure AND repaint (an outdated surface,
///   e.g. Windows DX12 on idle-minimize; without the follow-up redraw the
///   recovered surface stays black under `ControlFlow::Wait`).
/// - `Skipped` ⇒ a BOUNDED retry (a surface that came back Timeout/Occluded
///   on restore; the OLD policy did nothing here, so it stayed black until
///   an unrelated event — this is the residual fixed here).
///
/// `Presented` settles. The GPU triggers themselves are on-device-only;
/// this pins the decision deterministically.
#[test]
fn after_frame_maps_outcomes_to_recovery_actions() {
    assert_eq!(
        after_frame(FrameOutcome::Reconfigure),
        FrameAction::ReconfigureThenRedraw,
        "an outdated surface must reconfigure and request a redraw"
    );
    assert_eq!(
        after_frame(FrameOutcome::RecreateSurface),
        FrameAction::RecreateSurfaceThenRedraw,
        "a lost surface must be recreated and request a redraw"
    );
    assert_eq!(
        after_frame(FrameOutcome::RecreateDevice),
        FrameAction::DeviceLost,
        "a device loss must not be treated as a surface reconfigure"
    );
    assert_eq!(
        after_frame(FrameOutcome::Presented),
        FrameAction::Idle,
        "a presented frame must settle (no extra paint scheduled)"
    );
    // The load-bearing assertion here: a skipped frame must
    // schedule a bounded retry, not dead-end (the black-screen residual).
    // Both skip kinds map to the same bounded retry — escalation is a
    // separate, stateful decision layered on top at the call site.
    for occluded in [false, true] {
        match after_frame(FrameOutcome::Skipped { occluded }) {
            FrameAction::RetryAfter(delay) => {
                assert!(
                    delay > Duration::ZERO && delay <= Duration::from_millis(100),
                    "a skipped frame must retry after a bounded, non-zero delay, got {delay:?}"
                );
            }
            other => panic!("a skipped frame must schedule a bounded retry, got {other:?}"),
        }
    }
}

/// ANTI-FREEZE ESCALATION: a chronic acquire timeout reaching the
/// consecutive-skip threshold escalates to a surface recreate — the episode
/// that previously retried forever (an explicit-sync fence that never
/// signals left a live window frozen for minutes with the watchdog logging
/// the stall) now routes into the existing recreate machinery.
#[test]
fn chronic_timeout_escalates_to_recreate_at_threshold() {
    let mut esc = SkipEscalation::default();
    assert!(
        !esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER - 1),
        "below the threshold the ladder keeps its ordinary retry"
    );
    assert_eq!(
        esc.attempts(),
        0,
        "a declined escalation must not spend budget"
    );
    assert!(
        esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
        "reaching the threshold must escalate to a surface recreate"
    );
    assert_eq!(esc.attempts(), 1);
}

/// The recreate budget is bounded per episode: after `MAX` attempts a
/// still-unacquirable surface falls back to the event-driven keep-alive
/// (never a recreate-loop on a wedged driver), and the existing slow-retry
/// ladder keeps scheduling wakes underneath.
#[test]
fn escalation_is_bounded_then_falls_back_to_keepalive() {
    let mut esc = SkipEscalation::default();
    // Each successful escalation resets the consecutive counter at the call
    // site, so the episode re-earns the threshold before the next attempt.
    for attempt in 1..=MAX_SKIPPED_FRAME_RECREATES {
        assert!(
            esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
            "attempt {attempt} is within the per-episode budget"
        );
    }
    for _ in 0..4 {
        assert!(
            !esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER * 2),
            "budget spent: chronic skips must fall back, never recreate-loop"
        );
    }
    assert_eq!(
        next_skipped_retry_delay(false, SKIPPED_FRAME_ESCALATE_AFTER * 2),
        Some(SKIPPED_FRAME_SLOW_RETRY),
        "the keep-alive wake must still schedule under the fallback"
    );
}

/// A successful present re-arms the recreate budget (same boundary as the
/// freeze watchdog) so a later, unrelated episode gets fresh attempts.
#[test]
fn present_rearms_the_escalation_budget() {
    let mut esc = SkipEscalation::default();
    for _ in 0..MAX_SKIPPED_FRAME_RECREATES {
        assert!(esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER));
    }
    assert!(!esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER));
    esc.note_presented();
    assert_eq!(esc.attempts(), 0);
    assert!(
        esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
        "a present must re-arm the budget for the next episode"
    );
}

/// Legitimate unavailability never escalates: an occluded surface is
/// correctly unacquirable (recreating it on a timer would churn every
/// covered window's swapchain), and a minimized window has nothing to
/// paint. Both keep today's retry ladder exactly.
#[test]
fn occluded_and_minimized_skips_never_escalate() {
    let mut esc = SkipEscalation::default();
    assert!(
        !esc.should_recreate(true, false, SKIPPED_FRAME_ESCALATE_AFTER * 4),
        "occluded skips must never escalate, however chronic"
    );
    assert!(
        !esc.should_recreate(false, true, SKIPPED_FRAME_ESCALATE_AFTER * 4),
        "minimized skips must never escalate"
    );
    assert_eq!(esc.attempts(), 0, "exempt skips must not spend budget");
}

/// A recreate attempt must never leave the loop wake-less: success
/// repaints immediately; failure schedules a slow bounded timed retry
/// (the keep-alive cadence) instead of dead-ending until an external
/// event arrives. This pins the fix for the failed-recreate strand.
#[test]
fn recreate_attempt_always_leaves_a_wake() {
    assert_eq!(
        after_recreate_attempt(false),
        RecreateFollowUp::Redraw,
        "a successful recreate must repaint the fresh surface immediately"
    );
    match after_recreate_attempt(true) {
        RecreateFollowUp::RetryAfter(delay) => {
            assert_eq!(
                delay, SKIPPED_FRAME_SLOW_RETRY,
                "a failed recreate must retry at the slow keep-alive cadence"
            );
        }
        RecreateFollowUp::Redraw => {
            panic!("a failed recreate must not redraw into the broken surface")
        }
    }
}

/// Repeated recreate FAILURES respect the per-episode escalation budget:
/// the attempt is spent when escalation fires (before the recreate runs),
/// so a failing recreate never refunds itself. After the budget, chronic
/// skips fall back to the keep-alive — with a wake scheduled at every step
/// of the sequence, so the loop can never strand.
#[test]
fn repeated_recreate_failures_spend_the_budget_then_keep_alive() {
    let mut esc = SkipEscalation::default();
    for attempt in 1..=MAX_SKIPPED_FRAME_RECREATES {
        assert!(
            esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER),
            "attempt {attempt}: escalation fires within budget"
        );
        // The recreate FAILS: the follow-up still schedules a wake, and
        // the spent attempt stays spent.
        assert_eq!(
            after_recreate_attempt(true),
            RecreateFollowUp::RetryAfter(SKIPPED_FRAME_SLOW_RETRY),
        );
        assert_eq!(
            esc.attempts(),
            attempt,
            "failure must not refund the budget"
        );
    }
    // Budget exhausted: no further recreates, keep-alive still wakes.
    assert!(!esc.should_recreate(false, false, SKIPPED_FRAME_ESCALATE_AFTER * 2));
    assert_eq!(
        next_skipped_retry_delay(false, SKIPPED_FRAME_ESCALATE_AFTER * 2),
        Some(SKIPPED_FRAME_SLOW_RETRY),
        "past the budget the ladder's keep-alive wake must still schedule"
    );
}

/// The escalation record is state-only: counters and flags, no terminal
/// content — same privacy discipline as the stall and episode records.
#[test]
fn skip_escalation_record_is_state_only() {
    let record = format_skip_escalation_record(2, 33, true);
    assert_eq!(
        record,
        "skip_escalation_recreate attempt=2 consecutive_skips=33 focused=true"
    );
}

/// Pins the spin guards on the skipped-frame retry: a minimized window never
/// retries (nothing to paint), and the consecutive-skip budget is finite so
/// a persistently-unavailable surface falls back to event-driven `Wait`
/// instead of wake-looping forever.
#[test]
fn skipped_retry_is_guarded_against_spin() {
    // Visible window, fresh budget: retry is allowed.
    assert!(
        should_schedule_skipped_retry(false, 0),
        "a visible window with budget remaining must retry a skipped frame"
    );
    // Minimized: never retry regardless of budget.
    assert!(
        !should_schedule_skipped_retry(true, 0),
        "a minimized (0x0) window must not retry — nothing to paint"
    );
    // Budget exhausted: stop retrying (fall back to Wait).
    assert!(
        !should_schedule_skipped_retry(false, MAX_SKIPPED_RETRIES),
        "the retry budget must be finite so a stuck surface can't wake-loop"
    );
    assert!(
        should_schedule_skipped_retry(false, MAX_SKIPPED_RETRIES - 1),
        "the last retry within budget must still be allowed"
    );
}

/// ANTI-FREEZE regression lock: once the fast-retry budget is spent, a
/// visible surface must STILL schedule a retry — a slow keep-alive, not
/// `None`. The previous policy dead-ended here, which under
/// `ControlFlow::Wait` left a long-lived, non-interacted background window
/// permanently unpainted (and apparently input-dead) until an external
/// window event forced a repaint. The one legitimate opt-out is a minimized
/// (0x0) window, which has nothing to paint and is re-armed by its restore
/// event.
#[test]
fn skipped_retry_falls_back_to_slow_keepalive_never_silent() {
    // Minimized: no retry regardless of budget (nothing to paint).
    assert_eq!(
        next_skipped_retry_delay(true, 0),
        None,
        "a minimized (0x0) window schedules no retry"
    );
    assert_eq!(
        next_skipped_retry_delay(true, MAX_SKIPPED_RETRIES + 5),
        None,
        "a minimized window stays opted out even past the budget"
    );

    // Visible, under budget: fast retry (recover within a frame).
    assert_eq!(
        next_skipped_retry_delay(false, 0),
        Some(SKIPPED_FRAME_RETRY),
        "a fresh skip retries fast"
    );
    assert_eq!(
        next_skipped_retry_delay(false, MAX_SKIPPED_RETRIES - 1),
        Some(SKIPPED_FRAME_RETRY),
        "the last skip within budget still retries fast"
    );

    // Visible, budget spent: slow keep-alive — the load-bearing invariant.
    // It must be a real scheduled retry (never `None`), and slower than the
    // fast cadence so it is not a busy-spin.
    for spent in [MAX_SKIPPED_RETRIES, MAX_SKIPPED_RETRIES + 1, 10_000] {
        let delay = next_skipped_retry_delay(false, spent);
        assert_eq!(
            delay,
            Some(SKIPPED_FRAME_SLOW_RETRY),
            "budget spent (n={spent}) must keep-alive, not go silent"
        );
    }
    assert!(
        SKIPPED_FRAME_SLOW_RETRY > SKIPPED_FRAME_RETRY,
        "the keep-alive must be slower than the fast retry (no busy-spin)"
    );
}

/// BLACK-SCREEN-ON-RESTORE residual: a restore that arrives as `Focused(true)`
/// WITHOUT a non-zero `Resized` first (the Windows case) must still clear the
/// minimized state so the vetoed skipped-frame retry can schedule and the
/// surface repaints. Drives the real `on_window_focus_changed` handler (the
/// extracted event-arm body), not a reimplementation.
#[test]
fn focus_gain_clears_minimized_state_so_repaint_can_schedule() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    // Simulate a minimize (a 0x0 `Resized`) followed by some skipped frames,
    // so the retry budget is partially spent and the spin guard is vetoing.
    app.window_minimized = true;
    app.consecutive_skipped_frames = 3;
    app.skip_episode.note_skipped(Instant::now());
    assert!(
        !should_schedule_skipped_retry(app.window_minimized, app.consecutive_skipped_frames),
        "precondition: while minimized the skipped-frame retry is vetoed (black screen)"
    );

    // The restore arrives ONLY as focus-gain (no non-zero Resized).
    app.on_window_focus_changed(true);

    assert!(
        !app.window_minimized,
        "focus-gain restore must clear the minimized flag"
    );
    assert_eq!(
        app.consecutive_skipped_frames, 0,
        "focus-gain restore must reset the skipped-frame retry budget"
    );
    assert!(
        should_schedule_skipped_retry(app.window_minimized, app.consecutive_skipped_frames),
        "after restore the bounded retry-wake must no longer be vetoed"
    );
    assert!(
        app.pending_surface_reconfigure,
        "the active episode must be observed before restore resets the retry budget"
    );
}

#[test]
fn focus_gain_without_skips_does_not_request_reconfigure() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    assert!(!app.skip_episode.is_active());

    app.on_window_focus_changed(true);

    assert!(
        !app.pending_surface_reconfigure,
        "the ordinary focus path must not add surface work"
    );
}

#[test]
fn focus_loss_clears_every_window_pointer_latch() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.pointer_left_held = true;
    app.pointer_drag = PointerDrag::Scrollbar { grab_dy: 1.0 };
    app.divider_drag = Some(0);
    app.rail_seam_drag = true;
    app.tab_bar_seam_drag = true;
    app.rail_ws_drag = Some(RailWorkspaceDrag::new(0, 4.0, 8.0));
    app.top_tab_drag = Some(TopTabDrag::new(0, 4.0, 8.0));
    app.report_button = Some(CoreMouseButton::Left);

    app.on_window_focus_changed(false);

    assert!(!app.pointer_left_held);
    assert_eq!(app.pointer_drag, PointerDrag::None);
    assert_eq!(app.divider_drag, None);
    assert!(!app.rail_seam_drag);
    assert!(!app.tab_bar_seam_drag);
    assert_eq!(app.rail_ws_drag, None);
    assert_eq!(app.top_tab_drag, None);
    assert_eq!(app.report_button, None);
}

/// The pointer leaving the window surface is one named handler with two
/// responsibilities, and the auto-hide half is gated.
///
/// A compositor can terminate the implicit pointer grab at the surface edge
/// without delivering the paired release, so leaving during a divider gesture
/// settles that gesture. The motion-aware reveal trigger's previous sample is
/// dropped ONLY while auto-hide is active; with the rail pinned, the sample
/// survives so a later in-window segment still measures from where the pointer
/// actually was.
#[test]
fn cursor_left_settles_the_divider_and_clears_the_rail_sample_only_under_autohide() {
    let Some(mut app) = build_idle_app() else {
        return;
    };

    // Rail pinned, auto-hide off: the sample is left exactly as it was.
    app.settings.workspace_rail = crate::settings::WorkspaceRail::Always;
    app.settings.tab_rail_autohide = false;
    app.divider_drag = Some(0);
    app.last_rail_pointer_px = Some(12.0);
    assert!(!app.rail_autohide_active());

    app.on_cursor_left();

    assert_eq!(
        app.divider_drag, None,
        "leaving settles the divider gesture"
    );
    assert_eq!(
        app.last_rail_pointer_px,
        Some(12.0),
        "an inactive auto-hide leaves the motion sample untouched"
    );

    // Auto-hide on: the same event drops the stale sample.
    app.settings.tab_rail_autohide = true;
    app.divider_drag = Some(0);
    assert!(app.rail_autohide_active());

    app.on_cursor_left();

    assert_eq!(app.divider_drag, None);
    assert_eq!(
        app.last_rail_pointer_px, None,
        "an active auto-hide drops the pre-leave sample so re-entry starts fresh"
    );
}

#[test]
fn active_session_change_clears_window_latches_prefix_and_pending_upload() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.divider_drag = Some(0);
    app.rail_seam_drag = true;
    app.tab_bar_seam_drag = true;
    app.rail_ws_drag = Some(RailWorkspaceDrag::new(0, 4.0, 8.0));
    app.top_tab_drag = Some(TopTabDrag::new(0, 4.0, 8.0));
    app.pending_image_paste = Some(PendingImagePaste {
        session: app.sessions.active_id(),
        png: vec![1, 2, 3],
    });
    let prefix = app.prefix_engine.prefix().expect("default prefix enabled");
    app.prefix_engine.on_chord(prefix, Instant::now());
    assert!(app.prefix_engine.is_pending());

    app.on_active_session_changed();

    assert_eq!(app.divider_drag, None);
    assert!(!app.rail_seam_drag);
    assert!(!app.tab_bar_seam_drag);
    assert_eq!(app.rail_ws_drag, None);
    assert_eq!(app.top_tab_drag, None);
    assert!(!app.prefix_engine.is_pending());
    assert!(app.pending_image_paste.is_none());
}

#[test]
fn overlay_entry_clears_every_window_drag_latch() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.divider_drag = Some(0);
    app.rail_seam_drag = true;
    app.tab_bar_seam_drag = true;
    app.rail_ws_drag = Some(RailWorkspaceDrag::new(0, 4.0, 8.0));
    app.top_tab_drag = Some(TopTabDrag::new(0, 4.0, 8.0));

    app.reset_pointer_state_for_overlay();

    assert_eq!(app.divider_drag, None);
    assert!(!app.rail_seam_drag);
    assert!(!app.tab_bar_seam_drag);
    assert_eq!(app.rail_ws_drag, None);
    assert_eq!(app.top_tab_drag, None);
}

/// Same residual via the other Windows restore signal: `Occluded(false)`
/// without a non-zero `Resized`. Drives the real `on_window_occluded`
/// handler. The occlude (`true`) direction must NOT set the flag (occlusion
/// is not minimize).
#[test]
fn un_occlude_clears_minimized_state_and_occlude_does_not_set_it() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.window_minimized = true;
    app.consecutive_skipped_frames = 2;
    app.skip_episode.note_skipped(Instant::now());

    assert!(
        app.on_window_occluded(false),
        "active skip recovery must request an immediate redraw"
    );
    assert!(
        !app.window_minimized,
        "Occluded(false) restore must clear the minimized flag"
    );
    assert_eq!(
        app.consecutive_skipped_frames, 0,
        "Occluded(false) restore must reset the skipped-frame retry budget"
    );
    assert!(
        app.pending_surface_reconfigure,
        "un-occlude must defer one surface reconfigure"
    );

    // Occlude (covered by another window) is NOT minimize: the flag must
    // stay false so a merely-covered window keeps repainting.
    assert!(!app.on_window_occluded(true));
    assert!(
        !app.window_minimized,
        "Occluded(true) must not be treated as minimize"
    );
}

/// Guard: restoring when NOT minimized is a harmless no-op (the Linux/macOS
/// path, where un-minimize goes through `Resized` and the flag is already
/// false by the time Focused/Occluded fire). Must not clobber a live budget.
#[test]
fn restore_from_minimized_is_a_noop_when_not_minimized() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.window_minimized = false;
    app.consecutive_skipped_frames = 4;
    let cleared = app.restore_from_minimized();
    assert!(!cleared, "no minimized state to clear");
    assert_eq!(
        app.consecutive_skipped_frames, 4,
        "a no-op restore must not touch the retry budget"
    );
}

/// The redraw path has two early exits taken when there is no surface. Both
/// left the window-event handler before its trailing pending-exit check, so
/// the extracted handler reports the early exit and the match arm returns on
/// it. Without that signal a surface-less redraw would start honouring a
/// pending exit it previously skipped.
#[test]
fn a_surfaceless_redraw_reports_the_early_exit_that_skips_pending_exit() {
    let mut app = build_idle_app().expect("headless app builds without a surface");
    assert!(app.gpu.is_none(), "fixture must have no surface");
    app.pending_exit = true;

    assert!(
        app.on_redraw_requested(),
        "a redraw with no surface takes an early exit"
    );
    assert!(
        app.pending_exit,
        "the early exit must leave the pending-exit flag for a later event"
    );
}

/// The OS-theme arm records the reported preference unconditionally and
/// re-resolves the active theme only while following is on. Recording is what
/// a later `follow_os_theme` switch reads, so it must not become conditional.
#[test]
fn os_theme_report_is_recorded_always_and_followed_only_when_enabled() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = build_idle_app().expect("headless app builds without a surface");
    app.settings.follow_os_theme = false;
    let authored = app.theme;

    app.on_os_theme_changed(winit::window::Theme::Dark);
    assert_eq!(
        app.os_theme,
        Some(winit::window::Theme::Dark),
        "the reported preference is recorded while following is off"
    );
    assert_eq!(
        app.theme, authored,
        "following off must leave the active theme untouched"
    );

    app.settings.follow_os_theme = true;
    app.on_os_theme_changed(winit::window::Theme::Light);
    assert_eq!(
        app.os_theme,
        Some(winit::window::Theme::Light),
        "the latest reported preference replaces the previous one"
    );
}

/// Build a fresh, un-driven `App` for wake-scheduling tests over a headless
/// (no-PTY) session, so the fixture creates no OS child.
/// The `ModifiersChanged` arm forwards to `on_modifiers_changed`. The cached
/// modifier state is what the next `KeyboardInput` encodes with, and the arm
/// additionally repaints the Ctrl-armed path underline -- but only while
/// interactive paths are on AND a path is hovered. Both halves are pinned so
/// the forwarding cannot quietly drop either the cache update or its gate.
#[test]
fn modifiers_forwarding_caches_state_and_gates_the_ctrl_repaint() {
    use winit::keyboard::ModifiersState;

    let mut app = build_idle_app().expect("headless app builds without a surface");
    app.settings.interactive_paths = false;
    app.hovered_path = None;
    app.needs_rebuild = false;

    app.on_modifiers_changed(winit::event::Modifiers::from(ModifiersState::CONTROL));
    assert!(
        app.modifiers.ctrl,
        "ctrl must reach the cached modifier state"
    );
    assert!(!app.modifiers.alt, "alt must stay clear");
    assert!(!app.modifiers.shift, "shift must stay clear");
    assert!(
        !app.super_key,
        "super is tracked separately and must stay clear"
    );
    assert!(
        !app.needs_rebuild,
        "with interactive paths off, a ctrl transition must not force a rebuild"
    );

    app.settings.interactive_paths = true;
    app.hovered_path = Some(crate::paths::Resolved {
        abs: "/synthetic/hovered".to_owned(),
        kind: crate::paths::FsKind::File,
        line: None,
        col: None,
    });
    app.on_modifiers_changed(winit::event::Modifiers::default());
    assert!(
        !app.modifiers.ctrl,
        "releasing ctrl must clear the cached state"
    );
    assert!(
        app.needs_rebuild,
        "a ctrl transition over a hovered path must repaint the armed underline"
    );
}

fn build_idle_app() -> Option<App> {
    let dims = Dimensions::new(24, 80);
    let (app, _terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        dims,
        Settings::default(),
    );
    Some(app)
}

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .expect("recorded bytes")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A headless app whose active writer records exact terminal input. This
/// keeps activity-policy tests at the production key-routing seam without
/// writing to a real shell.
fn build_recording_app() -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(24, 80);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, _terminal) = crate::native::test_support::headless_app_with_writer(
        NativeOptions::default(),
        dims,
        Settings::default(),
        writer,
    );
    Some((app, bytes))
}

#[test]
fn unfocused_cursor_params_are_solid_stationary_and_focus_aware() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    let now = Instant::now();
    app.focused = false;
    app.cursor_anim_alpha = 0.25;
    app.cursor_anim_offset = [6.0, -3.0];
    app.cursor_ease_deadline = Some(now + Duration::from_millis(16));
    app.cursor_slide_deadline = Some(now + Duration::from_millis(16));
    app.cursor_slide_start = Some(now);

    app.update_cursor_easing(now, false, true);
    let snapshot = Snapshot {
        dimensions: app.grid,
        cursor: Position::default(),
        cursor_visible: true,
        colors: crate::core::DynamicColors::default(),
        cells: vec![crate::core::Cell::default(); app.grid.columns * app.grid.rows],
    };
    app.update_cursor_motion(
        now,
        &snapshot,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
    );

    let params = app.cursor_render_params();
    assert!(
        !params.focused,
        "focus bit reaches the shared cursor params"
    );
    assert_eq!(params.alpha, 1.0, "unfocused cursor holds solid");
    assert_eq!(params.offset, [0.0, 0.0], "unfocused cursor snaps");
    assert_eq!(app.cursor_blink_fade_deadline(), None);
    assert_eq!(app.cursor_motion_deadline(), None);
}

#[test]
fn armed_top_drag_keeps_the_grabbed_label_in_the_rendered_frame() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.set_session_tab_title_for_test(0, "GrabbedTop");
    let mut drag = TopTabDrag::new(0, 0.0, 0.0);
    assert!(drag.update_arm(pointer::CHROME_DRAG_THRESHOLD_PX + 1.0, 0.0));
    drag.drop_idx = 1;
    app.top_tab_drag = Some(drag);

    let output = app.render_top_bar_widget(
        80,
        0.0,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
        WindowPadding::ZERO,
    );
    let rendered: String = output.glyphs.iter().map(|glyph| glyph.ch).collect();
    assert!(rendered.contains("GrabbedTop"));
    assert!(
        output
            .glyphs
            .iter()
            .filter(|glyph| "GrabbedTop".contains(glyph.ch))
            .any(|glyph| glyph.attrs.bold()),
        "armed proxy label must retain lifted emphasis"
    );
}

#[test]
fn armed_rail_drag_keeps_the_grabbed_label_in_the_rendered_frame() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.rename_workspace_for_test(0, "GrabbedRail");
    let mut drag = RailWorkspaceDrag::new(0, 0.0, 0.0);
    assert!(drag.update_arm(0.0, pointer::CHROME_DRAG_THRESHOLD_PX + 1.0));
    drag.drop_idx = 1;
    app.rail_ws_drag = Some(drag);

    let cols = 16;
    let output = app.render_rail_widget(
        cols,
        24,
        [0.0, 0.0],
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
        RailSide::Left,
    );
    let label_row = output.glyphs.chunks(cols).any(|row| {
        row.iter()
            .map(|glyph| glyph.ch)
            .collect::<String>()
            .contains("GrabbedRail")
    });
    assert!(label_row);
    assert!(
        output
            .glyphs
            .iter()
            .filter(|glyph| "GrabbedRail".contains(glyph.ch))
            .any(|glyph| glyph.attrs.bold()),
        "armed proxy label must retain lifted emphasis"
    );
}

/// Regression guard for the focus-gated config-reload poll. On a fresh,
/// un-driven `App` the live-reload watcher is the only focus-dependent wake
/// source (cursor blink stays `None` until polled, and every other source
/// is at rest), so toggling focus isolates the gate: focused schedules the
/// 1 Hz config stat, unfocused suppresses it and the loop parks at
/// zero-wake idle. A regression that drops the gate would bring back the
/// once-a-second background wake this test forbids.
#[test]
fn config_reload_wake_is_suppressed_while_unfocused() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    // No resolvable config path on this host ⇒ no deadline to gate; skip.
    let Some(config_deadline) = app.settings_reloader.deadline() else {
        return;
    };

    app.focused = true;
    assert_eq!(
        app.next_wake_deadline(),
        Some(config_deadline),
        "a focused window schedules the config-reload poll"
    );

    app.focused = false;
    assert_eq!(
        app.next_wake_deadline(),
        None,
        "a backgrounded window schedules no timer wake (zero-wake idle)"
    );
}

/// NF20 regression: a multiplexer prefix (default Ctrl+B) that is pressed and
/// then times out with no follow-up key must not busy-spin the event loop.
///
/// `pending_deadline()` is a `next_wake_deadline` source, so a prefix left
/// pending past its timeout kept the loop scheduling `WaitUntil(<past>)` — a
/// 0-timeout poll that returns immediately every iteration and pins a core —
/// until the next key or focus loss cleared it. The about-to-wait maintenance
/// pass now expires the stale prefix on the timer, so the recomputed wait
/// deadline is never a past instant. Drives the real deadline arithmetic
/// (enter → wake at the boundary → maintenance → recompute); fails before the
/// maintenance-side expiry existed (the final assert saw a past deadline).
#[test]
fn timed_out_prefix_does_not_spin_the_event_loop() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    // Isolate the prefix as the only possible wake source: unfocused
    // suppresses the config-reload poll, autohide is off (no rail wake), and
    // nothing else is armed on a fresh idle app.
    app.focused = false;
    assert_eq!(
        app.next_wake_deadline(),
        None,
        "idle app parks at zero wake"
    );

    // Press the multiplexer prefix at t0; it becomes pending and arms a
    // timeout deadline the loop will wait on.
    let t0 = Instant::now();
    let prefix = app
        .prefix_engine
        .prefix()
        .expect("the default pane prefix (Ctrl+B) is enabled");
    app.prefix_engine.on_chord(prefix, t0);
    assert!(app.prefix_engine.is_pending(), "prefix pending after entry");
    let deadline = app
        .prefix_engine
        .pending_deadline()
        .expect("a pending prefix arms a timeout boundary");
    assert_eq!(
        app.next_wake_deadline(),
        Some(deadline),
        "the pending prefix is the scheduled wake (a future boundary)"
    );

    // The loop wakes at/after the boundary and runs its maintenance pass.
    // That pass MUST forget the timed-out prefix; otherwise the recomputed
    // deadline is `deadline` again — now in the past — and the loop spins.
    let woken = deadline + Duration::from_millis(1);
    app.run_about_to_wait_maintenance_for_test(woken);
    assert!(
        !app.prefix_engine.is_pending(),
        "the timed-out prefix is expired on the timer, not left pending"
    );
    match app.next_wake_deadline() {
        None => {}
        Some(next) => assert!(
            next > woken,
            "no past-instant wake survives the maintenance pass \
             (a deadline <= now re-arms WaitUntil(past) and busy-spins)"
        ),
    }
}

/// NF21-2 acceptance (ii): a single-pane terminal with nothing animating
/// schedules NO animation wake — the restored `animation_deadline()`
/// collector source contributes nothing at rest, so the strict zero-wake
/// idle invariant is preserved.
#[test]
fn idle_single_pane_schedules_no_animation_wake() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.focused = false;
    assert_eq!(
        app.animation_deadline(),
        None,
        "no contributor is animating at rest"
    );
    assert_eq!(
        app.next_wake_deadline(),
        None,
        "idle single-pane parks at zero wake — the NF21-2 source adds nothing at rest"
    );
}

/// NF21-2 acceptance (i, bell contributor): a bell flash schedules a repaint
/// wake and a due wake requests a rebuild — even while the window is
/// unfocused and the cursor is not blinking. Fails before both halves of the
/// fix (no wake scheduled; no rebuild on the due wake).
#[test]
fn bell_flash_while_unfocused_schedules_a_wake_and_advances() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.focused = false;
    assert_eq!(
        app.next_wake_deadline(),
        None,
        "precondition: the idle app parks at zero wake"
    );
    app.bell_flash_start = Some(Instant::now());
    let wake = app.next_wake_deadline();
    assert!(
        wake.is_some(),
        "an in-flight bell flash must schedule a repaint wake (NF21-2)"
    );
    app.needs_rebuild = false;
    app.run_about_to_wait_maintenance_for_test(wake.unwrap());
    assert!(
        app.needs_rebuild,
        "a due animation wake requests a rebuild (no wake-without-redraw)"
    );
}

/// Reveal-zone regression (#1, padding-aware trigger): the trigger band is
/// measured from the window edge inward by `pad + reveal_px`, so a pointer
/// resting `reveal_px` into the *visible content* (just past the padding
/// margin) reveals — it is not stranded behind the padding.
#[test]
fn reveal_trigger_zone_is_padding_aware_interior_band() {
    let pad = 12.0;
    let reveal_px = 8.0;
    let reach = pad + reveal_px; // 20
    let surface_w = 1000.0;

    // LEFT: content starts at x=pad(12). A pointer at x=15 (3px into visible
    // content) must trigger; the old edge-only zone [0, 8] would have
    // stranded it behind the padding.
    assert!(reveal_edge_contains(RailSide::Left, 15.0, reach, surface_w));
    assert!(reveal_edge_contains(RailSide::Left, 0.0, reach, surface_w));
    assert!(reveal_edge_contains(RailSide::Left, 20.0, reach, surface_w));
    assert!(!reveal_edge_contains(
        RailSide::Left,
        21.0,
        reach,
        surface_w
    ));

    // RIGHT: content ends at surface_w-pad(988). A pointer at x=985 (3px into
    // visible content from the right) must trigger.
    assert!(reveal_edge_contains(
        RailSide::Right,
        985.0,
        reach,
        surface_w
    ));
    assert!(reveal_edge_contains(
        RailSide::Right,
        surface_w,
        reach,
        surface_w
    ));
    assert!(reveal_edge_contains(
        RailSide::Right,
        surface_w - reach,
        reach,
        surface_w
    ));
    assert!(!reveal_edge_contains(
        RailSide::Right,
        surface_w - reach - 1.0,
        reach,
        surface_w
    ));
}

/// Reveal-zone regression (#2, keep-alive = union): the keep-alive region is
/// the trigger zone UNIONED with the drawn band, so a pointer parked anywhere
/// over the revealed band (or in the padding-aware trigger zone) holds the
/// rail — hide grace begins only on leaving that union. This also pins the
/// union so a future band narrower than the trigger cannot leave a gap.
#[test]
fn reveal_keep_alive_is_the_union_of_trigger_and_band() {
    let reach = 20.0;
    let surface_w = 1000.0;

    // LEFT band drawn out to seam_x=128. Mid-band (x=64) holds; the trigger
    // zone (x=5) holds; past the seam (x=200) does not.
    let seam_l = 128.0;
    assert!(reveal_band_contains(
        RailSide::Left,
        64.0,
        seam_l,
        reach,
        surface_w
    ));
    assert!(reveal_band_contains(
        RailSide::Left,
        5.0,
        seam_l,
        reach,
        surface_w
    ));
    assert!(!reveal_band_contains(
        RailSide::Left,
        200.0,
        seam_l,
        reach,
        surface_w
    ));

    // UNION guard: an artificially narrow band (seam at x=10, narrower than
    // reach=20) still keeps alive across the whole trigger zone — the trigger
    // fills the gap the thin band would otherwise leave.
    let thin_seam = 10.0;
    assert!(
        reveal_band_contains(RailSide::Left, 15.0, thin_seam, reach, surface_w),
        "trigger zone covers the gap a band narrower than the reach leaves"
    );

    // RIGHT band drawn from seam_x=872 rightward. Mid-band (x=936) holds; the
    // right trigger zone (x=995) holds; left of the seam (x=800) does not.
    let seam_r = 872.0;
    assert!(reveal_band_contains(
        RailSide::Right,
        936.0,
        seam_r,
        reach,
        surface_w
    ));
    assert!(reveal_band_contains(
        RailSide::Right,
        995.0,
        seam_r,
        reach,
        surface_w
    ));
    assert!(!reveal_band_contains(
        RailSide::Right,
        800.0,
        seam_r,
        reach,
        surface_w
    ));
}

/// Reveal-zone regression (motion-aware trigger, from the live pointer
/// trace): a fast approach delivers samples 30–200 px apart that jump clean
/// over the static point zone, so the arm must test the whole *segment*
/// between consecutive samples — not just the current point.
#[test]
fn reveal_edge_segment_crosses_a_fast_sweep_over_the_point_zone() {
    let reach = 29.0; // ≈ the reference trace's reach
    let surface_w = 1000.0;

    // LEFT: the trace's dominant case — a move from x=60 to x=−5 has NEITHER
    // endpoint that a bounded [0, reach] point test would accept, yet the
    // path sweeps through the trigger band → the motion-aware test arms it.
    assert!(reveal_edge_segment_crosses(
        RailSide::Left,
        60.0,
        -5.0,
        reach,
        surface_w
    ));
    // A move that stops short of the band (60 → 40, both past the reach)
    // does NOT cross — the pointer never reached the edge.
    assert!(!reveal_edge_segment_crosses(
        RailSide::Left,
        60.0,
        40.0,
        reach,
        surface_w
    ));
    // A sweep from off-window INTO content past the band still crosses (the
    // pointer entered at the edge), where the current point alone would miss.
    assert!(reveal_edge_segment_crosses(
        RailSide::Left,
        -8.0,
        50.0,
        reach,
        surface_w
    ));

    // RIGHT: symmetric — a fast sweep toward the right edge that overshoots
    // past surface_w crosses the right band [surface_w − reach, surface_w].
    assert!(reveal_edge_segment_crosses(
        RailSide::Right,
        940.0,
        1010.0,
        reach,
        surface_w
    ));
    // Stopping short of the right band (940 → 960, both left of the band)
    // does not cross.
    assert!(!reveal_edge_segment_crosses(
        RailSide::Right,
        940.0,
        960.0,
        reach,
        surface_w
    ));
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn app_id_matches_packaged_desktop_identity() {
    assert_eq!(APP_ID, "io.unfinished_works.odytty");
    assert_eq!(
        linux_window_app_id(&NativeOptions::default()),
        "io.unfinished_works.odytty"
    );

    let overridden = NativeOptions {
        app_id: Some("com.example.Term".to_owned()),
        ..NativeOptions::default()
    };
    assert_eq!(linux_window_app_id(&overridden), "com.example.Term");
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn desktop_file_startup_wm_class_matches_app_id() {
    let desktop = include_str!("../../../dist/linux/io.unfinished_works.odytty.desktop");
    assert!(desktop.contains("Icon=io.unfinished_works.odytty\n"));
    assert!(desktop.contains(&format!("StartupWMClass={APP_ID}\n")));
    for key in [
        "X-TerminalArgExec=-e\n",
        "X-TerminalArgDir=--working-directory=\n",
        "X-TerminalArgTitle=--title=\n",
        "X-TerminalArgAppId=--app-id=\n",
        "X-TerminalArgHold=--hold\n",
    ] {
        assert!(desktop.contains(key), "missing desktop key {key:?}");
    }
}

#[test]
fn onboarding_opens_only_on_first_run_or_override() {
    // Absent config ⇒ first run ⇒ show.
    let missing = std::path::Path::new("/nonexistent/odytty/odytty.conf");
    assert!(should_show_onboarding(false, Some(missing)));
    // A path that exists ⇒ NOT first run ⇒ do not show. Cargo guarantees
    // this manifest is present during the test.
    let present = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    assert!(present.exists());
    assert!(!should_show_onboarding(false, Some(present.as_path())));
    // Env override forces it on regardless of file state.
    assert!(should_show_onboarding(true, Some(present.as_path())));
    // Unresolvable path ⇒ fail-safe to not nagging.
    assert!(!should_show_onboarding(false, None));
}

#[test]
fn plain_render_quality_forces_post_options_inactive() {
    let settings = Settings {
        render_quality: crate::settings::RenderQuality::Plain,
        bloom: true,
        crt: true,
        ..Settings::default()
    };

    let bloom = bloom_options(&settings);
    let crt = crt_options(&settings);

    assert!(!bloom.enabled);
    assert!(!crt.enabled);
}

#[test]
fn blink_holds_solid_when_not_blinking() {
    let mut state = blink();
    let t0 = Instant::now();
    // Steady cursor: always on, no scheduled wake.
    assert!(state.poll(t0, false, true));
    assert_eq!(state.deadline(), None);
    assert!(state.poll(t0 + Duration::from_secs(10), false, true));
    assert_eq!(state.deadline(), None);
}

#[test]
fn blink_holds_solid_when_unfocused() {
    let mut state = blink();
    let t0 = Instant::now();
    // Blinking requested but unfocused: solid, no wake scheduled.
    assert!(state.poll(t0, true, false));
    assert_eq!(state.deadline(), None);
}

#[test]
fn blink_waits_for_activity_hold_then_toggles_at_the_interval() {
    let mut state = blink();
    let t0 = Instant::now();
    // The first visible sample uses the same quiet hold as keyboard input.
    assert!(state.poll(t0, true, true));
    let deadline = state.deadline().expect("blink should schedule a wake");
    assert_eq!(deadline, t0 + CURSOR_ACTIVITY_HOLD);
    assert!(!state.is_due(t0));

    // Before the activity boundary: unchanged, still on.
    assert!(state.poll(
        t0 + CURSOR_ACTIVITY_HOLD - Duration::from_millis(1),
        true,
        true
    ));
    assert!(!state.is_due(t0 + CURSOR_ACTIVITY_HOLD - Duration::from_millis(1)));

    // The first boundary flips to off. Later edges retain the configured
    // half-period rather than adding another activity hold.
    assert!(state.is_due(t0 + CURSOR_ACTIVITY_HOLD));
    assert!(!state.poll(t0 + CURSOR_ACTIVITY_HOLD, true, true));
    assert_eq!(
        state.deadline(),
        Some(t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(500)),
        "next toggle is one interval later"
    );

    // Next interval flips back on.
    assert!(state.poll(
        t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(500),
        true,
        true
    ));
}

#[test]
fn blink_resets_to_solid_when_focus_lost_mid_cycle() {
    let mut state = blink();
    let t0 = Instant::now();
    assert!(state.poll(t0, true, true));
    // Toggle to off-phase.
    assert!(!state.poll(t0 + CURSOR_ACTIVITY_HOLD, true, true));
    // Losing focus forces solid-on and clears the scheduled wake.
    assert!(state.poll(
        t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(100),
        true,
        false
    ));
    assert_eq!(state.deadline(), None);
}

#[test]
fn blink_activity_rearms_visibility_and_parks_after_long_idle() {
    let mut state = blink();
    let t0 = Instant::now();
    assert!(state.poll(t0, true, true));
    assert!(!state.poll(t0 + CURSOR_ACTIVITY_HOLD, true, true));

    let activity = t0 + CURSOR_ACTIVITY_HOLD + Duration::from_millis(20);
    state.note_activity(activity, true, true);
    assert!(
        state.poll(activity, true, true),
        "activity restores solid-on"
    );
    assert_eq!(state.deadline(), Some(activity + CURSOR_ACTIVITY_HOLD));

    let stop = activity + CURSOR_BLINK_STOP_AFTER;
    assert!(state.is_due(stop));
    assert!(state.poll(stop, true, true), "long idle parks visible");
    assert_eq!(state.deadline(), None, "parked cursor cannot self-wake");
    assert!(
        state.poll(stop + Duration::from_millis(1), true, true),
        "the next render sample keeps the idle-parked cursor visible"
    );
    assert_eq!(
        state.deadline(),
        None,
        "a render after the idle boundary cannot re-arm blinking"
    );

    state.note_activity(stop + Duration::from_millis(1), true, true);
    assert_eq!(
        state.deadline(),
        Some(stop + Duration::from_millis(1) + CURSOR_ACTIVITY_HOLD),
        "the next key re-arms one bounded visible hold"
    );
}

#[test]
fn blink_idle_park_survives_the_maintenance_to_render_path() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    let activity = Instant::now();
    app.focused = true;
    app.note_cursor_keyboard_activity(activity);
    let stop = activity + CURSOR_BLINK_STOP_AFTER;

    // This is the event-loop consumer: it resolves the deadline and asks
    // for the redraw whose render path will sample the cursor again.
    app.run_about_to_wait_maintenance_for_test(stop);
    assert_eq!(
        app.cursor_blink.deadline(),
        None,
        "maintenance clears the parked cursor's wake before redraw"
    );

    // This is the following render consumer. The shipped bug treated the
    // cleared activity timestamp as first use here and re-armed a 650 ms
    // blink wake after every idle park.
    let blinking = app.terminal.lock().expect("terminal").cursor_blinking();
    let focused = app.focused;
    assert!(
        app.cursor_blink
            .poll(stop + Duration::from_millis(1), blinking, focused)
    );
    assert_eq!(
        app.cursor_blink.deadline(),
        None,
        "the redraw leaves an idle-parked blinking cursor solid with no wake"
    );
    assert!(
        !app.cursor_blink.is_due(stop + Duration::from_secs(1)),
        "the parked cursor cannot leave a stale deadline for the scheduler"
    );
}

#[test]
fn blink_activity_never_overrides_steady_or_unfocused_cursor_policy() {
    let mut state = blink();
    let now = Instant::now();

    state.note_activity(now, false, true);
    assert!(state.poll(now, false, true));
    assert_eq!(
        state.deadline(),
        None,
        "steady DECSCUSR stays authoritative"
    );

    state.note_activity(now, true, false);
    assert!(state.poll(now, true, false));
    assert_eq!(state.deadline(), None, "unfocused cursor has no wake");
}

#[test]
fn keyboard_activity_rearms_press_and_repeat_without_changing_pty_bytes() {
    let Some((mut app, bytes)) = build_recording_app() else {
        return;
    };
    app.cursor_blink.park();
    app.cursor_anim_alpha = 0.0;
    app.cursor_ease_deadline = Some(Instant::now() + Duration::from_millis(16));
    app.cursor_ease_phase_on = false;
    let logical = WinitKey::Character("x".into());

    app.handle_key_event(
        logical.clone(),
        logical.clone(),
        PhysicalKey::Code(winit::keyboard::KeyCode::KeyX),
        KeyEventType::Press,
    );
    assert!(
        app.cursor_blink.deadline().is_some(),
        "a press re-arms the visible hold"
    );
    assert_eq!(app.cursor_anim_alpha, 1.0, "a press cancels an off fade");
    assert_eq!(app.cursor_ease_deadline, None, "a press adds no fade wake");

    app.handle_key_event(
        logical.clone(),
        logical.clone(),
        PhysicalKey::Code(winit::keyboard::KeyCode::KeyX),
        KeyEventType::Repeat,
    );
    assert!(
        app.cursor_blink.deadline().is_some(),
        "a repeat keeps the cursor visible"
    );

    app.cursor_blink.park();
    app.handle_key_event(
        logical.clone(),
        logical,
        PhysicalKey::Code(winit::keyboard::KeyCode::KeyX),
        KeyEventType::Release,
    );
    assert_eq!(
        app.cursor_blink.deadline(),
        None,
        "a release alone is not keyboard activity"
    );
    assert_eq!(
        bytes.lock().expect("recorded bytes").as_slice(),
        b"xx",
        "activity tracking adds no PTY bytes and keeps release encoding unchanged"
    );
}

#[test]
fn focus_boundaries_park_then_rearm_the_active_cursor_hold() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    let now = Instant::now();
    app.cursor_blink.note_activity(now, true, true);
    assert!(app.cursor_blink.deadline().is_some());

    app.on_window_focus_changed(false);
    assert_eq!(
        app.cursor_blink.deadline(),
        None,
        "focus loss immediately drops the active blink wake"
    );

    app.on_window_focus_changed(true);
    assert!(
        app.cursor_blink.deadline().is_some(),
        "focus gain begins a fresh visible hold for the active pane"
    );
}

#[test]
fn reduced_motion_keeps_activity_blink_edges_hard() {
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.settings.reduced_motion = true;
    let now = Instant::now();
    app.cursor_blink.note_activity(now, true, true);
    let cursor_on = app
        .cursor_blink
        .poll(now + CURSOR_ACTIVITY_HOLD, true, true);
    assert!(!cursor_on, "the activity boundary still reaches blink off");
    app.update_cursor_easing(now + CURSOR_ACTIVITY_HOLD, cursor_on, true);
    assert_eq!(app.cursor_blink_alpha(), 1.0, "no reduced-motion fade");
    assert_eq!(app.cursor_blink_fade_deadline(), None, "no easing wake");
    assert!(
        app.cursor_blink.deadline().is_some(),
        "the normal blink half-period remains a bounded hard-edge wake"
    );
}

// ---- Shell-integration "applies to new shells" notice ----

/// The gating decision is pure so EVERY combination is pinned here —
/// including the no-live-session case, which `build_idle_app` cannot
/// construct (`App::new` always seeds one session, and there is no public
/// close to drain it).
#[test]
fn new_shells_notice_fires_only_on_off_to_on_with_session() {
    // prior, next, has_live_session
    assert!(
        App::should_announce_shell_integration_to_new_shells(false, true, true),
        "OFF->ON with a live shell is the one honest case"
    );
    // No live session to inform -> stay silent.
    assert!(!App::should_announce_shell_integration_to_new_shells(
        false, true, false
    ));
    // ON at startup / ON->ON reload: no transition.
    assert!(!App::should_announce_shell_integration_to_new_shells(
        true, true, true
    ));
    // ON->OFF: the reverse toggle never nags.
    assert!(!App::should_announce_shell_integration_to_new_shells(
        true, false, true
    ));
    // OFF->OFF: no transition.
    assert!(!App::should_announce_shell_integration_to_new_shells(
        false, false, true
    ));
}

/// Driving the real settings-reload seam OFF->ON while a live session
/// exists must surface the transient notice — the wiring added here.
#[test]
fn off_to_on_reload_raises_new_shells_notice() {
    // The reload seam republishes process-global render state (default
    // colors / palette / contrast floor), so serialize against the other
    // render-globals tests.
    let _guard = crate::test_lock::render_globals_lock();
    let Some(mut app) = build_idle_app() else {
        return;
    };
    // Force shell_integration OFF as the precondition (it now ships ON by
    // default) so flipping it ON is the genuine OFF->ON transition this
    // seam must announce. `build_idle_app` seeds one live session.
    app.settings.shell_integration = false;
    assert!(!app.settings.shell_integration);
    assert!(!app.sessions.is_empty());
    assert!(
        app.open_notice_message_for_test().is_none(),
        "no notice before the toggle"
    );

    let mut next = app.settings.clone();
    next.shell_integration = true;
    app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some("Shell integration applies to new shells — open a new tab or split to activate."),
        "an OFF->ON toggle with a live shell must surface the new-shells notice"
    );
}

/// The reverse transition (ON->OFF) genuinely applies through the seam
/// (shell_integration changes, so it is not an early no-change return) yet
/// must never raise the notice.
#[test]
fn on_to_off_reload_raises_no_notice() {
    let _guard = crate::test_lock::render_globals_lock();
    let Some(mut app) = build_idle_app() else {
        return;
    };
    app.settings.shell_integration = true;

    let mut next = app.settings.clone();
    next.shell_integration = false;
    app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

    assert!(
        app.open_notice_message_for_test().is_none(),
        "an ON->OFF toggle must not surface the new-shells notice"
    );
}

/// The reload seam republishes process-global render state (default
/// colors, ANSI palette, minimum-contrast floor) from `Settings`. In a test
/// binary that state is shared with every other test in the process, so the
/// seam must leave nothing behind once it returns: the shipped default
/// floor is 17:1, and leaking it silently changes the colors every later
/// test resolves through the render path.
#[test]
fn reload_seam_leaves_no_residual_render_globals() {
    let _guard = crate::test_lock::render_globals_lock();
    let Some(mut app) = build_idle_app() else {
        return;
    };
    let floor_before = text::min_contrast();
    let colors_before = text::color_globals_for_test();
    assert_ne!(
        app.settings.effective_min_contrast(),
        floor_before,
        "precondition: the seam must publish a floor different from the baseline"
    );

    // Any real change drives the publish; an unchanged reload returns early.
    let mut next = app.settings.clone();
    next.shell_integration = !app.settings.shell_integration;
    app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

    assert_eq!(
        text::min_contrast(),
        floor_before,
        "the seam must not leave its published contrast floor behind"
    );
    assert_eq!(
        text::color_globals_for_test(),
        colors_before,
        "the seam must not leave published theme colors behind"
    );
}

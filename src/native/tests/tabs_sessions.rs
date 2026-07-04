// SPDX-License-Identifier: GPL-3.0-only
//! Headless multi-session foundation tests for the tabs packet.

use crate::core::Color;
use crate::settings::TabRailWidth;

use super::super::pty::UserEvent;
use super::*;

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type SessionFixture = (
    Arc<Mutex<Terminal>>,
    Arc<Mutex<PtySession>>,
    Arc<Mutex<Vec<u8>>>,
);

#[allow(clippy::type_complexity)]
fn recorded_session(
    dims: Dimensions,
) -> Option<(
    Arc<Mutex<Terminal>>,
    PtyWriter,
    Arc<Mutex<PtySession>>,
    Arc<Mutex<Vec<u8>>>,
)> {
    let session = spawn_test_pause_shell(dims).ok()?;
    let _ = session.take_writer().ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    Some((terminal, writer, pty, bytes))
}

fn single_session_app() -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal, writer, pty, bytes) = recorded_session(dims)?;
    let app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, bytes))
}

fn app_with_two_sessions() -> Option<(App, [SessionFixture; 2])> {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal_a, writer_a, pty_a, bytes_a) = recorded_session(dims)?;
    let (terminal_b, writer_b, pty_b, bytes_b) = recorded_session(dims)?;
    let mut app = App::new(
        options,
        terminal_a.clone(),
        writer_a,
        pty_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.push_session_for_test(terminal_b.clone(), writer_b, pty_b.clone());
    Some((
        app,
        [(terminal_a, pty_a, bytes_a), (terminal_b, pty_b, bytes_b)],
    ))
}

fn tab_bar_app() -> Option<App> {
    let (app, _fixtures) = app_with_two_sessions()?;
    Some(app)
}

/// Inject a fresh workspace (a single-tab, recorded-PTY workspace) and switch to
/// it, so the workspace rail has multiple slots and the active workspace is
/// single-tab (no top bar competes with the rail). Returns `false` when no PTY
/// fixture is available (the caller should already have one from `tab_bar_app`).
fn add_workspace(app: &mut App) -> bool {
    let dims = NativeOptions::default().initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        return false;
    };
    app.push_workspace_for_test(terminal, writer, pty);
    true
}

/// WP1 capture: a headless multi-workspace, multi-pane `App` captures into a
/// `ShapeSnapshot` that mirrors names, tab count/order, the split tree, and
/// per-pane cwd — and the captured shape round-trips through the JSON layer.
#[test]
fn capture_shape_records_workspaces_tabs_panes_and_cwd() {
    use crate::native::persistence::{PaneShape, ShapeSnapshot};

    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Seed a known cwd on the first session's terminal (tab 0's sole pane).
    fixtures[0]
        .0
        .lock()
        .expect("terminal")
        .seed_working_directory("/home/tester/project".to_owned());

    // Split the ACTIVE tab (tab 0) so it becomes a two-pane split; tab 1 stays
    // a lone leaf. `push` appends tabs without switching, so tab 0 is active.
    let dims = NativeOptions::default().initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);

    // Add a second workspace (this seam switches to it).
    assert!(add_workspace(&mut app));

    let snapshot = app.capture_shape_for_test();
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.workspaces.len(), 2);
    assert_eq!(
        snapshot.active_workspace, 1,
        "adding a workspace switches to it"
    );

    let ws0 = &snapshot.workspaces[0];
    assert_eq!(ws0.name, "Workspace 1");
    assert_eq!(ws0.tabs.len(), 2);

    // Tab 0 was split: a Columns split whose first (pre-existing) leaf carries
    // the seeded cwd; the freshly spawned pane is the second leaf.
    match &ws0.tabs[0].layout {
        PaneShape::Split {
            axis,
            first,
            second,
            ..
        } => {
            assert_eq!(*axis, crate::native::persistence::SplitAxisShape::Columns);
            assert_eq!(
                **first,
                PaneShape::Leaf {
                    cwd: Some("/home/tester/project".to_owned())
                }
            );
            assert!(matches!(**second, PaneShape::Leaf { .. }));
        }
        other => panic!("tab 0 should be a split, got {other:?}"),
    }
    // Tab 1 stays single-pane.
    assert!(matches!(ws0.tabs[1].layout, PaneShape::Leaf { .. }));

    // Second workspace: one single-pane tab.
    let ws1 = &snapshot.workspaces[1];
    assert_eq!(ws1.name, "Workspace 2");
    assert_eq!(ws1.tabs.len(), 1);
    assert!(matches!(ws1.tabs[0].layout, PaneShape::Leaf { .. }));

    // The captured shape survives a JSON round-trip byte-for-byte.
    let text = snapshot.to_json_pretty();
    let restored = ShapeSnapshot::from_json_str(&text).expect("captured shape round-trips");
    assert_eq!(restored, snapshot);
}

/// Build a fresh workspace backed by a recorded PTY, switch to it, and return
/// its session's terminal handle so a test can drive its bell.
fn add_workspace_with_terminal(app: &mut App) -> Option<Arc<Mutex<Terminal>>> {
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, pty, _bytes) = recorded_session(dims)?;
    app.push_workspace_for_test(terminal.clone(), writer, pty);
    Some(terminal)
}

/// NF21-6: a bell rung in a BACKGROUND tab is drained by the arena-wide
/// maintenance sweep and latches that tab's activity flag — without switching
/// to it and without touching the active tab's flag.
#[test]
fn background_tab_bell_latches_activity_without_switching() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Ring a bell in the background tab (tab 1); tab 0 is active/focused.
    fixtures[1].0.lock().expect("terminal").advance(b"\x07");

    let (focused, background, _prompt) = app.drain_bells_for_test();
    assert!(!focused, "the focused pane did not ring");
    assert!(background, "a background session rang -> urgency path");
    // The background tab latched; the active tab did not. (A latched flag on a
    // non-active tab also proves no switch happened — a switch would clear it.)
    assert!(app.tab_activity_for_test(0, 1));
    assert!(!app.tab_activity_for_test(0, 0));
}

/// NF21-6 / §5 rule 3: a bell rung in a BACKGROUND WORKSPACE latches its tab's
/// activity (and the derived workspace rollup) without waiting for a switch —
/// the surface a bell is most for after workspaces landed.
#[test]
fn background_workspace_bell_latches_activity_without_switching() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Add and switch to a second workspace, so workspace 0 is now background.
    let Some(_ws1_terminal) = add_workspace_with_terminal(&mut app) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Ring a bell in workspace 0, tab 0 (now a background workspace).
    fixtures[0].0.lock().expect("terminal").advance(b"\x07");

    let (focused, background, _prompt) = app.drain_bells_for_test();
    assert!(!focused, "the focused pane is in the active workspace");
    assert!(
        background,
        "a background-workspace bell reaches the urgency path"
    );
    assert!(
        app.tab_activity_for_test(0, 0),
        "background workspace's tab latched"
    );
    assert!(
        app.workspace_activity_for_test(0),
        "derived workspace rollup latched"
    );
    assert!(
        !app.workspace_activity_for_test(1),
        "the active workspace stays clean"
    );
}

/// NF21-6: the active-visible focused pane keeps today's behavior — its bell
/// starts the viewport flash and never latches an activity flag (you saw it).
#[test]
fn focused_pane_bell_flashes_and_does_not_latch_activity() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_bell_visual_for_test();
    // Ring a bell in the active/focused tab (tab 0).
    fixtures[0].0.lock().expect("terminal").advance(b"\x07");

    app.run_about_to_wait_maintenance_for_test(Instant::now());
    assert!(
        app.bell_flash_active_for_test(),
        "focused bell starts the flash"
    );
    assert!(
        !app.tab_activity_for_test(0, 0),
        "the active-visible tab never latches its own activity"
    );
}

/// NF21-6: a bell on the focused pane of a MULTIPANE active tab now drains
/// (previously it stranded — the multipane render path never drained bells).
#[test]
fn multipane_focused_bell_drains_and_does_not_strand() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let dims = NativeOptions::default().initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Split the active tab; focus lands on the new pane (`terminal`).
    app.seed_split_pane_for_test(true, terminal.clone(), writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);

    terminal.lock().expect("terminal").advance(b"\x07");
    let (focused_first, _bg1, _p1) = app.drain_bells_for_test();
    assert!(focused_first, "a multipane focused-pane bell drains");
    let (focused_again, _bg2, _p2) = app.drain_bells_for_test();
    assert!(!focused_again, "the bell drained once and did not strand");
}

/// NF21-6: switching to a flagged tab clears its activity latch — viewing is
/// what clears the rollup signal.
#[test]
fn switching_to_a_flagged_tab_clears_its_activity() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    fixtures[1].0.lock().expect("terminal").advance(b"\x07");
    app.drain_bells_for_test();
    assert!(app.tab_activity_for_test(0, 1), "background tab latched");

    // View tab 1, then run a maintenance drain: the active tab's flag clears.
    assert!(app.switch_to_session_for_test(1));
    app.drain_bells_for_test();
    assert!(
        !app.tab_activity_for_test(0, 1),
        "the now-viewed tab's activity is cleared"
    );
}

fn scrollback_bytes(lines: usize) -> Vec<u8> {
    let mut text = String::new();
    for i in 0..lines {
        text.push_str(&format!("row-{i:02}\r\n"));
    }
    text.into_bytes()
}

#[test]
fn single_pane_overlay_geometry_equals_the_window_grid_and_pointer_cell() {
    // Byte-identity guard for the multi-pane overlay fix: on a single-pane tab
    // (no GPU geometry) the window-overlay grid dims and window-space pointer
    // cell fall back to exactly `self.grid` / `self.pointer_cell`, so the
    // single-pane overlay hit-test path is unchanged. The multi-pane mapping is
    // unit-tested separately in `panes::tests`.
    let Some((mut app, _bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_pointer_cell_for_test(7, 13);
    let ((cols, rows), pointer) = app.overlay_geometry_for_test();
    assert_eq!((cols, rows), app.grid_dims_for_test());
    assert_eq!(pointer, Some(CellPoint { row: 7, column: 13 }));
}

#[test]
fn backgrounded_session_blink_does_not_spin_the_event_loop() {
    // NF20-B regression: `next_wake_deadline` sourced the cursor-blink toggle of
    // EVERY session, but the blink is only consumed for the ACTIVE session (the
    // `Deref` target). So a blinking cursor in tab A, once tab B is activated,
    // left A's toggle deadline in the wake set with nothing to advance it —
    // `WaitUntil(<past>)` returned immediately every iteration and busy-spun a
    // core. The fix narrows the source to the active pane and parks background
    // panes' timers in maintenance. This drives the real switch + maintenance
    // and asserts the STRICT invariant: after maintenance the next wake is None
    // or STRICTLY in the future — never a stale past instant.
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Arm the ACTIVE pane's (session 0) blink: its toggle deadline (t0+530ms)
    // now sits in the wake set. Also arm the other two per-session deadline
    // sources — cursor ease/slide and the synchronized-output hold — so this one
    // flow covers every fan-out source `next_wake_deadline` mins over sessions.
    let t0 = Instant::now();
    app.arm_active_cursor_blink_for_test(t0);
    app.arm_active_cursor_anim_for_test(t0); // ease t0+200ms, slide t0+150ms
    app.arm_active_sync_hold_for_test(t0);
    assert!(
        app.next_wake_deadline_for_test()
            .is_some_and(|d| d <= t0 + Duration::from_millis(530)),
        "an armed active pane schedules a near-future wake"
    );

    // Switch to session 1 — session 0 is now BACKGROUND and never rendered, so
    // its blink toggle is never polled again.
    assert!(app.switch_to_session_for_test(1));

    // Well past session 0's toggle boundary, drive the maintenance pass the loop
    // runs before every wait recompute. It must NOT leave session 0's stale
    // deadline in the wake set.
    let later = t0 + Duration::from_secs(5);
    app.run_about_to_wait_maintenance_for_test(later);
    match app.next_wake_deadline_for_test() {
        None => {}
        Some(next) => assert!(
            next > later,
            "a backgrounded pane's stale blink must not survive as a past wake \
             (was {:?} past now)",
            later.saturating_duration_since(next)
        ),
    }

    // SWITCH-BACK: activate session 0 again. Because it was parked while
    // backgrounded, it starts from a clean (non-stale) blink — so even the frame
    // the pane is switched back on parks cleanly (no past-instant wake).
    assert!(app.switch_to_session_for_test(0));
    let later2 = later + Duration::from_secs(1);
    app.run_about_to_wait_maintenance_for_test(later2);
    match app.next_wake_deadline_for_test() {
        None => {}
        Some(next) => assert!(
            next > later2,
            "a pane switched back to must not carry a stale past wake"
        ),
    }
}

#[test]
fn splitting_a_pane_does_not_spin_from_the_focused_panes_timers() {
    // NF21-1 regression (default-config busy-spin the instant any tab is split).
    // NF20-B narrowed the wake SOURCES to the active pane and parked BACKGROUND
    // panes, both assuming the render path advances the active pane's blink /
    // ease / slide. That holds only single-pane: a multi-pane active tab renders
    // through `rebuild_multipane`, which polls no cursor timer, and
    // `park_background_timers` exempted the FOCUSED pane — so a blinking cursor's
    // past `next_toggle` stranded in the wake set and `WaitUntil(<past>)` spun a
    // core (plus a full multipane rebuild per iteration). The fix parks the
    // focused pane's CURSOR timers too while the active tab is multi-pane (its
    // synchronized-output hold stays live — that deadline is the crash
    // watchdog, released via the render path, modeled here by
    // `resolve_synchronized_output_hold_for_test`). Extends the NF20-B strict
    // invariant to a SPLIT tab arming all three timer types.
    let Some((mut app, _bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _b)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Split the single pane; focus lands on the NEW pane, so the active tab is
    // now multi-pane and the focused pane is exactly the strand NF20-B misses.
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);

    // Arm all three per-session timer sources on the (multi-pane) FOCUSED pane.
    let t0 = Instant::now();
    app.arm_active_cursor_blink_for_test(t0); // toggle t0+530ms
    app.arm_active_cursor_anim_for_test(t0); // ease t0+200ms, slide t0+150ms
    app.arm_active_sync_hold_for_test(t0); // hold t0+150ms
    assert!(
        app.next_wake_deadline_for_test()
            .is_some_and(|d| d <= t0 + Duration::from_millis(530)),
        "an armed focused pane schedules a near-future wake"
    );

    // Well past every boundary, drive the maintenance pass + the render-path
    // sync-hold resolution the loop runs each iteration. The focused pane's
    // cursor timers must be parked (no consumer in multipane) and its hold
    // released — the next wake must be None or STRICTLY in the future.
    let later = t0 + Duration::from_secs(5);
    app.run_about_to_wait_maintenance_for_test(later);
    app.resolve_synchronized_output_hold_for_test(later);
    match app.next_wake_deadline_for_test() {
        None => {}
        Some(next) => assert!(
            next > later,
            "the focused pane of a split tab must not strand a past wake \
             (was {:?} past now)",
            later.saturating_duration_since(next)
        ),
    }
}

#[test]
fn output_into_a_background_split_pane_drives_a_rebuild() {
    // NF21-7 regression (paired with NF21-1; its spin masked this until fixed).
    // `App` has no own `needs_rebuild` — `self.needs_rebuild` Derefs to the
    // FOCUSED pane. PTY output for a non-focused but visible split pane sets THAT
    // pane's flag and requests a redraw, but the frame gate read only the focused
    // pane's flag, so a build streaming into the other half of a split never
    // repainted until the user typed into the focused pane. The gate now ORs the
    // rebuild flag across every visible pane of the active tab.
    let Some((mut app, _bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _b)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // The sole pane's token, captured before the split makes it the background.
    let background = app.active_session_token_for_test();
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Focus moved to the new pane, so `background` is now a visible-but-NOT-
    // focused pane of the split tab.
    assert_ne!(
        background,
        app.active_session_token_for_test(),
        "focus lands on the new pane, leaving the original as the background pane"
    );

    // Settle every visible pane (both start dirty at construction): gate closed.
    app.clear_visible_pane_rebuild_flags_for_test();
    assert!(
        !app.should_rebuild_frame_for_test(),
        "an idle split tab does not request a rebuild"
    );

    // Route real PTY output to the background pane through the production redraw
    // event. It marks THAT pane dirty (not the focused one) and requests a wake.
    let should_exit = app.dispatch_user_event_for_test(UserEvent::Redraw {
        session: background,
    });
    assert!(!should_exit);
    assert_eq!(
        app.pane_needs_rebuild_for_test(background),
        Some(true),
        "output marks the producing (background) pane dirty"
    );
    assert_eq!(
        app.pane_needs_rebuild_for_test(app.active_session_token_for_test()),
        Some(false),
        "the focused pane stays clean — its flag alone would keep the gate shut"
    );
    assert!(
        app.should_rebuild_frame_for_test(),
        "output into a background visible pane must drive a frame rebuild"
    );
}

#[test]
fn input_routes_to_the_active_session_writer_after_switch() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let [(_, _, bytes_a), (_, _, bytes_b)] = fixtures;

    app.drive_text_key_for_test("a");
    assert_eq!(&*bytes_a.lock().expect("bytes a"), b"a");
    assert!(bytes_b.lock().expect("bytes b").is_empty());

    assert!(app.switch_to_session_for_test(1));
    app.drive_text_key_for_test("b");

    assert_eq!(&*bytes_a.lock().expect("bytes a"), b"a");
    assert_eq!(&*bytes_b.lock().expect("bytes b"), b"b");
}

#[test]
fn session_output_and_scrollback_stay_independent() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.advance_session_bytes_for_test(0, b"alpha-session\r\n");
    app.advance_session_bytes_for_test(1, b"beta-session\r\n");

    let plain_a = app.session_plain_text_for_test(0).expect("plain text a");
    let plain_b = app.session_plain_text_for_test(1).expect("plain text b");

    assert!(plain_a.contains("alpha-session"));
    assert!(!plain_a.contains("beta-session"));
    assert!(plain_b.contains("beta-session"));
    assert!(!plain_b.contains("alpha-session"));
}

#[test]
fn resize_updates_both_terminals_and_ptys() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let cell = cell(8, 16);
    assert!(app.resize_grid(cell, 512, 256));
    let expected = Dimensions::new(64, 15);

    assert_eq!(app.session_dimensions_for_test(0), Some(expected));
    assert_eq!(app.session_dimensions_for_test(1), Some(expected));
    #[cfg(unix)]
    {
        assert_eq!(app.session_pty_dimensions_for_test(0), Some(expected));
        assert_eq!(app.session_pty_dimensions_for_test(1), Some(expected));
    }
}

#[test]
fn closing_active_session_activates_neighbor_and_last_close_sets_exit() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    assert!(app.switch_to_session_for_test(1));
    assert_eq!(app.active_session_id_for_test(), 1);

    assert!(!app.close_active_tab_for_test());
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert!(!app.pending_exit_for_test());

    assert!(app.close_active_tab_for_test());
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_window_title_for_test(), "OdyTTY");
    assert!(app.pending_exit_for_test());
}

#[test]
fn shell_exit_for_non_last_session_does_not_exit_app() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let session = app
        .session_token_at_position_for_test(1)
        .expect("session token");
    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });

    assert!(!should_exit);
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert!(!app.pending_exit_for_test());
}

#[test]
fn tab_bar_show_rule_is_hidden_for_one_session_and_visible_for_two() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal.clone(),
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    assert!(!app.tab_bar_visible_for_test());

    let Some((terminal_b, writer_b, pty_b, _bytes_b)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.push_session_for_test(terminal_b, writer_b, pty_b);
    assert!(app.tab_bar_visible_for_test());
}

#[test]
fn tab_bar_shows_for_a_lone_renamed_tab() {
    // F4 ODP-7 / F4-NF1: a single tab normally hides the bar, but once it
    // carries a custom name the bar must show so the named "workflow" tab is
    // visible. (Fails before the show-rule change: a lone tab stayed hidden
    // regardless of its title override.)
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    assert!(
        !app.tab_bar_visible_for_test(),
        "a lone unnamed tab hides the bar"
    );

    app.set_session_title_override_for_test(0, Some("deploy"));
    assert!(
        app.tab_bar_visible_for_test(),
        "a lone renamed tab shows the bar"
    );

    // Clearing the override reverts to hidden.
    app.set_session_title_override_for_test(0, None);
    assert!(
        !app.tab_bar_visible_for_test(),
        "clearing the name hides the bar again"
    );
}

#[test]
fn always_show_tab_bar_setting_reveals_the_bar_for_a_single_tab() {
    // F4 ODP-7: the opt-in `always_show_tab_bar` setting shows the strip even
    // for one unnamed tab, applied live. (Fails before the show-rule change:
    // the setting did not exist / was not consulted.)
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    assert!(!app.tab_bar_visible_for_test(), "default hides the bar");

    let next = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    app.apply_saved_settings_live_for_test(next);
    assert!(
        app.tab_bar_visible_for_test(),
        "always_show_tab_bar reveals the bar for a single tab"
    );

    let off = Settings {
        always_show_tab_bar: false,
        ..Settings::default()
    };
    app.apply_saved_settings_live_for_test(off);
    assert!(
        !app.tab_bar_visible_for_test(),
        "turning the setting off hides the bar again"
    );
}

#[test]
fn tab_bar_reservation_reduces_shell_rows_by_one_when_visible() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal.clone(),
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );

    let cell = cell(8, 16);
    assert!(!app.resize_grid(cell, 640, 384));
    assert_eq!(
        app.session_dimensions_for_test(0),
        Some(Dimensions::new(80, 24))
    );

    let Some((terminal_b, writer_b, pty_b, _bytes_b)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.push_session_for_test(terminal_b, writer_b, pty_b);
    assert!(app.resize_grid(cell, 640, 384));
    assert_eq!(
        app.session_dimensions_for_test(0),
        Some(Dimensions::new(80, 23))
    );
    assert_eq!(
        app.session_dimensions_for_test(1),
        Some(Dimensions::new(80, 23))
    );
}

#[test]
fn tab_bar_hit_test_reports_switch_close_and_new_actions() {
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));

    app.set_pointer_px_for_test(12.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("switch"));

    app.set_pointer_px_for_test(188.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("close"));

    app.set_pointer_px_for_test(628.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new"));
}

// --- F4-V2 R1: vertical tab rail integration ---

#[test]
fn tab_reserve_combines_top_bar_and_workspace_rail() {
    // The top tab bar (tabs) reserves a row off the top; the workspace rail
    // reserves columns off a side. They are independent bands that can coexist.
    // With one workspace and the default auto rail, only the top bar reserves
    // (byte-identical to a top-only bar).
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    // Pin a manual rail width so the reservation is name-length independent.
    app.set_tab_rail_width_manual_for_test(16);

    // Two tabs → top bar shown; one workspace → auto rail hidden. Top only.
    app.set_tab_bar_placement_for_test("top");
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 0),
        "top bar reserves one row, no rail"
    );

    // Force the workspace rail on the LEFT: BOTH bands reserve now.
    app.set_workspace_rail_for_test("left");
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 16),
        "top bar row + left rail band"
    );

    // The RIGHT rail reserves the same column count off the other side.
    app.set_workspace_rail_for_test("right");
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 16),
        "top bar row + right rail band (mirror)"
    );
}

#[test]
fn workspace_rail_grows_decorated_snapshot_by_columns_beside_the_top_bar() {
    // With the workspace rail shown alongside the top tab bar, the single-pane
    // decoration grows the snapshot by the rail's COLUMNS (content shifts right)
    // WITHOUT adding rows — the top bar's row is already present (the rail is a
    // full-height sidebar beside it). The grown axis must match `tab_reserve`.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    // Pin a manual rail width so the grown-columns delta is a fixed 16.
    app.set_tab_rail_width_manual_for_test(16);

    // Two tabs → top bar shown; auto rail hidden (one workspace). Top only.
    let (top_cols, top_rows) = app
        .decorated_snapshot_dims_for_test()
        .expect("top decorated dims");

    // Force the rail on the left: the decoration composes the top bar and the
    // full-height rail, growing columns by the 16-col band, rows unchanged.
    app.set_workspace_rail_for_test("left");
    let (rail_cols, rail_rows) = app
        .decorated_snapshot_dims_for_test()
        .expect("rail decorated dims");

    assert_eq!(
        rail_rows, top_rows,
        "the rail adds columns, not rows (the top bar row stays)"
    );
    assert_eq!(
        rail_cols,
        top_cols + 16,
        "the rail adds its 16-col band beside the content"
    );
}

#[test]
fn left_rail_hit_test_resolves_switch_close_and_new() {
    // Row-major rail hit-test round-trip: the same TabHit dispatch the bar uses,
    // resolved through the rail's X-band + row-stacked slots (F4-V2).
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // F4-P4: pin a manual rail width (16) so the close-× column (rail_cols − 2)
    // is a fixed 14 regardless of the auto-sized width.
    app.set_tab_rail_width_manual_for_test(16);
    // Pin short single-line labels so the F4-P1 floating-`+` anchor (one gap
    // below the last slot's LABEL row) is deterministic regardless of the live
    // shell title's length.
    // W2: two workspaces give the rail two slots; the active workspace holds a
    // single tab so no top bar competes for the corner. Short names keep the
    // slot geometry deterministic (single label row), matching the labels the
    // pre-workspace rail tabs used.
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");

    // R1.1 geometry: a 1-row top margin, then tab 0 at rows [1,3), tab 1 at
    // [4,6). The close × sits inside the ring inset at col 14 (rail_cols 16 −
    // inset 1 − 1).
    //
    // Slot 0 body: row 1, label col → centre (12, 24).
    app.set_pointer_px_for_test(12.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("switch"),
        "slot 0 body → switch"
    );

    // Slot 0 close ×: inset top-right cell (row 1, col 14) → centre (116, 24).
    app.set_pointer_px_for_test(116.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("close"),
        "slot 0 × → close"
    );

    // F4-P1 floating-`+` fix: the `+` anchors one gap below tab 1's single label
    // row (4 + 1 + gap 1 = row 6), NOT a full stride below (old row 7). Centre
    // y = 6*16 + 8 = 104.
    app.set_pointer_px_for_test(64.0, 104.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new"), "+ slot → new");
}

#[test]
fn click_right_of_the_rail_is_not_a_tab_hit() {
    // A click in the content area (past the rail's right edge, x ≥ rail_w=128)
    // is never a tab hit — the X-band gate excludes it.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: a second workspace makes the auto rail appear on the left; the active
    // workspace's single tab means no top bar, so the content area is clear.
    add_workspace(&mut app);

    app.set_pointer_px_for_test(200.0, 8.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "content-area click → no tab hit"
    );
}

// --- F4-P4: rail seam drag → manual width, double-click → auto ---

/// A hermetic temp `odytty.conf` for a persistence test (unique per process +
/// test thread, seeded with a comment to prove non-destructive rewrite).
fn temp_rail_conf(tag: &str) -> std::path::PathBuf {
    let thread = std::thread::current()
        .name()
        .unwrap_or("test")
        .replace(|c: char| !c.is_ascii_alphanumeric(), "_");
    let base = std::env::temp_dir().join(format!("odytty-{tag}-{}-{}", std::process::id(), thread));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("odytty.conf");
    std::fs::write(&path, "# kept\n").unwrap();
    path
}

#[test]
fn rail_seam_drag_sets_and_persists_a_manual_width() {
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    // Pin a manual width so the seam is deterministic: left rail band [0, 128),
    // so the inner seam sits at x = 16*8 = 128 (headless origin at 0).
    app.set_tab_rail_width_manual_for_test(16);
    let conf = temp_rail_conf("seam-drag");
    app.set_config_path_for_test(conf.clone());

    // A press ON the seam arms the drag; no width change yet (fails-before: the
    // pre-P4 rail had no draggable seam).
    app.set_pointer_px_for_test(128.0, 100.0);
    app.mouse_left_press_for_test();
    assert!(
        app.rail_seam_dragging_for_test(),
        "a press on the seam arms a drag"
    );
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(16));

    // Motion sets the manual width the pointer maps to: 80px / 8 = 10 cells.
    app.pointer_move_for_test(80.0, 100.0);
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(10));
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::ColResize,
        "a column-resize cursor is shown during the drag"
    );

    // Release disarms and persists the dragged width to the temp config, non-
    // destructively.
    app.mouse_left_release_for_test();
    assert!(
        !app.rail_seam_dragging_for_test(),
        "release disarms the drag"
    );
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(10));
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(written.contains("# kept"), "existing config preserved");
    assert!(
        written.contains("tab_rail_width = 10"),
        "the dragged manual width persisted; got: {written:?}"
    );
    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}

#[test]
fn double_click_rail_seam_resets_to_auto_and_persists() {
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    // Start from a manual width (seam at x = 20*8 = 160).
    app.set_tab_rail_width_manual_for_test(20);
    let conf = temp_rail_conf("seam-dblclick");
    app.set_config_path_for_test(conf.clone());
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(20));

    // Two quick presses on the seam (a double-click) reset the rail to auto.
    app.set_pointer_px_for_test(160.0, 100.0);
    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();
    app.mouse_left_press_for_test();
    assert_eq!(
        app.tab_rail_width_for_test(),
        TabRailWidth::Auto,
        "double-click the seam resets to auto width"
    );
    assert!(
        !app.rail_seam_dragging_for_test(),
        "the reset does not leave a drag armed"
    );
    let written = std::fs::read_to_string(&conf).unwrap();
    assert!(
        written.contains("tab_rail_width = auto"),
        "the auto reset persisted; got: {written:?}"
    );
    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}

#[test]
fn rail_seam_hover_shows_a_resize_cursor_off_the_tab_slots() {
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // seam at x = 128

    // Hovering the seam grab band shows a column-resize cursor.
    app.pointer_move_for_test(128.0, 100.0);
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::ColResize,
        "the seam grab band shows a resize cursor"
    );
    // Hovering a tab slot (well inside the band) does not.
    app.pointer_move_for_test(20.0, 24.0);
    assert_ne!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::ColResize,
        "a tab slot is not a resize target"
    );
}

// --- F4-P3: rail auto-hide (ODP-4) ---

#[test]
fn autohide_removes_the_rail_reservation() {
    // The load-bearing rule: enabling auto-hide drops the rail's content
    // reservation to zero (one reflow at toggle time) — reveal is a pure
    // overlay, never a reflow. Pinned left reserves the band + gap; auto-hidden
    // reserves nothing.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    // W2: a second workspace makes the auto rail appear (left side, inherited
    // from placement); the active workspace has a single tab, so no top bar is
    // reserved and the reservation is the rail band alone.
    add_workspace(&mut app);
    assert_eq!(
        app.tab_reserve_for_test(),
        (0, 16),
        "pinned left rail reserves its band"
    );

    app.set_tab_rail_autohide_for_test(true);
    assert!(app.rail_autohide_active_for_test());
    assert_eq!(
        app.tab_reserve_for_test(),
        (0, 0),
        "auto-hide removes the reservation entirely (content is full-width)"
    );
    // The overlay band still resolves its width (for the floating strip),
    // independent of the now-zero reservation.
    assert_eq!(app.rail_overlay_cols_for_test(), 16);

    // Toggling back off restores the reservation (the reverse reflow).
    app.set_tab_rail_autohide_for_test(false);
    assert_eq!(app.tab_reserve_for_test(), (0, 16));
}

#[test]
fn autohide_only_applies_to_side_rails_not_the_top_bar() {
    // Auto-hide is a rail feature; the top bar keeps `always_show_tab_bar`
    // semantics, so the knob is inert (and the reservation unchanged) on top.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("top");
    app.set_tab_rail_autohide_for_test(true);
    assert!(
        !app.rail_autohide_active_for_test(),
        "auto-hide never applies to the top bar"
    );
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 0),
        "the top bar still reserves its row under the (ignored) autohide knob"
    );
}

#[test]
fn reveal_edge_zone_is_the_window_edge_and_band_extends_to_the_seam() {
    // The trigger zone is within `tab_rail_reveal_px` of the window edge; the
    // keep-alive band runs from the edge to the seam (⊇ the trigger zone).
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128] (pad 0 headless)
    app.set_tab_rail_autohide_for_test(true);

    // Default reveal_px = 16 (logical) at scale 1.0: x=2 is in the edge zone;
    // x=40 (deep in the band) is not the trigger but IS the keep-alive band;
    // x=200 (past the seam) is neither.
    assert_eq!(app.reveal_contact_for_test(2.0), Some((true, true)));
    assert_eq!(
        app.reveal_contact_for_test(40.0),
        Some((false, true)),
        "mid-band holds the reveal but does not re-trigger"
    );
    assert_eq!(
        app.reveal_contact_for_test(200.0),
        Some((false, false)),
        "past the seam is off the rail entirely"
    );
}

#[test]
fn reveal_zone_is_logical_px_scaled_for_hidpi() {
    // REGRESSION (trigger zone too thin on fractional/HiDPI): the reveal zone is
    // a LOGICAL-px width, scaled to the physical-px space winit reports pointer
    // coordinates in. At scale 2.0 the default 16 logical px must trigger out to
    // physical x=32 — with the old physical-px zone it would have stopped at x=8
    // and the rail would be unreachable on a scaled display.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);
    app.set_test_scale_for_test(2.0);

    // Physical x=30 is within 16 logical px (32 physical) of the edge → triggers.
    assert_eq!(
        app.reveal_contact_for_test(30.0).map(|(edge, _)| edge),
        Some(true),
        "16 logical px at 2x scale reaches physical x=30"
    );
    // Physical x=40 is beyond the scaled zone → no trigger (still in the band).
    assert_eq!(
        app.reveal_contact_for_test(40.0).map(|(edge, _)| edge),
        Some(false),
        "past 32 physical px the edge no longer triggers"
    );
}

#[test]
fn reveal_wiring_reaches_interior_and_holds_at_scale_1_5_with_padding() {
    // NF20-B live-wiring regression: the operator reported (scale ~1.5, real
    // window padding) that the reveal only triggered at the very window edge and
    // would not stay up with the pointer over the band. This exercises the FULL
    // live path — `update_rail_autohide_pointer` (real contact geometry) + the
    // machine — with an injected clock at scale 1.5 with real padding, so the
    // interior-reach + keep-alive behavior is pinned at true device values, not
    // just the pad-0 / scale-1 headless case the earlier tests covered.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(12, 24)); // ~8x16 logical at 1.5x
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band_w = 16*12 = 192; seam at pad+192
    app.set_tab_rail_autohide_for_test(true);
    app.set_test_scale_for_test(1.5);
    let pad = WindowPadding::from_logical(8.0, 1.5); // 12 physical px
    app.set_test_surface_for_test(1000, 800, pad);
    // reach = pad(12) + reveal_px(16)*scale(1.5)=24 → 36. seam_x = 12 + 192 = 204.

    let t0 = std::time::Instant::now();

    // (1) INTERIOR REACH: a pointer 8px INTO the visible content (x=20, past the
    // 12px padding margin) must arm the reveal — not require the bare edge.
    app.feed_rail_pointer_for_test(20.0, t0);
    // Not visible yet (show debounce), but a poll past the debounce reveals it.
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "interior contact arms the debounce, not an instant reveal"
    );
    let revealed_at = t0 + std::time::Duration::from_millis(130); // past the show debounce
    app.feed_rail_pointer_for_test(20.0, revealed_at);
    assert!(
        app.rail_autohide_is_visible_for_test(revealed_at),
        "reveal triggers from an INTERIOR position (x=20, well inside the window)"
    );

    // (2) KEEP-ALIVE OVER THE BAND: moving deeper onto the drawn band (x=100,
    // mid-band) must HOLD the reveal — not drop it.
    let hold_at = revealed_at + std::time::Duration::from_millis(50);
    app.feed_rail_pointer_for_test(100.0, hold_at);
    assert!(
        app.rail_autohide_is_visible_for_test(hold_at),
        "pointer over the drawn band holds the reveal (keep-alive union)"
    );
    // A stationary pointer over the band across a maintenance poll stays up.
    let dwell = hold_at + std::time::Duration::from_millis(700); // past a hide grace
    assert!(
        !app.poll_rail_autohide_for_test(dwell),
        "no visibility change"
    );
    assert!(
        app.rail_autohide_is_visible_for_test(dwell),
        "a pointer parked over the band never times out"
    );

    // (3) HIDE ON LEAVE: past the seam (x=250) starts the grace; it hides only
    // after the grace elapses (not instantly).
    let leave_at = dwell + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(250.0, leave_at);
    assert!(
        app.rail_autohide_is_visible_for_test(leave_at),
        "leaving the band starts the hide grace — still visible through it"
    );
    let hidden_at = leave_at + std::time::Duration::from_millis(700); // > 600ms grace
    app.poll_rail_autohide_for_test(hidden_at);
    assert!(
        !app.rail_autohide_is_visible_for_test(hidden_at),
        "after the grace, the rail hides"
    );
}

#[test]
fn reveal_visibility_flip_marks_the_frame_for_rebuild() {
    // Live-trace regression (the operator's "the state says visible at +27px but
    // I don't see the rail until I cross off the window edge"). The reveal state
    // machine was already correct — the trace showed `visible=true` on-window —
    // but the paint never landed. Root cause: the rail overlay is assembled ONLY
    // inside the `should_rebuild_frame` gate (`build_rail_overlay`), and that gate
    // reads `needs_rebuild`. The rail-reveal paths requested a redraw WITHOUT
    // marking the frame dirty, so `RedrawRequested` skipped the rebuild and
    // re-presented the previous, rail-less frame. Over a quiescent terminal
    // (nothing else setting `needs_rebuild`) the reveal only painted when an
    // unrelated event — the pointer crossing off-window — happened to dirty a
    // frame. Both the maintenance-poll flip and the pointer-driven flip must now
    // set `needs_rebuild`.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    assert!(
        app.rail_autohide_active_for_test(),
        "precondition: a left rail with autohide on is active"
    );

    let t0 = std::time::Instant::now();

    // (A) MAINTENANCE-POLL FLIP — the operator's exact scenario. Arm at the edge,
    // then STOP moving the pointer; the show-debounce elapses while the loop is
    // parked, so the flip to Revealed happens in the about-to-wait maintenance
    // poll, not in a pointer event. Clear the rebuild flag right after arming so
    // the only thing that can re-set it is the reveal flip itself.
    app.feed_rail_pointer_for_test(8.0, t0); // edge → arms Revealing (not visible)
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "edge contact arms the debounce, not an instant reveal"
    );
    app.clear_needs_rebuild_for_test();
    let revealed_at = t0 + std::time::Duration::from_millis(130); // past show debounce
    app.run_about_to_wait_maintenance_for_test(revealed_at);
    assert!(
        app.rail_autohide_is_visible_for_test(revealed_at),
        "the maintenance poll crosses the debounce and reveals the rail"
    );
    assert!(
        app.needs_rebuild_for_test(),
        "the reveal flip in the maintenance poll must mark the frame for rebuild — \
         a bare redraw request is dropped by the `should_rebuild_frame` gate and \
         the overlay never paints (the live-trace bug)"
    );

    // (B) HIDE FLIP — leaving the band starts the grace; when the grace elapses in
    // the maintenance poll the rail must be REMOVED from the frame, which likewise
    // needs a rebuild.
    let leave_at = revealed_at + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(400.0, leave_at); // off the band → hide grace
    app.clear_needs_rebuild_for_test();
    let hidden_at = leave_at + std::time::Duration::from_millis(700); // > 600ms grace
    app.run_about_to_wait_maintenance_for_test(hidden_at);
    assert!(
        !app.rail_autohide_is_visible_for_test(hidden_at),
        "after the grace the rail hides"
    );
    assert!(
        app.needs_rebuild_for_test(),
        "the hide flip must also rebuild so the overlay is dropped from the frame"
    );

    // (C) POINTER-DRIVEN FLIP — when the debounce elapses on a pointer sample
    // (the pointer is still moving over the band as it reveals), that path must
    // mark the frame dirty too.
    let t1 = hidden_at + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(8.0, t1); // re-arm at the edge
    app.clear_needs_rebuild_for_test();
    let t2 = t1 + std::time::Duration::from_millis(130); // past the debounce, on a sample
    app.feed_rail_pointer_for_test(50.0, t2); // still over the band → reveals here
    assert!(
        app.rail_autohide_is_visible_for_test(t2),
        "the reveal completes on the pointer sample past the debounce"
    );
    assert!(
        app.needs_rebuild_for_test(),
        "the pointer-driven reveal flip must mark the frame for rebuild"
    );
}

#[test]
fn reveal_arms_at_the_edge_and_the_band_carries_the_debounce() {
    // A brief edge touch during a fast approach must still reveal: the edge
    // contact ARMS the debounce, and the keep-alive band then CARRIES it to
    // completion even as the pointer moves off the thin trigger zone into the
    // band (the pointer does not have to sit pinned in the ≤reveal_px strip for
    // the whole debounce). Drives the real feed path with an injected clock.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    // pad 0, scale 1 → reach = 16 (default reveal_px). Zone is [0, 16].
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);

    let t0 = std::time::Instant::now();
    // Touch the edge once (x=8) → arms Revealing (not visible yet).
    app.feed_rail_pointer_for_test(8.0, t0);
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "edge contact arms the debounce, not an instant reveal"
    );
    // Move off the trigger zone but stay over the band (x=50 < seam 128). The
    // band keep-alive holds the arm; past the show debounce it reveals.
    let t1 = t0 + std::time::Duration::from_millis(90);
    app.feed_rail_pointer_for_test(50.0, t1);
    assert!(
        app.rail_autohide_is_visible_for_test(t1),
        "the band carries the armed reveal through the debounce — no need to \
         sit pinned in the thin edge strip"
    );
}

#[test]
fn reveal_arms_and_holds_across_the_live_trace_sequences() {
    // Round-5 regression, built from the operator's live ODYTTY_RAIL_TRACE. Two
    // sequences the trace exposed as broken, replayed through the real feed path:
    //   (1) a fast approach that overshoots OFF the window edge (samples jump
    //       30–200px and hop over a static point zone) must still arm+reveal;
    //   (2) a fast in-then-past-the-seam FOLLOW-THROUGH within the confirm window
    //       (the trace's 7.9px→214px in 74ms) must NOT abort the armed reveal —
    //       the motion-aware trigger keeps `in_edge` set so the confirm completes.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    // pad 0, scale 1 → reach = 16 (default reveal_px). Trigger zone [0, 16].
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);

    let t0 = std::time::Instant::now();

    // (1) FAST APPROACH OVERSHOOTING OFF THE WINDOW. A sample at x=200 lands in
    // content (no arm); the next sample jumps clean over [0,16] to x=−8 (past the
    // left edge, where a tiling compositor clamps). The segment 200→−8 sweeps the
    // trigger band → arm.
    app.feed_rail_pointer_for_test(200.0, t0);
    app.feed_rail_pointer_for_test(-8.0, t0);
    assert!(
        !app.rail_autohide_is_visible_for_test(t0),
        "the overshoot arms the confirm, not an instant reveal"
    );
    let armed = t0 + std::time::Duration::from_millis(40); // past the 30ms confirm
    app.feed_rail_pointer_for_test(-8.0, armed);
    assert!(
        app.rail_autohide_is_visible_for_test(armed),
        "a fast approach that overshoots the edge reveals (not only a precise \
         landing inside the thin zone)"
    );

    // Settle back to hidden so the follow-through starts from a clean Hidden.
    let leave = armed + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(400.0, leave); // off the band → hide grace
    let hidden = leave + std::time::Duration::from_millis(700); // > 600ms grace
    app.poll_rail_autohide_for_test(hidden);
    assert!(
        !app.rail_autohide_is_visible_for_test(hidden),
        "precondition: back to Hidden before the follow-through sequence"
    );

    // (2) FAST FOLLOW-THROUGH PAST THE SEAM. A deliberate quick approach lands at
    // the edge (x=8, arms), then ONE fast sample overshoots past the seam
    // (x=214) inside the confirm window. The segment 8→214 still sweeps the
    // trigger band, so `in_edge` stays set and the confirm is NOT aborted — the
    // reveal completes. (Pre-fix, the out-of-band sample aborted the arm and the
    // rail "wouldn't open" on a fast approach.)
    let t2 = hidden + std::time::Duration::from_millis(10);
    app.feed_rail_pointer_for_test(8.0, t2);
    assert!(
        !app.rail_autohide_is_visible_for_test(t2),
        "edge contact arms the confirm"
    );
    let follow = t2 + std::time::Duration::from_millis(10); // within the 30ms confirm
    app.feed_rail_pointer_for_test(214.0, follow);
    let done = t2 + std::time::Duration::from_millis(40); // past the confirm
    app.poll_rail_autohide_for_test(done);
    assert!(
        app.rail_autohide_is_visible_for_test(done),
        "a fast follow-through past the seam does not abort the armed reveal"
    );
}

#[test]
fn reveal_yields_to_an_active_scrollbar_drag() {
    // Coexistence (ODP-5): while a scroll-thumb drag is in progress, a pointer
    // even at the very edge reports no reveal contact — the drag wins the edge.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    // Without a drag, the edge triggers reveal.
    assert_eq!(app.reveal_contact_for_test(2.0), Some((true, true)));
    // With a scroll-thumb drag in progress, the same edge yields (no contact).
    app.begin_scrollbar_drag_for_test();
    assert_eq!(
        app.reveal_contact_for_test(2.0),
        Some((false, false)),
        "a scrollbar drag suppresses reveal at the edge"
    );
}

#[test]
fn autohide_disables_the_seam_resize_leaving_reveal_the_only_edge_gesture() {
    // Seam-drag vs reveal-zone PRECEDENCE (documented): under auto-hide the rail
    // is a floating overlay that is not seam-resized, so the F4-P4 seam grab band
    // is inert — the reveal trigger is the only edge interaction, and the two
    // never conflict. A press at the (pinned) seam x therefore starts no drag.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16); // pinned seam would be at x=128
    app.set_tab_rail_autohide_for_test(true);

    assert_eq!(
        app.pointer_over_rail_seam_for_test(128.0),
        Some(false),
        "the seam grab band is inert under auto-hide"
    );
    app.set_pointer_px_for_test(128.0, 100.0);
    app.mouse_left_press_for_test();
    assert!(
        !app.rail_seam_dragging_for_test(),
        "no seam drag arms under auto-hide"
    );
}

#[test]
fn revealed_band_click_hits_the_rail_hidden_does_not() {
    // While revealed, a press on the overlay band resolves to a rail TabHit (the
    // floating rail owns the pointer); while hidden, the same press falls through
    // (no chrome), so content clicks are never eaten by an invisible rail.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    // W2: two workspaces give the rail two slots; the active workspace holds a
    // single tab so no top bar competes for the corner. Short names keep the
    // slot geometry deterministic (single label row), matching the labels the
    // pre-workspace rail tabs used.
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    app.set_tab_rail_autohide_for_test(true);

    // Slot 0 body (top-margin row 1, label col 2 → centre (2·8+4, 24)).
    app.set_pointer_px_for_test(2.0 * 8.0 + 4.0, 24.0);
    // Hidden: no tab hit (falls through to content).
    assert!(!app.rail_overlay_visible_for_test());
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "a hidden auto-hide rail eats no clicks"
    );
    // Revealed: the same press is a rail switch hit.
    app.force_rail_reveal_for_test();
    assert!(app.rail_overlay_visible_for_test());
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("switch"),
        "a revealed overlay band press hits the rail"
    );
}

// --- F4-P3 REGRESSION repros (auto-hide breaks mouse routing) ---

#[test]
fn repro_right_click_in_content_opens_menu_under_autohide() {
    // REGRESSION: with auto-hide on and the rail hidden, a right-click in the
    // content area must open the context menu exactly as with auto-hide off.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    // Pointer deep in content, far from the reveal edge; rail hidden.
    app.pointer_move_for_test(400.0, 200.0);
    assert!(
        !app.rail_overlay_visible_for_test(),
        "rail hidden in content"
    );
    app.mouse_right_press_for_test();
    assert!(
        app.context_menu_open_for_test(),
        "right-click in content must open the context menu under auto-hide"
    );
}

#[test]
fn repro_reveal_holds_across_maintenance_polls_no_flicker() {
    // REGRESSION (flicker): once revealed with the pointer parked over the band,
    // the maintenance poll must keep it revealed — it must not oscillate.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let t0 = std::time::Instant::now();
    // Pointer at the edge → arm the reveal, then poll comfortably past the show
    // debounce (SHOW_DEBOUNCE is 80ms; +150ms clears it with margin).
    app.pointer_move_for_test(2.0, 100.0);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(150));
    assert!(
        app.rail_overlay_visible_for_test(),
        "revealed after debounce"
    );

    // Pointer now sits mid-band (x=40, past the edge zone, inside the band).
    app.pointer_move_for_test(40.0, 100.0);
    // Several maintenance polls with the pointer parked must all stay revealed.
    for ms in [200u64, 400, 800, 1600, 3200] {
        app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(ms));
        assert!(
            app.rail_overlay_visible_for_test(),
            "still revealed at +{ms}ms mid-band (no flicker)"
        );
    }
}

#[test]
fn repro_revealed_content_right_click_opens_menu_not_swallowed() {
    // REGRESSION: while the rail is REVEALED (the flicker state), a right-click
    // in CONTENT (past the band seam) must still open the content context menu —
    // the floating overlay must not swallow content-area clicks.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16); // band [0..128], seam at x=128
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();
    assert!(app.rail_overlay_visible_for_test());

    // Pointer far into content, well past the seam.
    app.pointer_move_for_test(400.0, 200.0);
    app.mouse_right_press_for_test();
    assert!(
        app.context_menu_open_for_test(),
        "content right-click must open the menu even while the rail is revealed"
    );
}

#[test]
fn repro_open_menu_suppresses_the_rail_overlay_no_occlusion() {
    // REGRESSION ("can't right-click Settings"): the revealed rail strip is
    // composited topmost — OVER any open window overlay. So while a context menu
    // (or Settings / palette) is open, the rail must NOT be drawn, or it paints
    // over the menu and hides the items the pointer is clicking. An open overlay
    // owns the screen; the floating rail steps aside until it closes.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    // Rail revealed with no overlay → drawn.
    app.force_rail_reveal_for_test();
    assert!(
        app.rail_overlay_visible_for_test(),
        "revealed rail is drawn when nothing overlays it"
    );

    // Open a context menu (right-click in content) → the rail must step aside so
    // it does not paint over the menu.
    app.pointer_move_for_test(400.0, 200.0);
    app.mouse_right_press_for_test();
    assert!(app.context_menu_open_for_test(), "menu is open");
    assert!(
        !app.rail_overlay_visible_for_test(),
        "an open overlay suppresses the rail so it can't occlude the menu"
    );
}

// --- F4-P3 REGRESSION: phantom top-bar leak + idle self-wake (NF20) ---

#[test]
fn autohide_left_decoration_grows_no_phantom_top_bar() {
    // REGRESSION (top-bar leak): with auto-hide on and placement=left, the
    // single-pane decoration must NOT grow a top bar. The dispatch keys off
    // `rail_side()`, which reads the (deliberately zeroed) auto-hide reservation
    // and reports `None`; without the auto-hide guard an auto-hidden LEFT rail
    // fell through to the top-bar branch and grew a one-row bar across the top
    // (decorated rows = raw rows + 1). The rail draws only as a floating overlay,
    // so the decorated snapshot must equal the raw snapshot — no rows off the
    // top, no columns off the side — in BOTH the hidden and revealed phases.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // W2: a single-tab active workspace means no top bar, so the autohidden
    // rail (a floating overlay) is the ONLY chrome — the decoration must add
    // nothing. A second workspace keeps the rail shown.
    add_workspace(&mut app);
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let raw = app.raw_snapshot_dims_for_test().expect("raw dims");

    // Hidden: no chrome decorated at all.
    assert!(!app.rail_overlay_visible_for_test());
    assert_eq!(
        app.decorated_snapshot_dims_for_test(),
        Some(raw),
        "auto-hidden left rail must not grow a phantom top bar (hidden)"
    );

    // Revealed: still no pinned decoration — the reveal is a separate floating
    // overlay, not a snapshot grow.
    app.force_rail_reveal_for_test();
    assert!(app.rail_overlay_visible_for_test());
    assert_eq!(
        app.decorated_snapshot_dims_for_test(),
        Some(raw),
        "a revealed auto-hide rail must not grow a phantom top bar (revealed)"
    );
}

#[test]
fn autohide_right_decoration_grows_no_phantom_top_bar() {
    // Mirror of the left case for placement=right — the same `rail_side()`
    // predicate leaked a top bar on a right-placed auto-hidden rail too.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("right");
    // W2: a single-tab active workspace means no top bar, so the autohidden
    // rail (a floating overlay) is the ONLY chrome — the decoration must add
    // nothing. A second workspace keeps the rail shown.
    add_workspace(&mut app);
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let raw = app.raw_snapshot_dims_for_test().expect("raw dims");
    assert_eq!(
        app.decorated_snapshot_dims_for_test(),
        Some(raw),
        "auto-hidden right rail must not grow a phantom top bar"
    );
}

#[test]
fn autohide_top_placement_still_decorates_its_bar() {
    // Guard the fix's blast radius: the auto-hide early-return must NOT fire on
    // the top bar (auto-hide never applies there), so a top placement still
    // grows its one-row bar exactly as before.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("top");
    app.set_tab_rail_autohide_for_test(true); // inert on top
    assert!(!app.rail_autohide_active_for_test());

    let raw = app.raw_snapshot_dims_for_test().expect("raw dims");
    let decorated = app
        .decorated_snapshot_dims_for_test()
        .expect("decorated dims");
    assert_eq!(decorated.0, raw.0, "top bar grows rows, not columns");
    assert_eq!(
        decorated.1,
        raw.1 + 1,
        "the top bar still reserves + decorates its one row"
    );
}

#[test]
fn autohide_idle_states_schedule_no_self_wake() {
    // REGRESSION (NF20 CPU spin): an idle auto-hidden rail must not schedule a
    // wake — neither steady Hidden nor Revealed-with-the-pointer-parked. A past
    // or immediate wake here would spin the event loop (`WaitUntil(now)` → wake →
    // poll → no change → repeat) at frame rate, pegging a core. Both idle phases
    // must add nothing to `next_wake_deadline`.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.set_tab_bar_placement_for_test("left");
    // W2: force the workspace rail on so its reserve/seam/reveal machinery
    // is exercised (the rail now lists workspaces).
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    app.set_tab_rail_autohide_for_test(true);

    let t0 = std::time::Instant::now();

    // Idle Hidden (pointer parked deep in content): no wake.
    app.pointer_move_for_test(400.0, 200.0);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(50));
    assert!(!app.rail_overlay_visible_for_test(), "hidden at rest");
    assert!(
        !app.rail_autohide_wants_wake_for_test(t0 + std::time::Duration::from_millis(60)),
        "an idle hidden rail schedules no self-wake"
    );

    // Reveal by parking the pointer at the edge, poll past the debounce, then
    // confirm the settled Revealed state also schedules no wake.
    app.pointer_move_for_test(2.0, 200.0);
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(200));
    assert!(
        app.rail_overlay_visible_for_test(),
        "revealed after debounce"
    );
    // Several settled polls with the pointer parked at the edge: still no wake.
    for ms in [400u64, 800, 1600, 3200] {
        let now = t0 + std::time::Duration::from_millis(ms);
        app.run_about_to_wait_maintenance_for_test(now);
        assert!(
            app.rail_overlay_visible_for_test(),
            "still revealed at +{ms}ms (parked at edge)"
        );
        assert!(
            !app.rail_autohide_wants_wake_for_test(now),
            "an idle revealed rail schedules no self-wake at +{ms}ms"
        );
    }
}

// --- F4-P2: right rail (mirror of the left rail) ---

#[test]
fn right_rail_hit_test_is_x_flipped_to_the_far_side() {
    // The right rail is the mirror of the left: its X-band sits at the FAR side
    // (after the content columns), so a click on the rail band there is a tab
    // hit while a click in the LEFT content area is not (the flip of
    // `click_right_of_the_rail_is_not_a_tab_hit`).
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("right");
    // W2: two workspaces give the rail two slots; the active workspace holds a
    // single tab so no top bar competes for the corner. Short names keep the
    // slot geometry deterministic (single label row), matching the labels the
    // pre-workspace rail tabs used.
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");

    // Headless: no live resize, so `self.grid` is the full grid and the right
    // rail band starts at column `cols` (content stays at column 0; pad/gap 0).
    // Slot 0 body: top-margin row 1, label col 2 → centre ((cols+2)·8+4, 24).
    let (cols, _rows) = app.grid_dims_for_test();
    let rail_x0 = cols as f64 * 8.0;
    app.set_pointer_px_for_test(rail_x0 + 2.0 * 8.0 + 4.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        Some("switch"),
        "right rail slot 0 body → switch"
    );

    // A click in the LEFT content area (well before the rail band) is not a tab
    // hit — the X-band gate excludes it, mirroring the left rail's exclusion of
    // the right content.
    app.set_pointer_px_for_test(8.0, 24.0);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "left content click → no tab hit (mirror)"
    );
}

#[test]
fn right_and_left_rails_grow_the_decorated_snapshot_identically() {
    // Both rails grow the single-pane decorated snapshot by `rail_cols` columns
    // (never rows); only the side the content sits on differs. The dims are
    // therefore identical — the mirror is a placement, not a size, difference.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_session_title_override_for_test(0, Some("aa"));

    app.set_tab_bar_placement_for_test("left");
    let left = app
        .decorated_snapshot_dims_for_test()
        .expect("left decorated dims");
    app.set_tab_bar_placement_for_test("right");
    let right = app
        .decorated_snapshot_dims_for_test()
        .expect("right decorated dims");
    assert_eq!(
        left, right,
        "left and right decorations differ only by side, not size"
    );
}

#[test]
fn right_rail_leaves_the_content_origin_unshifted_for_overlays() {
    // A right rail reserves columns off the RIGHT, so the content origin is
    // unmoved: IME-candidate / click-hint / overlay quads anchored in content
    // space need no shift. This is the asymmetry vs the left rail (which shifts
    // content right by the reserved band).
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));

    app.set_tab_bar_placement_for_test("right");
    // W2: a second workspace makes the auto rail appear; the active workspace's
    // single tab means no top bar, so the only offset is the rail's (0 for a
    // right rail, positive for a left rail).
    add_workspace(&mut app);
    assert_eq!(
        app.tab_chrome_offset_px_for_test(),
        Some((0.0, 0.0)),
        "right rail: content origin unmoved"
    );

    // Guard that the (0, 0) above is a real right-rail property, not a constant:
    // the left rail DOES shift the content origin right by the reserved band.
    app.set_tab_bar_placement_for_test("left");
    let (dx, dy) = app
        .tab_chrome_offset_px_for_test()
        .expect("left chrome offset");
    assert!(dx > 0.0, "left rail shifts content right");
    assert_eq!(dy, 0.0, "a rail never shifts content down");
}

#[test]
fn right_rail_scrollbar_stays_in_the_content_not_under_the_rail() {
    // MANDATORY collision test (ODP-5): with a right rail, the scroll thumb hugs
    // the content grid's right edge, which sits at/left of the rail band's left
    // edge — never under it. So a press resolves to exactly one target: the thumb
    // grabs the scrollbar (and is not a tab hit), and the rail band is a tab
    // action (and never grabs the scrollbar).
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Scrollback on the active (first) session so the thumb is visible.
    {
        let mut terminal = fixtures[0].0.lock().expect("terminal");
        terminal.advance(&scrollback_bytes(200));
    }
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_bar_placement_for_test("right");
    // W2: force the workspace rail on the right so the collision geometry is
    // exercised (the rail lists workspaces now).
    app.set_workspace_rail_for_test("right");
    app.scroll_up_for_test(usize::MAX);
    let len = app.scrollback_len_for_test();
    if len == 0 {
        eprintln!("skipping: no scrollback materialized");
        return;
    }
    let (cols, rows) = app.grid_dims_for_test();
    let offset = app.viewport_offset_for_test();
    let thumb = scroll_indicator_quad(
        offset,
        len,
        Dimensions::new(cols, rows),
        cell(8, 16),
        [1.0, 1.0, 1.0, 0.62],
    )
    .expect("thumb visible while scrolled back");

    // Press on the thumb → scrollbar grab, and NOT a tab hit (it is left of the
    // rail band).
    let tx = ((thumb.rect[0] + thumb.rect[2]) / 2.0) as f64;
    let ty = ((thumb.rect[1] + thumb.rect[3]) / 2.0) as f64;
    app.set_pointer_px_for_test(tx, ty);
    assert_eq!(
        app.tab_bar_hit_for_test(),
        None,
        "the thumb is not under the rail"
    );
    assert_eq!(
        app.left_button_outcome_for_test(true),
        "grab",
        "thumb press grabs the scrollbar"
    );
    app.left_button_outcome_for_test(false); // release the grab

    // Press on the rail band → a tab action, never a scrollbar grab.
    let rail_x = cols as f64 * 8.0 + 2.0 * 8.0 + 4.0;
    app.set_pointer_px_for_test(rail_x, 24.0);
    assert!(
        app.tab_bar_hit_for_test().is_some(),
        "the rail band is a tab hit"
    );
    assert_ne!(
        app.left_button_outcome_for_test(true),
        "grab",
        "a rail press must not grab the scrollbar"
    );
}

#[test]
fn visible_tab_bar_row_has_no_default_background_cells() {
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));

    let backgrounds = app
        .tab_bar_row_backgrounds_for_test()
        .expect("tab bar backgrounds");
    assert!(!backgrounds.is_empty());
    assert!(backgrounds.iter().all(|bg| *bg != Color::Default));
}

#[test]
fn title_override_controls_effective_tab_label_and_clear_restores_osc_title() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.set_session_tab_title_for_test(0, "osc-title");
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("osc-title")
    );

    app.set_session_title_override_for_test(0, Some("work"));
    assert_eq!(app.session_tab_title_for_test(0).as_deref(), Some("work"));

    app.set_session_tab_title_for_test(0, "shell-updated");
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("work"),
        "shell titles stop overriding a custom tab name"
    );

    app.set_session_title_override_for_test(0, None);
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shell-updated")
    );
}

#[test]
fn rename_modal_commits_edits_cancels_and_empty_commit_clears_override() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let [(_, _, bytes_a), _] = fixtures;

    app.set_session_tab_title_for_test(0, "shell");
    assert!(app.begin_rename_tab_for_test(0));
    app.drive_text_key_for_test("!");
    assert_eq!(
        &*bytes_a.lock().expect("bytes a"),
        b"",
        "rename modal captures typed text before the PTY path"
    );
    app.drive_named_key_for_test(NamedKey::ArrowLeft);
    app.drive_named_key_for_test(NamedKey::Backspace);
    app.drive_text_key_for_test("work");
    app.drive_named_key_for_test(NamedKey::Enter);
    assert!(!app.rename_active_for_test());
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shelwork!")
    );

    assert!(app.begin_rename_tab_for_test(0));
    app.drive_text_key_for_test(" discarded");
    app.drive_named_key_for_test(NamedKey::Escape);
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shelwork!"),
        "Esc cancels without changing the committed override"
    );

    assert!(app.begin_rename_tab_for_test(0));
    for _ in 0.."shelwork!".chars().count() {
        app.drive_named_key_for_test(NamedKey::Backspace);
    }
    app.drive_named_key_for_test(NamedKey::Enter);
    assert_eq!(
        app.session_tab_title_for_test(0).as_deref(),
        Some("shell"),
        "empty commit clears the override and restores the OSC title"
    );
}

/// Replicate the rename modal's editable-input origin from the grid dims — the
/// same math `rename_layout` uses, so a click column maps to a known character.
fn rename_input_origin(cols: usize, rows: usize) -> (usize, usize) {
    let width = cols.clamp(8, 48);
    let left = (cols - width) / 2;
    let top = (rows - 3) / 2;
    let input_row = top + 1;
    let input_left = left + 2 + "Tab name: ".chars().count();
    (input_row, input_left)
}

#[test]
fn rename_field_click_places_caret_and_drag_selects() {
    // F4-RENAME-MOUSE: a click positions the caret, a drag extends a selection,
    // and typing replaces the selected span.
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let (cols, rows) = app.grid_dims_for_test();
    let (input_row, input_left) = rename_input_origin(cols, rows);

    app.set_session_title_override_for_test(0, Some("hello world"));
    assert!(app.begin_rename_tab_for_test(0));
    assert_eq!(
        app.rename_cursor_for_test(),
        Some(11),
        "caret starts at end"
    );

    // Click on character index 2 ('l' of "hello").
    app.rename_pointer_press_for_test(input_row, input_left + 2);
    assert_eq!(
        app.rename_cursor_for_test(),
        Some(2),
        "click places the caret"
    );
    assert_eq!(
        app.rename_selection_for_test(),
        None,
        "a plain click makes no selection"
    );

    // Drag to character index 5 → selection [2, 5) = "llo".
    app.rename_pointer_drag_for_test(input_row, input_left + 5);
    assert_eq!(app.rename_selection_for_test(), Some((2, 5)));
    app.rename_pointer_release_for_test();
    assert_eq!(
        app.rename_selection_for_test(),
        Some((2, 5)),
        "release keeps the finalized selection"
    );

    // Typing replaces the selected span.
    app.drive_text_key_for_test("X");
    assert_eq!(app.rename_text_for_test().as_deref(), Some("heX world"));
    assert_eq!(app.rename_selection_for_test(), None, "insertion clears it");
}

#[test]
fn rename_field_double_click_selects_word_and_backspace_replaces_it() {
    // F4-RENAME-MOUSE: a double-click selects the word under the pointer, and a
    // subsequent Backspace deletes the whole selection.
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let (cols, rows) = app.grid_dims_for_test();
    let (input_row, input_left) = rename_input_origin(cols, rows);

    app.set_session_title_override_for_test(0, Some("hello world"));
    assert!(app.begin_rename_tab_for_test(0));

    // Two quick presses on the same cell (char 7, inside "world") → word select.
    app.rename_pointer_press_for_test(input_row, input_left + 7);
    app.rename_pointer_press_for_test(input_row, input_left + 7);
    assert_eq!(
        app.rename_selection_for_test(),
        Some((6, 11)),
        "double-click selects the whole word"
    );

    app.drive_named_key_for_test(NamedKey::Backspace);
    assert_eq!(
        app.rename_text_for_test().as_deref(),
        Some("hello "),
        "Backspace replaces the selected word"
    );
    assert_eq!(app.rename_selection_for_test(), None);
}

#[test]
fn shell_exit_for_last_session_requests_app_exit() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );

    let session = app
        .session_token_at_position_for_test(0)
        .expect("session token");
    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session });

    assert!(should_exit);
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_window_title_for_test(), "OdyTTY");
    assert!(app.pending_exit_for_test());
}

#[test]
fn switching_sessions_restores_per_session_title_viewport_selection_and_search() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.advance_session_bytes_for_test(0, b"\x1b]2;session-alpha\x07");
    app.advance_session_bytes_for_test(1, b"\x1b]2;session-beta\x07");
    app.advance_session_bytes_for_test(0, &scrollback_bytes(40));
    app.scroll_up_for_test(3);
    app.open_search_for_test();
    app.force_selection_for_test(0, 0, 0, 4);

    assert_eq!(app.active_window_title_for_test(), "session-alpha");
    assert!(app.viewport_offset_for_test() > 0);
    assert!(app.selection_range_for_test().is_some());
    assert!(app.search_open_for_test());

    assert!(app.switch_to_session_for_test(1));
    assert_eq!(app.active_window_title_for_test(), "session-beta");
    assert_eq!(app.viewport_offset_for_test(), 0);
    assert_eq!(app.selection_range_for_test(), None);
    assert!(!app.search_open_for_test());

    assert!(app.switch_to_session_for_test(0));
    assert_eq!(app.active_window_title_for_test(), "session-alpha");
    assert!(app.viewport_offset_for_test() > 0);
    assert!(app.selection_range_for_test().is_some());
    assert!(app.search_open_for_test());
}

#[test]
fn redraw_for_non_active_session_only_marks_that_session_dirty() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.set_session_needs_rebuild_for_test(0, false);
    app.set_session_needs_rebuild_for_test(1, false);

    let session = app
        .session_token_at_position_for_test(1)
        .expect("session token");
    let should_exit = app.dispatch_user_event_for_test(UserEvent::Redraw { session });

    assert!(!should_exit);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert_eq!(app.session_needs_rebuild_for_test(0), Some(false));
    assert_eq!(app.session_needs_rebuild_for_test(1), Some(true));
}

#[test]
fn stale_shell_exit_for_closed_first_tab_is_a_no_op() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let removed = app
        .session_token_at_position_for_test(0)
        .expect("removed token");

    assert!(!app.close_active_tab_for_test());
    assert_eq!(app.session_count_for_test(), 1);

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session: removed });

    assert!(!should_exit);
    assert_eq!(app.session_count_for_test(), 1);
    assert_eq!(app.active_session_id_for_test(), 0);
    assert!(!app.pending_exit_for_test());
}

#[test]
fn closing_middle_tab_preserves_remaining_session_tokens_and_active_session() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_c, writer_c, pty_c, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    let mut app = App::new(
        options,
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.push_session_for_test(terminal_b, writer_b, pty_b);
    app.push_session_for_test(terminal_c, writer_c, pty_c);

    let first = app
        .session_token_at_position_for_test(0)
        .expect("first token");
    let middle = app
        .session_token_at_position_for_test(1)
        .expect("middle token");
    let last = app
        .session_token_at_position_for_test(2)
        .expect("last token");

    assert!(app.switch_to_session_for_test(2));
    assert_eq!(app.active_session_token_for_test(), last);

    let should_exit = app.dispatch_user_event_for_test(UserEvent::ShellExited { session: middle });

    assert!(!should_exit);
    assert_eq!(app.session_count_for_test(), 2);
    assert_eq!(app.session_token_at_position_for_test(0), Some(first));
    assert_eq!(app.session_token_at_position_for_test(1), Some(last));
    assert_eq!(app.active_session_token_for_test(), last);
}

#[test]
fn token_position_round_trip_holds_after_switches() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    assert!(app.switch_to_session_for_test(1));
    let first = app
        .session_token_at_position_for_test(0)
        .expect("first token");
    let second = app
        .session_token_at_position_for_test(1)
        .expect("second token");

    assert_ne!(first, second);
    assert_eq!(app.session_token_at_position_for_test(0), Some(first));
    assert_eq!(app.session_token_at_position_for_test(1), Some(second));
}

#[test]
fn close_all_sessions_empties_and_terminates() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.close_all_sessions_for_test();

    assert_eq!(app.session_count_for_test(), 0);
}

// ----- focused-pane overlays in the multi-pane render path (1c-3c) -----

/// Count the cells that differ between two equal-shaped snapshots (the cells a
/// paint mutated). Theme-agnostic: selection/search change cell attrs whether
/// the style is themed (background) or unthemed (inverse).
fn changed_cells(before: &Snapshot, after: &Snapshot) -> usize {
    before
        .cells
        .iter()
        .zip(after.cells.iter())
        .filter(|(a, b)| a != b)
        .count()
}

#[test]
fn focused_pane_overlay_maps_selection_with_the_pane_grid() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Wrapped selection on the focused session: absolute row 0, columns 0..=70.
    app.set_selection_range_for_test(0, 0, 0, 70);
    let metrics = cell(10, 20);

    // Paint onto a 40-column pane snapshot: the far selection column (70) clamps
    // to the pane width, highlighting all 40 cells of row 0.
    let pane_grid = Dimensions::new(40, 4);
    let mut pane_snap = snapshot(&["", "", "", ""], 40);
    let pane_before = pane_snap.clone();
    app.paint_focused_pane_overlays(&mut pane_snap, pane_grid, 0, 0, metrics);
    let changed_pane = changed_cells(&pane_before, &pane_snap);

    // Paint the SAME selection onto an 80-column snapshot: row 0 columns 0..=70
    // highlight (71 cells). The difference proves the paint keys to the PANE
    // grid argument, not the whole-window `self.grid`.
    let wide_grid = Dimensions::new(80, 4);
    let mut wide_snap = snapshot(&["", "", "", ""], 80);
    let wide_before = wide_snap.clone();
    app.paint_focused_pane_overlays(&mut wide_snap, wide_grid, 0, 0, metrics);
    let changed_wide = changed_cells(&wide_before, &wide_snap);

    assert_eq!(changed_pane, 40, "row 0 clamps to the 40-col pane width");
    assert_eq!(changed_wide, 71, "row 0 columns 0..=70 in the 80-col grid");
    assert_ne!(
        changed_pane, changed_wide,
        "the pane grid, not the window grid, drives the highlight mapping"
    );
}

#[test]
fn focused_pane_overlay_paints_focused_pane_search_matches() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Searchable content in the focused session, then a query that matches it.
    app.advance_session_bytes_for_test(0, b"needle\r\n");
    app.drive_search_for_test("needle");
    // Require a real match so the assertion is meaningful (skip if the PTY-backed
    // terminal did not register the write in this environment).
    if app.search_match_count_for_test() == 0 {
        eprintln!("skipping: no search match registered");
        return;
    }

    let metrics = cell(10, 20);
    let grid = Dimensions::new(40, 4);
    let scrollback_len = app.scrollback_len_for_test();
    let mut snap = snapshot(&["needle", "", "", ""], 40);
    let before = snap.clone();
    app.paint_focused_pane_overlays(&mut snap, grid, 0, scrollback_len, metrics);

    // The "needle" match (row 0, 6 columns) highlights the focused pane snapshot.
    assert!(
        changed_cells(&before, &snap) > 0,
        "the focused pane's search match must highlight its own snapshot"
    );
}

// ----- §7 K2: prefix engine wired into the App key path -----

#[test]
fn bare_pane_chord_without_prefix_is_plain_input() {
    let Some((mut app, bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // `%` (Shift+%) with no pending prefix is ordinary text — the engine returns
    // Inactive and the byte reaches the PTY exactly as before §7.
    app.drive_char_with_mods_for_test('%', false, true);
    assert_eq!(&*bytes.lock().expect("bytes"), b"%");
    // It did not split.
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn single_pane_passes_ctrl_b_through_to_pty() {
    let Some((mut app, bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.active_pane_count_for_test(), 1);
    // Byte-identity guarantee: on a single-pane tab the prefix engine is gated
    // out, so Ctrl-b reaches the PTY as the literal 0x02 (readline
    // backward-char) exactly like the pre-§7 path. The prefix only engages once
    // the tab is split — see `split_tab_engages_prefix_capture`.
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(&*bytes.lock().expect("bytes"), &[0x02]);
    // It did not enter prefix-pending and did not split.
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn split_tab_engages_prefix_capture() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Split so the active tab now holds two panes; focus lands on the new pane.
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Now Ctrl-b IS the multiplexer prefix: it enters pending and forwards
    // nothing to the focused pane's PTY (tmux semantics, unchanged from before
    // the single-pane gate).
    app.drive_char_with_mods_for_test('b', true, false);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "multi-pane Ctrl-b is captured by the prefix engine, not sent to the PTY"
    );
    // The pending prefix resolves a pane action (Ctrl-b o cycles focus),
    // proving capture is live on the split tab and still forwards nothing.
    app.drive_char_with_mods_for_test('o', false, false);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "the resolved pane chord is not forwarded to the PTY either"
    );
}

#[test]
fn closing_a_split_back_to_single_pane_restores_ctrl_b_passthrough() {
    let Some((mut app, fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // bytes_a is the original (pane A) PTY; focus returns to it after the new
    // pane is closed.
    let [(_, _, bytes_a), _] = fixtures;
    let Some((terminal, writer, pty, _new_bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Ctrl-b x closes the focused (new) pane, collapsing back to a single pane;
    // focus returns to pane A. The close handler cancels any pending prefix as
    // the tab returns to the plain single-pane input path.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('x', false, false);
    assert_eq!(app.active_pane_count_for_test(), 1);
    bytes_a.lock().expect("bytes a").clear();
    // The very next Ctrl-b is plain input again: with one pane the engine is
    // gated out, so it passes through to pane A's PTY as the literal 0x02. This
    // proves no stale pending prefix survived the drop to single-pane.
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(
        &*bytes_a.lock().expect("bytes a"),
        &[0x02],
        "after collapsing to one pane, Ctrl-b passes through byte-identically"
    );
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn prefix_then_unknown_key_fires_nothing() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // The prefix engine only engages on a split tab, so seed two panes first.
    let Some((terminal, writer, pty, bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    app.drive_char_with_mods_for_test('b', true, false); // prefix
    app.drive_char_with_mods_for_test('q', false, false); // not in the table
    // The unknown second key is swallowed (cancel), so nothing reaches the PTY
    // and no pane op ran (still two panes).
    assert!(bytes.lock().expect("bytes").is_empty());
    assert_eq!(app.active_pane_count_for_test(), 2);
}

#[test]
fn prefix_focus_next_cycles_panes() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Seed the active tab into a two-pane column split; focus lands on the new
    // (right) pane.
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let new_pane = app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    assert_eq!(app.focused_pane_id_for_test(), new_pane);
    // Ctrl-b o cycles focus to the other pane in tree order.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_ne!(
        app.focused_pane_id_for_test(),
        new_pane,
        "Ctrl-b o moved focus off the new pane"
    );
}

#[test]
fn prefix_close_pane_collapses_the_split() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(false, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Ctrl-b x closes the focused pane, collapsing the split back to one pane.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('x', false, false);
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn prefix_equalize_keeps_the_split_intact() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    // Ctrl-b = equalizes split ratios; the dispatch runs without disturbing the
    // pane structure.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('=', false, false);
    assert_eq!(app.active_pane_count_for_test(), 2);
}

#[test]
fn prefix_zoom_toggles_full_bleed_pane() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal, writer, pty, _)) = recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    assert!(!app.active_is_zoomed_for_test());
    // Ctrl-b z zooms the focused pane full-bleed; the layout tree is preserved
    // (still two panes), only the render/resize collapses to the focused one.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('z', false, false);
    assert!(app.active_is_zoomed_for_test(), "Ctrl-b z zoomed the pane");
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "zoom preserves the layout tree"
    );
    // Ctrl-b z again un-zooms.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('z', false, false);
    assert!(!app.active_is_zoomed_for_test(), "Ctrl-b z un-zoomed");
}

#[test]
fn prefix_zoom_is_a_noop_on_a_single_pane() {
    let Some((mut app, _bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.active_pane_count_for_test(), 1);
    // Ctrl-b z on a lone pane never zooms: the prefix engine is gated out on a
    // single pane, so both keys are plain input (Ctrl-b → 0x02, z → 'z') and the
    // single-pane render path is untouched. Zoom stays off.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('z', false, false);
    assert!(!app.active_is_zoomed_for_test());
    assert_eq!(app.active_pane_count_for_test(), 1);
}

#[test]
fn prefix_is_disabled_when_set_off() {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let Some((terminal, writer, pty, bytes)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let settings = Settings {
        pane_prefix: None, // prefix model off
        ..Settings::default()
    };
    let mut app = App::new(
        options,
        terminal,
        writer,
        pty,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    // With the prefix off, Ctrl-b is ordinary input again: it sends the literal
    // 0x02, byte-identical to the pre-§7 build.
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(&*bytes.lock().expect("bytes"), &[0x02]);
}

#[test]
fn doubled_prefix_passes_the_literal_byte_to_the_pty() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // The nested-multiplexer passthrough only applies once the prefix engine is
    // engaged, i.e. on a split tab. Seed two panes first.
    let Some((terminal, writer, pty, bytes)) =
        recorded_session(NativeOptions::default().initial_grid)
    else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.seed_split_pane_for_test(true, terminal, writer, pty);
    assert_eq!(app.active_pane_count_for_test(), 2);
    // Ctrl-b Ctrl-b → the focused pane's PTY receives a literal 0x02 (K3), so a
    // tmux running inside OdyTTY still gets its own prefix. The first Ctrl-b
    // enters pending and sends nothing; the second triggers the passthrough.
    app.drive_char_with_mods_for_test('b', true, false);
    assert!(
        bytes.lock().expect("bytes").is_empty(),
        "first prefix sends nothing"
    );
    app.drive_char_with_mods_for_test('b', true, false);
    assert_eq!(&*bytes.lock().expect("bytes"), &[0x02]);
}

#[test]
fn wheel_scrolls_the_pane_under_the_cursor_not_the_focused_pane() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    {
        let mut terminal = terminal_b.lock().expect("terminal b");
        terminal.advance(&scrollback_bytes(80));
    }
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_a = app.focused_pane_id_for_test();
    let pane_b = app.seed_split_pane_for_test(true, terminal_b, writer_b, pty_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    assert_eq!(app.focused_pane_id_for_test(), pane_b);

    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_a,
        "precondition: focus is pane A while pointer is over pane B"
    );
    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_a),
        Some(0),
        "pane A starts at the live tail"
    );
    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_b),
        Some(0),
        "pane B starts at the live tail"
    );

    app.set_pointer_px_for_test(
        (COLS as u32 * CW * 3 / 4) as f64,
        (ROWS as u32 * CH / 2) as f64,
    );
    app.dispatch_wheel_for_test(1.0);

    assert_eq!(
        app.viewport_offset_for_token_for_test(pane_a),
        Some(0),
        "focused pane A must not scroll when the pointer is over pane B"
    );
    assert!(
        app.viewport_offset_for_token_for_test(pane_b)
            .unwrap_or_default()
            > 0,
        "wheel over pane B should scroll pane B's scrollback"
    );
}

#[test]
fn wheel_scroll_still_scrolls_the_lone_single_pane() {
    let Some((mut app, _bytes)) = single_session_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    if let Ok(mut terminal) = app.terminal.lock() {
        terminal.advance(&scrollback_bytes(80));
    }
    assert_eq!(app.viewport_offset_for_test(), 0, "starts at the live tail");

    app.dispatch_wheel_for_test(1.0);

    assert!(
        app.viewport_offset_for_test() > 0,
        "single-pane wheel routing should still scroll the active viewport"
    );
}

/// C11: focus-follows-click into a split must anchor the selection under the
/// LIVE click, not the newly-focused pane's stale per-session `pointer_px`.
///
/// `pointer_px`/`pointer_cell` are per-session; before the fix the press
/// switched focus to pane B and THEN read `self.pointer_px`, which now derefed
/// to B's own last-stored (stale) coordinate — anchoring the first drag at the
/// wrong cell. Seed B with a stale coord at its top-left (cell 0,0), focus A,
/// click deep inside B, and assert the anchor lands under the click (row 10),
/// not B's stale (0,0).
#[test]
fn focus_follows_click_anchors_under_the_live_click_not_stale_coords() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_a = app.focused_pane_id_for_test();
    let pane_b = app.seed_split_pane_for_test(true, terminal_b, writer_b, pty_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    // The split leaves focus on pane B; seed B's stale pointer at its top-left.
    assert_eq!(app.focused_pane_id_for_test(), pane_b);
    app.set_pointer_px_for_test(0.0, 0.0);

    // Move focus back to pane A (Ctrl-b o), so B is unfocused and holds its
    // stale coordinate while A is the active session.
    app.drive_char_with_mods_for_test('b', true, false);
    app.drive_char_with_mods_for_test('o', false, false);
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_a,
        "precondition: focus is pane A"
    );

    // Live click deep inside pane B (right 3/4 width, row 10). This is the
    // active session's (A's) live pointer, the coords the press resolves with.
    app.set_pointer_px_for_test((COLS as u32 * CW * 3 / 4) as f64, (10 * CH) as f64);
    let outcome = app.left_button_outcome_for_test(true);

    assert_eq!(
        outcome, "select",
        "an in-pane left press begins a selection"
    );
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_b,
        "the click focuses pane B (focus-follows-click)"
    );
    let anchor = app
        .pointer_cell_for_test()
        .expect("the press seeds a selection anchor");
    assert_eq!(
        anchor.row, 10,
        "anchor row must follow the live click (row 10), not stale (0)"
    );
    assert!(
        anchor.column > 0,
        "anchor column must follow the live click deep in pane B, not stale column 0"
    );
}

// Fill every row of a terminal with a long bare URL so the hover scan finds an
// openable URL under ANY cell the pointer (or a buggy clamp) maps to — making
// the assertions independent of exact split-pane cell geometry.
fn fill_grid_with_url(terminal: &Arc<Mutex<Terminal>>, rows: usize) {
    let url = format!("https://example.com/{}", "a".repeat(120));
    let mut t = terminal.lock().expect("terminal");
    // Autowrap off so the over-long URL fills each row to its last column without
    // wrapping into the next row (and without scrolling the grid on the last
    // row); the trailing chars harmlessly overwrite the final cell.
    t.advance(b"\x1b[?7l");
    for row in 1..=rows {
        t.advance(format!("\x1b[{row};1H").as_bytes());
        t.advance(url.as_bytes());
    }
}

#[test]
fn hover_over_a_non_focused_pane_does_not_resolve_a_link_in_the_focused_pane() {
    // Hover analog of focus-follows-click. After a column split focus is on the
    // RIGHT pane (B); moving the pointer over the LEFT pane (A) must NOT map the
    // pointer into the focused pane's grid and light a false bare-URL hit (+ hand
    // cursor) from B. Before the fix `active_pane_pointer_cell` mapped the over-A
    // pointer relative to B's rect — x_px − rect.x goes negative and clamps to
    // B's column 0 — so a link at B's left edge latched while hovering pane A.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_b = app.seed_split_pane_for_test(true, terminal_b.clone(), writer_b, pty_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();

    // The split focuses the new pane B (right half); leave focus there.
    assert_eq!(
        app.focused_pane_id_for_test(),
        pane_b,
        "precondition: focus is pane B (right) while the pointer will be over pane A (left)"
    );
    // Fill the focused pane (B) with links so the buggy clamp-to-column-0 would
    // resolve one; the fix must suppress hover over the non-focused pane instead.
    fill_grid_with_url(&terminal_b, ROWS);

    // Pointer over pane A (left quarter) at row 12.
    app.pointer_move_for_test((COLS as u32 * CW / 4) as f64, (ROWS as u32 * CH / 2) as f64);

    assert_eq!(
        app.hovered_url_for_test(),
        None,
        "hovering a non-focused pane must not resolve a link clamped into the focused pane"
    );
    assert_ne!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Pointer,
        "no hand cursor while hovering a non-focused pane"
    );
}

#[test]
fn hover_over_the_focused_pane_in_a_split_still_resolves_a_link() {
    // The suppression must be precise: hovering the FOCUSED pane in a split
    // resolves links exactly as a single pane does (Rule D — not a blanket
    // multi-pane hover kill).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    let pane_b = app.seed_split_pane_for_test(true, terminal_b.clone(), writer_b, pty_b);
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    // The split focuses the new pane B; leave focus there and hover over it.
    assert_eq!(app.focused_pane_id_for_test(), pane_b);
    fill_grid_with_url(&terminal_b, ROWS);

    app.pointer_move_for_test(
        (COLS as u32 * CW * 3 / 4) as f64,
        (ROWS as u32 * CH / 2) as f64,
    );

    assert!(
        app.hovered_url_for_test().is_some(),
        "hovering the focused pane in a split must still resolve its link"
    );
}

#[test]
fn single_pane_hover_still_resolves_a_link() {
    // Rule D: the lone single-pane hover path is byte-identical — the
    // non-focused-pane suppression never engages (`multipane_geometry` is None).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal, writer, pty, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    fill_grid_with_url(&terminal, ROWS);
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );

    // Pointer over the URL at row 12 (column 20 is well within the 40-cell span).
    app.pointer_move_for_test(f64::from(CW) * 20.5, (ROWS as u32 * CH / 2) as f64);

    assert!(
        app.hovered_url_for_test().is_some(),
        "single-pane hover must still resolve a bare URL under the pointer"
    );
}

/// Build a two-pane split `App` headlessly at an exact `COLS×ROWS`-cell surface
/// with zero padding and a single tab (no tab bar), then reflow so each pane
/// holds its narrow split sub-grid. The active (focused) pane is the new one
/// `split_active_with` focuses, so closing it collapses the tab back onto the
/// original survivor. Returns `None` when no PTY is available (CI sandboxes).
fn split_app_at_surface(columns: bool, cols: usize, rows: usize, cw: u32, ch: u32) -> Option<App> {
    let dims = Dimensions::new(cols, rows);
    let (t1, w1, p1, _) = recorded_session(dims)?;
    let mut app = App::new(
        NativeOptions::default(),
        t1,
        w1,
        p1,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    // The headless cell/surface seams are per-session (they Deref to the active
    // session — production resolves these from the shared GPU instead). Set them
    // while the original pane is focused so the *survivor* of the close carries
    // them, then again after the split so the new (focused) pane has them too;
    // otherwise the post-close reflow can't resolve the content rect headlessly.
    let surf_w = cols as u32 * cw;
    let surf_h = rows as u32 * ch;
    app.set_test_cell_for_test(cell(cw, ch));
    app.set_test_surface_for_test(surf_w, surf_h, crate::native::WindowPadding::ZERO);
    let (t2, w2, p2, _) = recorded_session(dims)?;
    app.seed_split_pane_for_test(columns, t2, w2, p2);
    app.set_test_cell_for_test(cell(cw, ch));
    app.set_test_surface_for_test(surf_w, surf_h, crate::native::WindowPadding::ZERO);
    // Reflow the split so each pane is sized to its (narrow) sub-rect — the
    // pre-close state where the survivor is clipped to half-width.
    app.reflow_active_panes_for_test();
    Some(app)
}

#[test]
fn closing_a_column_split_reflows_survivor_to_full_width() {
    // Regression: closing one half of a column split must reflow the surviving
    // pane back to the FULL content width. Before the fix, `multipane_geometry()`
    // returned `None` once single-pane and the reflow was skipped entirely, so
    // the survivor kept its narrow split sub-grid (wrapping + selection clipped).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let Some(mut app) = split_app_at_surface(true, COLS, ROWS, CW, CH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Precondition: the focused split pane is narrower than the full width.
    let (split_cols, _) = app.active_session_grid_dims_for_test();
    assert!(
        split_cols < COLS,
        "split pane should be narrower than full width, got {split_cols}"
    );
    // Close the focused pane → the tab collapses onto the survivor.
    app.close_focused_pane_for_test();
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "tab collapsed to one pane"
    );
    // The survivor must now span the full content width (and full height — a
    // column split never changed the row count, so this also guards no regress).
    let (cols, rows) = app.active_session_grid_dims_for_test();
    assert_eq!(
        (cols, rows),
        (COLS, ROWS),
        "survivor must reflow to the full content grid after the split collapses"
    );
}

#[test]
fn closing_a_row_split_reflows_survivor_to_full_height() {
    // Analogous guard on a row split: the survivor must reflow back to the full
    // content height (the axis a row split had clipped).
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let Some(mut app) = split_app_at_surface(false, COLS, ROWS, CW, CH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let (_, split_rows) = app.active_session_grid_dims_for_test();
    assert!(
        split_rows < ROWS,
        "split pane should be shorter than full height, got {split_rows}"
    );
    app.close_focused_pane_for_test();
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "tab collapsed to one pane"
    );
    let (cols, rows) = app.active_session_grid_dims_for_test();
    assert_eq!(
        (cols, rows),
        (COLS, ROWS),
        "survivor must reflow to the full content grid after the split collapses"
    );
}

/// Drag-select must work inside a focused pane AFTER a split, through the exact
/// production pointer routing (`update_pointer_cell` + `handle_mouse_input`).
///
/// Regression: in a multi-pane tab the left-press handler grabbed the press for
/// divider-hit-test / focus-follows-click and then `return`ed BEFORE reaching
/// `begin_selection`, so a drag never anchored a selection. A second-order bug
/// compounded it: the pointer-cell mapping used the WINDOW origin while
/// `self.grid`/`self.selection` operate on the focused pane's offset sub-rect,
/// so even once the press fired the anchor/extent columns collapsed onto the
/// same clamped edge.
///
/// The gesture here is HORIZONTAL-ONLY (same y, two different x inside the
/// focused right pane), so a non-empty selection range proves BOTH layers: the
/// press must reach `begin_selection` AND the columns must map pane-relative
/// (window-origin mapping would clamp both x past the pane's right edge to the
/// same column → an empty range).
#[test]
fn drag_select_works_inside_a_pane_after_split() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let Some(mut app) = split_app_at_surface(true, COLS, ROWS, CW, CH) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "precondition: two-pane column split"
    );
    // Content rect is the full 640x384 surface (no padding, single tab → no tab
    // bar). An even column split puts the focused (new) pane on the RIGHT:
    // PaneRect(x=320, w=320) → cols [320,640) in window px.
    // Press at window px (480, 100): pane-relative col = (480-320)/8 = 20.
    // Move  to window px (560, 100): pane-relative col = (560-320)/8 = 30.
    // Same row, distinct columns ⇒ a non-empty selection iff both layers work.
    app.pointer_move_for_test(480.0, 100.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.pointer_move_for_test(560.0, 100.0);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);

    let range = app
        .selection_range_for_test()
        .expect("a horizontal drag inside the focused pane must set a selection range");
    let (start_row, start_col, end_row, end_col) = range;
    assert_eq!(start_row, end_row, "horizontal drag stays on one row");
    assert_ne!(
        start_col, end_col,
        "anchor and focus must map to DISTINCT pane-relative columns"
    );
    // Pane-relative, not window-absolute: the focused pane is 40 cols wide, so
    // both columns sit well inside it (window-absolute would be ~60/70).
    assert!(
        end_col < COLS / 2,
        "columns must be pane-relative (< pane width), got {end_col}"
    );
}

/// Single-pane guard: the same horizontal drag gesture still sets a selection on
/// an unsplit tab. The multi-pane geometry path is `None` here, so this is the
/// byte-identical historical mapping and must stay GREEN with or without the
/// per-pane fix.
#[test]
fn drag_select_still_works_single_pane() {
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal, writer, pty, _fixtures)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "precondition: single pane"
    );

    app.pointer_move_for_test(160.0, 100.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.pointer_move_for_test(240.0, 100.0);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);

    let range = app
        .selection_range_for_test()
        .expect("single-pane horizontal drag must still set a selection range");
    let (start_row, start_col, end_row, end_col) = range;
    assert_eq!(start_row, end_row);
    assert_ne!(start_col, end_col);
}

#[test]
fn divider_drag_coalesces_the_pty_resize_to_one_flush_at_release() {
    // Coalescing guard (Phase H): a divider drag fires one pointer-move event
    // per pixel. Routing each through the full pane resize flooded the shell with
    // one kernel resize (`ResizePseudoConsole`/`TIOCSWINSZ`) per move — on
    // Windows ConPTY that scrambles PSReadLine's prompt as it repaints
    // mid-resize. The fix reflows the on-screen grid LIVE per move but defers the
    // single kernel resize to drag-end. This test crosses many cell boundaries
    // during the drag and asserts ZERO kernel resizes fire until the release.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal_a, writer_a, pty_a, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let Some((terminal_b, writer_b, pty_b, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal_a.clone(),
        writer_a,
        pty_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.seed_split_pane_for_test(true, terminal_b.clone(), writer_b, pty_b.clone());
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    assert_eq!(
        app.active_pane_count_for_test(),
        2,
        "precondition: two-pane column split"
    );

    // Helper: kernel-resize call count for both panes' PTYs.
    let count = |a: &Arc<Mutex<PtySession>>, b: &Arc<Mutex<PtySession>>| -> (usize, usize) {
        (
            a.lock().expect("pty a").resize_call_count(),
            b.lock().expect("pty b").resize_call_count(),
        )
    };
    // Pane A's column count, to prove the model reflows LIVE during the drag.
    let a_cols = || {
        terminal_a
            .lock()
            .expect("term a")
            .screen()
            .dimensions()
            .columns
    };

    // Snapshot the baseline AFTER setup (split + reflow already issued kernel
    // resizes); the assertions below measure the delta caused by the drag.
    let (a0, b0) = count(&pty_a, &pty_b);
    let a_cols_before = a_cols();

    // Grab the vertical divider at the content midpoint (x = 640/2 = 320).
    const MID_X: f64 = (COLS as f64 * CW as f64) / 2.0; // 320.0
    const MID_Y: f64 = (ROWS as f64 * CH as f64) / 2.0; // 192.0
    app.set_pointer_px_for_test(MID_X, MID_Y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);

    // Drag the divider LEFT across nine whole-cell boundaries (8 px each). Each
    // step crosses a cell edge so the grid model reflows live every move.
    for step in 1..=9 {
        let x = MID_X - f64::from(step) * f64::from(CW);
        app.pointer_move_for_test(x, MID_Y);
    }

    // RED pre-fix: ~2 kernel resizes per move (one per pane) accumulate here.
    // GREEN post-fix: zero kernel resizes during the whole drag.
    let (a1, b1) = count(&pty_a, &pty_b);
    assert_eq!(
        (a1 - a0, b1 - b0),
        (0, 0),
        "no kernel PTY resize may fire during a divider drag (was {} / {} per pane)",
        a1 - a0,
        b1 - b0
    );
    // ...but the on-screen grid MUST have reflowed live (the visual is not
    // frozen): dragging the divider left narrows pane A.
    assert!(
        a_cols() < a_cols_before,
        "the grid model must reflow live during the drag (pane A cols {} -> {})",
        a_cols_before,
        a_cols()
    );

    // Release flushes exactly one coalesced kernel resize per pane.
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    let (a2, b2) = count(&pty_a, &pty_b);
    assert_eq!(
        (a2 - a1, b2 - b1),
        (1, 1),
        "drag-end must flush exactly one kernel resize per pane"
    );
}

#[test]
fn single_pane_has_no_divider_drag_resize_path() {
    // Rule D byte-identity guard: a single-pane tab has no divider, so a left
    // press + drag + release never enters the divider-drag coalescing path and
    // issues no extra kernel resize from it.
    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CW: u32 = 8;
    const CH: u32 = 16;
    let dims = Dimensions::new(COLS, ROWS);
    let Some((terminal, writer, pty, _)) = recorded_session(dims) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_test_cell_for_test(cell(CW, CH));
    app.set_test_surface_for_test(
        COLS as u32 * CW,
        ROWS as u32 * CH,
        crate::native::WindowPadding::ZERO,
    );
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "precondition: single pane"
    );

    let base = pty.lock().expect("pty").resize_call_count();
    app.set_pointer_px_for_test(320.0, 192.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.pointer_move_for_test(240.0, 192.0);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert_eq!(
        pty.lock().expect("pty").resize_call_count(),
        base,
        "a single-pane tab has no divider-drag resize path"
    );
}

#[test]
fn attach_overlay_closes_when_switching_to_already_attached_session() {
    // C5: the session-attach summon overlay must close when the selected
    // session is already attached in a tab. Before the fix, the already-attached
    // branch of route_attach_session switched tabs but returned WITHOUT closing
    // the overlay, so keyboard dispatch kept routing every key into the overlay's
    // type-to-filter box (never the switched-to session) until Esc.
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        return;
    };
    // Tag the active session as attached to a known id so find_attached_tab
    // resolves it (the dedup path), then open the summon overlay over it.
    app.mark_active_session_attached_for_test("s-already");
    app.open_session_attach_with_synthetic_sessions_for_test(&["s-already"]);
    assert!(
        app.overlay_open_for_test(),
        "precondition: the attach overlay is open"
    );

    // Accept the already-attached session: dedup switches to its tab.
    app.route_attach_session_for_test("s-already");

    assert!(
        !app.overlay_open_for_test(),
        "C5: the attach overlay must close on the already-attached branch so \
         keystrokes reach the switched-to session, not the filter box"
    );
}

// --- W2: workspace rail chrome (design doc §7, ODP-2/-6) ---

#[test]
fn single_workspace_default_shows_no_workspace_rail() {
    // ODP-2: a single-workspace launch is ZERO chrome change from a top-only tab
    // bar — the auto rail stays hidden until a second workspace exists.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_rail_width_manual_for_test(16);
    // Two tabs → top bar; one workspace + auto rail → no rail band.
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 0),
        "one workspace: top bar only, no rail"
    );

    // A second workspace makes the auto rail appear; the active (new) workspace
    // is single-tab, so the top bar drops and the rail band takes over.
    add_workspace(&mut app);
    assert_eq!(app.workspace_count_for_test(), 2);
    assert_eq!(
        app.tab_reserve_for_test(),
        (0, 16),
        "two workspaces: the auto rail appears"
    );
}

#[test]
fn workspace_rail_always_shows_with_a_single_workspace() {
    // ODP-2: `workspace_rail = always` pins the rail even with one workspace
    // (alongside the top bar).
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_tab_rail_width_manual_for_test(16);
    app.set_workspace_rail_for_test("always");
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(
        app.tab_reserve_for_test(),
        (1, 16),
        "always: top bar row + rail band with one workspace"
    );
}

#[test]
fn clicking_a_workspace_rail_slot_switches_the_active_workspace() {
    // ODP-10 / §7.1: a press on a rail slot dispatches to `switch_workspace`,
    // not `switch_tab`; the band reports the WORKSPACE surface.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app); // two workspaces; active = the new one (index 1)
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Slot 0 body (top-margin row 1, label col) → the pointer is over the
    // WORKSPACE band, and a press switches to workspace 0.
    app.set_pointer_px_for_test(12.0, 24.0);
    assert_eq!(
        app.chrome_hit_band_for_test(),
        Some("workspace"),
        "the rail band is a workspace surface, not a tab surface"
    );
    app.mouse_left_press_for_test();
    assert_eq!(
        app.active_workspace_index_for_test(),
        0,
        "a rail slot press switches the active workspace"
    );
}

#[test]
fn the_rail_plus_slot_resolves_to_a_new_workspace_hit() {
    // §7.4: the rail `+` slot is New Workspace — it resolves to a `NewTab` hit on
    // the WORKSPACE band (dispatched to `handle_new_workspace`).
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    add_workspace(&mut app);
    app.rename_workspace_for_test(0, "a");
    app.rename_workspace_for_test(1, "b");

    // The `+` anchors one gap below workspace 1's single label row (row 6 centre
    // y = 6*16 + 8 = 104), mirroring the tab-rail `+` geometry.
    app.set_pointer_px_for_test(64.0, 104.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new"), "+ slot → new");
    assert_eq!(
        app.chrome_hit_band_for_test(),
        Some("workspace"),
        "the + is on the workspace band → New Workspace"
    );
}

#[test]
fn the_shared_rename_field_retargets_the_workspace_name() {
    // §7.1: the rename field is shared with the tab bar; on the rail it re-targets
    // `Workspace.name`.
    let Some(mut app) = tab_bar_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_test_cell_for_test(cell(8, 16));
    app.set_workspace_rail_for_test("left");
    app.rename_workspace_for_test(0, "old");
    let after = app.rename_workspace_via_field_for_test(0, "renamed");
    assert_eq!(
        after.as_deref(),
        Some("renamed"),
        "committing the rename field writes Workspace.name"
    );
    assert!(
        !app.rename_active_for_test(),
        "Enter closes the rename field"
    );
}

// ---- WP2: debounced workspace-shape autosave (sub-ODP 8c/8d) ----

/// A burst of shape mutations coalesces into exactly one autosave write once the
/// debounce quiet window elapses — a drag/rapid-edit stream is one write, not N.
#[test]
fn shape_autosave_debounces_a_mutation_burst_into_one_write() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_primary_instance_for_test(true);

    let t0 = Instant::now();
    // First pass establishes the fingerprint baseline: no write, nothing pending.
    app.run_about_to_wait_maintenance_for_test(t0);
    assert_eq!(app.autosave_saves_for_test(), 0);
    assert!(!app.autosave_pending_for_test());

    // A shape mutation arms the debounce but does not write within the window.
    assert!(add_workspace(&mut app));
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(10));
    assert!(
        app.autosave_pending_for_test(),
        "a mutation arms the debounce"
    );
    assert_eq!(
        app.autosave_saves_for_test(),
        0,
        "not written within the window"
    );

    // A further mutation re-arms (coalesces) rather than writing twice.
    assert!(add_workspace(&mut app));
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_millis(500));
    assert_eq!(app.autosave_saves_for_test(), 0);

    // Once the quiet window elapses, exactly one write fires and clears pending.
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_secs(3));
    assert_eq!(
        app.autosave_saves_for_test(),
        1,
        "the burst coalesced into a single write"
    );
    assert!(!app.autosave_pending_for_test());

    // No further writes without a further mutation.
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_secs(6));
    assert_eq!(app.autosave_saves_for_test(), 1);
}

/// A non-primary instance never writes the shape snapshot, even across a
/// mutation and a full debounce window (sub-ODP 8d: only the lock holder saves).
#[test]
fn shape_autosave_is_inert_on_a_non_primary_instance() {
    let Some((mut app, _fixtures)) = app_with_two_sessions() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Default construction is non-primary (set_primary_instance never called).
    let t0 = Instant::now();
    app.run_about_to_wait_maintenance_for_test(t0);
    assert!(add_workspace(&mut app));
    app.run_about_to_wait_maintenance_for_test(t0 + std::time::Duration::from_secs(3));
    assert_eq!(app.autosave_saves_for_test(), 0);
    assert!(!app.autosave_pending_for_test());
}

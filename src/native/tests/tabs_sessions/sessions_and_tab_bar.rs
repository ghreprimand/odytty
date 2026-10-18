// SPDX-License-Identifier: GPL-3.0-only
//! Sessions and tab bar tests for the multi-session tabs feature.

use super::*;

/// Reusable Phase A ownership fixture: workspace 0 has two panes and workspace
/// 1 has one. The returned ids let later command/notification tests assert that
/// state is attributed to the exact pane and restored with its workspace.
struct PaneWorkspaceOwnershipFixture {
    app: App,
    first_pane: usize,
    split_pane: usize,
    second_workspace_pane: usize,
}

fn pane_workspace_ownership_fixture() -> PaneWorkspaceOwnershipFixture {
    let (mut app, _bytes) = single_session_app();
    let first_pane = app.focused_pane_id_for_test();
    let dims = NativeOptions::default().initial_grid;

    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let split_pane = app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);

    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let second_workspace = app.push_headless_workspace_with_for_test(terminal, writer, headless);
    assert_eq!(second_workspace, 1);
    let second_workspace_pane = app.focused_pane_id_for_test();

    PaneWorkspaceOwnershipFixture {
        app,
        first_pane,
        split_pane,
        second_workspace_pane,
    }
}

#[test]
fn pane_focus_is_owned_and_restored_by_its_workspace() {
    let mut fixture = pane_workspace_ownership_fixture();

    assert_ne!(fixture.first_pane, fixture.split_pane);
    assert_ne!(fixture.split_pane, fixture.second_workspace_pane);
    assert_eq!(fixture.app.workspace_count_for_test(), 2);
    assert_eq!(fixture.app.active_workspace_index_for_test(), 1);
    assert_eq!(
        fixture.app.focused_pane_id_for_test(),
        fixture.second_workspace_pane
    );
    assert_eq!(fixture.app.active_pane_count_for_test(), 1);

    fixture
        .app
        .dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);
    assert_eq!(fixture.app.active_workspace_index_for_test(), 0);
    assert_eq!(fixture.app.focused_pane_id_for_test(), fixture.split_pane);
    assert_eq!(fixture.app.active_pane_count_for_test(), 2);

    fixture
        .app
        .dispatch_workspace_action_for_test(BindableAction::NextWorkspace);
    assert_eq!(fixture.app.active_workspace_index_for_test(), 1);
    assert_eq!(
        fixture.app.focused_pane_id_for_test(),
        fixture.second_workspace_pane
    );
    assert_eq!(fixture.app.active_pane_count_for_test(), 1);
}

/// WP1 capture: a headless multi-workspace, multi-pane `App` captures into a
/// `ShapeSnapshot` that mirrors names, tab count/order, the split tree, and
/// per-pane cwd - and the captured shape round-trips through the JSON layer.
#[test]
fn capture_shape_records_workspaces_tabs_panes_and_cwd() {
    use crate::native::persistence::{PaneShape, ShapeSnapshot};

    let (mut app, fixtures) = app_with_two_sessions();

    // Seed a known cwd on the first session's terminal (tab 0's sole pane).
    fixtures[0]
        .0
        .lock()
        .expect("terminal")
        .seed_working_directory("/tmp/odytty-fixture/project".to_owned());

    // Split the ACTIVE tab (tab 0) so it becomes a two-pane split; tab 1 stays
    // a lone leaf. `push` appends tabs without switching, so tab 0 is active.
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);
    assert_eq!(app.active_pane_count_for_test(), 2);

    // Add a second workspace (this seam switches to it).
    add_workspace(&mut app);

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
                    cwd: Some("/tmp/odytty-fixture/project".to_owned()),
                    session_host_id: None,
                    remote_host: None,
                    launch_profile: None,
                    read_only: false,
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
fn add_workspace_with_terminal(app: &mut App) -> Arc<Mutex<Terminal>> {
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    app.push_headless_workspace_with_for_test(terminal.clone(), writer, headless);
    terminal
}

/// A bell rung in a BACKGROUND tab is drained by the arena-wide
/// maintenance sweep and latches that tab's activity flag - without switching
/// to it and without touching the active tab's flag.
#[test]
fn background_tab_bell_latches_activity_without_switching() {
    let (mut app, fixtures) = app_with_two_sessions();
    // Ring a bell in the background tab (tab 1); tab 0 is active/focused.
    fixtures[1].0.lock().expect("terminal").advance(b"\x07");

    let (focused, background, _prompt) = app.drain_bells_for_test();
    assert!(!focused, "the focused pane did not ring");
    assert!(background, "a background session rang -> urgency path");
    // The background tab latched; the active tab did not. (A latched flag on a
    // non-active tab also proves no switch happened - a switch would clear it.)
    assert!(app.tab_activity_for_test(0, 1));
    assert!(!app.tab_activity_for_test(0, 0));
    app.set_test_cell_for_test(cell(8, 16));
    let chrome = app
        .tab_bar_band_text_for_test()
        .expect("two tabs render a top bar")
        .join("\n");
    assert_eq!(
        chrome.matches('●').count(),
        1,
        "the maintained background-tab latch reaches visible chrome exactly once"
    );
}

/// NF21-6 / §5 rule 3: a bell rung in a BACKGROUND WORKSPACE latches its tab's
/// activity (and the derived workspace rollup) without waiting for a switch -
/// the surface a bell is most for after workspaces landed.
#[test]
fn background_workspace_bell_latches_activity_without_switching() {
    let (mut app, fixtures) = app_with_two_sessions();
    // Add and switch to a second workspace, so workspace 0 is now background.
    let _ws1_terminal = add_workspace_with_terminal(&mut app);
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

/// The active-visible focused pane keeps today's behavior - its bell
/// starts the viewport flash and never latches an activity flag (you saw it).
#[test]
fn focused_pane_bell_flashes_and_does_not_latch_activity() {
    let (mut app, fixtures) = app_with_two_sessions();
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

/// A bell on the focused pane of a MULTIPANE active tab now drains
/// (previously it stranded - the multipane render path never drained bells).
#[test]
fn multipane_focused_bell_drains_and_does_not_strand() {
    let (mut app, _fixtures) = app_with_two_sessions();
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    // Split the active tab; focus lands on the new pane (`terminal`).
    app.seed_headless_split_pane_with_for_test(true, terminal.clone(), writer, headless);
    assert_eq!(app.active_pane_count_for_test(), 2);

    terminal.lock().expect("terminal").advance(b"\x07");
    let (focused_first, _bg1, _p1) = app.drain_bells_for_test();
    assert!(focused_first, "a multipane focused-pane bell drains");
    let (focused_again, _bg2, _p2) = app.drain_bells_for_test();
    assert!(!focused_again, "the bell drained once and did not strand");
}

/// Switching to a flagged tab clears its activity latch - viewing is
/// what clears the rollup signal.
#[test]
fn switching_to_a_flagged_tab_clears_its_activity() {
    let (mut app, fixtures) = app_with_two_sessions();
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

#[test]
fn single_pane_overlay_geometry_equals_the_window_grid_and_pointer_cell() {
    // Byte-identity guard for the multi-pane overlay fix: on a single-pane tab
    // (no GPU geometry) the window-overlay grid dims and window-space pointer
    // cell fall back to exactly `self.grid` / `self.pointer_cell`, so the
    // single-pane overlay hit-test path is unchanged. The multi-pane mapping is
    // unit-tested separately in `panes::tests`.
    let (mut app, _bytes) = single_session_app();
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
    // left A's toggle deadline in the wake set with nothing to advance it -
    // `WaitUntil(<past>)` returned immediately every iteration and busy-spun a
    // core. The fix narrows the source to the active pane and parks background
    // panes' timers in maintenance. This drives the real switch + maintenance
    // and asserts the STRICT invariant: after maintenance the next wake is None
    // or STRICTLY in the future - never a stale past instant.
    //
    // Headless Apps have no native surface (`window.is_some()` is false), so
    // render-gated blink/animation sources would be excluded by the plain
    // next_wake_deadline path. Model a presented window explicitly so this
    // regression cannot pass vacuously on sync-hold / config-only wakes.
    let (mut app, _fixtures) = app_with_two_sessions();

    // Arm the ACTIVE pane's (session 0) blink: its activity-hold deadline
    // (t0+650ms)
    // now sits in the wake set. Also arm the other two per-session deadline
    // sources - cursor ease/slide and the synchronized-output hold - so this one
    // flow covers every fan-out source `next_wake_deadline` mins over sessions.
    let t0 = Instant::now();
    app.arm_active_cursor_blink_for_test(t0);
    app.arm_active_cursor_anim_for_test(t0); // ease t0+200ms, slide t0+150ms
    app.arm_active_sync_hold_for_test(t0);
    assert!(
        app.next_wake_deadline_for_surface_for_test(true)
            .is_some_and(|d| d <= t0 + Duration::from_millis(530)),
        "an armed active pane schedules a near-future wake"
    );

    // Switch to session 1 - session 0 is now BACKGROUND and never rendered, so
    // its blink toggle is never polled again.
    assert!(app.switch_to_session_for_test(1));

    // Well past session 0's toggle boundary, drive the maintenance pass the loop
    // runs before every wait recompute. It must NOT leave session 0's stale
    // deadline in the wake set.
    let later = t0 + Duration::from_secs(5);
    app.run_about_to_wait_maintenance_for_test(later);
    match app.next_wake_deadline_for_surface_for_test(true) {
        None => {}
        Some(next) => assert!(
            next > later,
            "a backgrounded pane's stale blink must not survive as a past wake \
             (was {:?} past now)",
            later.saturating_duration_since(next)
        ),
    }

    // Activate session 0 again. Because it was parked while
    // backgrounded, it starts from a clean (non-stale) blink - so even the frame
    // the pane is switched back on parks cleanly (no past-instant wake).
    assert!(app.switch_to_session_for_test(0));
    let later2 = later + Duration::from_secs(1);
    app.run_about_to_wait_maintenance_for_test(later2);
    match app.next_wake_deadline_for_surface_for_test(true) {
        None => {}
        Some(next) => assert!(
            next > later2,
            "a pane switched back to must not carry a stale past wake"
        ),
    }
}

#[test]
fn split_focused_cursor_effects_advance_without_background_wakes() {
    // The split render path consumes cursor motion/easing only for the focused
    // pane. Non-focused panes remain parked, preserving the NF20-B invariant
    // that every sourced wake has a render consumer.
    let (mut app, _bytes) = single_session_app();
    let (terminal, writer, headless, _b) = recorded_session(NativeOptions::default().initial_grid);

    // Focus lands on the new pane; the original becomes an idle background pane.
    app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);
    assert_eq!(app.active_pane_count_for_test(), 2);

    let t0 = Instant::now();
    let cell = CellSize {
        width: 8,
        height: 16,
        baseline: 0,
    };
    let origin = [200.0, 96.0];
    let prior = snapshot(&["  "], 2);
    let mut current = prior.clone();
    current.cursor.column = 1;

    app.disable_cursor_effects_for_test();
    app.set_last_presented_snapshot_for_test(prior);
    let (off_effects, off_glow, off_streak, off_params) =
        app.advance_multipane_cursor_effects_for_test(t0, &mut current, cell, origin);
    assert!(off_effects.is_empty());
    assert!(off_glow.is_none());
    assert!(off_streak.is_none());
    assert_eq!(off_params, CursorRenderParams::default());
    assert!(
        app.focused_cursor_animation_deadline_for_test().is_none(),
        "the cursor-effect off path arms no frame-paced wake"
    );

    app.enable_cursor_effects_for_test();
    app.set_last_presented_snapshot_for_test(snapshot(&["  "], 2));

    // The first frame arms the slide; the next advances the shared cursor/aura
    // parameters. This one-cell move intentionally emits no trail echo, keeping
    // focused split-pane typing as crisp as single-pane.
    let _ = app.advance_multipane_cursor_effects_for_test(t0, &mut current, cell, origin);
    let (effects, glow, streak, params) = app.advance_multipane_cursor_effects_for_test(
        t0 + Duration::from_millis(20),
        &mut current,
        cell,
        origin,
    );
    assert_ne!(
        params.offset,
        [0.0, 0.0],
        "focused split cursor advances mid-slide"
    );
    assert!(
        params.alpha > 0.0 && params.alpha < 1.0,
        "focused split cursor advances eased alpha"
    );
    assert!(effects.is_empty(), "one-cell moves omit the trail echo");
    assert!(streak.is_none(), "nearby moves emit no large-jump streak");
    let logical_cursor_x = origin[0] + current.cursor.column as f32 * cell.width as f32;
    let glow = glow.expect("focused split emits one clipped aura request");
    assert_eq!(
        glow.clip_rect,
        [
            origin[0],
            origin[1],
            origin[0] + 2.0 * cell.width as f32,
            origin[1] + cell.height as f32,
        ]
    );
    let aura = build_cursor_glow_instance(
        &current,
        cell,
        CursorStyle::Block,
        origin,
        params,
        1.0,
        1.0,
        glow,
        None,
    )
    .expect("focused split builds one aura instance");
    assert!((aura.source_rect[0] - (logical_cursor_x + params.offset[0])).abs() < 0.01);
    assert!(aura.quad_rect[0] >= origin[0]);
    assert!(aura.quad_rect[2] <= origin[0] + 2.0 * cell.width as f32);

    app.run_about_to_wait_maintenance_for_test(t0 + Duration::from_millis(20));
    assert!(
        app.background_cursor_timers_parked_for_test(),
        "idle panes arm no cursor-animation wake"
    );
    assert!(
        app.next_wake_deadline_for_surface_for_test(true)
            .is_some_and(|deadline| deadline > t0),
        "the focused pane retains a bounded future wake on a presented surface"
    );

    // A later consumer frame settles the slide/ease; only the normal blink
    // toggle may remain, and it must still be strictly in the future.
    let later = t0 + Duration::from_secs(5);
    app.run_about_to_wait_maintenance_for_test(later);
    let _ = app.advance_multipane_cursor_effects_for_test(later, &mut current, cell, origin);
    match app.next_wake_deadline_for_surface_for_test(true) {
        None => {}
        Some(next) => assert!(
            next > later,
            "the focused split pane must not strand a past wake \
             (was {:?} past now)",
            later.saturating_duration_since(next)
        ),
    }
}

#[test]
fn split_large_jump_streak_uses_only_the_focused_pane_clip_and_wake() {
    let (mut app, _bytes) = single_session_app();
    let (terminal, writer, headless, _b) = recorded_session(NativeOptions::default().initial_grid);
    app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);
    app.enable_cursor_effects_for_test();

    let t0 = Instant::now();
    let cell = CellSize {
        width: 8,
        height: 16,
        baseline: 0,
    };
    let origin = [160.0, 80.0];
    let mut prior = snapshot(&["                    "], 20);
    let mut current = prior.clone();
    current.cursor.column = 12;
    app.set_last_presented_snapshot_for_test(prior.clone());
    let _ = app.advance_multipane_cursor_effects_for_test(t0, &mut prior, cell, origin);

    let started_at = t0 + Duration::from_millis(1);
    let (_, _, follower, params) =
        app.advance_multipane_cursor_effects_for_test(started_at, &mut current, cell, origin);
    let follower = follower.expect("large jump starts the focused follower immediately");
    assert_eq!(
        app.cursor_streak_deadline_for_test(),
        Some(started_at + Duration::from_millis(16))
    );
    assert!(params.follower_active);
    assert_eq!(follower.destination, Position { row: 0, column: 12 });
    assert!(follower.rect[0] > 0.0 && follower.rect[2] < 13.0 * cell.width as f32);
    assert_eq!(
        follower.clip_rect,
        [
            origin[0],
            origin[1],
            origin[0] + 20.0 * cell.width as f32,
            origin[1] + cell.height as f32,
        ]
    );
    assert!(
        app.background_cursor_timers_parked_for_test(),
        "the background split pane contributes no streak wake"
    );
}

#[test]
fn output_into_a_background_split_pane_drives_a_rebuild() {
    // NF21-7 regression (paired with NF21-1; its spin masked this until fixed).
    // `App` has no own `needs_rebuild` - `self.needs_rebuild` Derefs to the
    // FOCUSED pane. PTY output for a non-focused but visible split pane sets THAT
    // pane's flag and requests a redraw, but the frame gate read only the focused
    // pane's flag, so a build streaming into the other half of a split never
    // repainted until the user typed into the focused pane. The gate now ORs the
    // rebuild flag across every visible pane of the active tab.
    let (mut app, _bytes) = single_session_app();
    let (terminal, writer, headless, _b) = recorded_session(NativeOptions::default().initial_grid);
    // The sole pane's token, captured before the split makes it the background.
    let background = app.active_session_token_for_test();
    app.seed_headless_split_pane_with_for_test(true, terminal, writer, headless);
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
        "the focused pane stays clean: its flag alone would keep the gate shut"
    );
    assert!(
        app.should_rebuild_frame_for_test(),
        "output into a background visible pane must drive a frame rebuild"
    );
}

#[test]
fn input_routes_to_the_active_session_writer_after_switch() {
    let (mut app, fixtures) = app_with_two_sessions();
    let [(_, bytes_a), (_, bytes_b)] = fixtures;

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
    let (mut app, _fixtures) = app_with_two_sessions();

    app.advance_tab_bytes_at_position_for_test(0, b"alpha-session\r\n");
    app.advance_tab_bytes_at_position_for_test(1, b"beta-session\r\n");

    let plain_a = app
        .tab_plain_text_at_position_for_test(0)
        .expect("plain text a");
    let plain_b = app
        .tab_plain_text_at_position_for_test(1)
        .expect("plain text b");

    assert!(plain_a.contains("alpha-session"));
    assert!(!plain_a.contains("beta-session"));
    assert!(plain_b.contains("beta-session"));
    assert!(!plain_b.contains("alpha-session"));
}

#[test]
fn resize_updates_both_terminals_and_ptys() {
    // The one tab fixture that needs real PTYs: it asserts the size the kernel
    // reports for each child. A shell that cannot start fails the test.
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let spawn = || {
        let session = spawn_test_pause_shell(dims).expect("spawn test shell");
        let _ = session.take_writer().expect("take the shell writer");
        let writer: PtyWriter = Arc::new(Mutex::new(Box::new(std::io::sink())));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        (terminal, writer, Arc::new(Mutex::new(session)))
    };
    let (terminal_a, writer_a, pty_a) = spawn();
    let (terminal_b, writer_b, pty_b) = spawn();
    let mut app = App::new(
        options,
        terminal_a,
        writer_a,
        pty_a,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.push_session_for_test(terminal_b, writer_b, pty_b);

    let cell = cell(8, 16);
    assert!(app.resize_grid(cell, 512, 256));
    let expected = Dimensions::new(64, 15);

    assert_eq!(app.tab_dimensions_at_position_for_test(0), Some(expected));
    assert_eq!(app.tab_dimensions_at_position_for_test(1), Some(expected));
    #[cfg(unix)]
    {
        assert_eq!(
            app.tab_pty_dimensions_at_position_for_test(0),
            Some(expected)
        );
        assert_eq!(
            app.tab_pty_dimensions_at_position_for_test(1),
            Some(expected)
        );
    }
}

#[test]
fn closing_active_session_activates_neighbor_and_last_close_sets_exit() {
    let (mut app, _fixtures) = app_with_two_sessions();

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
    let (mut app, _fixtures) = app_with_two_sessions();

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
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let mut app = App::new_headless(
        options,
        terminal.clone(),
        writer,
        headless,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    assert!(!app.tab_bar_visible_for_test());

    let (terminal_b, writer_b, headless_b, _bytes_b) = recorded_session(dims);
    app.push_headless_session_with_for_test(terminal_b, writer_b, headless_b);
    assert!(app.tab_bar_visible_for_test());
}

#[test]
fn tab_bar_shows_for_a_lone_renamed_tab() {
    // F4 a single tab normally hides the bar, but once it
    // carries a custom name the bar must show so the named "workflow" tab is
    // visible. (Fails before the show-rule change: a lone tab stayed hidden
    // regardless of its title override.)
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let mut app = App::new_headless(
        options,
        terminal,
        writer,
        headless,
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
    // F4 the opt-in `always_show_tab_bar` setting shows the strip even
    // for one unnamed tab, applied live. (Fails before the show-rule change:
    // the setting did not exist / was not consulted.)
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let mut app = App::new_headless(
        options,
        terminal,
        writer,
        headless,
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
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    let mut app = App::new_headless(
        options,
        terminal.clone(),
        writer,
        headless,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );

    let cell = cell(8, 16);
    assert!(!app.resize_grid(cell, 640, 384));
    assert_eq!(
        app.tab_dimensions_at_position_for_test(0),
        Some(Dimensions::new(80, 24))
    );

    let (terminal_b, writer_b, headless_b, _bytes_b) = recorded_session(dims);
    app.push_headless_session_with_for_test(terminal_b, writer_b, headless_b);
    assert!(app.resize_grid(cell, 640, 384));
    assert_eq!(
        app.tab_dimensions_at_position_for_test(0),
        Some(Dimensions::new(80, 23))
    );
    assert_eq!(
        app.tab_dimensions_at_position_for_test(1),
        Some(Dimensions::new(80, 23))
    );
}

#[test]
fn tab_bar_hit_test_reports_switch_close_and_new_actions() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));

    app.set_pointer_px_for_test(12.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("switch"));

    app.set_pointer_px_for_test(188.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("close"));

    app.set_pointer_px_for_test(612.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new"));

    app.set_pointer_px_for_test(628.0, 8.0);
    assert_eq!(app.tab_bar_hit_for_test(), Some("new_profile"));
}

#[test]
fn top_tab_drag_real_route_reorders_and_preserves_active_identity() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    let first = app
        .session_token_at_position_for_test(0)
        .expect("first tab token");
    let second = app
        .session_token_at_position_for_test(1)
        .expect("second tab token");
    assert_eq!(app.active_session_token_for_test(), first);

    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    assert_eq!(app.top_tab_drag_for_test(), Some((false, 0)));
    app.pointer_move_for_test(500.0, 8.0);
    assert_eq!(app.top_tab_drag_for_test(), Some((true, 2)));
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Grabbing
    );
    app.mouse_left_release_for_test();

    assert_eq!(app.top_tab_drag_for_test(), None);
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Default
    );
    assert_eq!(app.session_token_at_position_for_test(0), Some(second));
    assert_eq!(app.session_token_at_position_for_test(1), Some(first));
    assert_eq!(app.active_session_token_for_test(), first);
}

#[test]
fn top_chrome_geometry_agrees_in_window_space_with_a_wide_left_rail() {
    let mut app = tab_bar_app();
    let cell = cell(8, 16);
    let padding = WindowPadding::from_logical(4.0, 1.0);
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(900, 400, padding);
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(32);
    add_workspace(&mut app);
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    app.push_headless_session_with_for_test(terminal, writer, headless);
    app.set_test_cell_for_test(cell);
    app.set_test_surface_for_test(900, 400, padding);

    // The top strip begins at pad + 32 rail columns + the chrome-facing gap
    // (one window padding between the rail band and the bar's
    // content-aligned columns) = 4 + 256 + 4 = 264px. With two tabs, slot 1
    // begins 24 columns later at 456px. A pointer just inside that slot must
    // hit slot 1, resolve a drag of slot 0 before slot 1, and report the same
    // 456px insertion boundary. This failed when drag math interpreted raw
    // window X as bar-local X.
    let pointer_x = 457.0;
    let (hit_idx, drop_idx, boundary, slot_start) = app
        .top_chrome_geometry_probe_for_test(pointer_x, 12.0, 0)
        .expect("pointer resolves inside the shifted top strip");
    assert_eq!(hit_idx, 1);
    assert_eq!(drop_idx, 1);
    assert_eq!(boundary, 456.0);
    assert_eq!(boundary, slot_start);
}

#[test]
fn top_tab_drag_real_route_commits_a_neighbour_drop() {
    let mut app = tab_bar_app();
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    app.push_headless_session_with_for_test(terminal, writer, headless);
    app.set_test_cell_for_test(cell(8, 16));
    let before: Vec<_> = (0..3)
        .map(|idx| {
            app.session_token_at_position_for_test(idx)
                .expect("tab token")
        })
        .collect();

    // Press the third slot and hover the compacted second slot's upper half.
    // The origin-excluded drop layout must resolve this as insertion before tab
    // 1, not as the lifted tab's stale home index.
    app.set_pointer_px_for_test(400.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(200.0, 8.0);
    assert_eq!(app.top_tab_drag_for_test(), Some((true, 1)));
    app.mouse_left_release_for_test();

    assert_eq!(app.session_token_at_position_for_test(0), Some(before[0]));
    assert_eq!(app.session_token_at_position_for_test(1), Some(before[2]));
    assert_eq!(app.session_token_at_position_for_test(2), Some(before[1]));
}

#[test]
fn top_tab_drag_threshold_is_symmetric_across_real_neighbour_midpoints() {
    fn three_tab_app() -> App {
        let mut app = tab_bar_app();
        let dims = NativeOptions::default().initial_grid;
        let (terminal, writer, headless, _bytes) = recorded_session(dims);
        app.push_headless_session_with_for_test(terminal, writer, headless);
        app.set_test_cell_for_test(cell(8, 16));
        app
    }

    let mut left = three_tab_app();
    // Three 24-column slots have real midpoints at x=96, 288, 480. Grab the
    // middle tab at its center. One pixel before the left neighbor midpoint is
    // still the no-op insertion; one pixel beyond crosses it.
    left.set_pointer_px_for_test(288.0, 8.0);
    left.mouse_left_press_for_test();
    left.pointer_move_for_test(97.0, 8.0);
    assert_eq!(left.top_tab_drag_for_test(), Some((true, 2)));
    left.pointer_move_for_test(95.0, 8.0);
    assert_eq!(left.top_tab_drag_for_test(), Some((true, 0)));

    let mut right = three_tab_app();
    right.set_pointer_px_for_test(288.0, 8.0);
    right.mouse_left_press_for_test();
    right.pointer_move_for_test(479.0, 8.0);
    assert_eq!(right.top_tab_drag_for_test(), Some((true, 2)));
    right.pointer_move_for_test(481.0, 8.0);
    assert_eq!(right.top_tab_drag_for_test(), Some((true, 3)));
}

#[test]
fn sub_threshold_top_tab_press_stays_a_switch_click() {
    let mut app = tab_bar_app();
    app.set_test_cell_for_test(cell(8, 16));
    let first = app.active_session_token_for_test();
    let second = app
        .session_token_at_position_for_test(1)
        .expect("second tab token");

    app.set_pointer_px_for_test(200.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(202.0, 10.0);
    assert_eq!(app.top_tab_drag_for_test(), Some((false, 1)));
    app.mouse_left_release_for_test();

    assert_eq!(app.active_session_token_for_test(), second);
    assert_ne!(app.active_session_token_for_test(), first);
    assert_eq!(app.session_token_at_position_for_test(0), Some(first));
    assert_eq!(app.session_token_at_position_for_test(1), Some(second));
}

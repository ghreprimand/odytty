// SPDX-License-Identifier: GPL-3.0-only
//! W3 App-level workspace keyboard + command-palette tests. These drive a real
//! `App` (with a real `EventLoop` proxy so the workspace/tab spawns succeed;
//! skipped when no PTY is available) through the production dispatch paths:
//! the six workspace `BindableAction`s and the palette workspace rows
//! (`workspace-switch-<idx>` / `workspace-new` / `workspace-rename`). The
//! model-level hierarchy invariants live in `session.rs`; here we pin the App
//! wiring — creation, cycling, close-with-exit-guard, rename commit, and the
//! palette routing.

use super::super::pty::UserEvent;
use super::super::session::{Session, SessionToken, WorkspaceSet};
use super::*;
use crate::settings::BindableAction;
use winit::event_loop::EventLoop;
#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

/// Build an `App` backed by a real `EventLoop` proxy so `handle_new_workspace`
/// (which spawns a fresh shell) succeeds. Returns `None` when no PTY is
/// available. Skipped at runtime on macOS: the harness builds an
/// off-main-thread `EventLoop`, which AppKit forbids.
fn app_with_proxy() -> Option<(App, EventLoop<UserEvent>)> {
    let dims = Dimensions::new(80, 24);
    let session = spawn_test_pause_shell(dims).ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
    }
    #[cfg(target_os = "windows")]
    {
        EventLoopBuilderExtWindows::with_any_thread(&mut builder, true);
    }
    let event_loop = builder.build().ok()?;
    let proxy = event_loop.create_proxy();
    let sessions = WorkspaceSet::new(
        Session::new(SessionToken(0), terminal, writer, pty, None),
        Some(proxy),
    );
    let app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, event_loop))
}

macro_rules! app_or_skip {
    () => {{
        let Some((app, event_loop)) = app_with_proxy() else {
            eprintln!("skipping: no PTY available");
            return;
        };
        // Keep the loop alive for the test's duration (dropping it early would
        // invalidate the proxy the workspace spawns rely on).
        (app, event_loop)
    }};
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn new_workspace_action_appends_and_switches() {
    let (mut app, _event_loop) = app_or_skip!();
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(app.active_workspace_index_for_test(), 0);

    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);

    assert_eq!(
        app.workspace_count_for_test(),
        2,
        "a workspace was appended"
    );
    assert_eq!(
        app.active_workspace_index_for_test(),
        1,
        "focus follows the new workspace"
    );
    // Default rail names.
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["Workspace 1".to_owned(), "Workspace 2".to_owned()]
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn next_and_prev_workspace_cycle_wrapping() {
    let (mut app, _event_loop) = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 3);
    assert_eq!(app.active_workspace_index_for_test(), 2);

    app.dispatch_workspace_action_for_test(BindableAction::NextWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 0, "next wraps to 0");
    app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);
    assert_eq!(
        app.active_workspace_index_for_test(),
        2,
        "prev wraps to end"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn workspace_cycle_chords_dispatch_through_production_key_path() {
    let (mut app, _event_loop) = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Ctrl+Shift+PageUp = PrevWorkspace, Ctrl+Shift+PageDown = NextWorkspace.
    app.drive_named_key_with_mods_for_test(NamedKey::PageUp, true, true);
    assert_eq!(app.active_workspace_index_for_test(), 0);
    app.drive_named_key_with_mods_for_test(NamedKey::PageDown, true, true);
    assert_eq!(app.active_workspace_index_for_test(), 1);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn close_workspace_removes_it_without_exiting_when_others_remain() {
    let (mut app, _event_loop) = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);

    app.dispatch_workspace_action_for_test(BindableAction::CloseWorkspace);
    assert_eq!(
        app.workspace_count_for_test(),
        1,
        "the workspace was reaped"
    );
    assert!(
        !app.pending_exit_for_test(),
        "closing a non-last workspace never exits the app"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn close_last_workspace_signals_exit_without_emptying_the_arena() {
    let (mut app, _event_loop) = app_or_skip!();
    assert_eq!(app.workspace_count_for_test(), 1);

    app.dispatch_workspace_action_for_test(BindableAction::CloseWorkspace);
    assert!(
        app.pending_exit_for_test(),
        "closing the last workspace exits the app"
    );
    // The guard returns before reaping, so the arena still holds the workspace
    // (teardown happens on the exit path, not here) — no Deref-on-empty panic.
    assert_eq!(app.workspace_count_for_test(), 1);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn rename_workspace_action_opens_overlay_and_commits_the_active_name() {
    let (mut app, _event_loop) = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::RenameWorkspace);
    assert!(app.rename_overlay_open_for_test(), "rename overlay opened");

    app.commit_rename_for_test("infra");
    assert!(
        !app.rename_overlay_open_for_test(),
        "overlay closed on commit"
    );
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["infra".to_owned()],
        "the active workspace's rail name changed"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn empty_rename_leaves_the_workspace_name_unchanged() {
    let (mut app, _event_loop) = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::RenameWorkspace);
    app.commit_rename_for_test("   ");
    assert_eq!(
        app.workspace_names_for_test(),
        vec!["Workspace 1".to_owned()],
        "a blank field keeps the existing label"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn palette_switch_row_deep_switches_workspace() {
    let (mut app, _event_loop) = app_or_skip!();
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // "Switch to workspace 0" via its stable dynamic id.
    app.handle_palette_action_for_test("workspace-switch-0");
    assert_eq!(app.active_workspace_index_for_test(), 0);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn palette_new_workspace_row_creates_a_workspace() {
    let (mut app, _event_loop) = app_or_skip!();
    assert_eq!(app.workspace_count_for_test(), 1);
    app.handle_palette_action_for_test("workspace-new");
    assert_eq!(app.workspace_count_for_test(), 2);
    assert_eq!(app.active_workspace_index_for_test(), 1);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn palette_rename_workspace_row_opens_the_overlay() {
    let (mut app, _event_loop) = app_or_skip!();
    app.handle_palette_action_for_test("workspace-rename");
    assert!(app.rename_overlay_open_for_test());
    app.commit_rename_for_test("app");
    assert_eq!(app.workspace_names_for_test(), vec!["app".to_owned()]);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn move_tab_to_workspace_splices_without_switching() {
    let (mut app, _event_loop) = app_or_skip!();
    // ws0 gets a second tab; ws1 is created (and becomes active), then we go
    // back to ws0 so the move is from the active workspace.
    app.new_tab_for_test();
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.active_workspace_index_for_test(), 1);
    app.handle_palette_action_for_test("workspace-switch-0");
    assert_eq!(app.active_workspace_index_for_test(), 0);
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);

    // Move the active tab of ws0 to ws1 via the picker path.
    let token = app.active_session_token_for_test();
    app.move_tab_to_workspace_for_test(token, 1);

    // v1: the active workspace does not follow the tab.
    assert_eq!(app.active_workspace_index_for_test(), 0);
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        1,
        "ws0 lost a tab"
    );
    // ws1 gained it (it had one tab, now two).
    app.handle_palette_action_for_test("workspace-switch-1");
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        2,
        "ws1 gained a tab"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn moving_the_last_tab_out_closes_the_source_workspace_app() {
    let (mut app, _event_loop) = app_or_skip!();
    // Two single-tab workspaces; active is ws1 after creation.
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);
    assert_eq!(app.active_workspace_index_for_test(), 1);

    // Moving ws1's only tab out (into ws0) empties and closes ws1 (ODP-3).
    let token = app.active_session_token_for_test();
    app.move_tab_to_workspace_for_test(token, 0);
    assert_eq!(app.workspace_count_for_test(), 1, "emptied source closed");
    assert!(
        !app.pending_exit_for_test(),
        "a surviving workspace remains"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn move_tab_is_a_noop_with_a_single_workspace() {
    let (mut app, _event_loop) = app_or_skip!();
    assert_eq!(app.workspace_count_for_test(), 1);
    let token = app.active_session_token_for_test();
    // Single workspace: no destinations, so the picker never opens (W4-v2).
    assert_eq!(app.open_move_tab_workspace_picker_for_test(token), 0);
    // Nothing moved: still one workspace, one tab.
    assert_eq!(app.workspace_count_for_test(), 1);
    assert_eq!(app.active_workspace_tab_count_for_test(), 1);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn rename_band_holds_the_single_pane_opaque_region_under_transparency() {
    // PROMPT-OPACITY: the rename/prompt band paints on its own path (not
    // `overlay_rect`), so before it was folded into the single-pane opaque span
    // it rendered translucent under a translucent window. With no modal open
    // the span is `None` (the opaque-window path stays byte-identical); opening
    // a workspace rename must mark the band's cells opaque.
    let (mut app, _event_loop) = app_or_skip!();
    assert!(
        app.single_pane_overlay_opaque_region_for_test().is_none(),
        "no modal open ⇒ no opaque span (opaque path is byte-identical)"
    );

    app.dispatch_workspace_action_for_test(BindableAction::RenameWorkspace);
    assert!(app.rename_overlay_open_for_test(), "rename prompt opened");

    let region = app
        .single_pane_overlay_opaque_region_for_test()
        .expect("an open rename band marks an opaque span");
    let (columns, rows) = app.grid_dims_for_test();
    let (top_rows, _side_cols) = app.tab_reserve_for_test();
    // The band is the centered 8..=48-wide, 3-tall box, shifted down by the
    // tab-chrome reservation. Pin width/height/top so the opaque cells match
    // the painted band exactly.
    assert_eq!(region.width, columns.clamp(8, 48), "band width");
    assert_eq!(region.height, 3, "band height");
    assert_eq!(
        region.top,
        (rows - 3) / 2 + top_rows,
        "band top offset by the tab-chrome reservation"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn secondary_instance_raises_the_restore_suppressed_notice() {
    // SECONDARY-INSTANCE-NOTICE: a second concurrent window is silently inert on
    // restore/autosave. When the user expects restore, the startup gate must
    // surface the one-line banner so the silence stops reading as "restore
    // didn't work".
    let (mut app, _event_loop) = app_or_skip!();
    app.set_primary_instance_for_test(false);
    app.set_restore_workspaces_for_test(true);
    app.notice_secondary_instance_for_test();
    let message = app
        .open_notice_message_for_test()
        .expect("a secondary instance expecting restore raises the notice");
    assert!(
        message.contains("won't restore or autosave"),
        "notice explains the suppression: {message}"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn primary_instance_stays_silent_on_the_restore_notice() {
    // The owner of the lock restores and autosaves normally — no notice.
    let (mut app, _event_loop) = app_or_skip!();
    app.set_primary_instance_for_test(true);
    app.set_restore_workspaces_for_test(true);
    app.notice_secondary_instance_for_test();
    assert!(
        app.open_notice_message_for_test().is_none(),
        "the primary instance never raises the suppression notice"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn secondary_instance_without_restore_expectation_stays_silent() {
    // With restore off the user is not relying on it, so the secondary window
    // has nothing to explain — no notice.
    let (mut app, _event_loop) = app_or_skip!();
    app.set_primary_instance_for_test(false);
    app.set_restore_workspaces_for_test(false);
    app.notice_secondary_instance_for_test();
    assert!(
        app.open_notice_message_for_test().is_none(),
        "restore off ⇒ the secondary window stays silent"
    );
}

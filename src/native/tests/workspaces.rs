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

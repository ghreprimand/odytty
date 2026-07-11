// SPDX-License-Identifier: GPL-3.0-only
//! NF21-P2 App-level tests: input-latch lifecycle across an active-session
//! change. A tab or workspace switch must shed the transient pointer/IME latches
//! so the outgoing session cannot strand a mid-drag `Selecting` state, the
//! incoming session cannot paint a phantom hover, and an in-flight IME
//! composition cannot commit into (or paint at) the new surface. The button-held
//! guard on the grid motion path is the belt-and-suspenders proof that a
//! `Selecting` latch with the button up can never extend on a bare `CursorMoved`.
//!
//! These drive a real `App` over a real `EventLoop` proxy (so tab/workspace
//! spawns succeed); skipped when no PTY is available, ignored on macOS (the
//! harness builds an off-main-thread winit `EventLoop`).

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
        (app, event_loop)
    }};
}

/// Arm a live grid selection drag: set an anchor cell and begin the selection
/// (no trailing release), leaving `pointer_drag` in `Selecting` with the
/// held-button flag set — the exact mid-drag state a switch must clear.
fn arm_drag(app: &mut App) {
    app.set_pointer_cell_for_test(0, 2);
    app.begin_selection_for_test();
    assert!(app.selecting_for_test(), "precondition: drag is armed");
    assert!(
        app.grid_left_held_for_test(),
        "precondition: held flag set while dragging"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn keyboard_tab_switch_clears_outgoing_drag() {
    let (mut app, _event_loop) = app_or_skip!();
    // A second tab to switch to and back from.
    app.new_tab_for_test();
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);
    // Focus tab 0 and arm a drag on it.
    app.switch_to_next_tab_for_test(); // -> tab 0 (wraps from tab 1)
    let armed_index = app.active_workspace_tab_count_for_test();
    assert_eq!(armed_index, 2);
    arm_drag(&mut app);

    // Switch away (the active-session-change seam fires) and back.
    app.switch_to_next_tab_for_test();
    assert!(
        !app.selecting_for_test(),
        "the incoming tab must not inherit a Selecting latch"
    );
    assert!(
        !app.grid_left_held_for_test(),
        "the held flag is dropped at the switch"
    );
    app.switch_to_next_tab_for_test();

    // Back on the originally-dragged tab: its latch was cleared at switch-out, so
    // it is no longer selecting and a buttonless move cannot resume the drag.
    assert!(
        !app.selecting_for_test(),
        "outgoing tab's Selecting latch was cleared, not resurrected"
    );
    assert!(!app.grid_left_held_for_test());
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn workspace_switch_clears_outgoing_drag() {
    let (mut app, _event_loop) = app_or_skip!();
    // A second workspace (focus follows it), then arm a drag there.
    app.dispatch_workspace_action_for_test(BindableAction::NewWorkspace);
    assert_eq!(app.workspace_count_for_test(), 2);
    arm_drag(&mut app);

    // Workspace Prev/Next both route through the same seam post-W1.
    app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);
    assert!(
        !app.selecting_for_test(),
        "the incoming workspace must not inherit a Selecting latch"
    );
    assert!(!app.grid_left_held_for_test());
    app.dispatch_workspace_action_for_test(BindableAction::NextWorkspace);
    assert!(
        !app.selecting_for_test(),
        "outgoing workspace's Selecting latch was cleared, not resurrected"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn button_held_guard_refuses_extend_after_lost_release() {
    let (mut app, _event_loop) = app_or_skip!();
    arm_drag(&mut app);

    // Simulate a lost release (alt-tab): the drag state persists but the held
    // flag is dropped on focus loss.
    app.on_window_focus_changed_for_test(false);
    assert!(
        !app.grid_left_held_for_test(),
        "focus loss drops the held flag"
    );

    // A bare CursorMoved on focus regain must NOT extend the stale drag.
    let before = app.selection_text_for_test();
    app.grid_pointer_moved_for_test(400.0, 200.0);
    let after = app.selection_text_for_test();
    assert_eq!(
        before, after,
        "buttonless move must not extend the stale selection latch"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn button_held_guard_allows_extend_while_held() {
    let (mut app, _event_loop) = app_or_skip!();
    arm_drag(&mut app);
    // Held flag is set; a move DOES extend (the guard is not over-broad).
    app.grid_pointer_moved_for_test(400.0, 0.0);
    assert!(
        app.selecting_for_test(),
        "an in-progress held drag still extends normally"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn focus_loss_cancels_scrollbar_drag_before_buttonless_motion() {
    let (mut app, _event_loop) = app_or_skip!();
    app.begin_scrollbar_drag_for_test();
    assert!(
        app.scrollbar_dragging_for_test(),
        "precondition: scrollbar drag is armed"
    );

    app.on_window_focus_changed_for_test(false);
    assert!(
        !app.scrollbar_dragging_for_test(),
        "focus loss cancels the scrollbar latch"
    );

    // A later buttonless cursor move follows the ordinary hover path instead
    // of reviving the cancelled scrub.
    app.grid_pointer_moved_for_test(400.0, 200.0);
    assert!(!app.scrollbar_dragging_for_test());
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn focus_loss_clears_pressed_mouse_report_button() {
    let (mut app, _event_loop) = app_or_skip!();
    app.enable_mouse_reporting_for_test();
    assert_eq!(app.left_button_outcome_for_test(true), "report");
    assert!(app.report_button_for_test().is_some());

    app.on_window_focus_changed_for_test(false);
    assert_eq!(app.report_button_for_test(), None);
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn tab_switch_clears_stale_pointer_cell() {
    let (mut app, _event_loop) = app_or_skip!();
    app.new_tab_for_test();
    // Seed a hover cell, then switch: the incoming session must not carry it (no
    // phantom hover / stale Ctrl+click target before the first real move).
    app.set_pointer_cell_for_test(5, 10);
    assert!(app.pointer_cell_for_test().is_some());
    app.switch_to_next_tab_for_test();
    assert!(
        app.pointer_cell_for_test().is_none(),
        "the incoming session starts with no stale pointer cell"
    );
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn switch_drops_in_flight_ime_preedit() {
    let (mut app, _event_loop) = app_or_skip!();
    app.new_tab_for_test();
    app.set_ime_preedit_for_test("中");
    assert_eq!(app.ime_preedit_for_test(), "中");
    app.switch_to_next_tab_for_test();
    assert_eq!(
        app.ime_preedit_for_test(),
        "",
        "an in-flight composition does not survive a switch to the new surface"
    );
}

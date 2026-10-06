// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored pointer and tab ownership regression fixtures.
use super::*;
use crate::native::pty::UserEvent;
use crate::native::session::SessionToken;

fn app() -> App {
    let settings = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    let (mut app, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        settings,
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    app
}

#[test]
fn outside_tab_release_requests_a_new_window_through_the_real_pointer_path() {
    let mut app = app();
    let token = app.active_session_token_for_test();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-60.0, -80.0);
    app.mouse_left_release_for_test();
    assert!(
        app.take_move_request().is_some(),
        "outside release must request reparenting"
    );
    assert_eq!(
        app.active_session_token_for_test(),
        token,
        "request does not respawn or detach"
    );
    assert_eq!(app.top_tab_drag_for_test(), None);
}

#[test]
fn returning_to_the_strip_keeps_reorder_and_never_requests_tear_out() {
    let mut app = app();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-60.0, -80.0);
    app.pointer_move_for_test(20.0, 8.0);
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
}

#[test]
fn the_real_pointer_badge_rekeys_and_escape_cancels_without_a_move() {
    let mut app = app();
    assert!(!app.tear_out_visual_for_test().0);
    let epoch = app.presentation_epoch_for_test();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(655.0, 8.0);
    assert!(
        !app.tear_out_visual_for_test().0,
        "not yet beyond threshold"
    );
    app.pointer_move_for_test(656.0, 8.0);
    let (signature, glyphs) = app.tear_out_visual_for_test();
    assert!(signature);
    assert!(glyphs.contains("New window"));
    assert_ne!(app.presentation_epoch_for_test(), epoch);
    app.drive_named_key_for_test(NamedKey::Escape);
    assert!(!app.tear_out_visual_for_test().0);
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
}

#[test]
fn focus_loss_and_unmatched_release_cancel_the_real_pointer_gesture() {
    let mut app = app();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-16.0, 8.0);
    assert!(app.tear_out_visual_for_test().0);
    app.on_window_focus_changed_for_test(false);
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
    assert!(!app.tear_out_visual_for_test().0);
}

#[test]
fn a_surface_resize_revalidates_the_outside_candidate_before_release() {
    let mut app = app();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(700.0, 8.0);
    assert!(app.tear_out_visual_for_test().0);
    app.set_test_surface_for_test(800, 384, WindowPadding::ZERO);
    app.mouse_left_release_for_test();
    assert!(
        app.take_move_request().is_none(),
        "the pointer is now inside the resized surface"
    );
    assert!(!app.tear_out_visual_for_test().0);
}

#[test]
fn requesting_an_inactive_tab_tear_out_keeps_the_sources_active_identity() {
    let mut app = app();
    let original = app.active_session_token_for_test();
    app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        crate::native::test_support::headless_writer(),
        Dimensions::new(80, 24),
    );
    assert_eq!(app.active_session_token_for_test(), original);
    let x = (0..640)
        .find(|x| {
            app.top_chrome_geometry_probe_for_test(f64::from(*x), 8.0, 1)
                .is_some_and(|(index, _, _, _)| index == 1)
        })
        .expect("second tab slot");
    app.set_pointer_px_for_test(f64::from(x) + 1.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-40.0, -40.0);
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_some());
    assert_eq!(
        app.active_session_token_for_test(),
        original,
        "queuing an inactive tab move must not activate it in the source"
    );
}

fn two_tab_app() -> (App, SessionToken, SessionToken) {
    let mut app = app();
    let first = app.active_session_token_for_test();
    app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        crate::native::test_support::headless_writer(),
        Dimensions::new(80, 24),
    );
    let tokens = app.tab_tokens_for_test();
    assert_eq!(tokens.len(), 2);
    (app, first, tokens[1])
}

fn tab_slot_x(app: &App, index: usize) -> f64 {
    let x = (0..640)
        .find(|x| {
            app.top_chrome_geometry_probe_for_test(f64::from(*x), 8.0, 1)
                .is_some_and(|(hit, _, _, _)| hit == index)
        })
        .expect("tab slot");
    f64::from(x) + 1.0
}

/// Fails before the fix: the close confirmation left the gesture live, so its
/// badge stayed on screen and the next click anywhere was swallowed as the
/// stale drag's release, reordering the tab.
#[test]
fn a_close_confirmation_mid_drag_cancels_the_gesture_and_a_later_click_moves_nothing() {
    let (mut app, first, second) = two_tab_app();
    app.set_foreground_jobs_running_for_test();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(tab_slot_x(&app, 1) + 200.0, 8.0);
    assert_eq!(app.top_tab_drag_for_test().map(|drag| drag.0), Some(true));
    app.pointer_move_for_test(-60.0, -80.0);
    assert!(app.tear_out_visual_for_test().0, "armed outside the window");
    app.request_window_close_for_test();
    assert!(app.confirm_close_open_for_test());
    assert_eq!(
        app.top_tab_drag_for_test(),
        None,
        "the dialog ends the drag"
    );
    let (badge, glyphs) = app.tear_out_visual_for_test();
    assert!(!badge && !glyphs.contains("New window"), "no stuck badge");
    app.mouse_left_release_for_test();
    app.close_overlay_for_test();
    app.pointer_move_for_test(300.0, 200.0);
    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
    assert_eq!(app.tab_tokens_for_test(), vec![first, second]);
}

/// Sibling of the tab gesture: the workspace rail drag ends on the same close
/// confirmation instead of reordering workspaces on the next click.
#[test]
fn a_close_confirmation_mid_drag_cancels_a_workspace_rail_drag() {
    let mut app = app();
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    for _ in 0..2 {
        app.push_headless_workspace_for_test(
            Arc::new(Mutex::new(Terminal::new(80, 24))),
            crate::native::test_support::headless_writer(),
            Dimensions::new(80, 24),
        );
    }
    app.set_foreground_jobs_running_for_test();
    let before = app.active_workspace_index_for_test();
    app.set_pointer_px_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(12.0, 140.0);
    assert_eq!(app.rail_ws_drag_for_test().map(|drag| drag.0), Some(true));
    app.request_window_close_for_test();
    assert!(app.confirm_close_open_for_test());
    assert_eq!(
        app.rail_ws_drag_for_test(),
        None,
        "the dialog ends the drag"
    );
    app.mouse_left_release_for_test();
    app.close_overlay_for_test();
    app.pointer_move_for_test(400.0, 200.0);
    app.mouse_left_press_for_test();
    app.mouse_left_release_for_test();
    assert_eq!(app.active_workspace_index_for_test(), before);
    assert_eq!(app.rail_ws_drag_for_test(), None);
}

#[test]
fn the_dragged_tab_exiting_mid_drag_ends_the_gesture_without_a_move() {
    let (mut app, first, second) = two_tab_app();
    app.set_pointer_px_for_test(tab_slot_x(&app, 1), 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-40.0, -40.0);
    assert!(app.tear_out_visual_for_test().0);
    app.clear_needs_rebuild_for_test();
    let _ = app.dispatch_user_event_for_test(UserEvent::ShellExited { session: second });
    assert_eq!(app.top_tab_drag_for_test(), None);
    assert!(!app.tear_out_visual_for_test().0, "no stuck badge");
    assert!(app.needs_rebuild_for_test(), "the cleared badge repaints");
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
    assert_eq!(app.tab_tokens_for_test(), vec![first]);
}

#[test]
fn another_tab_exiting_mid_drag_ends_the_gesture_without_a_stale_reorder() {
    let (mut app, first, second) = two_tab_app();
    app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        crate::native::test_support::headless_writer(),
        Dimensions::new(80, 24),
    );
    let third = app.tab_tokens_for_test()[2];
    app.set_pointer_px_for_test(tab_slot_x(&app, 2), 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(2.0, 8.0);
    assert_eq!(
        app.top_tab_drag_for_test(),
        Some((true, 1)),
        "armed, inserting before the middle tab"
    );
    app.clear_needs_rebuild_for_test();
    let _ = app.dispatch_user_event_for_test(UserEvent::ShellExited { session: first });
    assert_eq!(app.top_tab_drag_for_test(), None);
    assert!(app.needs_rebuild_for_test());
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
    assert_eq!(app.tab_tokens_for_test(), vec![second, third]);
}

/// Fails before the fix: any overlay that opens mid-drag without the pointer
/// reset kept the gesture and its badge alive after taking the release.
#[test]
fn an_overlay_taking_the_release_ends_the_gesture_and_its_badge() {
    let (mut app, first, second) = two_tab_app();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-60.0, -80.0);
    assert!(app.tear_out_visual_for_test().0);
    app.open_confirm_close_for_test();
    let epoch = app.presentation_epoch_for_test();
    app.mouse_left_release_for_test();
    assert_eq!(app.top_tab_drag_for_test(), None);
    assert!(!app.tear_out_visual_for_test().0, "no stuck badge");
    assert_ne!(
        app.presentation_epoch_for_test(),
        epoch,
        "the badge repaints away"
    );
    assert!(app.take_move_request().is_none());
    assert_eq!(app.tab_tokens_for_test(), vec![first, second]);
}

#[test]
fn a_live_tab_drag_requests_a_provisional_window_before_release() {
    let mut app = app();
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-60.0, -80.0);
    assert!(
        app.take_move_request().is_some(),
        "live follow must open before release"
    );
    app.drive_named_key_for_test(NamedKey::Escape);
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
}

#[test]
fn live_tab_drag_off_keeps_the_release_time_pointer_path() {
    let settings = Settings {
        always_show_tab_bar: true,
        live_tab_drag: false,
        ..Settings::default()
    };
    let (mut app, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        settings,
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-60.0, -80.0);
    assert!(app.take_move_request().is_none());
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn hyprland_follow_arms_through_wayland_pointer_input_before_release() {
    let _lock = crate::test_lock::test_env_lock();
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", value),
                    None => std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE"),
                }
            }
        }
    }
    let _restore = Restore(std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE"));
    unsafe {
        std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", "fixture");
    }
    let mut app = app();
    app.set_wayland_surface_present_for_test(true);
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(-60.0, -80.0);
    assert!(
        app.take_move_request().is_some(),
        "Hyprland follow must arm before release"
    );
    app.drive_named_key_for_test(NamedKey::Escape);
}

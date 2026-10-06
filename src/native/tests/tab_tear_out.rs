// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored pointer and tab ownership regression fixtures.
use super::*;

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

// SPDX-License-Identifier: GPL-3.0-only
//! Pane ownership, focus policy, and restoration defaults for notifications.

use super::*;

#[test]
fn background_notification_stays_with_its_workspace_until_viewed() {
    let dims = Dimensions::new(40, 8);
    let (mut app, _) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    let background_terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let background_ws = app.push_headless_workspace_for_test(
        background_terminal.clone(),
        crate::native::test_support::headless_writer(),
        dims,
    );
    let background_token = app.focused_pane_id_for_test();
    app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);

    background_terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]9;finished\x07");
    let (_, background_request, _) = app.drain_all_notifications_for_test(Instant::now(), true);
    assert!(background_request);
    assert!(app.pane_attention_for_test(background_token).0);
    assert!(app.workspace_activity_for_test(background_ws));

    app.dispatch_workspace_action_for_test(BindableAction::NextWorkspace);
    app.drain_all_notifications_for_test(Instant::now(), true);
    assert!(!app.pane_attention_for_test(background_token).0);
    assert!(!app.workspace_activity_for_test(background_ws));
}

#[test]
fn focused_window_policy_distinguishes_visible_from_unfocused_requests() {
    let (mut app, terminal) = headless_app_for_test();
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]777;notify;build;complete\x07");
    let (_, background_request, _) = app.drain_all_notifications_for_test(Instant::now(), false);
    assert!(background_request);
}

#[test]
fn fresh_session_restores_no_transient_attention_state() {
    let (app, _) = headless_app_for_test();
    let token = app.active_session_id_for_test();
    assert_eq!(
        app.pane_attention_for_test(token),
        (false, false, false, None)
    );
}

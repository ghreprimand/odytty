// SPDX-License-Identifier: GPL-3.0-only
//! Reopening the last closed navigator item puts its title on the tab the
//! relaunch created, never on whichever tab is active, and a relaunch that
//! fails keeps the item so it can be retried.

use super::*;
use crate::native::session::SessionToken;
use crate::native::test_support::{headless_app_with_writer, headless_writer};

fn app_with_a_closed_tab() -> (App, SessionToken) {
    let (mut app, _terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        headless_writer(),
    );
    let survivor = app.active_session_token_for_test();
    let position = app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        headless_writer(),
        Dimensions::new(80, 24),
    );
    app.set_session_title_override_for_test(position, Some("logs"));
    let closed = app
        .session_token_at_position_for_test(position)
        .expect("tab");
    assert!(app.switch_to_session_for_test(position));
    // Closing a tab records it in the navigator's recently-closed ring.
    let _ = app.close_active_tab_for_test();
    assert_eq!(
        app.session_token_at_position_for_test(1),
        None,
        "{closed:?} closed"
    );
    assert_eq!(app.navigator_recently_closed_len_for_test(), 1);
    assert_eq!(app.active_session_token_for_test(), survivor);
    (app, survivor)
}

#[test]
fn a_failed_reopen_keeps_the_item_and_leaves_the_active_tab_title_alone() {
    let (mut app, survivor) = app_with_a_closed_tab();
    let before = app.session_tab_title_for_test(0);
    assert_eq!(app.reopen_last_closed_with_launch_for_test(false), None);
    assert_eq!(app.active_session_token_for_test(), survivor);
    assert_eq!(
        app.session_tab_title_for_test(0),
        before,
        "the surviving tab keeps its own title"
    );
    assert_eq!(
        app.navigator_recently_closed_len_for_test(),
        1,
        "the closed item stays for a retry"
    );
}

#[test]
fn a_successful_reopen_titles_the_new_tab() {
    let (mut app, _survivor) = app_with_a_closed_tab();
    let token = app
        .reopen_last_closed_with_launch_for_test(true)
        .expect("launched");
    assert_eq!(app.session_token_at_position_for_test(1), Some(token));
    assert_eq!(app.session_tab_title_for_test(1).as_deref(), Some("logs"));
    assert_eq!(app.navigator_recently_closed_len_for_test(), 0);
}

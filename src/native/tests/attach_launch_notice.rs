// SPDX-License-Identifier: GPL-3.0-only
//! `odytty attach <id>` against a session that cannot be attached opens the
//! ordinary local tab and says so in a notice, instead of only logging.

use super::*;

#[test]
fn a_failed_launch_attach_raises_a_notice_naming_the_session() {
    let (mut app, _terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    let tabs = app.active_workspace_tab_count_for_test();
    // A control character in the id is not echoed into the notice.
    app.attach_launch_session("s-odytty-test-no-such-session\u{1b}[31m");
    let notice = app.open_notice_message_for_test().unwrap_or_default();
    assert!(
        notice.starts_with("Could not attach session s-odytty-test-no-such-session[31m: "),
        "got {notice:?}"
    );
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        tabs,
        "the local tab stays the only tab"
    );
}

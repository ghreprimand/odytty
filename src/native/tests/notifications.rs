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
    let background_token = app.active_session_token_for_test();
    app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);

    background_terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]9;finished\x07");
    let (_, background_request, _) = app.drain_all_notifications_for_test(Instant::now(), true);
    assert!(background_request);
    assert!(
        app.pane_attention_for_test(background_token)
            .expect("live attention owner")
            .0
    );
    assert!(app.workspace_activity_for_test(background_ws));

    app.dispatch_workspace_action_for_test(BindableAction::NextWorkspace);
    app.drain_all_notifications_for_test(Instant::now(), true);
    assert!(
        !app.pane_attention_for_test(background_token)
            .expect("live attention owner")
            .0
    );
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
    let token = app.active_session_token_for_test();
    assert_eq!(
        app.pane_attention_for_test(token),
        Some((false, false, false, None))
    );
}

/// The pane attention badge replaces the top-right cell outright: a wide
/// glyph's tail, a combining mark, or inverse and invisible output under it
/// never changes how the badge draws, and a covered wide lead is blanked.
#[test]
fn attention_badge_replaces_the_cell_under_it() {
    let dims = Dimensions::new(6, 2);
    let (app, _) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    let snapshot_of = |bytes: &[u8]| {
        let mut terminal = Terminal::new(dims.columns, dims.rows);
        terminal.advance(bytes);
        terminal.screen().snapshot()
    };
    let cases: [(&str, &[u8]); 3] = [
        ("wide tail", "abcd\u{4e00}".as_bytes()),
        ("combining mark", "abcde\u{301}".as_bytes()),
        ("inverse invisible", b"abcde\x1b[7;8;4mx"),
    ];
    for (name, bytes) in cases {
        let mut snapshot = snapshot_of(bytes);
        app.paint_pane_attention_cell(&mut snapshot, None, true, false, false);
        let badge = snapshot.cells[5];
        assert_eq!(badge.ch, '\u{2022}', "{name}: badge glyph");
        assert!(!badge.wide_continuation, "{name}: no wide tail");
        assert!(badge.combining().is_empty(), "{name}: no inherited marks");
        assert!(
            !badge.attrs.inverse() && !badge.attrs.hidden() && !badge.attrs.underline(),
            "{name}: no inherited display attributes"
        );
        assert!(badge.attrs.bold(), "{name}: badge face");
    }
    let mut wide = snapshot_of("abcd\u{4e00}".as_bytes());
    app.paint_pane_attention_cell(&mut wide, None, true, false, false);
    assert_eq!(wide.cells[4].ch, ' ', "the covered wide lead is blanked");
}

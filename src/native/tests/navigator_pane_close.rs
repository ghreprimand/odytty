// SPDX-License-Identifier: GPL-3.0-only
//! Navigator pane-close regressions (acceptance: operator x on a
//! pane must not kill sibling panes).
//!
//! Desired contract for `close_navigator_target` after confirm:
//! - `NavigatorTarget::Live` closes only that pane; siblings in the same tab
//!   survive and the tab remains.
//! - `NavigatorTarget::Tab` still closes the whole tab (all leaves).
//! - A stale / unknown token is a no-op (does not close the focused tab).
//! - `NavigatorCloseCanceled` mutates no sessions.
//!
//! `close_navigator_target` keeps the `Live` and `Tab` arms separate so a pane
//! close and a tab close no longer share `close_active_tab`.

use std::sync::{Arc, Mutex};

use super::*;
use crate::core::{Dimensions, Terminal};
use crate::native::overlay::OverlayOutcome;
use crate::native::session::SessionToken;
use crate::native::session_navigator::NavigatorTarget;
use crate::settings::Settings;

fn headless_pair(dims: Dimensions) -> (Arc<Mutex<Terminal>>, crate::native::pty::PtyWriter) {
    (
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        crate::native::test_support::headless_writer(),
    )
}

/// One multi-pane tab (pane_a + pane_b) plus a second single-pane tab so a
/// Tab-close is not an app-exit signal.
fn app_with_split_and_extra_tab() -> (App, SessionToken, SessionToken, SessionToken) {
    let dims = Dimensions::new(80, 24);
    let (mut app, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        dims,
        Settings::default(),
    );
    let pane_a = SessionToken(0);
    let (t_b, w_b) = headless_pair(dims);
    let pane_b = SessionToken(app.seed_headless_split_pane_for_test(true, t_b, w_b, dims) as u64);
    let (t2, w2) = headless_pair(dims);
    let before = app.session_count_for_test();
    let _ = app.push_headless_session_for_test(t2, w2, dims);
    assert_eq!(app.session_count_for_test(), before + 1);
    // Tokens are allocated 0 (seed), 1 (split), 2 (pushed tab).
    let second_tab = SessionToken(2);
    assert!(app.session_exists_for_test(second_tab));
    // Focus the multi-pane tab (pane_a) by token.
    app.focus_session_token_for_test(pane_a);
    assert_eq!(
        SessionToken(app.focused_pane_id_for_test() as u64),
        pane_a,
        "focused pane_a of the split tab"
    );
    assert_eq!(
        app.active_tab_pane_tokens_for_test(),
        vec![pane_a, pane_b],
        "precondition: multi-pane tab leaves"
    );
    assert_eq!(app.active_workspace_tab_count_for_test(), 2);
    (app, pane_a, pane_b, second_tab)
}

#[test]
fn navigator_live_close_preserves_sibling_panes() {
    let (mut app, pane_a, pane_b, _second) = app_with_split_and_extra_tab();
    let before_tabs = app.active_workspace_tab_count_for_test();
    let before_sessions = app.session_count_for_test();

    app.close_navigator_target_for_test(NavigatorTarget::Live(pane_b));

    assert!(
        app.session_exists_for_test(pane_a),
        "sibling pane must survive a Live navigator close"
    );
    assert!(
        !app.session_exists_for_test(pane_b),
        "the targeted Live pane must be closed"
    );
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        before_tabs,
        "Live close must keep the tab (collapse split, not remove tab)"
    );
    assert_eq!(
        app.session_count_for_test(),
        before_sessions - 1,
        "exactly one pane session reaped"
    );
    assert_eq!(
        app.active_pane_count_for_test(),
        1,
        "split collapses to a single surviving pane"
    );
}

#[test]
fn navigator_tab_close_still_reaps_every_pane_in_the_tab() {
    let (mut app, pane_a, pane_b, second_tab) = app_with_split_and_extra_tab();
    let before_tabs = app.active_workspace_tab_count_for_test();

    app.close_navigator_target_for_test(NavigatorTarget::Tab(pane_a));

    assert!(
        !app.session_exists_for_test(pane_a) && !app.session_exists_for_test(pane_b),
        "Tab close must reap every leaf of the targeted tab"
    );
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        before_tabs - 1,
        "Tab close removes the whole tab"
    );
    assert!(
        app.session_exists_for_test(second_tab),
        "the other tab must remain"
    );
}

#[test]
fn navigator_stale_live_token_is_harmless() {
    let (mut app, pane_a, pane_b, second_tab) = app_with_split_and_extra_tab();
    let before_tabs = app.active_workspace_tab_count_for_test();
    let before_sessions = app.session_count_for_test();
    let before_focus = app.focused_pane_id_for_test();
    let stale = SessionToken(9_001);

    app.close_navigator_target_for_test(NavigatorTarget::Live(stale));

    assert_eq!(app.active_workspace_tab_count_for_test(), before_tabs);
    assert_eq!(app.session_count_for_test(), before_sessions);
    assert_eq!(app.focused_pane_id_for_test(), before_focus);
    assert!(app.session_exists_for_test(pane_a));
    assert!(app.session_exists_for_test(pane_b));
    assert!(app.session_exists_for_test(second_tab));
}

#[test]
fn navigator_close_cancel_mutates_no_sessions() {
    let (mut app, pane_a, pane_b, second_tab) = app_with_split_and_extra_tab();
    let before_tabs = app.active_workspace_tab_count_for_test();
    let before_sessions = app.session_count_for_test();
    let target = NavigatorTarget::Live(pane_b);

    app.apply_overlay_outcome_for_test(OverlayOutcome::NavigatorCloseCanceled(target));

    assert_eq!(app.active_workspace_tab_count_for_test(), before_tabs);
    assert_eq!(app.session_count_for_test(), before_sessions);
    assert!(app.session_exists_for_test(pane_a));
    assert!(app.session_exists_for_test(pane_b));
    assert!(app.session_exists_for_test(second_tab));
}

/// Menu Close emits NavigatorCloseRequest; App opens confirm; confirmed
/// Live close reaps only that pane on a split+extra-tab fixture.
#[test]
fn navigator_close_request_confirm_live_closes_only_that_pane() {
    let (mut app, pane_a, pane_b, second_tab) = app_with_split_and_extra_tab();
    let before_tabs = app.active_workspace_tab_count_for_test();
    let before_sessions = app.session_count_for_test();

    app.apply_overlay_outcome_for_test(OverlayOutcome::NavigatorCloseRequest(
        NavigatorTarget::Live(pane_b),
    ));
    assert!(
        app.overlay_open_for_test(),
        "CloseRequest must open the confirm card"
    );
    assert_eq!(
        app.overlay_signature_for_test().mode,
        crate::native::overlay::OverlayMode::ConfirmNavigatorClose
    );

    app.apply_overlay_outcome_for_test(OverlayOutcome::NavigatorCloseConfirmed(
        NavigatorTarget::Live(pane_b),
    ));

    assert!(app.session_exists_for_test(pane_a));
    assert!(!app.session_exists_for_test(pane_b));
    assert!(app.session_exists_for_test(second_tab));
    assert_eq!(app.active_workspace_tab_count_for_test(), before_tabs);
    assert_eq!(app.session_count_for_test(), before_sessions - 1);
}

/// Cancel arm: CloseRequest then NavigatorCloseCanceled mutates nothing.
#[test]
fn navigator_close_request_then_cancel_mutates_no_sessions() {
    let (mut app, pane_a, pane_b, second_tab) = app_with_split_and_extra_tab();
    let before_tabs = app.active_workspace_tab_count_for_test();
    let before_sessions = app.session_count_for_test();
    let target = NavigatorTarget::Live(pane_b);

    app.apply_overlay_outcome_for_test(OverlayOutcome::NavigatorCloseRequest(target.clone()));
    app.apply_overlay_outcome_for_test(OverlayOutcome::NavigatorCloseCanceled(target));

    assert_eq!(app.active_workspace_tab_count_for_test(), before_tabs);
    assert_eq!(app.session_count_for_test(), before_sessions);
    assert!(app.session_exists_for_test(pane_a));
    assert!(app.session_exists_for_test(pane_b));
    assert!(app.session_exists_for_test(second_tab));
}

/// A menu-captured Focus/Close on a token that no longer exists is a no-op.
#[test]
fn navigator_stale_menu_focus_and_close_confirmed_are_harmless() {
    let (mut app, pane_a, pane_b, second_tab) = app_with_split_and_extra_tab();
    let before_tabs = app.active_workspace_tab_count_for_test();
    let before_sessions = app.session_count_for_test();
    let before_focus = app.focused_pane_id_for_test();
    let stale = SessionToken(9_002);

    app.apply_overlay_outcome_for_test(OverlayOutcome::FocusSession(stale));
    assert_eq!(app.focused_pane_id_for_test(), before_focus);
    assert_eq!(app.session_count_for_test(), before_sessions);

    app.apply_overlay_outcome_for_test(OverlayOutcome::NavigatorCloseConfirmed(
        NavigatorTarget::Live(stale),
    ));
    assert_eq!(app.active_workspace_tab_count_for_test(), before_tabs);
    assert_eq!(app.session_count_for_test(), before_sessions);
    assert_eq!(app.focused_pane_id_for_test(), before_focus);
    assert!(app.session_exists_for_test(pane_a));
    assert!(app.session_exists_for_test(pane_b));
    assert!(app.session_exists_for_test(second_tab));
}

#[test]
fn navigator_tab_close_resolves_after_its_original_pane_has_closed() {
    let (mut app, pane_a, pane_b, other) = app_with_split_and_extra_tab();
    app.close_navigator_target_for_test(NavigatorTarget::Live(pane_a));
    app.focus_session_token_for_test(other);
    app.close_navigator_target_for_test(NavigatorTarget::Tab(pane_a));
    assert!(!app.session_exists_for_test(pane_b));
    assert!(app.session_exists_for_test(other));
}

#[test]
fn navigator_structural_focus_resolves_the_surviving_pane() {
    use crate::native::session_navigator::NavigatorAction;
    let (mut app, pane_a, pane_b, other) = app_with_split_and_extra_tab();
    app.close_navigator_target_for_test(NavigatorTarget::Live(pane_a));
    for target in [
        NavigatorTarget::Tab(pane_a),
        NavigatorTarget::Workspace(pane_a),
    ] {
        app.focus_session_token_for_test(pane_b);
        app.focus_session_token_for_test(other);
        app.apply_overlay_outcome_for_test(OverlayOutcome::NavigatorAction(
            NavigatorAction::Focus(target.clone()),
        ));
        // A workspace focuses its currently active tab; a tab finds its own
        // surviving pane even while another tab is active.
        let expected = if matches!(target, NavigatorTarget::Workspace(_)) {
            other
        } else {
            pane_b
        };
        assert_eq!(app.focused_pane_id_for_test() as u64, expected.0);
    }
}

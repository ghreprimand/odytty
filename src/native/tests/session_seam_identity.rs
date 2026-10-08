// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored session lookup regression fixtures.
use super::*;

#[test]
fn position_reads_and_writes_follow_the_active_workspace() {
    let (mut app, _) = headless_app_for_test();
    let dims = Dimensions::new(37, 9);
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_workspace_for_test(
        terminal,
        super::super::test_support::headless_writer(),
        dims,
    );
    app.advance_tab_bytes_at_position_for_test(0, b"workspace-b");
    assert!(
        app.tab_plain_text_at_position_for_test(0)
            .expect("live tab")
            .contains("workspace-b")
    );
}

#[test]
fn dirty_read_observes_the_same_workspace_as_its_setter() {
    let (mut app, _) = headless_app_for_test();
    let dims = Dimensions::new(37, 9);
    app.push_headless_workspace_for_test(
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        super::super::test_support::headless_writer(),
        dims,
    );
    app.set_tab_needs_rebuild_at_position_for_test(0, false);
    assert_eq!(app.tab_needs_rebuild_at_position_for_test(0), Some(false));
}

#[test]
fn tab_position_reads_skip_unfocused_split_leaves() {
    let (mut app, _) = headless_app_for_test();
    let dims = Dimensions::new(37, 9);
    app.seed_headless_split_pane_for_test(
        true,
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        super::super::test_support::headless_writer(),
        dims,
    );
    app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        super::super::test_support::headless_writer(),
        dims,
    );
    app.advance_tab_bytes_at_position_for_test(1, b"second-tab");
    assert!(
        app.tab_plain_text_at_position_for_test(1)
            .expect("second tab")
            .contains("second-tab")
    );
}

#[test]
fn absent_attention_owner_is_not_a_clean_pane() {
    let (app, _) = headless_app_for_test();
    assert_eq!(
        app.pane_attention_for_test(crate::native::session::SessionToken(u64::MAX)),
        None
    );
}

#[test]
fn all_position_observers_resolve_the_same_focused_pane() {
    let (mut app, _) = headless_app_for_test();
    let dims = Dimensions::new(37, 9);
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_workspace_for_test(
        terminal.clone(),
        super::super::test_support::headless_writer(),
        dims,
    );
    let sibling = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.seed_headless_split_pane_for_test(
        true,
        sibling.clone(),
        super::super::test_support::headless_writer(),
        dims,
    );
    // Explicitly select the second leaf and prove tab positions resolve the
    // focused pane rather than an earlier leaf in layout order.
    let sibling_token = *app
        .active_tab_pane_tokens_for_test()
        .last()
        .expect("split leaf");
    app.focus_session_token_for_test(sibling_token);
    app.advance_tab_bytes_at_position_for_test(0, b"focused-leaf\x1b]10;#123456\x07");
    let expected_colors = {
        let terminal = sibling.lock().expect("sibling terminal");
        let colors = terminal.dynamic_colors();
        (colors.foreground, colors.background)
    };
    assert_eq!(
        app.tab_dynamic_colors_at_position_for_test(0),
        Some(expected_colors)
    );
    assert_eq!(
        app.tab_dimensions_at_position_for_test(0),
        Some(sibling.lock().expect("terminal").screen().dimensions())
    );
    assert!(
        app.tab_plain_text_at_position_for_test(0)
            .expect("live tab")
            .contains("focused-leaf")
    );
    assert!(
        !terminal
            .lock()
            .expect("original leaf")
            .screen()
            .plain_text()
            .contains("focused-leaf")
    );
    let answer = app
        .tab_osc_answer_at_position_for_test(0, b"\x1b]10;?\x07")
        .expect("live OSC owner");
    assert!(
        String::from_utf8(answer)
            .expect("ASCII OSC answer")
            .contains("1212/3434/5656")
    );
    app.set_tab_needs_rebuild_at_position_for_test(0, false);
    assert_eq!(app.tab_needs_rebuild_at_position_for_test(0), Some(false));
    assert_eq!(app.pane_needs_rebuild_for_test(sibling_token), Some(false));
    assert_eq!(app.tab_plain_text_at_position_for_test(1), None);
    assert_eq!(app.tab_dimensions_at_position_for_test(1), None);
    assert_eq!(app.tab_dynamic_colors_at_position_for_test(1), None);
    assert_eq!(
        app.tab_osc_answer_at_position_for_test(1, b"\x1b]10;?\x07"),
        None
    );
    assert_eq!(app.tab_needs_rebuild_at_position_for_test(1), None);
    #[cfg(unix)]
    assert_eq!(app.tab_pty_dimensions_at_position_for_test(1), None);
}

#[test]
fn stable_profile_and_attention_observers_survive_reorder_and_reject_closed_panes() {
    let (mut app, _) = headless_app_for_test();
    let first = app.active_session_token_for_test();
    let dims = Dimensions::new(37, 9);
    app.push_headless_workspace_for_test(
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        super::super::test_support::headless_writer(),
        dims,
    );
    let second = app.active_session_token_for_test();
    app.move_workspace_at_for_test(1, true);
    assert_eq!(app.all_session_tokens_for_test(), vec![second, first]);
    assert_eq!(app.pane_launch_profile_for_test(first), Some(None));
    assert_eq!(
        app.pane_attention_for_test(first),
        Some((false, false, false, None))
    );
    app.focus_session_token_for_test(first);
    assert!(
        !app.close_active_tab_for_test(),
        "a surviving pane keeps the window open"
    );
    assert!(!app.session_exists_for_test(first));
    assert_eq!(app.pane_launch_profile_for_test(first), None);
    assert_eq!(app.pane_attention_for_test(first), None);
    assert_eq!(app.pane_launch_profile_for_test(second), Some(None));
    assert_eq!(
        app.pane_attention_for_test(second),
        Some((false, false, false, None))
    );
}

#[test]
fn tab_position_observers_follow_tab_reorder_and_close() {
    let (mut app, _) = headless_app_for_test();
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    let first = app.active_session_token_for_test();
    let dims = Dimensions::new(37, 9);
    app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows))),
        super::super::test_support::headless_writer(),
        dims,
    );
    let second = app
        .session_token_at_position_for_test(1)
        .expect("second tab");
    app.advance_tab_bytes_at_position_for_test(0, b"first-tab");
    app.advance_tab_bytes_at_position_for_test(1, b"second-tab");
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(500.0, 8.0);
    app.mouse_left_release_for_test();
    assert_eq!(app.session_token_at_position_for_test(0), Some(second));
    assert_eq!(app.session_token_at_position_for_test(1), Some(first));
    assert!(
        app.tab_plain_text_at_position_for_test(0)
            .expect("reordered tab")
            .contains("second-tab")
    );
    app.focus_session_token_for_test(first);
    assert!(
        !app.close_active_tab_for_test(),
        "a surviving pane keeps the window open"
    );
    assert_eq!(app.session_token_at_position_for_test(0), Some(second));
    assert_eq!(app.tab_plain_text_at_position_for_test(1), None);
}

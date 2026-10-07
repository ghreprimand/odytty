// SPDX-License-Identifier: GPL-3.0-only
//! Command-palette rows whose ids carry an index resolve against the targets
//! captured when the palette opened, so a list that changes while the palette
//! is open never retargets a row at a neighbour, and a target that has gone is
//! refused.

use super::*;
use crate::native::test_support::{headless_app_with_writer, headless_writer};

fn headless_app() -> App {
    let (mut app, _terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        headless_writer(),
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    app
}

/// Three headless workspaces named `w0`, `w1`, `w2`, with `w0` active.
fn three_workspaces() -> App {
    let mut app = headless_app();
    for _ in 0..2 {
        app.push_headless_workspace_for_test(
            Arc::new(Mutex::new(Terminal::new(80, 24))),
            headless_writer(),
            Dimensions::new(80, 24),
        );
    }
    for idx in 0..3 {
        app.rename_workspace_for_test(idx, &format!("w{idx}"));
    }
    app.handle_palette_action_for_test("workspace-switch-0");
    assert_eq!(app.active_workspace_index_for_test(), 0);
    app
}

fn active_workspace_name(app: &App) -> String {
    app.workspace_names_for_test()[app.active_workspace_index_for_test()].clone()
}

#[test]
fn a_workspace_row_follows_its_workspace_after_the_rail_order_changes() {
    let mut app = three_workspaces();
    app.capture_palette_targets_for_test(&[], &[]);
    // The rail reorders while the palette is open: w0 moves behind w1.
    app.move_workspace_at_for_test(0, false);
    assert_eq!(app.workspace_names_for_test(), ["w1", "w0", "w2"]);
    // The row labelled for w1 when the palette opened carries index 1.
    app.accept_palette_action_for_test("workspace-switch-1");
    assert_eq!(active_workspace_name(&app), "w1");
}

#[test]
fn a_workspace_row_whose_workspace_closed_does_nothing() {
    let mut app = three_workspaces();
    app.capture_palette_targets_for_test(&[], &[]);
    app.close_workspace_at_for_test(1);
    assert_eq!(app.workspace_names_for_test(), ["w0", "w2"]);
    if active_workspace_name(&app) != "w0" {
        app.dispatch_workspace_action_for_test(BindableAction::PrevWorkspace);
    }
    assert_eq!(active_workspace_name(&app), "w0");
    // The row for the closed w1 must not reach w2, which now holds index 1.
    app.accept_palette_action_for_test("workspace-switch-1");
    assert_eq!(active_workspace_name(&app), "w0");
}

#[test]
fn a_pane_row_focuses_the_pane_it_named_after_a_pane_closes() {
    let mut app = headless_app();
    for _ in 0..2 {
        app.seed_headless_split_pane_for_test(
            true,
            Arc::new(Mutex::new(Terminal::new(40, 24))),
            headless_writer(),
            Dimensions::new(40, 24),
        );
    }
    let panes = app.active_tab_pane_tokens_for_test();
    assert_eq!(panes.len(), 3);
    // The test geometry seams are per session, so seed every pane's.
    for token in &panes {
        app.focus_session_token_for_test(*token);
        app.set_test_cell_for_test(cell(8, 16));
        app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    }
    app.reflow_active_panes_for_test();
    app.handle_palette_action_for_test("stack-panes");
    let order = app.active_tab_pane_tokens_for_test();
    app.focus_session_token_for_test(order[0]);
    assert_eq!(app.pane_focus_row_labels().len(), 3);
    app.capture_palette_targets_for_test(&[], &[]);
    // The first pane exits while the palette is open.
    app.close_focused_pane_for_test();
    app.focus_session_token_for_test(order[2]);
    // "Focus Pane 2 of 3" named the second pane, which is now first in order.
    app.accept_palette_action_for_test("pane-focus-1");
    assert_eq!(app.active_session_token_for_test(), order[1]);
}

#[test]
fn host_and_profile_rows_use_the_names_listed_when_the_palette_opened() {
    let mut app = three_workspaces();
    app.capture_palette_targets_for_test(&["alpha", "beta"], &["work", "play"]);
    app.accept_palette_action_for_test("workspace-bind-1");
    assert_eq!(
        app.active_workspace_binding_for_test().as_deref(),
        Some("beta")
    );
    app.capture_palette_targets_for_test(&["alpha", "beta"], &["work", "play"]);
    app.accept_palette_action_for_test("profile-bind-1");
    assert_eq!(
        app.active_workspace_launch_profile_for_test().as_deref(),
        Some("play")
    );
}

#[test]
fn workspace_rows_never_act_on_a_workspace_that_became_active_later() {
    for id in ["workspace-bind-0", "profile-bind-0", "workspace-rename"] {
        let mut app = three_workspaces();
        app.capture_palette_targets_for_test(&["alpha"], &["work"]);
        // The workspace the palette opened over closes; another becomes active.
        app.close_workspace_at_for_test(0);
        app.accept_palette_action_for_test(id);
        assert_eq!(app.active_workspace_binding_for_test(), None, "{id}");
        assert_eq!(app.active_workspace_launch_profile_for_test(), None, "{id}");
        assert!(!app.rename_overlay_open_for_test(), "{id}");
    }
}

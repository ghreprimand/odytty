// SPDX-License-Identifier: GPL-3.0-only
//! A background workspace's progress, unseen activity, and bound-profile
//! marker reach the workspace rail: each changes what the floating rail
//! paints, so its render cache repaints.

use super::*;
use crate::native::test_support::{headless_app_with_writer, headless_writer};

/// A revealed floating rail over two workspaces; returns the App and the
/// first (now background) workspace's terminal.
fn revealed_rail_app() -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, background) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        headless_writer(),
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 400, WindowPadding::ZERO);
    app.push_headless_workspace_for_test(
        Arc::new(Mutex::new(Terminal::new(80, 24))),
        headless_writer(),
        Dimensions::new(80, 24),
    );
    app.set_tab_bar_placement_for_test("left");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_autohide_for_test(true);
    app.force_rail_reveal_for_test();
    assert!(app.rail_overlay_visible_for_test());
    app.drain_progress_for_test();
    (app, background)
}

#[test]
fn background_workspace_progress_repaints_the_floating_rail() {
    let (mut app, background) = revealed_rail_app();
    let before = app.rail_overlay_content_hash_for_test();

    // The first workspace, now in the background, reports 50% progress.
    background
        .lock()
        .expect("terminal")
        .advance(b"\x1b]9;4;1;50\x07");
    app.drain_progress_for_test();

    assert_ne!(
        app.rail_overlay_content_hash_for_test(),
        before,
        "the progress rollup changes the floating rail's content"
    );
}

#[test]
fn background_workspace_activity_repaints_the_floating_rail() {
    let (mut app, background) = revealed_rail_app();
    let before = app.rail_overlay_content_hash_for_test();
    // A bell in the background workspace latches its unseen-activity badge.
    background.lock().expect("terminal").advance(b"\x07");
    let _ = app.drain_bells_for_test();
    assert!(app.workspace_activity_for_test(0), "activity latched");
    assert_ne!(
        app.rail_overlay_content_hash_for_test(),
        before,
        "the activity badge changes the floating rail's content"
    );
}

#[test]
fn binding_the_active_workspace_repaints_the_floating_rail() {
    let (mut app, _background) = revealed_rail_app();
    let before = app.rail_overlay_content_hash_for_test();
    app.set_workspace_binding_for_test(Some("alpha".to_owned()));
    assert_ne!(
        app.rail_overlay_content_hash_for_test(),
        before,
        "the bound marker changes the floating rail's content"
    );
}

#[test]
fn pressing_and_dragging_a_floating_rail_slot_repaints_the_floating_rail() {
    let (mut app, _background) = revealed_rail_app();
    // Settle hover over the first slot first, so the press is the only change.
    app.pointer_move_for_test(12.0, 24.0);
    let hovered = app.rail_overlay_content_hash_for_test();
    app.mouse_left_press_for_test();
    assert_eq!(
        app.rail_ws_drag_for_test(),
        Some((false, 0)),
        "the press lands on the floating rail's first slot"
    );
    assert_ne!(
        app.rail_overlay_content_hash_for_test(),
        hovered,
        "the pressed slot fill changes the floating rail's content"
    );
    let pressed = app.rail_overlay_content_hash_for_test();
    app.pointer_move_for_test(12.0, 60.0);
    assert_eq!(
        app.rail_ws_drag_for_test().map(|(armed, _)| armed),
        Some(true)
    );
    assert_ne!(
        app.rail_overlay_content_hash_for_test(),
        pressed,
        "the armed preview changes the floating rail's content"
    );
}

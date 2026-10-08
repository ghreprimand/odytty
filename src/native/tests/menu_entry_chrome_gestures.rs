// SPDX-License-Identifier: GPL-3.0-only
//! A context menu opened while a chrome gesture holds the left button ends
//! that gesture, through the real pointer path of a headless App: a top-tab
//! reorder (with live tear-out armed by default), a workspace-rail reorder,
//! and a tab-bar height seam drag. Motion with both buttons still held then
//! drives the menu, not the gesture: the order and height stay as they were,
//! no tear-out window is requested, one Escape closes the menu, and the later
//! left release changes nothing.

use super::*;
use crate::native::test_support::headless_writer;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};

fn tab_bar_app() -> App {
    let settings = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    let (mut app, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        settings,
    );
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    app
}

fn escape_closes_the_menu(app: &mut App) {
    assert!(app.context_menu_open_for_test(), "the menu is open");
    app.drive_named_key_for_test(NamedKey::Escape);
    assert!(
        !app.context_menu_open_for_test(),
        "one Escape closes the menu"
    );
}

#[test]
fn a_menu_opened_during_a_tab_drag_ends_it_without_reorder_or_tear_out() {
    let mut app = tab_bar_app();
    let dims = Dimensions::new(80, 24);
    let second = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_session_for_test(second, headless_writer(), dims);
    let order = app.tab_tokens_for_test();
    assert_eq!(order.len(), 2, "two tabs");

    app.set_pointer_px_for_test(12.0, 8.0);
    app.pointer_move_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(40.0, 8.0);
    assert_eq!(
        app.top_tab_drag_for_test().map(|(armed, _)| armed),
        Some(true),
        "the reorder gesture is armed"
    );
    app.mouse_right_press_for_test();
    assert!(
        app.context_menu_open_for_test(),
        "the right press opened a menu"
    );
    assert_eq!(app.top_tab_drag_for_test(), None, "the menu ended the drag");

    // Both buttons still held: leaving the window would arm a live tear-out
    // and moving along the strip would retarget the drop.
    app.pointer_move_for_test(-60.0, -80.0);
    app.pointer_move_for_test(500.0, 8.0);
    assert!(
        app.take_move_request().is_none(),
        "no tear-out window is requested under the menu"
    );
    assert_eq!(app.top_tab_drag_for_test(), None);

    escape_closes_the_menu(&mut app);
    app.mouse_left_release_for_test();
    assert!(app.take_move_request().is_none());
    assert_eq!(
        app.tab_tokens_for_test(),
        order,
        "the tab order is unchanged"
    );
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Default,
        "the drag cursor is gone"
    );
}

#[test]
fn a_menu_opened_during_a_workspace_drag_ends_it_without_reorder() {
    let (mut app, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    app.set_workspace_rail_for_test("left");
    app.set_tab_rail_width_manual_for_test(16);
    let dims = Dimensions::new(80, 24);
    for _ in 0..2 {
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        app.push_headless_workspace_for_test(terminal, headless_writer(), dims);
    }
    for (index, name) in ["a", "b", "c"].into_iter().enumerate() {
        app.rename_workspace_for_test(index, name);
    }
    let active = app.active_workspace_index_for_test();

    app.set_pointer_px_for_test(12.0, 24.0);
    app.pointer_move_for_test(12.0, 24.0);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(12.0, 60.0);
    assert_eq!(
        app.rail_ws_drag_for_test().map(|(armed, _)| armed),
        Some(true),
        "the rail reorder gesture is armed"
    );
    app.mouse_right_press_for_test();
    assert!(
        app.context_menu_open_for_test(),
        "the right press opened a menu"
    );
    assert_eq!(app.rail_ws_drag_for_test(), None, "the menu ended the drag");

    app.pointer_move_for_test(12.0, 140.0);
    assert_eq!(app.rail_ws_drag_for_test(), None);
    escape_closes_the_menu(&mut app);
    app.mouse_left_release_for_test();
    assert_eq!(app.workspace_names_for_test(), vec!["a", "b", "c"]);
    assert_eq!(app.active_workspace_index_for_test(), active);
}

#[test]
fn a_menu_opened_during_a_tab_bar_seam_drag_ends_it_without_resizing() {
    let base = crate::test_dirs::fresh_temp_dir("odytty-menu-seam-");
    super::config_env::with_config_base(&base, true, || {
        let mut app = tab_bar_app();
        let conf = base.join("odytty.conf");
        std::fs::write(&conf, "").expect("scratch config");
        app.set_config_path_for_test(conf.clone());
        app.set_tab_bar_height_manual_for_test(2);
        let seam = f64::from(app.tab_bar_seam_y_for_test().expect("a top bar"));
        // The pointer crossed the content on its way to the seam, so a menu
        // has a cell to open at.
        app.pointer_move_for_test(200.0, seam + 2.0 * f64::from(CELL.height));
        app.set_pointer_px_for_test(200.0, seam);
        app.mouse_left_press_for_test();
        assert!(app.tab_bar_seam_dragging_for_test(), "the seam is grabbed");
        let dragged = app.tab_bar_height_for_test();
        app.mouse_right_press_for_test();
        assert!(
            app.context_menu_open_for_test(),
            "the right press opened a menu"
        );
        assert!(
            !app.tab_bar_seam_dragging_for_test(),
            "the menu ended the seam drag"
        );
        app.pointer_move_for_test(200.0, seam + 3.0 * f64::from(CELL.height));
        assert_eq!(
            app.tab_bar_height_for_test(),
            dragged,
            "motion under the menu does not resize the bar"
        );
        escape_closes_the_menu(&mut app);
        app.mouse_left_release_for_test();
        assert_eq!(app.tab_bar_height_for_test(), dragged);
        assert_eq!(
            std::fs::read_to_string(&conf).expect("scratch config"),
            "",
            "the consumed release persists nothing"
        );
    });
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_right_press_that_opens_no_menu_leaves_the_tab_drag_alone() {
    let mut app = tab_bar_app();
    let dims = Dimensions::new(80, 24);
    let second = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.push_headless_session_for_test(second, headless_writer(), dims);
    // No pointer motion has resolved a cell yet, so a menu has nowhere to
    // open; the press must not end the gesture it interrupted.
    app.set_pointer_px_for_test(12.0, 8.0);
    app.mouse_left_press_for_test();
    app.set_pointer_px_for_test(40.0, 8.0);
    app.mouse_right_press_for_test();
    assert!(!app.context_menu_open_for_test(), "no menu opened");
    assert!(
        app.top_tab_drag_for_test().is_some(),
        "the tab drag is still held"
    );
    app.mouse_left_release_for_test();
    assert_eq!(app.top_tab_drag_for_test(), None, "its release ends it");
}

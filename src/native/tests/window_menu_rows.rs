// SPDX-License-Identifier: GPL-3.0-only
//! The v0.16 window, layout, export, and merge rows of the right-click menus,
//! driven through the production right-click path of a headless App: a tab
//! right-click, an empty-strip right-click, and a terminal-content right-click.
//!
//! Each row dispatches the command-palette row of the same name, so the tests
//! pin presence and absence per condition, that activating a row reaches the
//! same App state the palette row reaches, that a tab row acts on the
//! right-clicked (not the active) tab, and that the menu render signature
//! changes with the facts that select the rows. Nothing here depends on the
//! host OS: the rows come from the same helpers on Linux, macOS, and Windows,
//! and the quick terminal, which the window owner reports no sibling windows
//! for, offers no "to Window" or merge rows by the same rule.

use super::multipane_labels::split_app;
use super::*;
use crate::native::app::reparent::MoveRequest;
use crate::native::context_menu_ui::MenuLayout;
use crate::native::merge_picker::MergeDirection;
use crate::native::session::MoveScope;

const MOVE_TAB_NEW: &str = "Move Tab to New Window";
const MOVE_TAB_WINDOW: &str = "Move Tab to Window\u{2026}";
const MOVE_PANE_NEW: &str = "Move Pane to New Window";
const MOVE_PANE_WINDOW: &str = "Move Pane to Window\u{2026}";
const MERGE_INTO: &str = "Merge This Window Into\u{2026}";
const PULL_INTO: &str = "Pull Window Into This One\u{2026}";
const EXPORT_TEXT: &str = "Export Scrollback As Text\u{2026}";
const EXPORT_HTML: &str = "Export Scrollback As HTML\u{2026}";
const STACK: &str = "Stack Panes";
const FLOAT: &str = "Float Panes";
const TILE: &str = "Tile Panes";
const ARRANGE: &str = "Arrange Floating Pane";

const ALL_ROWS: [&str; 12] = [
    MOVE_TAB_NEW,
    MOVE_TAB_WINDOW,
    MOVE_PANE_NEW,
    MOVE_PANE_WINDOW,
    MERGE_INTO,
    PULL_INTO,
    EXPORT_TEXT,
    EXPORT_HTML,
    STACK,
    FLOAT,
    TILE,
    ARRANGE,
];

/// The v0.16 rows a menu currently offers, in menu order.
fn offered(app: &App) -> Vec<&'static str> {
    app.context_menu_labels_for_test()
        .into_iter()
        .filter(|label| ALL_ROWS.contains(label))
        .collect()
}

/// A single-pane window with `tabs` tabs and a fixed cell size.
fn tab_app(tabs: usize) -> App {
    let dims = Dimensions::new(80, 24);
    let (mut app, _terminal) =
        headless_app_with(NativeOptions::default(), dims, Settings::default());
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
    for _ in 1..tabs {
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        app.push_headless_session_for_test(
            terminal,
            crate::native::test_support::headless_writer(),
            dims,
        );
    }
    app
}

/// Right-click the top-strip tab at `idx` through the production press route.
fn right_click_tab(app: &mut App, idx: usize) {
    assert!(
        app.point_at_top_tab_for_test(idx),
        "tab {idx} is on the strip"
    );
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test(), "the tab menu opened");
}

/// Right-click the empty part of the tab strip.
fn right_click_empty_strip(app: &mut App) {
    assert!(
        app.point_at_empty_top_strip_for_test(),
        "the strip has an empty part"
    );
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test(), "the strip menu opened");
}

/// Right-click inside the terminal content.
fn right_click_content(app: &mut App) {
    app.set_pointer_px_for_test(100.0, 100.0);
    app.set_pointer_cell_for_test(6, 12);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test(), "the content menu opened");
}

/// Walk keyboard focus to the item labelled `label` and activate it.
fn activate_row(app: &mut App, label: &str) {
    let labels = app.context_menu_labels_for_test();
    let index = labels
        .iter()
        .position(|candidate| *candidate == label)
        .unwrap_or_else(|| panic!("{label} is offered: {labels:?}"));
    for _ in 0..index {
        app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::ArrowDown), false, false);
    }
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(
        !app.context_menu_open_for_test(),
        "activating closes the menu"
    );
}

fn close_menu(app: &mut App) {
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    assert!(!app.context_menu_open_for_test());
}

/// The tab right-click offers the move rows exactly when their palette rows
/// are offered: "to New Window" needs another tab left behind, and "to
/// Window..." needs another window. Merge rows never show on a tab.
#[test]
fn tab_menu_offers_the_move_rows_when_they_apply() {
    let mut lone = tab_app(1);
    lone.set_sibling_window_count(1);
    lone.set_pointer_cell_for_test(0, 4);
    let token = lone.active_session_token_for_test();
    lone.open_tab_slot_menu_for_test(token);
    assert!(lone.context_menu_open_for_test());
    assert_eq!(
        offered(&lone),
        vec![MOVE_TAB_WINDOW],
        "a lone tab cannot open in a new window but can move to a sibling"
    );

    let mut two = tab_app(2);
    right_click_tab(&mut two, 0);
    assert_eq!(
        offered(&two),
        vec![MOVE_TAB_NEW],
        "no sibling window: only the new-window row"
    );
    close_menu(&mut two);

    two.set_sibling_window_count(1);
    right_click_tab(&mut two, 0);
    assert_eq!(offered(&two), vec![MOVE_TAB_NEW, MOVE_TAB_WINDOW]);
}

/// "Move Tab to New Window" on a tab that is not active makes that tab the
/// active one and queues the same request the palette row queues.
#[test]
fn tab_menu_move_to_new_window_acts_on_the_clicked_inactive_tab() {
    let mut app = tab_app(2);
    let active_before = app.active_session_token_for_test();
    let clicked_idx = if app.session_token_at_position_for_test(0) == Some(active_before) {
        1
    } else {
        0
    };
    let clicked = app
        .session_token_at_position_for_test(clicked_idx)
        .expect("clicked tab");
    assert_ne!(
        clicked, active_before,
        "the clicked tab is not the active one"
    );

    right_click_tab(&mut app, clicked_idx);
    activate_row(&mut app, MOVE_TAB_NEW);

    assert_eq!(app.active_session_token_for_test(), clicked);
    assert_eq!(
        app.take_move_request(),
        Some(MoveRequest::NewWindow(MoveScope::ActiveTab))
    );

    // The palette row queues the identical request.
    let mut palette = tab_app(2);
    palette.handle_palette_action_for_test("move-tab-new-window");
    assert_eq!(
        palette.take_move_request(),
        Some(MoveRequest::NewWindow(MoveScope::ActiveTab))
    );
}

/// "Move Tab to Window..." opens the picker for the clicked tab.
#[test]
fn tab_menu_move_to_window_requests_the_picker_for_the_clicked_tab() {
    let mut app = tab_app(2);
    app.set_sibling_window_count(1);
    let active_before = app.active_session_token_for_test();
    let clicked_idx = if app.session_token_at_position_for_test(0) == Some(active_before) {
        1
    } else {
        0
    };
    let clicked = app
        .session_token_at_position_for_test(clicked_idx)
        .expect("clicked tab");

    right_click_tab(&mut app, clicked_idx);
    activate_row(&mut app, MOVE_TAB_WINDOW);

    assert_eq!(app.active_session_token_for_test(), clicked);
    assert_eq!(
        app.take_merge_picker_request(),
        Some(MergeDirection::MoveTabInto)
    );
}

/// The empty-strip menu offers merge and pull only with another window, and
/// each row requests the same picker the palette row requests.
#[test]
fn empty_strip_menu_offers_merge_rows_only_with_a_sibling_window() {
    let mut alone = tab_app(2);
    right_click_empty_strip(&mut alone);
    assert!(
        offered(&alone).is_empty(),
        "no sibling window: no merge rows: {:?}",
        alone.context_menu_labels_for_test()
    );
    close_menu(&mut alone);

    alone.set_sibling_window_count(1);
    right_click_empty_strip(&mut alone);
    assert_eq!(offered(&alone), vec![MERGE_INTO, PULL_INTO]);
    activate_row(&mut alone, MERGE_INTO);
    assert_eq!(
        alone.take_merge_picker_request(),
        Some(MergeDirection::MergeThisInto)
    );

    right_click_empty_strip(&mut alone);
    activate_row(&mut alone, PULL_INTO);
    assert_eq!(
        alone.take_merge_picker_request(),
        Some(MergeDirection::PullIntoThis)
    );
}

/// A single-pane tab offers neither layout nor pane-move rows, but always
/// offers the two export rows.
#[test]
fn single_pane_content_menu_offers_export_but_no_layout_or_pane_move_rows() {
    let mut app = tab_app(1);
    app.set_sibling_window_count(1);
    right_click_content(&mut app);
    assert_eq!(offered(&app), vec![EXPORT_TEXT, EXPORT_HTML]);
}

/// In a split tab the layout rows hide the arrangement already in force, and
/// Arrange Floating Pane appears only in a floating tab.
#[test]
fn content_menu_layout_rows_follow_the_arrangement() {
    let (mut app, _terminal) = split_app(2);
    right_click_content(&mut app);
    assert_eq!(app.context_menu_window_actions().layout, MenuLayout::Tiled);
    assert_eq!(
        offered(&app),
        vec![MOVE_PANE_NEW, EXPORT_TEXT, EXPORT_HTML, STACK, FLOAT],
        "tiled: stack and float, no tile and no arrange"
    );

    activate_row(&mut app, FLOAT);
    assert_eq!(
        app.context_menu_window_actions().layout,
        MenuLayout::Floating
    );
    right_click_content(&mut app);
    assert_eq!(
        offered(&app),
        vec![
            MOVE_PANE_NEW,
            EXPORT_TEXT,
            EXPORT_HTML,
            STACK,
            TILE,
            ARRANGE
        ],
        "floating: no float, with arrange"
    );

    activate_row(&mut app, ARRANGE);
    assert!(
        app.float_arrange_active_for_test(),
        "Arrange armed the mode"
    );

    right_click_content(&mut app);
    activate_row(&mut app, STACK);
    assert_eq!(
        app.context_menu_window_actions().layout,
        MenuLayout::Stacked
    );
    right_click_content(&mut app);
    assert_eq!(
        offered(&app),
        vec![MOVE_PANE_NEW, EXPORT_TEXT, EXPORT_HTML, FLOAT, TILE],
        "stacked: no stack, no arrange"
    );

    activate_row(&mut app, TILE);
    assert_eq!(app.context_menu_window_actions().layout, MenuLayout::Tiled);
}

/// Each layout row ends in the arrangement the palette row of the same name
/// produces.
#[test]
fn layout_menu_rows_match_the_palette_rows() {
    for (label, id, expected) in [
        (STACK, "stack-panes", MenuLayout::Stacked),
        (FLOAT, "float-panes", MenuLayout::Floating),
    ] {
        let (mut from_menu, _t1) = split_app(2);
        right_click_content(&mut from_menu);
        activate_row(&mut from_menu, label);
        let (mut from_palette, _t2) = split_app(2);
        from_palette.handle_palette_action_for_test(id);
        assert_eq!(from_menu.context_menu_window_actions().layout, expected);
        assert_eq!(
            from_menu.context_menu_window_actions(),
            from_palette.context_menu_window_actions(),
            "{label}"
        );
    }
}

/// The pane-move rows follow the palette's own eligibility, and each queues the
/// request the palette row queues.
#[test]
fn content_menu_pane_move_rows_follow_the_palette_and_dispatch_its_requests() {
    let (mut app, _terminal) = split_app(2);
    right_click_content(&mut app);
    assert!(offered(&app).contains(&MOVE_PANE_NEW));
    assert!(
        !offered(&app).contains(&MOVE_PANE_WINDOW),
        "no sibling window: no pane picker row"
    );
    activate_row(&mut app, MOVE_PANE_NEW);
    assert_eq!(
        app.take_move_request(),
        Some(MoveRequest::NewWindow(MoveScope::ActivePane))
    );

    app.set_sibling_window_count(1);
    right_click_content(&mut app);
    assert!(offered(&app).contains(&MOVE_PANE_WINDOW));
    activate_row(&mut app, MOVE_PANE_WINDOW);
    assert_eq!(
        app.take_merge_picker_request(),
        Some(MergeDirection::MovePaneInto)
    );
}

/// The menu offers a move row exactly when the palette's move rows say it
/// should, for every combination of split and sibling window.
#[test]
fn menu_move_rows_agree_with_the_palette_move_rows() {
    for split in [false, true] {
        for siblings in [0, 1] {
            let (mut app, _terminal) = if split {
                split_app(2)
            } else {
                (tab_app(1), Arc::new(Mutex::new(Terminal::new(80, 24))))
            };
            app.set_sibling_window_count(siblings);
            right_click_content(&mut app);
            let rows = app.move_palette_rows();
            let menu = offered(&app);
            assert_eq!(menu.contains(&MOVE_PANE_NEW), split, "split {split}");
            assert_eq!(menu.contains(&MOVE_PANE_NEW), rows.pane_to_new_window);
            assert_eq!(
                menu.contains(&MOVE_PANE_WINDOW),
                rows.pane_to_window,
                "split {split} siblings {siblings}"
            );
        }
    }
}

/// Both export rows reach the palette's export path. A headless App has no
/// event-loop proxy, so the dialog cannot open and the path says so.
#[test]
fn export_menu_rows_reach_the_palette_export_path() {
    for label in [EXPORT_TEXT, EXPORT_HTML] {
        let mut app = tab_app(1);
        right_click_content(&mut app);
        activate_row(&mut app, label);
        assert_eq!(
            app.open_notice_message_for_test().as_deref(),
            Some("Native scrollback export is unavailable."),
            "{label}"
        );
    }
}

/// The facts that select the rows are part of the menu render signature, so a
/// change repaints the menu instead of re-presenting the old frame.
#[test]
fn the_menu_render_signature_changes_with_the_window_facts() {
    let mut app = tab_app(2);
    right_click_tab(&mut app, 0);
    let without = app.overlay_signature_for_test().context_menu;
    close_menu(&mut app);

    app.set_sibling_window_count(1);
    right_click_tab(&mut app, 0);
    let with_sibling = app.overlay_signature_for_test().context_menu;
    assert_ne!(without, with_sibling, "a sibling window repaints the menu");
    close_menu(&mut app);

    let (mut split, _terminal) = split_app(2);
    right_click_content(&mut split);
    let tiled = split.overlay_signature_for_test().context_menu;
    activate_row(&mut split, FLOAT);
    right_click_content(&mut split);
    let floating = split.overlay_signature_for_test().context_menu;
    assert_ne!(tiled, floating, "a layout change repaints the menu");
}

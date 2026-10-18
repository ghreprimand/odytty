// SPDX-License-Identifier: GPL-3.0-only
//! The paragraph direction controls (`bidi_direction`): the Settings >
//! Rendering "Paragraph direction" row and the right-click direction radio
//! rows beside "Reorder Right-to-Left Text", driven through the real overlay
//! key and pointer paths. A change applies live, lays out every paragraph
//! without its own direction again on the next frame, re-keys the frame, and
//! persists only through Settings Save.

use super::*;
use crate::settings::BidiDirection;
use std::time::Instant;
use winit::keyboard::{Key as WinitKey, NamedKey};

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 40;
const ROWS: usize = 12;
/// Logical "ab אבג xy". In a right-to-left paragraph "ab" is a left-to-right
/// run at the right edge: logical column 0 draws at visual column 38.
const LINE: &str = "ab \u{05D0}\u{05D1}\u{05D2} xy";
const LTR_ROW: &str = "Left-to-Right Paragraphs";
const RTL_ROW: &str = "Right-to-Left Paragraphs";
const AUTO_ROW: &str = "Automatic Paragraph Direction";

fn checked(label: &str) -> String {
    format!("\u{2713} {label}")
}

fn app(bidi_reorder: bool) -> App {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings {
            bidi_reorder,
            ..Settings::default()
        },
    );
    terminal.lock().expect("terminal").advance(LINE.as_bytes());
    app.set_test_cell_for_test(CELL);
    app
}

fn key(app: &mut App, key: WinitKey, ctrl: bool) {
    app.drive_overlay_key_for_test(key, ctrl, false);
}

fn temp_conf(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("odytty-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("odytty.conf");
    std::fs::write(&path, "# kept\n").unwrap();
    path
}

/// The display map of a frame presented now.
fn frame_map(app: &mut App) -> crate::grid::BidiDisplayMap {
    let _ = app.present_bidi_frame_for_test(Instant::now());
    app.bidi_frame_map_for_test().expect("display map").clone()
}

fn open_menu(app: &mut App) -> Vec<&'static str> {
    app.set_pointer_cell_for_test(1, 1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    app.context_menu_labels_for_test()
}

/// Walk the menu focus down until the row containing `needle` renders, and
/// return its grid row.
fn find_menu_row(app: &mut App, needle: &str) -> usize {
    let (cols, rows) = app.grid_dims_for_test();
    for _ in 0..64 {
        let rendered = app.render_overlay_rows_for_test(cols, rows);
        if let Some(row) = rendered.iter().position(|line| line.contains(needle)) {
            return row;
        }
        key(app, WinitKey::Named(NamedKey::ArrowDown), false);
    }
    panic!("menu row {needle:?} not found");
}

/// Right-click the grid, then click the menu row containing `needle`.
fn click_menu_row(app: &mut App, needle: &str) {
    let _ = open_menu(app);
    let grid_row = find_menu_row(app, needle);
    let rect = app.overlay_rect_for_test().expect("context menu open");
    app.set_pointer_cell_for_test(grid_row, rect.body_left);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
}

#[test]
fn context_menu_sets_rtl_paragraph_direction_live() {
    let mut app = app(true);
    let before = frame_map(&mut app);
    assert_eq!(before.visual_column(0, 0), 0, "left to right by default");

    let labels = open_menu(&mut app);
    let reorder = labels
        .iter()
        .position(|label| label.contains("Reorder Right-to-Left Text"))
        .expect("the reordering toggle");
    let expected = [checked(LTR_ROW), RTL_ROW.to_owned(), AUTO_ROW.to_owned()];
    assert_eq!(
        &labels[reorder + 1..reorder + 4],
        expected.as_slice(),
        "three radio rows right after the toggle, the current one checked"
    );
    key(&mut app, WinitKey::Named(NamedKey::Escape), false);

    let epoch = app.presentation_epoch_for_test();
    click_menu_row(&mut app, RTL_ROW);
    assert!(!app.context_menu_open_for_test(), "the menu closes");
    assert_eq!(
        app.bidi_direction_setting_for_test(),
        BidiDirection::RightToLeft
    );
    assert!(
        app.presentation_epoch_for_test() > epoch,
        "re-keys the frame"
    );
    let map = frame_map(&mut app);
    assert_eq!(
        map.visual_column(0, 0),
        38,
        "the existing line is laid out again"
    );
    assert_eq!(map.visual_column(0, 1), 39);

    let labels = open_menu(&mut app);
    assert!(labels.iter().any(|l| *l == checked(RTL_ROW)), "{labels:?}");
    assert!(labels.contains(&LTR_ROW), "{labels:?}");
    assert!(!labels.iter().any(|l| *l == checked(LTR_ROW)), "{labels:?}");
    key(&mut app, WinitKey::Named(NamedKey::Escape), false);

    // Automatic: "ab" is the first strong text, so the row is left to right.
    click_menu_row(&mut app, AUTO_ROW);
    assert_eq!(app.bidi_direction_setting_for_test(), BidiDirection::Auto);
    assert_eq!(frame_map(&mut app).visual_column(0, 0), 0);
}

#[test]
fn keyboard_enter_on_a_direction_row_applies_it() {
    let mut app = app(true);
    let _ = open_menu(&mut app);
    for _ in 0..64 {
        if app.context_menu_focused_label_for_test() == Some(RTL_ROW) {
            break;
        }
        key(&mut app, WinitKey::Named(NamedKey::ArrowDown), false);
    }
    assert_eq!(app.context_menu_focused_label_for_test(), Some(RTL_ROW));
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    assert!(!app.context_menu_open_for_test());
    assert_eq!(
        app.bidi_direction_setting_for_test(),
        BidiDirection::RightToLeft
    );
}

#[test]
fn direction_rows_hide_while_reordering_is_off() {
    let mut app = app(false);
    let labels = open_menu(&mut app);
    assert!(labels.contains(&"Reorder Right-to-Left Text"), "{labels:?}");
    for row in [LTR_ROW, RTL_ROW, AUTO_ROW] {
        assert!(
            !labels.iter().any(|label| label.contains(row)),
            "{row} has no effect while reordering is off: {labels:?}"
        );
    }
}

#[test]
fn direction_change_changes_render_signature() {
    let mut app = app(true);
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let (before, _) = app.redraw_single_pane_probe_for_test().expect("frame");
    click_menu_row(&mut app, RTL_ROW);
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let (after, _) = app.redraw_single_pane_probe_for_test().expect("frame");
    assert_ne!(
        after.content.bidi_placement, before.content.bidi_placement,
        "the new placement joins the frame signature"
    );

    // Choosing the direction already in force changes nothing.
    let epoch = app.presentation_epoch_for_test();
    click_menu_row(&mut app, &checked(RTL_ROW));
    assert!(!app.context_menu_open_for_test());
    assert_eq!(app.presentation_epoch_for_test(), epoch);
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let (again, _) = app.redraw_single_pane_probe_for_test().expect("frame");
    assert_eq!(again.content.bidi_placement, after.content.bidi_placement);
}

#[test]
fn context_menu_direction_is_not_saved() {
    let mut app = app(true);
    let conf = temp_conf("bidi-direction-menu");
    app.set_config_path_for_test(conf.clone());
    click_menu_row(&mut app, RTL_ROW);
    assert_eq!(
        app.bidi_direction_setting_for_test(),
        BidiDirection::RightToLeft
    );
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), "# kept\n");
    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}

/// Open Settings inside the Rendering section with the selection on the
/// Paragraph direction row.
fn select_direction_row(app: &mut App) {
    app.open_settings_section_for_test("Rendering");
    let signature = app.overlay_signature_for_test();
    let entries = &signature.panel.entries;
    let target = entries
        .iter()
        .position(|entry| entry.key == "bidi_direction")
        .expect("the Rendering section lists bidi_direction");
    let toggle = entries
        .iter()
        .position(|entry| entry.key == "bidi_reorder")
        .expect("the Rendering section lists bidi_reorder");
    assert_eq!(target, toggle + 1, "the row sits beside Bidirectional text");
    for _ in signature.panel.selected..target {
        key(app, WinitKey::Named(NamedKey::ArrowDown), false);
    }
}

#[test]
fn settings_direction_row_applies_and_saves() {
    let mut app = app(true);
    let conf = temp_conf("bidi-direction-panel");
    app.set_config_path_for_test(conf.clone());
    select_direction_row(&mut app);
    assert_eq!(
        app.settings_panel_displayed_value_for_test("bidi_direction"),
        Some("ltr".to_owned())
    );

    let epoch = app.presentation_epoch_for_test();
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert_eq!(
        app.bidi_direction_setting_for_test(),
        BidiDirection::RightToLeft,
        "Enter cycles ltr to rtl and applies live"
    );
    assert!(app.presentation_epoch_for_test() > epoch);
    assert_eq!(frame_map(&mut app).visual_column(0, 0), 38);

    key(&mut app, WinitKey::Character("s".into()), true);
    let saved = std::fs::read_to_string(&conf).unwrap();
    assert!(saved.contains("bidi_direction = rtl"), "{saved}");
    assert!(saved.contains("# kept"));
    assert_eq!(
        app.bidi_direction_setting_for_test(),
        BidiDirection::RightToLeft,
        "Save keeps it applied"
    );
    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}

/// The menu reads the live setting at every open, so a Settings change shows
/// as the checked row.
#[test]
fn context_menu_checks_the_direction_set_in_settings() {
    let mut app = app(true);
    select_direction_row(&mut app);
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert_eq!(app.bidi_direction_setting_for_test(), BidiDirection::Auto);
    app.close_overlay_for_test();
    let labels = open_menu(&mut app);
    assert!(labels.iter().any(|l| *l == checked(AUTO_ROW)), "{labels:?}");
    assert!(labels.contains(&LTR_ROW) && labels.contains(&RTL_ROW));
}

/// A change made from the menu reaches the Settings panel as an unsaved
/// edit: the panel shows it, and a later panel edit of another row does not
/// write the panel's older copy back over it.
#[test]
fn menu_changes_survive_a_later_settings_edit() {
    let mut app = app(false);
    click_menu_row(&mut app, "Reorder Right-to-Left Text");
    assert!(app.bidi_reorder_setting_for_test());
    click_menu_row(&mut app, RTL_ROW);
    assert_eq!(
        app.bidi_direction_setting_for_test(),
        BidiDirection::RightToLeft
    );
    app.open_settings_section_for_test("Rendering");
    assert_eq!(
        app.settings_panel_displayed_value_for_test("bidi_reorder"),
        Some("on".to_owned())
    );
    assert_eq!(
        app.settings_panel_displayed_value_for_test("bidi_direction"),
        Some("rtl".to_owned())
    );
    // Cycle the direction row once in the panel: rtl becomes auto, and
    // reordering stays on.
    let signature = app.overlay_signature_for_test();
    let target = signature
        .panel
        .entries
        .iter()
        .position(|entry| entry.key == "bidi_direction")
        .expect("bidi_direction row");
    for _ in signature.panel.selected..target {
        key(&mut app, WinitKey::Named(NamedKey::ArrowDown), false);
    }
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert_eq!(app.bidi_direction_setting_for_test(), BidiDirection::Auto);
    assert!(
        app.bidi_reorder_setting_for_test(),
        "the panel edit keeps the menu's reordering"
    );
}

/// Menu changes are unsaved Settings edits: the panel counts them, and its
/// Save writes them to odytty.conf.
#[test]
fn settings_save_persists_menu_changes() {
    let mut app = app(false);
    let conf = temp_conf("bidi-direction-menu-save");
    app.set_config_path_for_test(conf.clone());
    click_menu_row(&mut app, "Reorder Right-to-Left Text");
    click_menu_row(&mut app, RTL_ROW);
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), "# kept\n");
    app.open_settings_section_for_test("Rendering");
    key(&mut app, WinitKey::Character("s".into()), true);
    let saved = std::fs::read_to_string(&conf).unwrap();
    assert!(saved.contains("bidi_reorder = on"), "{saved}");
    assert!(saved.contains("bidi_direction = rtl"), "{saved}");
    assert!(saved.contains("# kept"));
    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}

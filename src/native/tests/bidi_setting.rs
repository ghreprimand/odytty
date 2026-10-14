// SPDX-License-Identifier: GPL-3.0-only
//! The user-facing bidi control (`bidi_reorder`): the Settings > Rendering
//! "Bidirectional text" row and the right-click "Reorder Right-to-Left Text"
//! toggle, driven through the real overlay key and pointer paths. The setting
//! is off by default, applies live, re-keys the frame, persists on Save,
//! plans display order on the primary screen only, and leaves every off-path
//! answer unchanged.

use super::*;
use std::time::Instant;
use winit::keyboard::{Key as WinitKey, NamedKey};

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 40;
const ROWS: usize = 12;
/// Logical "ab אבג xy": columns 3..=5 are Hebrew and draw reversed.
const LINE: &str = "ab \u{05D0}\u{05D1}\u{05D2} xy";
const CHECKED: &str = "\u{2713} Reorder Right-to-Left Text";

fn app() -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    terminal.lock().expect("terminal").advance(LINE.as_bytes());
    app.set_test_cell_for_test(CELL);
    (app, terminal)
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

/// Whether a frame presented now plans a display map.
fn frame_plans_a_map(app: &mut App) -> bool {
    let _ = app.present_bidi_frame_for_test(Instant::now());
    app.bidi_frame_map_for_test().is_some()
}

/// Open Settings inside the Rendering section, which holds `bidi_reorder`,
/// with the selection on that row.
fn select_bidi_row(app: &mut App) {
    app.open_settings_section_for_test("Rendering");
    let signature = app.overlay_signature_for_test();
    let target = signature
        .panel
        .entries
        .iter()
        .position(|entry| entry.key == "bidi_reorder")
        .expect("the Rendering section lists bidi_reorder");
    assert!(
        signature
            .panel
            .entries
            .iter()
            .any(|entry| entry.key == "ligatures"),
        "the row sits with the other text-rendering switches"
    );
    for _ in signature.panel.selected..target {
        key(app, WinitKey::Named(NamedKey::ArrowDown), false);
    }
}

#[test]
fn off_by_default_plans_no_map_and_maps_no_pointer() {
    let (mut app, _terminal) = app();
    assert!(!app.bidi_reorder_setting_for_test());
    assert!(!frame_plans_a_map(&mut app));
    for column in 0..COLUMNS {
        app.pointer_move_for_test(
            f64::from(CELL.width) * (column as f64 + 0.5),
            f64::from(CELL.height) * 0.5,
        );
        assert_eq!(
            app.pointer_cell_for_test().map(|point| point.column),
            Some(column),
            "the pointer stays logical with reordering off"
        );
    }
}

#[test]
fn settings_row_toggles_live_rekeys_the_frame_and_persists() {
    let (mut app, _terminal) = app();
    let conf = temp_conf("bidi-reorder-panel");
    app.set_config_path_for_test(conf.clone());
    select_bidi_row(&mut app);
    assert_eq!(
        app.settings_panel_displayed_value_for_test("bidi_reorder"),
        Some("off".to_owned())
    );

    let epoch = app.presentation_epoch_for_test();
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert!(
        app.bidi_reorder_setting_for_test(),
        "the toggle applies live"
    );
    assert!(
        app.presentation_epoch_for_test() > epoch,
        "the toggle re-keys the render signature"
    );
    assert!(frame_plans_a_map(&mut app), "frames now plan display order");
    let map = app.bidi_frame_map_for_test().expect("map").clone();
    assert!(map.row_is_reordered(0));
    assert_eq!(map.visual_column(0, 3), 5);

    key(&mut app, WinitKey::Character("s".into()), true);
    let saved = std::fs::read_to_string(&conf).unwrap();
    assert!(saved.contains("bidi_reorder = on"), "{saved}");
    assert!(saved.contains("# kept"));
    assert!(app.bidi_reorder_setting_for_test(), "Save keeps it applied");

    let epoch = app.presentation_epoch_for_test();
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert!(!app.bidi_reorder_setting_for_test());
    assert!(app.presentation_epoch_for_test() > epoch);
    assert!(!frame_plans_a_map(&mut app), "off again: no map");

    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}

/// Right-click the grid, then click the menu row containing `needle`.
fn click_menu_row(app: &mut App, needle: &str) {
    app.set_pointer_cell_for_test(1, 1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    let (cols, rows) = app.grid_dims_for_test();
    let mut grid_row = None;
    for _ in 0..64 {
        let rendered = app.render_overlay_rows_for_test(cols, rows);
        if let Some(row) = rendered.iter().position(|line| line.contains(needle)) {
            grid_row = Some(row);
            break;
        }
        key(app, WinitKey::Named(NamedKey::ArrowDown), false);
    }
    let grid_row = grid_row.unwrap_or_else(|| panic!("menu row {needle:?} not found"));
    let rect = app.overlay_rect_for_test().expect("context menu open");
    app.set_pointer_cell_for_test(grid_row, rect.body_left);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
}

#[test]
fn context_menu_toggle_is_checked_while_on_and_rekeys_the_frame() {
    let (mut app, _terminal) = app();
    app.set_pointer_cell_for_test(1, 1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    let labels = app.context_menu_labels_for_test();
    assert!(labels.contains(&"Reorder Right-to-Left Text"), "{labels:?}");
    assert!(!labels.contains(&CHECKED));
    key(&mut app, WinitKey::Named(NamedKey::Escape), false);

    let epoch = app.presentation_epoch_for_test();
    click_menu_row(&mut app, "Reorder Right-to-Left Text");
    assert!(!app.context_menu_open_for_test(), "the menu closes");
    assert!(app.bidi_reorder_setting_for_test(), "the menu turns it on");
    assert!(app.presentation_epoch_for_test() > epoch);
    assert!(frame_plans_a_map(&mut app));

    app.set_pointer_cell_for_test(1, 1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    let labels = app.context_menu_labels_for_test();
    assert!(labels.contains(&CHECKED), "checked while on: {labels:?}");
    assert!(!labels.contains(&"Reorder Right-to-Left Text"));
    key(&mut app, WinitKey::Named(NamedKey::Escape), false);

    let epoch = app.presentation_epoch_for_test();
    click_menu_row(&mut app, CHECKED);
    assert!(
        !app.bidi_reorder_setting_for_test(),
        "the menu turns it off"
    );
    assert!(app.presentation_epoch_for_test() > epoch);
    assert!(!frame_plans_a_map(&mut app));
}

#[test]
fn the_alternate_screen_is_never_reordered() {
    let (mut app, terminal) = app();
    let settings = Settings {
        bidi_reorder: true,
        ..Settings::default()
    };
    app.apply_overlay_settings_for_test(settings);
    assert!(frame_plans_a_map(&mut app), "primary screen plans");
    terminal
        .lock()
        .expect("terminal")
        .advance(format!("\x1b[?1049h{LINE}").as_bytes());
    assert!(!frame_plans_a_map(&mut app), "alternate screen: no map");
    for column in 0..COLUMNS {
        app.pointer_move_for_test(
            f64::from(CELL.width) * (column as f64 + 0.5),
            f64::from(CELL.height) * 0.5,
        );
        assert_eq!(
            app.pointer_cell_for_test().map(|point| point.column),
            Some(column)
        );
    }
    terminal.lock().expect("terminal").advance(b"\x1b[?1049l");
    assert!(frame_plans_a_map(&mut app), "back on the primary screen");
}

#[test]
fn the_setting_maps_the_pointer_to_the_logical_cell_drawn_under_it() {
    let (mut app, _terminal) = app();
    app.apply_overlay_settings_for_test(Settings {
        bidi_reorder: true,
        ..Settings::default()
    });
    assert!(frame_plans_a_map(&mut app));
    app.pointer_move_for_test(f64::from(CELL.width) * 5.5, f64::from(CELL.height) * 0.5);
    assert_eq!(
        app.pointer_cell_for_test().map(|point| point.column),
        Some(3),
        "screen column 5 draws alef, logical column 3"
    );
}

/// A right-click menu may cover a reordered run without changing the content
/// map. The terminal cells below it remain terminal cells, not menu text.
#[test]
fn right_click_keeps_bidi_placement_while_the_menu_is_open() {
    let (mut app, terminal) = app();
    app.apply_overlay_settings_for_test(Settings {
        bidi_reorder: true,
        ..Settings::default()
    });
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let before = app.bidi_frame_map_for_test().expect("display map").clone();
    assert_eq!(before.visual_column(0, 3), 5);
    let (signature_before, _) = app.redraw_single_pane_probe_for_test().expect("frame");
    app.set_pointer_cell_for_test(0, 1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    let (signature_open, _) = app.redraw_single_pane_probe_for_test().expect("menu frame");
    assert_eq!(
        signature_open.content.bidi_placement,
        signature_before.content.bidi_placement
    );
    assert_ne!(
        signature_open.content.overlay,
        signature_before.content.overlay
    );
    let frame = app.present_bidi_frame_for_test(Instant::now());
    assert_eq!(app.bidi_frame_map_for_test(), Some(&before));
    assert!(
        frame.overlay_top.is_some(),
        "the menu has its own opaque layer"
    );
    assert_eq!(
        frame.painted.cells,
        terminal.lock().expect("terminal").snapshot().cells
    );
    key(&mut app, WinitKey::Named(NamedKey::Escape), false);
    let _ = app.present_bidi_frame_for_test(Instant::now());
    assert_eq!(app.bidi_frame_map_for_test(), Some(&before));
}

#[test]
fn window_overlay_siblings_keep_the_terminal_bidi_plan() {
    let openers: &[fn(&mut App)] = &[
        App::open_settings_overlay_for_test,
        |app| app.open_palette_with_synthetic_history_for_test(0),
        App::open_theme_builder_for_test,
        App::open_key_bindings_overlay_for_test,
        App::open_confirm_close_for_test,
        |app| {
            let token = app.active_session_token_for_test();
            app.open_move_tab_workspace_picker_for_test(token);
        },
        |app| {
            assert!(app.begin_rename_tab_for_test(0));
        },
        |app| app.enter_rename_workspace_for_test(0),
        App::enter_save_all_layout_prompt_for_test,
    ];
    for (index, open) in openers.iter().enumerate() {
        let (mut app, terminal) = app();
        app.apply_overlay_settings_for_test(Settings {
            bidi_reorder: true,
            ..Settings::default()
        });
        app.push_headless_workspace_for_test(
            terminal.clone(),
            crate::native::test_support::headless_writer(),
            Dimensions::new(COLUMNS, ROWS),
        );
        // Put the same run on every row, so centered panels and rename bands
        // exercise changed rows as well as the context menu's top rows.
        terminal
            .lock()
            .expect("terminal")
            .advance(format!("\r\n{LINE}").repeat(ROWS - 1).as_bytes());
        let _ = app.present_bidi_frame_for_test(Instant::now());
        let before = app.bidi_frame_map_for_test().expect("display map").clone();
        open(&mut app);
        let frame = app.present_bidi_frame_for_test(Instant::now());
        assert!(
            frame.overlay_top.is_some(),
            "overlay {index} has a separate layer"
        );
        assert_eq!(
            app.bidi_frame_map_for_test(),
            Some(&before),
            "overlay {index}"
        );
    }
}

#[test]
fn search_highlights_keep_bidi_placement_outside_the_inline_query_band() {
    let (mut app, _) = app();
    app.apply_overlay_settings_for_test(Settings {
        bidi_reorder: true,
        ..Settings::default()
    });
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let before = app.bidi_frame_map_for_test().expect("display map").clone();
    app.open_search_for_test();
    key(&mut app, WinitKey::Character("ab".into()), false);
    let _ = app.present_bidi_frame_for_test(Instant::now());
    let after = app.bidi_frame_map_for_test().expect("display map");
    assert_eq!(after.visual_column(0, 3), before.visual_column(0, 3));
    assert!(after.row_is_reordered(0));
}

// SPDX-License-Identifier: GPL-3.0-only
//! Chrome seam drags keep their geometry and the open Settings draft in step,
//! through the real pointer path of a headless App: a pinned right rail on a
//! surface that is not a whole number of cells keeps its width on a drag that
//! has not moved, the rail width drag and reset update an open Settings
//! panel the way the top-bar height drag does (driven directly, since an open
//! panel captures the pointer), and the top-bar height drag
//! measures from the padded surface it is drawn on.

use super::*;
use crate::settings::{TabBarHeight, TabRailWidth};

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};

/// A scratch directory removed when the test ends.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A headless App on a `width` x `height` surface whose seam releases persist
/// to a scratch config file.
fn sized_app(
    width: u32,
    height: u32,
    padding: WindowPadding,
    settings: Settings,
) -> (App, Scratch) {
    let (mut app, _terminal) =
        headless_app_with(NativeOptions::default(), Dimensions::new(100, 25), settings);
    let dir = crate::test_dirs::fresh_temp_dir("odytty-seam-basis-");
    let conf = dir.join("odytty.conf");
    std::fs::write(&conf, "").expect("scratch config");
    app.set_config_path_for_test(conf);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(width, height, padding);
    app.resize_grid_with_padding_for_test(CELL, padding, width, height);
    (app, Scratch(dir))
}

/// A pinned right workspace rail, 16 columns wide, on an 805px surface:
/// the surface leaves a remainder narrower than one cell.
fn right_rail_app() -> (App, Scratch) {
    let (mut app, scratch) = sized_app(805, 400, WindowPadding::ZERO, Settings::default());
    app.set_tab_bar_placement_for_test("right");
    app.set_workspace_rail_for_test("always");
    app.set_tab_rail_width_manual_for_test(16);
    (app, scratch)
}

/// The centre of the seam's grab band, found through the production hit test.
fn rail_seam_x(app: &App) -> f64 {
    let hits: Vec<u32> = (0..805)
        .filter(|x| app.pointer_over_rail_seam_for_test(f64::from(*x)) == Some(true))
        .collect();
    assert!(!hits.is_empty(), "the pinned rail has a seam");
    f64::from(hits[0] + hits[hits.len() - 1]) / 2.0
}

#[test]
fn a_pinned_right_rail_keeps_its_width_on_a_drag_that_has_not_moved() {
    let (mut app, _scratch) = right_rail_app();
    let seam = rail_seam_x(&app);
    app.set_pointer_px_for_test(seam, 100.0);
    app.mouse_left_press_for_test();
    assert!(
        app.rail_seam_dragging_for_test(),
        "the press grabs the seam"
    );
    app.pointer_move_for_test(seam, 100.0);
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(16));
    // One cell toward the content widens the rail by one column.
    app.pointer_move_for_test(seam - f64::from(CELL.width), 100.0);
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(17));
    app.mouse_left_release_for_test();
}

#[test]
fn a_rail_width_drag_and_reset_update_an_open_settings_panel() {
    let (mut app, _scratch) = right_rail_app();
    let seam = rail_seam_x(&app);
    app.open_settings_overlay_for_test();
    let before = app.settings_panel_displayed_value_for_test("tab_rail_width");
    assert!(before.is_some(), "the panel shows the rail width");
    // An open panel captures the pointer, so the drag and reset are driven
    // through their production functions directly.
    app.drag_rail_seam_to_pointer_for_test(seam - 2.0 * f64::from(CELL.width));
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Manual(18));
    let dragged = app.settings_panel_displayed_value_for_test("tab_rail_width");
    assert_ne!(dragged, before, "the panel follows the drag");
    app.reset_rail_width_to_auto_for_test();
    assert_eq!(app.tab_rail_width_for_test(), TabRailWidth::Auto);
    assert_ne!(
        app.settings_panel_displayed_value_for_test("tab_rail_width"),
        dragged,
        "the panel follows the reset"
    );
}

#[test]
fn a_padded_top_bar_keeps_its_height_on_a_drag_that_has_not_moved() {
    let padding = WindowPadding::from_logical(12.0, 1.0);
    let settings = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    let (mut app, _scratch) = sized_app(800, 400, padding, settings);
    app.set_tab_bar_height_manual_for_test(2);
    let seam = f64::from(app.tab_bar_seam_y_for_test().expect("a top bar"));
    app.set_pointer_px_for_test(200.0, seam);
    app.mouse_left_press_for_test();
    assert!(
        app.tab_bar_seam_dragging_for_test(),
        "the press grabs the seam"
    );
    app.pointer_move_for_test(200.0, seam);
    assert_eq!(app.tab_bar_height_for_test(), TabBarHeight::Manual(2));
    app.mouse_left_release_for_test();
}

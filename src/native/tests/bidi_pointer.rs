// SPDX-License-Identifier: GPL-3.0-only
//! S3a: pointer hit testing and drag selection under the test-only bidi
//! display gate, through the real pointer path of a headless single-pane App.
//! The pointer over a screen column addresses the logical cell drawn there;
//! selection endpoints and copied text stay logical.

use super::*;

use crate::selection::CellPoint;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};

/// Logical "ab אבג xy": columns 3..=5 are Hebrew and draw reversed.
const LINE: &str = "ab \u{05D0}\u{05D1}\u{05D2} xy";

fn app() -> App {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(20, 4),
        Settings::default(),
    );
    terminal.lock().expect("terminal").advance(LINE.as_bytes());
    app.set_test_cell_for_test(CELL);
    app
}

fn pixel(visual_column: usize) -> (f64, f64) {
    (
        f64::from(CELL.width) * visual_column as f64 + 4.0,
        f64::from(CELL.height) / 2.0,
    )
}

#[test]
fn pointer_addresses_the_logical_cell_drawn_under_it() {
    let mut app = app();
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    assert!(map.row_is_reordered(0));
    for column in 0..20 {
        let (x, y) = pixel(map.visual_column(0, column));
        app.pointer_move_for_test(x, y);
        assert_eq!(
            app.pointer_cell_for_test(),
            Some(CellPoint { row: 0, column }),
            "the pointer over visual {} addresses logical {column}",
            map.visual_column(0, column)
        );
    }
}

#[test]
fn pointer_is_unchanged_with_the_gate_off() {
    let mut app = app();
    for column in 0..20 {
        let (x, y) = pixel(column);
        app.pointer_move_for_test(x, y);
        assert_eq!(
            app.pointer_cell_for_test(),
            Some(CellPoint { row: 0, column })
        );
    }
}

#[test]
fn drag_across_a_direction_boundary_selects_and_copies_logical_text() {
    let mut app = app();
    app.present_bidi_frame_map_for_test();
    let map = app.bidi_frame_map_for_test().expect("gate map").clone();
    // Press over "b" (logical 1), drag to the screen column showing alef
    // (logical 3, drawn at visual 5), release.
    let (x0, y) = pixel(map.visual_column(0, 1));
    assert_eq!(map.visual_column(0, 3), 5);
    let (x1, _) = pixel(map.visual_column(0, 3));
    app.pointer_move_for_test(x0, y);
    app.mouse_left_press_for_test();
    app.pointer_move_for_test(x1, y);
    app.mouse_left_release_for_test();
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some("b \u{05D0}"),
        "the selection spans logical columns 1..=3 and copies in logical order"
    );
}

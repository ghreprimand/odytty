// SPDX-License-Identifier: GPL-3.0-only
//! A held selection drag that autoscrolls with bidirectional display on
//! resolves its endpoint against the rows shown after the scroll, through the
//! real pointer path: the endpoint is the logical cell drawn under the pointer
//! on the newly shown row, not the logical column of the row that was there
//! before. Rows alternate between a Hebrew-first line, whose first screen
//! column draws logical column 3, and a left-to-right line, whose first screen
//! column draws logical column 0. Copied text stays logical.

use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 20;
const ROWS: usize = 4;
/// Logical "אבגד xy": the four Hebrew cells draw reversed at screen 0..=3.
const HEBREW: &str = "\u{05D0}\u{05D1}\u{05D2}\u{05D3} xy";
const LATIN: &str = "abcd xy";

fn lines(count: usize) -> String {
    (0..count)
        .map(|i| if i % 2 == 0 { HEBREW } else { LATIN })
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Size the window grid to the terminal so the drag's edge bands are its
/// first and last rows. Legacy drag speed scrolls one row per step,
/// so the row under the pointer always changes direction class.
fn fit(app: &mut App) {
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.resize_grid(CELL, COLUMNS as u32 * CELL.width, ROWS as u32 * CELL.height);
}

fn settings() -> Settings {
    Settings {
        scroll_drag_speed: crate::settings::ScrollDragSpeed::Legacy,
        ..Settings::default()
    }
}

fn single_pane_app() -> App {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        settings(),
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(lines(21).as_bytes());
    fit(&mut app);
    app
}

fn px(row: usize, column: usize) -> (f64, f64) {
    (
        f64::from(CELL.width) * column as f64 + 4.0,
        f64::from(CELL.height) * row as f64 + 8.0,
    )
}

/// The logical column the presented frame draws at screen column 0 of `row`.
fn logical_at_first_column(app: &mut App, row: usize) -> usize {
    app.present_bidi_frame_map_for_test();
    app.bidi_frame_map_for_test()
        .expect("reordering plans a map")
        .logical_column(row, 0)
}

#[test]
fn an_upward_autoscroll_ends_on_the_cell_drawn_on_the_revealed_row() {
    let mut app = single_pane_app();
    let before = logical_at_first_column(&mut app, 0);
    let (x, y) = px(2, 10);
    app.pointer_move_for_test(x, y);
    app.mouse_left_press_for_test();
    // Inside the first row's band, over screen column 0.
    app.pointer_move_for_test(4.0, 2.0);
    assert_eq!(app.viewport_offset_for_test(), 1, "one row was revealed");
    let after = logical_at_first_column(&mut app, 0);
    assert_ne!(before, after, "the revealed row draws a different cell");
    let (_, start_col, _, _) = app.selection_range_for_test().expect("a selection");
    assert_eq!(
        start_col, after,
        "the endpoint is the cell under the pointer"
    );
    let text = app.selection_text_for_test().expect("selected text");
    let first = text.lines().next().expect("a first line");
    let expected = if after == 3 { "\u{05D3} xy" } else { LATIN };
    assert_eq!(first, expected, "copied text stays logical");
}

#[test]
fn a_downward_autoscroll_ends_on_the_cell_drawn_on_the_revealed_row() {
    let mut app = single_pane_app();
    app.dispatch_wheel_for_test(2.0);
    let offset = app.viewport_offset_for_test();
    assert!(offset >= 2, "the viewport is in history");
    let before = logical_at_first_column(&mut app, ROWS - 1);
    let (x, y) = px(1, 10);
    app.pointer_move_for_test(x, y);
    app.mouse_left_press_for_test();
    // Inside the last row's band, over screen column 0.
    let (_, last) = px(ROWS - 1, 0);
    app.pointer_move_for_test(4.0, last + 6.0);
    assert_eq!(
        app.viewport_offset_for_test(),
        offset - 1,
        "one row was revealed"
    );
    let after = logical_at_first_column(&mut app, ROWS - 1);
    assert_ne!(before, after, "the revealed row draws a different cell");
    let (_, _, _, end_col) = app.selection_range_for_test().expect("a selection");
    assert_eq!(end_col, after, "the endpoint is the cell under the pointer");
}

#[test]
fn a_split_pane_autoscroll_ends_on_the_cell_drawn_on_the_revealed_row() {
    let (mut app, _first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS * 2 + 1),
        settings(),
    );
    let dims = Dimensions::new(COLUMNS, ROWS);
    let pane = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    pane.lock().expect("pane").advance(lines(21).as_bytes());
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(std::io::sink())));
    app.seed_headless_split_pane_for_test(false, Arc::clone(&pane), writer, dims);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        (ROWS * 2 + 1) as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    app.set_bidi_display_for_test(true);
    let focused = app.active_session_token_for_test();
    let map_first = |app: &mut App| {
        let _ = app.rebuild_multipane_probe_for_test();
        app.bidi_pane_map_for_test(focused)
            .expect("the focused pane plans a map")
            .logical_column(0, 0)
    };
    let before = map_first(&mut app);
    let (_, origin) = app.focused_pane_grid_for_test().expect("pane geometry");
    let at = |row: usize, column: usize| {
        (
            f64::from(origin[0]) + (column as f64 + 0.5) * f64::from(CELL.width),
            f64::from(origin[1]) + (row as f64 + 0.5) * f64::from(CELL.height),
        )
    };
    let (x, y) = at(2, 10);
    app.pointer_move_for_test(x, y);
    app.mouse_left_press_for_test();
    let (x, y) = at(0, 0);
    app.pointer_move_for_test(x, y - 4.0);
    assert_eq!(app.viewport_offset_for_test(), 1, "one row was revealed");
    let after = map_first(&mut app);
    assert_ne!(before, after, "the revealed row draws a different cell");
    let (_, start_col, _, _) = app.selection_range_for_test().expect("a selection");
    assert_eq!(
        start_col, after,
        "the endpoint is the cell under the pointer"
    );
}

// SPDX-License-Identifier: GPL-3.0-only
//! Focused-pane operations in a split tab use the pane's own grid. The App's
//! `grid` is the whole-window content grid, which is the focused pane's size
//! only on a single-pane tab: paging, search jumps, copy-mode following,
//! line selection, and the touchpad scroll clamp all measure the pane.

use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 40;
const ROWS: usize = 8;

/// A `COLUMNS` x `ROWS` window split once along `columns`; the new pane is
/// focused and has processed `output`. Returns the app and the pane's size.
fn split_app(columns: bool, output: &[u8]) -> (App, Dimensions) {
    let (mut app, _first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    let pane_dims = if columns {
        Dimensions::new(COLUMNS / 2 - 1, ROWS)
    } else {
        Dimensions::new(COLUMNS, ROWS / 2 - 1)
    };
    let pane = Arc::new(Mutex::new(Terminal::new(pane_dims.columns, pane_dims.rows)));
    pane.lock().expect("terminal").advance(output);
    app.seed_headless_split_pane_for_test(
        columns,
        pane,
        crate::native::test_support::headless_writer(),
        pane_dims,
    );
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.reflow_active_panes_for_test();
    let dims = app.active_terminal_dimensions_for_test();
    assert!(!app.active_is_single_pane_for_test());
    (app, dims)
}

fn numbered_lines(count: usize) -> Vec<u8> {
    (0..count)
        .map(|index| format!("line {index:03}\r\n"))
        .collect::<String>()
        .into_bytes()
}

#[test]
fn page_up_in_a_rows_split_scrolls_one_pane_screenful() {
    let (mut app, dims) = split_app(false, &numbered_lines(60));
    assert!(dims.rows < ROWS);
    app.handle_palette_action_for_test("scroll-up");
    assert_eq!(
        app.viewport_offset_for_test(),
        dims.rows - 1,
        "a page is the pane's rows less one"
    );
}

#[test]
fn a_search_jump_in_a_rows_split_lands_the_match_inside_the_pane() {
    let mut output = numbered_lines(30);
    output.extend_from_slice(b"needle\r\n");
    output.extend_from_slice(&numbered_lines(30));
    let (mut app, dims) = split_app(false, &output);
    app.open_search_for_test();
    assert!(app.search_open_for_test());
    app.drive_text_key_for_test("needle");
    let scrollback_len = app.scrollback_len_for_test();
    let top = scrollback_len - app.viewport_offset_for_test();
    // "needle" is the 31st line printed: absolute row 30.
    assert!(
        (top..top + dims.rows).contains(&30),
        "match row 30 must be on the pane's screen (top {top}, {} rows)",
        dims.rows
    );
}

#[test]
fn copy_mode_moving_down_keeps_the_caret_on_the_pane_screen() {
    let (mut app, dims) = split_app(false, &numbered_lines(40));
    app.handle_palette_action_for_test("copy-mode");
    for _ in 0..10 {
        app.drive_text_key_for_test("k");
    }
    let after_up = app.viewport_offset_for_test();
    for _ in 0..4 {
        app.drive_text_key_for_test("j");
    }
    // Ten rows up from the bottom row parks the caret at the top; four rows
    // down leaves it below the shorter pane's screen unless the view follows.
    assert_eq!(
        app.viewport_offset_for_test(),
        after_up - (4 - (dims.rows - 1)),
        "the view follows the caret within the pane's rows"
    );
}

#[test]
fn a_triple_click_in_a_columns_split_selects_to_the_pane_edge() {
    let (mut app, dims) = split_app(true, b"alpha beta gamma\r\n");
    assert!(dims.columns < COLUMNS);
    // Right pane: columns from the divider to the window edge.
    let x = (COLUMNS * CELL.width as usize) as f64 - 4.0 * f64::from(CELL.width);
    let y = f64::from(CELL.height) / 2.0;
    app.pointer_move_for_test(x, y);
    for _ in 0..3 {
        app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
        app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    }
    let (_, _, end_row, end_column) = app.selection_range_for_test().expect("line selected");
    assert_eq!(
        end_column,
        dims.columns - 1,
        "the line ends at the pane edge"
    );
    let _ = end_row;
}

#[test]
fn a_giant_touchpad_delta_in_a_rows_split_moves_at_most_one_pane_screen() {
    let (mut app, dims) = split_app(false, &numbered_lines(80));
    let (w, h) = (
        (COLUMNS * CELL.width as usize) as f64,
        (ROWS * CELL.height as usize) as f64,
    );
    // Over the focused lower pane.
    app.pointer_move_for_test(w / 2.0, h - 4.0);
    app.dispatch_pixel_wheel_for_test(1.0e6);
    let offset = app.viewport_offset_for_test();
    assert!(offset > 0, "the pane scrolled");
    assert!(
        offset <= dims.rows,
        "one delta moves at most the pane's {} rows, moved {offset}",
        dims.rows
    );
}

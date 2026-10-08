// SPDX-License-Identifier: GPL-3.0-only
//! A held selection drag keeps extending through the real App input path when
//! the pointer leaves the grid that owns it: in a split, across padding, a
//! divider, the other pane, the window edge, and the tab chrome, clamped to the
//! owning pane's nearest edge cell and scrolling that pane's history from its
//! own first and last rows; on a single pane, across the tab chrome. Focus never
//! moves during the drag, and a new press or hover keeps the strict mapping.

use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 40;
const ROWS: usize = 12;
const HISTORY: &[u8] = b"history line\r\n";

type Token = crate::native::session::SessionToken;

/// A `COLUMNS` x `ROWS` window (plus `top_rows` of tab chrome) split along
/// `columns`. Both panes hold scrollback; the new pane is focused. Returns the
/// app and the `(first, new)` tokens.
fn split_app(columns: bool, settings: Settings, top_rows: usize) -> (App, (Token, Token)) {
    let (mut app, first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        settings,
    );
    first.lock().expect("terminal").advance(&HISTORY.repeat(60));
    let old = app.active_session_token_for_test();
    let pane_dims = if columns {
        Dimensions::new(COLUMNS / 2 - 1, ROWS)
    } else {
        Dimensions::new(COLUMNS, ROWS / 2 - 1)
    };
    let pane = Arc::new(Mutex::new(Terminal::new(pane_dims.columns, pane_dims.rows)));
    pane.lock().expect("terminal").advance(&HISTORY.repeat(60));
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(std::io::sink())));
    app.seed_headless_split_pane_for_test(columns, pane, writer, pane_dims);
    let new = app.active_session_token_for_test();
    for token in [old, new] {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(CELL);
        app.set_test_surface_for_test(
            COLUMNS as u32 * CELL.width,
            (ROWS + top_rows) as u32 * CELL.height,
            crate::native::WindowPadding::ZERO,
        );
    }
    app.focus_session_token_for_test(new);
    app.reflow_active_panes_for_test();
    (app, (old, new))
}

/// The window point at the centre of the focused pane's drawn cell
/// (`row`, `column`).
fn pane_cell_px(app: &App, row: usize, column: usize) -> (f64, f64) {
    let (_, origin) = app.focused_pane_grid_for_test().expect("pane geometry");
    (
        f64::from(origin[0]) + (column as f64 + 0.5) * f64::from(CELL.width),
        f64::from(origin[1]) + (row as f64 + 0.5) * f64::from(CELL.height),
    )
}

/// The focused pane's drawn grid rows.
fn pane_rows(app: &App) -> usize {
    let (inner, origin) = app.focused_pane_grid_for_test().expect("pane geometry");
    ((inner[1] + inner[3] - origin[1]) / CELL.height as f32) as usize
}

fn press_at(app: &mut App, (x, y): (f64, f64)) {
    app.pointer_move_for_test(x, y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    assert!(app.selecting_for_test(), "the press began a selection");
}

fn require_history(app: &App) -> bool {
    if app.scrollback_len_for_test() == 0 {
        eprintln!("skipping: no scrollback materialized");
        return false;
    }
    true
}

#[test]
fn a_held_drag_into_the_other_pane_clamps_to_the_owning_pane_edge() {
    let (mut app, (_, right)) = split_app(true, Settings::default(), 0);
    let start = pane_cell_px(&app, 2, 5);
    press_at(&mut app, start);
    let (_, y) = pane_cell_px(&app, 2, 0);
    // Over the left pane, past the divider and the right pane's padding.
    app.pointer_move_for_test(f64::from(CELL.width), y);
    assert_eq!(app.active_session_token_for_test(), right, "focus stays");
    let (start_row, start_col, end_row, end_col) = app
        .selection_range_for_test()
        .expect("the drag reached the right pane's first column");
    assert_eq!(start_row, end_row, "the drag stays on its row");
    assert_eq!((start_col, end_col), (0, 5));
    assert_eq!(
        app.viewport_offset_for_test(),
        0,
        "a sideways drag never scrolls"
    );
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    // Once released, hover over the other pane has no cell again.
    app.pointer_move_for_test(f64::from(CELL.width), y);
    assert_eq!(app.pointer_cell_for_test(), None);
}

#[test]
fn a_held_drag_into_the_pane_above_scrolls_the_owning_pane() {
    let (mut app, (_, bottom)) = split_app(false, Settings::default(), 0);
    if !require_history(&app) {
        return;
    }
    let start = pane_cell_px(&app, 2, 4);
    press_at(&mut app, start);
    let (x, _) = pane_cell_px(&app, 0, 4);
    // Into the top pane, above the divider.
    app.pointer_move_for_test(x, 1.5 * f64::from(CELL.height));
    assert_eq!(app.active_session_token_for_test(), bottom, "focus stays");
    assert!(
        app.viewport_offset_for_test() > 0,
        "the bottom pane scrolled into its history"
    );
    assert!(
        app.selection_range_for_test().is_some(),
        "the selection grew toward the pane's first row"
    );
}

#[test]
fn the_pane_first_row_is_the_upward_autoscroll_band() {
    let (mut app, _) = split_app(false, Settings::default(), 0);
    if !require_history(&app) {
        return;
    }
    let start = pane_cell_px(&app, 2, 4);
    press_at(&mut app, start);
    // Inside the bottom pane's first drawn row, mid-window.
    let (x, y) = pane_cell_px(&app, 0, 4);
    app.pointer_move_for_test(x, y);
    assert!(
        app.viewport_offset_for_test() > 0,
        "the pane's own first row starts the upward autoscroll"
    );
}

#[test]
fn a_held_drag_past_the_window_bottom_reaches_the_pane_last_row() {
    let (mut app, _) = split_app(false, Settings::default(), 0);
    let start = pane_cell_px(&app, 2, 4);
    press_at(&mut app, start);
    let rows = pane_rows(&app);
    let (x, _) = pane_cell_px(&app, 0, 4);
    app.pointer_move_for_test(x, (ROWS as f64 + 2.0) * f64::from(CELL.height));
    let (start_row, start_col, end_row, end_col) = app
        .selection_range_for_test()
        .expect("the drag reached the pane's last row");
    assert_eq!(
        end_row - start_row,
        rows - 1 - 2,
        "clamped to the last drawn row"
    );
    assert_eq!((start_col, end_col), (4, 4));
    assert_eq!(
        app.viewport_offset_for_test(),
        0,
        "already at the live bottom"
    );
}

#[test]
fn a_held_split_drag_over_the_tab_bar_scrolls_instead_of_hovering_a_tab() {
    let settings = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    let (mut app, (top, _)) = split_app(false, settings, 2);
    app.focus_session_token_for_test(top);
    if !require_history(&app) {
        return;
    }
    let (_, chrome_dy) = app.tab_chrome_offset_px_for_test().expect("chrome");
    assert!(chrome_dy > 0.0, "the top bar is shown");
    let tab = (2.0 * f64::from(CELL.width), chrome_dy / 2.0);
    app.pointer_move_for_test(tab.0, tab.1);
    assert_eq!(app.tab_bar_hit_for_test(), Some("switch"), "a tab is there");
    let start = pane_cell_px(&app, 2, 2);
    press_at(&mut app, start);
    app.pointer_move_for_test(tab.0, tab.1);
    assert_eq!(app.active_session_token_for_test(), top, "focus stays");
    assert!(!app.top_tab_hovered_for_test(), "no tab hover mid-drag");
    assert!(app.viewport_offset_for_test() > 0, "the top pane scrolled");
}

#[test]
fn a_held_single_pane_drag_over_the_tab_bar_scrolls_instead_of_hovering_a_tab() {
    let settings = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        settings,
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(&HISTORY.repeat(60));
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        (ROWS as u32 + 2) * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    if !require_history(&app) {
        return;
    }
    let (_, chrome_dy) = app.tab_chrome_offset_px_for_test().expect("chrome");
    assert!(chrome_dy > 0.0, "the top bar is shown");
    let tab = (2.0 * f64::from(CELL.width), chrome_dy / 2.0);
    app.pointer_move_for_test(tab.0, tab.1);
    assert_eq!(app.tab_bar_hit_for_test(), Some("switch"), "a tab is there");
    press_at(&mut app, (20.0, chrome_dy + 2.5 * f64::from(CELL.height)));
    app.pointer_move_for_test(tab.0, tab.1);
    assert!(!app.top_tab_hovered_for_test(), "no tab hover mid-drag");
    assert!(app.viewport_offset_for_test() > 0, "the drag scrolled");
}

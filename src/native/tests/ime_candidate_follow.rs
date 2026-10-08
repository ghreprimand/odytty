// SPDX-License-Identifier: GPL-3.0-only
//! The IME candidate window follows its cursor cell during a composition,
//! through the redraw's follow step of a headless App: a program moving the
//! cursor or a window resize moving the grid relocates it without a new pre-edit,
//! an unchanged anchor is not reissued, and an empty pre-edit sends nothing.

use winit::event::Ime;

use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};

fn app() -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(40, 12),
        Settings::default(),
    );
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        40 * CELL.width,
        12 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    (app, terminal)
}

#[test]
fn a_cursor_move_during_a_composition_moves_the_candidate_window() {
    let (mut app, terminal) = app();
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    let first = app.follow_ime_cursor_area_for_test().expect("area sent");
    assert_eq!(first.0, [0.0, 0.0], "the cursor starts at the origin");
    // The program moves the cursor to row 5, column 10 (1-based 6;11).
    terminal.lock().expect("terminal").advance(b"\x1b[6;11H");
    let moved = app.follow_ime_cursor_area_for_test().expect("area sent");
    assert_eq!(
        moved,
        (
            [10.0 * CELL.width as f32, 5.0 * CELL.height as f32],
            [CELL.width, CELL.height]
        ),
        "the candidate window follows the cursor without a new pre-edit"
    );
    assert_eq!(app.ime_cursor_area_origin_for_test(), Some(moved.0));
}

#[test]
fn a_window_resize_during_a_composition_moves_the_candidate_window() {
    let (mut app, _terminal) = app();
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    let before = app.follow_ime_cursor_area_for_test().expect("area sent");
    // The window-resize path with new padding insets the grid.
    let padding = crate::native::WindowPadding::from_logical(6.0, 1.0);
    let (width, height) = (40 * CELL.width + 12, 12 * CELL.height + 12);
    app.set_test_surface_for_test(width, height, padding);
    app.resize_grid_with_padding_for_test(CELL, padding, width, height);
    let after = app.follow_ime_cursor_area_for_test().expect("area sent");
    assert_eq!(
        after.0,
        [before.0[0] + 6.0, before.0[1] + 6.0],
        "the candidate window moved with the padded grid"
    );
    assert_eq!(app.ime_cursor_area_origin_for_test(), Some(after.0));
}

#[test]
fn an_empty_preedit_sends_no_candidate_area() {
    let (mut app, terminal) = app();
    assert_eq!(
        app.follow_ime_cursor_area_for_test(),
        None,
        "no composition"
    );
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    assert!(app.follow_ime_cursor_area_for_test().is_some());
    app.handle_ime(Ime::Preedit(String::new(), None));
    terminal.lock().expect("terminal").advance(b"\x1b[3;3H");
    assert_eq!(
        app.follow_ime_cursor_area_for_test(),
        None,
        "an ended composition is not followed"
    );
}

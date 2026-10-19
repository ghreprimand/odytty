// SPDX-License-Identifier: GPL-3.0-only
//! A triple-click selects the whole logical line under the pointer: every
//! physical row the terminal joined with a soft wrap, including rows above
//! or below the viewport, up to the hard line breaks around it. That is the
//! same text a drag across the wrap selects, and the unit xterm documents for
//! lines it wrapped itself. A line drag after a triple-click grows by whole
//! logical lines. Bidi reordering changes only where cells are drawn, so the
//! unit is the same with it on. Platform-neutral App tests over a headless
//! session.

use super::*;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};

fn app_with_content(columns: usize, rows: usize, content: &[u8]) -> App {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(columns, rows),
        Settings::default(),
    );
    terminal.lock().expect("terminal").advance(content);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        columns as u32 * CELL.width,
        rows as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app
}

/// Three press/release pairs at the centre of visible cell (`row`,
/// `column`) through the real pointer and mouse-button dispatch.
fn triple_click(app: &mut App, row: usize, column: usize) {
    let x = (column as f64 + 0.5) * f64::from(CELL.width);
    let y = (row as f64 + 0.5) * f64::from(CELL.height);
    app.pointer_move_for_test(x, y);
    assert_eq!(
        app.pointer_cell_for_test().map(|point| point.row),
        Some(row),
        "the pointer is over row {row}"
    );
    for _ in 0..3 {
        app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
        app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    }
}

/// A 109-byte line of ASCII words, the length that wraps once at 80 columns.
fn long_line() -> String {
    let mut line = String::new();
    let mut word = 0;
    while line.len() < 109 {
        line.push_str(&format!("w{word:02} "));
        word += 1;
    }
    line.truncate(109);
    line
}

#[test]
fn a_triple_click_on_either_row_of_a_wrapped_line_copies_the_whole_line() {
    let line = long_line();
    let content = format!("before\r\n{line}\r\nafter");
    for row in [1, 2] {
        let mut app = app_with_content(80, 6, content.as_bytes());
        triple_click(&mut app, row, 3);
        assert_eq!(
            app.copy_shortcut_text_for_test().as_deref(),
            Some(line.as_str()),
            "row {row}: all 109 bytes, no neighbouring line"
        );
    }
}

#[test]
fn a_triple_click_matches_a_drag_across_the_wrap() {
    let line = long_line();
    let mut app = app_with_content(80, 6, format!("{line}\r\nnext").as_bytes());
    app.set_pointer_cell_for_test(0, 0);
    app.begin_selection_for_test();
    app.extend_drag_to_cell_for_test(1, 109 - 80 - 1);
    app.finish_selection_for_test();
    let dragged = app.copy_shortcut_text_for_test();
    triple_click(&mut app, 1, 0);
    assert_eq!(app.copy_shortcut_text_for_test(), dragged);
}

#[test]
fn a_line_that_began_above_the_viewport_is_selected_whole() {
    // 75 characters wrap over four 20-column rows; with a 3-row window the
    // first row is in history.
    let line: String = (0..75).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    let mut app = app_with_content(20, 3, format!("{line}\r\nz").as_bytes());
    assert!(
        app.scrollback_len_for_test() >= 1,
        "the line reaches history"
    );
    triple_click(&mut app, 0, 5);
    assert_eq!(
        app.copy_shortcut_text_for_test().as_deref(),
        Some(line.as_str())
    );
}

#[test]
fn a_line_drag_after_a_triple_click_grows_by_whole_logical_lines() {
    let first: String = "x".repeat(30);
    let second: String = "y".repeat(25);
    let mut app = app_with_content(20, 8, format!("{first}\r\n{second}\r\nend").as_bytes());
    app.set_pointer_cell_for_test(0, 2);
    for i in 0..3 {
        if i > 0 {
            app.finish_selection_for_test();
            app.set_pointer_cell_for_test(0, 2);
        }
        app.begin_selection_for_test();
    }
    assert_eq!(
        app.selection_text_for_test().as_deref(),
        Some(first.as_str())
    );
    // Onto the first row of the second line: the whole second line joins.
    app.extend_drag_to_cell_for_test(2, 0);
    assert_eq!(
        app.selection_text_for_test(),
        Some(format!("{first}\n{second}"))
    );
}

#[test]
fn a_triple_click_with_bidi_reordering_on_selects_the_same_logical_line() {
    let mut line = String::new();
    for _ in 0..4 {
        line.push_str("\u{05D0}\u{05D1}\u{05D2} abc ");
    }
    let content = format!("{line}\r\nnext");
    let plain = {
        let mut app = app_with_content(20, 6, content.as_bytes());
        triple_click(&mut app, 1, 2);
        app.copy_shortcut_text_for_test()
    };
    let mut app = app_with_content(20, 6, content.as_bytes());
    app.present_bidi_frame_map_for_test();
    assert!(
        !app.bidi_frame_map_for_test().expect("map").is_identity(),
        "the fixture reorders"
    );
    triple_click(&mut app, 1, 2);
    let reordered = app.copy_shortcut_text_for_test();
    assert_eq!(reordered, plain);
    assert_eq!(reordered.as_deref(), Some(line.trim_end()));
}

#[test]
fn a_line_unit_on_a_long_wrapped_line_copies_no_history_cells() {
    // 4,000 characters wrap over 200 rows of 20 columns; with a 5-row window
    // almost all of them are in history. Finding the line's ends reads the
    // wrap structure only, so neither the triple-click nor any line-drag
    // motion projects the line's history cells.
    let line: String = (0..4000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let mut app = app_with_content(20, 5, format!("{line}\r\nend").as_bytes());
    assert!(
        app.scrollback_len_for_test() >= 190,
        "the line reaches history"
    );
    let before = crate::core::hydrated_history_cells_for_test();
    app.set_pointer_cell_for_test(0, 2);
    for i in 0..3 {
        if i > 0 {
            app.finish_selection_for_test();
            app.set_pointer_cell_for_test(0, 2);
        }
        app.begin_selection_for_test();
    }
    for motion in 0..10 {
        app.extend_drag_to_cell_for_test(motion % 3, 1);
    }
    let hydrated = crate::core::hydrated_history_cells_for_test() - before;
    assert_eq!(hydrated, 0, "history cells projected for the line unit");
    let text = app.selection_text_for_test().expect("a selection");
    assert!(text.starts_with(&line), "the whole long line is selected");
}

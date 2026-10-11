// SPDX-License-Identifier: GPL-3.0-only
//! Copy-mode word motions that cross the viewport edge onto a wide-glyph word
//! rest on the glyph's lead cell, exactly as they do for visible rows. The
//! off-screen lookup carries owner (continuation) information, not only the
//! resolved character, which names a spacer's lead.

use super::*;

/// Seven rows of a six-column, three-row terminal: `a`..`e`, a wide word, `ok`.
/// Rows 0-3 are scrollback, row 4 is the top of the live screen.
fn wide_word_app() -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(6, 3),
        Settings::default(),
    );
    terminal
        .lock()
        .expect("terminal")
        .advance("a\r\nb\r\nc\r\nd\r\ne\r\n\u{754C}\u{5B57}\r\nok".as_bytes());
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    (app, terminal)
}

fn caret(app: &App) -> (usize, usize) {
    let point = app
        .copy_mode_cursor_for_test()
        .expect("copy mode is active");
    (point.row, point.column)
}

#[test]
fn word_end_from_the_viewport_bottom_rests_on_the_wide_lead_off_screen() {
    let (mut app, _terminal) = wide_word_app();
    app.handle_palette_action_for_test("copy-mode");
    // Walk the caret up until the view has scrolled so row 5 is below it.
    for _ in 0..4 {
        app.drive_text_key_for_test("k");
    }
    for _ in 0..2 {
        app.drive_text_key_for_test("j");
    }
    assert_eq!(caret(&app).0, 4, "the caret is on the last visible row");
    assert!(
        app.viewport_offset_for_test() > 0,
        "the wide word is below the viewport"
    );
    app.drive_text_key_for_test("e");
    assert_eq!(
        caret(&app),
        (5, 2),
        "the word end rests on the lead of the last wide glyph, not its spacer"
    );
    app.drive_text_key_for_test("e");
    assert_eq!(
        caret(&app),
        (6, 1),
        "a second `e` advances to the next word"
    );
}

// SPDX-License-Identifier: GPL-3.0-only
//! The IME candidate window follows its cursor cell during a composition,
//! through the redraw's follow step of a headless App: a program moving the
//! cursor or a window resize moving the grid relocates it without a new pre-edit,
//! an unchanged anchor is not reissued, and an empty pre-edit sends nothing.
//! Through the real redraw with bidirectional reordering on, the window
//! anchors at the cursor cell the same frame draws, in a single-pane tab and
//! in a split, whose focused pane shows the composition inline.

use winit::event::Ime;

use super::*;
use crate::native::session::SessionToken;

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

/// Logical "ab אבג xy" with the cursor on the first Hebrew letter (logical
/// column 3), which a reordered row draws at column 5.
const BIDI_ROW: &[u8] = "ab \u{05D0}\u{05D1}\u{05D2} xy\x1b[1;4H".as_bytes();

/// Starting a composition on a reordered row: the pre-edit row is drawn in
/// identity order by the same redraw, so the candidate window the redraw
/// sends anchors at the cursor cell that frame draws (logical column 3), not
/// the previous frame's reordered column 5.
#[test]
fn a_composition_on_a_reordered_row_anchors_at_the_drawn_cursor() {
    let (mut app, terminal) = app();
    terminal.lock().expect("terminal").advance(BIDI_ROW);
    app.set_bidi_display_for_test(true);
    let _ = app.redraw_single_pane_probe_for_test();
    assert_eq!(
        app.ime_cursor_area_origin_for_test(),
        Some([5.0 * CELL.width as f32, 0.0]),
        "the previous frame drew the cursor reordered"
    );
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    let _ = app.redraw_single_pane_probe_for_test();
    let drawn = app.ime_cursor_area_origin_for_test().expect("anchor");
    assert_eq!(
        drawn,
        [3.0 * CELL.width as f32, 0.0],
        "drawn in identity order"
    );
    assert_eq!(
        app.ime_cursor_area_sent_for_test().map(|area| area.0),
        Some(drawn),
        "the redraw sends the cursor cell it draws"
    );
}

/// A two-pane split whose focused second pane prints [`BIDI_ROW`], with
/// reordering on and one split frame presented. Returns the app, the focused
/// pane's terminal and its token.
fn split_app() -> (App, Arc<Mutex<Terminal>>, SessionToken) {
    let (mut app, _terminal) = app();
    let dims = Dimensions::new(19, 12);
    let pane = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    pane.lock().expect("pane").advance(BIDI_ROW);
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(std::io::sink())));
    app.seed_headless_split_pane_for_test(true, Arc::clone(&pane), writer, dims);
    let focused = app.active_session_token_for_test();
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(CELL);
        app.set_test_surface_for_test(
            40 * CELL.width,
            12 * CELL.height,
            crate::native::WindowPadding::ZERO,
        );
    }
    app.focus_session_token_for_test(focused);
    app.reflow_active_panes_for_test();
    app.set_bidi_display_for_test(true);
    let _ = app.redraw_multipane_probe_for_test();
    (app, pane, focused)
}

/// The screen column the focused pane's presented map draws logical column 3
/// of the first row at.
fn focused_column(app: &App, focused: SessionToken) -> Option<usize> {
    app.bidi_pane_map_for_test(focused)
        .map(|map| map.visual_column(0, 3))
}

/// In the focused pane of a split frame: the program rewrites the reordered
/// row as plain text while a composition is open, so the next frame draws
/// that row in identity order, and the redraw's candidate window anchors at
/// the cursor cell that frame draws, not the previous frame's placement.
#[test]
fn a_split_composition_follows_the_frame_drawn_after_a_row_rewrite() {
    let (mut app, pane, focused) = split_app();
    let column = |app: &App| focused_column(app, focused);
    assert_eq!(
        column(&app),
        Some(5),
        "the previous frame drew it reordered"
    );
    let reordered = app.ime_cursor_area_origin_for_test().expect("anchor");
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    // The program rewrites the row as plain text; the cursor stays logical 3.
    pane.lock()
        .expect("pane")
        .advance(b"\x1b[1;1Habcdefghij\x1b[1;4H");
    let _ = app.redraw_multipane_probe_for_test();
    assert_eq!(
        column(&app),
        Some(3),
        "the rewritten row is drawn in identity order"
    );
    let drawn = app.ime_cursor_area_origin_for_test().expect("anchor");
    assert_eq!(drawn[0], reordered[0] - 2.0 * CELL.width as f32);
    assert_eq!(
        app.ime_cursor_area_sent_for_test().map(|area| area.0),
        Some(drawn),
        "the redraw sends the cursor cell the focused pane draws"
    );
}

/// A split frame's focused pane shows the composition inline at its cursor,
/// like a single-pane tab; the pre-edit row then draws in identity order and
/// the redraw sends the cursor cell that frame draws.
#[test]
fn a_split_composition_on_a_reordered_row_anchors_at_the_drawn_cursor() {
    let (mut app, _pane, focused) = split_app();
    let column = |app: &App| focused_column(app, focused);
    assert_eq!(
        column(&app),
        Some(5),
        "the previous frame drew it reordered"
    );
    let reordered = app.ime_cursor_area_origin_for_test().expect("anchor");
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    let probes = app.redraw_multipane_probe_for_test();
    let shown = probes.iter().find(|probe| probe.focused).expect("focused");
    assert!(
        shown.rows[0].starts_with("ab \u{4e2d}"),
        "the focused pane shows the composition inline: {:?}",
        shown.rows
    );
    assert_eq!(
        column(&app),
        Some(3),
        "the pre-edit row is drawn in identity order"
    );
    let drawn = app.ime_cursor_area_origin_for_test().expect("anchor");
    assert_eq!(drawn[0], reordered[0] - 2.0 * CELL.width as f32);
    assert_eq!(
        app.ime_cursor_area_sent_for_test().map(|area| area.0),
        Some(drawn),
        "the redraw sends the cursor cell the focused pane draws"
    );
}

/// The candidate window anchors at the cursor the presented frame drew. A
/// cursor move that lands after the frame captured its snapshot does not move
/// the window to a cell no frame drew; the next frame draws the new cursor
/// and the window follows it then.
#[test]
fn the_candidate_window_follows_the_cursor_the_frame_drew() {
    let (mut app, terminal) = app();
    terminal.lock().expect("terminal").advance(b"\x1b[1;4H");
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    let _ = app.redraw_single_pane_probe_for_test();
    let drawn = [3.0 * CELL.width as f32, 0.0];
    assert_eq!(
        app.ime_cursor_area_sent_for_test().map(|area| area.0),
        Some(drawn)
    );
    // Output moves the cursor after the frame captured it.
    terminal.lock().expect("terminal").advance(b"\x1b[2;9H");
    let followed = app.follow_ime_cursor_area_for_test().expect("area sent");
    assert_eq!(followed.0, drawn, "still the cursor the frame drew");
    let _ = app.redraw_single_pane_probe_for_test();
    assert_eq!(
        app.ime_cursor_area_sent_for_test().map(|area| area.0),
        Some([8.0 * CELL.width as f32, CELL.height as f32]),
        "the next frame draws the new cursor and the window follows"
    );
}

/// The same in the focused pane of a split frame.
#[test]
fn a_split_candidate_window_follows_the_cursor_the_frame_drew() {
    let (mut app, pane, _focused) = split_app();
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    let _ = app.redraw_multipane_probe_for_test();
    let drawn = app
        .ime_cursor_area_sent_for_test()
        .map(|area| area.0)
        .expect("area sent");
    pane.lock().expect("pane").advance(b"\x1b[4;2H");
    let followed = app.follow_ime_cursor_area_for_test().expect("area sent");
    assert_eq!(followed.0, drawn, "still the cursor the frame drew");
    let _ = app.redraw_multipane_probe_for_test();
    let next = app
        .ime_cursor_area_sent_for_test()
        .map(|area| area.0)
        .expect("area sent");
    assert_eq!(
        next[1] - drawn[1],
        3.0 * CELL.height as f32,
        "the next frame draws the new cursor and the window follows"
    );
}

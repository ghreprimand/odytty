// SPDX-License-Identifier: GPL-3.0-only
//! Pointer routing in a split tab through the real App input path: a left
//! press in the focused pane follows the same report, open, and selection
//! ladder as a single pane; the pointer maps against the origin the pane's
//! glyphs are drawn from; and a window-level modal owns pointer motion over
//! every pane.
//!
//! A left press that keeps the focused pane falls through to the shared
//! single-pane ladder. Each arm after it is covered here or elsewhere: the
//! chrome arms (a content press with the tab bar shown, and a right press under
//! it), the latched-button and selection releases, the open-modifier and
//! report arms, the content context menu, and the middle-click PRIMARY paste.
//! The scroll-thumb arm cannot take a split press: a split tab draws no thumb,
//! which `split_pointer_bounds` checks across the whole thumb band.

use std::io::Write;

use super::*;
use crate::selection::CellPoint;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 40;
const ROWS: usize = 8;

type Recorded = Arc<Mutex<Vec<u8>>>;

struct RecordingWriter(Recorded);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("bytes").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A `COLUMNS` x `ROWS` window split into two columns. The new right pane is
/// focused, has processed `output`, and records its PTY bytes. Returns the app,
/// the right pane's recorder, and the `(left, right)` tokens.
fn split_app(output: &[u8]) -> (App, Recorded, (SessionTokenAlias, SessionTokenAlias)) {
    split_app_with(Settings::default(), output)
}

fn split_app_with(
    settings: Settings,
    output: &[u8],
) -> (App, Recorded, (SessionTokenAlias, SessionTokenAlias)) {
    let (mut app, _first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        settings,
    );
    let left = app.active_session_token_for_test();
    let pane_dims = Dimensions::new(COLUMNS / 2 - 1, ROWS);
    let pane = Arc::new(Mutex::new(Terminal::new(pane_dims.columns, pane_dims.rows)));
    pane.lock().expect("terminal").advance(output);
    let recorded = Recorded::default();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter(recorded.clone()))));
    app.seed_headless_split_pane_for_test(true, pane, writer, pane_dims);
    let right = app.active_session_token_for_test();
    for token in [left, right] {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(CELL);
        app.set_test_surface_for_test(
            COLUMNS as u32 * CELL.width,
            ROWS as u32 * CELL.height,
            crate::native::WindowPadding::ZERO,
        );
    }
    app.focus_session_token_for_test(right);
    app.reflow_active_panes_for_test();
    (app, recorded, (left, right))
}

type SessionTokenAlias = crate::native::session::SessionToken;

fn window_px() -> (f64, f64) {
    (
        (COLUMNS * CELL.width as usize) as f64,
        (ROWS * CELL.height as usize) as f64,
    )
}

/// A point inside the right pane, a few cells in.
fn right_pane_point() -> (f64, f64) {
    let (w, _) = window_px();
    (
        w - 6.5 * f64::from(CELL.width),
        2.5 * f64::from(CELL.height),
    )
}

fn take(recorded: &Recorded) -> String {
    let mut bytes = recorded.lock().expect("bytes");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    bytes.clear();
    text
}

fn click(app: &mut App, (x, y): (f64, f64)) {
    app.pointer_move_for_test(x, y);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
}

#[test]
fn a_left_click_in_the_focused_split_pane_reaches_a_mouse_reporting_program() {
    let (mut app, recorded, _) = split_app(b"\x1b[?1000h\x1b[?1006h");
    take(&recorded);
    click(&mut app, right_pane_point());
    let reports = take(&recorded);
    assert!(
        reports.contains("\x1b[<0;") && reports.ends_with('m'),
        "the press and its release are reported: {reports:?}"
    );
    assert!(!app.selecting_for_test(), "no local selection began");
}

#[test]
fn shift_still_selects_locally_in_a_reporting_split_pane() {
    let (mut app, recorded, _) = split_app(b"\x1b[?1000h\x1b[?1006h");
    take(&recorded);
    app.set_shift_modifier_for_test(true);
    let point = right_pane_point();
    app.pointer_move_for_test(point.0, point.1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    assert!(app.selecting_for_test(), "Shift overrides reporting");
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert_eq!(take(&recorded), "", "nothing reaches the program");
}

#[test]
fn a_focus_transfer_click_into_a_reporting_pane_reports_neither_half() {
    let (mut app, recorded, (left, right)) = split_app(b"\x1b[?1000h\x1b[?1006h");
    app.focus_session_token_for_test(left);
    take(&recorded);
    click(&mut app, right_pane_point());
    assert_eq!(app.active_session_token_for_test(), right, "focus moved");
    assert_eq!(
        take(&recorded),
        "",
        "the focusing press and its release stay local"
    );
    // The next click in the now-focused pane is an ordinary reported click.
    click(&mut app, right_pane_point());
    assert!(take(&recorded).contains("\x1b[<0;"));
}

/// A 40-column window split in two leaves the left leaf a sub-cell remainder,
/// so its glyphs are drawn shifted right, flush to the divider. The pointer
/// resolves against that drawn origin.
#[test]
fn the_left_pane_pointer_cell_matches_the_column_drawn_under_it() {
    let (mut app, _recorded, (left, _)) = split_app(b"");
    app.focus_session_token_for_test(left);
    let (inner, origin) = app
        .focused_pane_grid_for_test()
        .expect("left pane geometry");
    let shift = f64::from(origin[0] - inner[0]);
    assert!(shift > 0.0, "the left leaf draws from a shifted origin");
    // Just inside drawn column 1.
    let x = f64::from(origin[0]) + f64::from(CELL.width) + 1.0;
    app.pointer_move_for_test(x, 4.0);
    assert_eq!(
        app.pointer_cell_for_test(),
        Some(CellPoint { row: 0, column: 1 })
    );
    // The leading remainder strip has no cell.
    app.pointer_move_for_test(f64::from(inner[0]) + shift / 2.0, 4.0);
    assert_eq!(app.pointer_cell_for_test(), None);
}

#[test]
fn copy_mode_owns_pointer_motion_over_every_pane() {
    let (mut app, recorded, _) = split_app(b"\x1b[?1003h\x1b[?1006h");
    app.handle_palette_action_for_test("copy-mode");
    assert!(app.modal_captures_pointer_for_test());
    take(&recorded);
    let (x, y) = right_pane_point();
    app.pointer_move_for_test(x, y);
    app.pointer_move_for_test(x + 8.0, y);
    app.pointer_move_for_test(8.0, y);
    assert_eq!(take(&recorded), "", "no motion report beneath copy mode");
}

#[test]
fn an_open_overlay_owns_motion_over_the_other_pane() {
    let (mut app, recorded, _) = split_app(b"\x1b[?1003h\x1b[?1006h");
    app.handle_palette_action_for_test("settings");
    assert!(app.overlay_open_for_test());
    take(&recorded);
    // Over the non-focused left pane and over the focused one.
    app.pointer_move_for_test(8.0, 20.0);
    let (x, y) = right_pane_point();
    app.pointer_move_for_test(x, y);
    assert_eq!(take(&recorded), "");
    assert_eq!(
        app.cursor_icon_for_test(),
        winit::window::CursorIcon::Default
    );
}

/// A split tab with the tab bar shown, so a content press passes the chrome
/// routing before it reaches the shared ladder.
fn split_app_under_tab_bar(output: &[u8]) -> (App, Recorded) {
    let settings = Settings {
        always_show_tab_bar: true,
        ..Settings::default()
    };
    let (app, recorded, _) = split_app_with(settings, output);
    assert!(app.tab_bar_visible_for_test(), "the tab bar is shown");
    (app, recorded)
}

#[test]
fn a_split_pane_click_under_the_tab_bar_reports_both_halves() {
    let (mut app, recorded) = split_app_under_tab_bar(b"\x1b[?1000h\x1b[?1006h");
    take(&recorded);
    let point = right_pane_point();
    app.pointer_move_for_test(point.0, point.1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    let press = take(&recorded);
    assert!(
        press.starts_with("\x1b[<0;") && press.ends_with('M'),
        "the press passes the chrome arms: {press:?}"
    );
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    let release = take(&recorded);
    assert!(
        release.starts_with("\x1b[<0;") && release.ends_with('m'),
        "the release is not taken as a chrome release: {release:?}"
    );
    assert_eq!(
        app.active_workspace_tab_count_for_test(),
        1,
        "no tab action ran"
    );
}

#[test]
fn a_right_press_in_a_split_pane_under_the_tab_bar_opens_the_content_menu() {
    let (mut app, _recorded) = split_app_under_tab_bar(b"");
    let point = right_pane_point();
    app.pointer_move_for_test(point.0, point.1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Right);
    assert!(app.context_menu_open_for_test());
    let labels = app.context_menu_labels_for_test();
    assert!(
        labels.contains(&"Paste Text") && labels.contains(&"Close Pane"),
        "the content menu, not the tab-strip menu: {labels:?}"
    );
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
#[test]
fn a_middle_press_in_a_split_pane_pastes_primary_into_that_pane() {
    let (mut app, recorded, _) = split_app(b"");
    app.inject_paste_text_for_test("primary words");
    take(&recorded);
    let point = right_pane_point();
    app.pointer_move_for_test(point.0, point.1);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Middle);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Middle);
    assert!(
        take(&recorded).contains("primary words"),
        "the focused pane receives the PRIMARY text"
    );
}

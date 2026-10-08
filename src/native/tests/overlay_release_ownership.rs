// SPDX-License-Identifier: GPL-3.0-only
//! Surfaces that open without the full overlay pointer reset still own the
//! buttons held when they open, through the real App input path: the right
//! press that opens a context menu, a reported or local drag interrupted by a
//! context menu or the close confirmation, and a pane-divider drag interrupted
//! by an overlay or copy mode. Each owned release stays out of a
//! mouse-reporting program even after the surface closes, and a context menu
//! keeps the selection Copy needs.

use std::io::{self, Write};

use super::*;
use winit::keyboard::NamedKey;

const CELL: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};
const COLUMNS: usize = 40;
const ROWS: usize = 12;
/// Button-event tracking (drag motion is reported) with SGR encoding.
const REPORTING: &[u8] = b"\x1b[?1002h\x1b[?1006h";

#[derive(Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

type Recorded = Arc<Mutex<Vec<u8>>>;

fn recording_writer() -> (PtyWriter, Recorded) {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    (Arc::new(Mutex::new(Box::new(recorder))), bytes)
}

fn fit(app: &mut App) {
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
}

fn app_with(output: &[u8]) -> (App, Recorded) {
    let (writer, bytes) = recording_writer();
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
        writer,
    );
    terminal.lock().expect("terminal").advance(output);
    fit(&mut app);
    app.pointer_move_for_test(40.5, 40.5);
    (app, bytes)
}

/// A two-column split whose focused (right) pane reports the mouse into
/// the returned recorder.
fn split_reporting_app() -> (App, Recorded) {
    let (mut app, _first) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
    );
    let dims = Dimensions::new(COLUMNS / 2 - 1, ROWS);
    let pane = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    pane.lock().expect("pane").advance(REPORTING);
    let (writer, bytes) = recording_writer();
    app.seed_headless_split_pane_for_test(true, pane, writer, dims);
    fit(&mut app);
    app.reflow_active_panes_for_test();
    (app, bytes)
}

fn take(recorded: &Recorded) -> String {
    let mut bytes = recorded.lock().expect("bytes");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    bytes.clear();
    text
}

fn button(app: &mut App, button: WinitMouseButton, pressed: bool) {
    app.dispatch_mouse_button_for_test(pressed, button);
}

fn escape(app: &mut App) {
    app.drive_named_key_for_test(NamedKey::Escape);
}

/// Shift+right press in a reporting program: the menu opens instead of a
/// report.
fn open_menu_with_shift_right(app: &mut App) {
    app.set_shift_modifier_for_test(true);
    button(app, WinitMouseButton::Right, true);
    assert!(app.context_menu_open_for_test(), "the menu opened");
}

#[test]
fn the_right_press_that_opened_a_menu_keeps_its_release_after_escape() {
    let (mut app, recorded) = app_with(REPORTING);
    open_menu_with_shift_right(&mut app);
    escape(&mut app);
    assert!(!app.context_menu_open_for_test(), "the menu closed");
    app.set_shift_modifier_for_test(false);
    take(&recorded);
    button(&mut app, WinitMouseButton::Right, false);
    assert_eq!(take(&recorded), "", "the program never sees the release");
    // The next right click is an ordinary reported click.
    button(&mut app, WinitMouseButton::Right, true);
    button(&mut app, WinitMouseButton::Right, false);
    let reports = take(&recorded);
    assert!(
        reports.contains("\x1b[<2;") && reports.ends_with('m'),
        "a new right click reports both halves: {reports:?}"
    );
}

#[test]
fn a_reported_drag_interrupted_by_a_menu_reports_nothing_after_it_closes() {
    let (mut app, recorded) = app_with(REPORTING);
    button(&mut app, WinitMouseButton::Left, true);
    assert!(
        take(&recorded).contains("\x1b[<0;"),
        "the press is reported"
    );
    // While a reported drag holds the report latch, a right press is
    // reported, so the menu comes from the tab strip's menu entry.
    app.open_empty_tab_strip_menu_for_test();
    assert!(app.context_menu_open_for_test(), "the menu opened");
    escape(&mut app);
    take(&recorded);
    app.pointer_move_for_test(64.5, 40.5);
    assert_eq!(take(&recorded), "", "no held-button motion after the menu");
    button(&mut app, WinitMouseButton::Left, false);
    assert_eq!(take(&recorded), "", "no left release after the menu");
}

#[test]
fn a_menu_keeps_the_selection_but_ends_its_drag() {
    let (mut app, _recorded) = app_with(b"some text to select here\r\n");
    app.pointer_move_for_test(4.5, 8.5);
    button(&mut app, WinitMouseButton::Left, true);
    app.pointer_move_for_test(60.5, 8.5);
    let selected = app.selection_range_for_test();
    assert!(selected.is_some(), "the drag selected text");
    // A held selection drag ignores a right press over content, so the
    // menu comes from the tab strip's menu entry.
    app.open_empty_tab_strip_menu_for_test();
    assert!(app.context_menu_open_for_test(), "the menu opened");
    assert_eq!(
        app.selection_range_for_test(),
        selected,
        "the menu keeps the selection for Copy"
    );
    escape(&mut app);
    assert!(!app.selecting_for_test(), "the menu ended the drag");
    app.pointer_move_for_test(200.5, 120.5);
    assert_eq!(
        app.selection_range_for_test(),
        selected,
        "motion after the menu does not extend the old drag"
    );
    button(&mut app, WinitMouseButton::Left, false);
    assert_eq!(app.selection_range_for_test(), selected);
}

#[test]
fn a_reported_press_held_into_the_close_confirmation_keeps_its_release() {
    let (mut app, recorded) = app_with(REPORTING);
    button(&mut app, WinitMouseButton::Left, true);
    assert!(
        take(&recorded).contains("\x1b[<0;"),
        "the press is reported"
    );
    app.set_foreground_jobs_running_for_test();
    app.request_window_close_for_test();
    assert!(
        app.overlay_open_for_test(),
        "the close confirmation is open"
    );
    escape(&mut app);
    assert!(!app.overlay_open_for_test(), "the confirmation closed");
    take(&recorded);
    app.pointer_move_for_test(64.5, 40.5);
    button(&mut app, WinitMouseButton::Left, false);
    assert_eq!(take(&recorded), "", "no motion or release after it closed");
}

#[test]
fn a_divider_drag_interrupted_by_an_overlay_keeps_its_left_release() {
    divider_drag_interrupted_by("settings");
}

#[test]
fn a_divider_drag_interrupted_by_copy_mode_keeps_its_left_release() {
    divider_drag_interrupted_by("copy-mode");
}

fn divider_drag_interrupted_by(surface: &str) {
    let (mut app, recorded) = split_reporting_app();
    let (tiled, inner) = app
        .focused_pane_rects_for_test()
        .expect("two-pane geometry");
    let divider_x = f64::from(tiled[0] - 0.5);
    let y = f64::from(inner[1] + inner[3] / 2.0);
    app.set_pointer_px_for_test(divider_x, y);
    button(&mut app, WinitMouseButton::Left, true);
    assert!(
        app.divider_drag_active_for_test(),
        "{surface}: divider drag"
    );
    app.handle_palette_action_for_test(surface);
    assert!(
        !app.divider_drag_active_for_test(),
        "{surface}: the divider settled"
    );
    escape(&mut app);
    assert!(
        !app.overlay_open_for_test() && !app.modal_captures_pointer_for_test(),
        "{surface}: closed"
    );
    // Back over the reporting pane's content, then let go.
    app.pointer_move_for_test(f64::from(inner[0] + inner[2] / 2.0), y);
    take(&recorded);
    button(&mut app, WinitMouseButton::Left, false);
    assert_eq!(
        take(&recorded),
        "",
        "{surface}: the divider's release never reaches the program"
    );
}

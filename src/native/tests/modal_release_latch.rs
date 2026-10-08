// SPDX-License-Identifier: GPL-3.0-only
//! A mouse button whose press belonged to the window, not the terminal, keeps
//! its release out of the terminal, through the real App input path: a press
//! captured by copy mode, the rename prompt, or an overlay, and a button already
//! held when one of them opened. The release is consumed even after the surface
//! closes, a new press of the same button starts a normal gesture, and another
//! button is unaffected.

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

fn app_with(output: &[u8]) -> (App, Recorded) {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(COLUMNS, ROWS),
        Settings::default(),
        writer,
    );
    terminal.lock().expect("terminal").advance(output);
    app.set_test_cell_for_test(CELL);
    app.set_test_surface_for_test(
        COLUMNS as u32 * CELL.width,
        ROWS as u32 * CELL.height,
        crate::native::WindowPadding::ZERO,
    );
    app.pointer_move_for_test(40.5, 40.5);
    (app, bytes)
}

fn take(recorded: &Recorded) -> String {
    let mut bytes = recorded.lock().expect("bytes");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    bytes.clear();
    text
}

fn left(app: &mut App, pressed: bool) {
    app.dispatch_mouse_button_for_test(pressed, WinitMouseButton::Left);
}

fn escape(app: &mut App) {
    app.drive_named_key_for_test(NamedKey::Escape);
}

#[test]
fn a_reported_drag_interrupted_by_copy_mode_reports_nothing_after_it_closes() {
    let (mut app, recorded) = app_with(REPORTING);
    left(&mut app, true);
    assert!(
        take(&recorded).contains("\x1b[<0;"),
        "the press is reported"
    );
    app.handle_palette_action_for_test("copy-mode");
    assert!(app.modal_captures_pointer_for_test(), "copy mode is open");
    escape(&mut app);
    assert!(!app.modal_captures_pointer_for_test(), "copy mode closed");
    take(&recorded);
    app.pointer_move_for_test(64.5, 40.5);
    assert_eq!(take(&recorded), "", "no held-button motion after the modal");
    left(&mut app, false);
    assert_eq!(take(&recorded), "", "no release after the modal");
}

#[test]
fn a_press_captured_by_the_rename_prompt_keeps_its_release_after_escape() {
    let (mut app, recorded) = app_with(REPORTING);
    assert!(
        app.begin_rename_tab_for_test(0),
        "the rename prompt is open"
    );
    take(&recorded);
    left(&mut app, true);
    escape(&mut app);
    assert!(app.rename_text_for_test().is_none(), "the prompt closed");
    left(&mut app, false);
    assert_eq!(take(&recorded), "", "the program never sees the release");
    // The next click is an ordinary reported click.
    left(&mut app, true);
    left(&mut app, false);
    let reports = take(&recorded);
    assert!(
        reports.contains('M') && reports.ends_with('m'),
        "a new click reports both halves: {reports:?}"
    );
}

#[test]
fn a_press_captured_by_an_overlay_keeps_its_release_after_escape() {
    let (mut app, recorded) = app_with(REPORTING);
    app.handle_palette_action_for_test("settings");
    assert!(app.overlay_open_for_test(), "settings is open");
    left(&mut app, true);
    escape(&mut app);
    assert!(!app.overlay_open_for_test(), "settings closed");
    take(&recorded);
    left(&mut app, false);
    assert_eq!(take(&recorded), "", "the program never sees the release");
}

#[test]
fn a_lost_release_does_not_swallow_the_next_gesture() {
    let (mut app, recorded) = app_with(REPORTING);
    assert!(app.begin_rename_tab_for_test(0));
    left(&mut app, true);
    escape(&mut app);
    take(&recorded);
    // The release never arrived; a new press starts a normal gesture.
    left(&mut app, true);
    left(&mut app, false);
    let reports = take(&recorded);
    assert!(
        reports.contains('M') && reports.ends_with('m'),
        "both halves of the new gesture are reported: {reports:?}"
    );
}

#[test]
fn another_button_is_unaffected_by_an_owned_left_release() {
    let (mut app, recorded) = app_with(REPORTING);
    assert!(app.begin_rename_tab_for_test(0));
    left(&mut app, true);
    escape(&mut app);
    take(&recorded);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Middle);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Middle);
    let reports = take(&recorded);
    assert!(
        reports.contains("\x1b[<1;") && reports.ends_with('m'),
        "the middle click is reported: {reports:?}"
    );
    left(&mut app, false);
    assert_eq!(
        take(&recorded),
        "",
        "the owned left release is still consumed"
    );
}

#[test]
fn a_local_selection_interrupted_by_copy_mode_does_not_resume() {
    let (mut app, _recorded) = app_with(b"");
    left(&mut app, true);
    assert!(app.selecting_for_test(), "the press began a selection");
    app.handle_palette_action_for_test("copy-mode");
    escape(&mut app);
    assert!(!app.selecting_for_test(), "the modal ended the drag");
    let before = app.selection_range_for_test();
    app.pointer_move_for_test(200.5, 120.5);
    assert_eq!(
        app.selection_range_for_test(),
        before,
        "motion after the modal does not extend the old drag"
    );
    left(&mut app, false);
    assert!(!app.selecting_for_test());
}

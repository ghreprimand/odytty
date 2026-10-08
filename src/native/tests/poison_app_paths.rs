// SPDX-License-Identifier: GPL-3.0-only
//! Every live terminal-model read and write uses the shared poison-recovery
//! policy, the same one the PTY reader and the renderer use. These tests drive
//! the real App paths (paste, split render, cursor blink, selected-input
//! delete, key and mouse encoding, focus reports, alternate scroll, bells, and
//! resize): a poisoned model is still read for what the child enabled and
//! still updated, never treated as "mode off", "pane absent", "no selection",
//! or "nothing to do".

use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;
use winit::keyboard::NamedKey;

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

fn recording_app(output: &[u8]) -> (App, Recorded, Arc<Mutex<Terminal>>) {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );
    terminal.lock().expect("terminal").advance(output);
    (app, bytes, terminal)
}

/// Poison `terminal` by panicking while holding its guard, with the panic
/// hook silenced so the expected panic prints nothing.
fn poison(terminal: &Arc<Mutex<Terminal>>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = terminal.lock().expect("terminal");
        panic!("poison the terminal model");
    }));
    std::panic::set_hook(previous);
    assert!(terminal.lock().is_err(), "the model is poisoned");
}

fn bytes_of(recorded: &Recorded) -> Vec<u8> {
    recorded.lock().expect("bytes").clone()
}

#[test]
fn a_poisoned_bracketed_session_still_receives_a_bracketed_paste() {
    let (mut app, recorded, terminal) = recording_app(b"\x1b[?2004h");
    poison(&terminal);
    app.inject_paste_text_for_test("first\nsecond");
    app.handle_paste_shortcut_for_test();
    assert!(
        !app.risky_paste_pending_for_test(),
        "a bracketed child needs no confirmation"
    );
    let written = String::from_utf8_lossy(&bytes_of(&recorded)).into_owned();
    assert!(
        written.starts_with("\x1b[200~") && written.ends_with("\x1b[201~"),
        "the paste is framed for the bracketed child: {written:?}"
    );
}

#[test]
fn a_held_paste_confirmed_after_poison_is_still_delivered() {
    let (mut app, recorded, terminal) = recording_app(b"");
    app.inject_paste_text_for_test("first\nsecond");
    app.handle_paste_shortcut_for_test();
    assert!(app.risky_paste_pending_for_test(), "the paste is held");
    poison(&terminal);
    app.confirm_risky_paste_for_test(false);
    assert_eq!(
        bytes_of(&recorded),
        b"first\rsecond",
        "the mode is unchanged, so the confirmed paste goes out"
    );
}

#[test]
fn a_poisoned_split_pane_still_renders() {
    let (mut app, _recorded, _terminal) = recording_app(b"");
    let dims = Dimensions::new(39, 24);
    let pane = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    pane.lock().expect("pane").advance(b"poisoned pane text");
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(io::sink())));
    app.seed_headless_split_pane_for_test(true, Arc::clone(&pane), writer, dims);
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    app.set_test_surface_for_test(640, 384, crate::native::WindowPadding::ZERO);
    app.reflow_active_panes_for_test();
    poison(&pane);
    let probes = app.rebuild_multipane_probe_for_test();
    assert_eq!(probes.len(), 2, "both panes are drawn");
    assert!(
        probes.iter().any(|probe| probe
            .rows
            .iter()
            .any(|row| row.contains("poisoned pane text"))),
        "the poisoned pane shows its text: {probes:?}"
    );
}

#[test]
fn typing_into_a_poisoned_blinking_session_keeps_the_blink_hold() {
    let (mut app, _recorded, terminal) = recording_app(b"\x1b[1 q");
    app.on_window_focus_changed_for_test(true);
    poison(&terminal);
    app.drive_text_key_for_test("a");
    assert!(
        app.active_cursor_blink_deadline_for_test().is_some(),
        "the child asked for a blinking cursor, so typing schedules its hold"
    );
}

#[test]
fn delete_over_a_poisoned_input_selection_edits_only_the_input() {
    let content = b"\x1b]133;A\x07$ \x1b]133;B\x07abc\x1b7\x1b[1;16H23.1s\x1b8\x1b]133;P;odytty-edit;len=3;cur=3\x07";
    let (mut app, recorded, terminal) = recording_app(content);
    app.force_selection_for_test(0, 0, 0, 19);
    app.set_pointer_cell_for_test(5, 10);
    poison(&terminal);
    app.drive_named_key_for_test(NamedKey::Delete);
    assert_eq!(
        bytes_of(&recorded),
        [b"\x1b[D".repeat(3), b"\x1b[3~".repeat(3)].concat(),
        "the selected input is deleted, not a single blind Delete"
    );
}

/// The poisoned model still accepts output, as the PTY reader writes it.
fn advance_poisoned(terminal: &Arc<Mutex<Terminal>>, bytes: &[u8]) {
    crate::native::lock_recover(terminal).advance(bytes);
}

const CELL_8X16: CellSize = CellSize {
    width: 8,
    height: 16,
    baseline: 12,
};

#[test]
fn a_poisoned_session_keeps_its_kitty_keyboard_encoding() {
    let (mut app, recorded, terminal) = recording_app(b"\x1b[>1u");
    poison(&terminal);
    app.drive_named_key_for_test(NamedKey::Escape);
    assert_eq!(
        bytes_of(&recorded),
        b"\x1b[27u",
        "the child's disambiguate flag still encodes Escape"
    );
}

#[test]
fn a_poisoned_mouse_reporting_session_still_receives_the_click() {
    let (mut app, recorded, terminal) = recording_app(b"\x1b[?1000h\x1b[?1006h");
    app.set_test_cell_for_test(CELL_8X16);
    app.set_test_surface_for_test(640, 384, crate::native::WindowPadding::ZERO);
    poison(&terminal);
    app.pointer_move_for_test(4.0, 8.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    assert_eq!(
        bytes_of(&recorded),
        b"\x1b[<0;1;1M",
        "the press is reported, not taken as a selection"
    );
}

#[test]
fn a_poisoned_session_still_receives_focus_reports() {
    let (mut app, recorded, terminal) = recording_app(b"\x1b[?1004h");
    app.on_window_focus_changed_for_test(true);
    recorded.lock().expect("bytes").clear();
    poison(&terminal);
    app.on_window_focus_changed_for_test(false);
    assert_eq!(bytes_of(&recorded), b"\x1b[O", "focus loss is reported");
}

#[test]
fn a_poisoned_alternate_screen_still_turns_the_wheel_into_arrows() {
    let (mut app, recorded, terminal) = recording_app(b"\x1b[?1049h\x1b[?1007h");
    poison(&terminal);
    app.dispatch_wheel_for_test(1.0);
    let written = bytes_of(&recorded);
    assert!(
        !written.is_empty()
            && written
                .chunks(3)
                .all(|key| key == b"\x1b[A" || key == b"\x1bOA"),
        "the wheel becomes up arrows: {written:?}"
    );
}

#[test]
fn a_bell_rung_in_a_poisoned_session_is_still_drained() {
    let (mut app, _recorded, terminal) = recording_app(b"");
    poison(&terminal);
    advance_poisoned(&terminal, b"\x07");
    let (focused_bell, _, _) = app.drain_bells_for_test();
    assert!(focused_bell, "the bell is taken from the poisoned model");
}

#[test]
fn a_poisoned_session_still_follows_a_window_resize() {
    let (mut app, _recorded, terminal) = recording_app(b"");
    poison(&terminal);
    assert!(
        app.resize_grid(CELL_8X16, 50 * 8, 20 * 16),
        "the grid changed"
    );
    let dims = crate::native::lock_recover(&terminal).screen().dimensions();
    assert_eq!((dims.columns, dims.rows), (50, 20), "the model was resized");
}

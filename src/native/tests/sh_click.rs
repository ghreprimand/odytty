// SPDX-License-Identifier: GPL-3.0-only
//! SH-CLICK native wiring tests: OSC 133 click-to-position-cursor.
//!
//! Headless (no GPU/window): the App is driven over a one-shot PTY whose writer
//! is a recording buffer, so the cursor-positioning arrow burst it emits can be
//! asserted byte-for-byte. The click is driven through the production
//! `handle_mouse_input` press/release routing (via `left_button_outcome_for_test`)
//! so the full precedence — overlay → selection → TUI report → local — is
//! exercised, not reimplemented. Skipped when no PTY is available (CI sandboxes),
//! mirroring the other App-level suites.

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Build an `App` over a one-shot PTY whose writer is a recording buffer, feed
/// `content` into its terminal, and turn `sh_click` on. Returns the app plus the
/// captured-bytes handle, or `None` when no PTY is available.
fn build_app(content: &[u8]) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    build_app_with(content, true)
}

fn build_app_with(content: &[u8], sh_click: bool) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
    // Spawn provides the `pty` field; the writer is the recorder so the emitted
    // arrows are observable (the real PTY writer would swallow them into a shell).
    let _ = session.take_writer().ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    let pty = Arc::new(Mutex::new(session));
    let mut app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.set_sh_click_for_test(sh_click);
    Some((app, bytes))
}

/// A live prompt awaiting input, advertising click-events: an OSC 133 `A` with a
/// `click_events=1` attribute, then `$ hello` so the cursor sits at column 7.
fn live_prompt_click_enabled() -> &'static [u8] {
    b"\x1b]133;A;click_events=1\x07$ hello"
}

/// Drive a bare left click at `(row, column)` through the production press/release
/// routing and return the bytes the PTY writer received.
fn click_at(app: &mut App, bytes: &Arc<Mutex<Vec<u8>>>, row: usize, column: usize) -> Vec<u8> {
    app.set_pointer_cell_for_test(row, column);
    let _ = app.left_button_outcome_for_test(true); // press
    let _ = app.left_button_outcome_for_test(false); // release
    bytes.lock().expect("bytes").clone()
}

#[test]
fn click_left_of_cursor_emits_left_arrows_on_live_prompt() {
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return; // no PTY in this environment
    };
    // Cursor at column 7 ("$ hello"); click column 2 -> delta -5 -> 5x Left.
    let written = click_at(&mut app, &bytes, 0, 2);
    assert_eq!(
        written,
        b"\x1b[D".repeat(5),
        "a bare click left of the cursor moves the shell cursor left"
    );
}

#[test]
fn click_right_of_cursor_emits_right_arrows() {
    // Put the cursor early ("$ ") so a click to the right yields Right arrows.
    let Some((mut app, bytes)) = build_app(b"\x1b]133;A;click_events=1\x07$ ") else {
        return;
    };
    // Cursor at column 2; click column 6 -> delta +4 -> 4x Right.
    let written = click_at(&mut app, &bytes, 0, 6);
    assert_eq!(written, b"\x1b[C".repeat(4));
}

#[test]
fn click_honors_application_cursor_mode() {
    // Finding A regression guard: with DECCKM (application cursor) on, the burst
    // is the SS3 form, byte-identical to a real arrow keypress in that mode.
    let Some((mut app, bytes)) = build_app(b"\x1b]133;A;click_events=1\x07\x1b[?1h$ hello") else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 2);
    assert_eq!(written, b"\x1bOD".repeat(5));
}

#[test]
fn click_on_cursor_cell_emits_nothing() {
    // T4 same-cell: a click on the cursor's own column is a no-op (delta 0).
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 7);
    assert!(written.is_empty(), "same-cell click yields no movement");
}

#[test]
fn click_off_path_is_inert_when_setting_off() {
    // T1: with sh_click off (the default), the click path is byte-identical to
    // today — no arrows, even though the shell advertised click-events.
    let Some((mut app, bytes)) = build_app_with(live_prompt_click_enabled(), false) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 2);
    assert!(written.is_empty(), "feature off -> no bytes emitted");
}

#[test]
fn click_without_advertised_click_events_is_inert() {
    // A plain prompt with NO click_events attribute: the core flag stays off, so
    // even with sh_click on the feature does nothing (shell-gated by construction).
    let Some((mut app, bytes)) = build_app(b"\x1b]133;A\x07$ hello") else {
        return;
    };
    let written = click_at(&mut app, &bytes, 0, 2);
    assert!(written.is_empty());
}

#[test]
fn shift_click_does_not_reposition() {
    // T2: Shift is the selection/passthrough seam; SH-CLICK never reads it and
    // never fires under Shift.
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    app.set_shift_modifier_for_test(true);
    let written = click_at(&mut app, &bytes, 0, 2);
    assert!(written.is_empty(), "shift+click stays a selection gesture");
}

#[test]
fn tui_mouse_reporting_wins_over_click_to_position() {
    // T3 (highest risk): a TUI with mouse reporting active owns the click — the
    // report gate returns before the local click-to-position path is reached.
    let Some((mut app, _bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    app.enable_mouse_reporting_for_test(); // DECSET 1000
    app.set_pointer_cell_for_test(0, 2);
    app.set_pointer_px_for_test(16.0, 0.0);
    let outcome = app.left_button_outcome_for_test(true);
    assert_eq!(
        outcome, "report",
        "an active mouse-reporting mode routes the press to the app, not click-to-position"
    );
}

#[test]
fn click_during_running_command_does_not_reposition() {
    // T4 prompt-context gate: the prompt has executed (an OutputStart mark
    // exists), so there is no live prompt even though click_events is still
    // enabled. A click on the cursor row must NOT emit arrows into the program.
    let Some((mut app, bytes)) =
        build_app(b"\x1b]133;A;click_events=1\x07$ cmd\r\n\x1b]133;C\x07output")
    else {
        return;
    };
    // The cursor now sits on the output row (row 1) after "output" (column 6);
    // click left of it on the same row.
    let written = click_at(&mut app, &bytes, 1, 2);
    assert!(
        written.is_empty(),
        "a running command (no live prompt) is not click-to-position territory"
    );
}

#[test]
fn click_on_a_different_row_than_the_cursor_is_inert() {
    // D-SHC-4: v1 is same-row horizontal only. A click on a row other than the
    // cursor's falls through (no wrong jump), rather than emitting arrows.
    let Some((mut app, bytes)) = build_app(live_prompt_click_enabled()) else {
        return;
    };
    let written = click_at(&mut app, &bytes, 3, 2);
    assert!(written.is_empty(), "off-row click never repositions in v1");
}

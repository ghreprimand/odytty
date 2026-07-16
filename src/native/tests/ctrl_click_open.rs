// SPDX-License-Identifier: GPL-3.0-only
//! CTRL-CLICK-OPEN native wiring tests: a plain open-modifier left click (Ctrl
//! on Linux/Windows, Cmd on macOS) over a resolved span opens it even while a
//! mouse-tracking TUI has reporting enabled, matching the kitty/iTerm2/GNOME
//! Terminal convention. Driven through the production `handle_mouse_input` press
//! route so the report-gate-vs-open precedence is exercised, not reimplemented —
//! the shipped policy unit tests drive the predicate directly and so could pass
//! while the real winit press route stayed broken.
//!
//! Headless (no GPU/window): the App runs over a one-shot PTY whose writer is a
//! recording buffer, so the SGR mouse report the app WOULD receive is observable
//! byte-for-byte — an empty buffer proves the press was intercepted and opened
//! instead of reported. The open dispatch is proven WITHOUT launching a real
//! opener by pointing the editor override at a sentinel program that cannot
//! exist (`odytty_ci_selftest_editor`): the argv-only spawn fails cleanly with
//! ENOENT and raises the transient open-notice, so the notice is the positive
//! "the open ran" signal and no process is ever started on the shared machine.
//! Synthetic only: `/proj/...` is fabricated and the stat-gate is a `MapProbe`
//! over a fixed map. Skipped when no PTY is available (CI sandboxes).

use super::*;
use crate::native::app::interactive_paths::MapProbe;
use crate::paths::FsKind;

const COLS: usize = 80;
const ROWS: usize = 24;
const CELL_W: u32 = 8;
const CELL_H: u32 = 16;

/// A resolved interactive path carrying a `:line:col` suffix. The `:line` forces
/// the editor branch of the open ladder (not the platform default opener), so
/// the sentinel editor override is what would be spawned — a clean ENOENT
/// failure rather than a real `xdg-open`/`open` launch.
const PATH_LINE: &[u8] = b"/proj/src/main.rs:42:7";

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

/// Build an App over a one-shot PTY whose writer records everything the app
/// would send to the child (so an SGR mouse report is observable), feed
/// `content` into the terminal, and return the app plus the captured-bytes
/// handle. `None` when no PTY is available.
fn build_app(content: &[u8]) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(COLS, ROWS);
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (mut app, terminal) =
        headless_app_with_writer(NativeOptions::default(), dims, Settings::default(), writer);
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    app.set_test_cell_for_test(cell(CELL_W, CELL_H));
    Some((app, bytes))
}

/// Turn interactive paths on, install a stat-gate that resolves
/// `/proj/src/main.rs` to a file, and point the editor override at a sentinel
/// program that cannot exist so an open dispatch fails cleanly (no real opener).
fn arm_path_open(app: &mut App) {
    app.set_interactive_paths_for_test(true);
    app.set_test_path_probe_for_test(MapProbe::new([("/proj/src/main.rs", FsKind::File)]));
    // Sentinel: not a real binary; the argv-only spawn ENOENTs and raises a
    // notice instead of launching anything on the shared box.
    app.set_interactive_paths_editor_for_test("odytty_ci_selftest_editor");
}

/// Hold/release the host's open modifier — Cmd (super) on macOS, Ctrl on Linux
/// and Windows — the same per-OS chord `OpenerOs::host()` resolves inside the
/// press route. The live route reads the host OS internally, so the test drives
/// whichever modifier is the open chord on the runner.
fn hold_open_modifier(app: &mut App, held: bool) {
    if cfg!(target_os = "macos") {
        app.set_super_key_for_test(held);
    } else {
        app.set_ctrl_modifier_for_test(held);
    }
}

/// Position the pointer inside the path span at row 0 so `hovered_path` latches.
fn hover_on_path(app: &mut App) {
    app.pointer_move_for_test(f64::from(CELL_W) * 5.5, f64::from(CELL_H) * 0.5);
}

/// Position the pointer over empty cells far to the right of the path so no span
/// is under it.
fn hover_off_span(app: &mut App) {
    app.pointer_move_for_test(f64::from(CELL_W) * 60.5, f64::from(CELL_H) * 0.5);
}

/// Drive a real left press+release through the production route.
fn left_press_release(app: &mut App) {
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
}

#[test]
fn ctrl_click_over_path_under_mouse_reporting_opens_and_does_not_report() {
    let Some((mut app, pty_bytes)) = build_app(PATH_LINE) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    arm_path_open(&mut app);
    app.enable_mouse_reporting_for_test(); // DECSET 1000: a TUI now owns clicks
    hover_on_path(&mut app);
    assert!(
        app.hovered_path_for_test().is_some(),
        "the path span is hovered even while mouse reporting is enabled"
    );
    pty_bytes.lock().expect("bytes").clear();

    hold_open_modifier(&mut app, true);
    left_press_release(&mut app);

    // The open ran: the sentinel-editor dispatch fails with ENOENT and raises
    // the transient notice (no real opener launched).
    assert!(
        app.open_notice_message_for_test().is_some(),
        "Ctrl+click over a resolved path opens even while a TUI has mouse reporting on"
    );
    // ...and the press was NOT reported to the child: the app saw no SGR report.
    assert!(
        pty_bytes.lock().expect("bytes").is_empty(),
        "the intercepted open must not leak a mouse report to the reporting app"
    );
}

#[test]
fn ctrl_click_off_a_span_under_mouse_reporting_still_reports_to_the_app() {
    let Some((mut app, pty_bytes)) = build_app(PATH_LINE) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    arm_path_open(&mut app);
    app.enable_mouse_reporting_for_test();
    hover_off_span(&mut app);
    assert!(
        app.hovered_path_for_test().is_none(),
        "no resolved span sits under the pointer"
    );
    pty_bytes.lock().expect("bytes").clear();

    hold_open_modifier(&mut app, true);
    left_press_release(&mut app);

    // No span was hovered, so the open interception is skipped and the Ctrl+click
    // reports to the app exactly as before — the mouse-tracking app still gets
    // its clicks.
    assert!(
        app.open_notice_message_for_test().is_none(),
        "nothing is dispatched when the Ctrl+click is not over a resolved span"
    );
    assert!(
        !pty_bytes.lock().expect("bytes").is_empty(),
        "a Ctrl+click off a resolved span must still report to the mouse-tracking app"
    );
}

#[test]
fn plain_click_over_path_under_mouse_reporting_reports_as_before() {
    let Some((mut app, pty_bytes)) = build_app(PATH_LINE) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    arm_path_open(&mut app);
    app.enable_mouse_reporting_for_test();
    hover_on_path(&mut app);
    assert!(app.hovered_path_for_test().is_some());
    pty_bytes.lock().expect("bytes").clear();

    // No open modifier held: the report gate wins (control), so the app receives
    // the click and nothing opens — byte-identical to today.
    left_press_release(&mut app);

    assert!(
        app.open_notice_message_for_test().is_none(),
        "a plain click never opens a span"
    );
    assert!(
        !pty_bytes.lock().expect("bytes").is_empty(),
        "a plain click under mouse reporting still reports to the app"
    );
}

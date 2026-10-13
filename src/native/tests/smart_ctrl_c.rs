// SPDX-License-Identifier: GPL-3.0-only
//! App-level wiring tests for the `smart_ctrl_c` copy-or-interrupt
//! policy.
//!
//! These drive the production `handle_key_event_with_text` path headlessly to pin the
//! WIRING, not the pure decision (the chord/selection predicate lives inline in
//! `smart_ctrl_c_intercept`): that a plain Ctrl+C under the copy-or-interrupt
//! policy copies + clears a local selection without sending the interrupt, that
//! the default (off) policy leaves the selection intact and falls through to the
//! interrupt encode, that a plain Ctrl+C with no selection never intercepts, and
//! that Ctrl+Shift+C still copies without the smart-path clear. The copied text
//! is read back through the recorded clipboard write, and the bytes that reach
//! the PTY are recorded, so both safety properties are asserted: the chord
//! copies, and it does not also interrupt the foreground program.
//!
//! These run over a headless session (no real PTY): they exercise only App-level
//! key wiring, selection state and the bytes handed to the PTY writer, not shell
//! behavior.

use super::*;
use crate::settings::SmartCtrlC;

const COLS: usize = 80;
const ROWS: usize = 24;

/// The interrupt byte a plain Ctrl+C encodes to.
const ETX: &[u8] = b"\x03";

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

fn build_app(content: &[u8]) -> (App, Arc<Mutex<Vec<u8>>>) {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(COLS, ROWS),
        Settings::default(),
        writer,
    );
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    (app, bytes)
}

fn sent(bytes: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    bytes.lock().expect("bytes").clone()
}

#[test]
fn plain_ctrl_c_copies_and_clears_selection_under_policy() {
    let (mut app, bytes) = build_app(b"hello world");
    app.set_smart_ctrl_c_for_test(SmartCtrlC::CopyOrInterrupt);
    app.force_selection_for_test(0, 0, 0, 4);
    assert!(
        app.selection_range_for_test().is_some(),
        "precondition: selection"
    );

    // Plain Ctrl+C (no Shift) under the copy-or-interrupt policy.
    app.drive_char_with_mods_for_test('c', true, false);

    assert!(
        app.selection_range_for_test().is_none(),
        "smart Ctrl+C copied and cleared the selection (so the next Ctrl+C interrupts)"
    );
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some("hello"),
        "the selected text reached the clipboard"
    );
    assert!(
        sent(&bytes).is_empty(),
        "copying must not also send the interrupt byte to the foreground program"
    );
}

#[test]
fn plain_ctrl_c_with_policy_off_leaves_selection_for_interrupt() {
    let (mut app, bytes) = build_app(b"hello world");
    // Explicit off policy - byte-identical interrupt path. (The shipped default
    // is now copy-or-interrupt, so the off path is set explicitly here.)
    app.set_smart_ctrl_c_for_test(SmartCtrlC::Off);
    app.force_selection_for_test(0, 0, 0, 4);
    app.drive_char_with_mods_for_test('c', true, false);

    assert!(
        app.selection_range_for_test().is_some(),
        "with smart Ctrl+C off, the chord falls through to the interrupt encode and never touches the selection"
    );
    assert_eq!(sent(&bytes), ETX, "the interrupt byte is sent");
    assert_eq!(
        app.last_clipboard_write_for_test(),
        None,
        "nothing was copied"
    );
}

#[test]
fn plain_ctrl_c_with_no_selection_does_not_intercept() {
    let (mut app, bytes) = build_app(b"hello world");
    app.set_smart_ctrl_c_for_test(SmartCtrlC::CopyOrInterrupt);
    assert!(
        app.selection_range_for_test().is_none(),
        "precondition: no selection"
    );

    // No selection: the intercept declines and the chord falls through to the
    // interrupt encode. The App must not panic and no selection may appear.
    app.drive_char_with_mods_for_test('c', true, false);

    assert!(app.selection_range_for_test().is_none());
    assert_eq!(sent(&bytes), ETX, "the interrupt byte is sent");
    assert_eq!(
        app.last_clipboard_write_for_test(),
        None,
        "nothing was copied"
    );
}

#[test]
fn ctrl_shift_c_still_copies_without_clearing_under_policy() {
    let (mut app, bytes) = build_app(b"hello world");
    app.set_smart_ctrl_c_for_test(SmartCtrlC::CopyOrInterrupt);
    app.force_selection_for_test(0, 0, 0, 4);

    // Ctrl+Shift+C is the unambiguous Copy binding: it copies but does NOT take
    // the smart-path clear, so the selection survives. Only plain Ctrl+C clears.
    app.drive_char_with_mods_for_test('c', true, true);

    assert!(
        app.selection_range_for_test().is_some(),
        "Ctrl+Shift+C copies but leaves the selection; the smart clear is plain-Ctrl+C only"
    );
    assert_eq!(
        app.last_clipboard_write_for_test().as_deref(),
        Some("hello"),
        "the selected text reached the clipboard"
    );
    assert!(
        sent(&bytes).is_empty(),
        "the copy chord sends nothing to the PTY"
    );
}

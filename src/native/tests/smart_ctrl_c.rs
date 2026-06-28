// SPDX-License-Identifier: GPL-3.0-only
//! SMART-CTRLC: App-level wiring tests for the `smart_ctrl_c` copy-or-interrupt
//! policy.
//!
//! These drive the production `handle_key_event` path headlessly to pin the
//! WIRING, not the pure decision (the chord/selection predicate lives inline in
//! `smart_ctrl_c_intercept`): that a plain Ctrl+C under the copy-or-interrupt
//! policy copies + clears a local selection, that the default (off) policy
//! leaves the selection intact and falls through to the interrupt encode, that a
//! plain Ctrl+C with no selection never intercepts, and that Ctrl+Shift+C still
//! copies without the smart-path clear. Selection-cleared is the observable
//! (the clipboard backend is headless), which is exactly the state the intercept
//! mutates before swallowing the chord.
//!
//! Skipped when no PTY is available (CI sandboxes).

use super::*;
use crate::settings::SmartCtrlC;

const COLS: usize = 80;
const ROWS: usize = 24;

fn build_app(content: &[u8]) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(content);
    }
    let pty = Arc::new(Mutex::new(session));
    Some(App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    ))
}

#[test]
fn plain_ctrl_c_copies_and_clears_selection_under_policy() {
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
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
}

#[test]
fn plain_ctrl_c_with_policy_off_leaves_selection_for_interrupt() {
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // Explicit off policy — byte-identical interrupt path. (The shipped default
    // is now copy-or-interrupt, so the off path is set explicitly here.)
    app.set_smart_ctrl_c_for_test(SmartCtrlC::Off);
    app.force_selection_for_test(0, 0, 0, 4);
    app.drive_char_with_mods_for_test('c', true, false);

    assert!(
        app.selection_range_for_test().is_some(),
        "with smart Ctrl+C off, the chord falls through to the interrupt encode and never touches the selection"
    );
}

#[test]
fn plain_ctrl_c_with_no_selection_does_not_intercept() {
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_smart_ctrl_c_for_test(SmartCtrlC::CopyOrInterrupt);
    assert!(
        app.selection_range_for_test().is_none(),
        "precondition: no selection"
    );

    // No selection: the intercept declines and the chord falls through to the
    // interrupt encode. The App must not panic and no selection may appear.
    app.drive_char_with_mods_for_test('c', true, false);

    assert!(app.selection_range_for_test().is_none());
}

#[test]
fn ctrl_shift_c_still_copies_without_clearing_under_policy() {
    let Some(mut app) = build_app(b"hello world") else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.set_smart_ctrl_c_for_test(SmartCtrlC::CopyOrInterrupt);
    app.force_selection_for_test(0, 0, 0, 4);

    // Ctrl+Shift+C is the unambiguous Copy binding: it copies but does NOT take
    // the smart-path clear, so the selection survives. Only plain Ctrl+C clears.
    app.drive_char_with_mods_for_test('c', true, true);

    assert!(
        app.selection_range_for_test().is_some(),
        "Ctrl+Shift+C copies but leaves the selection; the smart clear is plain-Ctrl+C only"
    );
}

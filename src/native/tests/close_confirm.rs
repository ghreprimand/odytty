// SPDX-License-Identifier: GPL-3.0-only
//! CLOSE-CONFIRM native-wiring integration: the confirmation dialog routes
//! through the real overlay-key path so a confirm sets the App's `pending_exit`
//! flag (which `window_event` turns into an actual exit), while a dismiss leaves
//! it clear. These drive a real `App` over a one-shot PTY (skipped when none is
//! available); no GPU is needed — the dialog state and exit flag are observable
//! without a surface.

use super::*;
use winit::keyboard::Key as WinitKey;

const COLS: usize = 60;
const ROWS: usize = 12;

/// Build an `App` over a one-shot PTY. Returns `None` when no PTY is available.
fn build_app() -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some(app)
}

#[test]
fn confirm_default_is_on() {
    // D-CC-5: confirm_close defaults ON (footgun protection); the prompt only
    // appears when a job is actually running, so the idle path is unaffected.
    assert!(Settings::default().confirm_close);
}

#[test]
fn enter_confirms_and_flags_pending_exit() {
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert!(!app.pending_exit_for_test());

    app.open_confirm_close_for_test();
    assert!(app.confirm_close_open_for_test());

    // Enter drives the production overlay-key path → ForceClose → pending_exit.
    app.drive_overlay_key_for_test(
        WinitKey::Named(winit::keyboard::NamedKey::Enter),
        false,
        false,
    );
    assert!(
        app.pending_exit_for_test(),
        "confirming the dialog must flag the pending exit"
    );
    assert!(
        !app.confirm_close_open_for_test(),
        "the dialog closes itself before the App exits (TRAP-4)"
    );
}

#[test]
fn escape_dismisses_without_pending_exit() {
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    app.open_confirm_close_for_test();
    assert!(app.confirm_close_open_for_test());

    // Esc cancels: the dialog closes and the window never exits (TRAP-2).
    app.drive_overlay_key_for_test(
        WinitKey::Named(winit::keyboard::NamedKey::Escape),
        false,
        false,
    );
    assert!(
        !app.pending_exit_for_test(),
        "dismissing the dialog must NOT flag an exit"
    );
    assert!(!app.confirm_close_open_for_test());
}

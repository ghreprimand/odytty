// SPDX-License-Identifier: GPL-3.0-only
//! KB-REMAP native-wiring integration: a chord captured through the REAL
//! production overlay-key path lands as a live binding. This is the R2
//! kill-shot proof — the chord-capture bypass must fire BEFORE the lossy
//! `overlay_input_from_winit` mapper, or `Ctrl+Shift+J` would collapse to a
//! plain `Char('j')` (modifiers lost) and never capture.
//!
//! Drives a real `App` over a one-shot PTY (skipped when none is available, as
//! in CI sandboxes); no GPU needed — the binding table is observable before any
//! surface exists.

use super::*;
use crate::settings::BindableAction;

const COLS: usize = 60;
const ROWS: usize = 20;

fn build_app() -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
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
fn chord_capture_bypass_binds_through_the_real_key_path() {
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Default selection is the first action (Search); ctrl+shift+j is unbound by
    // default, so capturing it is conflict-free.
    let j = WinitKey::Character("j".into());
    assert_eq!(
        app.live_action_for_chord_for_test(&j, true, true),
        None,
        "ctrl+shift+j starts unbound"
    );

    app.open_key_bindings_overlay_for_test();
    assert!(
        !app.overlay_capturing_chord_for_test(),
        "just-opened modal is browsing, not capturing"
    );

    // Enter arms capture for the selected row (no modifiers).
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(
        app.overlay_capturing_chord_for_test(),
        "Enter must arm chord capture"
    );

    // The KILL-SHOT: Ctrl+Shift+J through the production key path. If the bypass
    // were placed after the lossy mapper, the modifiers would be stripped and
    // nothing would bind.
    app.drive_overlay_key_for_test(j.clone(), true, true);

    assert!(
        !app.overlay_capturing_chord_for_test(),
        "a valid chord commits and returns to browsing"
    );
    assert_eq!(
        app.live_action_for_chord_for_test(&j, true, true),
        Some(BindableAction::Search),
        "ctrl+shift+j is now live-bound to Search via the real apply path"
    );
}

#[test]
fn esc_while_capturing_cancels_without_binding() {
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.open_key_bindings_overlay_for_test();
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(app.overlay_capturing_chord_for_test());

    // Bare Esc cancels capture (its natural modal-control role) — it is never
    // bound, and the modal stays open and browsing.
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    assert!(
        !app.overlay_capturing_chord_for_test(),
        "Esc disarms capture"
    );

    let j = WinitKey::Character("j".into());
    assert_eq!(
        app.live_action_for_chord_for_test(&j, true, true),
        None,
        "no binding was committed on cancel"
    );
}

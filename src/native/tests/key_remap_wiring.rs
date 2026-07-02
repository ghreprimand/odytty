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

#[test]
fn settings_chord_reaches_capture_instead_of_toggling_overlay() {
    // C10: the Settings chord (Ctrl+Shift+,) resolves to SettingsPanel and is
    // checked ABOVE the overlay-open guard in handle_key_event. Before the fix
    // that pre-empted chord capture — arming a remap row and pressing the
    // Settings chord toggled the Settings panel instead of capturing, so
    // Ctrl+Shift+, could never be assigned to any action. The fix gates the
    // SettingsPanel/ThemePicker shortcuts on !is_capturing_chord(), so while a
    // row is armed the chord falls through to the capture path.
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };

    app.open_key_bindings_overlay_for_test();
    // Arm capture on the default-selected row (Search).
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(
        app.overlay_capturing_chord_for_test(),
        "Enter arms chord capture"
    );

    // Drive Ctrl+Shift+, through the FULL production key path. Ctrl+Shift+, is
    // SettingsPanel's default binding, so capturing it onto the Search row is a
    // conflict — deliver_chord raises a conflict-confirm and capture stays
    // engaged. The discriminator: capture is STILL active, proving the chord
    // reached the remap path and did NOT toggle the Settings overlay.
    app.drive_char_with_mods_for_test(',', true, true);

    assert!(
        app.overlay_capturing_chord_for_test(),
        "C10: the Settings chord must reach chord capture (conflict pending), \
         not pre-empt it by toggling the Settings overlay"
    );
}

#[test]
fn held_settings_chord_does_not_repeat_toggle_the_overlay() {
    // C22: a held Settings chord auto-repeats. The old code fired
    // toggle_settings_overlay on every Repeat, open/close-flickering the panel.
    // The fix acts on the initial Press only; Repeats fall through harmlessly.
    let Some(mut app) = build_app() else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert!(!app.overlay_open_for_test(), "starts with no overlay");

    // Initial Press opens the Settings overlay.
    app.drive_char_with_mods_typed_for_test(',', true, true, KeyEventType::Press);
    assert!(
        app.overlay_open_for_test(),
        "the initial Press opens the Settings overlay"
    );

    // Each auto-repeat event from holding the chord must NOT toggle it. The old
    // code toggled on every Repeat, so a SINGLE repeat closed the panel (and an
    // even number would deceptively reopen it — assert after each repeat).
    app.drive_char_with_mods_typed_for_test(',', true, true, KeyEventType::Repeat);
    assert!(
        app.overlay_open_for_test(),
        "C22: the first held-chord auto-repeat must not toggle the overlay closed"
    );
    app.drive_char_with_mods_typed_for_test(',', true, true, KeyEventType::Repeat);
    assert!(
        app.overlay_open_for_test(),
        "C22: a second auto-repeat must still leave the overlay open"
    );
}

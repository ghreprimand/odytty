// SPDX-License-Identifier: GPL-3.0-only
//! KB-REMAP native-wiring integration: a chord captured through the REAL
//! production overlay-key path lands as a live binding. This is the R2
//! kill-shot proof - the chord-capture bypass must fire BEFORE the lossy
//! `overlay_input_from_winit` mapper, or `Ctrl+Shift+J` would collapse to a
//! plain `Char('j')` (modifiers lost) and never capture.
//!
//! Drives a real `App` over a headless session that owns no PTY child; no GPU
//! needed - the binding table is observable before any
//! surface exists.

use super::*;
use crate::settings::BindableAction;

const COLS: usize = 60;
const ROWS: usize = 20;

fn build_app() -> App {
    let dims = Dimensions::new(COLS, ROWS);
    let (app, _terminal) = headless_app_with(NativeOptions::default(), dims, Settings::default());
    app
}

#[test]
fn chord_capture_bypass_binds_through_the_real_key_path() {
    let mut app = build_app();

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

    // The Ctrl+Shift+J through the production key path. If the bypass
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
    let mut app = build_app();

    app.open_key_bindings_overlay_for_test();
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(app.overlay_capturing_chord_for_test());

    // Bare Esc cancels capture (its natural modal-control role) - it is never
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
    // The Settings chord (Ctrl+Shift+,) resolves to SettingsPanel and is
    // checked ABOVE the overlay-open guard in handle_key_event_with_text. Before the fix
    // that pre-empted chord capture - arming a remap row and pressing the
    // Settings chord toggled the Settings panel instead of capturing, so
    // Ctrl+Shift+, could never be assigned to any action. The fix gates the
    // SettingsPanel/ThemePicker shortcuts on !is_capturing_chord(), so while a
    // row is armed the chord falls through to the capture path.
    let mut app = build_app();

    app.open_key_bindings_overlay_for_test();
    // Arm capture on the default-selected row (Search).
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(
        app.overlay_capturing_chord_for_test(),
        "Enter arms chord capture"
    );

    // Drive Ctrl+Shift+, through the FULL production key path. Ctrl+Shift+, is
    // SettingsPanel's default binding, so capturing it onto the Search row is a
    // conflict - deliver_chord raises a conflict-confirm and capture stays
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
    // A held Settings chord auto-repeats. The old code fired
    // toggle_settings_overlay on every Repeat, open/close-flickering the panel.
    // The fix acts on the initial Press only; Repeats fall through harmlessly.
    let mut app = build_app();
    assert!(!app.overlay_open_for_test(), "starts with no overlay");

    // Initial Press opens the Settings overlay.
    app.drive_char_with_mods_typed_for_test(',', true, true, KeyEventType::Press);
    assert!(
        app.overlay_open_for_test(),
        "the initial Press opens the Settings overlay"
    );

    // Each auto-repeat event from holding the chord must NOT toggle it. The old
    // code toggled on every Repeat, so a SINGLE repeat closed the panel (and an
    // even number would deceptively reopen it - assert after each repeat).
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

#[test]
fn new_window_chord_requests_an_in_process_sibling() {
    // Ctrl+Shift+N routes through the full production key path to
    // request_new_window (same-process sibling), not a re-exec spawn.
    let mut app = build_app();

    let n = WinitKey::Character("n".into());
    assert_eq!(
        app.live_action_for_chord_for_test(&n, true, true),
        Some(BindableAction::NewWindow),
        "Ctrl+Shift+N binds to New Window by default"
    );

    assert!(
        !app.has_pending_new_window_for_test(),
        "no New Window request at rest"
    );
    app.drive_char_with_mods_typed_for_test('n', true, true, KeyEventType::Press);
    let request = app
        .take_new_window_request_for_test()
        .expect("chord queues one New Window request");
    assert!(
        request.profile.is_none(),
        "ordinary New Window uses the default launch profile"
    );

    // A plain 'n' (no Ctrl+Shift) must NOT request a sibling.
    assert!(!app.has_pending_new_window_for_test());
    app.drive_char_with_mods_typed_for_test('n', false, false, KeyEventType::Press);
    assert!(
        !app.has_pending_new_window_for_test(),
        "an unmodified 'n' is shell input, never a New Window request"
    );
}

#[test]
fn new_window_chord_is_idempotent_while_a_request_is_pending() {
    let mut app = build_app();
    app.drive_char_with_mods_typed_for_test('n', true, true, KeyEventType::Press);
    assert!(app.has_pending_new_window_for_test());
    app.drive_char_with_mods_typed_for_test('n', true, true, KeyEventType::Press);
    assert!(
        app.has_pending_new_window_for_test(),
        "a second chord while pending still leaves exactly one request"
    );
    let _ = app.take_new_window_request_for_test();
    assert!(!app.has_pending_new_window_for_test());
}

#[test]
fn reload_conflict_prompt_survives_real_input_and_cancel() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = build_app();
    app.open_key_bindings_overlay_for_test();
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    app.drive_overlay_key_for_test(WinitKey::Character("p".into()), true, true);
    assert!(app.overlay_capturing_chord_for_test());
    let before = app.overlay_signature_for_test();
    app.apply_reloaded_settings_for_test(Settings {
        window_opacity: 55.0,
        ..Settings::default()
    });
    let rendered = app.render_overlay_rows_for_test(120, 40).join("\n");
    assert!(rendered.contains("reassign to"), "{rendered}");
    assert!(rendered.contains("[Enter] yes  [Esc] no"), "{rendered}");
    assert!(rendered.contains("Configuration reloaded"), "{rendered}");
    assert_ne!(app.overlay_signature_for_test(), before);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    assert!(!app.overlay_capturing_chord_for_test());
    assert!(app.overlay_open_for_test());
    assert_eq!(
        app.live_action_for_chord_for_test(&WinitKey::Character("p".into()), true, true),
        Some(BindableAction::CommandPalette)
    );
}

#[test]
fn reload_close_prompt_survives_real_input_and_cancel() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = build_app();
    app.open_key_bindings_overlay_for_test();
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    app.drive_overlay_key_for_test(WinitKey::Character("j".into()), true, true);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    app.apply_reloaded_settings_for_test(Settings::default());
    let rendered = app.render_overlay_rows_for_test(120, 40).join("\n");
    assert!(
        rendered.contains("Unsaved keybinding changes."),
        "{rendered}"
    );
    assert!(
        rendered.contains("[S] save  [D] discard  [C] keep editing"),
        "{rendered}"
    );
    assert!(rendered.contains("Configuration reloaded"), "{rendered}");
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    assert!(app.overlay_open_for_test());
    assert!(
        !app.render_overlay_rows_for_test(120, 40)
            .join("\n")
            .contains("Unsaved keybinding changes.")
    );
    assert!(
        app.render_overlay_rows_for_test(120, 40)
            .join("\n")
            .contains("ctrl+shift+j *"),
        "the unsaved editor binding survives cancellation"
    );
}

fn assert_modal_wheel_is_inert(app: &mut App) {
    app.render_overlay_rows_for_test(60, 12);
    let before = app.overlay_signature_for_test();
    for delta in [-1.0, 1.0] {
        app.dispatch_wheel_for_test(delta);
        assert_eq!(app.overlay_signature_for_test(), before);
    }
}

#[test]
fn wheel_during_capture_keeps_the_highlight_on_the_armed_action() {
    let mut app = build_app();
    app.open_key_bindings_overlay_for_test();
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert!(app.overlay_capturing_chord_for_test());
    assert_modal_wheel_is_inert(&mut app);
    app.drive_overlay_key_for_test(WinitKey::Character("j".into()), true, true);
    assert_eq!(
        app.live_action_for_chord_for_test(&WinitKey::Character("j".into()), true, true),
        Some(BindableAction::Search)
    );
}

#[test]
fn wheel_during_conflict_keeps_the_pending_action_visible() {
    let mut app = build_app();
    app.open_key_bindings_overlay_for_test();
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    app.drive_overlay_key_for_test(WinitKey::Character("p".into()), true, true);
    assert!(
        app.overlay_signature_for_test()
            .key_remap
            .conflict
            .is_some()
    );
    assert_modal_wheel_is_inert(&mut app);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    assert_eq!(
        app.live_action_for_chord_for_test(&WinitKey::Character("p".into()), true, true),
        Some(BindableAction::Search)
    );
}

#[test]
fn wheel_during_dirty_close_prompt_keeps_the_editor_position() {
    let mut app = build_app();
    app.open_key_bindings_overlay_for_test();
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
    app.drive_overlay_key_for_test(WinitKey::Character("j".into()), true, true);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    assert!(
        app.overlay_signature_for_test()
            .key_remap
            .pending_close_prompt
    );
    assert_modal_wheel_is_inert(&mut app);
    app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Escape), false, false);
    assert!(
        !app.overlay_signature_for_test()
            .key_remap
            .pending_close_prompt
    );
    assert!(app.overlay_open_for_test());
}

#[test]
fn browsing_wheel_moves_the_keybinding_window_and_highlight() {
    let mut app = build_app();
    app.open_key_bindings_overlay_for_test();
    app.render_overlay_rows_for_test(60, 12);
    let before = app.overlay_signature_for_test().key_remap;
    app.dispatch_wheel_for_test(-1.0);
    let after = app.overlay_signature_for_test().key_remap;
    assert!(after.scroll > before.scroll);
    assert!(after.selected > before.selected);
    assert_ne!(after, before);
    app.dispatch_wheel_for_test(1.0);
    assert_eq!(
        app.overlay_signature_for_test().key_remap.scroll,
        before.scroll
    );
}

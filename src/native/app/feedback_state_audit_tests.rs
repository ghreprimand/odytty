// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored local-input and clipboard-failure fixtures.
use super::App;
use crate::core::{Dimensions, Terminal};
use crate::input::{KeyEventType, Modifiers};
use crate::native::overlay::{OverlayInput, SettingsTarget};
use crate::native::{
    options::NativeOptions, pty::PtyWriter, test_support::headless_app_with_writer,
};
use crate::selection::CellPoint;
use crate::settings::Settings;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};
use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey, PhysicalKey};

#[derive(Default)]
struct Writer {
    bytes: Arc<Mutex<Vec<u8>>>,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
type Model = Arc<Mutex<Terminal>>;
type RecordedBytes = Arc<Mutex<Vec<u8>>>;

fn recording_app() -> (App, Model, RecordedBytes) {
    let recorder = Writer::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, model) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(100, 40),
        Settings::default(),
        writer,
    );
    (app, model, bytes)
}
fn key(app: &mut App, logical: WinitKey, code: KeyCode, ty: KeyEventType) {
    app.drive_raw_key_event_for_test(
        logical.clone(),
        logical,
        PhysicalKey::Code(code),
        Modifiers::default(),
        ty,
    );
}
fn consumed_keys(mode: &[u8]) {
    let (mut app, model, bytes) = recording_app();
    model.lock().unwrap().advance(mode);
    app.queue_osc52_prompt_for_test();
    key(
        &mut app,
        WinitKey::Character("a".into()),
        KeyCode::KeyA,
        KeyEventType::Press,
    );
    key(
        &mut app,
        WinitKey::Character("b".into()),
        KeyCode::KeyB,
        KeyEventType::Press,
    );
    key(
        &mut app,
        WinitKey::Named(NamedKey::Escape),
        KeyCode::Escape,
        KeyEventType::Press,
    );
    assert!(app.osc52_prompt_metadata_for_test().is_none());
    for (text, code) in [("a", KeyCode::KeyA), ("b", KeyCode::KeyB)] {
        key(
            &mut app,
            WinitKey::Character(text.into()),
            code,
            KeyEventType::Repeat,
        );
        key(
            &mut app,
            WinitKey::Character(text.into()),
            code,
            KeyEventType::Release,
        );
    }
    key(
        &mut app,
        WinitKey::Named(NamedKey::Escape),
        KeyCode::Escape,
        KeyEventType::Release,
    );
    assert!(
        bytes.lock().unwrap().is_empty(),
        "consumed down edges must own every repeat/up edge"
    );
    key(
        &mut app,
        WinitKey::Character("a".into()),
        KeyCode::KeyA,
        KeyEventType::Press,
    );
    assert!(
        !bytes.lock().unwrap().is_empty(),
        "a new press after release is real input"
    );
}
#[test]
fn feedback_osc52_consumed_keys_do_not_leak_kitty_events() {
    consumed_keys(b"\x1b[=10u");
}
#[cfg(windows)]
#[test]
fn feedback_osc52_consumed_keys_do_not_leak_win32_events() {
    consumed_keys(b"\x1b[?9001h");
}

#[test]
fn feedback_osc52_cancel_keeps_held_key_ownership() {
    let (mut app, model, bytes) = recording_app();
    model.lock().unwrap().advance(b"\x1b[=10u");
    app.queue_osc52_prompt_for_test();
    key(
        &mut app,
        WinitKey::Character("a".into()),
        KeyCode::KeyA,
        KeyEventType::Press,
    );
    app.cancel_osc52_prompt();
    key(
        &mut app,
        WinitKey::Character("a".into()),
        KeyCode::KeyA,
        KeyEventType::Release,
    );
    assert!(
        bytes.lock().unwrap().is_empty(),
        "cancel must not surrender held keys to the PTY"
    );
}

#[test]
fn feedback_about_failure_does_not_claim_diagnostics_were_copied() {
    let (mut app, _, _) = recording_app();
    app.clipboard.force_write_fail = true;
    app.overlay.open_settings_target(SettingsTarget::Root);
    app.overlay
        .set_about_info(crate::native::about::AboutInfo::collect(None));
    let _ = app.overlay.handle_input(OverlayInput::End);
    let _ = app.overlay.handle_input(OverlayInput::Activate);
    let _ = app.overlay.handle_input(OverlayInput::End);
    let before = app.overlay.render_signature();
    key(
        &mut app,
        WinitKey::Named(NamedKey::Enter),
        KeyCode::Enter,
        KeyEventType::Press,
    );
    let after = app.overlay.render_signature();
    assert_eq!(
        after.panel.message.as_deref(),
        Some("Could not copy diagnostics to clipboard.")
    );
    assert_ne!(
        before, after,
        "clipboard result must change the painted signature"
    );
}

#[test]
fn feedback_osc52_inherited_repeat_cannot_decide_consent() {
    let (mut app, _, _) = recording_app();
    app.queue_osc52_prompt_for_test();
    let logical = WinitKey::Character("s".into());
    app.drive_raw_key_event_for_test(
        logical.clone(),
        logical,
        PhysicalKey::Code(KeyCode::KeyS),
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        },
        KeyEventType::Repeat,
    );
    assert!(
        app.osc52_prompt_metadata_for_test().is_some(),
        "consent needs a fresh down edge"
    );
}

#[test]
fn feedback_about_pointer_feedback_reports_the_clipboard_result() {
    use crate::native::overlay::{OverlayPointer, PointerButton, apply_overlay, overlay_rect};
    for failed in [true, false] {
        let (mut app, model, _) = recording_app();
        app.clipboard.force_write_fail = failed;
        app.overlay.open_settings_target(SettingsTarget::Root);
        app.overlay
            .set_about_info(crate::native::about::AboutInfo::collect(None));
        let _ = app.overlay.handle_input(OverlayInput::End);
        let _ = app.overlay.handle_input(OverlayInput::Activate);
        let mut snapshot = model.lock().unwrap().snapshot();
        apply_overlay(&mut snapshot, &mut app.overlay);
        let columns = snapshot.dimensions.columns;
        let row = snapshot
            .cells
            .chunks(columns)
            .position(|cells| {
                cells
                    .iter()
                    .map(|cell| cell.ch)
                    .collect::<String>()
                    .contains("Copy diagnostics")
            })
            .expect("copy action is visible");
        let rect = overlay_rect(&app.overlay, columns, snapshot.dimensions.rows).unwrap();
        let before = app.overlay.render_signature();
        let outcome = app.overlay.handle_pointer(
            OverlayPointer::Press {
                cell: CellPoint {
                    row,
                    column: rect.body_left + 1,
                },
                button: PointerButton::Left,
                x_in_body: None,
            },
            rect,
        );
        app.apply_overlay_outcome_for_test(outcome);
        let after = app.overlay.render_signature();
        let expected = if failed {
            "Could not copy diagnostics to clipboard."
        } else {
            "Diagnostics copied to clipboard."
        };
        assert_eq!(after.panel.message.as_deref(), Some(expected));
        assert_ne!(before, after, "copy feedback must repaint");
    }
}

#[test]
fn feedback_focus_loss_keeps_consent_owned_releases_local() {
    let (mut app, model, bytes) = recording_app();
    model.lock().unwrap().advance(b"\x1b[=10u");
    app.queue_osc52_prompt_for_test();
    key(
        &mut app,
        WinitKey::Character("a".into()),
        KeyCode::KeyA,
        KeyEventType::Press,
    );
    app.on_window_focus_changed_for_test(false);
    assert!(app.osc52_prompt_metadata_for_test().is_none());
    key(
        &mut app,
        WinitKey::Character("a".into()),
        KeyCode::KeyA,
        KeyEventType::Release,
    );
    assert!(bytes.lock().unwrap().is_empty());
}

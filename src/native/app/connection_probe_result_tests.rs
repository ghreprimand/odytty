// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored probe completions through the App maintenance route.
use super::*;
use crate::native::test_support::{headless_app_with_writer, headless_writer};
use crate::ssh_connect::ProbeClass;
use std::sync::mpsc::{Sender, channel};

fn form_app() -> App {
    let (mut app, _) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 40),
        Settings::default(),
        headless_writer(),
    );
    open_form(&mut app);
    app
}
fn open_form(app: &mut App) {
    app.overlay.open_connections(Vec::new(), Vec::new());
    app.drive_named_key_for_test(winit::keyboard::NamedKey::Tab);
    for ch in "fixture".chars() {
        app.overlay.handle_input(OverlayInput::Char(ch));
    }
    for _ in 0..5 {
        app.drive_named_key_for_test(winit::keyboard::NamedKey::Tab);
    }
}
fn request(app: &mut App) {
    assert!(matches!(
        app.overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::TestConnection(_)
    ));
}
fn injected_probe(app: &mut App) -> Sender<Result<ProbeClass, String>> {
    request(app);
    let (tx, rx) = channel();
    app.connection_probe = Some(super::connection_probe::PendingConnectionProbe {
        identity: app.overlay.connection_form_probe_identity().unwrap(),
        receiver: rx,
    });
    tx
}
fn rows(app: &mut App) -> String {
    app.render_overlay_rows_for_test(80, 40).join("\n")
}
fn edit(app: &mut App) {
    for _ in 0..5 {
        app.drive_named_key_for_test(winit::keyboard::NamedKey::ArrowUp);
    }
    app.overlay.handle_input(OverlayInput::Char('2'));
}
#[test]
fn completion_after_edit_cannot_authenticate_changed_values() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    edit(&mut app);
    tx.send(Ok(ProbeClass::AuthOk)).unwrap();
    app.poll_connection_probe();
    assert!(!rows(&mut app).contains("Reachable"));
    assert!(app.connection_probe.is_none());
}
#[test]
fn completion_after_reopen_cannot_authenticate_the_new_form() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    app.drive_named_key_for_test(winit::keyboard::NamedKey::Escape);
    open_form(&mut app);
    request(&mut app);
    tx.send(Ok(ProbeClass::AuthOk)).unwrap();
    app.poll_connection_probe();
    assert!(!rows(&mut app).contains("Reachable"));
}
#[test]
fn completion_after_retry_of_identical_values_is_stale() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    request(&mut app);
    tx.send(Ok(ProbeClass::AuthOk)).unwrap();
    app.poll_connection_probe();
    assert!(!rows(&mut app).contains("Reachable"));
}
#[test]
fn an_edited_form_drops_an_empty_old_receiver() {
    let mut app = form_app();
    let _tx = injected_probe(&mut app);
    edit(&mut app);
    app.poll_connection_probe();
    assert!(app.connection_probe.is_none());
}
#[test]
fn disconnected_current_worker_finishes_with_an_error() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    drop(tx);
    app.needs_rebuild = false;
    let before = app.overlay_signature_for_test();
    app.poll_connection_probe();
    assert!(app.needs_rebuild);
    assert_ne!(before, app.overlay_signature_for_test());
    assert!(rows(&mut app).contains("probe stopped"));
    assert!(app.connection_probe.is_none());
}
#[test]
fn unchanged_form_accepts_a_current_completion() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    tx.send(Ok(ProbeClass::AuthOk)).unwrap();
    app.poll_connection_probe();
    assert!(rows(&mut app).contains("Reachable"));
    assert!(app.connection_probe.is_none());
}
#[test]
fn a_pending_current_worker_remains_running() {
    let mut app = form_app();
    let _tx = injected_probe(&mut app);
    app.poll_connection_probe();
    assert!(app.connection_probe.is_some());
    assert!(rows(&mut app).contains("Testing"));
}

#[test]
fn navigating_without_editing_keeps_the_current_result_valid() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    app.drive_named_key_for_test(winit::keyboard::NamedKey::Tab);
    tx.send(Ok(ProbeClass::Unreachable)).unwrap();
    app.needs_rebuild = false;
    let before = app.overlay_signature_for_test();
    app.poll_connection_probe();
    assert!(rows(&mut app).contains("Unreachable"));
    assert!(app.needs_rebuild);
    assert_ne!(before, app.overlay_signature_for_test());
}

#[test]
fn disconnected_stale_worker_does_not_replace_new_form_status() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    open_form(&mut app);
    request(&mut app);
    let before = app.overlay_signature_for_test();
    app.needs_rebuild = false;
    drop(tx);
    app.poll_connection_probe();
    assert_eq!(before, app.overlay_signature_for_test());
    assert!(!app.needs_rebuild);
    assert!(rows(&mut app).contains("Testing"));
    assert!(!rows(&mut app).contains("probe stopped"));
}

#[test]
fn closing_the_form_drops_the_receiver_without_repainting() {
    let mut app = form_app();
    let tx = injected_probe(&mut app);
    app.drive_named_key_for_test(winit::keyboard::NamedKey::Escape);
    tx.send(Ok(ProbeClass::AuthOk)).unwrap();
    app.needs_rebuild = false;
    app.poll_connection_probe();
    assert!(app.connection_probe.is_none());
    assert!(app.overlay_rect_for_test().is_none());
    assert!(!app.needs_rebuild);
}

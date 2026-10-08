// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored key-browser fixtures through the App key and render paths.

use crate::core::Dimensions;
use crate::native::{
    options::NativeOptions,
    test_support::{headless_app_with_writer, headless_writer},
};
use crate::settings::Settings;
use winit::keyboard::NamedKey;

#[test]
fn browser_navigation_repaints_the_visible_candidate_through_app_input() {
    let (mut app, _) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 10),
        Settings::default(),
        headless_writer(),
    );
    app.overlay.open_connections(Vec::new(), Vec::new());
    app.drive_named_key_for_test(NamedKey::Tab);
    app.overlay.open_identity_key_browse(
        (0..30)
            .map(|i| format!("/fixtures/keys/key{i:02}"))
            .collect(),
    );
    let first = app.render_overlay_rows_for_test(80, 10);
    assert!(first.iter().any(|row| row.contains("key00")));
    assert!(!first.iter().any(|row| row.contains("key29")));
    let before = app.overlay_signature_for_test();
    app.drive_named_key_for_test(NamedKey::End);
    let last = app.render_overlay_rows_for_test(80, 10);
    assert!(last.iter().any(|row| row.contains("key29")));
    assert!(!last.iter().any(|row| row.contains("key00")));
    assert_ne!(before, app.overlay_signature_for_test());
    app.drive_named_key_for_test(NamedKey::Home);
    let returned = app.render_overlay_rows_for_test(80, 10);
    assert_eq!(returned, first);
}

fn short_form_app(edit: bool) -> super::App {
    let (mut app, _) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 10),
        Settings::default(),
        headless_writer(),
    );
    let hosts = if edit {
        vec![crate::connection_hosts::ConnectionHost {
            alias: "fixture".into(),
            host_name: Some("fixture.example.invalid".into()),
            user: None,
            port: None,
            theme: None,
            font: None,
            title: None,
            integration: None,
            reuse: None,
            tmux: None,
            protocol: None,
            persist: None,
            identity_file: None,
            source: crate::connection_hosts::ConnectionHostSource::Odytty,
        }]
    } else {
        Vec::new()
    };
    app.overlay.open_connections(hosts, Vec::new());
    app.drive_named_key_for_test(if edit {
        NamedKey::ArrowRight
    } else {
        NamedKey::Tab
    });
    app
}

fn click_text(app: &mut super::App, text: &str) {
    let rows = app.render_overlay_rows_for_test(80, 10);
    let row = rows
        .iter()
        .position(|r| r.contains(text))
        .expect("control visible");
    let rect = app.overlay_rect_for_test().unwrap();
    app.set_pointer_cell_for_test(row, rect.body_left + 1);
    app.handle_overlay_pointer_button(
        winit::event::ElementState::Pressed,
        winit::event::MouseButton::Left,
    );
    app.handle_overlay_pointer_button(
        winit::event::ElementState::Released,
        winit::event::MouseButton::Left,
    );
}

#[test]
fn short_add_form_follows_keyboard_focus_to_cancel() {
    let mut app = short_form_app(false);
    app.render_overlay_rows_for_test(80, 10);
    let before = app.overlay_signature_for_test();
    for _ in 0..7 {
        app.drive_named_key_for_test(NamedKey::Tab);
    }
    let rows = app.render_overlay_rows_for_test(80, 10);
    assert!(rows.iter().any(|r| r.contains("Cancel")), "{rows:?}");
    assert_ne!(before, app.overlay_signature_for_test());
    click_text(&mut app, "Cancel");
    assert!(app.overlay_rect_for_test().is_none());
}

#[test]
fn short_edit_advanced_form_follows_focus_and_pointer_mapping() {
    let mut app = short_form_app(true);
    for _ in 0..4 {
        app.drive_named_key_for_test(NamedKey::Tab);
    }
    click_text(&mut app, "Advanced");
    app.drive_named_key_for_test(NamedKey::Tab);
    let rows = app.render_overlay_rows_for_test(80, 10);
    assert!(rows.iter().any(|r| r.contains("IdentityFile")), "{rows:?}");
    for _ in 0..8 {
        app.drive_named_key_for_test(NamedKey::Tab);
    }
    let rows = app.render_overlay_rows_for_test(80, 10);
    assert!(rows.iter().any(|r| r.contains("Save")), "{rows:?}");
    app.drive_named_key_for_test(NamedKey::Tab);
    click_text(&mut app, "Cancel");
    assert!(app.overlay_rect_for_test().is_none());
}

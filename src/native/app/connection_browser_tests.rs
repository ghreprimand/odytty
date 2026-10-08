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

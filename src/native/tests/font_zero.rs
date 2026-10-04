// SPDX-License-Identifier: GPL-3.0-only
//! Alternate-zero control through the real settings path: the Fonts row
//! toggles through the production overlay-key route, applies live through the
//! reload seam (which publishes the switch the renderer's atlas and shaper read
//! while it applies text options), re-keys the frame cache, and persists
//! `font_zero` on Save.

use super::*;
use winit::keyboard::{Key as WinitKey, NamedKey};

fn temp_conf(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("odytty-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("odytty.conf");
    std::fs::write(&path, "# kept\n").unwrap();
    path
}

fn key(app: &mut App, key: WinitKey, ctrl: bool) {
    app.drive_overlay_key_for_test(key, ctrl, false);
}

#[test]
fn font_zero_row_toggles_live_rekeys_the_frame_and_persists() {
    // Serialize with other render-global tests. In a test binary the reload
    // seam restores the process-wide switches when it returns, so the live
    // value is read from the App's settings.
    let _render_globals = crate::test_lock::render_globals_lock();
    let (mut app, _terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(40, 8),
        Settings::default(),
    );
    let conf = temp_conf("font-zero-panel");
    app.set_config_path_for_test(conf.clone());
    assert!(!app.font_zero_setting_for_test(), "off by default");

    app.open_settings_overlay_for_test();
    // Fonts is the second section of the Level-1 list.
    key(&mut app, WinitKey::Named(NamedKey::ArrowDown), false);
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    let signature = app.overlay_signature_for_test();
    assert!(
        signature
            .panel
            .entries
            .iter()
            .any(|entry| entry.key == "font_size"),
        "drilled into the Fonts section"
    );
    let target = signature
        .panel
        .entries
        .iter()
        .position(|entry| entry.key == "font_zero")
        .expect("font_zero row in Fonts");
    for _ in signature.panel.selected..target {
        key(&mut app, WinitKey::Named(NamedKey::ArrowDown), false);
    }
    assert_eq!(
        app.settings_panel_displayed_value_for_test("font_zero"),
        Some("off".to_owned())
    );

    let epoch = app.presentation_epoch_for_test();
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert_eq!(
        app.settings_panel_displayed_value_for_test("font_zero"),
        Some("on".to_owned())
    );
    assert!(app.font_zero_setting_for_test(), "the toggle applies live");
    assert!(
        app.presentation_epoch_for_test() > epoch,
        "the toggle re-keys the render signature"
    );

    key(&mut app, WinitKey::Character("s".into()), true);
    let saved = std::fs::read_to_string(&conf).unwrap();
    assert!(saved.contains("font_zero = on"), "{saved}");
    assert!(saved.contains("# kept"));

    // Toggling back is live as well.
    let epoch = app.presentation_epoch_for_test();
    key(&mut app, WinitKey::Named(NamedKey::Enter), false);
    app.flush_pending_overlay_settings_for_test();
    assert!(!app.font_zero_setting_for_test());
    assert!(app.presentation_epoch_for_test() > epoch);

    let _ = std::fs::remove_dir_all(conf.parent().unwrap());
}

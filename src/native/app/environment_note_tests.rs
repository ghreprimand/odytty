// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored Settings environment precedence regressions.
use super::*;
use crate::native::test_support::headless_app_with;
use winit::keyboard::KeyCode;

fn key(app: &mut App, logical: WinitKey, code: KeyCode, ctrl: bool, shift: bool) {
    for kind in [KeyEventType::Press, KeyEventType::Release] {
        app.drive_raw_key_event_for_test(
            logical.clone(),
            logical.clone(),
            PhysicalKey::Code(code),
            Modifiers {
                ctrl,
                shift,
                ..Modifiers::default()
            },
            kind,
        );
    }
}

fn select(app: &mut App) {
    key(
        app,
        WinitKey::Character(",".into()),
        KeyCode::Comma,
        true,
        true,
    );
    for section in 0..24 {
        for _ in 0..section {
            key(
                app,
                WinitKey::Named(NamedKey::ArrowDown),
                KeyCode::ArrowDown,
                false,
                false,
            );
        }
        key(
            app,
            WinitKey::Named(NamedKey::Enter),
            KeyCode::Enter,
            false,
            false,
        );
        let sig = app.overlay_signature_for_test();
        if let Some(target) = sig
            .panel
            .entries
            .iter()
            .position(|e| e.key == "bidi_reorder")
        {
            for _ in sig.panel.selected..target {
                key(
                    app,
                    WinitKey::Named(NamedKey::ArrowDown),
                    KeyCode::ArrowDown,
                    false,
                    false,
                );
            }
            return;
        }
        key(
            app,
            WinitKey::Named(NamedKey::Escape),
            KeyCode::Escape,
            false,
            false,
        );
        for _ in 0..section {
            key(
                app,
                WinitKey::Named(NamedKey::ArrowUp),
                KeyCode::ArrowUp,
                false,
                false,
            );
        }
    }
    panic!("bidi row missing");
}

fn fixture() -> App {
    let (mut app, _) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(120, 40),
        Settings::default(),
    );
    app.settings_reloader.set_env_values_for_test(
        [(
            crate::settings::BIDI_REORDER_ENV,
            std::ffi::OsString::from("off"),
        )]
        .into_iter()
        .collect(),
    );
    select(&mut app);
    app
}

#[test]
fn overridden_row_names_the_variable_before_truncated_help() {
    let mut app = fixture();
    let lines = app.render_overlay_rows_for_test(120, 40).join("\n");
    assert!(
        lines.contains("Environment override: ODYTTY_BIDI_REORDER"),
        "override notice missing"
    );
    assert!(
        lines.contains("Save still writes"),
        "saved edit explanation missing"
    );
}

#[test]
fn saving_an_overridden_edit_keeps_config_and_explains_the_effective_value() {
    let _globals = crate::test_lock::render_globals_lock();
    let dir = crate::test_dirs::fresh_temp_dir("environment-note");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let path = dir.join("odytty.conf");
    std::fs::write(&path, "bidi_reorder = off\n").expect("fixture config");
    let mut app = fixture();
    app.settings_reloader
        .set_config_path_for_test(Some(path.clone()));
    let before = app.overlay_signature_for_test();
    key(
        &mut app,
        WinitKey::Named(NamedKey::Enter),
        KeyCode::Enter,
        false,
        false,
    );
    app.flush_pending_overlay_settings_for_test();
    assert!(app.settings.bidi_reorder);
    assert_ne!(before, app.overlay_signature_for_test());
    key(
        &mut app,
        WinitKey::Character("s".into()),
        KeyCode::KeyS,
        true,
        false,
    );
    let saved = std::fs::read_to_string(&path).expect("saved config");
    assert!(saved.contains("bidi_reorder = on"));
    assert!(
        !app.settings.bidi_reorder,
        "startup environment still wins on Save"
    );
    let lines = app.render_overlay_rows_for_test(120, 40).join("\n");
    assert!(
        lines.contains("Environment override: ODYTTY_BIDI_REORDER"),
        "Save hides override explanation"
    );
}

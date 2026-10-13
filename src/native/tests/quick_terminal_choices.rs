// SPDX-License-Identifier: GPL-3.0-only
//! Fixed quick-terminal choices through the native settings input path.

use super::*;
use crate::settings::SettingKind;
use winit::keyboard::{Key as WinitKey, NamedKey};

#[test]
fn quick_terminal_settings_rows_offer_exact_fixed_choices() {
    for (key, choices) in [
        (
            "quick_terminal_edge",
            &["top", "bottom", "left", "right"][..],
        ),
        ("quick_terminal_animation", &["slide", "instant"][..]),
    ] {
        let (mut app, _terminal) = headless_app_with(
            NativeOptions::default(),
            Dimensions::new(80, 24),
            Settings::default(),
        );
        app.open_settings_section_for_test("Sessions");
        let signature = app.overlay_signature_for_test();
        let target = signature
            .panel
            .entries
            .iter()
            .position(|entry| entry.key == key)
            .unwrap();
        for _ in signature.panel.selected..target {
            app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::ArrowDown), false, false);
        }
        let entry = app.settings_selected_info_for_test().unwrap();
        assert_eq!(entry.key, key);
        assert_eq!(entry.kind, SettingKind::Enum);
        assert_eq!(entry.options, choices);
        assert!(
            !entry.reloadable,
            "quick-terminal choices remain startup-only"
        );
        let before = app.overlay_signature_for_test();
        app.drive_overlay_key_for_test(WinitKey::Named(NamedKey::Enter), false, false);
        let after = app.overlay_signature_for_test();
        assert_eq!(after.panel.editing_key, None);
        assert_eq!(after.panel.entries, before.panel.entries);
        assert!(
            after
                .panel
                .message
                .as_deref()
                .unwrap()
                .contains("Startup-only")
        );
        assert_ne!(
            after, before,
            "the startup-only notice invalidates the frame"
        );
    }
}

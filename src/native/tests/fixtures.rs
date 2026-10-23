// SPDX-License-Identifier: GPL-3.0-only
//! Shared native test inputs and explicit row navigation.

use super::*;

pub(super) fn split_app_with_writers(
    panes: usize,
    first_writer: PtyWriter,
    mut next_writer: impl FnMut() -> PtyWriter,
) -> (App, Arc<Mutex<Terminal>>) {
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        first_writer,
    );
    app.set_test_cell_for_test(cell(8, 16));
    app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    let first = app.active_session_token_for_test();
    for _ in 1..panes {
        app.seed_headless_split_pane_for_test(
            true,
            Arc::new(Mutex::new(Terminal::new(40, 24))),
            next_writer(),
            Dimensions::new(40, 24),
        );
    }
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(cell(8, 16));
        app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    }
    app.focus_session_token_for_test(first);
    app.reflow_active_panes_for_test();
    (app, terminal)
}

pub(super) fn focus_context_menu_label(app: &mut App, label: &str) {
    let labels = app.context_menu_labels_for_test();
    assert!(labels.contains(&label), "menu contains target {label}");
    for _ in 0..=labels.len() {
        if app.context_menu_focused_label_for_test() == Some(label) {
            return;
        }
        app.drive_overlay_key_for_test(
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown),
            false,
            false,
        );
    }
    panic!("menu focus did not reach {label}");
}

pub(super) fn select_setting(app: &mut App, section: &str, key: &str) {
    use winit::keyboard::{Key, NamedKey};
    app.open_settings_section_for_test(section);
    let panel = app.overlay_signature_for_test().panel;
    let target = panel
        .entries
        .iter()
        .position(|entry| entry.key == key)
        .unwrap_or_else(|| panic!("missing {section} setting {key}"));
    app.drive_overlay_key_for_test(Key::Named(NamedKey::Home), false, false);
    for _ in 0..target {
        app.drive_overlay_key_for_test(Key::Named(NamedKey::ArrowDown), false, false);
    }
    assert_eq!(
        app.settings_selected_info_for_test()
            .expect("selected setting")
            .key,
        key
    );
}

/// A closed-overlay render signature (no `Default` on the type).
pub(super) fn closed_overlay_signature() -> OverlayRenderSignature {
    OverlayRenderSignature {
        open: false,
        mode: OverlayMode::Settings,
        panel: SettingsPanelSignature {
            environment_overrides: Vec::new(),
            surface_opaque: false,
            selected: 0,
            scroll: 0,
            editing_key: None,
            editing_buffer: None,
            changed_count: 0,
            message: None,
            entries: Vec::new(),
            query: String::new(),
            search_active: false,
            level: crate::native::settings_panel::SettingsLevel::SectionList,
            section_selected: 0,
            section_scroll: 0,
            pending_close_prompt: false,
            path_picker: None,
        },
        theme_picker: ThemePickerSignature {
            selected: 0,
            scroll: 0,
            original: "plain",
            current: "plain",
            message: None,
            entries: Vec::new(),
        },
        theme_builder: ThemeBuilderSignature {
            original: "plain",
            selected: 0,
            scroll: 0,
            editing: None,
            message: None,
            channel: "L (lightness)",
            selected_color: (0, 0, 0),
        },
        profile_manager: crate::native::profile_manager::ProfileManagerSignature::default(),
        font_picker: FontPickerSignature {
            selected: 0,
            scroll: 0,
            original: String::new(),
            current: String::new(),
            query: String::new(),
            message: None,
            entries: Vec::new(),
        },
        key_remap: KeyRemapSignature {
            selected: 0,
            scroll: 0,
            pending_close_prompt: false,
            capture: None,
            conflict: None,
            message: None,
            bindings: String::new(),
        },
        onboarding: OnboardingSignature::default(),
        context_menu: ContextMenuSignature::default(),
        command_palette: PaletteOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        replay: ReplayOverlaySignature {
            cursor: 0,
            frames_len: 0,
            frame_fingerprint: 0,
        },
        connections: ConnectionOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        connection_form: crate::native::connection_form::ConnectionFormSignature::default(),
        session_attach: SessionAttachOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        open_with: OpenWithOverlaySignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        workspace_picker: WorkspacePickerSignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        profile_picker: ProfilePickerSignature {
            query: String::new(),
            selected: None,
            results_len: 0,
            results_fingerprint: 0,
        },
        dialog_payload: 0,
    }
}

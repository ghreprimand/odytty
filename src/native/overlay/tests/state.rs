// SPDX-License-Identifier: GPL-3.0-only
//! Overlay state and navigation tests: mode transitions, latch clearing,
//! picker return paths, and close behavior.

use super::*;

#[test]
fn settings_save_failure_disarms_close_after_save() {
    // SaveAndClose arms `close_after_save`; if the write fails, the latch
    // must clear so a later plain save cannot close the panel unbidden.
    let mut overlay = OverlayUi {
        mode: OverlayMode::Settings,
        close_after_save: true,
        ..OverlayUi::default()
    };
    overlay.save_failed("disk full".to_owned());
    assert!(
        !overlay.close_after_save,
        "a failed settings save must not leave close-after-save armed"
    );
}

#[test]
fn keybind_save_failure_disarms_close_after_save() {
    let mut overlay = OverlayUi {
        mode: OverlayMode::KeyBindings,
        key_remap_close_after_save: true,
        ..OverlayUi::default()
    };
    overlay.save_failed("disk full".to_owned());
    assert!(
        !overlay.key_remap_close_after_save,
        "a failed keybind save must not leave close-after-save armed"
    );
}

#[test]
fn escape_requests_close_without_mutating_state() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    let before = overlay.render_signature();

    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Close
    );
    assert_eq!(overlay.render_signature(), before);
}

#[test]
fn confirm_close_open_is_idempotent() {
    // TRAP-3: a repeated close request (some window managers fire twice)
    // must not stack dialogs — open_confirm_close starts with close().
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_close();
    overlay.open_confirm_close();
    assert!(overlay.is_open());
    assert_eq!(overlay.render_signature().mode, OverlayMode::ConfirmClose);
}

#[test]
fn escape_while_searching_does_not_close_overlay() {
    // R7: the overlay-close Esc is gated on `!is_searching()`, so an Esc in
    // search mode runs the panel's two-step exit instead of closing.
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    assert_eq!(
        overlay.handle_input(OverlayInput::Char('/')),
        OverlayOutcome::Consumed
    );
    for ch in "cursor".chars() {
        let _ = overlay.handle_input(OverlayInput::Char(ch));
    }
    // First Esc clears the query, overlay stays open.
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    // Second Esc exits search, still not closing the overlay.
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    // With search fully exited, Esc now closes.
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Close
    );
}

#[test]
fn theme_picker_cancel_restores_original_theme_and_closes() {
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);

    assert!(matches!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::ApplySettings(_)
    ));
    let OverlayOutcome::ApplySettings(settings) = overlay.handle_input(OverlayInput::Close) else {
        panic!("expected restoration settings");
    };

    assert_eq!(settings.theme, crate::theme::Theme::ODYSSEY);
    assert!(!overlay.is_open());
}

#[test]
fn theme_picker_cancel_returns_to_settings_when_launched_from_settings_panel() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    let original_theme = overlay.settings.theme;
    // Open Themes section and activate the theme row.
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenThemePicker
    );

    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);

    let OverlayOutcome::ApplySettings(restored) = overlay.handle_input(OverlayInput::Close) else {
        panic!("expected settings restore when canceling the theme picker");
    };
    assert_eq!(restored.theme, original_theme);
    assert!(overlay.is_open());
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "cancel should return to Settings from picker"
    );
}

#[test]
fn theme_picker_save_returns_to_settings_when_launched_from_settings_panel() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();

    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenThemePicker
    );

    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);

    assert!(matches!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::ApplySettings(_)
    ));
    let OverlayOutcome::SaveSettings(changes) = overlay.handle_input(OverlayInput::Activate) else {
        panic!("expected theme picker save request");
    };
    assert_eq!(changes.len(), 1);

    overlay.save_succeeded(changes.len());
    assert!(overlay.is_open());
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "theme picker apply from settings should return to settings panel"
    );
}

#[test]
fn theme_picker_canonical_reload_replaces_stale_user_theme_label_before_return() {
    let mut overlay = OverlayUi::new(&Settings {
        // Model a file-loaded theme: its runtime name is the static `custom`
        // placeholder while the configured token remains displayable.
        theme: crate::theme::Theme {
            name: "custom",
            ..crate::theme::Theme::ODYSSEY
        },
        theme_config: Some("red-planet-dark".to_owned()),
        ..Settings::default()
    });
    overlay.open_settings();

    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenThemePicker
    );
    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);

    let OverlayOutcome::ApplySettings(preview) = overlay.handle_input(OverlayInput::Down) else {
        panic!("expected theme preview");
    };
    let selected = preview.theme;
    overlay.apply_settings(&preview);
    let OverlayOutcome::SaveSettings(_) = overlay.handle_input(OverlayInput::Activate) else {
        panic!("expected theme picker save request");
    };

    // Production re-reads odytty.conf and applies this canonical Settings while
    // ThemePicker is still active, before save_succeeded returns to Settings.
    let canonical = Settings {
        theme: selected,
        theme_config: None,
        theme_is_system: false,
        ..overlay.settings.clone()
    };
    overlay.apply_settings(&canonical);
    overlay.save_succeeded(1);

    assert_eq!(
        overlay.settings_panel_value_for_test("theme"),
        Some(selected.name.to_owned()),
        "the parent panel must show the canonical saved theme, not the old user-theme token"
    );
}

#[test]
fn theme_picker_save_then_panel_commit_keeps_external_theme() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();

    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenThemePicker
    );

    let settings = overlay.settings.clone();
    overlay.open_theme_picker(&settings);

    let OverlayOutcome::ApplySettings(preview) = overlay.handle_input(OverlayInput::Down) else {
        panic!("expected theme preview");
    };
    let preview_theme = preview.theme;
    overlay.apply_settings(&preview);

    let OverlayOutcome::SaveSettings(changes) = overlay.handle_input(OverlayInput::Activate) else {
        panic!("expected theme picker save request");
    };
    overlay.save_succeeded(changes.len());

    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    while overlay.render_signature().panel.section_selected != 1 {
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
    }
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    for _ in 0..8 {
        if overlay
            .render_signature()
            .panel
            .entries
            .get(overlay.render_signature().panel.selected)
            .is_some_and(|entry| entry.key == "font_size")
        {
            break;
        }
        assert_eq!(
            overlay.handle_input(OverlayInput::Down),
            OverlayOutcome::Consumed
        );
    }
    assert_eq!(
        overlay
            .render_signature()
            .panel
            .entries
            .get(overlay.render_signature().panel.selected)
            .map(|entry| entry.key),
        Some("font_size")
    );
    let OverlayOutcome::ApplySettings(committed) = overlay.handle_input(OverlayInput::Right) else {
        panic!("expected second settings edit to apply");
    };

    assert_eq!(
        committed.theme, preview_theme,
        "panel commit must not rebuild settings from the old theme baseline"
    );
}

#[test]
fn font_picker_cancel_returns_to_settings_when_launched_from_settings_panel() {
    let mut overlay = OverlayUi::default();
    overlay.open_settings();

    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    let original_font_family = overlay.settings.font_family.clone();
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenFontPicker
    );

    let settings = overlay.settings.clone();
    overlay.open_font_picker(&settings);
    let outcome = overlay.handle_input(OverlayInput::Close);
    assert_eq!(outcome, OverlayOutcome::Consumed);
    assert!(overlay.is_open());
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "font picker cancel should return to Settings panel"
    );
    assert_eq!(overlay.settings.font_family, original_font_family);
}

#[test]
fn font_picker_apply_stays_open_when_launched_from_settings_panel() {
    // FONT-PICKER-STAY-OPEN: Enter applies+saves the font but KEEPS the
    // picker open so the user can keep cycling. It must NOT return to the
    // settings panel after the save succeeds.
    let mut overlay = OverlayUi::default();
    overlay.open_settings();

    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenFontPicker
    );

    let settings = overlay.settings.clone();
    overlay.open_font_picker(&settings);
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    let outcome = overlay.handle_input(OverlayInput::Activate);
    let OverlayOutcome::SaveSettings(changes) = outcome else {
        panic!("expected font picker save request");
    };
    assert_eq!(changes.len(), 1);
    // The applied family is the value in the SettingEdit (the source of
    // truth for what was just applied); `overlay.settings` is only refreshed
    // by the app's reload path, not by `save_succeeded`.
    let applied = changes[0].value.clone();

    overlay.save_succeeded(changes.len());
    // Stay-open contract: still open, still in FontPicker mode.
    assert!(overlay.is_open(), "picker must stay open after apply");
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::FontPicker,
        "font picker apply must stay in FontPicker mode, not return to panel"
    );

    // The applied family now shows the "current" marker in the rendered list
    // (font_picker.save_succeeded adopts it as self.original).
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let lines = overlay.visible_lines(rect.body_width, rect.body_height);
    assert!(
        lines
            .iter()
            .any(|line| line.text.contains("current") && line.text.contains(&applied)),
        "applied family {applied:?} must render with the current marker after apply"
    );

    // Esc still returns to the settings panel (the panel-launched path).
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Consumed
    );
    assert!(overlay.is_open());
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "Esc must return to the settings panel after the panel-launched picker"
    );
}

#[test]
fn font_picker_apply_stays_open_then_esc_closes_standalone() {
    // FONT-PICKER-STAY-OPEN standalone path (Ctrl+Shift+F): Enter applies and
    // keeps the picker open; a second Enter applies another font; Esc closes.
    let mut overlay = OverlayUi::default();
    overlay.open_font_picker(&overlay.settings.clone());
    assert_eq!(overlay.render_signature().mode, OverlayMode::FontPicker);

    // First Enter: apply + stay open.
    assert_eq!(
        overlay.handle_input(OverlayInput::Down),
        OverlayOutcome::Consumed
    );
    let outcome = overlay.handle_input(OverlayInput::Activate);
    let OverlayOutcome::SaveSettings(changes) = outcome else {
        panic!("expected first save request");
    };
    overlay.save_succeeded(changes.len());
    assert!(overlay.is_open(), "picker stays open after first apply");
    assert_eq!(overlay.render_signature().mode, OverlayMode::FontPicker);

    // Second Enter: still persists and still stays open (cycling works).
    let outcome = overlay.handle_input(OverlayInput::Activate);
    let OverlayOutcome::SaveSettings(changes2) = outcome else {
        panic!("expected second save request");
    };
    overlay.save_succeeded(changes2.len());
    assert!(overlay.is_open(), "picker stays open after second apply");
    assert_eq!(overlay.render_signature().mode, OverlayMode::FontPicker);

    // Esc on the standalone path fully closes the overlay.
    assert_eq!(
        overlay.handle_input(OverlayInput::Close),
        OverlayOutcome::Close
    );
    assert!(!overlay.is_open(), "standalone Esc closes the picker");
}

// --- Phase 14: attach-choice dialog (open / key route / click parity) ---

#[test]
fn attach_choice_opens_in_attach_choice_mode() {
    let mut overlay = OverlayUi::default();
    overlay.open_attach_choice("s-0001-aaaa".to_owned());
    assert!(
        overlay.is_attach_choice(),
        "the dialog opens in AttachChoice"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::AttachChoice,
        "render signature reflects the new mode"
    );
    // The dialog has a real centered rect (renderable).
    assert!(overlay_rect(&overlay, 80, 24).is_some());
}

// --- Manage Sessions: kill-confirmation dialog (open / key / click parity) ---

#[test]
fn confirm_kill_session_opens_in_confirm_kill_mode() {
    let mut overlay = OverlayUi::default();
    overlay.open_confirm_kill_session("s-0001-aaaa".to_owned());
    assert!(
        overlay.is_confirm_kill_session(),
        "the dialog opens in ConfirmKillSession"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::ConfirmKillSession,
        "render signature reflects the new mode"
    );
    // The dialog has a real centered rect (renderable), and the body names
    // the target session.
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let lines = overlay.visible_lines(rect.body_width, rect.body_height);
    assert!(lines[0].text.contains("s-0001-aaaa"), "body names the id");
}

// --- Detach & switch choice dialog (open / key / click parity) ---

#[test]
fn detach_switch_opens_in_detach_switch_mode_and_names_cwd() {
    let mut overlay = OverlayUi::default();
    overlay.open_detach_switch_choice("/home/user/proj".to_owned());
    assert!(
        overlay.is_detach_switch_choice(),
        "the dialog opens in DetachSwitchChoice"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::DetachSwitchChoice,
        "render signature reflects the new mode"
    );
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    let lines = overlay.visible_lines(rect.body_width, rect.body_height);
    assert!(
        lines[0].text.contains("/home/user/proj"),
        "body names the cwd"
    );
}

#[test]
fn settings_stepper_click_cannot_leave_drag_state_across_close_reopen() {
    // Settings steppers do not arm drag state, so a missing release cannot
    // survive close/reopen or drive a phantom value.
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Drill into Fonts section to get a stepper row.
    overlay.handle_input(OverlayInput::Down); // Fonts
    overlay.handle_input(OverlayInput::Activate); // drill in
    let (_, up) = overlay
        .first_stepper_button_cells(80, 24)
        .expect("a stepper row is visible in Fonts section");
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Click a stepper, then close WITHOUT a release.
    let _ = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: up,
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert!(
        !overlay.is_settings_dragging(),
        "settings stepper click does not arm drag state"
    );
    overlay.close();
    assert!(!overlay.is_settings_dragging());

    // Reopen and assert a bare Move does nothing.
    overlay.open_settings();
    assert!(!overlay.is_settings_dragging(), "reopen has no stale drag");
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    assert_eq!(
        overlay.handle_pointer(
            OverlayPointer::Move {
                cell: up,
                x_in_body: None
            },
            rect
        ),
        OverlayOutcome::Consumed,
        "hover after reopen is inert"
    );
}

#[test]
fn focus_loss_after_settings_stepper_click_keeps_overlay_open_and_inert() {
    // Settings steppers never arm drag state. Focus-loss cleanup remains
    // safe and a bare hover Move on focus regain cannot commit a phantom
    // numeric value.
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Drill into Fonts section to get a stepper row.
    overlay.handle_input(OverlayInput::Down); // Fonts
    overlay.handle_input(OverlayInput::Activate); // drill in
    let (down, up) = overlay
        .first_stepper_button_cells(80, 24)
        .expect("a stepper row is visible in Fonts section");
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");

    // Click the up button.
    let _ = overlay.handle_pointer(
        OverlayPointer::Press {
            cell: up,
            button: PointerButton::Left,
            x_in_body: None,
        },
        rect,
    );
    assert!(
        !overlay.is_settings_dragging(),
        "settings stepper click does not arm drag state"
    );

    // Focus loss WITHOUT a release and WITHOUT a close.
    overlay.cancel_settings_drag();
    assert!(overlay.is_open(), "focus loss does not close the overlay");
    assert!(!overlay.is_settings_dragging());

    // A bare hover Move after focus regain is inert.
    let rect = overlay_rect(&overlay, 80, 24).expect("rect");
    assert_eq!(
        overlay.handle_pointer(
            OverlayPointer::Move {
                cell: down,
                x_in_body: None
            },
            rect
        ),
        OverlayOutcome::Consumed,
        "hover after focus regain is inert"
    );
    assert!(
        !overlay.is_settings_dragging(),
        "hover did not re-arm the drag"
    );
}

#[test]
fn key_bindings_esc_returns_to_settings() {
    // Keyboard Esc in KeyBindings navigates back to Settings (consistent
    // with pickers' cancel-to-return path).
    let mut overlay = OverlayUi::default();
    let settings = overlay.settings.clone();
    overlay.open_settings();
    overlay.open_key_bindings(&settings);
    assert_eq!(overlay.render_signature().mode, OverlayMode::KeyBindings);

    let outcome = overlay.handle_input(OverlayInput::Close);
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "key bindings Esc should emit ApplySettings"
    );
    assert!(overlay.is_open(), "overlay stays open after Esc");
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "key bindings Esc returns to Settings panel"
    );
}

#[test]
fn theme_builder_esc_from_picker_returns_to_theme_picker() {
    // ThemeBuilder opened from ThemePicker: keyboard Esc returns to
    // ThemePicker (cancel edits, restore original theme, stay open).
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    // Manually set up the picker → builder transition.
    overlay.open_theme_picker(&settings);
    overlay.theme_builder.open(&settings);
    overlay.mode = OverlayMode::ThemeBuilder;
    overlay.builder_from_picker = true;

    let outcome = overlay.handle_input(OverlayInput::Close);
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "theme builder Esc should emit ApplySettings"
    );
    assert!(overlay.is_open(), "overlay stays open after Esc");
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::ThemePicker,
        "theme builder Esc returns to ThemePicker when opened from picker"
    );
}

#[test]
fn theme_builder_esc_standalone_closes_overlay() {
    // ThemeBuilder opened standalone (not from ThemePicker): Esc / click-away
    // closes the overlay entirely (existing behavior, unaffected by back-nav).
    let mut overlay = OverlayUi::new(&Settings {
        theme: crate::theme::Theme::ODYSSEY,
        ..Settings::default()
    });
    let settings = overlay.settings.clone();
    overlay.open_theme_builder(&settings); // standalone path, builder_from_picker = false
    assert_eq!(overlay.render_signature().mode, OverlayMode::ThemeBuilder);

    let outcome = overlay.handle_input(OverlayInput::Close);
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "standalone builder Esc emits ApplySettings (restore theme)"
    );
    assert!(
        !overlay.is_open(),
        "standalone builder Esc closes the overlay"
    );
}

#[test]
fn theme_builder_esc_from_settings_returns_to_settings_panel() {
    // ThemeBuilder opened from the settings panel (the Themes section's
    // "Open Theme Builder" action row): Esc / back-button returns to the
    // settings panel at the Themes section, not a full overlay close, and
    // never leaves the panel parked at a deep level for the next open.
    let mut overlay = OverlayUi::default();
    overlay.open_settings();
    // Drill into Themes (the first section), then activate the last row —
    // the synthetic "Open Theme Builder" action row.
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::End),
        OverlayOutcome::Consumed
    );
    assert_eq!(
        overlay.handle_input(OverlayInput::Activate),
        OverlayOutcome::OpenThemeBuilder,
        "the Themes action row opens the builder"
    );
    // The App bounces OpenThemeBuilder into open_theme_builder.
    let settings = overlay.settings.clone();
    overlay.open_theme_builder(&settings);
    assert_eq!(overlay.render_signature().mode, OverlayMode::ThemeBuilder);

    let outcome = overlay.handle_input(OverlayInput::Close);
    assert!(
        matches!(outcome, OverlayOutcome::ApplySettings(_)),
        "builder Esc emits ApplySettings (restore theme)"
    );
    assert!(
        overlay.is_open(),
        "builder Esc from the Settings path keeps the overlay open"
    );
    assert_eq!(
        overlay.render_signature().mode,
        OverlayMode::Settings,
        "builder Esc returns to the settings panel"
    );
    assert_eq!(
        overlay.settings_active_section_for_test(),
        Some("Themes"),
        "the panel resumes at the Themes section it was left on"
    );
}

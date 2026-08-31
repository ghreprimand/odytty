// SPDX-License-Identifier: GPL-3.0-only
//! Unit tests for the settings panel. Kept as the child `mod tests` of
//! `settings_panel`, so `use super::*` still reaches the module's private
//! items -- this is a pure file move for module-size relief, not a behavior
//! or visibility change.

use super::*;
use crate::settings::{FONT_SIZE_ENV, Settings};
use std::collections::BTreeSet;

const EXPERT_ONLY_GROUPS: &[&str] = &[];

// ── Helpers ──────────────────────────────────────────────────────────────

/// Navigate to Level 2 for the section containing `key`, then select that
/// entry. Keeps the rest of the panel state (edits, etc.) intact, so callers
/// can test value changes without re-creating the panel.
fn select_key(panel: &mut SettingsPanel, key: &str) {
    let group = panel
        .all_entries
        .iter()
        .find(|e| e.key == key)
        .expect("known key")
        .group;
    let section_index = SECTIONS
        .iter()
        .position(|s| s.groups.contains(&group))
        .expect("known group in SECTIONS");
    // Only drill in if not already in the right section.
    match &panel.level {
        SettingsLevel::SectionDetail { section_index: si } if *si == section_index => {}
        _ => panel.drill_into_section(section_index),
    }
    let idx = panel
        .entries
        .iter()
        .position(|e| e.key == key)
        .expect("key in section entries");
    panel.set_selection(idx);
}

fn clear_edit_buffer(panel: &mut SettingsPanel) {
    let len = panel
        .editing
        .as_ref()
        .map(|edit| edit.buffer.chars().count())
        .unwrap_or(0);
    for _ in 0..len {
        let _ = panel.handle_input(OverlayInput::Backspace);
    }
}

fn poll_path_picker(panel: &mut SettingsPanel) {
    for _ in 0..50 {
        panel.update_body_height(20);
        panel.update_body_width(80);
        if panel
            .build_visible_rows(80, 20)
            .iter()
            .all(|(line, _)| !line.text.contains("Loading..."))
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

// ── Existing tests (updated for two-level model) ─────────────────────────

#[test]
fn descriptions_are_complete_for_every_setting() {
    let settings = Settings::default();
    let entries = settings.setting_info();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .all(|entry| !entry.description.trim().is_empty())
    );
}

#[test]
fn every_setting_group_has_one_visible_section_unless_expert_only() {
    let settings = Settings::default();
    let catalog_groups = settings
        .setting_info()
        .into_iter()
        .map(|entry| entry.group)
        .collect::<BTreeSet<_>>();

    assert!(!catalog_groups.is_empty());

    for group in EXPERT_ONLY_GROUPS {
        assert!(
            catalog_groups.contains(*group),
            "expert-only group {group:?} is not present in the settings catalog"
        );
        let section_count = SECTIONS
            .iter()
            .filter(|section| section.groups.contains(group))
            .count();
        assert_eq!(
            section_count, 0,
            "expert-only group {group:?} should not map to a visible section"
        );
    }

    for group in catalog_groups {
        if EXPERT_ONLY_GROUPS.contains(&group) {
            continue;
        }
        let section_count = SECTIONS
            .iter()
            .filter(|section| section.groups.contains(&group))
            .count();
        assert_eq!(
            section_count, 1,
            "setting group {group:?} should map to exactly one visible section"
        );
    }
}

#[test]
fn tab_and_pane_settings_gather_under_the_layout_section() {
    // The tab, rail, panel, and pane knobs all resolve into the single
    // "Layout" Level-1 section (groups Tabs, Workspace rail, Panel, Panes),
    // so a user hunting any layout setting finds them in one discoverable
    // place instead of scattered across Rendering/Input.
    let settings = Settings::default();
    let entries = settings.setting_info();
    let section_index = SECTIONS
        .iter()
        .position(|s| s.name == "Layout")
        .expect("Layout section present");
    for key in [
        "tab_bar_placement",
        "always_show_tab_bar",
        "tab_rail_width",
        "tab_panel_strength",
        "inactive_pane_dim",
        "pane_prefix",
    ] {
        let group = entries
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("known key {key}"))
            .group;
        let resolved = SECTIONS
            .iter()
            .position(|s| s.groups.contains(&group))
            .unwrap_or_else(|| panic!("group {group:?} maps to a section"));
        assert_eq!(
            resolved, section_index,
            "{key} (group {group:?}) must land in the Layout section"
        );
    }
}

#[test]
fn programming_ligatures_is_the_first_reachable_rendering_row() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let rendering = SECTIONS
        .iter()
        .position(|section| section.name == "Rendering")
        .expect("Rendering section");
    panel.drill_into_section(rendering);

    let signature = panel.render_signature();
    assert_eq!(signature.selected, 0);
    let row = signature.entries.first().expect("Rendering settings row");
    assert_eq!(row.key, "ligatures");
    assert_eq!(row.value, "on");
}

#[test]
fn panel_navigation_is_bounded_and_scrolls() {
    let mut panel = SettingsPanel::new(&Settings::default());
    // At Level 1, Down moves section_selected.
    assert_eq!(panel.render_signature().section_selected, 0);
    let _ = panel.handle_input(OverlayInput::Down);
    assert_eq!(panel.render_signature().section_selected, 1);

    // Drill into a section; Level-2 navigation uses selected/scroll.
    panel.drill_into_section(2); // Rendering (many entries)
    // SETTINGS-COMPACT: shrink the window so the compact section overflows
    // its content region (the fixed help footer reserves the rest), keeping
    // End's scroll observable.
    panel.update_body_height(14);
    assert_eq!(panel.render_signature().selected, 0);
    let _ = panel.handle_input(OverlayInput::Down);
    assert_eq!(panel.render_signature().selected, 1);
    let _ = panel.handle_input(OverlayInput::End);
    let end = panel.render_signature();
    assert_eq!(end.selected, end.entries.len() - 1);
    assert!(end.scroll > 0);
    let _ = panel.handle_input(OverlayInput::Home);
    assert_eq!(panel.render_signature().selected, 0);
}

#[test]
fn display_rows_include_current_values_and_help_text() {
    let settings = Settings {
        font_size_px: 18.0,
        ..Settings::default()
    };
    let mut panel = SettingsPanel::new(&settings);
    // Drill into Fonts section to see font_size.
    select_key(&mut panel, "font_size");
    let lines = panel.visible_lines(70, 80);
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let font_size_line = lines
        .iter()
        .find(|line| line.text.contains("Font size:"))
        .expect("font size row present");
    assert!(
        font_size_line.text.contains("[<]  18  [>]"),
        "stepper readout shows the live value: {:?}",
        font_size_line.text
    );
    assert!(text.contains(FONT_SIZE_ENV));
}

#[test]
fn tab_bar_height_arrows_cross_the_auto_boundary() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "tab_bar_height");

    let entry = panel.selected_entry().expect("tab height selected");
    assert_eq!(entry.kind, SettingKind::Number);
    assert_eq!(entry.numeric.expect("stepper spec").step, 1.0);

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Right) else {
        panic!("right from auto applies the minimum manual height");
    };
    assert_eq!(settings.tab_bar_height, TabBarHeight::Manual(1));

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Left) else {
        panic!("left below the minimum returns to auto");
    };
    assert_eq!(settings.tab_bar_height, TabBarHeight::Auto);
}

#[test]
fn tab_bar_height_edit_replaces_auto_and_commits_typed_digits() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "tab_bar_height");
    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::Consumed
    );
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some("auto")
    );

    assert_eq!(
        panel.handle_input(OverlayInput::Char('3')),
        SettingsPanelOutcome::Consumed
    );
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some("3"),
        "the first digit replaces the auto sentinel instead of appending to it"
    );

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("Enter commits the typed height");
    };
    assert_eq!(settings.tab_bar_height, TabBarHeight::Manual(3));
    assert!(panel.editing.is_none());

    let mut panel = SettingsPanel::new(&Settings {
        tab_bar_height: TabBarHeight::Manual(3),
        ..Settings::default()
    });
    select_key(&mut panel, "tab_bar_height");
    let _ = panel.handle_input(OverlayInput::Activate);
    for ch in "auto".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("Enter commits the typed auto sentinel");
    };
    assert_eq!(settings.tab_bar_height, TabBarHeight::Auto);
}

#[test]
fn click_to_type_edit_replaces_the_prefilled_value_on_the_first_char() {
    // A numeric readout opens pre-filled with the current value shown as a
    // hint. Re-typing a full value must REPLACE the prefill, not append to
    // it: without the replace-on-first-char arm, "500" typed onto "250"
    // concatenated to "250500" and clamped to the range max (1000).
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "new_output_fade_ms");
    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::Consumed
    );
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some("250"),
        "the edit opens holding the current value as a hint"
    );

    for ch in "500".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some("500"),
        "the first digit replaces the prefill; the rest append"
    );

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("Enter commits the typed value");
    };
    assert_eq!(
        settings.new_output_fade_ms, 500.0,
        "the exact typed value applies, not the concatenated clamp"
    );
    assert!(panel.editing.is_none());
}

#[test]
fn click_to_type_edit_backspace_first_clears_the_prefill() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "new_output_fade_ms");
    let _ = panel.handle_input(OverlayInput::Activate);
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some("250")
    );

    // Backspace as the first edit clears the whole prefill (select-all feel),
    // not just the trailing character.
    let _ = panel.handle_input(OverlayInput::Backspace);
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some(""),
        "the first Backspace clears the prefill entirely"
    );

    for ch in "300".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("Enter commits the typed value");
    };
    assert_eq!(settings.new_output_fade_ms, 300.0);
}

#[test]
fn click_to_type_edit_appends_after_the_first_char_replaces() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "new_output_fade_ms");
    let _ = panel.handle_input(OverlayInput::Activate);

    let _ = panel.handle_input(OverlayInput::Char('4'));
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some("4"),
        "the first char replaces the prefill"
    );
    let _ = panel.handle_input(OverlayInput::Char('0'));
    let _ = panel.handle_input(OverlayInput::Char('0'));
    assert_eq!(
        panel.editing.as_ref().map(|edit| edit.buffer.as_str()),
        Some("400"),
        "subsequent chars append normally"
    );
}

#[test]
fn render_signature_changes_on_every_typed_char_while_editing() {
    // Pins Bug B: the overlay repaints only on signature change, so the
    // in-progress edit buffer MUST be part of the signature or the typed
    // echo stays frozen until Enter/Esc.
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "new_output_fade_ms");
    let _ = panel.handle_input(OverlayInput::Activate);

    let opened = panel.render_signature();
    assert_eq!(opened.editing_buffer.as_deref(), Some("250"));

    let _ = panel.handle_input(OverlayInput::Char('5'));
    let after_first = panel.render_signature();
    assert_ne!(
        opened, after_first,
        "the first keystroke changes the signature"
    );
    assert_eq!(after_first.editing_buffer.as_deref(), Some("5"));

    let _ = panel.handle_input(OverlayInput::Char('0'));
    let after_second = panel.render_signature();
    assert_ne!(
        after_first, after_second,
        "each subsequent keystroke changes the signature"
    );

    let _ = panel.handle_input(OverlayInput::Backspace);
    let after_backspace = panel.render_signature();
    assert_ne!(
        after_second, after_backspace,
        "Backspace while editing also changes the signature"
    );
}

#[test]
fn bool_toggle_applies_and_revert_clears_diff() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "synthetic_styles");

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("expected bool toggle to apply");
    };
    assert!(!settings.synthetic_styles);
    assert_eq!(panel.render_signature().changed_count, 1);

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("expected bool revert to apply");
    };
    assert!(settings.synthetic_styles);
    assert_eq!(panel.render_signature().changed_count, 0);
}

#[test]
fn themed_ui_roles_row_is_documented_and_editable() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "themed_ui_roles");
    let lines = panel.visible_lines(80, 80);
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("Themed UI roles: on"));
    assert!(text.contains(crate::settings::THEMED_UI_ROLES_ENV));
    assert!(text.contains("legacy foreground cursor"));

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("expected bool toggle to apply");
    };
    assert!(!settings.themed_ui_roles);
    assert_eq!(panel.render_signature().changed_count, 1);
}

#[test]
fn symbol_fallback_rows_are_documented_and_editable() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "symbol_fallback");
    let lines = panel.visible_lines(96, 80);
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("Symbol fallback: on"));
    assert!(text.contains(crate::settings::SYMBOL_FALLBACK_ENV));
    assert!(text.contains("plain missing-glyph path"));

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("expected bool toggle to apply");
    };
    assert!(!settings.symbol_fallback);
    assert_eq!(panel.render_signature().changed_count, 1);

    // symbol_font is a Path row → opens path picker in the new model.
    select_key(&mut panel, "symbol_font");
    let lines = panel.visible_lines(96, 80);
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Symbol font file: auto"));
    assert!(text.contains(crate::settings::SYMBOL_FONT_ENV));
    assert!(text.contains("bundled symbols face"));

    // Enter opens the path picker (new behaviour; was RowEdit).
    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::Consumed
    );
    assert!(
        panel.path_picker.is_some(),
        "path picker opened for symbol_font"
    );
    // Esc cancels without changing the value.
    assert_eq!(
        panel.handle_input(OverlayInput::Close),
        SettingsPanelOutcome::Consumed
    );
    assert!(panel.path_picker.is_none(), "picker closed on Esc");
}

#[test]
fn save_reports_changes_and_success_clears_diff() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "visual");
    let SettingsPanelOutcome::Apply(_) = panel.handle_input(OverlayInput::Right) else {
        panic!("expected enum cycle to apply");
    };

    let SettingsPanelOutcome::Save(changes) = panel.handle_input(OverlayInput::Save) else {
        panic!("expected save request");
    };
    assert_eq!(changes.len(), 1);
    panel.save_succeeded(changes.len());
    let signature = panel.render_signature();
    assert_eq!(signature.changed_count, 0);
    assert!(
        signature
            .message
            .as_deref()
            .is_some_and(|message| message.contains("Saved 1"))
    );
}

#[test]
fn enum_cycle_applies_next_value() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "visual");

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Right) else {
        panic!("expected enum cycle to apply");
    };
    assert_eq!(settings.visual.as_str(), "off");
    assert_eq!(panel.render_signature().changed_count, 1);
}

#[test]
fn theme_enter_opens_theme_picker_in_two_level_model() {
    // In the two-level model, Enter on the theme row opens the theme picker,
    // not a text editor (D-S2L-3 decision).
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "theme");

    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::OpenThemePicker
    );
    // No editing started.
    assert_eq!(panel.render_signature().editing_key, None);
}

#[test]
fn theme_row_left_right_cycle_theme_without_opening_picker() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "theme");

    assert!(matches!(
        panel.handle_input(OverlayInput::Right),
        SettingsPanelOutcome::Apply(_)
    ));
    assert!(matches!(
        panel.handle_input(OverlayInput::Left),
        SettingsPanelOutcome::Apply(_)
    ));
}

#[test]
fn theme_row_b_opens_builder() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "theme");

    assert_eq!(
        panel.handle_input(OverlayInput::Char('b')),
        SettingsPanelOutcome::OpenThemeBuilder
    );
}

#[test]
fn themes_section_has_open_theme_builder_action_entry() {
    // v0.3.1 discoverability: the Themes Level-2 list ends with a selectable
    // "Open Theme Builder" action row that emits OpenThemeBuilder on Enter —
    // no `b` press, no row edit (the Theme Builder was hard to discover).
    let mut panel = SettingsPanel::new(&Settings::default());
    let themes = SECTIONS
        .iter()
        .position(|s| s.name == "Themes")
        .expect("Themes section");
    panel.drill_into_section(themes);

    // The action entry is present and last in the list.
    let action_pos = panel
        .entries
        .iter()
        .position(|e| e.key == THEME_BUILDER_ACTION_KEY)
        .expect("action entry present in Themes");
    assert_eq!(
        action_pos,
        panel.entries.len() - 1,
        "the action row sits at the end of the Themes entries"
    );
    assert_eq!(panel.entries[action_pos].name, "Open Theme Builder");

    // Activating it opens the builder directly.
    panel.set_selection(action_pos);
    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::OpenThemeBuilder
    );
}

#[test]
fn theme_builder_action_survives_live_value_sync() {
    // A live settings echo (apply_settings) must not drop the synthetic
    // action row — it has no real value, so a naive value-sync would force a
    // group-filter rebuild that loses it.
    let mut panel = SettingsPanel::new(&Settings::default());
    let themes = SECTIONS
        .iter()
        .position(|s| s.name == "Themes")
        .expect("Themes section");
    panel.drill_into_section(themes);
    assert!(
        panel
            .entries
            .iter()
            .any(|e| e.key == THEME_BUILDER_ACTION_KEY)
    );

    panel.apply_settings(&Settings::default());
    assert!(
        panel
            .entries
            .iter()
            .any(|e| e.key == THEME_BUILDER_ACTION_KEY),
        "the action row survives a live value-sync"
    );
}

#[test]
fn font_family_enter_opens_font_picker() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_family");

    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::OpenFontPicker
    );
}

#[test]
fn font_family_left_right_are_no_op_for_picker() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_family");

    assert_eq!(
        panel.handle_input(OverlayInput::Right),
        SettingsPanelOutcome::Consumed
    );
    assert_eq!(
        panel.handle_input(OverlayInput::Left),
        SettingsPanelOutcome::Consumed
    );
}

#[test]
fn number_entry_uses_parser_clamp() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_size");
    let _ = panel.handle_input(OverlayInput::Activate);
    clear_edit_buffer(&mut panel);
    for ch in "200".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate) else {
        panic!("expected number edit to apply");
    };
    assert_eq!(settings.font_size_px, crate::settings::MAX_FONT_SIZE_PX);
}

#[test]
fn number_step_is_clamped_and_does_not_reframe_scroll() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_size");
    panel.update_body_height(18);

    for _ in 0..200 {
        let _ = panel.handle_input(OverlayInput::Right);
    }
    let max = panel
        .entries
        .iter()
        .find(|entry| entry.key == "font_size")
        .expect("font_size entry")
        .value
        .parse::<f32>()
        .unwrap_or(f32::NAN);
    assert_eq!(max, crate::settings::MAX_FONT_SIZE_PX);

    let before_scroll = panel.render_signature().scroll;
    let _ = panel.handle_input(OverlayInput::Right);
    assert_eq!(
        panel.render_signature().scroll,
        before_scroll,
        "scroll should not move when a number is clamped at max"
    );

    for _ in 0..220 {
        let _ = panel.handle_input(OverlayInput::Left);
    }
    let min = panel
        .entries
        .iter()
        .find(|entry| entry.key == "font_size")
        .expect("font_size entry")
        .value
        .parse::<f32>()
        .unwrap_or(f32::NAN);
    assert_eq!(min, crate::settings::MIN_FONT_SIZE_PX);

    let before_scroll = panel.render_signature().scroll;
    let _ = panel.handle_input(OverlayInput::Left);
    assert_eq!(
        panel.render_signature().scroll,
        before_scroll,
        "scroll should stay stable at min clamp"
    );
}

#[test]
fn path_entry_opens_path_picker_and_cancel_is_clean() {
    // Path rows open the inline path picker in the two-level model.
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font");
    // Enter opens picker.
    let outcome = panel.handle_input(OverlayInput::Activate);
    assert_eq!(outcome, SettingsPanelOutcome::Consumed);
    assert!(panel.path_picker.is_some(), "path picker opened");
    assert_eq!(
        panel.render_signature().editing_key,
        None,
        "not in text edit"
    );
    // Esc cancels without a value change.
    let _ = panel.handle_input(OverlayInput::Close);
    assert!(panel.path_picker.is_none(), "picker closed on Esc");
    assert_eq!(
        panel.render_signature().changed_count,
        0,
        "no change after cancel"
    );
}

#[test]
fn path_picker_state_is_render_observable() {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("odytty-path-sig-{unique}"));
    fs::create_dir(&dir).expect("create temp dir");
    fs::create_dir(dir.join("child")).expect("create child dir");
    fs::write(dir.join("wall.png"), b"not a real png").expect("write image path");

    let mut panel = SettingsPanel::new(&Settings {
        background_image: Some(dir.clone()),
        ..Settings::default()
    });
    select_key(&mut panel, "background_image");

    let before_open = panel.render_signature();
    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::Consumed
    );
    let opened = panel.render_signature();
    assert_ne!(before_open, opened, "opening picker must repaint overlay");
    assert!(
        opened.path_picker.is_some(),
        "picker state participates in render signature"
    );

    poll_path_picker(&mut panel);
    let loaded = panel.render_signature();
    assert!(
        loaded.path_picker.as_ref().is_some_and(|sig| !sig.loading),
        "loaded picker state is observable"
    );

    let _ = panel.handle_input(OverlayInput::Down);
    assert_ne!(
        loaded,
        panel.render_signature(),
        "picker selection changes must repaint overlay"
    );

    fs::remove_dir_all(dir).expect("remove temp dir");
}

#[test]
fn path_picker_pointer_click_activates_picker_entry() {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("odytty-path-click-{unique}"));
    fs::create_dir(&dir).expect("create temp dir");
    let image_path = dir.join("wall.png");
    fs::write(&image_path, b"not a real png").expect("write image path");

    let mut panel = SettingsPanel::new(&Settings {
        background_image: Some(dir.clone()),
        ..Settings::default()
    });
    select_key(&mut panel, "background_image");
    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::Consumed
    );
    assert!(panel.path_picker.is_some(), "path picker opened");
    poll_path_picker(&mut panel);

    let rows = panel.build_visible_rows(80, 20);
    let image_row = rows
        .iter()
        .enumerate()
        .find_map(|(row, (line, hit))| {
            (line.text.contains("wall.png") && hit.entry_index.is_some()).then_some(row)
        })
        .expect("image file row visible");

    let SettingsPanelOutcome::Apply(settings) = panel.handle_pointer_press(
        80,
        20,
        image_row,
        0,
        crate::native::overlay::PointerButton::Left,
        None,
    ) else {
        panic!("path-picker click should commit the clicked path");
    };
    assert_eq!(
        settings.background_image.as_deref(),
        Some(image_path.as_path())
    );
    assert!(panel.path_picker.is_none(), "picker closes after selection");

    fs::remove_dir_all(dir).expect("remove temp dir");
}

#[test]
fn committing_background_image_also_enables_image_treatment() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "background_image");

    let SettingsPanelOutcome::Apply(settings) =
        panel.commit_value("background_image", "/tmp/wall.jpg")
    else {
        panic!("background image commit should apply");
    };

    assert_eq!(
        settings.background_treatment,
        crate::settings::BackgroundTreatment::Image
    );
    assert_eq!(
        settings.background_image.as_deref(),
        Some(std::path::Path::new("/tmp/wall.jpg"))
    );
    assert!(
        (settings.cell_bg_opacity - 0.85).abs() < 1e-3,
        "new wallpapers get a visible default"
    );
    let treatment = panel
        .render_signature()
        .entries
        .into_iter()
        .find(|entry| entry.key == "background_treatment")
        .expect("background treatment entry present");
    assert_eq!(treatment.value, "image");
}

#[test]
fn background_scrim_auto_steps_to_numeric_override() {
    // Start from the `auto` (None) scrim explicitly — the shipped default is
    // now a fixed 0.5, so this sets auto to exercise the auto→numeric step.
    let settings = Settings {
        background_image_scrim: None,
        ..Settings::default()
    };
    let mut panel = SettingsPanel::new(&settings);
    select_key(&mut panel, "background_image_scrim");

    let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Right) else {
        panic!("scrim step should apply");
    };

    assert_eq!(settings.background_image_scrim, Some(0.05));
    assert_eq!(
        panel.selected_entry().expect("selected entry").value,
        "0.05"
    );
}

#[test]
fn font_family_failure_surfaces_clear_overlay_message() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_family");
    // font_family is now an Enum-like row that opens the font picker (not
    // RowEdit). Activating it emits OpenFontPicker.
    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::OpenFontPicker
    );
    // No message about a failed family name — the picker is the UX.
    // Verify the changed_count is still 0.
    assert_eq!(panel.render_signature().changed_count, 0);
}

#[test]
fn invalid_edit_is_rejected_in_panel() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_size");
    let _ = panel.handle_input(OverlayInput::Activate);
    clear_edit_buffer(&mut panel);
    for ch in "nope".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }

    assert_eq!(
        panel.handle_input(OverlayInput::Activate),
        SettingsPanelOutcome::Consumed
    );
    let signature = panel.render_signature();
    assert_eq!(signature.changed_count, 0);
    assert!(
        signature
            .message
            .as_deref()
            .is_some_and(|message| message.contains("valid pixel size"))
    );
}

#[test]
fn slash_enters_search_and_filters_to_matches() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let total = panel.render_signature().entries.len();
    // `/` at Level 1 enters search.
    assert!(panel.handle_input(OverlayInput::Char('/')) == SettingsPanelOutcome::Consumed);
    assert!(panel.is_searching());
    for ch in "cursor".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    let sig = panel.render_signature();
    assert!(sig.search_active);
    assert_eq!(sig.query, "cursor");
    assert!(!sig.entries.is_empty() && sig.entries.len() < total);
    assert!(
        sig.entries
            .iter()
            .all(|entry| entry.key.contains("cursor") || entry.key == "cursor_blink")
            || sig.entries.iter().any(|entry| entry.key.contains("cursor"))
    );
    let text = panel
        .visible_lines(80, 80)
        .iter()
        .map(|line| line.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Search: cursor"));
}

#[test]
fn search_matches_against_description_text() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "legacy".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    let sig = panel.render_signature();
    assert!(
        !sig.entries.is_empty(),
        "a description-only match is surfaced"
    );
}

#[test]
fn two_step_escape_clears_then_exits_search() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let total = panel.render_signature().entries.len();
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "font".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    assert!(!panel.render_signature().query.is_empty());
    // First Esc clears query, stays in search.
    let _ = panel.handle_input(OverlayInput::Close);
    let sig = panel.render_signature();
    assert!(sig.search_active);
    assert!(sig.query.is_empty());
    assert_eq!(sig.entries.len(), total);
    // Second Esc exits search entirely.
    let _ = panel.handle_input(OverlayInput::Close);
    let sig = panel.render_signature();
    assert!(!sig.search_active);
    assert_eq!(sig.entries.len(), total);
}

#[test]
fn backspace_trims_query_and_refilters() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "cursor".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    let narrowed = panel.render_signature().entries.len();
    let _ = panel.handle_input(OverlayInput::Backspace);
    let sig = panel.render_signature();
    assert_eq!(sig.query, "curso");
    assert!(sig.entries.len() >= narrowed);
}

#[test]
fn no_match_query_shows_notice_and_keeps_overlay() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "zzzznosuchsetting".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    let sig = panel.render_signature();
    assert!(sig.search_active);
    assert!(sig.entries.is_empty());
    let text = panel
        .visible_lines(80, 80)
        .iter()
        .map(|line| line.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("No settings match"));
}

#[test]
fn selection_clamps_after_filter_narrows_list() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let _ = panel.handle_input(OverlayInput::End);
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "theme".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    let sig = panel.render_signature();
    assert!(sig.selected < sig.entries.len().max(1));
}

#[test]
fn empty_query_signature_matches_unsearched_panel() {
    let baseline = SettingsPanel::new(&Settings::default())
        .render_signature()
        .entries;
    let mut panel = SettingsPanel::new(&Settings::default());
    let _ = panel.handle_input(OverlayInput::Char('/'));
    let entries = panel.render_signature().entries;
    assert_eq!(entries, baseline);
}

#[test]
fn editing_a_filtered_row_exits_search_cleanly() {
    // In the new model, Enter in search drills into the entry's section
    // instead of starting a text edit. For non-bool/enum entries, this
    // moves to Level 2 and selects the entry.
    let mut panel = SettingsPanel::new(&Settings::default());
    let total = panel.render_signature().entries.len();
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "bloom intensity".chars() {
        if ch == ' ' {
            break; // avoid space activating
        }
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    // Move to a numeric row and activate → drills into its section.
    select_key(&mut panel, "font_size");
    let _ = panel.handle_input(OverlayInput::Activate);
    // After drilling in, search should be exited.
    let sig = panel.render_signature();
    assert!(!sig.search_active, "drill exits search");
    assert!(
        sig.entries.len() < total,
        "section-filtered roster is shorter than full list"
    );
}

#[test]
fn refresh_clears_active_search() {
    let mut panel = SettingsPanel::new(&Settings::default());
    let total = panel.render_signature().entries.len();
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "cursor".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    assert!(panel.is_searching());
    panel.refresh(&Settings::default());
    let sig = panel.render_signature();
    assert!(!sig.search_active);
    assert!(sig.query.is_empty());
    assert_eq!(sig.entries.len(), total);
}

#[test]
fn arrowing_to_last_entry_keeps_it_visible() {
    let mut panel = SettingsPanel::new(&Settings::default());
    // Drill into Rendering (many entries) for Level-2 behavior.
    panel.drill_into_section(2);
    let body_height = 24;
    panel.update_body_height(body_height);
    let _ = panel.handle_input(OverlayInput::End);
    let sig = panel.render_signature();
    let last = sig.entries.len() - 1;
    assert_eq!(sig.selected, last, "End navigates to the last entry");

    let body_width = 80;
    let lines = panel.visible_lines(body_width, body_height);
    let selected_key = panel.entries[panel.selected].key;
    let hit_map = panel.visible_hit_map(body_width, body_height);
    assert_eq!(lines.len(), hit_map.len());
    let visible_value = hit_map.iter().enumerate().any(|(row_i, hit)| {
        use crate::native::settings_panel::pointer::RowZone;
        hit.entry_index == Some(last)
            && matches!(hit.zone, RowZone::Value { .. } | RowZone::Stepper { .. })
            && lines[row_i].focused
    });
    assert!(
        visible_value,
        "selected entry '{selected_key}' value/stepper row must be in the rendered window \
         (scroll={}, body_height={body_height})",
        sig.scroll,
    );
}

#[test]
fn setting_value_rows_are_bold_and_headers_are_not() {
    let mut panel = SettingsPanel::new(&Settings::default());
    // Drill into Rendering to get a mix of header + value rows.
    panel.drill_into_section(2);
    let lines = panel.visible_lines(80, 40);
    assert!(
        !lines[0].bold,
        "first line (group header) must not be bold: {:?}",
        lines[0].text
    );
    let has_bold = lines.iter().any(|line| line.bold);
    assert!(has_bold, "no bold rows found in settings panel lines");
    // Compact rows carry no inline help; the focused row's help renders in
    // the fixed footer at the panel bottom and is never bold (only the
    // primary value rows are). The last non-empty body line is a footer
    // help line.
    let footer_help = lines
        .iter()
        .rev()
        .find(|line| !line.text.trim().is_empty())
        .expect("footer help line present");
    assert!(!footer_help.bold, "footer help lines must not be bold");
}

// ── Two-level model trap tests ────────────────────────────────────────────

/// T-level-hitmap: Level-1 section rows use `SectionRow` zone; Level-2
/// setting rows use `Value`/`Stepper`. Hit-map switches correctly with level.
#[test]
fn level_hitmap_switches_on_level_change() {
    use crate::native::settings_panel::pointer::RowZone;
    let panel = SettingsPanel::new(&Settings::default());
    // Level 1: expect SectionRow zones.
    let hits = panel.visible_hit_map(80, 20);
    assert!(
        hits.iter().any(|h| h.zone == RowZone::SectionRow),
        "Level 1 must emit SectionRow zones"
    );
    assert!(
        !hits
            .iter()
            .any(|h| matches!(h.zone, RowZone::Value { .. } | RowZone::Stepper { .. })),
        "Level 1 must not emit Value/Stepper zones"
    );

    // Level 2: expect Value/Stepper zones, no SectionRow.
    let mut panel2 = SettingsPanel::new(&Settings::default());
    panel2.drill_into_section(0); // Themes
    let hits2 = panel2.visible_hit_map(80, 20);
    assert!(
        !hits2.iter().any(|h| h.zone == RowZone::SectionRow),
        "Level 2 must not emit SectionRow zones"
    );
    assert!(
        hits2
            .iter()
            .any(|h| matches!(h.zone, RowZone::Value { .. } | RowZone::Stepper { .. })),
        "Level 2 must emit Value/Stepper zones"
    );
}

/// T-scroll-per-level: Level-1 section_scroll and Level-2 scroll are
/// independent; entering Level 2 starts at top; returning to Level 1
/// restores section_scroll.
#[test]
fn scroll_is_independent_per_level() {
    let mut panel = SettingsPanel::new(&Settings::default());
    // Move section_selected so section_scroll might change (navigate down).
    for _ in 0..5 {
        let _ = panel.handle_input(OverlayInput::Down);
    }
    let l1_selected = panel.render_signature().section_selected;

    // Drill into Rendering.
    panel.drill_into_section(2);
    assert_eq!(panel.render_signature().scroll, 0, "Level 2 starts at top");

    // Scroll down at Level 2.
    panel.scroll_lines(3);
    assert_eq!(
        panel.render_signature().scroll,
        3,
        "Level 2 scroll advanced"
    );

    // Back to Level 1 (Esc).
    let _ = panel.handle_input(OverlayInput::Close);
    let sig = panel.render_signature();
    assert_eq!(
        sig.level,
        SettingsLevel::SectionList,
        "Esc at Level 2 returns to Level 1"
    );
    assert_eq!(
        sig.section_selected, l1_selected,
        "Level 1 selection is restored"
    );
}

#[test]
fn section_deep_link_keeps_the_level_one_list_at_the_top() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.update_body_height(40);
    panel.update_body_width(80);

    panel.open_section("Layout");
    assert_eq!(
        panel.active_section_name_for_test(),
        Some("Layout"),
        "deep link still enters the requested section"
    );
    assert_eq!(
        panel.section_selected, 3,
        "back navigation preserves the deep-link target as the selection"
    );
    assert_eq!(
        panel.section_scroll, 0,
        "deep linking must not persist a scrolled Level-1 list"
    );

    let _ = panel.handle_input(OverlayInput::Close);
    let listing = body_text(&panel);
    assert!(
        listing.contains("Themes"),
        "a subsequent generic section list starts at the top: {listing}"
    );
    assert!(
        listing.contains("Layout"),
        "the selected target remains visible in the full section list: {listing}"
    );
}

/// T-editing-clears-on-level-change: Esc at Level 2 while editing clears
/// editing before returning to Level 1.
#[test]
fn editing_is_cleared_on_level_change() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_size");
    let _ = panel.handle_input(OverlayInput::Activate); // opens RowEdit
    assert!(panel.render_signature().editing_key.is_some(), "edit open");
    // Esc cancels the edit (stays at Level 2).
    let _ = panel.handle_input(OverlayInput::Close);
    assert!(
        panel.render_signature().editing_key.is_none(),
        "edit cleared after first Esc"
    );
    // Still at Level 2.
    assert!(
        matches!(
            panel.render_signature().level,
            SettingsLevel::SectionDetail { .. }
        ),
        "still at Level 2 after edit cancel"
    );
    // Second Esc returns to Level 1.
    let _ = panel.handle_input(OverlayInput::Close);
    assert_eq!(
        panel.render_signature().level,
        SettingsLevel::SectionList,
        "second Esc returns to Level 1"
    );
    // Editing must be None at Level 1 too.
    assert!(panel.render_signature().editing_key.is_none());
}

/// T-changed-count-survives: pending edits survive drill-in and back.
#[test]
fn changed_count_survives_level_transitions() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "synthetic_styles");
    let _ = panel.handle_input(OverlayInput::Activate); // toggle bool
    assert_eq!(panel.render_signature().changed_count, 1, "1 edit recorded");

    // Back to Level 1.
    let _ = panel.handle_input(OverlayInput::Close);
    assert_eq!(
        panel.render_signature().changed_count,
        1,
        "edit survives return to Level 1"
    );

    // Drill into another section.
    panel.drill_into_section(4); // Cursor
    assert_eq!(
        panel.render_signature().changed_count,
        1,
        "edit survives drill into another section"
    );

    // Back to Level 1 again.
    let _ = panel.handle_input(OverlayInput::Close);
    assert_eq!(
        panel.render_signature().changed_count,
        1,
        "edit survives multiple level transitions"
    );
}

/// T-two-substates: path_picker and pending_close_prompt are mutually
/// exclusive; activating one clears the other.
#[test]
fn two_substates_are_mutually_exclusive() {
    let mut panel = SettingsPanel::new(&Settings::default());
    // Open the dirty-close prompt.
    select_key(&mut panel, "synthetic_styles");
    let _ = panel.handle_input(OverlayInput::Activate); // make it dirty
    let _ = panel.handle_input(OverlayInput::Close); // Esc at Level 2 → Level 1
    let _ = panel.handle_input(OverlayInput::Close); // Esc at Level 1 dirty → prompt
    assert!(
        panel.render_signature().pending_close_prompt,
        "dirty prompt opened"
    );
    // While the prompt is showing, path_picker must not be active.
    assert!(
        panel.path_picker.is_none(),
        "no path_picker while prompt is showing"
    );

    // Cancel the prompt.
    let _ = panel.handle_input(OverlayInput::Char('c'));
    assert!(
        !panel.render_signature().pending_close_prompt,
        "prompt cancelled"
    );

    // Open a path picker.
    select_key(&mut panel, "font");
    let _ = panel.handle_input(OverlayInput::Activate);
    assert!(panel.path_picker.is_some(), "path picker opened");
    // pending_close_prompt must not be active.
    assert!(
        !panel.render_signature().pending_close_prompt,
        "no dirty prompt while path picker is open"
    );
}

/// T-search-vs-level: `/` is inert at Level 2; it only opens search at Level 1.
#[test]
fn slash_is_inert_at_level_two() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.drill_into_section(2); // Rendering
    // `/` at Level 2 must not enter search mode.
    let _ = panel.handle_input(OverlayInput::Char('/'));
    assert!(
        !panel.render_signature().search_active,
        "search must not activate at Level 2"
    );
}

/// T-identity: fresh panel + no edits → Esc emits Close (not consumed);
/// Ctrl+S with no changes shows a "no unsaved" message.
#[test]
fn identity_esc_closes_and_save_nops_when_clean() {
    let mut panel = SettingsPanel::new(&Settings::default());
    // Level 1, clean → Esc should return Close.
    assert_eq!(
        panel.handle_input(OverlayInput::Close),
        SettingsPanelOutcome::Close,
        "Esc at Level 1 clean must emit Close"
    );
    // Ctrl+S with no changes.
    let outcome = panel.handle_input(OverlayInput::Save);
    assert_eq!(
        outcome,
        SettingsPanelOutcome::Consumed,
        "Ctrl+S with no changes must be Consumed"
    );
    let msg = panel.render_signature().message;
    assert!(
        msg.as_deref().is_some_and(|m| m.contains("No unsaved")),
        "no-op save shows a message"
    );
}

/// Dirty-close prompt: Esc at Level 1 with pending edits opens the prompt;
/// S saves-and-closes; D discards-and-closes; C cancels.
#[test]
fn dirty_close_prompt_flow() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "synthetic_styles");
    let _ = panel.handle_input(OverlayInput::Activate); // make dirty
    let _ = panel.handle_input(OverlayInput::Close); // Esc at Level 2 → Level 1
    assert_eq!(panel.render_signature().changed_count, 1);

    // Esc at Level 1 with dirty edits → shows the prompt.
    let outcome = panel.handle_input(OverlayInput::Close);
    assert_eq!(outcome, SettingsPanelOutcome::Consumed);
    assert!(
        panel.render_signature().pending_close_prompt,
        "prompt is showing"
    );

    // C (cancel) clears the prompt and returns to settings.
    let _ = panel.handle_input(OverlayInput::Char('c'));
    assert!(
        !panel.render_signature().pending_close_prompt,
        "prompt dismissed"
    );
    assert_eq!(
        panel.render_signature().changed_count,
        1,
        "edits still present"
    );

    // Re-show the prompt.
    let _ = panel.handle_input(OverlayInput::Close); // Level 1 dirty → prompt
    // D discards and closes.
    let outcome = panel.handle_input(OverlayInput::Char('d'));
    assert_eq!(outcome, SettingsPanelOutcome::DiscardAndClose);
    assert!(!panel.render_signature().pending_close_prompt);

    // Re-show the prompt with a fresh edit. Use a different setting to
    // avoid the double-toggle cancellation (the previous edit is still in
    // the edits field since DiscardAndClose doesn't reset the panel edits —
    // that's the overlay/App layer's job). Use `visual` which starts at
    // "off" and hasn't been toggled yet, giving a net 1 change.
    // First reset the edits to a clean state.
    panel.refresh(&Settings::default());
    select_key(&mut panel, "visual");
    let _ = panel.handle_input(OverlayInput::Right); // cycle visual → "ambient"
    let _ = panel.handle_input(OverlayInput::Close); // Level 2 → Level 1
    let _ = panel.handle_input(OverlayInput::Close); // Level 1 dirty → prompt
    assert!(
        panel.render_signature().pending_close_prompt,
        "prompt appeared again"
    );

    // S saves-and-closes.
    let outcome = panel.handle_input(OverlayInput::Char('s'));
    let SettingsPanelOutcome::SaveAndClose(changes) = outcome else {
        panic!("expected SaveAndClose from S key in prompt");
    };
    assert_eq!(changes.len(), 1);
    assert!(!panel.render_signature().pending_close_prompt);
}

/// Level-1 Enter drills into the focused section; Level-2 Esc backs out.
#[test]
fn level1_enter_drills_and_level2_esc_backs_out() {
    let mut panel = SettingsPanel::new(&Settings::default());
    assert_eq!(
        panel.render_signature().level,
        SettingsLevel::SectionList,
        "starts at Level 1"
    );

    // Down to Fonts (index 1), then Enter.
    let _ = panel.handle_input(OverlayInput::Down); // section_selected = 1 (Fonts)
    let _ = panel.handle_input(OverlayInput::Activate);
    assert_eq!(
        panel.render_signature().level,
        SettingsLevel::SectionDetail { section_index: 1 },
        "Enter at Level 1 drills into Fonts"
    );
    // Entries should be the Fonts group only.
    assert!(
        panel
            .render_signature()
            .entries
            .iter()
            .all(|e| e.key == "font"
                || e.key == "font_family"
                || e.key == "font_size"
                || e.key == "font_weight"
                || e.key == "line_height"
                || e.key == "synthetic_styles"
                || e.key == "symbol_fallback"
                || e.key == "symbol_font"
                || e.key == "symbol_map"),
        "Level 2 Fonts shows Font-group entries"
    );

    // Esc at Level 2 → Level 1.
    let outcome = panel.handle_input(OverlayInput::Close);
    assert_eq!(
        outcome,
        SettingsPanelOutcome::Consumed,
        "Esc at Level 2 is Consumed"
    );
    assert_eq!(
        panel.render_signature().level,
        SettingsLevel::SectionList,
        "Esc at Level 2 returns to Level 1"
    );
}

// ── SETTINGS-PANEL-STATE-FIX regression tests ────────────────────────────

/// Entry indices whose Value/Stepper row is currently visible at the active
/// scroll, read from the shared row walker (the same source the pointer
/// hit-map and `selected_in_window` consume).
fn visible_entry_indices(panel: &SettingsPanel, w: usize, h: usize) -> Vec<usize> {
    use crate::native::settings_panel::pointer::RowZone;
    panel
        .build_settings_rows(w, h)
        .into_iter()
        .filter_map(|(_, hit)| match hit.zone {
            RowZone::Value { .. } | RowZone::Stepper { .. } => hit.entry_index,
            _ => None,
        })
        .collect()
}

/// Bug A: selecting (e.g. pressing) a row that is already on-screen must NOT
/// recenter the viewport. The old `visible_slack` reframe yanked the view on
/// any selection below the top third — this is what jumped a slider to the
/// bottom the instant you adjusted it.
#[test]
fn selecting_a_visible_row_does_not_recenter_scroll() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.drill_into_section(2); // Rendering (many entries)
    panel.update_body_width(90);
    panel.update_body_height(28);
    let _ = panel.visible_lines(90, 28);
    let vis = visible_entry_indices(&panel, 90, 28);
    let last_visible = *vis.iter().max().expect("some rows visible at top");
    assert!(
        last_visible >= 1,
        "need a non-top visible row to be meaningful"
    );
    // Pointer press path is `set_selection`; start from the top of scroll.
    panel.scroll = 0;
    panel.set_selection(last_visible);
    assert_eq!(
        panel.scroll, 0,
        "an already-visible row must not move the viewport"
    );
    assert!(
        panel.selected_in_window(28),
        "the selected row stays visible"
    );
}

/// Bug A [follow-lag] trap: arrowing the selection BELOW the visible window
/// must still scroll — minimally — to reveal it (no VIEWPORT-FOLLOW-LAG
/// regression), while arrowing within the window does not scroll.
#[test]
fn offscreen_selection_scrolls_minimally_within_window_does_not() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.drill_into_section(2); // Rendering
    panel.update_body_width(90);
    // SETTINGS-COMPACT: a short window whose content region (after the fixed
    // help footer reserves its rows) is smaller than the section's compact
    // row count, so the last entry is genuinely off-screen at the top.
    panel.update_body_height(14);
    let _ = panel.visible_lines(90, 14);

    // Arrow within the visible window: no scroll.
    let vis = visible_entry_indices(&panel, 90, 14);
    let last_visible = *vis.iter().max().expect("rows visible");
    panel.set_selection(last_visible);
    assert_eq!(panel.scroll, 0, "still within window: no scroll");

    // Jump to the end: an off-screen selection must be revealed.
    panel.set_selection(panel.entries.len() - 1);
    assert!(
        panel.scroll > 0,
        "off-screen selection scrolls to reveal it"
    );
    assert!(
        panel.scroll <= panel.selected,
        "scroll never overshoots past selection"
    );
    assert!(panel.selected_in_window(14), "End selection is revealed");
}

/// Bug A end-clamp trap: keyboard steps at the numeric min/max saturate the
/// value exactly and never jump the scroll.
#[test]
fn arrow_steps_saturate_at_min_and_max_without_scroll_jump() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "font_size");
    panel.update_body_width(90);
    panel.update_body_height(28);
    let _ = panel.visible_lines(90, 28);
    let spec = panel.selected_entry().unwrap().numeric.unwrap();

    // Drive to the minimum.
    for _ in 0..200 {
        let _ = panel.step_or_cycle_selected(-1);
    }
    let at_min = panel
        .selected_entry()
        .unwrap()
        .value
        .parse::<f32>()
        .unwrap();
    assert!((at_min - spec.min).abs() < 1e-3, "value saturates at min");
    let scroll_at_min = panel.scroll;
    let _ = panel.step_or_cycle_selected(-1); // already at the floor
    assert_eq!(panel.scroll, scroll_at_min, "no scroll jump at min");
    let still_min = panel
        .selected_entry()
        .unwrap()
        .value
        .parse::<f32>()
        .unwrap();
    assert!((still_min - spec.min).abs() < 1e-3, "value held at min");

    // Drive to the maximum.
    for _ in 0..400 {
        let _ = panel.step_or_cycle_selected(1);
    }
    let at_max = panel
        .selected_entry()
        .unwrap()
        .value
        .parse::<f32>()
        .unwrap();
    assert!((at_max - spec.max).abs() < 1e-3, "value saturates at max");
    let scroll_at_max = panel.scroll;
    let _ = panel.step_or_cycle_selected(1); // already at the ceiling
    assert_eq!(panel.scroll, scroll_at_max, "no scroll jump at max");
    let still_max = panel
        .selected_entry()
        .unwrap()
        .value
        .parse::<f32>()
        .unwrap();
    assert!((still_max - spec.max).abs() < 1e-3, "value held at max");
}

/// Bug B + Bug C: a live apply (the OverlayEdit round-trip seam) while
/// drilled into a section must PRESERVE the section filter (Bug B) and the
/// current level (Bug C), and must not clobber pending dirty edits. This is
/// the bloom-threshold "multi-line slider row" shape.
#[test]
fn live_apply_preserves_section_filter_level_and_dirty_state() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "bloom_threshold");
    let section_keys: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
    assert!(section_keys.contains(&"bloom_threshold"));
    assert!(
        section_keys.len() < panel.all_entries.len(),
        "section view is a strict subset of all settings"
    );
    let level_before = panel.render_signature().level;
    assert!(matches!(level_before, SettingsLevel::SectionDetail { .. }));

    // Commit a value change in this section (creates a pending dirty edit).
    let entry = panel.selected_entry().unwrap().clone();
    let spec = entry.numeric.unwrap();
    let cur = entry.value.parse::<f32>().unwrap();
    let target = if (cur - spec.min).abs() > spec.step {
        spec.min
    } else {
        spec.max
    };
    let outcome = panel.commit_value(entry.key, &format!("{target:.3}"));
    assert!(
        matches!(outcome, SettingsPanelOutcome::Apply(_)),
        "a real value change applies"
    );
    assert_eq!(panel.render_signature().changed_count, 1, "one dirty edit");

    // Simulate the live-apply round-trip seam. The incoming `settings` is the
    // unedited baseline (as a Save's `Settings::from_env` re-read can differ
    // from the in-panel edit overlay) — pre-fix this triggered the spurious
    // level-resetting refresh().
    panel.apply_settings(&Settings::default());

    // Bug B: still only this section's settings.
    let after: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
    assert_eq!(
        after, section_keys,
        "section filter preserved after live apply"
    );
    // Bug C: level unchanged.
    assert_eq!(
        panel.render_signature().level,
        level_before,
        "drilled-in level preserved after live apply"
    );
    // [dirty-preserve]: the pending edit is not silently discarded.
    assert_eq!(
        panel.render_signature().changed_count,
        1,
        "pending dirty edit preserved across live apply"
    );
}

/// Bug B [search-preserve] trap: a live apply while searching must keep the
/// search filter, not reset to the full list or drop search mode.
#[test]
fn live_apply_preserves_active_search_filter() {
    let mut panel = SettingsPanel::new(&Settings::default());
    // Enter search and type a needle that matches a known subset.
    let _ = panel.handle_input(OverlayInput::Char('/'));
    for ch in "bloom".chars() {
        let _ = panel.handle_input(OverlayInput::Char(ch));
    }
    assert!(panel.render_signature().search_active, "search is active");
    let filtered: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
    assert!(!filtered.is_empty() && filtered.len() < panel.all_entries.len());

    panel.apply_settings(&Settings::default());

    assert!(
        panel.render_signature().search_active,
        "search stays active after live apply"
    );
    let after: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
    assert_eq!(after, filtered, "search filter preserved after live apply");
}

#[test]
fn rebase_onto_external_then_commit_does_not_revert_theme() {
    use crate::theme::Theme;

    let base = Settings {
        theme: Theme::PLAIN,
        ..Settings::default()
    };
    let mut panel = SettingsPanel::new(&base);
    select_key(&mut panel, "font_size");

    // Snapshot nav state before the external theme application.
    let (level_before, section_before) = (panel.level, panel.section_selected);
    assert!(
        matches!(level_before, SettingsLevel::SectionDetail { .. }),
        "precondition: drilled into a section"
    );

    // External theme application reconciles into the panel.
    panel.rebase_onto_external(&Settings {
        theme: Theme::ODYSSEY_NOIR,
        ..Settings::default()
    });

    assert_eq!(panel.level, level_before, "level preserved by rebase");
    assert_eq!(
        panel.section_selected, section_before,
        "section preserved by rebase"
    );
    // font_size should still be selected (re-find by key).
    assert_eq!(
        panel.entries.get(panel.selected).map(|e| e.key),
        Some("font_size"),
        "selected key preserved by rebase"
    );

    // Commit a different setting in the panel; this used to rebuild from a
    // stale theme baseline.
    let SettingsPanelOutcome::Apply(first) = panel.handle_input(OverlayInput::Right) else {
        panic!("font_size step should apply");
    };
    let SettingsPanelOutcome::Apply(second) = panel.handle_input(OverlayInput::Right) else {
        panic!("second font_size step should apply");
    };
    // Both commit rounds must carry the new theme (the bug reverted it).
    assert_eq!(first.theme, Theme::ODYSSEY_NOIR);

    assert_eq!(
        second.theme,
        Theme::ODYSSEY_NOIR,
        "panel commit must not revert the externally-applied theme"
    );
    assert_eq!(
        panel.render_signature().changed_count,
        1,
        "dirty count is font_size only, theme is clean baseline"
    );
    // Nav still intact after the commit too.
    assert_eq!(panel.level, level_before);
}

#[test]
fn rebase_onto_external_preserves_pending_dirty_edit() {
    use crate::theme::Theme;

    let mut panel = SettingsPanel::new(&Settings {
        theme: Theme::PLAIN,
        ..Settings::default()
    });
    select_key(&mut panel, "font_size");
    let SettingsPanelOutcome::Apply(_) = panel.handle_input(OverlayInput::Right) else {
        panic!("font_size step should apply");
    };
    assert_eq!(panel.render_signature().changed_count, 1);

    panel.rebase_onto_external(&Settings {
        theme: Theme::ODYSSEY,
        ..Settings::default()
    });

    assert_eq!(panel.render_signature().changed_count, 1, "edit survived");
    assert_eq!(
        panel.edits.settings().theme,
        Theme::ODYSSEY,
        "theme adopted as clean baseline"
    );
    assert_eq!(
        panel
            .edits
            .changes()
            .iter()
            .map(|c| c.key)
            .collect::<Vec<_>>(),
        vec!["font_size"]
    );
}

// ── ABOUT: in-app About view ─────────────────────────────────────────────

/// Build an `AboutInfo` with a fixed synthetic adapter for deterministic
/// assertions (the real adapter varies by host).
fn test_about() -> AboutInfo {
    AboutInfo {
        name: "OdyTTY",
        version: "9.9.9",
        license: "GPL-3.0-only",
        git_sha: "abc1234",
        build_date: "2026-06-27",
        target: "x86_64-unknown-linux-gnu",
        rustc_version: "1.96.0 (test)",
        display_server: "Wayland",
        adapter: Some(crate::native::gpu::AdapterDiagnostics {
            name: "Test GPU".to_owned(),
            backend: "Vulkan".to_owned(),
            device_type: "DiscreteGpu".to_owned(),
            driver: "TestDriver".to_owned(),
            driver_info: "1.2.3".to_owned(),
        }),
    }
}

fn body_text(panel: &SettingsPanel) -> String {
    panel
        .build_visible_rows(80, 40)
        .into_iter()
        .map(|(line, _)| line.text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn about_row_is_the_last_section_row_and_drills_into_about() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.update_body_height(40);
    panel.update_body_width(80);

    // The Level-1 list shows an "About" row past the real sections.
    let listing = body_text(&panel);
    assert!(
        listing.contains("About"),
        "section list shows an About row: {listing}"
    );

    // Selecting the last row (End) and activating drills into About.
    panel.set_about(test_about());
    let _ = panel.handle_input(OverlayInput::End);
    assert_eq!(panel.section_selected, SECTIONS.len(), "About row focused");
    let _ = panel.handle_input(OverlayInput::Activate);
    assert!(
        matches!(panel.level, SettingsLevel::About),
        "activating the About row enters the About level"
    );
}

#[test]
fn about_view_shows_version_build_and_gpu_lines() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.update_body_height(40);
    panel.update_body_width(80);
    panel.set_about(test_about());
    panel.drill_into_section(SECTIONS.len()); // enter About

    let body = body_text(&panel);
    assert!(body.contains("OdyTTY 9.9.9"), "version line: {body}");
    assert!(body.contains("abc1234"), "commit line present");
    assert!(body.contains("Test GPU"), "gpu name present");
    assert!(body.contains("Vulkan"), "backend present");
    // The three project links and the Copy row are actionable rows.
    assert!(body.contains("Homepage"), "homepage link present");
    assert!(body.contains("Copy diagnostics"), "copy row present");
}

#[test]
fn about_diagnostics_block_has_facts_but_no_home_paths() {
    let about = test_about();
    let block = about.diagnostics_block();
    assert!(block.contains("OdyTTY 9.9.9"));
    assert!(block.contains("x86_64-unknown-linux-gnu"));
    assert!(block.contains("Test GPU"));
    assert!(block.contains("Wayland"));
    // Privacy: the copy blob must not carry filesystem/home paths.
    assert!(
        !block.contains("/home/") && !block.contains("$HOME"),
        "diagnostics block must not leak home paths: {block}"
    );
}

#[test]
fn about_copy_row_emits_copy_outcome_with_diagnostics() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.set_about(test_about());
    panel.drill_into_section(SECTIONS.len());
    // Focus the Copy row (last actionable row) and activate.
    let _ = panel.handle_input(OverlayInput::End);
    let outcome = panel.handle_input(OverlayInput::Activate);
    match outcome {
        SettingsPanelOutcome::CopyToClipboard(text) => {
            assert!(text.contains("OdyTTY 9.9.9"), "copies diagnostics: {text}");
        }
        other => panic!("expected CopyToClipboard, got {other:?}"),
    }
}

#[test]
fn about_link_row_emits_open_url_outcome() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.set_about(test_about());
    panel.drill_into_section(SECTIONS.len());
    // First actionable row is the first project link.
    let _ = panel.handle_input(OverlayInput::Home);
    let outcome = panel.handle_input(OverlayInput::Activate);
    match outcome {
        SettingsPanelOutcome::OpenUrl(url) => {
            assert_eq!(url, ABOUT_LINKS[0].url, "opens the first project link");
        }
        other => panic!("expected OpenUrl, got {other:?}"),
    }
}

#[test]
fn external_palette_status_row_syncs_from_live_follower() {
    let mut panel = SettingsPanel::new(&Settings::default());
    select_key(&mut panel, "external_palette_status");
    assert_eq!(
        panel.displayed_value_for_test("external_palette_status"),
        Some("off".to_owned())
    );
    panel.sync_external_palette_status("applied");
    assert_eq!(
        panel.displayed_value_for_test("external_palette_status"),
        Some("applied".to_owned())
    );
    panel.sync_external_palette_status("error: palette file missing: /nope");
    let value = panel
        .displayed_value_for_test("external_palette_status")
        .expect("status");
    assert!(value.starts_with("error:"));
}

#[test]
fn about_esc_returns_to_section_list() {
    let mut panel = SettingsPanel::new(&Settings::default());
    panel.set_about(test_about());
    panel.drill_into_section(SECTIONS.len());
    assert!(matches!(panel.level, SettingsLevel::About));
    let _ = panel.handle_input(OverlayInput::Close);
    assert!(
        matches!(panel.level, SettingsLevel::SectionList),
        "Esc from About returns to the section list"
    );
}

// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl SettingsPanel {
    // ── Value activation ────────────────────────────────────────────────────

    pub(super) fn activate_selected(&mut self) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        if !entry.reloadable {
            self.message = Some("Startup-only setting; edit odytty.conf and restart.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }
        // The synthetic "Open Theme Builder" action row opens the builder
        // directly (v0.3.1 discoverability) — no `b` press, no row edit.
        if entry.key == THEME_BUILDER_ACTION_KEY {
            self.message = Some("Opening theme builder.".to_owned());
            return SettingsPanelOutcome::OpenThemeBuilder;
        }
        if entry.key == PROFILE_MANAGER_ACTION_KEY {
            self.message = Some("Opening profile manager.".to_owned());
            return SettingsPanelOutcome::OpenProfileManager;
        }
        // Key-specific overrides (run before kind dispatch):
        // - theme: Enter opens the theme picker (not RowEdit) in the two-level model.
        // - font_family: Enter opens the font picker (key is String kind, not Enum).
        if entry.key == "theme" {
            self.message = Some("Opening built-in theme picker.".to_owned());
            return SettingsPanelOutcome::OpenThemePicker;
        }
        if entry.key == "font_family" {
            self.message = Some("Opening font picker.".to_owned());
            return SettingsPanelOutcome::OpenFontPicker;
        }
        match entry.kind {
            SettingKind::Bool => {
                let next = if entry.value == "on" { "off" } else { "on" };
                self.commit_value(entry.key, next)
            }
            SettingKind::Enum => self.cycle_selected(1),
            SettingKind::List if entry.key == "keybinds" => SettingsPanelOutcome::OpenKeyBindings,
            // Path rows open the inline path picker (SETTINGS-REDESIGN §8).
            SettingKind::Path => {
                let original = entry.value.clone();
                let start_dir = resolve_start_dir(&original);
                // T-two-substates: clear editing before opening the picker.
                self.editing = None;
                self.path_picker = Some(PathPickerState::new(entry.key, start_dir, original));
                SettingsPanelOutcome::Consumed
            }
            SettingKind::Number | SettingKind::String | SettingKind::List => {
                self.editing = Some(RowEdit::for_entry(&entry));
                self.message =
                    Some("Editing: type a value, Enter applies, Esc cancels.".to_owned());
                SettingsPanelOutcome::Consumed
            }
        }
    }

    pub(super) fn handle_editing_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        match input {
            OverlayInput::Close => {
                if let Some(edit) = self.editing.take() {
                    self.message = Some(format!("Cancelled edit for {}.", edit.key));
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Activate => {
                let Some(edit) = self.editing.take() else {
                    return SettingsPanelOutcome::Consumed;
                };
                let key = edit.key;
                let value = edit.buffer;
                self.commit_value(key, &value)
            }
            OverlayInput::Backspace => {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.replace_on_char {
                        edit.buffer.clear();
                        edit.replace_on_char = false;
                    } else {
                        edit.buffer.pop();
                    }
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.replace_on_char {
                        edit.buffer.clear();
                        edit.replace_on_char = false;
                    }
                    edit.buffer.push(ch);
                }
                SettingsPanelOutcome::Consumed
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    pub(super) fn step_or_cycle_selected(&mut self, direction: isize) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        if entry.key == "tab_bar_height" {
            let next = stepped_tab_bar_height(&entry.value, direction);
            return self.commit_value(entry.key, &next);
        }
        if entry.key == "background_image_scrim" {
            let parsed =
                entry
                    .value
                    .parse::<f32>()
                    .unwrap_or(if direction < 0 { 1.0 } else { 0.0 });
            let next = if let Some(spec) = entry.numeric {
                let step = spec.step * direction as f32;
                (parsed + step).clamp(spec.min, spec.max)
            } else {
                parsed
            };
            return self.commit_value(entry.key, &format!("{next:.3}"));
        }
        match entry.kind {
            SettingKind::Enum => self.cycle_selected(direction),
            SettingKind::Number => {
                let parsed = entry.value.parse::<f32>().unwrap_or(0.0);
                let next = if let Some(spec) = entry.numeric {
                    let step = spec.step * direction as f32;
                    (parsed + step).clamp(spec.min, spec.max)
                } else {
                    parsed
                };
                self.commit_value(entry.key, &format!("{:.3}", next))
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    pub(super) fn cycle_selected(&mut self, direction: isize) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        let options = edit_options(&entry);
        let Some(current) = options.iter().position(|value| *value == entry.value) else {
            self.message = Some("Type a custom value with Enter.".to_owned());
            return SettingsPanelOutcome::Consumed;
        };
        let len = options.len() as isize;
        let next = (current as isize + direction).rem_euclid(len) as usize;
        self.commit_value(entry.key, options[next])
    }

    pub(super) fn commit_value(&mut self, key: &'static str, value: &str) -> SettingsPanelOutcome {
        let before_scroll = self.scroll;
        if key == "background_image" && !value.trim().is_empty() && value.trim() != "none" {
            let _ = self.edits.apply_raw("background_treatment", "image");
            if self.edits.settings().cell_bg_opacity >= DEFAULT_CELL_BG_OPACITY - 0.001 {
                let _ = self.edits.apply_raw("cell_bg_opacity", "0.850");
            }
        }
        let commit_value;
        let value = if key == "cell_bg_opacity" {
            let visibility = value.trim().parse::<f32>().unwrap_or(0.0).clamp(0.0, 1.0);
            commit_value = format!("{:.3}", 1.0 - visibility);
            commit_value.as_str()
        } else {
            value
        };
        if key == "background_image" {
            self.update_entry_value_in_place("background_treatment");
            self.update_entry_value_in_place("cell_bg_opacity");
        }
        match self.edits.apply_raw(key, value) {
            Ok(Some(settings)) => {
                // Update only the changed row's display value in place instead
                // of rebuilding the full `setting_info()` table on every
                // repeated edit.
                self.update_entry_value_in_place(key);
                if key == "background_image" {
                    self.update_entry_value_in_place("background_treatment");
                    self.update_entry_value_in_place("cell_bg_opacity");
                }
                self.restore_scroll_after_commit(before_scroll);
                self.message = Some(format!("Applied {key}."));
                SettingsPanelOutcome::Apply(settings)
            }
            Ok(None) => {
                self.update_entry_value_in_place(key);
                if key == "background_image" {
                    self.update_entry_value_in_place("background_treatment");
                    self.update_entry_value_in_place("cell_bg_opacity");
                }
                self.restore_scroll_after_commit(before_scroll);
                self.message = Some("No setting change.".to_owned());
                SettingsPanelOutcome::Consumed
            }
            Err(error) => {
                self.message = Some(error.message);
                SettingsPanelOutcome::Consumed
            }
        }
    }

    /// Re-derive the display `value` for a single setting key from the current
    /// edit-overlay settings and patch it into `all_entries` and the filtered
    /// `entries` list in place. Falls back to a
    /// full `setting_info()` rebuild if the key is not found or the single-key
    /// derivation is unavailable, so the panel stays correct if the inventory
    /// shape ever changes. Only the `value` field can change from a live edit;
    /// group/key/name/description/kind/numeric/options/reloadable are static.
    pub(super) fn update_entry_value_in_place(&mut self, key: &'static str) {
        let Some(new_value) = self.edits.settings().display_value_for_key(key) else {
            self.all_entries = self.edits.settings().setting_info();
            self.refresh_entries_after_commit();
            return;
        };
        for entry in &mut self.all_entries {
            if entry.key == key {
                entry.value.clone_from(&new_value);
            }
        }
        for entry in &mut self.entries {
            if entry.key == key {
                entry.value = new_value.clone();
            }
        }
    }

    pub(super) fn restore_scroll_after_commit(&mut self, before_scroll: usize) {
        if self.entries.is_empty() {
            self.scroll = 0;
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.scroll = before_scroll.min(self.entries.len().saturating_sub(1));
    }

    /// Rebuild `entries` after a commit that updated `all_entries`. Preserves
    /// the section filter at Level 2 and the search filter in search mode.
    pub(super) fn refresh_entries_after_commit(&mut self) {
        if self.search_active {
            self.apply_search_filter();
            return;
        }
        if let SettingsLevel::SectionDetail { section_index } = &self.level.clone() {
            let si = *section_index;
            if SECTIONS.get(si).is_none() {
                return;
            }
            let key = self.entries.get(self.selected).map(|e| e.key);
            self.entries = self.section_entries(si);
            // Re-find the selected key in the new list (values may have changed).
            if let Some(key) = key
                && let Some(pos) = self.entries.iter().position(|e| e.key == key)
            {
                self.selected = pos;
            }
            self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        } else {
            // Level 1: restore the full list.
            self.entries = self.all_entries.clone();
        }
    }

    pub(super) fn save_changes(&mut self) -> SettingsPanelOutcome {
        let changes = self.edits.changes();
        if changes.is_empty() {
            self.message = Some("No unsaved setting changes.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }
        SettingsPanelOutcome::Save(changes)
    }
}

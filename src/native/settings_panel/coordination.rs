// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl SettingsPanel {
    pub(in crate::native) fn new(settings: &Settings) -> Self {
        let edits = SettingsEditOverlay::new(settings);
        let entries = edits.settings().setting_info();
        let mut panel = Self {
            all_entries: entries.clone(),
            entries,
            level: SettingsLevel::SectionList,
            about: None,
            section_selected: 0,
            section_scroll: 0,
            pending_close_prompt: false,
            path_picker: None,
            edits,
            environment_overrides: Vec::new(),
            selected: 0,
            scroll: 0,
            editing: None,
            message: None,
            query: String::new(),
            search_active: false,
            last_body_height: 18,
            last_body_width: 76,
        };
        panel.clamp();
        panel
    }

    pub(in crate::native) fn set_environment_overrides(&mut self, mut keys: Vec<&'static str>) {
        keys.sort_unstable();
        keys.dedup();
        self.environment_overrides = keys;
    }

    pub(super) fn environment_override_note(&self, entry: &SettingInfo) -> Option<String> {
        (!entry.env.is_empty() && self.environment_overrides.contains(&entry.env)).then(|| {
            let save = if entry.reloadable { " Save still writes the config;" } else { "" };
            format!("Environment override: {}.{save} Remove this variable for the config value to take effect on next launch.", entry.env)
        })
    }

    /// Called by the render path immediately before `visible_lines` so that
    /// keyboard navigation (`clamp`) knows the real visible window dimensions.
    pub(in crate::native) fn update_body_height(&mut self, body_height: usize) {
        if let Some(picker) = self.path_picker.as_mut() {
            picker.poll_pending();
        }
        if body_height > 0 {
            self.last_body_height = body_height;
        }
    }

    pub(in crate::native) fn update_body_width(&mut self, body_width: usize) {
        if let Some(picker) = self.path_picker.as_mut() {
            picker.poll_pending();
        }
        if body_width > 0 {
            self.last_body_width = body_width;
        }
    }

    /// Returns `true` when the selected entry's primary row appears within the
    /// rendered window (VIEWPORT-FOLLOW-LAG fix). Only meaningful at Level 2.
    pub(super) fn selected_in_window(&self, body_height: usize) -> bool {
        if body_height == 0 || self.entries.is_empty() {
            return true;
        }
        use crate::native::settings_panel::pointer::RowZone;
        self.build_settings_rows(self.last_body_width, body_height)
            .iter()
            .any(|(_, hit)| {
                hit.entry_index == Some(self.selected)
                    && matches!(hit.zone, RowZone::Value { .. } | RowZone::Stepper { .. })
            })
    }

    pub(in crate::native) fn refresh(&mut self, settings: &Settings) {
        let had_draft = self.edits.changed_count() > 0 || self.editing.is_some();
        self.rebase_onto_external(settings);
        if had_draft {
            self.message = Some(
                "Configuration reloaded; pending settings and active edits preserved.".to_owned(),
            );
        }
    }

    /// Reflect accepted panel edits without changing the clean baseline or
    /// navigation. The edit overlay already owns the applied values.
    pub(in crate::native) fn apply_settings(&mut self, _settings: &Settings) {
        // Avoid rebuilding the settings inventory during repeated live edits: the
        // OverlayEdit echo carries values the panel already committed into
        // `self.edits`, and `commit_value` already patched into
        // `all_entries`/`entries` in place. Re-derive every entry value from the
        // edit overlay in place rather than rebuilding the full `setting_info()`
        // table on every echo. A full rebuild is the fallback only if a key is
        // unknown to `display_value_for_key` (inventory shape changed).
        let needs_full_rebuild = self.sync_all_entry_values_in_place();
        if needs_full_rebuild {
            self.all_entries = self.edits.settings().setting_info();
            self.refresh_entries_after_commit();
        }
        self.clamp();
    }

    /// Patch every entry's `value` field in `all_entries` and the filtered
    /// `entries` from the current edit-overlay settings, in place. Returns
    /// `true` if any key was unknown to [`Settings::display_value_for_key`],
    /// signalling the caller should fall back to a full `setting_info()`
    /// rebuild.
    pub(super) fn sync_all_entry_values_in_place(&mut self) -> bool {
        let settings = self.edits.settings().clone();
        let mut unknown = false;
        for entry in &mut self.all_entries {
            if entry.key == "external_palette_status" {
                continue;
            }
            match settings.display_value_for_key(entry.key) {
                Some(value) => entry.value = value,
                None => unknown = true,
            }
        }
        for entry in &mut self.entries {
            // The synthetic action row carries no live value; skip it so it never
            // forces a spurious full rebuild (its value is static by design).
            if entry.key == THEME_BUILDER_ACTION_KEY || entry.key == PROFILE_MANAGER_ACTION_KEY {
                continue;
            }
            if entry.key == "external_palette_status" {
                continue;
            }
            match settings.display_value_for_key(entry.key) {
                Some(value) => entry.value = value,
                None => unknown = true,
            }
        }
        unknown
    }

    /// Patch the read-only external-palette follower status row from live App
    /// state (after apply, poll, or startup sync).
    pub(in crate::native) fn sync_external_palette_status(&mut self, display: &str) {
        for entry in &mut self.all_entries {
            if entry.key == "external_palette_status" {
                entry.value = display.to_owned();
            }
        }
        for entry in &mut self.entries {
            if entry.key == "external_palette_status" {
                entry.value = display.to_owned();
            }
        }
    }

    /// Reconcile externally-applied settings from a picker or reload into the edit
    /// overlay as the new clean baseline, while preserving pending panel edits
    /// and navigation state.
    pub(in crate::native) fn rebase_onto_external(&mut self, settings: &Settings) {
        self.edits.rebase_onto(settings);
        let selected_key = self
            .entries
            .get(self.selected)
            .map(|entry| entry.key)
            .unwrap_or("theme");
        self.all_entries = self.edits.settings().setting_info();
        self.refresh_entries_after_commit();
        if let Some(pos) = self.entries.iter().position(|e| e.key == selected_key) {
            self.selected = pos;
        }
        self.clamp();
    }

    pub(in crate::native) fn save_succeeded(&mut self, changed: usize) {
        self.edits.mark_saved();
        self.all_entries = self.edits.settings().setting_info();
        self.refresh_entries_after_commit();
        self.message = Some(format!("Saved {changed} setting change(s) to odytty.conf."));
        self.clamp();
    }

    pub(in crate::native) fn save_failed(&mut self, message: String) {
        self.message = Some(format!("Save failed: {message}"));
    }

    // NOTE: `is_editing` and `is_searching` were previously used by the
    // overlay.rs Close guard; the two-level model removed that guard (the panel
    // now handles all Close inputs internally). Kept for future callers.
    #[allow(dead_code)]
    pub(in crate::native) fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    pub(in crate::native) fn is_dragging(&self) -> bool {
        false
    }

    /// The current panel title (used by `apply_overlay` to render the title
    /// bar, which changes based on the active level and editing state).
    pub(in crate::native) fn panel_title(&self) -> String {
        match &self.level {
            SettingsLevel::SectionList => {
                if self.search_active {
                    return format!("OdyTTY Settings — Search: {}", self.query);
                }
                "OdyTTY Settings".to_owned()
            }
            SettingsLevel::SectionDetail { section_index } => {
                if let Some(edit) = &self.editing {
                    return format!(
                        "\u{270e} EDITING {} \u{2014} Enter applies \u{00b7} Esc cancels",
                        edit.key
                    );
                }
                let name = SECTIONS
                    .get(*section_index)
                    .map(|s| s.name)
                    .unwrap_or("Settings");
                format!("\u{2190} {name}  (Esc = back)")
            }
            SettingsLevel::About => "\u{2190} About  (Esc = back)".to_owned(),
        }
    }
}

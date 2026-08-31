// SPDX-License-Identifier: GPL-3.0-only
//! Live settings-edit orchestration and key-binding presentation.

use super::*;

impl SettingsEditOverlay {
    pub fn new(settings: &Settings) -> Self {
        let values = settings.to_edit_values();
        Self {
            base_values: values.clone(),
            values,
            settings: settings.clone(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn changes(&self) -> Vec<SettingEdit> {
        self.base_values
            .keys()
            .chain(self.values.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|env| self.base_values.get(env) != self.values.get(env))
            .filter_map(|env| setting_key_for_env(env).map(|key| (key, env)))
            .map(|(key, env)| SettingEdit {
                key,
                env,
                value: self.values.get(env).cloned().unwrap_or_default(),
            })
            .collect()
    }

    pub fn changed_count(&self) -> usize {
        self.changes().len()
    }

    pub fn mark_saved(&mut self) {
        self.base_values = self.values.clone();
    }

    /// Adopt externally-applied settings as the clean baseline, while replaying
    /// any pending panel-owned edits on top.
    pub fn rebase_onto(&mut self, new: &Settings) {
        let mut pending: Vec<(&'static str, Option<String>)> = Vec::new();
        for env in self
            .base_values
            .keys()
            .chain(self.values.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            if self.base_values.get(env) != self.values.get(env) {
                pending.push((env, self.values.get(env).cloned()));
            }
        }

        let nv = new.to_edit_values();
        self.base_values = nv.clone();
        self.values = nv;

        for (env, val) in pending {
            match val {
                Some(value) => {
                    self.values.insert(env, value);
                }
                None => {
                    self.values.remove(env);
                }
            }
        }

        // Pending edits were already parsed when accepted; fall back to the
        // incoming snapshot rather than leaving a half-applied settings view.
        self.settings = Settings::from_edit_values(&self.values).unwrap_or_else(|_| new.clone());
    }

    pub fn apply_raw(
        &mut self,
        key: &'static str,
        raw: &str,
    ) -> Result<Option<Settings>, SettingEditError> {
        let Some(info) = self
            .settings
            .setting_info()
            .into_iter()
            .find(|info| info.key == key)
        else {
            return Err(SettingEditError {
                key,
                message: "Unknown setting row.".to_owned(),
            });
        };
        if !info.reloadable {
            return Err(SettingEditError {
                key,
                message: "This setting is startup-only and cannot be edited live.".to_owned(),
            });
        }

        let mut values = self.values.clone();
        let trimmed = raw.trim();
        if clears_setting(key, trimmed) {
            values.remove(info.env);
        } else {
            values.insert(info.env, trimmed.to_owned());
        }

        let candidate = Settings::from_edit_values(&values).map_err(|mut error| {
            error.key = key;
            error
        })?;
        let canonical = candidate.to_edit_values();
        if let Some(value) = canonical.get(info.env) {
            values.insert(info.env, value.clone());
        } else {
            values.remove(info.env);
        }
        let candidate = Settings::from_edit_values(&values).map_err(|mut error| {
            error.key = key;
            error
        })?;
        if candidate == self.settings {
            self.values = values;
            return Ok(None);
        }

        self.values = values;
        self.settings = candidate.clone();
        Ok(Some(candidate))
    }
}

fn clears_setting(key: &str, value: &str) -> bool {
    (value.is_empty() || (key == "symbol_font" && value.eq_ignore_ascii_case("auto")))
        && matches!(
            key,
            "font"
                | "font_family"
                | "symbol_font"
                | "native_autoclose_ms"
                | "external_palette_path"
                | "os_theme_dark"
                | "os_theme_light"
        )
}

/// User-facing overlay message for a failed `font_family` edit, naming the
/// family and the precise reason. The current font is kept because the edit is
/// rejected (the loader is never switched to the embedded probe list here).
pub(super) fn font_family_error_message(
    family: &str,
    reason: crate::text::FontResolveError,
) -> String {
    use crate::text::FontResolveError;
    match reason {
        FontResolveError::NotFound => {
            format!("Font family \"{family}\" not found. Keeping the current font.")
        }
        FontResolveError::NotMonospace => {
            format!("Font family \"{family}\" is not monospace. Keeping the current font.")
        }
    }
}

fn setting_key_for_env(env: &str) -> Option<&'static str> {
    env_to_config_key(env)
}

pub(super) fn key_bindings_edit_value(bindings: &[KeyBindingOverride]) -> String {
    bindings
        .iter()
        .map(format_key_binding)
        .collect::<Vec<_>>()
        .join(";")
}

/// Serialize key-binding overrides to the `keybinds=` config value (KB-REMAP
/// persistence). Public wrapper over the internal serializer so the native
/// remap UI writes the EXACT string the parser round-trips — never a reinvented
/// format. An empty slice yields an empty string (clears the setting).
pub fn key_bindings_config_value(overrides: &[KeyBindingOverride]) -> String {
    key_bindings_edit_value(overrides)
}

/// Display string for a single chord (KB-REMAP UI). Public wrapper over the
/// internal formatter so the on-screen label and the persisted config value
/// agree byte-for-byte (e.g. `ctrl+shift+f`).
pub fn format_key_chord(chord: KeyChord) -> String {
    format_chord(chord)
}

/// Canonical display name for a bindable action (KB-REMAP UI). The single
/// authority shared with the settings-panel `keybinds` options list, so the
/// remap menu and the config tokens never drift.
pub fn bindable_action_display_name(action: BindableAction) -> &'static str {
    bindable_action_name(action)
}

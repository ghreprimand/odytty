// SPDX-License-Identifier: GPL-3.0-only
//! Sectioned add/edit form for the named-profile manager.
//!
//! Child of [`super`], so these `impl ProfileManager` blocks share the same
//! private draft fields declared on the struct there. The form exposes every
//! field the profile schema supports, grouped into Identity, Launch,
//! Appearance, Cursor, Effects, Layout, Switching, and Actions sections. Every
//! rendered line still carries its own [`ProfileManagerTarget`], so section
//! headers, the shell-suggestion hint, list rows, and validation errors cannot
//! shift a pointer target away from what is drawn (commit b7517388 contract).
//!
//! Field kinds:
//!   * plain text (name, shell, working directory, theme, paths, ...);
//!   * `Option<bool>` tri-states (follow-external-palette, bloom, crt, retro)
//!     that cycle inherit -> on -> off with Enter/Space/click;
//!   * enumerated cycles (platforms, visual, font weight, cursor style/blink,
//!     render quality) that step through valid values only, never free text;
//!   * bounded lists (command args, env pairs, host/directory match rules) with
//!     one rendered row per entry, an add row, and a visible limit message from
//!     `src/profiles/limits.rs`.
//!
//! No secret ever persists: env keys/values are re-validated by the schema
//! round-trip in [`ProfileManager::try_save`], and a rejected entry surfaces
//! inline without being silently dropped from the draft.

use std::collections::BTreeMap;

use crate::external_palette::ExternalPaletteProvider;
use crate::profiles::{
    DiscoveredShell, LaunchProfile, MAX_PROFILE_COMMAND_ARGS, MAX_PROFILE_ENV_ENTRIES,
    MAX_PROFILE_SWITCH_DIRECTORIES, MAX_PROFILE_SWITCH_HOSTS, ProfileCommand, ProfilePlatform,
    validate_profile_name,
};

use super::super::overlay::OverlayInput;
use super::super::shell_discovery;
use super::{
    FormField, FormMode, ManagerView, ProfileManager, ProfileManagerLine, ProfileManagerOutcome,
    ProfileManagerTarget, truncate,
};

impl ProfileManager {
    pub(super) fn open_add(&mut self) -> ProfileManagerOutcome {
        self.clear_draft();
        self.load_shell_suggestions();
        self.view = ManagerView::Form(FormMode::Add);
        self.form_focus = 0;
        self.form_scroll_offset.set(0);
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    pub(super) fn open_edit_selected(&mut self) -> ProfileManagerOutcome {
        let Some(name) = self.selected_name() else {
            return ProfileManagerOutcome::Consumed;
        };
        let Some(profile) = self.profiles.get(&name).cloned() else {
            return ProfileManagerOutcome::Consumed;
        };
        self.load_draft_from(&profile);
        self.load_shell_suggestions();
        self.replace_name = Some(profile.name.clone());
        self.view = ManagerView::Form(FormMode::Edit);
        self.form_focus = 0;
        self.form_scroll_offset.set(0);
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    pub(super) fn open_duplicate_selected(&mut self) -> ProfileManagerOutcome {
        let Some(name) = self.selected_name() else {
            return ProfileManagerOutcome::Consumed;
        };
        let Some(profile) = self.profiles.get(&name).cloned() else {
            return ProfileManagerOutcome::Consumed;
        };
        self.load_draft_from(&profile);
        self.load_shell_suggestions();
        self.draft_name = unique_copy_name(&profile.name, &self.profiles);
        self.replace_name = None;
        self.view = ManagerView::Form(FormMode::Duplicate);
        self.form_focus = 0;
        self.form_scroll_offset.set(0);
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    pub(super) fn open_rename_selected(&mut self) -> ProfileManagerOutcome {
        let Some(name) = self.selected_name() else {
            return ProfileManagerOutcome::Consumed;
        };
        let Some(profile) = self.profiles.get(&name).cloned() else {
            return ProfileManagerOutcome::Consumed;
        };
        self.load_draft_from(&profile);
        self.replace_name = Some(profile.name.clone());
        self.view = ManagerView::Form(FormMode::Rename);
        self.form_focus = 0;
        self.form_scroll_offset.set(0);
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    pub(super) fn handle_form_input(&mut self, input: OverlayInput) -> ProfileManagerOutcome {
        let fields = self.visible_form_fields();
        match input {
            OverlayInput::Close => {
                self.return_to_catalog();
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Up => {
                if self.form_focus > 0 {
                    self.form_focus -= 1;
                }
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Down | OverlayInput::Tab => {
                if self.form_focus + 1 < fields.len() {
                    self.form_focus += 1;
                }
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Right => {
                if matches!(fields.get(self.form_focus), Some(FormField::Shell)) {
                    self.cycle_shell_suggestion(1);
                }
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Left => {
                if matches!(fields.get(self.form_focus), Some(FormField::Shell)) {
                    self.cycle_shell_suggestion(-1);
                }
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Activate => fields
                .get(self.form_focus)
                .copied()
                .map(|field| self.activate_form_field(field))
                .unwrap_or(ProfileManagerOutcome::Consumed),
            // Space only doubles as a toggle/cycle activator on the closed-
            // vocabulary fields (tri-states and cycles), matching the
            // connection form. On every free-text field it must fall through
            // to `edit_active_buffer` so a literal space types, not vanishes.
            OverlayInput::Char(' ')
                if fields
                    .get(self.form_focus)
                    .is_some_and(|field| field.is_toggle_or_cycle()) =>
            {
                fields
                    .get(self.form_focus)
                    .copied()
                    .map(|field| self.activate_form_field(field))
                    .unwrap_or(ProfileManagerOutcome::Consumed)
            }
            OverlayInput::Backspace => {
                self.edit_active_buffer(|buf| {
                    buf.pop();
                });
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Char(ch) => {
                self.edit_active_buffer(|buf| {
                    buf.push(ch);
                });
                ProfileManagerOutcome::Consumed
            }
            _ => ProfileManagerOutcome::Consumed,
        }
    }

    /// The ordered, section-grouped field list for the current form mode.
    ///
    /// Fields are emitted in section order so [`Self::form_all_lines`] draws
    /// each section header exactly once, and dynamic list rows sit inside their
    /// owning section. Rename mode shows only the identity plus the actions.
    pub(super) fn visible_form_fields(&self) -> Vec<FormField> {
        if matches!(self.view, ManagerView::Form(FormMode::Rename)) {
            return vec![FormField::Name, FormField::Save, FormField::Cancel];
        }
        let mut fields = vec![
            // Identity
            FormField::Name,
            FormField::DisplayName,
            FormField::Platforms,
            // Launch
            FormField::Shell,
            FormField::WorkingDirectory,
            FormField::CommandProgram,
        ];
        for index in 0..self.draft_command_args.len() {
            fields.push(FormField::CommandArg(index));
            fields.push(FormField::RemoveCommandArg(index));
        }
        fields.push(FormField::AddCommandArg);
        for index in 0..self.draft_env.len() {
            fields.push(FormField::EnvKey(index));
            fields.push(FormField::EnvValue(index));
            fields.push(FormField::RemoveEnv(index));
        }
        fields.push(FormField::AddEnv);
        // Appearance
        fields.push(FormField::Theme);
        fields.push(FormField::Visual);
        fields.push(FormField::Font);
        fields.push(FormField::FontFamily);
        fields.push(FormField::FontWeight);
        fields.push(FormField::FontSizePx);
        fields.push(FormField::Title);
        fields.push(FormField::FollowExternalPalette);
        fields.push(FormField::ExternalPaletteProvider);
        fields.push(FormField::ExternalPalettePath);
        // Cursor
        fields.push(FormField::CursorStyle);
        fields.push(FormField::CursorBlink);
        // Effects
        fields.push(FormField::RenderQuality);
        fields.push(FormField::Bloom);
        fields.push(FormField::Crt);
        fields.push(FormField::Retro);
        // Layout
        fields.push(FormField::SavedLayout);
        fields.push(FormField::Connection);
        // Switching
        for index in 0..self.draft_match_hosts.len() {
            fields.push(FormField::MatchHost(index));
            fields.push(FormField::RemoveMatchHost(index));
        }
        fields.push(FormField::AddMatchHost);
        for index in 0..self.draft_match_directories.len() {
            fields.push(FormField::MatchDirectory(index));
            fields.push(FormField::RemoveMatchDirectory(index));
        }
        fields.push(FormField::AddMatchDirectory);
        // Actions
        fields.push(FormField::Save);
        fields.push(FormField::Cancel);
        fields
    }

    pub(super) fn activate_form_field(&mut self, field: FormField) -> ProfileManagerOutcome {
        match field {
            FormField::Save => return self.try_save(),
            FormField::Cancel => {
                self.return_to_catalog();
                return ProfileManagerOutcome::Consumed;
            }
            FormField::AddCommandArg => {
                if self.draft_command_args.len() >= MAX_PROFILE_COMMAND_ARGS {
                    self.error = Some(format!(
                        "command arguments are limited to {MAX_PROFILE_COMMAND_ARGS}"
                    ));
                } else {
                    self.draft_command_args.push(String::new());
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::RemoveCommandArg(index) => {
                if index < self.draft_command_args.len() {
                    self.draft_command_args.remove(index);
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::AddEnv => {
                if self.draft_env.len() >= MAX_PROFILE_ENV_ENTRIES {
                    self.error = Some(format!(
                        "environment overrides are limited to {MAX_PROFILE_ENV_ENTRIES}"
                    ));
                } else {
                    self.draft_env.push((String::new(), String::new()));
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::RemoveEnv(index) => {
                if index < self.draft_env.len() {
                    self.draft_env.remove(index);
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::AddMatchHost => {
                if self.draft_match_hosts.len() >= MAX_PROFILE_SWITCH_HOSTS {
                    self.error = Some(format!(
                        "host match rules are limited to {MAX_PROFILE_SWITCH_HOSTS}"
                    ));
                } else {
                    self.draft_match_hosts.push(String::new());
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::RemoveMatchHost(index) => {
                if index < self.draft_match_hosts.len() {
                    self.draft_match_hosts.remove(index);
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::AddMatchDirectory => {
                if self.draft_match_directories.len() >= MAX_PROFILE_SWITCH_DIRECTORIES {
                    self.error = Some(format!(
                        "directory match rules are limited to {MAX_PROFILE_SWITCH_DIRECTORIES}"
                    ));
                } else {
                    self.draft_match_directories.push(String::new());
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::RemoveMatchDirectory(index) => {
                if index < self.draft_match_directories.len() {
                    self.draft_match_directories.remove(index);
                }
                return ProfileManagerOutcome::Consumed;
            }
            FormField::Platforms => cycle(
                &mut self.draft_platforms,
                &["inherit", "linux", "macos", "windows", "all"],
            ),
            FormField::Visual => cycle(
                &mut self.draft_visual,
                &["inherit", "plain", "odyssey", "retro"],
            ),
            FormField::FontWeight => cycle(
                &mut self.draft_font_weight,
                &["inherit", "normal", "medium", "bold"],
            ),
            FormField::CursorStyle => cycle(
                &mut self.draft_cursor_style,
                &["inherit", "block", "beam", "underline"],
            ),
            FormField::CursorBlink => {
                cycle(&mut self.draft_cursor_blink, &["inherit", "on", "off"])
            }
            FormField::RenderQuality => cycle(
                &mut self.draft_render_quality,
                &["inherit", "low", "balanced", "high"],
            ),
            FormField::FollowExternalPalette => {
                toggle_optional(&mut self.draft_follow_external_palette)
            }
            FormField::Bloom => toggle_optional(&mut self.draft_bloom),
            FormField::Crt => toggle_optional(&mut self.draft_crt),
            FormField::Retro => toggle_optional(&mut self.draft_retro),
            // Text fields: activation only moves focus, which the caller has
            // already applied; there is no toggle to perform here.
            FormField::Name
            | FormField::DisplayName
            | FormField::Shell
            | FormField::WorkingDirectory
            | FormField::CommandProgram
            | FormField::CommandArg(_)
            | FormField::EnvKey(_)
            | FormField::EnvValue(_)
            | FormField::Theme
            | FormField::Font
            | FormField::FontFamily
            | FormField::FontSizePx
            | FormField::Title
            | FormField::ExternalPaletteProvider
            | FormField::ExternalPalettePath
            | FormField::Connection
            | FormField::SavedLayout
            | FormField::MatchHost(_)
            | FormField::MatchDirectory(_) => {}
        }
        ProfileManagerOutcome::Consumed
    }

    fn try_save(&mut self) -> ProfileManagerOutcome {
        let rename_only = matches!(self.view, ManagerView::Form(FormMode::Rename));
        let name = match validate_profile_name(&self.draft_name) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error.to_string());
                return ProfileManagerOutcome::Consumed;
            }
        };
        if self.profiles.contains_key(&name) && self.replace_name.as_deref() != Some(name.as_str())
        {
            self.error = Some(format!("profile {name:?} already exists"));
            return ProfileManagerOutcome::Consumed;
        }

        let mut profile = self
            .draft_base
            .clone()
            .unwrap_or_else(|| LaunchProfile::new(&name).expect("validated name"));
        profile.name = name.clone();
        if rename_only {
            // Keep every other field; only the identity changes.
        } else {
            profile.display_name = nonempty_opt(&self.draft_display_name);
            if !self.draft_command_program.trim().is_empty() {
                profile.launch.command = Some(ProfileCommand {
                    program: self.draft_command_program.trim().to_owned(),
                    args: self
                        .draft_command_args
                        .iter()
                        .filter_map(|arg| nonempty_opt(arg))
                        .collect(),
                    preserved: profile
                        .launch
                        .command
                        .as_ref()
                        .map(|command| command.preserved.clone())
                        .unwrap_or_default(),
                });
                profile.launch.shell = None;
            } else if let Some(command) = self.pending_shell_command.take() {
                profile.launch.command = Some(command);
                profile.launch.shell = None;
            } else {
                profile.launch.shell = nonempty_opt(&self.draft_shell);
            }
            profile.launch.working_directory = nonempty_opt(&self.draft_working_directory);
            profile.appearance.theme = nonempty_opt(&self.draft_theme);
            match optional_bool_field(&self.draft_follow_external_palette) {
                Ok(value) => profile.appearance.follow_external_palette = value,
                Err(message) => {
                    self.error = Some(message);
                    return ProfileManagerOutcome::Consumed;
                }
            }
            profile.appearance.external_palette_provider =
                nonempty_opt(&self.draft_external_palette_provider);
            profile.appearance.external_palette_path =
                nonempty_opt(&self.draft_external_palette_path);
            if profile.appearance.follow_external_palette == Some(true)
                && profile
                    .appearance
                    .external_palette_path
                    .as_deref()
                    .is_none_or(str::is_empty)
            {
                self.error = Some(
                    "external palette path is required when follow external palette is on"
                        .to_owned(),
                );
                return ProfileManagerOutcome::Consumed;
            }
            if let Some(raw) = profile.appearance.external_palette_provider.as_deref()
                && !raw.is_empty()
                && ExternalPaletteProvider::parse(raw).is_none()
            {
                self.error = Some(format!(
                    "unknown external palette provider {raw:?}; use odytty, colors_toml, or colors_json"
                ));
                return ProfileManagerOutcome::Consumed;
            }
            profile.appearance.font_family = nonempty_opt(&self.draft_font_family);
            profile.appearance.visual = enum_opt(&self.draft_visual);
            profile.appearance.font = nonempty_opt(&self.draft_font);
            profile.appearance.font_weight = enum_opt(&self.draft_font_weight);
            profile.appearance.font_size_px = match nonempty_opt(&self.draft_font_size_px) {
                Some(raw) => match raw.parse::<f32>() {
                    Ok(value) if value.is_finite() && value > 0.0 => Some(value),
                    _ => {
                        self.error = Some("font size px must be a positive number".to_owned());
                        return ProfileManagerOutcome::Consumed;
                    }
                },
                None => None,
            };
            profile.appearance.title = nonempty_opt(&self.draft_title);
            profile.cursor.style = enum_opt(&self.draft_cursor_style);
            profile.cursor.blink = enum_opt(&self.draft_cursor_blink);
            profile.effects.render_quality = enum_opt(&self.draft_render_quality);
            profile.effects.bloom = match optional_bool_field(&self.draft_bloom) {
                Ok(value) => value,
                Err(_) => {
                    self.error = Some("bloom must be on, off, or inherit".to_owned());
                    return ProfileManagerOutcome::Consumed;
                }
            };
            profile.effects.crt = match optional_bool_field(&self.draft_crt) {
                Ok(value) => value,
                Err(_) => {
                    self.error = Some("crt must be on, off, or inherit".to_owned());
                    return ProfileManagerOutcome::Consumed;
                }
            };
            profile.effects.retro = match optional_bool_field(&self.draft_retro) {
                Ok(value) => value,
                Err(_) => {
                    self.error = Some("retro must be on, off, or inherit".to_owned());
                    return ProfileManagerOutcome::Consumed;
                }
            };
            profile.layout.saved_layout = nonempty_opt(&self.draft_saved_layout);
            profile.switch.match_hosts = self
                .draft_match_hosts
                .iter()
                .filter_map(|value| nonempty_opt(value))
                .collect();
            profile.switch.match_directories = self
                .draft_match_directories
                .iter()
                .filter_map(|value| nonempty_opt(value))
                .collect();
            // Build the env map row by row so an incomplete override cannot be
            // silently discarded. A row empty on BOTH sides is a placeholder
            // and skipped; a row with exactly one side filled is a user error
            // and blocks the save with an inline message, leaving the draft
            // intact so the operator can fix it.
            let mut env = BTreeMap::new();
            for (key, value) in &self.draft_env {
                let key_trimmed = key.trim();
                let value_trimmed = value.trim();
                if key_trimmed.is_empty() && value_trimmed.is_empty() {
                    continue;
                }
                if key_trimmed.is_empty() || value_trimmed.is_empty() {
                    self.error = Some(
                        "environment key and value are both required; \
                         clear both to drop the row"
                            .to_owned(),
                    );
                    return ProfileManagerOutcome::Consumed;
                }
                env.insert(key_trimmed.to_owned(), value_trimmed.to_owned());
            }
            profile.launch.env = env;
            // Preserve a loaded platform set exactly when the chooser token was
            // not touched, so a hand-authored multi-platform set is never
            // silently collapsed to the single token the chooser can show.
            if self.draft_platforms != format_platforms(profile.platforms.as_ref()) {
                profile.platforms = parse_platforms(&self.draft_platforms);
            }
            profile.connection = nonempty_opt(&self.draft_connection);
        }

        if let Err(error) = profile.validate() {
            self.error = Some(error.to_string());
            return ProfileManagerOutcome::Consumed;
        }

        let replace = match &self.replace_name {
            Some(old) if old != &name => Some(old.clone()),
            _ => None,
        };
        ProfileManagerOutcome::Persist {
            profile: Box::new(profile),
            replace,
        }
    }

    fn load_draft_from(&mut self, profile: &LaunchProfile) {
        self.draft_base = Some(profile.clone());
        self.draft_name = profile.name.clone();
        self.draft_display_name = profile.display_name.clone().unwrap_or_default();
        self.draft_shell = profile.launch.shell.clone().unwrap_or_default();
        self.draft_working_directory = profile.launch.working_directory.clone().unwrap_or_default();
        self.draft_theme = profile.appearance.theme.clone().unwrap_or_default();
        self.draft_follow_external_palette = profile
            .appearance
            .follow_external_palette
            .map(|enabled| if enabled { "on" } else { "off" }.to_owned())
            .unwrap_or_else(|| "inherit".to_owned());
        self.draft_external_palette_provider = profile
            .appearance
            .external_palette_provider
            .clone()
            .unwrap_or_default();
        self.draft_external_palette_path = profile
            .appearance
            .external_palette_path
            .clone()
            .unwrap_or_default();
        self.draft_font_family = profile.appearance.font_family.clone().unwrap_or_default();
        self.draft_title = profile.appearance.title.clone().unwrap_or_default();
        self.draft_connection = profile.connection.clone().unwrap_or_default();
        self.draft_command_program = profile
            .launch
            .command
            .as_ref()
            .map(|command| command.program.clone())
            .unwrap_or_default();
        self.draft_command_args = profile
            .launch
            .command
            .as_ref()
            .map(|command| command.args.clone())
            .unwrap_or_default();
        self.draft_env = profile
            .launch
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        self.draft_platforms = format_platforms(profile.platforms.as_ref());
        self.draft_visual = profile
            .appearance
            .visual
            .clone()
            .unwrap_or_else(|| "inherit".to_owned());
        self.draft_font = profile.appearance.font.clone().unwrap_or_default();
        self.draft_font_weight = profile
            .appearance
            .font_weight
            .clone()
            .unwrap_or_else(|| "inherit".to_owned());
        self.draft_font_size_px = profile
            .appearance
            .font_size_px
            .map(|value| value.to_string())
            .unwrap_or_default();
        self.draft_cursor_style = profile
            .cursor
            .style
            .clone()
            .unwrap_or_else(|| "inherit".to_owned());
        self.draft_cursor_blink = profile
            .cursor
            .blink
            .clone()
            .unwrap_or_else(|| "inherit".to_owned());
        self.draft_render_quality = profile
            .effects
            .render_quality
            .clone()
            .unwrap_or_else(|| "inherit".to_owned());
        self.draft_bloom = optional_bool_text(profile.effects.bloom);
        self.draft_crt = optional_bool_text(profile.effects.crt);
        self.draft_retro = optional_bool_text(profile.effects.retro);
        self.draft_saved_layout = profile.layout.saved_layout.clone().unwrap_or_default();
        self.draft_match_hosts = profile.switch.match_hosts.clone();
        self.draft_match_directories = profile.switch.match_directories.clone();
        self.pending_shell_command = profile.launch.command.clone();
        if self.pending_shell_command.is_some() {
            self.draft_shell = profile
                .launch
                .command
                .as_ref()
                .map(|command| {
                    if command.args.is_empty() {
                        command.program.clone()
                    } else {
                        format!("{} {}", command.program, command.args.join(" "))
                    }
                })
                .unwrap_or_default();
        }
    }

    pub(super) fn clear_draft(&mut self) {
        self.draft_base = None;
        self.replace_name = None;
        self.draft_name.clear();
        self.draft_display_name.clear();
        self.draft_shell.clear();
        self.draft_working_directory.clear();
        self.draft_theme.clear();
        self.draft_follow_external_palette = "inherit".to_owned();
        self.draft_external_palette_provider.clear();
        self.draft_external_palette_path.clear();
        self.draft_font_family.clear();
        self.draft_title.clear();
        self.draft_connection.clear();
        self.draft_command_program.clear();
        self.draft_command_args.clear();
        self.draft_env.clear();
        self.draft_platforms = "inherit".to_owned();
        self.draft_visual = "inherit".to_owned();
        self.draft_font.clear();
        self.draft_font_weight = "inherit".to_owned();
        self.draft_font_size_px.clear();
        self.draft_cursor_style = "inherit".to_owned();
        self.draft_cursor_blink = "inherit".to_owned();
        self.draft_render_quality = "inherit".to_owned();
        self.draft_bloom = "inherit".to_owned();
        self.draft_crt = "inherit".to_owned();
        self.draft_retro = "inherit".to_owned();
        self.draft_saved_layout.clear();
        self.draft_match_hosts.clear();
        self.draft_match_directories.clear();
        self.discovered_shells.clear();
        self.shell_suggestion_index = 0;
        self.pending_shell_command = None;
        self.form_focus = 0;
        self.form_scroll_offset.set(0);
    }

    fn load_shell_suggestions(&mut self) {
        self.discovered_shells = shell_discovery::discovered_shells();
        self.shell_suggestion_index = 0;
    }

    fn cycle_shell_suggestion(&mut self, delta: isize) {
        if self.discovered_shells.is_empty() {
            return;
        }
        let len = self.discovered_shells.len();
        let next = (self.shell_suggestion_index as isize + delta).rem_euclid(len as isize);
        self.shell_suggestion_index = next as usize;
        let entry = self.discovered_shells[self.shell_suggestion_index].clone();
        self.apply_shell_suggestion(&entry);
    }

    fn apply_shell_suggestion(&mut self, entry: &DiscoveredShell) {
        if entry.args.is_empty() {
            self.draft_shell = entry.program.clone();
            self.pending_shell_command = None;
        } else {
            self.draft_shell = entry.label.clone();
            self.pending_shell_command = Some(ProfileCommand {
                program: entry.program.clone(),
                args: entry.args.clone(),
                preserved: Default::default(),
            });
        }
        if let Some(base) = &mut self.draft_base {
            base.launch.command = None;
        }
        self.error = None;
    }

    fn shell_suggestion_hint(&self) -> Option<String> {
        if self.discovered_shells.is_empty() {
            return None;
        }
        let entry = &self.discovered_shells[self.shell_suggestion_index];
        Some(format!(
            "Shell suggestions (Left/Right): {} ({}/{})",
            entry.label,
            self.shell_suggestion_index + 1,
            self.discovered_shells.len()
        ))
    }

    fn edit_active_buffer(&mut self, edit: impl FnOnce(&mut String)) {
        let fields = self.visible_form_fields();
        let Some(field) = fields.get(self.form_focus).copied() else {
            return;
        };
        let buffer = match field {
            FormField::Name => &mut self.draft_name,
            FormField::DisplayName => &mut self.draft_display_name,
            FormField::Shell => {
                self.pending_shell_command = None;
                if let Some(base) = &mut self.draft_base {
                    base.launch.command = None;
                }
                &mut self.draft_shell
            }
            FormField::WorkingDirectory => &mut self.draft_working_directory,
            FormField::Theme => &mut self.draft_theme,
            FormField::ExternalPaletteProvider => &mut self.draft_external_palette_provider,
            FormField::ExternalPalettePath => &mut self.draft_external_palette_path,
            FormField::FontFamily => &mut self.draft_font_family,
            FormField::Title => &mut self.draft_title,
            FormField::Connection => &mut self.draft_connection,
            FormField::CommandProgram => &mut self.draft_command_program,
            FormField::CommandArg(index) => match self.draft_command_args.get_mut(index) {
                Some(value) => value,
                None => return,
            },
            FormField::EnvKey(index) => match self.draft_env.get_mut(index) {
                Some((key, _)) => key,
                None => return,
            },
            FormField::EnvValue(index) => match self.draft_env.get_mut(index) {
                Some((_, value)) => value,
                None => return,
            },
            FormField::Font => &mut self.draft_font,
            FormField::FontSizePx => &mut self.draft_font_size_px,
            FormField::SavedLayout => &mut self.draft_saved_layout,
            FormField::MatchHost(index) => match self.draft_match_hosts.get_mut(index) {
                Some(value) => value,
                None => return,
            },
            FormField::MatchDirectory(index) => match self.draft_match_directories.get_mut(index) {
                Some(value) => value,
                None => return,
            },
            // Toggle, cycle, and action fields carry no free-text buffer.
            FormField::Save
            | FormField::Cancel
            | FormField::AddCommandArg
            | FormField::RemoveCommandArg(_)
            | FormField::AddEnv
            | FormField::RemoveEnv(_)
            | FormField::Platforms
            | FormField::Visual
            | FormField::FontWeight
            | FormField::FollowExternalPalette
            | FormField::CursorStyle
            | FormField::CursorBlink
            | FormField::RenderQuality
            | FormField::Bloom
            | FormField::Crt
            | FormField::Retro
            | FormField::AddMatchHost
            | FormField::RemoveMatchHost(_)
            | FormField::AddMatchDirectory
            | FormField::RemoveMatchDirectory(_) => return,
        };
        edit(buffer);
        self.error = None;
    }

    pub(super) fn form_visible_lines(
        &self,
        mode: FormMode,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ProfileManagerLine> {
        let lines = self.form_all_lines(body_width);
        if body_height == 0 || lines.len() <= body_height {
            self.form_scroll_offset.set(0);
            return lines;
        }
        let focused = lines.iter().position(|line| line.focused).unwrap_or(0);
        let max_offset = lines.len() - body_height;
        let mut offset = self.form_scroll_offset.get().min(max_offset);
        if focused < offset {
            offset = focused;
        } else if focused >= offset + body_height {
            offset = focused + 1 - body_height;
        }
        self.form_scroll_offset.set(offset);
        let _ = mode;
        lines.into_iter().skip(offset).take(body_height).collect()
    }

    pub(super) fn form_all_lines(&self, body_width: usize) -> Vec<ProfileManagerLine> {
        let mut lines = Vec::new();
        let fields = self.visible_form_fields();
        let mut section = None;
        for (row, field) in fields.iter().enumerate() {
            let next_section = form_section(*field);
            if section != Some(next_section) {
                lines.push(ProfileManagerLine {
                    text: format!("-- {next_section} --"),
                    focused: false,
                    bold: true,
                    target: ProfileManagerTarget::Inert,
                });
                section = Some(next_section);
            }
            let focused = row == self.form_focus;
            let text = self.form_field_text(*field);
            lines.push(ProfileManagerLine {
                text: truncate(&text, body_width),
                focused,
                bold: matches!(field, FormField::Save | FormField::Cancel) || focused,
                target: ProfileManagerTarget::FormField(*field),
            });
            if focused
                && matches!(field, FormField::Shell)
                && let Some(hint) = self.shell_suggestion_hint()
            {
                lines.push(ProfileManagerLine {
                    text: truncate(&hint, body_width),
                    focused: false,
                    bold: false,
                    target: ProfileManagerTarget::Inert,
                });
            }
        }
        if let Some(error) = &self.error {
            lines.push(ProfileManagerLine {
                text: truncate(error, body_width),
                focused: false,
                bold: false,
                target: ProfileManagerTarget::Inert,
            });
        }
        lines
    }

    fn form_field_text(&self, field: FormField) -> String {
        let empty = String::new();
        match field {
            FormField::Name => format!("Name: {}", self.draft_name),
            FormField::DisplayName => format!("Display name: {}", self.draft_display_name),
            FormField::Shell => format!("Shell: {}", self.draft_shell),
            FormField::WorkingDirectory => {
                format!("Working directory: {}", self.draft_working_directory)
            }
            FormField::Theme => format!("Theme: {}", self.draft_theme),
            FormField::FollowExternalPalette => {
                format!(
                    "Follow external palette: {}",
                    self.draft_follow_external_palette
                )
            }
            FormField::ExternalPaletteProvider => {
                format!(
                    "External palette provider: {}",
                    self.draft_external_palette_provider
                )
            }
            FormField::ExternalPalettePath => {
                format!(
                    "External palette path: {}",
                    self.draft_external_palette_path
                )
            }
            FormField::FontFamily => format!("Font family: {}", self.draft_font_family),
            FormField::Title => format!("Title: {}", self.draft_title),
            FormField::Connection => format!("Connection: {}", self.draft_connection),
            FormField::CommandProgram => {
                format!("Command program: {}", self.draft_command_program)
            }
            FormField::CommandArg(index) => format!(
                "Command arg {}: {}",
                index + 1,
                self.draft_command_args.get(index).unwrap_or(&empty)
            ),
            FormField::AddCommandArg => "[Add command argument]".to_owned(),
            FormField::RemoveCommandArg(index) => {
                format!("[Remove command argument {}]", index + 1)
            }
            FormField::EnvKey(index) => format!(
                "Environment key {}: {}",
                index + 1,
                self.draft_env
                    .get(index)
                    .map(|entry| entry.0.as_str())
                    .unwrap_or("")
            ),
            FormField::EnvValue(index) => format!(
                "Environment value {}: {}",
                index + 1,
                self.draft_env
                    .get(index)
                    .map(|entry| entry.1.as_str())
                    .unwrap_or("")
            ),
            FormField::AddEnv => "[Add environment override]".to_owned(),
            FormField::RemoveEnv(index) => {
                format!("[Remove environment override {}]", index + 1)
            }
            FormField::Platforms => format!("Platforms: {}", self.draft_platforms),
            FormField::Visual => format!("Visual: {}", self.draft_visual),
            FormField::Font => format!("Font: {}", self.draft_font),
            FormField::FontWeight => format!("Font weight: {}", self.draft_font_weight),
            FormField::FontSizePx => format!("Font size px: {}", self.draft_font_size_px),
            FormField::CursorStyle => format!("Cursor style: {}", self.draft_cursor_style),
            FormField::CursorBlink => format!("Cursor blink: {}", self.draft_cursor_blink),
            FormField::RenderQuality => format!("Render quality: {}", self.draft_render_quality),
            FormField::Bloom => format!("Bloom: {}", self.draft_bloom),
            FormField::Crt => format!("CRT: {}", self.draft_crt),
            FormField::Retro => format!("Retro: {}", self.draft_retro),
            FormField::SavedLayout => format!("Saved layout: {}", self.draft_saved_layout),
            FormField::MatchHost(index) => format!(
                "Host match {}: {}",
                index + 1,
                self.draft_match_hosts.get(index).unwrap_or(&empty)
            ),
            FormField::AddMatchHost => "[Add host match]".to_owned(),
            FormField::RemoveMatchHost(index) => format!("[Remove host match {}]", index + 1),
            FormField::MatchDirectory(index) => format!(
                "Directory match {}: {}",
                index + 1,
                self.draft_match_directories.get(index).unwrap_or(&empty)
            ),
            FormField::AddMatchDirectory => "[Add directory match]".to_owned(),
            FormField::RemoveMatchDirectory(index) => {
                format!("[Remove directory match {}]", index + 1)
            }
            FormField::Save => "[Save]".to_owned(),
            FormField::Cancel => "[Cancel]".to_owned(),
        }
    }
}

/// Trim a free-text or list draft value to `Some` only when non-empty.
///
/// This does NOT treat `"inherit"` as a sentinel: the `inherit` word belongs
/// exclusively to the closed-vocabulary fields (see [`enum_opt`] and
/// [`optional_bool_field`]). A free-text scalar or list entry must round-trip
/// any literal string, including the word "inherit", byte for byte.
fn nonempty_opt(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Interpret a tri-state toggle draft: `inherit`/empty is `None`, `on`/`off`
/// (and common synonyms) are `Some(bool)`, anything else is a hard error the
/// form surfaces inline rather than guessing.
fn optional_bool_field(value: &str) -> Result<Option<bool>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("inherit") {
        return Ok(None);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Ok(Some(true)),
        "off" | "false" | "0" | "no" => Ok(Some(false)),
        other => Err(format!(
            "follow external palette must be inherit, on, or off, not {other:?}"
        )),
    }
}

fn toggle_optional(value: &mut String) {
    *value = match value.as_str() {
        "inherit" | "" => "on".to_owned(),
        "on" => "off".to_owned(),
        _ => "inherit".to_owned(),
    };
}

fn cycle(value: &mut String, choices: &[&str]) {
    let index = choices
        .iter()
        .position(|choice| *choice == value)
        .unwrap_or(0);
    *value = choices[(index + 1) % choices.len()].to_owned();
}

fn enum_opt(value: &str) -> Option<String> {
    (!value.is_empty() && value != "inherit").then(|| value.to_owned())
}

fn optional_bool_text(value: Option<bool>) -> String {
    match value {
        Some(true) => "on".to_owned(),
        Some(false) => "off".to_owned(),
        None => "inherit".to_owned(),
    }
}

fn format_platforms(value: Option<&std::collections::BTreeSet<ProfilePlatform>>) -> String {
    let Some(value) = value else {
        return "inherit".to_owned();
    };
    if value.len() == 3 {
        return "all".to_owned();
    }
    value
        .iter()
        .next()
        .map(|platform| platform.as_str().to_owned())
        .unwrap_or_else(|| "inherit".to_owned())
}

fn parse_platforms(value: &str) -> Option<std::collections::BTreeSet<ProfilePlatform>> {
    let mut platforms = std::collections::BTreeSet::new();
    match value {
        "linux" => {
            platforms.insert(ProfilePlatform::Linux);
        }
        "macos" => {
            platforms.insert(ProfilePlatform::Macos);
        }
        "windows" => {
            platforms.insert(ProfilePlatform::Windows);
        }
        "all" => {
            platforms.extend([
                ProfilePlatform::Linux,
                ProfilePlatform::Macos,
                ProfilePlatform::Windows,
            ]);
        }
        _ => return None,
    }
    Some(platforms)
}

fn form_section(field: FormField) -> &'static str {
    match field {
        FormField::Name | FormField::DisplayName | FormField::Platforms => "Identity",
        FormField::Shell
        | FormField::WorkingDirectory
        | FormField::CommandProgram
        | FormField::CommandArg(_)
        | FormField::AddCommandArg
        | FormField::RemoveCommandArg(_)
        | FormField::EnvKey(_)
        | FormField::EnvValue(_)
        | FormField::AddEnv
        | FormField::RemoveEnv(_) => "Launch",
        FormField::Theme
        | FormField::Visual
        | FormField::Font
        | FormField::FontFamily
        | FormField::FontWeight
        | FormField::FontSizePx
        | FormField::Title
        | FormField::FollowExternalPalette
        | FormField::ExternalPaletteProvider
        | FormField::ExternalPalettePath => "Appearance",
        FormField::CursorStyle | FormField::CursorBlink => "Cursor",
        FormField::RenderQuality | FormField::Bloom | FormField::Crt | FormField::Retro => {
            "Effects"
        }
        FormField::SavedLayout | FormField::Connection => "Layout",
        FormField::MatchHost(_)
        | FormField::AddMatchHost
        | FormField::RemoveMatchHost(_)
        | FormField::MatchDirectory(_)
        | FormField::AddMatchDirectory
        | FormField::RemoveMatchDirectory(_) => "Switching",
        FormField::Save | FormField::Cancel => "Actions",
    }
}

fn unique_copy_name(base: &str, existing: &BTreeMap<String, LaunchProfile>) -> String {
    let candidate = format!("{base}-copy");
    if !existing.contains_key(&candidate) && validate_profile_name(&candidate).is_ok() {
        return candidate;
    }
    for index in 2..1000 {
        let candidate = format!("{base}-copy-{index}");
        if !existing.contains_key(&candidate) && validate_profile_name(&candidate).is_ok() {
            return candidate;
        }
    }
    format!("{base}-copy")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{ProfileCatalog, ShellKind};

    fn catalog_with(names: &[&str]) -> ProfileCatalog {
        let mut catalog = ProfileCatalog::default();
        for name in names {
            catalog.profiles.insert(
                (*name).to_owned(),
                LaunchProfile::new(*name).expect("profile"),
            );
        }
        catalog
    }

    /// Focus a form field by locating it in the ordered field list, matching
    /// the pointer path rather than a hardcoded Down count that field
    /// reordering would break.
    fn focus_field(manager: &mut ProfileManager, field: FormField) {
        let index = manager
            .visible_form_fields()
            .iter()
            .position(|candidate| *candidate == field)
            .expect("field present");
        manager.form_focus = index;
    }

    #[test]
    fn form_loads_discovered_shell_suggestions_on_add() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&[]), None);
        assert!(matches!(
            manager.open_add(),
            ProfileManagerOutcome::Consumed
        ));
        assert!(
            !manager.discovered_shells.is_empty(),
            "profile form must load cached shell discovery on demand"
        );
    }

    #[test]
    fn form_fields_are_grouped_into_contiguous_sections() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&[]), None);
        manager.open_add();
        let headers: Vec<String> = manager
            .form_all_lines(80)
            .into_iter()
            .filter(|line| line.text.starts_with("-- "))
            .map(|line| line.text)
            .collect();
        assert_eq!(
            headers,
            vec![
                "-- Identity --",
                "-- Launch --",
                "-- Appearance --",
                "-- Cursor --",
                "-- Effects --",
                "-- Layout --",
                "-- Switching --",
                "-- Actions --",
            ],
            "each section header appears exactly once, in order"
        );
    }

    #[test]
    fn command_program_row_is_not_duplicated() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&[]), None);
        manager.open_add();
        let count = manager
            .visible_form_fields()
            .iter()
            .filter(|field| matches!(field, FormField::CommandProgram))
            .count();
        assert_eq!(count, 1, "command program must appear exactly once");
    }

    #[test]
    fn shell_field_right_cycles_discovered_suggestions() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&[]), None);
        manager.open_add();
        focus_field(&mut manager, FormField::Shell);
        let before = manager.draft_shell.clone();
        manager.cycle_shell_suggestion(1);
        let after = manager.draft_shell.clone();
        assert_ne!(
            before, after,
            "cycling must apply the next discovered shell"
        );
        assert!(
            manager.shell_suggestion_hint().is_some(),
            "focused shell field shows a suggestion hint"
        );
    }

    #[test]
    fn structured_shell_suggestion_persists_as_profile_command() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&[]), None);
        manager.open_add();
        manager.draft_name = "wsl".to_owned();
        manager.apply_shell_suggestion(&DiscoveredShell {
            label: "WSL: Ubuntu".to_owned(),
            program: "wsl.exe".to_owned(),
            args: vec!["-d".to_owned(), "Ubuntu".to_owned()],
            kind: ShellKind::Wsl,
        });
        let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
            panic!("expected persist");
        };
        assert_eq!(
            profile
                .launch
                .command
                .as_ref()
                .map(|command| command.program.as_str()),
            Some("wsl.exe")
        );
        let command = profile.launch.command.expect("command");
        assert_eq!(command.args, vec!["-d".to_owned(), "Ubuntu".to_owned()]);
        assert!(profile.launch.shell.is_none());
    }

    #[test]
    fn create_edit_duplicate_rename_and_delete_flows() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&["dev"]), None);

        assert!(matches!(
            manager.open_add(),
            ProfileManagerOutcome::Consumed
        ));
        manager.draft_name = "work".to_owned();
        manager.draft_shell = "/bin/zsh".to_owned();
        let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
            panic!("expected persist");
        };
        assert_eq!(profile.name, "work");
        assert_eq!(profile.launch.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(replace, None);

        manager.open(catalog_with(&["dev", "work"]), None);
        manager.selected = 0;
        assert!(matches!(
            manager.open_duplicate_selected(),
            ProfileManagerOutcome::Consumed
        ));
        assert_eq!(manager.draft_name, "dev-copy");

        manager.open(catalog_with(&["dev"]), None);
        assert!(matches!(
            manager.open_rename_selected(),
            ProfileManagerOutcome::Consumed
        ));
        manager.draft_name = "edge".to_owned();
        let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
            panic!("expected rename persist");
        };
        assert_eq!(profile.name, "edge");
        assert_eq!(replace.as_deref(), Some("dev"));
    }

    #[test]
    fn env_editor_adds_rows_and_secret_entries_are_rejected_inline_not_dropped() {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default(), None);
        manager.open_add();
        manager.draft_name = "dev".to_owned();
        // Add one env row through the same activation the UI drives.
        focus_field(&mut manager, FormField::AddEnv);
        let _ = manager.activate_form_field(FormField::AddEnv);
        assert_eq!(manager.draft_env.len(), 1, "add row appends one env slot");
        manager.draft_env[0] = ("API_TOKEN".to_owned(), "nope".to_owned());

        assert!(matches!(
            manager.try_save(),
            ProfileManagerOutcome::Consumed
        ));
        assert!(
            manager
                .error
                .as_deref()
                .is_some_and(|error| error.contains("secret")),
            "secret env key must surface an inline error"
        );
        assert_eq!(
            manager.draft_env.len(),
            1,
            "the rejected entry stays in the draft; it is never silently dropped"
        );
        assert_eq!(manager.draft_env[0].0, "API_TOKEN");
    }

    #[test]
    fn env_editor_stops_at_the_bounded_limit_with_a_visible_message() {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default(), None);
        manager.open_add();
        for _ in 0..MAX_PROFILE_ENV_ENTRIES {
            let _ = manager.activate_form_field(FormField::AddEnv);
        }
        assert_eq!(manager.draft_env.len(), MAX_PROFILE_ENV_ENTRIES);
        let _ = manager.activate_form_field(FormField::AddEnv);
        assert_eq!(
            manager.draft_env.len(),
            MAX_PROFILE_ENV_ENTRIES,
            "the limit is not exceeded"
        );
        assert!(
            manager
                .error
                .as_deref()
                .is_some_and(|error| error.contains("limited to")),
            "hitting the limit shows a bounded message"
        );
    }

    #[test]
    fn tristate_toggles_cycle_inherit_on_off_and_persist() {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default(), None);
        manager.open_add();
        manager.draft_name = "fx".to_owned();
        assert_eq!(manager.draft_bloom, "inherit");
        let _ = manager.activate_form_field(FormField::Bloom); // -> on
        assert_eq!(manager.draft_bloom, "on");
        let _ = manager.activate_form_field(FormField::Bloom); // -> off
        assert_eq!(manager.draft_bloom, "off");
        let _ = manager.activate_form_field(FormField::Bloom); // -> inherit
        assert_eq!(manager.draft_bloom, "inherit");

        // Toggle crt to on and leave bloom at inherit; saving must not error.
        let _ = manager.activate_form_field(FormField::Crt);
        let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
            panic!("inherit tri-state must save cleanly, not error");
        };
        assert_eq!(profile.effects.bloom, None, "inherit maps to None");
        assert_eq!(profile.effects.crt, Some(true));
    }

    #[test]
    fn enumerated_fields_cycle_valid_values_only() {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default(), None);
        manager.open_add();
        manager.draft_name = "enum".to_owned();
        let _ = manager.activate_form_field(FormField::CursorStyle); // inherit -> block
        assert_eq!(manager.draft_cursor_style, "block");
        // Typing into a cycle field is a no-op: valid values only.
        focus_field(&mut manager, FormField::CursorStyle);
        let _ = manager.handle_input(OverlayInput::Char('z'));
        assert_eq!(manager.draft_cursor_style, "block");
        let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
            panic!("persist");
        };
        assert_eq!(profile.cursor.style.as_deref(), Some("block"));
    }

    #[test]
    fn external_palette_appearance_fields_round_trip_through_form() {
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.appearance.follow_external_palette = Some(true);
        profile.appearance.external_palette_provider = Some("colors_toml".to_owned());
        profile.appearance.external_palette_path = Some("/tmp/palette.toml".to_owned());
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), profile);
        let mut manager = ProfileManager::new();
        manager.open(catalog, None);
        let _ = manager.open_edit_selected();
        assert_eq!(manager.draft_follow_external_palette, "on");
        assert_eq!(manager.draft_external_palette_provider, "colors_toml");
        assert_eq!(manager.draft_external_palette_path, "/tmp/palette.toml");
        manager.draft_external_palette_path = "/tmp/edited.toml".to_owned();
        let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
            panic!("persist");
        };
        assert_eq!(profile.appearance.follow_external_palette, Some(true));
        assert_eq!(
            profile.appearance.external_palette_provider.as_deref(),
            Some("colors_toml")
        );
        assert_eq!(
            profile.appearance.external_palette_path.as_deref(),
            Some("/tmp/edited.toml")
        );
    }

    #[test]
    fn unknown_keys_survive_edit_draft() {
        let text = r#"{
  "schema_version": 1,
  "name": "dev",
  "future_flag": true,
  "launch": {
    "future_launch": 1,
    "command": {"program": "echo", "args": ["hi"], "future_command": true}
  },
  "appearance": {"future_appearance": "kept"},
  "cursor": {"future_cursor": false},
  "effects": {"future_effect": 0.5},
  "layout": {"future_layout": [1, 2]}
}"#;
        let profile = LaunchProfile::parse_json(text, Some("dev")).expect("parse");
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), profile);
        let mut manager = ProfileManager::new();
        manager.open(catalog, None);
        let _ = manager.open_edit_selected();
        manager.draft_title = "Dev".to_owned();
        let ProfileManagerOutcome::Persist { profile, .. } = manager.try_save() else {
            panic!("persist");
        };
        let serialized = profile.serialize_pretty();
        for key in [
            "future_flag",
            "future_launch",
            "future_command",
            "future_appearance",
            "future_cursor",
            "future_effect",
            "future_layout",
        ] {
            assert!(serialized.contains(&format!("\"{key}\"")), "{key} missing");
        }
        assert!(profile.preserved.contains_key("future_flag"));
        assert!(profile.launch.preserved.contains_key("future_launch"));
        assert!(
            profile
                .launch
                .command
                .as_ref()
                .expect("command")
                .preserved
                .contains_key("future_command")
        );
        assert!(
            profile
                .appearance
                .preserved
                .contains_key("future_appearance")
        );
        assert!(profile.cursor.preserved.contains_key("future_cursor"));
        assert!(profile.effects.preserved.contains_key("future_effect"));
        assert!(profile.layout.preserved.contains_key("future_layout"));
    }

    #[test]
    fn fully_populated_profile_round_trips_byte_for_byte_without_edits() {
        // Every section populated, including a hand-authored two-of-three
        // platform set that the single-token chooser cannot represent. Opening
        // and saving without touching the platform field must not collapse it.
        let text = r#"{
  "schema_version": 1,
  "name": "full",
  "display_name": "Full profile",
  "platforms": ["linux", "macos"],
  "future_top": {"kept": true},
  "launch": {
    "working_directory": "/srv/app",
    "command": {"program": "wsl.exe", "args": ["-d", "Ubuntu"], "future_cmd": 3},
    "env": {"EDITOR": "vi", "PAGER": "less"},
    "future_launch": [1, 2]
  },
  "appearance": {
    "theme": "odyssey-classic",
    "visual": "odyssey",
    "font": "berkeley",
    "font_family": "Berkeley Mono",
    "font_weight": "medium",
    "font_size_px": 13.5,
    "title": "Full",
    "follow_external_palette": false,
    "external_palette_provider": "colors_toml",
    "external_palette_path": "/tmp/p.toml",
    "future_look": "kept"
  },
  "cursor": {"style": "beam", "blink": "on", "future_cursor": 1},
  "effects": {"render_quality": "high", "bloom": true, "crt": false, "retro": true},
  "layout": {"saved_layout": "grid", "future_layout": 9},
  "connection": "prod-host",
  "switch": {"match_hosts": ["prod-*"], "match_directories": ["/srv/*"]}
}"#;
        let original = LaunchProfile::parse_json(text, Some("full")).expect("parse");
        let original_json = original.serialize_pretty();
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("full".to_owned(), original.clone());

        let mut manager = ProfileManager::new();
        manager.open(catalog, None);
        let _ = manager.open_edit_selected();

        // Every field is populated in the draft.
        assert_eq!(manager.draft_display_name, "Full profile");
        assert_eq!(manager.draft_working_directory, "/srv/app");
        assert_eq!(manager.draft_command_program, "wsl.exe");
        assert_eq!(manager.draft_command_args, vec!["-d", "Ubuntu"]);
        assert_eq!(manager.draft_env.len(), 2);
        assert_eq!(manager.draft_visual, "odyssey");
        assert_eq!(manager.draft_font_weight, "medium");
        assert_eq!(manager.draft_cursor_style, "beam");
        assert_eq!(manager.draft_render_quality, "high");
        assert_eq!(manager.draft_retro, "on");
        assert_eq!(manager.draft_saved_layout, "grid");
        assert_eq!(manager.draft_connection, "prod-host");
        assert_eq!(manager.draft_match_hosts, vec!["prod-*"]);

        // Save with no edits: the persisted JSON is byte-identical.
        let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
            panic!("persist");
        };
        assert_eq!(replace, None);
        assert_eq!(
            profile.serialize_pretty(),
            original_json,
            "an unedited save must reproduce the source document byte for byte"
        );
        // The two-of-three platform set survived intact.
        let platforms = profile.platforms.expect("platforms preserved");
        assert!(platforms.contains(&ProfilePlatform::Linux));
        assert!(platforms.contains(&ProfilePlatform::Macos));
        assert!(!platforms.contains(&ProfilePlatform::Windows));
    }

    #[test]
    fn invalid_name_surfaces_error_without_persist() {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default(), None);
        let _ = manager.open_add();
        manager.draft_name = "bad name!".to_owned();
        assert!(matches!(
            manager.try_save(),
            ProfileManagerOutcome::Consumed
        ));
        assert!(manager.error.is_some());
    }

    #[test]
    fn load_edit_save_reload_keeps_every_nested_preserved_map() {
        use crate::profiles::{profile_path_in_dir, read_profile_file, write_profile_file};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "odytty-profile-ui-preserved-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let text = r#"{
  "schema_version": 1,
  "name": "dev",
  "future_flag": true,
  "launch": {
    "shell": "/bin/zsh",
    "future_launch": {"kept": true},
    "command": {"program": "echo", "args": [], "future_command": 7}
  },
  "appearance": {"theme": "plain", "future_appearance": "kept"},
  "cursor": {"style": "block", "future_cursor": false},
  "effects": {"bloom": true, "future_effect": 0.25},
  "layout": {"saved_layout": "two", "future_layout": [9]}
}"#;
        let parsed = LaunchProfile::parse_json(text, Some("dev")).expect("parse");
        let path = profile_path_in_dir(&dir, "dev").expect("path");
        write_profile_file(&path, &parsed).expect("seed");

        let loaded = read_profile_file(&path, Some("dev")).expect("load");
        let mut catalog = ProfileCatalog::default();
        catalog.profiles.insert("dev".to_owned(), loaded);
        let mut manager = ProfileManager::new();
        manager.open(catalog, None);
        let _ = manager.open_edit_selected();
        manager.draft_title = "Edited".to_owned();
        let ProfileManagerOutcome::Persist { profile, replace } = manager.try_save() else {
            panic!("persist");
        };
        assert_eq!(replace, None);
        write_profile_file(&path, &profile).expect("save");
        let reloaded = read_profile_file(&path, Some("dev")).expect("reload");
        assert_eq!(reloaded.appearance.title.as_deref(), Some("Edited"));
        assert!(reloaded.preserved.contains_key("future_flag"));
        assert!(reloaded.launch.preserved.contains_key("future_launch"));
        assert!(
            reloaded
                .launch
                .command
                .as_ref()
                .expect("command")
                .preserved
                .contains_key("future_command")
        );
        assert!(
            reloaded
                .appearance
                .preserved
                .contains_key("future_appearance")
        );
        assert!(reloaded.cursor.preserved.contains_key("future_cursor"));
        assert!(reloaded.effects.preserved.contains_key("future_effect"));
        assert!(reloaded.layout.preserved.contains_key("future_layout"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

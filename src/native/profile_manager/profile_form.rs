// SPDX-License-Identifier: GPL-3.0-only
//! Sectioned add/edit form for the named-profile manager.
//!
//! Child of [`super`], so these `impl ProfileManager` blocks share the same
//! private draft fields declared on the struct there. The form exposes every
//! field the profile schema supports, grouped into Identity, Launch,
//! Appearance, Cursor, Effects, Layout, Switching, and Actions sections. Every
//! rendered line still carries its own [`ProfileManagerTarget`], so section
//! headers, the shell-suggestion hint, list rows, and validation errors cannot
//! shift a pointer target away from what is drawn.
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
        self.load_theme_suggestions();
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
        self.load_theme_suggestions();
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
        self.load_theme_suggestions();
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
                // Keyboard navigation resumes focus-following after a wheel pin.
                self.form_scroll_wheel_pinned = false;
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Down | OverlayInput::Tab => {
                if self.form_focus + 1 < fields.len() {
                    self.form_focus += 1;
                }
                self.form_scroll_wheel_pinned = false;
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Right => {
                match fields.get(self.form_focus) {
                    Some(FormField::Shell) => self.cycle_shell_suggestion(1),
                    Some(FormField::Theme) => self.cycle_theme_suggestion(1),
                    _ => {}
                }
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Left => {
                match fields.get(self.form_focus) {
                    Some(FormField::Shell) => self.cycle_shell_suggestion(-1),
                    Some(FormField::Theme) => self.cycle_theme_suggestion(-1),
                    _ => {}
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
                    crate::core::pop_text_owner(buf);
                });
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
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
                profile.launch.command = None;
                profile.launch.shell = nonempty_opt(&self.draft_shell);
            }
            profile.launch.working_directory = nonempty_opt(&self.draft_working_directory);
            // A non-empty starting directory the user set or changed this session
            // must name an existing directory, so a mistyped or removed path is
            // caught at save instead of dead-ending the tab at launch. A value
            // loaded unchanged from disk is preserved even if it is currently
            // missing (removable media, an unmounted share), so editing an
            // existing profile never destroys a field it did not touch; the
            // launch path falls back for a missing cwd regardless. Control
            // characters are still rejected by the schema round-trip below.
            let loaded_wd = self
                .draft_base
                .as_ref()
                .and_then(|base| base.launch.working_directory.as_deref());
            if let Some(dir) = profile.launch.working_directory.as_deref()
                && Some(dir) != loaded_wd
                && !crate::native::persistence::dir_exists_for_spawn(
                    std::path::Path::new(dir),
                    crate::cwd::DirectorySource::Explicit,
                )
            {
                self.error = Some(format!("working directory {dir:?} does not exist"));
                return ProfileManagerOutcome::Consumed;
            }
            profile.appearance.theme = nonempty_opt(&self.draft_theme);
            // A named theme the user set or changed this session must resolve to
            // a built-in or a user theme file, so an unknown name is caught here
            // instead of silently falling back to the global theme at launch. A
            // value loaded unchanged from disk round-trips even if its theme file
            // is momentarily absent, so an edit never destroys an untouched field.
            let loaded_theme = self
                .draft_base
                .as_ref()
                .and_then(|base| base.appearance.theme.as_deref());
            if let Some(theme) = profile.appearance.theme.as_deref()
                && Some(theme) != loaded_theme
                && !theme_name_is_known(theme)
            {
                self.error = Some(format!(
                    "unknown theme {theme:?}; use a built-in name or a theme file"
                ));
                return ProfileManagerOutcome::Consumed;
            }
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
            // intact so the user can fix it.
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
        self.pending_shell_command = None;
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
        self.theme_suggestions.clear();
        self.theme_suggestion_index = 0;
        self.pending_shell_command = None;
        self.form_focus = 0;
        self.form_scroll_offset.set(0);
        self.form_scroll_wheel_pinned = false;
    }

    fn load_shell_suggestions(&mut self) {
        #[cfg(test)]
        let shells = tests::fixture_shells().unwrap_or_else(shell_discovery::discovered_shells);
        #[cfg(not(test))]
        let shells = shell_discovery::discovered_shells();
        self.discovered_shells = shells;
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
        self.draft_command_program.clear();
        self.draft_command_args.clear();
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

    /// Build the Theme row's cycle roster on demand: sorted built-in theme names
    /// plus any user `.theme` file stems in the theme directory. Runs only when a
    /// profile form opens (never on the startup launch path), and its result is
    /// cached, so it never blocks the first usable terminal.
    fn load_theme_suggestions(&mut self) {
        #[cfg(test)]
        let themes = tests::fixture_themes().unwrap_or_else(discovered_theme_names);
        #[cfg(not(test))]
        let themes = discovered_theme_names();
        self.theme_suggestions = themes;
        // Anchor the index on the current draft value when it names a known theme
        // so the first Left/Right step is relative to what is shown.
        self.theme_suggestion_index = self
            .theme_suggestions
            .iter()
            .position(|name| name == self.draft_theme.trim())
            .unwrap_or(0);
    }

    fn cycle_theme_suggestion(&mut self, delta: isize) {
        if self.theme_suggestions.is_empty() {
            return;
        }
        let len = self.theme_suggestions.len();
        let next = (self.theme_suggestion_index as isize + delta).rem_euclid(len as isize);
        self.theme_suggestion_index = next as usize;
        self.draft_theme = self.theme_suggestions[self.theme_suggestion_index].clone();
        self.error = None;
    }

    fn theme_suggestion_hint(&self) -> Option<String> {
        if self.theme_suggestions.is_empty() {
            return None;
        }
        let name = &self.theme_suggestions[self.theme_suggestion_index];
        Some(format!(
            "Theme suggestions (Left/Right): {} ({}/{})",
            name,
            self.theme_suggestion_index + 1,
            self.theme_suggestions.len()
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
            FormField::Shell => &mut self.draft_shell,
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
        let before =
            matches!(field, FormField::Shell | FormField::CommandProgram).then(|| buffer.clone());
        edit(buffer);
        if before.as_ref().is_some_and(|before| buffer == before) {
            return;
        }
        // Switch exclusive launch choices only after a substantive edit.
        match field {
            FormField::Shell => {
                self.draft_command_program.clear();
                self.draft_command_args.clear();
                self.pending_shell_command = None;
                if let Some(base) = &mut self.draft_base {
                    base.launch.command = None;
                }
            }
            FormField::CommandProgram => {
                self.pending_shell_command = None;
                self.draft_shell.clear();
            }
            _ => {}
        }
        if field == FormField::CommandProgram && self.draft_command_program.trim().is_empty() {
            self.draft_command_args.clear();
        }
        self.error = None;
    }

    pub(super) fn form_visible_lines(
        &self,
        mode: FormMode,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ProfileManagerLine> {
        // Record the body height so a wheel scroll (which does not receive it)
        // can clamp against the current window.
        self.last_body_height.set(body_height);
        let lines = self.form_all_lines(body_width);
        if body_height == 0 || lines.len() <= body_height {
            self.form_scroll_offset.set(0);
            return lines;
        }
        let max_offset = lines.len() - body_height;
        let mut offset = self.form_scroll_offset.get().min(max_offset);
        // A wheel-pinned offset scrolls the window without dragging focus back
        // into view; keyboard focus moves clear the pin so navigation re-centers.
        if !self.form_scroll_wheel_pinned {
            let focused = lines.iter().position(|line| line.focused).unwrap_or(0);
            if focused < offset {
                offset = focused;
            } else if focused >= offset + body_height {
                offset = focused + 1 - body_height;
            }
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
            if focused
                && matches!(field, FormField::Theme)
                && let Some(hint) = self.theme_suggestion_hint()
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

/// The Theme row's cycle roster: sorted built-in theme names first, then any
/// user `.theme` file stems found in the theme directory (also sorted, and only
/// those not already shadowing a built-in name). Bounded to a single directory
/// read performed on demand when the form opens.
fn discovered_theme_names() -> Vec<String> {
    let mut builtins: Vec<String> = crate::theme::names().map(str::to_owned).collect();
    builtins.sort();
    let mut user: Vec<String> = crate::settings::theme_dir_path()
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("theme"))
                .then(|| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(str::to_owned)
                })
                .flatten()
        })
        .filter(|name| !builtins.iter().any(|b| b == name))
        .collect();
    user.sort();
    builtins.extend(user);
    builtins
}

/// Whether `name` resolves to a known theme: a built-in name or a user
/// `.theme`/theme file the launch resolver would accept. Save uses this so an
/// unknown theme is an inline error rather than a silent fallback at launch.
fn theme_name_is_known(name: &str) -> bool {
    crate::theme::Theme::from_name(name).is_some()
        || crate::settings::resolve_theme_file(name, crate::settings::theme_dir_path().as_deref())
            .is_some()
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
#[path = "profile_form_tests.rs"]
mod tests;

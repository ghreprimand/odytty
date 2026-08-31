// SPDX-License-Identifier: GPL-3.0-only
//! Named-profile manager overlay: catalog CRUD for settings UI.
//!
//! Presentation-only. The App loads a local catalog when opening this overlay,
//! persists Save/Delete/Import/Export outcomes, and never runs discovery on the
//! default launch path. Unknown future keys ride on the draft [`LaunchProfile`]
//! and survive edit/save via the schema round-trip.

use std::cell::Cell;
use std::collections::BTreeMap;

use crate::fuzzy;
use crate::profiles::{LaunchProfile, ProfileCatalog, validate_profile_name};

use super::overlay::OverlayInput;

const MAX_RESULTS: usize = 40;
const FOOTER_ROWS: usize = 2;
const ADD_ROW_LABEL: &str = "+ Add profile\u{2026}";
const KEY_HINT_LINE: &str =
    "Enter edit \u{b7} d duplicate \u{b7} r rename \u{b7} x delete \u{b7} i import \u{b7} e export";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormMode {
    Add,
    Edit,
    Duplicate,
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManagerView {
    Catalog,
    Form(FormMode),
    ConfirmDelete { name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormField {
    Name,
    DisplayName,
    Shell,
    WorkingDirectory,
    Theme,
    FontFamily,
    Title,
    Connection,
    Save,
    Cancel,
}

const FORM_FIELDS: &[FormField] = &[
    FormField::Name,
    FormField::DisplayName,
    FormField::Shell,
    FormField::WorkingDirectory,
    FormField::Theme,
    FormField::FontFamily,
    FormField::Title,
    FormField::Connection,
    FormField::Save,
    FormField::Cancel,
];

/// Outcomes the App must act on after the manager finishes a presentation step.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ProfileManagerOutcome {
    Consumed,
    Close,
    /// Persist `profile`. When `replace` is `Some`, delete that prior profile
    /// file after a successful write (rename / replace-edit).
    Persist {
        profile: Box<LaunchProfile>,
        replace: Option<String>,
    },
    Delete(String),
    RequestImport,
    RequestExport(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileManagerLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ProfileManagerSignature {
    view: u8,
    selected: usize,
    query: String,
    focus: usize,
    name: String,
    display_name: String,
    shell: String,
    working_directory: String,
    theme: String,
    font_family: String,
    title: String,
    connection: String,
    error: Option<String>,
    message: Option<String>,
    names: Vec<String>,
    confirm: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ProfileManager {
    profiles: BTreeMap<String, LaunchProfile>,
    warnings: Vec<String>,
    query: String,
    filtered: Vec<String>,
    selected: usize,
    scroll_offset: Cell<usize>,
    last_body_height: Cell<usize>,
    add_row_focused: bool,
    view: ManagerView,
    form_focus: usize,
    draft_name: String,
    draft_display_name: String,
    draft_shell: String,
    draft_working_directory: String,
    draft_theme: String,
    draft_font_family: String,
    draft_title: String,
    draft_connection: String,
    /// Full source profile so unknown nested keys survive an edit/save cycle.
    draft_base: Option<LaunchProfile>,
    /// Prior on-disk name for rename/edit-replace.
    replace_name: Option<String>,
    error: Option<String>,
    message: Option<String>,
}

impl Default for ProfileManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileManager {
    pub(super) fn new() -> Self {
        Self {
            profiles: BTreeMap::new(),
            warnings: Vec::new(),
            query: String::new(),
            filtered: Vec::new(),
            selected: 0,
            scroll_offset: Cell::new(0),
            last_body_height: Cell::new(0),
            add_row_focused: false,
            view: ManagerView::Catalog,
            form_focus: 0,
            draft_name: String::new(),
            draft_display_name: String::new(),
            draft_shell: String::new(),
            draft_working_directory: String::new(),
            draft_theme: String::new(),
            draft_font_family: String::new(),
            draft_title: String::new(),
            draft_connection: String::new(),
            draft_base: None,
            replace_name: None,
            error: None,
            message: None,
        }
    }

    /// Open (or reopen) with a freshly loaded local catalog. Never blocks on
    /// WSL/remote discovery; the App supplies only local files.
    pub(super) fn open(&mut self, catalog: ProfileCatalog) {
        self.profiles = catalog.profiles;
        self.warnings = catalog.warnings;
        self.query.clear();
        self.view = ManagerView::Catalog;
        self.add_row_focused = false;
        self.error = None;
        self.message = None;
        self.clear_draft();
        self.recompute_filter();
        self.selected = 0;
        self.scroll_offset.set(0);
    }

    pub(super) fn set_message(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    pub(super) fn title(&self) -> String {
        match &self.view {
            ManagerView::Catalog => "\u{2190} Named Profiles  (Esc = back)".to_owned(),
            ManagerView::Form(FormMode::Add) => "Add profile".to_owned(),
            ManagerView::Form(FormMode::Edit) => "Edit profile".to_owned(),
            ManagerView::Form(FormMode::Duplicate) => "Duplicate profile".to_owned(),
            ManagerView::Form(FormMode::Rename) => "Rename profile".to_owned(),
            ManagerView::ConfirmDelete { name } => format!("Delete profile {name}?"),
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        columns.saturating_sub(4).clamp(40, 72)
    }

    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        if !matches!(self.view, ManagerView::Catalog) {
            return (false, false);
        }
        let room = body_height.saturating_sub(FOOTER_ROWS + 2);
        self.last_body_height.set(body_height);
        let total = self.filtered.len();
        if total <= room || room == 0 {
            self.scroll_offset.set(0);
            return (false, false);
        }
        let selected = if self.add_row_focused {
            total.saturating_sub(1)
        } else {
            self.selected.min(total.saturating_sub(1))
        };
        let mut offset = self.scroll_offset.get().min(total.saturating_sub(room));
        if selected < offset {
            offset = selected;
        } else if selected >= offset + room {
            offset = selected + 1 - room;
        }
        self.scroll_offset.set(offset);
        (offset > 0, offset + room < total)
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ProfileManagerOutcome {
        match &self.view {
            ManagerView::Catalog => self.handle_catalog_input(input),
            ManagerView::Form(_) => self.handle_form_input(input),
            ManagerView::ConfirmDelete { .. } => self.handle_confirm_input(input),
        }
    }

    pub(super) fn handle_pointer_press(
        &mut self,
        _columns: usize,
        body_height: usize,
        row: usize,
        _col: usize,
    ) -> ProfileManagerOutcome {
        match self.view {
            ManagerView::Catalog => {
                let _ = self.scroll_indicator(body_height);
                let room = body_height.saturating_sub(FOOTER_ROWS + 2);
                let offset = self.scroll_offset.get();
                if row < room {
                    let index = offset + row;
                    if index < self.filtered.len() {
                        self.add_row_focused = false;
                        self.selected = index;
                        return self.open_edit_selected();
                    }
                } else if row == room {
                    return self.open_add();
                }
                ProfileManagerOutcome::Consumed
            }
            ManagerView::Form(_) => {
                let fields = self.visible_form_fields();
                if row < fields.len() {
                    self.form_focus = row;
                    if fields[row] == FormField::Save {
                        return self.try_save();
                    }
                    if fields[row] == FormField::Cancel {
                        self.return_to_catalog();
                    }
                }
                ProfileManagerOutcome::Consumed
            }
            ManagerView::ConfirmDelete { .. } => {
                if row == 0 {
                    self.confirm_delete()
                } else {
                    self.return_to_catalog();
                    ProfileManagerOutcome::Consumed
                }
            }
        }
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<ProfileManagerLine> {
        match &self.view {
            ManagerView::Catalog => self.catalog_lines(body_width, body_height),
            ManagerView::Form(mode) => self.form_lines(*mode, body_width),
            ManagerView::ConfirmDelete { name } => vec![
                ProfileManagerLine {
                    text: format!("Delete \u{201c}{name}\u{201d}? This cannot be undone."),
                    focused: false,
                    bold: true,
                },
                ProfileManagerLine {
                    text: "[Enter] Delete    [Esc] Cancel".to_owned(),
                    focused: true,
                    bold: false,
                },
            ],
        }
    }

    pub(super) fn render_signature(&self) -> ProfileManagerSignature {
        ProfileManagerSignature {
            view: match &self.view {
                ManagerView::Catalog => 0,
                ManagerView::Form(FormMode::Add) => 1,
                ManagerView::Form(FormMode::Edit) => 2,
                ManagerView::Form(FormMode::Duplicate) => 3,
                ManagerView::Form(FormMode::Rename) => 4,
                ManagerView::ConfirmDelete { .. } => 5,
            },
            selected: self.selected,
            query: self.query.clone(),
            focus: self.form_focus,
            name: self.draft_name.clone(),
            display_name: self.draft_display_name.clone(),
            shell: self.draft_shell.clone(),
            working_directory: self.draft_working_directory.clone(),
            theme: self.draft_theme.clone(),
            font_family: self.draft_font_family.clone(),
            title: self.draft_title.clone(),
            connection: self.draft_connection.clone(),
            error: self.error.clone(),
            message: self.message.clone(),
            names: self.filtered.clone(),
            confirm: match &self.view {
                ManagerView::ConfirmDelete { name } => Some(name.clone()),
                _ => None,
            },
        }
    }

    fn handle_catalog_input(&mut self, input: OverlayInput) -> ProfileManagerOutcome {
        match input {
            OverlayInput::Close => ProfileManagerOutcome::Close,
            OverlayInput::Up => {
                if self.add_row_focused {
                    self.add_row_focused = false;
                } else if self.selected > 0 {
                    self.selected -= 1;
                }
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Down => {
                if self.add_row_focused {
                    // stay
                } else if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                } else {
                    self.add_row_focused = true;
                }
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Activate if self.add_row_focused => self.open_add(),
            OverlayInput::Activate => self.open_edit_selected(),
            OverlayInput::Tab => self.open_add(),
            OverlayInput::Backspace => {
                self.query.pop();
                self.recompute_filter();
                ProfileManagerOutcome::Consumed
            }
            OverlayInput::Char(ch) => match ch {
                'd' | 'D' if self.query.is_empty() => self.open_duplicate_selected(),
                'r' | 'R' if self.query.is_empty() => self.open_rename_selected(),
                'x' | 'X' if self.query.is_empty() && !self.add_row_focused => {
                    self.open_confirm_delete_selected()
                }
                'i' | 'I' if self.query.is_empty() => ProfileManagerOutcome::RequestImport,
                'e' | 'E' if self.query.is_empty() => self.request_export_selected(),
                _ => {
                    self.query.push(ch);
                    self.recompute_filter();
                    ProfileManagerOutcome::Consumed
                }
            },
            _ => ProfileManagerOutcome::Consumed,
        }
    }

    fn handle_form_input(&mut self, input: OverlayInput) -> ProfileManagerOutcome {
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
            OverlayInput::Activate => match fields.get(self.form_focus).copied() {
                Some(FormField::Save) => self.try_save(),
                Some(FormField::Cancel) => {
                    self.return_to_catalog();
                    ProfileManagerOutcome::Consumed
                }
                _ => ProfileManagerOutcome::Consumed,
            },
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

    fn visible_form_fields(&self) -> Vec<FormField> {
        let rename_only = matches!(self.view, ManagerView::Form(FormMode::Rename));
        FORM_FIELDS
            .iter()
            .copied()
            .filter(|field| {
                !rename_only
                    || matches!(field, FormField::Name | FormField::Save | FormField::Cancel)
            })
            .collect()
    }

    fn handle_confirm_input(&mut self, input: OverlayInput) -> ProfileManagerOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('y') | OverlayInput::Char('Y') => {
                self.confirm_delete()
            }
            OverlayInput::Close | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                self.return_to_catalog();
                ProfileManagerOutcome::Consumed
            }
            _ => ProfileManagerOutcome::Consumed,
        }
    }

    fn confirm_delete(&mut self) -> ProfileManagerOutcome {
        let ManagerView::ConfirmDelete { name } = &self.view else {
            return ProfileManagerOutcome::Consumed;
        };
        let name = name.clone();
        self.return_to_catalog();
        ProfileManagerOutcome::Delete(name)
    }

    fn open_add(&mut self) -> ProfileManagerOutcome {
        self.clear_draft();
        self.view = ManagerView::Form(FormMode::Add);
        self.form_focus = 0;
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    fn open_edit_selected(&mut self) -> ProfileManagerOutcome {
        let Some(name) = self.selected_name() else {
            return ProfileManagerOutcome::Consumed;
        };
        let Some(profile) = self.profiles.get(&name).cloned() else {
            return ProfileManagerOutcome::Consumed;
        };
        self.load_draft_from(&profile);
        self.replace_name = Some(profile.name.clone());
        self.view = ManagerView::Form(FormMode::Edit);
        self.form_focus = 0;
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    fn open_duplicate_selected(&mut self) -> ProfileManagerOutcome {
        let Some(name) = self.selected_name() else {
            return ProfileManagerOutcome::Consumed;
        };
        let Some(profile) = self.profiles.get(&name).cloned() else {
            return ProfileManagerOutcome::Consumed;
        };
        self.load_draft_from(&profile);
        self.draft_name = unique_copy_name(&profile.name, &self.profiles);
        self.replace_name = None;
        self.view = ManagerView::Form(FormMode::Duplicate);
        self.form_focus = 0;
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    fn open_rename_selected(&mut self) -> ProfileManagerOutcome {
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
        self.error = None;
        ProfileManagerOutcome::Consumed
    }

    fn open_confirm_delete_selected(&mut self) -> ProfileManagerOutcome {
        let Some(name) = self.selected_name() else {
            return ProfileManagerOutcome::Consumed;
        };
        self.view = ManagerView::ConfirmDelete { name };
        ProfileManagerOutcome::Consumed
    }

    fn request_export_selected(&mut self) -> ProfileManagerOutcome {
        match self.selected_name() {
            Some(name) => ProfileManagerOutcome::RequestExport(name),
            None => ProfileManagerOutcome::Consumed,
        }
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
            profile.launch.shell = nonempty_opt(&self.draft_shell);
            profile.launch.working_directory = nonempty_opt(&self.draft_working_directory);
            profile.appearance.theme = nonempty_opt(&self.draft_theme);
            profile.appearance.font_family = nonempty_opt(&self.draft_font_family);
            profile.appearance.title = nonempty_opt(&self.draft_title);
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

    fn selected_name(&self) -> Option<String> {
        if self.add_row_focused {
            return None;
        }
        self.filtered.get(self.selected).cloned()
    }

    fn load_draft_from(&mut self, profile: &LaunchProfile) {
        self.draft_base = Some(profile.clone());
        self.draft_name = profile.name.clone();
        self.draft_display_name = profile.display_name.clone().unwrap_or_default();
        self.draft_shell = profile.launch.shell.clone().unwrap_or_default();
        self.draft_working_directory = profile.launch.working_directory.clone().unwrap_or_default();
        self.draft_theme = profile.appearance.theme.clone().unwrap_or_default();
        self.draft_font_family = profile.appearance.font_family.clone().unwrap_or_default();
        self.draft_title = profile.appearance.title.clone().unwrap_or_default();
        self.draft_connection = profile.connection.clone().unwrap_or_default();
    }

    fn clear_draft(&mut self) {
        self.draft_base = None;
        self.replace_name = None;
        self.draft_name.clear();
        self.draft_display_name.clear();
        self.draft_shell.clear();
        self.draft_working_directory.clear();
        self.draft_theme.clear();
        self.draft_font_family.clear();
        self.draft_title.clear();
        self.draft_connection.clear();
        self.form_focus = 0;
    }

    fn return_to_catalog(&mut self) {
        self.view = ManagerView::Catalog;
        self.clear_draft();
        self.error = None;
        self.recompute_filter();
    }

    fn recompute_filter(&mut self) {
        let names: Vec<String> = self.profiles.keys().cloned().collect();
        if self.query.is_empty() {
            self.filtered = names;
        } else {
            let ranked = fuzzy::rank(&self.query, &names);
            self.filtered = ranked
                .into_iter()
                .take(MAX_RESULTS)
                .filter_map(|(index, _)| names.get(index).cloned())
                .collect();
        }
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        if self.filtered.is_empty() {
            self.add_row_focused = true;
        }
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
            FormField::FontFamily => &mut self.draft_font_family,
            FormField::Title => &mut self.draft_title,
            FormField::Connection => &mut self.draft_connection,
            FormField::Save | FormField::Cancel => return,
        };
        edit(buffer);
        self.error = None;
    }

    fn catalog_lines(&self, body_width: usize, body_height: usize) -> Vec<ProfileManagerLine> {
        let mut lines = Vec::new();
        let query_label = if self.query.is_empty() {
            "Filter profiles\u{2026}".to_owned()
        } else {
            format!("Filter: {}", self.query)
        };
        lines.push(ProfileManagerLine {
            text: truncate(&query_label, body_width),
            focused: false,
            bold: false,
        });
        if let Some(warning) = self.warnings.first() {
            lines.push(ProfileManagerLine {
                text: truncate(warning, body_width),
                focused: false,
                bold: false,
            });
        } else if let Some(message) = &self.message {
            lines.push(ProfileManagerLine {
                text: truncate(message, body_width),
                focused: false,
                bold: false,
            });
        }

        let header = lines.len();
        let room = body_height.saturating_sub(FOOTER_ROWS + header);
        let _ = self.scroll_indicator(body_height);
        let offset = self.scroll_offset.get();
        if self.filtered.is_empty() {
            lines.push(ProfileManagerLine {
                text: "No profiles yet.".to_owned(),
                focused: false,
                bold: false,
            });
        } else {
            for (row, name) in self.filtered.iter().skip(offset).take(room).enumerate() {
                let absolute = offset + row;
                let label = match self
                    .profiles
                    .get(name)
                    .and_then(|p| p.display_name.as_deref())
                {
                    Some(display) if !display.is_empty() => format!("{name}  ({display})"),
                    _ => name.clone(),
                };
                lines.push(ProfileManagerLine {
                    text: truncate(&label, body_width),
                    focused: !self.add_row_focused && absolute == self.selected,
                    bold: true,
                });
            }
        }

        lines.push(ProfileManagerLine {
            text: ADD_ROW_LABEL.to_owned(),
            focused: self.add_row_focused,
            bold: self.add_row_focused,
        });
        lines.push(ProfileManagerLine {
            text: truncate(KEY_HINT_LINE, body_width),
            focused: false,
            bold: false,
        });
        lines
    }

    fn form_lines(&self, _mode: FormMode, body_width: usize) -> Vec<ProfileManagerLine> {
        let mut lines = Vec::new();
        let fields = self.visible_form_fields();
        for (row, field) in fields.iter().enumerate() {
            let focused = row == self.form_focus;
            let text = match field {
                FormField::Name => format!("Name: {}", self.draft_name),
                FormField::DisplayName => format!("Display name: {}", self.draft_display_name),
                FormField::Shell => format!("Shell: {}", self.draft_shell),
                FormField::WorkingDirectory => {
                    format!("Working directory: {}", self.draft_working_directory)
                }
                FormField::Theme => format!("Theme: {}", self.draft_theme),
                FormField::FontFamily => format!("Font family: {}", self.draft_font_family),
                FormField::Title => format!("Title: {}", self.draft_title),
                FormField::Connection => format!("Connection: {}", self.draft_connection),
                FormField::Save => "[Save]".to_owned(),
                FormField::Cancel => "[Cancel]".to_owned(),
            };
            lines.push(ProfileManagerLine {
                text: truncate(&text, body_width),
                focused,
                bold: matches!(field, FormField::Save | FormField::Cancel) || focused,
            });
        }
        if let Some(error) = &self.error {
            lines.push(ProfileManagerLine {
                text: truncate(error, body_width),
                focused: false,
                bold: false,
            });
        }
        lines
    }
}

fn nonempty_opt(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
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

fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (count, ch) in text.chars().enumerate() {
        if count + 1 >= width {
            out.push('\u{2026}');
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        text.chars().take(width).collect()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn create_edit_duplicate_rename_and_delete_flows() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&["dev"]));

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

        manager.open(catalog_with(&["dev", "work"]));
        manager.selected = 0;
        assert!(matches!(
            manager.open_duplicate_selected(),
            ProfileManagerOutcome::Consumed
        ));
        assert_eq!(manager.draft_name, "dev-copy");

        manager.open(catalog_with(&["dev"]));
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

        manager.open(catalog_with(&["edge"]));
        assert!(matches!(
            manager.open_confirm_delete_selected(),
            ProfileManagerOutcome::Consumed
        ));
        assert!(matches!(
            manager.handle_input(OverlayInput::Activate),
            ProfileManagerOutcome::Delete(name) if name == "edge"
        ));
    }

    #[test]
    fn validate_rejects_secret_env_before_persist() {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default());
        let _ = manager.open_add();
        manager.draft_name = "dev".to_owned();
        let mut base = LaunchProfile::new("dev").expect("profile");
        base.launch
            .env
            .insert("API_TOKEN".to_owned(), "nope".to_owned());
        manager.draft_base = Some(base);
        assert!(matches!(
            manager.try_save(),
            ProfileManagerOutcome::Consumed
        ));
        assert!(
            manager
                .error
                .as_deref()
                .is_some_and(|e| e.contains("secret"))
        );
    }

    #[test]
    fn import_and_export_requests_do_not_touch_disk() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&["dev"]));
        assert!(matches!(
            manager.handle_input(OverlayInput::Char('i')),
            ProfileManagerOutcome::RequestImport
        ));
        assert!(matches!(
            manager.handle_input(OverlayInput::Char('e')),
            ProfileManagerOutcome::RequestExport(name) if name == "dev"
        ));
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
        manager.open(catalog);
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
        manager.open(catalog);
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

    #[test]
    fn delete_requires_explicit_confirm_and_cancel_restores_catalog() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&["edge"]));
        assert!(matches!(
            manager.open_confirm_delete_selected(),
            ProfileManagerOutcome::Consumed
        ));
        // Stray input while confirming must not delete.
        assert!(matches!(
            manager.handle_input(OverlayInput::Char('z')),
            ProfileManagerOutcome::Consumed
        ));
        assert!(matches!(
            manager.handle_input(OverlayInput::Close),
            ProfileManagerOutcome::Consumed
        ));
        assert!(matches!(manager.view, ManagerView::Catalog));
        assert!(manager.profiles.contains_key("edge"));

        let _ = manager.open_confirm_delete_selected();
        assert!(matches!(
            manager.handle_input(OverlayInput::Activate),
            ProfileManagerOutcome::Delete(name) if name == "edge"
        ));
    }

    #[test]
    fn invalid_name_surfaces_error_without_persist() {
        let mut manager = ProfileManager::new();
        manager.open(ProfileCatalog::default());
        let _ = manager.open_add();
        manager.draft_name = "bad name!".to_owned();
        assert!(matches!(
            manager.try_save(),
            ProfileManagerOutcome::Consumed
        ));
        assert!(manager.error.is_some());
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Named-profile manager overlay: catalog CRUD for settings UI.
//!
//! Presentation-only. The App loads a local catalog when opening this overlay,
//! persists Save/Delete/Import/Export outcomes, and never runs discovery on the
//! default launch path. Unknown future keys ride on the draft [`LaunchProfile`]
//! and survive edit/save via the schema round-trip.
//!
//! The sectioned edit/add form is large enough to be its own concern, so its
//! draft state machine, field editing, and rendering live in the child module
//! [`profile_form`]. That module carries additional `impl ProfileManager`
//! blocks over the same private fields declared here; catalog listing, the
//! delete confirmation, and the shared line/target model stay in this file.

mod profile_form;

use std::cell::Cell;
use std::collections::BTreeMap;

use crate::fuzzy;
use crate::profiles::{DiscoveredShell, LaunchProfile, ProfileCatalog, ProfileCommand};

use super::overlay::OverlayInput;

const MAX_RESULTS: usize = 40;
const FOOTER_ROWS: usize = 2;
const ADD_ROW_LABEL: &str = "+ Add profile\u{2026}";
const KEY_HINT_LINE: &str = "Enter edit \u{b7} d duplicate \u{b7} r rename \u{b7} g set default \u{b7} x delete \u{b7} i import \u{b7} e export";

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

/// One editable or actionable field in the profile form.  List entries
/// (command args, env pairs, host/directory match rules) carry their own index
/// so a rendered row maps back to exactly one draft slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormField {
    Name,
    DisplayName,
    Platforms,
    Shell,
    WorkingDirectory,
    CommandProgram,
    CommandArg(usize),
    AddCommandArg,
    RemoveCommandArg(usize),
    EnvKey(usize),
    EnvValue(usize),
    AddEnv,
    RemoveEnv(usize),
    Theme,
    Visual,
    Font,
    FontFamily,
    FontWeight,
    FontSizePx,
    Title,
    FollowExternalPalette,
    ExternalPaletteProvider,
    ExternalPalettePath,
    CursorStyle,
    CursorBlink,
    RenderQuality,
    Bloom,
    Crt,
    Retro,
    SavedLayout,
    Connection,
    MatchHost(usize),
    AddMatchHost,
    RemoveMatchHost(usize),
    MatchDirectory(usize),
    AddMatchDirectory,
    RemoveMatchDirectory(usize),
    Save,
    Cancel,
}

impl FormField {
    /// True only for the closed-vocabulary fields (tri-states and enumerated
    /// cycles) whose activation cycles a value. Space activates these; on every
    /// free-text field space must type a literal space instead.
    pub(super) fn is_toggle_or_cycle(self) -> bool {
        matches!(
            self,
            FormField::Platforms
                | FormField::Visual
                | FormField::FontWeight
                | FormField::CursorStyle
                | FormField::CursorBlink
                | FormField::RenderQuality
                | FormField::FollowExternalPalette
                | FormField::Bloom
                | FormField::Crt
                | FormField::Retro
        )
    }
}

/// The pointer meaning of one rendered body line.  Keeping this beside the
/// presentation line makes row insertion (warnings, empty state, suggestions,
/// section headers, and validation errors) unable to shift a click target away
/// from what is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileManagerTarget {
    Inert,
    CatalogProfile(usize),
    Add,
    FormField(FormField),
    ConfirmButtons,
}

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
    /// Persist the selected profile as the global default launch profile.
    SetDefaultLaunchProfile(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileManagerLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
    target: ProfileManagerTarget,
}

/// Repaint change-detection signature for the manager.
///
/// Guard: the form signature is derived from the same `form_all_lines` text the
/// renderer draws, never from an enumerated list of draft fields. An earlier
/// enumerated signature omitted the fields added later (command, env, effects,
/// switch rules, ...), so typing into any of them produced no repaint until the
/// focus moved. Any new form field is covered automatically because it must
/// render through `form_field_text` to be visible at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ProfileManagerSignature {
    view: u8,
    selected: usize,
    add_row_focused: bool,
    query: String,
    global_default: Option<String>,
    warning: Option<String>,
    message: Option<String>,
    /// Catalog rows as drawn: name plus display name.
    catalog: Vec<(String, Option<String>)>,
    /// Form rows as drawn (text, focused, bold) at unbounded width.
    form: Vec<(String, bool, bool)>,
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
    form_scroll_offset: Cell<usize>,
    last_body_height: Cell<usize>,
    add_row_focused: bool,
    view: ManagerView,
    form_focus: usize,
    draft_name: String,
    draft_display_name: String,
    draft_shell: String,
    draft_working_directory: String,
    draft_theme: String,
    draft_follow_external_palette: String,
    draft_external_palette_provider: String,
    draft_external_palette_path: String,
    draft_font_family: String,
    draft_title: String,
    draft_connection: String,
    draft_command_program: String,
    draft_command_args: Vec<String>,
    draft_env: Vec<(String, String)>,
    draft_platforms: String,
    draft_visual: String,
    draft_font: String,
    draft_font_weight: String,
    draft_font_size_px: String,
    draft_cursor_style: String,
    draft_cursor_blink: String,
    draft_render_quality: String,
    draft_bloom: String,
    draft_crt: String,
    draft_retro: String,
    draft_saved_layout: String,
    draft_match_hosts: Vec<String>,
    draft_match_directories: Vec<String>,
    /// Cached shell suggestions loaded when a profile form opens (on demand).
    discovered_shells: Vec<DiscoveredShell>,
    shell_suggestion_index: usize,
    /// Structured WSL (or other argv) launch when a discovered row carries args.
    pending_shell_command: Option<ProfileCommand>,
    /// Full source profile so unknown nested keys survive an edit/save cycle.
    draft_base: Option<LaunchProfile>,
    /// Prior on-disk name for rename/edit-replace.
    replace_name: Option<String>,
    global_default: Option<String>,
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
            form_scroll_offset: Cell::new(0),
            last_body_height: Cell::new(0),
            add_row_focused: false,
            view: ManagerView::Catalog,
            form_focus: 0,
            draft_name: String::new(),
            draft_display_name: String::new(),
            draft_shell: String::new(),
            draft_working_directory: String::new(),
            draft_theme: String::new(),
            draft_follow_external_palette: "inherit".to_owned(),
            draft_external_palette_provider: String::new(),
            draft_external_palette_path: String::new(),
            draft_font_family: String::new(),
            draft_title: String::new(),
            draft_connection: String::new(),
            draft_command_program: String::new(),
            draft_command_args: Vec::new(),
            draft_env: Vec::new(),
            draft_platforms: "inherit".to_owned(),
            draft_visual: "inherit".to_owned(),
            draft_font: String::new(),
            draft_font_weight: "inherit".to_owned(),
            draft_font_size_px: String::new(),
            draft_cursor_style: "inherit".to_owned(),
            draft_cursor_blink: "inherit".to_owned(),
            draft_render_quality: "inherit".to_owned(),
            draft_bloom: "inherit".to_owned(),
            draft_crt: "inherit".to_owned(),
            draft_retro: "inherit".to_owned(),
            draft_saved_layout: String::new(),
            draft_match_hosts: Vec::new(),
            draft_match_directories: Vec::new(),
            discovered_shells: Vec::new(),
            shell_suggestion_index: 0,
            pending_shell_command: None,
            draft_base: None,
            replace_name: None,
            global_default: None,
            error: None,
            message: None,
        }
    }

    /// Open (or reopen) with a freshly loaded local catalog. Never blocks on
    /// WSL/remote discovery; the App supplies only local files.
    pub(super) fn open(&mut self, catalog: ProfileCatalog, global_default: Option<&str>) {
        self.profiles = catalog.profiles;
        self.warnings = catalog.warnings;
        self.global_default = global_default.map(str::to_owned);
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
        if matches!(self.view, ManagerView::Form(_)) {
            let total = self.form_all_lines(usize::MAX).len();
            let offset = self.form_scroll_offset.get();
            return (offset > 0, offset + body_height < total);
        }
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
        columns: usize,
        body_height: usize,
        row: usize,
        col: usize,
    ) -> ProfileManagerOutcome {
        let lines = self.visible_lines(columns, body_height);
        let clicked = lines.get(row);
        let target = clicked
            .map(|line| line.target)
            .unwrap_or(ProfileManagerTarget::Inert);
        let clicked_text = clicked.map(|line| line.text.clone()).unwrap_or_default();
        match target {
            ProfileManagerTarget::Inert => ProfileManagerOutcome::Consumed,
            ProfileManagerTarget::CatalogProfile(index) => {
                self.add_row_focused = false;
                self.selected = index;
                self.open_edit_selected()
            }
            ProfileManagerTarget::Add => self.open_add(),
            ProfileManagerTarget::FormField(field) => {
                let Some(index) = self.visible_form_fields().iter().position(|f| *f == field)
                else {
                    return ProfileManagerOutcome::Consumed;
                };
                self.form_focus = index;
                self.activate_form_field(field)
            }
            ProfileManagerTarget::ConfirmButtons => {
                // Derive the two button spans from the exact line the renderer
                // drew (`clicked_text`) rather than a re-typed literal, so the
                // hit-split can never drift from the visible label. `col` is a
                // display column; the label is ASCII so char offsets match.
                let esc = clicked_text.find("[Esc]");
                let enter = clicked_text.find("[Enter]");
                match (enter, esc) {
                    (_, Some(esc)) if col >= esc => {
                        self.return_to_catalog();
                        ProfileManagerOutcome::Consumed
                    }
                    (Some(enter), _) if col >= enter => self.confirm_delete(),
                    _ => ProfileManagerOutcome::Consumed,
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
            ManagerView::Form(mode) => self.form_visible_lines(*mode, body_width, body_height),
            ManagerView::ConfirmDelete { name } => vec![
                ProfileManagerLine {
                    text: format!("Delete \u{201c}{name}\u{201d}? This cannot be undone."),
                    focused: false,
                    bold: true,
                    target: ProfileManagerTarget::Inert,
                },
                ProfileManagerLine {
                    text: "[Enter] Delete    [Esc] Cancel".to_owned(),
                    focused: true,
                    bold: false,
                    target: ProfileManagerTarget::ConfirmButtons,
                },
            ],
        }
    }

    pub(super) fn render_signature(&self) -> ProfileManagerSignature {
        let form = match &self.view {
            ManagerView::Form(_) => self
                .form_all_lines(usize::MAX)
                .into_iter()
                .map(|line| (line.text, line.focused, line.bold))
                .collect(),
            _ => Vec::new(),
        };
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
            add_row_focused: self.add_row_focused,
            query: self.query.clone(),
            global_default: self.global_default.clone(),
            warning: self.warnings.first().cloned(),
            message: self.message.clone(),
            catalog: self
                .filtered
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        self.profiles
                            .get(name)
                            .and_then(|profile| profile.display_name.clone()),
                    )
                })
                .collect(),
            form,
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
                'g' | 'G' if self.query.is_empty() && !self.add_row_focused => {
                    self.set_default_selected()
                }
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

    fn set_default_selected(&mut self) -> ProfileManagerOutcome {
        let Some(name) = self.selected_name() else {
            return ProfileManagerOutcome::Consumed;
        };
        ProfileManagerOutcome::SetDefaultLaunchProfile(name)
    }

    fn selected_name(&self) -> Option<String> {
        if self.add_row_focused {
            return None;
        }
        self.filtered.get(self.selected).cloned()
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
            target: ProfileManagerTarget::Inert,
        });
        if let Some(warning) = self.warnings.first() {
            lines.push(ProfileManagerLine {
                text: truncate(warning, body_width),
                focused: false,
                bold: false,
                target: ProfileManagerTarget::Inert,
            });
        } else if let Some(message) = &self.message {
            lines.push(ProfileManagerLine {
                text: truncate(message, body_width),
                focused: false,
                bold: false,
                target: ProfileManagerTarget::Inert,
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
                target: ProfileManagerTarget::Inert,
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
                let marked = if self.global_default.as_deref() == Some(name.as_str()) {
                    format!("{label}  [default]")
                } else {
                    label
                };
                lines.push(ProfileManagerLine {
                    text: truncate(&marked, body_width),
                    focused: !self.add_row_focused && absolute == self.selected,
                    bold: true,
                    target: ProfileManagerTarget::CatalogProfile(absolute),
                });
            }
        }

        lines.push(ProfileManagerLine {
            text: ADD_ROW_LABEL.to_owned(),
            focused: self.add_row_focused,
            bold: self.add_row_focused,
            target: ProfileManagerTarget::Add,
        });
        lines.push(ProfileManagerLine {
            text: truncate(KEY_HINT_LINE, body_width),
            focused: false,
            bold: false,
            target: ProfileManagerTarget::Inert,
        });
        lines
    }
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
    fn import_and_export_requests_do_not_touch_disk() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&["dev"]), None);
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
    fn delete_requires_explicit_confirm_and_cancel_restores_catalog() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&["edge"]), None);
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
    fn catalog_pointer_press_on_the_add_row_opens_the_form() {
        let mut manager = ProfileManager::new();
        manager.open(catalog_with(&["dev"]), None);
        let lines = manager.visible_lines(80, 24);
        let add_row = lines
            .iter()
            .position(|line| line.text.contains("Add profile"))
            .expect("add row");
        assert!(matches!(
            manager.handle_pointer_press(80, 24, add_row, 0),
            ProfileManagerOutcome::Consumed
        ));
        assert_eq!(manager.title(), "Add profile");
    }
}

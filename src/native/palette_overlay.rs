// SPDX-License-Identifier: GPL-3.0-only
//! Native command-palette overlay state.
//!
//! The overlay is presentation state only: it owns a query, ranked row list,
//! and recent-directory cache, but it never writes to the PTY and never mutates
//! the terminal model. Accepting a row returns an outcome for the App to run
//! after the overlay closes.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::palette::{
    PaletteEntry, PaletteModel, PaletteOptions, PaletteSelection, PaletteSourceKind, SelectionWrap,
};
use crate::palette_catalog::compose_default_palette_entries;
use crate::palette_sources::{RecentDirs, read_history_for_shell};

use super::overlay::OverlayInput;

const MAX_RESULTS: usize = 40;

#[derive(Debug, Clone)]
pub(super) struct PaletteOverlay {
    model: PaletteModel,
    recent_dirs: RecentDirs,
    scroll_offset: Cell<usize>,
    last_body_height: Cell<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PaletteOverlayOutcome {
    Consumed,
    Close,
    TypeText(String),
    Action(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaletteOverlayLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaletteOverlaySignature {
    pub(super) query: String,
    pub(super) selected: Option<usize>,
    pub(super) results_len: usize,
    pub(super) results_fingerprint: u64,
}

impl Default for PaletteOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteOverlay {
    pub(super) fn new() -> Self {
        Self {
            model: PaletteModel::with_options(Vec::new(), palette_options()),
            recent_dirs: RecentDirs::default(),
            scroll_offset: Cell::new(0),
            last_body_height: Cell::new(0),
        }
    }

    pub(super) fn open_from_process_env(
        &mut self,
        cwd: Option<&str>,
        workspaces: &WorkspacePaletteContext<'_>,
    ) {
        let history = read_history_from_process_env();
        self.open_with_history_and_cwd(history, cwd, workspaces);
    }

    #[cfg(test)]
    pub(super) fn open_for_test<H, S>(&mut self, history: H, cwd: Option<&str>)
    where
        H: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.open_with_history_and_cwd(history, cwd, &WorkspacePaletteContext::names_only(&[]));
    }

    #[cfg(test)]
    pub(super) fn open_with_workspaces_for_test<H, S>(
        &mut self,
        history: H,
        cwd: Option<&str>,
        workspaces: &[String],
    ) where
        H: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.open_with_history_and_cwd(
            history,
            cwd,
            &WorkspacePaletteContext::names_only(workspaces),
        );
    }

    fn open_with_history_and_cwd<H, S>(
        &mut self,
        history: H,
        cwd: Option<&str>,
        workspaces: &WorkspacePaletteContext<'_>,
    ) where
        H: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.recent_dirs.observe_osc7_cwd(cwd);
        let directories = self.recent_dirs.candidates();
        let mut entries = compose_default_palette_entries(history, directories);
        entries.extend(workspace_palette_entries(workspaces));
        self.model = PaletteModel::with_options(entries, palette_options());
        self.reset_scroll();
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> PaletteOverlayOutcome {
        match input {
            OverlayInput::Close => PaletteOverlayOutcome::Close,
            OverlayInput::Up => {
                self.model.select_previous();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Down => {
                self.model.select_next();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.model.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
                self.model.move_selection(MAX_RESULTS as isize);
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.model.backspace_query();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.model.push_query_char(ch);
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                PaletteOverlayOutcome::Consumed
            }
            OverlayInput::Char(_) => PaletteOverlayOutcome::Consumed,
            OverlayInput::Activate => match self.model.selected_selection() {
                Some(PaletteSelection::Action { id }) => PaletteOverlayOutcome::Action(id),
                Some(PaletteSelection::TypeText { text, .. }) => {
                    PaletteOverlayOutcome::TypeText(text)
                }
                None => PaletteOverlayOutcome::Consumed,
            },
            OverlayInput::Left
            | OverlayInput::Right
            | OverlayInput::Save
            | OverlayInput::ActivateAlt
            | OverlayInput::Tab => PaletteOverlayOutcome::Consumed,
        }
    }

    /// Map a clicked body row to the result index it represents — the inverse of
    /// the [`Self::visible_lines`] windowing (UX4-P1 click→Activate). Row 0 is
    /// the `> query` prompt; results follow from the live `scroll_offset`.
    /// Returns `None` for the prompt row, the "No matches" hint, or a click past
    /// the last result.
    pub(super) fn row_at(&self, row_in_body: usize, body_height: usize) -> Option<usize> {
        if body_height == 0 || row_in_body == 0 {
            return None;
        }
        let results_len = self.model.results().len();
        if results_len == 0 {
            return None;
        }
        let visible_results = visible_result_rows(body_height);
        let within = row_in_body - 1;
        if within >= visible_results {
            return None;
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let index = scroll_offset + within;
        (index < results_len).then_some(index)
    }

    /// Select the result under a left-click, reporting whether it landed on a
    /// selectable row so the caller can route the existing Activate. Parity with
    /// Down×N + Activate by construction: it moves the model's selection cursor
    /// to the same index a Wheel/Down move would reach and re-follows the window.
    pub(super) fn click_row(&mut self, row_in_body: usize, body_height: usize) -> bool {
        let Some(target) = self.row_at(row_in_body, body_height) else {
            return false;
        };
        if let Some(current) = self.model.selected_index() {
            self.model
                .move_selection(target as isize - current as isize);
        }
        self.follow_selection_for_known_body_height();
        true
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<PaletteOverlayLine> {
        if body_height == 0 {
            self.last_body_height.set(0);
            self.scroll_offset.set(0);
            return Vec::new();
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2));
        lines.push(PaletteOverlayLine {
            text: truncate_for_width(&format!("> {}", self.model.query()), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        if self.model.results().is_empty() {
            self.scroll_offset.set(0);
            lines.push(PaletteOverlayLine {
                text: "No matches".to_owned(),
                focused: false,
                bold: false,
            });
            return lines;
        }
        let remaining = body_height - lines.len();
        for (visible_index, result) in self
            .model
            .results()
            .iter()
            .skip(scroll_offset)
            .take(remaining)
            .enumerate()
        {
            let index = scroll_offset + visible_index;
            let source = source_tag(result.kind);
            let label = sanitize_label(&result.label);
            lines.push(PaletteOverlayLine {
                text: truncate_for_width(&format!("{source}  {label}"), body_width),
                focused: self.model.selected_index() == Some(index),
                bold: result.kind == PaletteSourceKind::Action,
            });
        }
        lines
    }

    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        let visible_results = visible_result_rows(body_height);
        if visible_results == 0 || self.model.results().len() <= visible_results {
            self.scroll_offset.set(0);
            return (false, false);
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        (
            scroll_offset > 0,
            scroll_offset + visible_results < self.model.results().len(),
        )
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        columns.min(84)
    }

    pub(super) fn render_signature(&self) -> PaletteOverlaySignature {
        PaletteOverlaySignature {
            query: self.model.query().to_owned(),
            selected: self.model.selected_index(),
            results_len: self.model.results().len(),
            results_fingerprint: results_fingerprint(&self.model, self.scroll_offset.get()),
        }
    }

    fn reset_scroll(&self) {
        self.scroll_offset.set(0);
    }

    fn follow_selection_for_known_body_height(&self) {
        let body_height = self.last_body_height.get();
        if body_height > 0 {
            self.scroll_offset_for_body_height(body_height);
        }
    }

    fn scroll_offset_for_body_height(&self, body_height: usize) -> usize {
        self.last_body_height.set(body_height);
        let visible_results = visible_result_rows(body_height);
        let results_len = self.model.results().len();
        if visible_results == 0 || results_len <= visible_results {
            self.scroll_offset.set(0);
            return 0;
        }

        let max_scroll = results_len - visible_results;
        let mut scroll_offset = self.scroll_offset.get().min(max_scroll);
        if let Some(selected) = self.model.selected_index() {
            if selected < scroll_offset {
                scroll_offset = selected;
            } else if selected >= scroll_offset + visible_results {
                scroll_offset = selected + 1 - visible_results;
            }
        }
        self.scroll_offset.set(scroll_offset);
        scroll_offset
    }
}

fn palette_options() -> PaletteOptions {
    PaletteOptions {
        max_results: MAX_RESULTS,
        selection_wrap: SelectionWrap::Clamp,
    }
}

/// Stable id prefix for the per-workspace "switch to …" palette rows. The rail
/// index is appended (`workspace-switch-2`); [`parse_workspace_switch_id`]
/// recovers it, and the App routes it through `switch_to_workspace`.
pub(super) const WORKSPACE_SWITCH_ID_PREFIX: &str = "workspace-switch-";
/// Stable id for the "New Workspace" palette row.
pub(super) const WORKSPACE_NEW_ID: &str = "workspace-new";
/// Stable id for the "Rename Workspace" palette row (targets the active
/// workspace).
pub(super) const WORKSPACE_RENAME_ID: &str = "workspace-rename";
/// Stable id prefix for the F6-W5 "bind workspace to host …" rows. The host's
/// index in the known-host list is appended; [`parse_workspace_bind_id`]
/// recovers it and the App routes it through the binding mutator.
pub(super) const WORKSPACE_BIND_ID_PREFIX: &str = "workspace-bind-";
/// Stable id for the "Unbind Workspace From Host" row (only shown when the
/// active workspace is currently bound).
pub(super) const WORKSPACE_UNBIND_ID: &str = "workspace-unbind";
/// Stable id for the "New Local Tab" escape-hatch row (F6-W5): opens a local
/// shell even when the active workspace is bound to a host. Only shown when the
/// active workspace is bound (an unbound workspace's New Tab is already local).
pub(super) const WORKSPACE_NEW_LOCAL_TAB_ID: &str = "workspace-new-local-tab";
/// Stable id for the "Save Current Workspace as Layout" row (WP3 / 8g).
pub(super) const LAYOUT_SAVE_ID: &str = "layout-save";
/// Stable id prefix for the "Open Layout …" rows; the layout's index in the
/// saved-layout list is appended. [`parse_layout_open_id`] recovers it.
pub(super) const LAYOUT_OPEN_ID_PREFIX: &str = "layout-open-";
/// Stable id prefix for the "Delete Layout …" rows (WP3 / 8e).
pub(super) const LAYOUT_DELETE_ID_PREFIX: &str = "layout-delete-";

/// The workspace-facing context the command palette needs to build its rows:
/// the workspace names (switch rows, ODP-5), the known-host aliases (F6-W5 bind
/// rows), and which host — if any — the active workspace is currently bound to
/// (drives the unbind + new-local-tab escape rows and the "(bound)" marker).
pub(super) struct WorkspacePaletteContext<'a> {
    pub(super) names: &'a [String],
    pub(super) host_aliases: &'a [String],
    pub(super) bound_profile: Option<&'a str>,
    /// The names of saved layouts (WP3), for the open/delete rows. Empty until a
    /// layout has been saved.
    pub(super) layout_names: &'a [String],
}

impl<'a> WorkspacePaletteContext<'a> {
    /// A context with only workspace names — no host binding or layout surface.
    /// Used by the test seams that predate F6-W5 / WP3.
    #[cfg(test)]
    pub(super) fn names_only(names: &'a [String]) -> Self {
        Self {
            names,
            host_aliases: &[],
            bound_profile: None,
            layout_names: &[],
        }
    }
}

/// The workspace rows appended to the command palette: one "switch to …" row
/// per workspace in rail order, then create and rename rows (ODP-5), then the
/// F6-W5 host-binding surface — one "bind to …" row per known host, plus unbind
/// and "New Local Tab" escape rows when the active workspace is already bound.
/// Pure — returns action entries with stable ids the App dispatches after the
/// overlay closes.
fn workspace_palette_entries(ctx: &WorkspacePaletteContext<'_>) -> Vec<PaletteEntry> {
    let mut entries = Vec::with_capacity(ctx.names.len() + ctx.host_aliases.len() + 4);
    for (idx, name) in ctx.names.iter().enumerate() {
        entries.push(PaletteEntry::action(
            format!("{WORKSPACE_SWITCH_ID_PREFIX}{idx}"),
            format!("Workspace: {name}"),
        ));
    }
    entries.push(PaletteEntry::action(WORKSPACE_NEW_ID, "New Workspace"));
    entries.push(PaletteEntry::action(
        WORKSPACE_RENAME_ID,
        "Rename Workspace",
    ));
    for (idx, alias) in ctx.host_aliases.iter().enumerate() {
        let label = if ctx.bound_profile == Some(alias.as_str()) {
            format!("Workspace Host: {alias} (bound)")
        } else {
            format!("Bind Workspace to Host: {alias}")
        };
        entries.push(PaletteEntry::action(
            format!("{WORKSPACE_BIND_ID_PREFIX}{idx}"),
            label,
        ));
    }
    if ctx.bound_profile.is_some() {
        entries.push(PaletteEntry::action(
            WORKSPACE_UNBIND_ID,
            "Unbind Workspace From Host",
        ));
        entries.push(PaletteEntry::action(
            WORKSPACE_NEW_LOCAL_TAB_ID,
            "New Local Tab",
        ));
    }
    // WP3 named layouts: a save row, then open/delete rows per saved layout.
    entries.push(PaletteEntry::action(
        LAYOUT_SAVE_ID,
        "Save Current Workspace as Layout",
    ));
    for (idx, name) in ctx.layout_names.iter().enumerate() {
        entries.push(PaletteEntry::action(
            format!("{LAYOUT_OPEN_ID_PREFIX}{idx}"),
            format!("Open Layout: {name}"),
        ));
    }
    for (idx, name) in ctx.layout_names.iter().enumerate() {
        entries.push(PaletteEntry::action(
            format!("{LAYOUT_DELETE_ID_PREFIX}{idx}"),
            format!("Delete Layout: {name}"),
        ));
    }
    entries
}

/// Recover the layout index from an `layout-open-<idx>` action id.
pub(super) fn parse_layout_open_id(id: &str) -> Option<usize> {
    id.strip_prefix(LAYOUT_OPEN_ID_PREFIX)
        .and_then(|suffix| suffix.parse().ok())
}

/// Recover the layout index from a `layout-delete-<idx>` action id.
pub(super) fn parse_layout_delete_id(id: &str) -> Option<usize> {
    id.strip_prefix(LAYOUT_DELETE_ID_PREFIX)
        .and_then(|suffix| suffix.parse().ok())
}

/// Recover the rail index from a `workspace-switch-<idx>` action id, or `None`
/// when the id is not a workspace-switch row.
pub(super) fn parse_workspace_switch_id(id: &str) -> Option<usize> {
    id.strip_prefix(WORKSPACE_SWITCH_ID_PREFIX)
        .and_then(|suffix| suffix.parse().ok())
}

/// Recover the host index from a `workspace-bind-<idx>` action id, or `None`
/// when the id is not a host-binding row.
pub(super) fn parse_workspace_bind_id(id: &str) -> Option<usize> {
    id.strip_prefix(WORKSPACE_BIND_ID_PREFIX)
        .and_then(|suffix| suffix.parse().ok())
}

fn visible_result_rows(body_height: usize) -> usize {
    body_height.saturating_sub(1)
}

fn read_history_from_process_env() -> Vec<String> {
    let Some(shell) = env::var_os("SHELL").and_then(|value| value.into_string().ok()) else {
        return Vec::new();
    };
    let Some(home) = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let xdg_data_home = env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    read_history_for_shell(
        shell,
        home,
        xdg_config_home.as_deref(),
        xdg_data_home.as_deref(),
    )
}

fn source_tag(kind: PaletteSourceKind) -> &'static str {
    match kind {
        PaletteSourceKind::Action => "Action",
        PaletteSourceKind::History => "History",
        PaletteSourceKind::Directory => "Directory",
    }
}

fn results_fingerprint(model: &PaletteModel, scroll_offset: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    scroll_offset.hash(&mut hasher);
    for result in model.results().iter().take(MAX_RESULTS) {
        result.kind.hash(&mut hasher);
        result.label.hash(&mut hasher);
        result.entry_index.hash(&mut hasher);
    }
    hasher.finish()
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
}

fn truncate_for_width(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    for ch in text.chars().take(max_chars) {
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(history: &[&str], cwd: Option<&str>) -> PaletteOverlay {
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().copied(), cwd);
        overlay
    }

    fn type_query(overlay: &mut PaletteOverlay, query: &str) {
        for ch in query.chars() {
            assert_eq!(
                overlay.handle_input(OverlayInput::Char(ch)),
                PaletteOverlayOutcome::Consumed
            );
        }
    }

    fn command_history(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("qqq {index:02}")).collect()
    }

    /// F6-W5: the host-binding rows appear per known host, and the unbind +
    /// New Local Tab escape rows only show once the active workspace is bound.
    #[test]
    fn workspace_palette_entries_expose_host_binding() {
        let hosts = vec!["web".to_owned(), "db".to_owned()];
        let unbound = WorkspacePaletteContext {
            names: &["Workspace 1".to_owned()],
            host_aliases: &hosts,
            bound_profile: None,
            layout_names: &[],
        };
        let ids: Vec<String> = workspace_palette_entries(&unbound)
            .into_iter()
            .filter_map(|entry| match entry.selection() {
                PaletteSelection::Action { id } => Some(id),
                _ => None,
            })
            .collect();
        assert!(ids.contains(&"workspace-bind-0".to_owned()));
        assert!(ids.contains(&"workspace-bind-1".to_owned()));
        assert!(
            !ids.iter().any(|id| id == WORKSPACE_UNBIND_ID),
            "unbound workspace has no unbind row"
        );
        assert!(
            !ids.iter().any(|id| id == WORKSPACE_NEW_LOCAL_TAB_ID),
            "unbound workspace has no New Local Tab escape row"
        );

        let bound = WorkspacePaletteContext {
            names: &["Workspace 1".to_owned()],
            host_aliases: &hosts,
            bound_profile: Some("web"),
            layout_names: &[],
        };
        let labels: Vec<String> = workspace_palette_entries(&bound)
            .into_iter()
            .map(|entry| entry.label().to_owned())
            .collect();
        assert!(
            labels.iter().any(|l| l == "Workspace Host: web (bound)"),
            "the bound host is marked: {labels:?}"
        );
        assert!(labels.iter().any(|l| l == "Unbind Workspace From Host"));
        assert!(labels.iter().any(|l| l == "New Local Tab"));
    }

    #[test]
    fn fuzzy_query_ranks_history_candidate() {
        let mut overlay = open(&["git status", "cargo test"], Some("/workspace/service"));
        type_query(&mut overlay, "gst");

        let labels: Vec<_> = overlay
            .model
            .results()
            .iter()
            .map(|result| result.label.as_str())
            .collect();
        assert_eq!(labels.first().copied(), Some("git status"));
    }

    #[test]
    fn accept_history_types_text_without_newline() {
        let mut overlay = open(&["cargo test"], None);
        type_query(&mut overlay, "cargo");

        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            PaletteOverlayOutcome::TypeText("cargo test".to_owned())
        );
    }

    #[test]
    fn accept_action_returns_stable_action_id() {
        let mut overlay = open(&[], None);
        type_query(&mut overlay, "settings");

        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            PaletteOverlayOutcome::Action("settings".to_owned())
        );
    }

    fn open_with_workspaces(workspaces: &[&str]) -> PaletteOverlay {
        let names: Vec<String> = workspaces.iter().map(|s| (*s).to_owned()).collect();
        let mut overlay = PaletteOverlay::new();
        overlay.open_with_workspaces_for_test(std::iter::empty::<&str>(), None, &names);
        overlay
    }

    #[test]
    fn workspace_rows_include_switch_new_and_rename() {
        let mut overlay = open_with_workspaces(&["infra", "app"]);
        type_query(&mut overlay, "workspace");

        let labels: Vec<_> = overlay
            .model
            .results()
            .iter()
            .map(|result| result.label.as_str())
            .collect();
        assert!(labels.contains(&"Workspace: infra"), "labels: {labels:?}");
        assert!(labels.contains(&"Workspace: app"), "labels: {labels:?}");
        assert!(labels.contains(&"New Workspace"), "labels: {labels:?}");
        assert!(labels.contains(&"Rename Workspace"), "labels: {labels:?}");
    }

    #[test]
    fn accept_workspace_switch_row_returns_indexed_action_id() {
        let mut overlay = open_with_workspaces(&["infra", "app"]);
        // Second workspace's switch row carries the rail index in its id.
        type_query(&mut overlay, "app");

        let PaletteOverlayOutcome::Action(id) = overlay.handle_input(OverlayInput::Activate) else {
            panic!("workspace switch row must dispatch an action id");
        };
        assert_eq!(parse_workspace_switch_id(&id), Some(1));
    }

    #[test]
    fn workspace_switch_id_round_trips_only_for_switch_ids() {
        assert_eq!(parse_workspace_switch_id("workspace-switch-0"), Some(0));
        assert_eq!(parse_workspace_switch_id("workspace-switch-7"), Some(7));
        assert_eq!(parse_workspace_switch_id(WORKSPACE_NEW_ID), None);
        assert_eq!(parse_workspace_switch_id(WORKSPACE_RENAME_ID), None);
        assert_eq!(parse_workspace_switch_id("new-tab"), None);
    }

    #[test]
    fn no_workspace_switch_rows_when_no_names_but_create_still_offered() {
        let mut overlay = open_with_workspaces(&[]);
        type_query(&mut overlay, "workspace");
        let labels: Vec<_> = overlay
            .model
            .results()
            .iter()
            .map(|result| result.label.as_str())
            .collect();
        assert!(!labels.iter().any(|l| l.starts_with("Workspace:")));
        assert!(labels.contains(&"New Workspace"));
        assert!(labels.contains(&"Rename Workspace"));
    }

    #[test]
    fn recent_directories_survive_reopen() {
        let mut overlay = open(&[], Some("/work/one"));
        overlay.open_for_test(std::iter::empty::<&str>(), Some("/work/two"));
        type_query(&mut overlay, "/work");

        let labels: Vec<_> = overlay
            .model
            .results()
            .iter()
            .filter(|result| result.kind == PaletteSourceKind::Directory)
            .map(|result| result.label.as_str())
            .collect();
        assert_eq!(labels, vec!["/work/two", "/work/one"]);
    }

    #[test]
    fn visible_lines_are_bounded_by_body_height() {
        let overlay = open(&["one", "two", "three"], None);

        assert_eq!(overlay.visible_lines(80, 2).len(), 2);
    }

    #[test]
    fn visible_lines_follow_selection_when_body_overflows() {
        let history = command_history(10);
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().map(String::as_str), None);
        type_query(&mut overlay, "qqq");

        let before_lines = overlay.visible_lines(80, 4);
        let before_signature = overlay.render_signature();
        assert_eq!(before_lines[1].text, "History  qqq 00");
        assert!(before_lines[1].focused);

        for _ in 0..4 {
            assert_eq!(
                overlay.handle_input(OverlayInput::Down),
                PaletteOverlayOutcome::Consumed
            );
        }

        let after_lines = overlay.visible_lines(80, 4);
        let after_signature = overlay.render_signature();
        assert_ne!(after_lines, before_lines);
        assert_ne!(after_signature, before_signature);
        assert_ne!(
            after_signature.results_fingerprint,
            before_signature.results_fingerprint
        );
        assert_eq!(after_lines[1].text, "History  qqq 02");
        assert!(
            after_lines
                .iter()
                .any(|line| line.text == "History  qqq 04" && line.focused),
            "selected row must remain rendered after the view scrolls"
        );
    }

    #[test]
    fn scroll_indicator_is_inert_when_results_fit() {
        let history = command_history(3);
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().map(String::as_str), None);
        type_query(&mut overlay, "qqq");

        let before_signature = overlay.render_signature();
        let lines = overlay.visible_lines(80, 8);
        let after_signature = overlay.render_signature();

        assert_eq!(lines.len(), 4);
        assert_eq!(overlay.scroll_indicator(8), (false, false));
        assert_eq!(after_signature, before_signature);
    }

    #[test]
    fn scroll_indicator_reports_hidden_rows_after_selection_follow() {
        let history = command_history(10);
        let mut overlay = PaletteOverlay::new();
        overlay.open_for_test(history.iter().map(String::as_str), None);
        type_query(&mut overlay, "qqq");
        let _ = overlay.visible_lines(80, 4);

        for _ in 0..4 {
            assert_eq!(
                overlay.handle_input(OverlayInput::Down),
                PaletteOverlayOutcome::Consumed
            );
        }

        assert_eq!(overlay.scroll_indicator(4), (true, true));

        assert_eq!(
            overlay.handle_input(OverlayInput::End),
            PaletteOverlayOutcome::Consumed
        );

        assert_eq!(overlay.scroll_indicator(4), (true, false));
        assert_eq!(overlay.visible_lines(80, 4)[1].text, "History  qqq 07");
    }

    #[test]
    fn close_input_requests_close() {
        let mut overlay = open(&[], None);

        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            PaletteOverlayOutcome::Close
        );
    }

    // ── UX4-P1 click→Activate parity ───────────────────────────────────────

    #[test]
    fn click_row_selects_same_result_as_down_then_activate() {
        // A tall body so every result is visible; click body row N (1-based
        // after the prompt) must match Down×(N-1) + Activate.
        let history = command_history(5);
        for target in 0..5usize {
            let mut by_click = PaletteOverlay::new();
            by_click.open_for_test(history.iter().map(String::as_str), None);
            type_query(&mut by_click, "qqq");
            let _ = by_click.visible_lines(80, 12);
            assert!(
                by_click.click_row(target + 1, 12),
                "row {target} selectable"
            );
            let click_outcome = by_click.handle_input(OverlayInput::Activate);

            let mut by_keys = PaletteOverlay::new();
            by_keys.open_for_test(history.iter().map(String::as_str), None);
            type_query(&mut by_keys, "qqq");
            let _ = by_keys.visible_lines(80, 12);
            for _ in 0..target {
                by_keys.handle_input(OverlayInput::Down);
            }
            let key_outcome = by_keys.handle_input(OverlayInput::Activate);

            assert_eq!(click_outcome, key_outcome, "row {target} parity");
        }
    }

    #[test]
    fn click_query_prompt_and_empty_results_are_inert() {
        let mut overlay = open(&["cargo test"], None);
        type_query(&mut overlay, "cargo");
        let _ = overlay.visible_lines(80, 8);
        assert!(!overlay.click_row(0, 8)); // the `> query` prompt row
        // A query that matches nothing → the "No matches" hint, not a row.
        let mut empty = open(&["cargo test"], None);
        type_query(&mut empty, "zzzznope");
        let _ = empty.visible_lines(80, 8);
        assert!(!empty.click_row(1, 8));
    }
}

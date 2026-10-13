// SPDX-License-Identifier: GPL-3.0-only
//! Pure action catalog and source composer for the command palette.
//!
//! This module names the terminal-local actions dispatched by the native
//! palette. It does not execute actions, touch PTYs, read files, render an
//! overlay, or import native code. [`STABLE_ACTION_IDS`] is the complete stable
//! roster. The ids are also spelled out in [`DEFAULT_PALETTE_ACTIONS`],
//! [`PaletteAction::id`], [`PaletteAction::label`] and
//! [`PaletteAction::from_id`]; tests check the roster against the default
//! catalog and that every default action round-trips through its id.

use std::collections::HashSet;

use crate::palette::PaletteEntry;
use crate::palette_sources::{DEFAULT_HISTORY_MAX_ENTRIES, DEFAULT_SOURCE_ENTRY_MAX_CHARS};

/// Stable action id list, in default catalog order.
pub const STABLE_ACTION_IDS: &[&str] = &[
    "search",
    "settings",
    "theme-picker",
    "theme-from-colors",
    "copy",
    "paste",
    "scroll-up",
    "scroll-down",
    "jump-prompt-prev",
    "jump-prompt-next",
    "select-command-output",
    "select-command-with-prompt",
    "copy-command-output",
    "copy-command-with-prompt",
    "search-command-output",
    "jump-failed-command-prev",
    "jump-failed-command-next",
    "export-command-output",
    "export-scrollback-text",
    "export-scrollback-html",
    "notify-command-finished",
    "monitor-pane-activity",
    "monitor-pane-silence",
    "monitor-pane-bell",
    "monitor-pane-process-finish",
    "monitor-pane-command-failure",
    "clear-pane-monitors",
    "copy-mode",
    "hints",
    "clear-input",
    "toggle-read-only",
    "toggle-secure-input",
    "toggle-broadcast",
    "stop-broadcast",
    "new-tab",
    "close-tab",
    "next-tab",
    "prev-tab",
    "rename-tab",
    "split-pane-columns",
    "split-pane-rows",
    "focus-pane-left",
    "focus-pane-right",
    "focus-pane-up",
    "focus-pane-down",
    "focus-pane-next",
    "close-pane",
    "zoom-pane",
    "equalize-panes",
    "tile-panes",
    "stack-panes",
    "float-panes",
    "arrange-floating-pane",
    "session-navigator",
];

/// Terminal-local action exposed through the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteAction {
    Search,
    OpenSettings,
    OpenThemePicker,
    /// Capture the focused pane's live colors into a theme draft and open the
    /// theme editor on it.
    CreateThemeFromColors,
    CopySelection,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    JumpPromptPrev,
    JumpPromptNext,
    SelectCommandOutput,
    SelectCommandWithPrompt,
    CopyCommandOutput,
    CopyCommandWithPrompt,
    SearchCommandOutput,
    JumpFailedCommandPrev,
    JumpFailedCommandNext,
    ExportCommandOutput,
    /// Save the focused pane's scrollback and screen as plain text.
    ExportScrollbackText,
    /// Save the focused pane's scrollback and screen as sanitized HTML.
    ExportScrollbackHtml,
    NotifyCommandFinished,
    MonitorPaneActivity,
    MonitorPaneSilence,
    MonitorPaneBell,
    MonitorPaneProcessFinish,
    MonitorPaneCommandFailure,
    ClearPaneMonitors,
    CopyMode,
    Hints,
    ClearInput,
    /// Toggle the focused pane's read-only (input-disabled) mode.
    ToggleReadOnly,
    /// Toggle process-wide secure keyboard entry. Offered on macOS only.
    ToggleSecureInput,
    /// Add the focused pane to the broadcast receivers, or remove it.
    ToggleBroadcast,
    /// Empty the broadcast receiver set.
    StopBroadcast,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    RenameTab,
    SplitPaneColumns,
    SplitPaneRows,
    FocusPaneLeft,
    FocusPaneRight,
    FocusPaneUp,
    FocusPaneDown,
    FocusPaneNext,
    ClosePane,
    ZoomPane,
    EqualizePanes,
    /// Return the tab to the tiled split layout.
    TilePanes,
    /// Show one pane at a time; the focused pane fills the tab.
    StackPanes,
    /// Place the panes as overlapping rectangles inside the tab.
    FloatPanes,
    /// Move and resize the focused floating pane from the keyboard.
    ArrangeFloatingPane,
    SessionNavigator,
}

impl PaletteAction {
    /// Resolve a stable dispatch id.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "search" => Self::Search,
            "settings" => Self::OpenSettings,
            "theme-picker" => Self::OpenThemePicker,
            "theme-from-colors" => Self::CreateThemeFromColors,
            "copy" => Self::CopySelection,
            "paste" => Self::Paste,
            "scroll-up" => Self::ScrollPageUp,
            "scroll-down" => Self::ScrollPageDown,
            "jump-prompt-prev" => Self::JumpPromptPrev,
            "jump-prompt-next" => Self::JumpPromptNext,
            "select-command-output" => Self::SelectCommandOutput,
            "select-command-with-prompt" => Self::SelectCommandWithPrompt,
            "copy-command-output" => Self::CopyCommandOutput,
            "copy-command-with-prompt" => Self::CopyCommandWithPrompt,
            "search-command-output" => Self::SearchCommandOutput,
            "jump-failed-command-prev" => Self::JumpFailedCommandPrev,
            "jump-failed-command-next" => Self::JumpFailedCommandNext,
            "export-command-output" => Self::ExportCommandOutput,
            "export-scrollback-text" => Self::ExportScrollbackText,
            "export-scrollback-html" => Self::ExportScrollbackHtml,
            "notify-command-finished" => Self::NotifyCommandFinished,
            "monitor-pane-activity" => Self::MonitorPaneActivity,
            "monitor-pane-silence" => Self::MonitorPaneSilence,
            "monitor-pane-bell" => Self::MonitorPaneBell,
            "monitor-pane-process-finish" => Self::MonitorPaneProcessFinish,
            "monitor-pane-command-failure" => Self::MonitorPaneCommandFailure,
            "clear-pane-monitors" => Self::ClearPaneMonitors,
            "copy-mode" => Self::CopyMode,
            "hints" => Self::Hints,
            "clear-input" => Self::ClearInput,
            "toggle-read-only" => Self::ToggleReadOnly,
            "toggle-secure-input" => Self::ToggleSecureInput,
            "toggle-broadcast" => Self::ToggleBroadcast,
            "stop-broadcast" => Self::StopBroadcast,
            "new-tab" => Self::NewTab,
            "close-tab" => Self::CloseTab,
            "next-tab" => Self::NextTab,
            "prev-tab" => Self::PrevTab,
            "rename-tab" => Self::RenameTab,
            "split-pane-columns" => Self::SplitPaneColumns,
            "split-pane-rows" => Self::SplitPaneRows,
            "focus-pane-left" => Self::FocusPaneLeft,
            "focus-pane-right" => Self::FocusPaneRight,
            "focus-pane-up" => Self::FocusPaneUp,
            "focus-pane-down" => Self::FocusPaneDown,
            "focus-pane-next" => Self::FocusPaneNext,
            "close-pane" => Self::ClosePane,
            "zoom-pane" => Self::ZoomPane,
            "equalize-panes" => Self::EqualizePanes,
            "tile-panes" => Self::TilePanes,
            "stack-panes" => Self::StackPanes,
            "float-panes" => Self::FloatPanes,
            "arrange-floating-pane" => Self::ArrangeFloatingPane,
            "session-navigator" => Self::SessionNavigator,
            _ => return None,
        })
    }

    /// Stable dispatch id.
    pub fn id(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::OpenSettings => "settings",
            Self::OpenThemePicker => "theme-picker",
            Self::CreateThemeFromColors => "theme-from-colors",
            Self::CopySelection => "copy",
            Self::Paste => "paste",
            Self::ScrollPageUp => "scroll-up",
            Self::ScrollPageDown => "scroll-down",
            Self::JumpPromptPrev => "jump-prompt-prev",
            Self::JumpPromptNext => "jump-prompt-next",
            Self::SelectCommandOutput => "select-command-output",
            Self::SelectCommandWithPrompt => "select-command-with-prompt",
            Self::CopyCommandOutput => "copy-command-output",
            Self::CopyCommandWithPrompt => "copy-command-with-prompt",
            Self::SearchCommandOutput => "search-command-output",
            Self::JumpFailedCommandPrev => "jump-failed-command-prev",
            Self::JumpFailedCommandNext => "jump-failed-command-next",
            Self::ExportCommandOutput => "export-command-output",
            Self::ExportScrollbackText => "export-scrollback-text",
            Self::ExportScrollbackHtml => "export-scrollback-html",
            Self::NotifyCommandFinished => "notify-command-finished",
            Self::MonitorPaneActivity => "monitor-pane-activity",
            Self::MonitorPaneSilence => "monitor-pane-silence",
            Self::MonitorPaneBell => "monitor-pane-bell",
            Self::MonitorPaneProcessFinish => "monitor-pane-process-finish",
            Self::MonitorPaneCommandFailure => "monitor-pane-command-failure",
            Self::ClearPaneMonitors => "clear-pane-monitors",
            Self::CopyMode => "copy-mode",
            Self::Hints => "hints",
            Self::ClearInput => "clear-input",
            Self::ToggleReadOnly => "toggle-read-only",
            Self::ToggleSecureInput => "toggle-secure-input",
            Self::ToggleBroadcast => "toggle-broadcast",
            Self::StopBroadcast => "stop-broadcast",
            Self::NewTab => "new-tab",
            Self::CloseTab => "close-tab",
            Self::NextTab => "next-tab",
            Self::PrevTab => "prev-tab",
            Self::RenameTab => "rename-tab",
            Self::SplitPaneColumns => "split-pane-columns",
            Self::SplitPaneRows => "split-pane-rows",
            Self::FocusPaneLeft => "focus-pane-left",
            Self::FocusPaneRight => "focus-pane-right",
            Self::FocusPaneUp => "focus-pane-up",
            Self::FocusPaneDown => "focus-pane-down",
            Self::FocusPaneNext => "focus-pane-next",
            Self::ClosePane => "close-pane",
            Self::ZoomPane => "zoom-pane",
            Self::EqualizePanes => "equalize-panes",
            Self::TilePanes => "tile-panes",
            Self::StackPanes => "stack-panes",
            Self::FloatPanes => "float-panes",
            Self::ArrangeFloatingPane => "arrange-floating-pane",
            Self::SessionNavigator => "session-navigator",
        }
    }

    /// Human label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Search => "Search Scrollback",
            Self::OpenSettings => "Open Settings",
            Self::OpenThemePicker => "Open Theme Picker",
            Self::CreateThemeFromColors => "Create Theme From Current Colors",
            Self::CopySelection => "Copy Selection",
            Self::Paste => "Paste",
            Self::ScrollPageUp => "Scroll Page Up",
            Self::ScrollPageDown => "Scroll Page Down",
            Self::JumpPromptPrev => "Jump To Previous Prompt",
            Self::JumpPromptNext => "Jump To Next Prompt",
            Self::SelectCommandOutput => "Select Command Output",
            Self::SelectCommandWithPrompt => "Select Command With Prompt",
            Self::CopyCommandOutput => "Copy Command Output",
            Self::CopyCommandWithPrompt => "Copy Command With Prompt",
            Self::SearchCommandOutput => "Search Command Output",
            Self::JumpFailedCommandPrev => "Jump To Previous Failed Command",
            Self::JumpFailedCommandNext => "Jump To Next Failed Command",
            Self::ExportCommandOutput => "Export Command Output",
            Self::ExportScrollbackText => "Export Scrollback As Text",
            Self::ExportScrollbackHtml => "Export Scrollback As HTML",
            Self::NotifyCommandFinished => "Notify When This Command Finishes",
            Self::MonitorPaneActivity => "Monitor Pane Activity",
            Self::MonitorPaneSilence => "Monitor Pane Silence",
            Self::MonitorPaneBell => "Monitor Pane Bell",
            Self::MonitorPaneProcessFinish => "Monitor Pane Process Finish",
            Self::MonitorPaneCommandFailure => "Monitor Pane Command Failure",
            Self::ClearPaneMonitors => "Clear Pane Monitors",
            Self::CopyMode => "Enter Copy Mode",
            Self::Hints => "Open Hints",
            Self::ClearInput => "Clear Input",
            Self::ToggleReadOnly => "Toggle Read-Only Pane",
            Self::ToggleSecureInput => "Toggle Secure Keyboard Input",
            Self::ToggleBroadcast => "Broadcast to This Pane",
            Self::StopBroadcast => "Stop Broadcast",
            Self::NewTab => "New Tab",
            Self::CloseTab => "Close Tab",
            Self::NextTab => "Next Tab",
            Self::PrevTab => "Previous Tab",
            Self::RenameTab => "Rename Tab",
            Self::SplitPaneColumns => "Split Pane Into Columns",
            Self::SplitPaneRows => "Split Pane Into Rows",
            Self::FocusPaneLeft => "Focus Pane Left",
            Self::FocusPaneRight => "Focus Pane Right",
            Self::FocusPaneUp => "Focus Pane Up",
            Self::FocusPaneDown => "Focus Pane Down",
            Self::FocusPaneNext => "Focus Next Pane",
            Self::ClosePane => "Close Pane",
            Self::ZoomPane => "Zoom Pane",
            Self::EqualizePanes => "Equalize Panes",
            Self::TilePanes => "Tile Panes",
            Self::StackPanes => "Stack Panes",
            Self::FloatPanes => "Float Panes",
            Self::ArrangeFloatingPane => "Arrange Floating Pane",
            Self::SessionNavigator => "Open Session Navigator",
        }
    }

    /// Convert this action into a palette entry.
    pub fn entry(self) -> PaletteEntry {
        PaletteEntry::action(self.id(), self.label())
    }
}

/// Default action catalog, in stable display order.
pub const DEFAULT_PALETTE_ACTIONS: &[PaletteAction] = &[
    PaletteAction::Search,
    PaletteAction::OpenSettings,
    PaletteAction::OpenThemePicker,
    PaletteAction::CreateThemeFromColors,
    PaletteAction::CopySelection,
    PaletteAction::Paste,
    PaletteAction::ScrollPageUp,
    PaletteAction::ScrollPageDown,
    PaletteAction::JumpPromptPrev,
    PaletteAction::JumpPromptNext,
    PaletteAction::SelectCommandOutput,
    PaletteAction::SelectCommandWithPrompt,
    PaletteAction::CopyCommandOutput,
    PaletteAction::CopyCommandWithPrompt,
    PaletteAction::SearchCommandOutput,
    PaletteAction::JumpFailedCommandPrev,
    PaletteAction::JumpFailedCommandNext,
    PaletteAction::ExportCommandOutput,
    PaletteAction::ExportScrollbackText,
    PaletteAction::ExportScrollbackHtml,
    PaletteAction::NotifyCommandFinished,
    PaletteAction::MonitorPaneActivity,
    PaletteAction::MonitorPaneSilence,
    PaletteAction::MonitorPaneBell,
    PaletteAction::MonitorPaneProcessFinish,
    PaletteAction::MonitorPaneCommandFailure,
    PaletteAction::ClearPaneMonitors,
    PaletteAction::CopyMode,
    PaletteAction::Hints,
    PaletteAction::ClearInput,
    PaletteAction::ToggleReadOnly,
    PaletteAction::ToggleSecureInput,
    PaletteAction::ToggleBroadcast,
    PaletteAction::StopBroadcast,
    PaletteAction::NewTab,
    PaletteAction::CloseTab,
    PaletteAction::NextTab,
    PaletteAction::PrevTab,
    PaletteAction::RenameTab,
    PaletteAction::SplitPaneColumns,
    PaletteAction::SplitPaneRows,
    PaletteAction::FocusPaneLeft,
    PaletteAction::FocusPaneRight,
    PaletteAction::FocusPaneUp,
    PaletteAction::FocusPaneDown,
    PaletteAction::FocusPaneNext,
    PaletteAction::ClosePane,
    PaletteAction::ZoomPane,
    PaletteAction::EqualizePanes,
    PaletteAction::TilePanes,
    PaletteAction::StackPanes,
    PaletteAction::FloatPanes,
    PaletteAction::ArrangeFloatingPane,
    PaletteAction::SessionNavigator,
];

/// Bounds for composing already-read palette sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteCompositionLimits {
    /// Maximum history entries retained.
    pub max_history_entries: usize,
    /// Maximum directory entries retained.
    pub max_directory_entries: usize,
    /// Maximum characters retained per history/directory entry.
    pub max_entry_chars: usize,
}

impl Default for PaletteCompositionLimits {
    fn default() -> Self {
        Self {
            max_history_entries: DEFAULT_HISTORY_MAX_ENTRIES,
            max_directory_entries: DEFAULT_HISTORY_MAX_ENTRIES,
            max_entry_chars: DEFAULT_SOURCE_ENTRY_MAX_CHARS,
        }
    }
}

/// Build entries from the default action catalog plus bounded source rows.
pub fn compose_default_palette_entries<H, D, HS, DS>(
    history: H,
    directories: D,
) -> Vec<PaletteEntry>
where
    H: IntoIterator<Item = HS>,
    D: IntoIterator<Item = DS>,
    HS: AsRef<str>,
    DS: AsRef<str>,
{
    #[cfg(target_os = "macos")]
    {
        compose_palette_entries(
            DEFAULT_PALETTE_ACTIONS,
            history,
            directories,
            PaletteCompositionLimits::default(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        // The action stays in the stable catalog so matches stay exhaustive.
        // Windows and Linux do not register it: there is no secure-input
        // primitive, and the mode is never simulated.
        let actions: Vec<PaletteAction> = DEFAULT_PALETTE_ACTIONS
            .iter()
            .copied()
            .filter(|action| *action != PaletteAction::ToggleSecureInput)
            .collect();
        compose_palette_entries(
            &actions,
            history,
            directories,
            PaletteCompositionLimits::default(),
        )
    }
}

/// Build entries from action, history, and directory sources.
///
/// History and directory inputs are expected to be most-recent-first when that
/// is the desired tie-break. Empty rows are dropped, repeated rows are kept only
/// at their first occurrence, and each source is bounded independently.
pub fn compose_palette_entries<H, D, HS, DS>(
    actions: &[PaletteAction],
    history: H,
    directories: D,
    limits: PaletteCompositionLimits,
) -> Vec<PaletteEntry>
where
    H: IntoIterator<Item = HS>,
    D: IntoIterator<Item = DS>,
    HS: AsRef<str>,
    DS: AsRef<str>,
{
    let mut entries = action_entries(actions);
    entries.extend(history_entries(history, limits));
    entries.extend(directory_entries(directories, limits));
    entries
}

/// Build one deduplicated action entry per stable action id.
pub fn action_entries(actions: &[PaletteAction]) -> Vec<PaletteEntry> {
    let mut seen = HashSet::new();
    actions
        .iter()
        .copied()
        .filter(|action| seen.insert(action.id()))
        .map(PaletteAction::entry)
        .collect()
}

fn history_entries<I, S>(history: I, limits: PaletteCompositionLimits) -> Vec<PaletteEntry>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    bounded_unique_entries(history, limits.max_history_entries, limits.max_entry_chars)
        .into_iter()
        .map(PaletteEntry::history)
        .collect()
}

fn directory_entries<I, S>(directories: I, limits: PaletteCompositionLimits) -> Vec<PaletteEntry>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    bounded_unique_entries(
        directories,
        limits.max_directory_entries,
        limits.max_entry_chars,
    )
    .into_iter()
    .map(PaletteEntry::directory)
    .collect()
}

fn bounded_unique_entries<I, S>(
    values: I,
    max_entries: usize,
    max_entry_chars: usize,
) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if max_entries == 0 || max_entry_chars == 0 {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        let Some(entry) = normalize_entry(value.as_ref(), max_entry_chars) else {
            continue;
        };
        if !seen.insert(entry.clone()) {
            continue;
        }
        out.push(entry);
        if out.len() >= max_entries {
            break;
        }
    }
    out
}

fn normalize_entry(raw: &str, max_entry_chars: usize) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(max_entry_chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{PaletteModel, PaletteSelection, PaletteSourceKind};

    fn labels(entries: &[PaletteEntry]) -> Vec<&str> {
        entries.iter().map(PaletteEntry::label).collect()
    }

    fn action_ids(actions: &[PaletteAction]) -> Vec<&'static str> {
        actions.iter().copied().map(PaletteAction::id).collect()
    }

    #[test]
    fn stable_action_id_list_matches_default_catalog_and_is_unique() {
        assert_eq!(action_ids(DEFAULT_PALETTE_ACTIONS), STABLE_ACTION_IDS);

        let mut seen = HashSet::new();
        for id in STABLE_ACTION_IDS {
            assert!(seen.insert(*id), "duplicate action id: {id}");
            assert!(!id.is_empty());
            assert!(
                id.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
            );
        }
    }

    #[test]
    fn catalog_actions_carry_ids_and_labels() {
        assert_eq!(PaletteAction::SplitPaneColumns.id(), "split-pane-columns");
        assert_eq!(
            PaletteAction::SplitPaneColumns.label(),
            "Split Pane Into Columns"
        );
    }

    #[test]
    fn every_default_action_round_trips_through_its_id() {
        for action in DEFAULT_PALETTE_ACTIONS {
            assert_eq!(PaletteAction::from_id(action.id()), Some(*action));
        }
    }

    #[test]
    fn action_entries_deduplicate_by_stable_id() {
        let entries = action_entries(&[
            PaletteAction::Search,
            PaletteAction::Search,
            PaletteAction::OpenSettings,
        ]);

        assert_eq!(labels(&entries), vec!["Search Scrollback", "Open Settings"]);
    }

    #[test]
    fn composer_orders_actions_then_history_then_directories() {
        let entries = compose_palette_entries(
            &[PaletteAction::Search, PaletteAction::NewTab],
            ["git status", "cargo test"],
            ["/work/new", "/work/old"],
            PaletteCompositionLimits::default(),
        );

        assert_eq!(
            labels(&entries),
            vec![
                "Search Scrollback",
                "New Tab",
                "git status",
                "cargo test",
                "/work/new",
                "/work/old"
            ]
        );
    }

    #[test]
    fn composer_bounds_deduplicates_and_truncates_sources_independently() {
        let limits = PaletteCompositionLimits {
            max_history_entries: 2,
            max_directory_entries: 1,
            max_entry_chars: 5,
        };

        let entries = compose_palette_entries(
            &[],
            [" alpha ", "alpha", "bravo", "charlie"],
            [" /abcdef ", "/ghijk"],
            limits,
        );

        assert_eq!(labels(&entries), vec!["alpha", "bravo", "/abcd"]);
    }

    #[test]
    fn zero_limits_drop_history_and_directories_but_keep_actions() {
        let limits = PaletteCompositionLimits {
            max_history_entries: 0,
            max_directory_entries: 0,
            max_entry_chars: 128,
        };

        let entries = compose_palette_entries(
            &[PaletteAction::Paste],
            ["git status"],
            ["/work/project"],
            limits,
        );

        assert_eq!(labels(&entries), vec!["Paste"]);
    }

    #[test]
    fn composed_entries_feed_palette_model_with_action_and_literal_selections() {
        let entries = compose_palette_entries(
            &[PaletteAction::ZoomPane],
            ["cargo test"],
            ["/work/project"],
            PaletteCompositionLimits::default(),
        );
        let mut model = PaletteModel::new(entries);

        model.set_query("zoom");
        assert_eq!(
            model.selected_selection(),
            Some(PaletteSelection::Action {
                id: "zoom-pane".to_owned()
            })
        );

        model.set_query("cargo");
        assert_eq!(
            model.selected_selection(),
            Some(PaletteSelection::TypeText {
                text: "cargo test".to_owned(),
                source: PaletteSourceKind::History,
            })
        );
    }

    #[test]
    fn default_composer_includes_pane_actions_and_sources() {
        let entries = compose_default_palette_entries(["git status"], ["/work/project"]);
        let labels = labels(&entries);

        assert!(labels.contains(&"Split Pane Into Columns"));
        assert!(labels.contains(&"Split Pane Into Rows"));
        assert!(labels.contains(&"Focus Pane Left"));
        assert!(labels.contains(&"git status"));
        assert!(labels.contains(&"/work/project"));
    }
}

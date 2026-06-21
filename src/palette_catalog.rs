// SPDX-License-Identifier: GPL-3.0-only
//! Pure action catalog and source composer for the command palette.
//!
//! This module names the terminal-local actions that the future native palette
//! can dispatch. It does not execute actions, touch PTYs, read files, render an
//! overlay, or import native code.
//!
//! Stable action ids:
//! `search`, `settings`, `theme-picker`, `copy`, `paste`, `scroll-up`,
//! `scroll-down`, `jump-prompt-prev`, `jump-prompt-next`, `copy-mode`, `hints`,
//! `clear-input`, `new-tab`, `close-tab`, `next-tab`, `prev-tab`, `rename-tab`,
//! `split-pane-columns`, `split-pane-rows`, `focus-pane-left`,
//! `focus-pane-right`, `focus-pane-up`, `focus-pane-down`, `focus-pane-next`,
//! `close-pane`, `zoom-pane`, `equalize-panes`.

use std::collections::HashSet;

use crate::palette::PaletteEntry;
use crate::palette_sources::{DEFAULT_HISTORY_MAX_ENTRIES, DEFAULT_SOURCE_ENTRY_MAX_CHARS};

/// Stable action id list, in default catalog order.
pub const STABLE_ACTION_IDS: &[&str] = &[
    "search",
    "settings",
    "theme-picker",
    "copy",
    "paste",
    "scroll-up",
    "scroll-down",
    "jump-prompt-prev",
    "jump-prompt-next",
    "copy-mode",
    "hints",
    "clear-input",
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
];

/// Terminal-local action exposed through the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteAction {
    Search,
    OpenSettings,
    OpenThemePicker,
    CopySelection,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    JumpPromptPrev,
    JumpPromptNext,
    CopyMode,
    Hints,
    ClearInput,
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
}

impl PaletteAction {
    /// Stable dispatch id.
    pub fn id(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::OpenSettings => "settings",
            Self::OpenThemePicker => "theme-picker",
            Self::CopySelection => "copy",
            Self::Paste => "paste",
            Self::ScrollPageUp => "scroll-up",
            Self::ScrollPageDown => "scroll-down",
            Self::JumpPromptPrev => "jump-prompt-prev",
            Self::JumpPromptNext => "jump-prompt-next",
            Self::CopyMode => "copy-mode",
            Self::Hints => "hints",
            Self::ClearInput => "clear-input",
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
        }
    }

    /// Human label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Search => "Search Scrollback",
            Self::OpenSettings => "Open Settings",
            Self::OpenThemePicker => "Open Theme Picker",
            Self::CopySelection => "Copy Selection",
            Self::Paste => "Paste",
            Self::ScrollPageUp => "Scroll Page Up",
            Self::ScrollPageDown => "Scroll Page Down",
            Self::JumpPromptPrev => "Jump To Previous Prompt",
            Self::JumpPromptNext => "Jump To Next Prompt",
            Self::CopyMode => "Enter Copy Mode",
            Self::Hints => "Open Hints",
            Self::ClearInput => "Clear Input",
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
        }
    }

    /// Search aliases/keywords for future palette ranking.
    ///
    /// The current `PaletteEntry` stores one match label, so the composer emits
    /// one human-label action row per action and preserves these aliases as the
    /// pure dispatch contract for the native/model wiring packet.
    pub fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Search => &["find", "toggle search", "scrollback search"],
            Self::OpenSettings => &["preferences", "prefs", "config"],
            Self::OpenThemePicker => &["theme", "themes", "choose theme"],
            Self::CopySelection => &["copy", "clipboard", "selection"],
            Self::Paste => &["paste", "clipboard"],
            Self::ScrollPageUp => &["page up", "scrollback up"],
            Self::ScrollPageDown => &["page down", "scrollback down"],
            Self::JumpPromptPrev => &["previous prompt", "prompt up", "shell mark"],
            Self::JumpPromptNext => &["next prompt", "prompt down", "shell mark"],
            Self::CopyMode => &["keyboard selection", "select mode"],
            Self::Hints => &["quick select", "links", "paths"],
            Self::ClearInput => &["clear line", "kill line", "readline"],
            Self::NewTab => &["tab new", "create tab"],
            Self::CloseTab => &["tab close", "delete tab"],
            Self::NextTab => &["tab next", "right tab"],
            Self::PrevTab => &["previous tab", "tab prev", "left tab"],
            Self::RenameTab => &["tab title", "custom title"],
            Self::SplitPaneColumns => &[
                "split horizontal",
                "split right",
                "tmux split-window -h",
                "columns",
            ],
            Self::SplitPaneRows => &[
                "split vertical",
                "split down",
                "tmux split-window -v",
                "rows",
            ],
            Self::FocusPaneLeft => &["pane left", "tmux left"],
            Self::FocusPaneRight => &["pane right", "tmux right"],
            Self::FocusPaneUp => &["pane up", "tmux up"],
            Self::FocusPaneDown => &["pane down", "tmux down"],
            Self::FocusPaneNext => &["next pane", "tmux o"],
            Self::ClosePane => &["kill pane", "tmux x"],
            Self::ZoomPane => &["toggle pane zoom", "fullscreen pane", "tmux z"],
            Self::EqualizePanes => &["balance panes", "even panes", "tmux ="],
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
    PaletteAction::CopySelection,
    PaletteAction::Paste,
    PaletteAction::ScrollPageUp,
    PaletteAction::ScrollPageDown,
    PaletteAction::JumpPromptPrev,
    PaletteAction::JumpPromptNext,
    PaletteAction::CopyMode,
    PaletteAction::Hints,
    PaletteAction::ClearInput,
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
    compose_palette_entries(
        DEFAULT_PALETTE_ACTIONS,
        history,
        directories,
        PaletteCompositionLimits::default(),
    )
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
    fn catalog_actions_carry_labels_and_search_aliases() {
        assert_eq!(PaletteAction::SplitPaneColumns.id(), "split-pane-columns");
        assert_eq!(
            PaletteAction::SplitPaneColumns.label(),
            "Split Pane Into Columns"
        );
        assert!(
            PaletteAction::SplitPaneColumns
                .aliases()
                .contains(&"tmux split-window -h")
        );
        assert!(
            PaletteAction::OpenSettings
                .aliases()
                .contains(&"preferences")
        );
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

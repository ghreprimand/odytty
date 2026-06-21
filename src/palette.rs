// SPDX-License-Identifier: GPL-3.0-only
//! Presentation-agnostic command palette model.
//!
//! This module owns the headless command-palette state: query editing,
//! candidate ranking, result bounding, cursor movement, and selection decisions.
//! It deliberately has no overlay, native-window, GPU, PTY, or filesystem
//! dependencies. Callers feed it already-bounded action/history/directory
//! candidates, including values produced by `palette_sources`.
//!
//! Ranking policy:
//! - Empty queries show candidates by source priority, then input order.
//! - Non-empty queries run the dependency-free fuzzy scorer over each label.
//! - Actions receive the strongest source bonus, then directories, then history.
//! - Exact and prefix matches receive additional bonuses.
//! - Original input order is the final tie-break, preserving most-recent-first
//!   ordering for history and directory candidates supplied that way.

use crate::fuzzy::{Score, score};

/// Default maximum number of ranked rows retained by the model.
pub const DEFAULT_PALETTE_MAX_RESULTS: usize = 50;

const ACTION_SOURCE_BONUS: i32 = 2_000;
const DIRECTORY_SOURCE_BONUS: i32 = 500;
const HISTORY_SOURCE_BONUS: i32 = 0;
const EXACT_MATCH_BONUS: i32 = 2_000;
const PREFIX_MATCH_BONUS: i32 = 1_000;

/// Candidate origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteSourceKind {
    /// Local terminal action, identified by an action id on selection.
    Action,
    /// Shell history row, selected as literal text to type.
    History,
    /// Recent directory, selected as literal text to type.
    Directory,
}

impl PaletteSourceKind {
    fn priority(self) -> i32 {
        match self {
            Self::Action => 3,
            Self::Directory => 2,
            Self::History => 1,
        }
    }

    fn bonus(self) -> i32 {
        match self {
            Self::Action => ACTION_SOURCE_BONUS,
            Self::Directory => DIRECTORY_SOURCE_BONUS,
            Self::History => HISTORY_SOURCE_BONUS,
        }
    }
}

/// Candidate row stored by the palette model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    kind: PaletteSourceKind,
    label: String,
    payload: PalettePayload,
}

impl PaletteEntry {
    /// Create an action candidate.
    pub fn action(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: PaletteSourceKind::Action,
            label: label.into(),
            payload: PalettePayload::ActionId(id.into()),
        }
    }

    /// Create a shell-history candidate.
    pub fn history(command: impl Into<String>) -> Self {
        let command = command.into();
        Self {
            kind: PaletteSourceKind::History,
            label: command.clone(),
            payload: PalettePayload::LiteralText(command),
        }
    }

    /// Create a recent-directory candidate.
    pub fn directory(path: impl Into<String>) -> Self {
        let path = path.into();
        Self {
            kind: PaletteSourceKind::Directory,
            label: path.clone(),
            payload: PalettePayload::LiteralText(path),
        }
    }

    /// Candidate source.
    pub fn kind(&self) -> PaletteSourceKind {
        self.kind
    }

    /// Text matched and displayed for the row.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Decision returned if this row is accepted.
    pub fn selection(&self) -> PaletteSelection {
        match &self.payload {
            PalettePayload::ActionId(id) => PaletteSelection::Action { id: id.clone() },
            PalettePayload::LiteralText(text) => PaletteSelection::TypeText {
                text: text.clone(),
                source: self.kind,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PalettePayload {
    ActionId(String),
    LiteralText(String),
}

/// Result returned when the highlighted palette row is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteSelection {
    /// Run a local action by id.
    Action { id: String },
    /// Type literal text into the eventual focused PTY target.
    TypeText {
        /// Text to type. The model never executes it.
        text: String,
        /// Source of the literal text.
        source: PaletteSourceKind,
    },
}

/// Ranked row exposed to a future overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteResult {
    /// Index into the model's original candidate list.
    pub entry_index: usize,
    /// Candidate source.
    pub kind: PaletteSourceKind,
    /// Display/match label.
    pub label: String,
    /// Raw fuzzy score. Empty-query rows have no fuzzy score.
    pub fuzzy_score: Option<Score>,
    /// Final score after source/exact/prefix bonuses.
    pub rank_score: i32,
}

/// Cursor movement behavior at result-list edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionWrap {
    /// Clamp at the first/last result.
    Clamp,
    /// Wrap from first to last and last to first.
    Wrap,
}

/// Model configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteOptions {
    /// Maximum number of results retained after ranking.
    pub max_results: usize,
    /// Edge behavior for cursor movement.
    pub selection_wrap: SelectionWrap,
}

impl Default for PaletteOptions {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_PALETTE_MAX_RESULTS,
            selection_wrap: SelectionWrap::Clamp,
        }
    }
}

/// Headless command-palette state.
#[derive(Debug, Clone)]
pub struct PaletteModel {
    query: String,
    entries: Vec<PaletteEntry>,
    results: Vec<PaletteResult>,
    selected: Option<usize>,
    options: PaletteOptions,
}

impl PaletteModel {
    /// Build a model with default options.
    pub fn new(entries: Vec<PaletteEntry>) -> Self {
        Self::with_options(entries, PaletteOptions::default())
    }

    /// Build a model with explicit options.
    pub fn with_options(entries: Vec<PaletteEntry>, options: PaletteOptions) -> Self {
        let mut model = Self {
            query: String::new(),
            entries,
            results: Vec::new(),
            selected: None,
            options,
        };
        model.rerank();
        model
    }

    /// Current query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Candidate entries in original input order.
    pub fn entries(&self) -> &[PaletteEntry] {
        &self.entries
    }

    /// Current ranked, bounded result list.
    pub fn results(&self) -> &[PaletteResult] {
        &self.results
    }

    /// Selected row index within `results`.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Selected ranked row.
    pub fn selected_result(&self) -> Option<&PaletteResult> {
        self.selected.and_then(|index| self.results.get(index))
    }

    /// Decision for the selected row.
    pub fn selected_selection(&self) -> Option<PaletteSelection> {
        let result = self.selected_result()?;
        self.entries
            .get(result.entry_index)
            .map(PaletteEntry::selection)
    }

    /// Replace all candidates and preserve the current query.
    pub fn set_entries(&mut self, entries: Vec<PaletteEntry>) {
        self.entries = entries;
        self.rerank();
    }

    /// Replace the full query.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.rerank();
    }

    /// Append one query character.
    pub fn push_query_char(&mut self, ch: char) {
        self.query.push(ch);
        self.rerank();
    }

    /// Remove the final query character. Returns whether anything changed.
    pub fn backspace_query(&mut self) -> bool {
        let changed = self.query.pop().is_some();
        if changed {
            self.rerank();
        }
        changed
    }

    /// Clear the query. Returns whether anything changed.
    pub fn clear_query(&mut self) -> bool {
        if self.query.is_empty() {
            return false;
        }
        self.query.clear();
        self.rerank();
        true
    }

    /// Move the selection down by one row.
    pub fn select_next(&mut self) {
        self.move_selection(1);
    }

    /// Move the selection up by one row.
    pub fn select_previous(&mut self) {
        self.move_selection(-1);
    }

    /// Move the selection by a signed delta.
    pub fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.selected.unwrap_or(0);
        let len = self.results.len();
        self.selected = Some(match self.options.selection_wrap {
            SelectionWrap::Clamp => clamp_move(current, delta, len),
            SelectionWrap::Wrap => wrap_move(current, delta, len),
        });
    }

    fn rerank(&mut self) {
        self.results = rank_entries(&self.query, &self.entries, self.options.max_results);
        self.selected = if self.results.is_empty() {
            None
        } else {
            Some(self.selected.unwrap_or(0).min(self.results.len() - 1))
        };
    }
}

/// Build action entries from `(id, label)` pairs.
pub fn action_entries<I, Id, Label>(actions: I) -> Vec<PaletteEntry>
where
    I: IntoIterator<Item = (Id, Label)>,
    Id: Into<String>,
    Label: Into<String>,
{
    actions
        .into_iter()
        .map(|(id, label)| PaletteEntry::action(id, label))
        .collect()
}

/// Build history entries from command strings.
pub fn history_entries<I, S>(commands: I) -> Vec<PaletteEntry>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    commands.into_iter().map(PaletteEntry::history).collect()
}

/// Build directory entries from path strings.
pub fn directory_entries<I, S>(paths: I) -> Vec<PaletteEntry>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    paths.into_iter().map(PaletteEntry::directory).collect()
}

fn rank_entries(query: &str, entries: &[PaletteEntry], max_results: usize) -> Vec<PaletteResult> {
    if max_results == 0 {
        return Vec::new();
    }
    let mut ranked: Vec<(usize, PaletteResult)> = entries
        .iter()
        .enumerate()
        .filter_map(|(entry_index, entry)| rank_entry(query, entry_index, entry))
        .map(|result| (result.entry_index, result))
        .collect();
    ranked.sort_by(|(left_index, left), (right_index, right)| {
        right
            .rank_score
            .cmp(&left.rank_score)
            .then_with(|| entry_empty_priority(right).cmp(&entry_empty_priority(left)))
            .then_with(|| left_index.cmp(right_index))
    });
    ranked
        .into_iter()
        .take(max_results)
        .map(|(_, result)| result)
        .collect()
}

fn rank_entry(query: &str, entry_index: usize, entry: &PaletteEntry) -> Option<PaletteResult> {
    let (fuzzy_score, rank_score) = if query.is_empty() {
        (None, entry.kind.priority())
    } else {
        let fuzzy_score = score(query, entry.label())?;
        let rank_score = fuzzy_score.get()
            + entry.kind.bonus()
            + exact_bonus(query, entry.label())
            + prefix_bonus(query, entry.label());
        (Some(fuzzy_score), rank_score)
    };
    Some(PaletteResult {
        entry_index,
        kind: entry.kind(),
        label: entry.label().to_owned(),
        fuzzy_score,
        rank_score,
    })
}

fn entry_empty_priority(result: &PaletteResult) -> i32 {
    if result.fuzzy_score.is_none() {
        result.kind.priority()
    } else {
        0
    }
}

fn exact_bonus(query: &str, label: &str) -> i32 {
    if label.eq_ignore_ascii_case(query) {
        EXACT_MATCH_BONUS
    } else {
        0
    }
}

fn prefix_bonus(query: &str, label: &str) -> i32 {
    if starts_with_ignore_ascii_case(label, query) {
        PREFIX_MATCH_BONUS
    } else {
        0
    }
}

fn starts_with_ignore_ascii_case(label: &str, query: &str) -> bool {
    label
        .chars()
        .zip(query.chars())
        .all(|(left, right)| left.eq_ignore_ascii_case(&right))
        && label.chars().count() >= query.chars().count()
}

fn clamp_move(current: usize, delta: isize, len: usize) -> usize {
    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current.saturating_add(delta as usize).min(len - 1)
    }
}

fn wrap_move(current: usize, delta: isize, len: usize) -> usize {
    let len = len as isize;
    (current as isize + delta).rem_euclid(len) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette_sources::{DirectorySourceLimits, directory_candidates};

    fn labels(model: &PaletteModel) -> Vec<&str> {
        model
            .results()
            .iter()
            .map(|result| result.label.as_str())
            .collect()
    }

    #[test]
    fn empty_query_groups_by_source_and_preserves_input_order() {
        let model = PaletteModel::new(vec![
            PaletteEntry::history("git status"),
            PaletteEntry::directory("/work/newest"),
            PaletteEntry::action("open-settings", "Open Settings"),
            PaletteEntry::history("cargo test"),
            PaletteEntry::directory("/work/older"),
        ]);

        assert_eq!(
            labels(&model),
            vec![
                "Open Settings",
                "/work/newest",
                "/work/older",
                "git status",
                "cargo test"
            ]
        );
        assert_eq!(model.selected_index(), Some(0));
        assert!(model.results()[0].fuzzy_score.is_none());
    }

    #[test]
    fn mixed_source_ranking_prefers_actions_then_prefixes() {
        let mut model = PaletteModel::new(vec![
            PaletteEntry::history("git checkout feature"),
            PaletteEntry::action("copy", "Copy Selection"),
            PaletteEntry::directory("/repo/checkout"),
            PaletteEntry::action("close-pane", "Close Pane"),
        ]);

        model.set_query("co");

        assert_eq!(
            labels(&model),
            vec![
                "Copy Selection",
                "Close Pane",
                "/repo/checkout",
                "git checkout feature"
            ]
        );
        assert_eq!(
            model.selected_selection(),
            Some(PaletteSelection::Action {
                id: "copy".to_owned()
            })
        );
    }

    #[test]
    fn query_edit_ops_rerank_and_filter() {
        let mut model = PaletteModel::new(vec![
            PaletteEntry::history("cargo test"),
            PaletteEntry::history("git status"),
            PaletteEntry::history("make release"),
        ]);

        model.push_query_char('g');
        assert_eq!(labels(&model), vec!["git status", "cargo test"]);

        model.push_query_char('i');
        assert_eq!(labels(&model), vec!["git status"]);

        assert!(model.backspace_query());
        assert_eq!(model.query(), "g");
        assert_eq!(labels(&model), vec!["git status", "cargo test"]);

        assert!(model.clear_query());
        assert_eq!(model.query(), "");
        assert_eq!(
            labels(&model),
            vec!["cargo test", "git status", "make release"]
        );
    }

    #[test]
    fn selection_clamps_by_default() {
        let mut model = PaletteModel::new(vec![
            PaletteEntry::history("one"),
            PaletteEntry::history("two"),
            PaletteEntry::history("three"),
        ]);

        model.select_previous();
        assert_eq!(model.selected_index(), Some(0));

        model.move_selection(10);
        assert_eq!(model.selected_index(), Some(2));

        model.select_next();
        assert_eq!(model.selected_index(), Some(2));
    }

    #[test]
    fn selection_can_wrap() {
        let options = PaletteOptions {
            max_results: DEFAULT_PALETTE_MAX_RESULTS,
            selection_wrap: SelectionWrap::Wrap,
        };
        let mut model = PaletteModel::with_options(
            vec![PaletteEntry::history("one"), PaletteEntry::history("two")],
            options,
        );

        model.select_previous();
        assert_eq!(model.selected_index(), Some(1));

        model.select_next();
        assert_eq!(model.selected_index(), Some(0));
    }

    #[test]
    fn result_count_is_bounded() {
        let options = PaletteOptions {
            max_results: 2,
            selection_wrap: SelectionWrap::Clamp,
        };
        let model = PaletteModel::with_options(
            vec![
                PaletteEntry::history("one"),
                PaletteEntry::history("two"),
                PaletteEntry::history("three"),
            ],
            options,
        );

        assert_eq!(labels(&model), vec!["one", "two"]);
    }

    #[test]
    fn non_matching_query_clears_results_and_selection() {
        let mut model = PaletteModel::new(vec![PaletteEntry::history("cargo test")]);

        model.set_query("zzz");

        assert!(model.results().is_empty());
        assert_eq!(model.selected_index(), None);
        assert_eq!(model.selected_selection(), None);
    }

    #[test]
    fn selected_history_and_directory_return_literal_text_decisions() {
        let mut model = PaletteModel::new(vec![
            PaletteEntry::history("cargo test"),
            PaletteEntry::directory("/work/project"),
        ]);

        model.set_query("work");

        assert_eq!(
            model.selected_selection(),
            Some(PaletteSelection::TypeText {
                text: "/work/project".to_owned(),
                source: PaletteSourceKind::Directory,
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
    fn source_helpers_compose_with_bounded_directory_source() {
        let dirs = directory_candidates(
            ["/work/old", "/work/new", "/work/new"],
            DirectorySourceLimits {
                max_entries: 4,
                max_entry_chars: 128,
            },
        );
        let mut entries = action_entries([("open-settings", "Open Settings")]);
        entries.extend(directory_entries(dirs));
        entries.extend(history_entries(["git status"]));
        let model = PaletteModel::new(entries);

        assert_eq!(
            labels(&model),
            vec!["Open Settings", "/work/new", "/work/old", "git status"]
        );
    }

    #[test]
    fn set_entries_preserves_query_and_clamps_selection() {
        let mut model = PaletteModel::new(vec![
            PaletteEntry::history("alpha"),
            PaletteEntry::history("beta"),
            PaletteEntry::history("gamma"),
        ]);
        model.set_query("a");
        model.move_selection(2);

        model.set_entries(vec![PaletteEntry::history("alpha")]);

        assert_eq!(model.query(), "a");
        assert_eq!(labels(&model), vec!["alpha"]);
        assert_eq!(model.selected_index(), Some(0));
    }
}

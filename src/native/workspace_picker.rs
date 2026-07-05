// SPDX-License-Identifier: GPL-3.0-only
//! Native filtered name-list picker (W4-v2 + LAYOUT-SURFACE).
//!
//! A minimal sibling of the "Open With…" app picker (`open_with_overlay.rs`),
//! specialized over a frozen list of named entries. It serves two consumers
//! selected by [`WorkspacePickerPurpose`]:
//! * "Move to Workspace…" (W4-v2) — lists the workspaces a tab can move to
//!   (every workspace EXCEPT the one that owns the clicked tab) and on Enter
//!   emits [`WorkspacePickerOutcome::Move`] carrying the clicked tab's token
//!   paired with the chosen workspace's ORIGINAL index for the App to splice.
//! * "Open Layout ▸" (LAYOUT-SURFACE) — lists the saved layout names and on
//!   Enter emits [`WorkspacePickerOutcome::OpenLayout`] carrying the chosen
//!   name; with no saved layouts the picker still opens and shows an
//!   explanatory line so the feature is discoverable.
//!
//! This is the shared "menu item -> seeded picker -> tagged accept" pattern
//! (ODP-1 Option B) applied to the workspace list: the connection overlay serves
//! the host pickers, this thin sibling serves the workspace case. It owns a
//! frozen destination list captured at open time, never writes to the PTY, and
//! never mutates the live terminal model. Workspace names are user-set text, so
//! they are control-char sanitized before display exactly like the app/session
//! rows — a malformed name can never inject escape sequences into the plain-text
//! rows.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::fuzzy;

use super::overlay::OverlayInput;
use super::session::SessionToken;

/// Maximum rows rendered (keeps the overlay compact and the fuzzy ranking
/// bounded). Matches the ceiling the other list overlays use.
const MAX_RESULTS: usize = 40;

/// What an accepted pick means — the tagged-accept half of the shared
/// "menu item -> seeded picker -> tagged accept" pattern (ODP-1B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum WorkspacePickerPurpose {
    /// Move-to-Workspace (W4-v2): accept emits [`WorkspacePickerOutcome::Move`]
    /// with the carried token + the chosen entry's original workspace index.
    #[default]
    MoveTab,
    /// Open-Layout (LAYOUT-SURFACE): accept emits
    /// [`WorkspacePickerOutcome::OpenLayout`] with the chosen entry's name.
    OpenLayout,
}

/// One move destination: a workspace's ORIGINAL rail index (the
/// [`super::session::WorkspaceSet::move_tab_to_workspace`] target) paired with
/// its display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePickerEntry {
    pub(super) index: usize,
    pub(super) name: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct WorkspacePicker {
    /// The frozen destination list captured at open time, in rail order. The
    /// source workspace is already excluded by the seeder (the App), so every
    /// row here is a valid move target.
    entries: Vec<WorkspacePickerEntry>,
    /// The clicked tab whose token the move targets (F7 surface): the tab moves,
    /// not the active tab. Set at open; `None` before the first open (the empty
    /// default, which the entry list also guards). Unused for the Open-Layout
    /// purpose (layouts carry no token).
    token: Option<SessionToken>,
    /// Which consumer this picker was opened for (tagged accept).
    purpose: WorkspacePickerPurpose,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll_offset: Cell<usize>,
    last_body_height: Cell<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkspacePickerOutcome {
    Consumed,
    Close,
    /// The user accepted a destination. Carries the clicked tab's token plus the
    /// chosen workspace's original index; the App performs the `Tab` value
    /// splice. This overlay never mutates the model itself.
    Move(SessionToken, usize),
    /// The user accepted a saved layout (LAYOUT-SURFACE). Carries the chosen
    /// layout name; the App instantiates it (APPEND a new workspace, WP3 8e).
    OpenLayout(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePickerLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspacePickerSignature {
    pub(super) query: String,
    pub(super) selected: Option<usize>,
    pub(super) results_len: usize,
    pub(super) results_fingerprint: u64,
}

impl WorkspacePicker {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Load a frozen set of destinations for the tab `token` and reset the
    /// query/cursor for the Move-to-Workspace purpose. The list is owned by the
    /// overlay, so it stays stable while open even if workspaces change
    /// underneath.
    pub(super) fn open(&mut self, entries: Vec<WorkspacePickerEntry>, token: SessionToken) {
        self.purpose = WorkspacePickerPurpose::MoveTab;
        self.entries = entries;
        self.token = Some(token);
        self.query.clear();
        self.selected = 0;
        self.reset_scroll();
        self.recompute();
    }

    /// Load the saved layout names for the Open-Layout purpose (LAYOUT-SURFACE).
    /// The entry index is the name's position (unused on accept — Open-Layout
    /// keys on the name); an empty list is valid and renders the explanatory
    /// empty line rather than refusing to open, so the picker teaches the feature.
    pub(super) fn open_layouts(&mut self, names: Vec<String>) {
        self.purpose = WorkspacePickerPurpose::OpenLayout;
        self.entries = names
            .into_iter()
            .enumerate()
            .map(|(index, name)| WorkspacePickerEntry { index, name })
            .collect();
        self.token = None;
        self.query.clear();
        self.selected = 0;
        self.reset_scroll();
        self.recompute();
    }

    #[cfg(test)]
    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn recompute(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.entries.len()).take(MAX_RESULTS).collect();
        } else {
            let haystacks: Vec<String> = self.entries.iter().map(|e| sanitize(&e.name)).collect();
            self.filtered = fuzzy::rank(&self.query, &haystacks)
                .into_iter()
                .take(MAX_RESULTS)
                .map(|(index, _)| index)
                .collect();
        }
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        let next = (self.selected as isize + delta).clamp(0, max as isize);
        self.selected = next as usize;
    }

    /// The line shown when no rows are selectable. An Open-Layout picker with no
    /// saved layouts explains the feature (LAYOUT-SURFACE teaching moment);
    /// otherwise it is the ordinary "no filter match" hint.
    fn empty_line_text(&self) -> String {
        if self.entries.is_empty() && matches!(self.purpose, WorkspacePickerPurpose::OpenLayout) {
            "No saved layouts yet \u{2014} use Save as Layout first".to_owned()
        } else {
            "No matches".to_owned()
        }
    }

    fn selected_entry(&self) -> Option<&WorkspacePickerEntry> {
        let entry_index = *self.filtered.get(self.selected)?;
        self.entries.get(entry_index)
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> WorkspacePickerOutcome {
        match input {
            OverlayInput::Close => WorkspacePickerOutcome::Close,
            OverlayInput::Up => {
                self.move_selection(-1);
                self.follow_selection_for_known_body_height();
                WorkspacePickerOutcome::Consumed
            }
            OverlayInput::Down => {
                self.move_selection(1);
                self.follow_selection_for_known_body_height();
                WorkspacePickerOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                WorkspacePickerOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
                self.move_selection(MAX_RESULTS as isize);
                self.follow_selection_for_known_body_height();
                WorkspacePickerOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.query.pop();
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                WorkspacePickerOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                WorkspacePickerOutcome::Consumed
            }
            OverlayInput::Activate => match self.purpose {
                WorkspacePickerPurpose::MoveTab => match (self.token, self.selected_entry()) {
                    (Some(token), Some(entry)) => WorkspacePickerOutcome::Move(token, entry.index),
                    _ => WorkspacePickerOutcome::Consumed,
                },
                WorkspacePickerPurpose::OpenLayout => match self.selected_entry() {
                    Some(entry) => WorkspacePickerOutcome::OpenLayout(entry.name.clone()),
                    None => WorkspacePickerOutcome::Consumed,
                },
            },
            OverlayInput::Char(_)
            | OverlayInput::Left
            | OverlayInput::Right
            | OverlayInput::Save
            | OverlayInput::ActivateAlt
            | OverlayInput::Tab => WorkspacePickerOutcome::Consumed,
        }
    }

    pub(super) fn scroll_lines(&mut self, lines: isize) {
        self.move_selection(lines.signum());
        self.follow_selection_for_known_body_height();
    }

    /// Map a clicked body row to the selection cursor it represents — the
    /// inverse of the [`Self::visible_lines`] windowing. Row 0 is the `> query`
    /// prompt; destination rows follow from the live `scroll_offset`. Returns
    /// `None` for the prompt row, the empty/"No matches" hint, or a click past
    /// the last row.
    pub(super) fn row_at(&self, row_in_body: usize, body_height: usize) -> Option<usize> {
        if body_height == 0 || row_in_body == 0 || self.filtered.is_empty() {
            return None;
        }
        let visible_results = body_height - 1;
        let within = row_in_body - 1;
        if within >= visible_results {
            return None;
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let cursor = scroll_offset + within;
        (cursor < self.filtered.len()).then_some(cursor)
    }

    /// Select the row under a left-click, reporting whether it landed on a
    /// selectable row so the caller can route the existing Activate. Parity with
    /// Down×N + Activate by construction.
    pub(super) fn click_row(&mut self, row_in_body: usize, body_height: usize) -> bool {
        match self.row_at(row_in_body, body_height) {
            Some(cursor) => {
                self.selected = cursor;
                self.follow_selection_for_known_body_height();
                true
            }
            None => false,
        }
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<WorkspacePickerLine> {
        if body_height == 0 {
            self.last_body_height.set(0);
            self.scroll_offset.set(0);
            return Vec::new();
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2));
        lines.push(WorkspacePickerLine {
            text: truncate_for_width(&format!("> {}", self.query), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        if self.filtered.is_empty() {
            self.scroll_offset.set(0);
            lines.push(WorkspacePickerLine {
                text: self.empty_line_text(),
                focused: false,
                bold: false,
            });
            return lines;
        }
        let remaining = body_height - lines.len();
        for (visible_index, &entry_index) in self
            .filtered
            .iter()
            .skip(scroll_offset)
            .take(remaining)
            .enumerate()
        {
            let row = scroll_offset + visible_index;
            let Some(entry) = self.entries.get(entry_index) else {
                continue;
            };
            lines.push(WorkspacePickerLine {
                text: truncate_for_width(&sanitize(&entry.name), body_width),
                focused: row == self.selected,
                bold: false,
            });
        }
        lines
    }

    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        let visible_results = body_height.saturating_sub(1);
        if visible_results == 0 || self.filtered.len() <= visible_results {
            self.scroll_offset.set(0);
            return (false, false);
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        (
            scroll_offset > 0,
            scroll_offset + visible_results < self.filtered.len(),
        )
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
        let visible_results = body_height.saturating_sub(1);
        let results_len = self.filtered.len();
        if visible_results == 0 || results_len <= visible_results {
            self.scroll_offset.set(0);
            return 0;
        }
        let max_scroll = results_len - visible_results;
        let mut scroll_offset = self.scroll_offset.get().min(max_scroll);
        if self.selected < scroll_offset {
            scroll_offset = self.selected;
        } else if self.selected >= scroll_offset + visible_results {
            scroll_offset = self.selected + 1 - visible_results;
        }
        self.scroll_offset.set(scroll_offset);
        scroll_offset
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        columns.min(72)
    }

    pub(super) fn render_signature(&self) -> WorkspacePickerSignature {
        WorkspacePickerSignature {
            query: self.query.clone(),
            selected: if self.filtered.is_empty() {
                None
            } else {
                Some(self.selected)
            },
            results_len: self.filtered.len(),
            results_fingerprint: self.results_fingerprint(),
        }
    }

    fn results_fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.scroll_offset.get().hash(&mut hasher);
        for &entry_index in self.filtered.iter().take(MAX_RESULTS) {
            if let Some(entry) = self.entries.get(entry_index) {
                entry.index.hash(&mut hasher);
                entry.name.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// Strip control characters so a malformed workspace name can never inject
/// escape sequences into the overlay's plain-text rows.
fn sanitize(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control()).collect()
}

fn truncate_for_width(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(index: usize, name: &str) -> WorkspacePickerEntry {
        WorkspacePickerEntry {
            index,
            name: name.to_owned(),
        }
    }

    fn dests() -> Vec<WorkspacePickerEntry> {
        // Original indices 0, 2, 3 — index 1 is the excluded source workspace,
        // so the picker never sees it (exclusion is the seeder's job).
        vec![entry(0, "main"), entry(2, "prod"), entry(3, "scratch")]
    }

    fn open(entries: Vec<WorkspacePickerEntry>) -> WorkspacePicker {
        let mut overlay = WorkspacePicker::new();
        overlay.open(entries, SessionToken(7));
        overlay
    }

    fn type_query(overlay: &mut WorkspacePicker, query: &str) {
        for ch in query.chars() {
            assert_eq!(
                overlay.handle_input(OverlayInput::Char(ch)),
                WorkspacePickerOutcome::Consumed
            );
        }
    }

    #[test]
    fn empty_query_lists_all_destinations_in_order() {
        let overlay = open(dests());
        assert_eq!(overlay.render_signature().results_len, 3);
        let lines = overlay.visible_lines(60, 10);
        assert!(lines[1].text.starts_with("main"));
        assert!(lines[2].text.starts_with("prod"));
        assert!(lines[3].text.starts_with("scratch"));
    }

    #[test]
    fn fuzzy_filter_ranks_match_first() {
        let mut overlay = open(dests());
        type_query(&mut overlay, "prod");
        assert_eq!(overlay.render_signature().results_len, 1);
        let lines = overlay.visible_lines(60, 10);
        assert!(lines[1].text.starts_with("prod"));
    }

    #[test]
    fn accept_emits_move_with_token_and_original_index() {
        // Selecting the 2nd row (prod, original index 2) moves the opened token.
        let mut overlay = open(dests());
        overlay.handle_input(OverlayInput::Down);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            WorkspacePickerOutcome::Move(SessionToken(7), 2),
            "accept carries the clicked tab's token + the chosen ORIGINAL index"
        );
    }

    #[test]
    fn no_match_activate_is_inert() {
        let mut overlay = open(dests());
        type_query(&mut overlay, "zzz-nope");
        assert_eq!(overlay.render_signature().results_len, 0);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            WorkspacePickerOutcome::Consumed
        );
    }

    #[test]
    fn single_destination_lists_one_row() {
        let overlay = open(vec![entry(0, "main")]);
        assert_eq!(overlay.entry_count(), 1);
        let lines = overlay.visible_lines(60, 10);
        assert!(lines[1].text.starts_with("main"));
        assert_eq!(overlay.render_signature().results_len, 1);
    }

    #[test]
    fn close_input_requests_close() {
        let mut overlay = open(dests());
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            WorkspacePickerOutcome::Close
        );
    }

    #[test]
    fn selection_clamps_within_filtered_rows() {
        let mut overlay = open(dests());
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Down);
        }
        assert_eq!(overlay.render_signature().selected, Some(2));
        for _ in 0..10 {
            overlay.handle_input(OverlayInput::Up);
        }
        assert_eq!(overlay.render_signature().selected, Some(0));
    }

    #[test]
    fn control_chars_in_name_are_sanitized() {
        let overlay = open(vec![entry(0, "evil\u{1b}[31m\u{7}")]);
        let lines = overlay.visible_lines(120, 10);
        assert!(!lines[1].text.contains('\u{1b}'));
        assert!(!lines[1].text.contains('\u{7}'));
    }

    #[test]
    fn click_row_selects_same_destination_as_down_then_activate() {
        for target in 0..3 {
            let mut by_click = open(dests());
            let _ = by_click.visible_lines(60, 10);
            assert!(by_click.click_row(target + 1, 10));
            let click_move = by_click.handle_input(OverlayInput::Activate);

            let mut by_keys = open(dests());
            for _ in 0..target {
                by_keys.handle_input(OverlayInput::Down);
            }
            let key_move = by_keys.handle_input(OverlayInput::Activate);

            assert_eq!(click_move, key_move, "row {target} parity");
        }
    }

    #[test]
    fn click_query_prompt_and_past_end_are_inert() {
        let mut overlay = open(dests());
        let _ = overlay.visible_lines(60, 10);
        assert!(!overlay.click_row(0, 10));
        assert!(!overlay.click_row(4, 10));
    }

    #[test]
    fn visible_lines_bounded_by_body_height() {
        let overlay = open(dests());
        assert_eq!(overlay.visible_lines(60, 2).len(), 2);
        assert!(overlay.visible_lines(60, 10).len() <= 10);
    }

    // ── LAYOUT-SURFACE: Open-Layout purpose ────────────────────────────────

    fn open_layouts(names: &[&str]) -> WorkspacePicker {
        let mut overlay = WorkspacePicker::new();
        overlay.open_layouts(names.iter().map(|s| (*s).to_owned()).collect());
        overlay
    }

    #[test]
    fn open_layouts_lists_names_and_accept_emits_open_layout() {
        let mut overlay = open_layouts(&["dev", "prod", "scratch"]);
        assert_eq!(overlay.render_signature().results_len, 3);
        // The second row (prod) accepts as OpenLayout carrying the NAME, not an
        // index — layouts key on the name, unlike the Move purpose.
        overlay.handle_input(OverlayInput::Down);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            WorkspacePickerOutcome::OpenLayout("prod".to_owned()),
        );
    }

    #[test]
    fn open_layouts_fuzzy_filters_by_name() {
        let mut overlay = open_layouts(&["dev", "prod", "scratch"]);
        for ch in "prod".chars() {
            overlay.handle_input(OverlayInput::Char(ch));
        }
        assert_eq!(overlay.render_signature().results_len, 1);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            WorkspacePickerOutcome::OpenLayout("prod".to_owned()),
        );
    }

    #[test]
    fn empty_layout_picker_shows_an_explanatory_line_and_no_accept() {
        // With no saved layouts the picker still opens (discoverability) and
        // shows a teaching line; Activate is inert (nothing to open).
        let mut overlay = open_layouts(&[]);
        assert_eq!(overlay.render_signature().results_len, 0);
        let lines = overlay.visible_lines(80, 10);
        assert!(
            lines.iter().any(|l| l.text.contains("No saved layouts")),
            "empty layout picker teaches the feature: {lines:?}"
        );
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            WorkspacePickerOutcome::Consumed,
        );
    }

    #[test]
    fn reopening_for_move_after_layouts_restores_move_accept() {
        // The shared picker is reused across purposes: opening for Move after an
        // Open-Layout session emits the Move outcome again (purpose is reset).
        let mut overlay = open_layouts(&["dev"]);
        overlay.open(dests(), SessionToken(7));
        overlay.handle_input(OverlayInput::Down);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            WorkspacePickerOutcome::Move(SessionToken(7), 2),
        );
    }
}

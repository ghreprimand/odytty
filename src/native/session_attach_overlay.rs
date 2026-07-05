// SPDX-License-Identifier: GPL-3.0-only
//! Native session-attach summon overlay (Phase 5 / B2).
//!
//! The in-window analogue of `odytty attach`: a presentation-only overlay that
//! lists the live, detached session-host sessions, type-to-filters them, and on
//! Enter emits an [`SessionAttachOverlayOutcome::Attach`] carrying the chosen
//! session id for the App to attach into a **new tab**. Like the
//! connection-manager overlay it clones (`connection_overlay.rs`), it owns a
//! frozen list captured at open time, never writes to the PTY, and never mutates
//! the live terminal model.
//!
//! Privacy / safety: the overlay only displays whatever the App handed it
//! through [`crate::session_host::list_live_sessions`]; it reads
//! nothing itself. Session names carry the user-supplied `--title`, so they are
//! control-char-sanitized before display exactly like host names — a malformed
//! title can never inject escape sequences into the overlay's plain-text rows.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::fuzzy;
use crate::session_host::ListedSession;

use super::overlay::OverlayInput;

/// Maximum rows rendered in the result list (keeps the overlay compact and the
/// fuzzy ranking bounded regardless of how many sessions are live).
const MAX_RESULTS: usize = 40;

#[derive(Debug, Clone, Default)]
pub(super) struct SessionAttachOverlay {
    /// The frozen session list captured at open time, in load order (sorted by
    /// id by the registry).
    entries: Vec<ListedSession>,
    /// Current type-to-filter query.
    query: String,
    /// Indexes into `entries` for the rows that match `query`, best-first.
    filtered: Vec<usize>,
    /// Selection cursor into `filtered`. Clamped whenever `filtered` changes.
    selected: usize,
    /// Scroll offset into `filtered` for the visible window on a short overlay
    /// (OVERLAY-SMALL-WINDOW). Interior-mutable so the render pass — the only
    /// place the live body height is known — can keep the selection in view,
    /// mirroring the connection/palette overlays. `0` whenever everything fits,
    /// so a tall overlay is byte-identical to before scrolling existed.
    scroll_offset: Cell<usize>,
    /// The last body height the render pass saw, so keyboard nav (which has no
    /// body height) can re-follow the selection through the same math.
    last_body_height: Cell<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionAttachOverlayOutcome {
    Consumed,
    Close,
    /// The user accepted a session. The App attaches it into a new tab
    /// (presentation is done; this overlay never attaches anything itself).
    /// Carries the session id.
    Attach(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionAttachOverlayLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionAttachOverlaySignature {
    pub(super) query: String,
    pub(super) selected: Option<usize>,
    pub(super) results_len: usize,
    pub(super) results_fingerprint: u64,
}

impl SessionAttachOverlay {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Load a frozen set of live sessions and reset the query/cursor. The list
    /// is owned by the overlay, so it stays stable while open even if a session
    /// ends underneath it (a stale id is handled gracefully on accept).
    pub(super) fn open(&mut self, entries: Vec<ListedSession>) {
        self.entries = entries;
        self.query.clear();
        self.selected = 0;
        self.reset_scroll();
        self.recompute();
    }

    #[cfg(test)]
    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Recompute the filtered/ranked view from the current query. Empty query
    /// preserves load order; a non-empty query uses the shared fuzzy scorer over
    /// each row's searchable text (name + id).
    fn recompute(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.entries.len()).take(MAX_RESULTS).collect();
        } else {
            let haystacks: Vec<String> = self.entries.iter().map(match_text).collect();
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

    fn selected_entry(&self) -> Option<&ListedSession> {
        let entry_index = *self.filtered.get(self.selected)?;
        self.entries.get(entry_index)
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> SessionAttachOverlayOutcome {
        match input {
            OverlayInput::Close => SessionAttachOverlayOutcome::Close,
            OverlayInput::Up => {
                self.move_selection(-1);
                self.follow_selection_for_known_body_height();
                SessionAttachOverlayOutcome::Consumed
            }
            OverlayInput::Down => {
                self.move_selection(1);
                self.follow_selection_for_known_body_height();
                SessionAttachOverlayOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                SessionAttachOverlayOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
                self.move_selection(MAX_RESULTS as isize);
                self.follow_selection_for_known_body_height();
                SessionAttachOverlayOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.query.pop();
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                SessionAttachOverlayOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                SessionAttachOverlayOutcome::Consumed
            }
            OverlayInput::Activate => match self.selected_entry() {
                Some(entry) => SessionAttachOverlayOutcome::Attach(entry.id.clone()),
                None => SessionAttachOverlayOutcome::Consumed,
            },
            OverlayInput::Char(_)
            | OverlayInput::Left
            | OverlayInput::Right
            | OverlayInput::Save
            | OverlayInput::ActivateAlt
            | OverlayInput::Tab => SessionAttachOverlayOutcome::Consumed,
        }
    }

    /// Scroll one row in response to a wheel notch (negative = toward the top).
    pub(super) fn scroll_lines(&mut self, lines: isize) {
        self.move_selection(lines.signum());
        self.follow_selection_for_known_body_height();
    }

    /// Map a clicked body row to the selection cursor it represents — the
    /// inverse of the [`Self::visible_lines`] windowing (UX4-P1 click→Activate).
    /// Row 0 is the `> query` prompt; results follow from the live
    /// `scroll_offset`. Returns `None` for the prompt row, the empty/"No
    /// matches" hint, or a click past the last result.
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
    /// Down×N + Activate by construction: it sets the same `selected` cursor a
    /// Wheel/Down move would and re-follows the scroll window.
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

    /// The session id at a clicked body row WITHOUT moving the selection — the
    /// handle for a right-click "kill this session" (Manage Sessions). Reuses
    /// [`Self::row_at`] so it lands on exactly the same rows a left-click would
    /// select; the prompt row, the empty/"No matches" hint, and clicks past the
    /// last result all return `None`. Read-only: the attach (left-click) path is
    /// untouched.
    pub(super) fn id_at_row(&self, row_in_body: usize, body_height: usize) -> Option<String> {
        let cursor = self.row_at(row_in_body, body_height)?;
        let entry_index = *self.filtered.get(cursor)?;
        self.entries.get(entry_index).map(|entry| entry.id.clone())
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<SessionAttachOverlayLine> {
        if body_height == 0 {
            self.last_body_height.set(0);
            self.scroll_offset.set(0);
            return Vec::new();
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2));
        lines.push(SessionAttachOverlayLine {
            text: truncate_for_width(&format!("> {}", self.query), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        if self.entries.is_empty() {
            self.scroll_offset.set(0);
            lines.push(SessionAttachOverlayLine {
                text: truncate_for_width(
                    "No live sessions — start one with `odytty new` to attach here.",
                    body_width,
                ),
                focused: false,
                bold: false,
            });
            return lines;
        }
        if self.filtered.is_empty() {
            self.scroll_offset.set(0);
            lines.push(SessionAttachOverlayLine {
                text: "No matches".to_owned(),
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
            lines.push(SessionAttachOverlayLine {
                text: truncate_for_width(&row_label(entry), body_width),
                focused: row == self.selected,
                bold: false,
            });
        }
        lines
    }

    /// Hidden result rows above / below the visible window, for the shared
    /// scroll affordance (OVERLAY-SMALL-WINDOW). One body row is the query line,
    /// so the result viewport is `body_height - 1`. `(false, false)` whenever
    /// everything fits, so a tall overlay draws no arrows.
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
        columns.min(84)
    }

    pub(super) fn render_signature(&self) -> SessionAttachOverlaySignature {
        SessionAttachOverlaySignature {
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
        // Fold in the scroll offset so a view-only scroll (selection unchanged)
        // still changes the signature → the render cache repaints instead of
        // freezing the list (the cache-staleness lesson the connection overlay
        // records).
        self.scroll_offset.get().hash(&mut hasher);
        for &entry_index in self.filtered.iter().take(MAX_RESULTS) {
            if let Some(entry) = self.entries.get(entry_index) {
                entry.id.hash(&mut hasher);
                entry.name.hash(&mut hasher);
                entry.state.hash(&mut hasher);
                entry.pane_count.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// Build the searchable text for one session: name plus id so a user can
/// fuzzy-match on either. `age`/`state` are display metadata, not searched.
fn match_text(entry: &ListedSession) -> String {
    let mut text = entry.name.clone();
    text.push(' ');
    text.push_str(&entry.id);
    sanitize(&text)
}

/// Render one session row: `name   (id)   state   N panes`. The name carries
/// the user-supplied `--title` (falling back to the id), so it leads the row and
/// reads like "build" rather than a numeric id.
fn row_label(entry: &ListedSession) -> String {
    let mut label = sanitize(&entry.name);
    // Only show the id separately when it differs from the displayed name, so a
    // titled session reads cleanly and an untitled one does not repeat itself.
    if entry.name != entry.id {
        label.push_str("   (");
        label.push_str(&sanitize(&entry.id));
        label.push(')');
    }
    label.push_str("   ");
    label.push_str(entry.state);
    label.push_str("   ");
    label.push_str(&entry.pane_count.to_string());
    label.push_str(if entry.pane_count == 1 {
        " pane"
    } else {
        " panes"
    });
    label
}

/// Strip control characters so a malformed session title can never inject escape
/// sequences into the overlay's plain-text rows.
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

    fn session(id: &str, name: &str, pane_count: usize) -> ListedSession {
        ListedSession {
            id: id.to_owned(),
            name: name.to_owned(),
            state: "running",
            age_ms: 1000,
            pane_count,
        }
    }

    fn entries() -> Vec<ListedSession> {
        vec![
            session("s-0001-aaaa", "build", 1),
            session("s-0002-bbbb", "web", 2),
            // An untitled session: name falls back to the id.
            session("s-0003-cccc", "s-0003-cccc", 1),
        ]
    }

    fn open(entries: Vec<ListedSession>) -> SessionAttachOverlay {
        let mut overlay = SessionAttachOverlay::new();
        overlay.open(entries);
        overlay
    }

    fn type_query(overlay: &mut SessionAttachOverlay, query: &str) {
        for ch in query.chars() {
            assert_eq!(
                overlay.handle_input(OverlayInput::Char(ch)),
                SessionAttachOverlayOutcome::Consumed
            );
        }
    }

    #[test]
    fn empty_query_lists_all_in_load_order() {
        let overlay = open(entries());
        assert_eq!(overlay.render_signature().results_len, 3);
        let lines = overlay.visible_lines(80, 10);
        // Line 0 is the query prompt; rows follow.
        assert!(lines[1].text.starts_with("build"));
        assert!(lines[2].text.starts_with("web"));
        assert!(lines[3].text.starts_with("s-0003-cccc"));
    }

    #[test]
    fn row_label_surfaces_title_not_just_id() {
        // B0/B1 goal: a titled session reads "build" with its id in parens, not a
        // bare numeric id.
        let overlay = open(entries());
        let lines = overlay.visible_lines(120, 10);
        assert!(lines[1].text.starts_with("build"));
        assert!(lines[1].text.contains("(s-0001-aaaa)"));
        assert!(lines[1].text.contains("1 pane"));
        // web has 2 panes → pluralized.
        assert!(lines[2].text.contains("2 panes"));
    }

    #[test]
    fn untitled_session_does_not_repeat_the_id() {
        let overlay = open(entries());
        let lines = overlay.visible_lines(120, 10);
        // s-0003 has name == id, so the id is not shown twice.
        assert_eq!(lines[3].text.matches("s-0003-cccc").count(), 1);
    }

    #[test]
    fn fuzzy_filter_ranks_matching_session_first() {
        let mut overlay = open(entries());
        type_query(&mut overlay, "build");
        let signature = overlay.render_signature();
        assert_eq!(signature.results_len, 1);
        assert_eq!(signature.selected, Some(0));
        let lines = overlay.visible_lines(80, 10);
        assert!(lines[1].text.starts_with("build"));
    }

    #[test]
    fn fuzzy_matches_id_not_only_name() {
        let mut overlay = open(entries());
        // "bbbb" only appears in the id of the "web" session.
        type_query(&mut overlay, "bbbb");
        assert_eq!(overlay.render_signature().results_len, 1);
        assert!(matches!(
            overlay.handle_input(OverlayInput::Activate),
            SessionAttachOverlayOutcome::Attach(id) if id == "s-0002-bbbb"
        ));
    }

    #[test]
    fn accept_emits_attach_for_selected_session() {
        let mut overlay = open(entries());
        overlay.handle_input(OverlayInput::Down); // select web
        assert!(matches!(
            overlay.handle_input(OverlayInput::Activate),
            SessionAttachOverlayOutcome::Attach(id) if id == "s-0002-bbbb"
        ));
    }

    #[test]
    fn no_match_query_emits_consumed_on_activate() {
        let mut overlay = open(entries());
        type_query(&mut overlay, "zzzznomatch");
        assert_eq!(overlay.render_signature().results_len, 0);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            SessionAttachOverlayOutcome::Consumed
        );
    }

    #[test]
    fn empty_overlay_shows_hint_and_activate_is_inert() {
        let mut overlay = open(Vec::new());
        assert_eq!(overlay.entry_count(), 0);
        let lines = overlay.visible_lines(80, 10);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].text.contains("No live sessions"));
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            SessionAttachOverlayOutcome::Consumed
        );
    }

    #[test]
    fn close_input_requests_close() {
        let mut overlay = open(entries());
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            SessionAttachOverlayOutcome::Close
        );
    }

    #[test]
    fn selection_clamps_within_filtered_rows() {
        let mut overlay = open(entries());
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
    fn visible_lines_are_bounded_by_body_height() {
        let overlay = open(entries());
        assert_eq!(overlay.visible_lines(80, 2).len(), 2);
        assert!(overlay.visible_lines(80, 10).len() <= 10);
    }

    #[test]
    fn control_chars_in_session_name_are_sanitized() {
        let overlay = open(vec![session("s-0001-aaaa", "evil\u{1b}[31m\u{7}", 1)]);
        let lines = overlay.visible_lines(120, 10);
        assert!(!lines[1].text.contains('\u{1b}'));
        assert!(!lines[1].text.contains('\u{7}'));
    }

    // ── UX4-P1 click→Activate parity ───────────────────────────────────────

    #[test]
    fn click_row_zero_is_the_query_prompt_not_a_row() {
        let mut overlay = open(entries());
        let _ = overlay.visible_lines(80, 10);
        // Row 0 is the `> query` prompt — never a selectable row.
        assert!(overlay.row_at(0, 10).is_none());
        assert!(!overlay.click_row(0, 10));
    }

    #[test]
    fn click_row_selects_same_cursor_as_down_then_activate() {
        // Press on body row N (1-based after the prompt) must select the SAME
        // entry as Down×(N-1), so click→Activate == Down×(N-1)+Activate.
        for target in 0..3 {
            let mut by_click = open(entries());
            let _ = by_click.visible_lines(80, 10);
            assert!(
                by_click.click_row(target + 1, 10),
                "row {target} selectable"
            );
            let click_attach = by_click.handle_input(OverlayInput::Activate);

            let mut by_keys = open(entries());
            for _ in 0..target {
                by_keys.handle_input(OverlayInput::Down);
            }
            let key_attach = by_keys.handle_input(OverlayInput::Activate);

            assert_eq!(click_attach, key_attach, "row {target} parity");
        }
    }

    #[test]
    fn click_past_last_row_is_inert() {
        let mut overlay = open(entries());
        let _ = overlay.visible_lines(80, 10);
        // 3 entries → rows 1,2,3 valid; row 4 is past the end.
        assert!(overlay.row_at(4, 10).is_none());
        assert!(!overlay.click_row(4, 10));
    }

    #[test]
    fn click_on_empty_overlay_hint_is_inert() {
        let mut overlay = open(Vec::new());
        let _ = overlay.visible_lines(80, 10);
        // The hint line (row 1) is not a selectable row.
        assert!(!overlay.click_row(1, 10));
    }
}

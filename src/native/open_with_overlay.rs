// SPDX-License-Identifier: GPL-3.0-only
//! Native "Open With…" app-picker overlay (C3b).
//!
//! A near-1:1 clone of the session-attach summon overlay
//! (`session_attach_overlay.rs`), specialized over [`DesktopApp`] rows: a
//! presentation-only overlay that lists the applications that can open a
//! resolved file (enumerated App-side via `crate::desktop::enumerate_open_with`),
//! type-to-filters them, and on Enter emits
//! [`OpenWithOverlayOutcome::Open`] carrying the chosen app's fully-expanded,
//! argv-only command for the App to hand to `spawn_detached`. Like the overlay
//! it clones, it owns a frozen list captured at open time, never writes to the
//! PTY, and never mutates the live terminal model.
//!
//! Security/safety: every row's `argv` was built by
//! [`crate::desktop::exec_to_argv`] (Desktop-Entry quoting, NOT shell) before it
//! ever reached this overlay; this module only displays the app `Name` and
//! forwards the pre-built vector. App names are third-party text, so they are
//! control-char-sanitized before display exactly like session titles — a
//! malformed `Name` can never inject escape sequences into the plain-text rows.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::desktop::DesktopApp;
use crate::fuzzy;

use super::overlay::OverlayInput;

/// Maximum rows rendered (keeps the overlay compact and the fuzzy ranking
/// bounded). The enumeration is already capped at `desktop::MAX_OPEN_WITH`, so
/// this is a defensive ceiling that matches the other list overlays.
const MAX_RESULTS: usize = 40;

#[derive(Debug, Clone, Default)]
pub(super) struct OpenWithOverlay {
    /// The frozen app list captured at open time, in enumeration (best-first)
    /// order.
    entries: Vec<DesktopApp>,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll_offset: Cell<usize>,
    last_body_height: Cell<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OpenWithOverlayOutcome {
    Consumed,
    Close,
    /// The user accepted an app. Carries the pre-built, argv-only command (the
    /// path already substituted as one inert element); the App spawns it via the
    /// shared `spawn_detached`. This overlay never spawns anything itself.
    Open(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenWithOverlayLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenWithOverlaySignature {
    pub(super) query: String,
    pub(super) selected: Option<usize>,
    pub(super) results_len: usize,
    pub(super) results_fingerprint: u64,
}

impl OpenWithOverlay {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Load a frozen set of apps and reset the query/cursor. The list is owned
    /// by the overlay, so it stays stable while open.
    pub(super) fn open(&mut self, entries: Vec<DesktopApp>) {
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

    fn selected_entry(&self) -> Option<&DesktopApp> {
        let entry_index = *self.filtered.get(self.selected)?;
        self.entries.get(entry_index)
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> OpenWithOverlayOutcome {
        match input {
            OverlayInput::Close => OpenWithOverlayOutcome::Close,
            OverlayInput::Up => {
                self.move_selection(-1);
                self.follow_selection_for_known_body_height();
                OpenWithOverlayOutcome::Consumed
            }
            OverlayInput::Down => {
                self.move_selection(1);
                self.follow_selection_for_known_body_height();
                OpenWithOverlayOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                OpenWithOverlayOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
                self.move_selection(MAX_RESULTS as isize);
                self.follow_selection_for_known_body_height();
                OpenWithOverlayOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.query.pop();
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                OpenWithOverlayOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                OpenWithOverlayOutcome::Consumed
            }
            OverlayInput::Activate => match self.selected_entry() {
                Some(entry) => OpenWithOverlayOutcome::Open(entry.argv.clone()),
                None => OpenWithOverlayOutcome::Consumed,
            },
            OverlayInput::Char(_)
            | OverlayInput::Left
            | OverlayInput::Right
            | OverlayInput::Save
            | OverlayInput::Tab => OpenWithOverlayOutcome::Consumed,
        }
    }

    pub(super) fn scroll_lines(&mut self, lines: isize) {
        self.move_selection(lines.signum());
        self.follow_selection_for_known_body_height();
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<OpenWithOverlayLine> {
        if body_height == 0 {
            self.last_body_height.set(0);
            self.scroll_offset.set(0);
            return Vec::new();
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2));
        lines.push(OpenWithOverlayLine {
            text: truncate_for_width(&format!("> {}", self.query), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        if self.entries.is_empty() {
            self.scroll_offset.set(0);
            lines.push(OpenWithOverlayLine {
                text: truncate_for_width("No applications found to open this file.", body_width),
                focused: false,
                bold: false,
            });
            return lines;
        }
        if self.filtered.is_empty() {
            self.scroll_offset.set(0);
            lines.push(OpenWithOverlayLine {
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
            lines.push(OpenWithOverlayLine {
                text: truncate_for_width(&row_label(entry), body_width),
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
        columns.min(84)
    }

    pub(super) fn render_signature(&self) -> OpenWithOverlaySignature {
        OpenWithOverlaySignature {
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
                entry.id.hash(&mut hasher);
                entry.name.hash(&mut hasher);
                entry.argv.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// Searchable text for one app: the (sanitized) name plus its desktop id, so a
/// user can fuzzy-match on either.
fn match_text(entry: &DesktopApp) -> String {
    let mut text = sanitize(&entry.name);
    text.push(' ');
    text.push_str(&entry.id);
    text
}

/// Render one app row: just the (sanitized) display name. The argv is not shown
/// (it can be long and contains the path); the name is the user-facing choice.
fn row_label(entry: &DesktopApp) -> String {
    sanitize(&entry.name)
}

/// Strip control characters so a malformed app `Name` can never inject escape
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

    fn app(id: &str, name: &str, argv: &[&str]) -> DesktopApp {
        DesktopApp {
            id: id.to_owned(),
            name: name.to_owned(),
            argv: argv.iter().map(|s| s.to_owned().to_owned()).collect(),
        }
    }

    fn entries() -> Vec<DesktopApp> {
        vec![
            app("eog.desktop", "Image Viewer", &["eog", "/x/a.png"]),
            app("gimp.desktop", "GIMP", &["gimp", "/x/a.png"]),
            app("krita.desktop", "Krita", &["krita", "/x/a.png"]),
        ]
    }

    fn open(entries: Vec<DesktopApp>) -> OpenWithOverlay {
        let mut overlay = OpenWithOverlay::new();
        overlay.open(entries);
        overlay
    }

    fn type_query(overlay: &mut OpenWithOverlay, query: &str) {
        for ch in query.chars() {
            assert_eq!(
                overlay.handle_input(OverlayInput::Char(ch)),
                OpenWithOverlayOutcome::Consumed
            );
        }
    }

    #[test]
    fn empty_query_lists_all_in_order() {
        let overlay = open(entries());
        assert_eq!(overlay.render_signature().results_len, 3);
        let lines = overlay.visible_lines(80, 10);
        assert!(lines[1].text.starts_with("Image Viewer"));
        assert!(lines[2].text.starts_with("GIMP"));
        assert!(lines[3].text.starts_with("Krita"));
    }

    #[test]
    fn fuzzy_filter_ranks_match_first() {
        let mut overlay = open(entries());
        type_query(&mut overlay, "gimp");
        let signature = overlay.render_signature();
        assert_eq!(signature.results_len, 1);
        let lines = overlay.visible_lines(80, 10);
        assert!(lines[1].text.starts_with("GIMP"));
    }

    #[test]
    fn accept_emits_open_with_chosen_argv() {
        let mut overlay = open(entries());
        overlay.handle_input(OverlayInput::Down); // select GIMP
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OpenWithOverlayOutcome::Open(vec!["gimp".to_owned(), "/x/a.png".to_owned()])
        );
    }

    #[test]
    fn no_match_activate_is_inert() {
        let mut overlay = open(entries());
        type_query(&mut overlay, "zzzznope");
        assert_eq!(overlay.render_signature().results_len, 0);
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OpenWithOverlayOutcome::Consumed
        );
    }

    #[test]
    fn empty_overlay_shows_hint_and_activate_inert() {
        let mut overlay = open(Vec::new());
        assert_eq!(overlay.entry_count(), 0);
        let lines = overlay.visible_lines(80, 10);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].text.contains("No applications"));
        assert_eq!(
            overlay.handle_input(OverlayInput::Activate),
            OpenWithOverlayOutcome::Consumed
        );
    }

    #[test]
    fn close_input_requests_close() {
        let mut overlay = open(entries());
        assert_eq!(
            overlay.handle_input(OverlayInput::Close),
            OpenWithOverlayOutcome::Close
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
    fn visible_lines_bounded_by_body_height() {
        let overlay = open(entries());
        assert_eq!(overlay.visible_lines(80, 2).len(), 2);
        assert!(overlay.visible_lines(80, 10).len() <= 10);
    }

    #[test]
    fn control_chars_in_name_are_sanitized() {
        let overlay = open(vec![app(
            "evil.desktop",
            "evil\u{1b}[31m\u{7}",
            &["evil", "/x/a.png"],
        )]);
        let lines = overlay.visible_lines(120, 10);
        assert!(!lines[1].text.contains('\u{1b}'));
        assert!(!lines[1].text.contains('\u{7}'));
    }

    #[test]
    fn fuzzy_matches_id_not_only_name() {
        let mut overlay = open(entries());
        // "krita" is the name; match on the id stem too.
        type_query(&mut overlay, "krita.desktop");
        assert_eq!(overlay.render_signature().results_len, 1);
        assert!(matches!(
            overlay.handle_input(OverlayInput::Activate),
            OpenWithOverlayOutcome::Open(argv) if argv[0] == "krita"
        ));
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Named launch-profile picker for direct New Tab / New Workspace menu routes.
//!
//! Catalog load happens only when the picker opens; the default New Tab and New
//! Workspace actions stay one-click and never touch profile enumeration.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::fuzzy;

use super::overlay::OverlayInput;

const MAX_RESULTS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ProfilePickerPurpose {
    #[default]
    NewTab,
    NewWorkspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfilePickerEntry {
    pub(super) name: String,
    pub(super) label: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ProfilePicker {
    entries: Vec<ProfilePickerEntry>,
    purpose: ProfilePickerPurpose,
    query: String,
    filtered: Vec<usize>,
    selected: usize,
    scroll_offset: Cell<usize>,
    last_body_height: Cell<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProfilePickerOutcome {
    Consumed,
    Close,
    NewTab(String),
    NewWorkspace(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfilePickerLine {
    pub(super) text: String,
    pub(super) focused: bool,
    pub(super) bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfilePickerSignature {
    pub(super) query: String,
    pub(super) selected: Option<usize>,
    pub(super) results_len: usize,
    pub(super) results_fingerprint: u64,
}

impl ProfilePicker {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn open(&mut self, entries: Vec<ProfilePickerEntry>, purpose: ProfilePickerPurpose) {
        self.purpose = purpose;
        self.entries = entries;
        self.query.clear();
        self.selected = 0;
        self.reset_scroll();
        self.recompute();
    }

    fn recompute(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.entries.len()).take(MAX_RESULTS).collect();
        } else {
            let haystacks: Vec<String> = self
                .entries
                .iter()
                .map(|entry| sanitize(&format!("{} {}", entry.name, entry.label)))
                .collect();
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

    fn empty_line_text(&self) -> String {
        if self.entries.is_empty() {
            "No launch profiles yet \u{2014} create one in Settings".to_owned()
        } else {
            "No matches".to_owned()
        }
    }

    fn selected_entry(&self) -> Option<&ProfilePickerEntry> {
        let entry_index = *self.filtered.get(self.selected)?;
        self.entries.get(entry_index)
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ProfilePickerOutcome {
        match input {
            OverlayInput::Close => ProfilePickerOutcome::Close,
            OverlayInput::Up => {
                self.move_selection(-1);
                self.follow_selection_for_known_body_height();
                ProfilePickerOutcome::Consumed
            }
            OverlayInput::Down => {
                self.move_selection(1);
                self.follow_selection_for_known_body_height();
                ProfilePickerOutcome::Consumed
            }
            OverlayInput::PageUp | OverlayInput::Home => {
                self.move_selection(-(MAX_RESULTS as isize));
                self.follow_selection_for_known_body_height();
                ProfilePickerOutcome::Consumed
            }
            OverlayInput::PageDown | OverlayInput::End => {
                self.move_selection(MAX_RESULTS as isize);
                self.follow_selection_for_known_body_height();
                ProfilePickerOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.query.pop();
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                ProfilePickerOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.recompute();
                self.reset_scroll();
                self.follow_selection_for_known_body_height();
                ProfilePickerOutcome::Consumed
            }
            OverlayInput::Activate => match self.purpose {
                ProfilePickerPurpose::NewTab => match self.selected_entry() {
                    Some(entry) => ProfilePickerOutcome::NewTab(entry.name.clone()),
                    None => ProfilePickerOutcome::Consumed,
                },
                ProfilePickerPurpose::NewWorkspace => match self.selected_entry() {
                    Some(entry) => ProfilePickerOutcome::NewWorkspace(entry.name.clone()),
                    None => ProfilePickerOutcome::Consumed,
                },
            },
            OverlayInput::Char(_)
            | OverlayInput::Left
            | OverlayInput::Right
            | OverlayInput::Save
            | OverlayInput::ActivateAlt
            | OverlayInput::Tab => ProfilePickerOutcome::Consumed,
        }
    }

    pub(super) fn scroll_lines(&mut self, lines: isize) {
        self.move_selection(lines.signum());
        self.follow_selection_for_known_body_height();
    }

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
    ) -> Vec<ProfilePickerLine> {
        if body_height == 0 {
            self.last_body_height.set(0);
            self.scroll_offset.set(0);
            return Vec::new();
        }
        let scroll_offset = self.scroll_offset_for_body_height(body_height);
        let mut lines = Vec::with_capacity(body_height.min(MAX_RESULTS + 2));
        lines.push(ProfilePickerLine {
            text: truncate_for_width(&format!("> {}", self.query), body_width),
            focused: false,
            bold: true,
        });
        if lines.len() >= body_height {
            return lines;
        }
        if self.filtered.is_empty() {
            self.scroll_offset.set(0);
            lines.push(ProfilePickerLine {
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
            lines.push(ProfilePickerLine {
                text: truncate_for_width(&sanitize(&entry.label), body_width),
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

    pub(super) fn title(&self) -> &'static str {
        match self.purpose {
            ProfilePickerPurpose::NewTab => "New Tab with Profile\u{2026}",
            ProfilePickerPurpose::NewWorkspace => "New Workspace with Profile\u{2026}",
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        columns.min(72)
    }

    pub(super) fn render_signature(&self) -> ProfilePickerSignature {
        ProfilePickerSignature {
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
                entry.name.hash(&mut hasher);
                entry.label.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

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

    fn entries() -> Vec<ProfilePickerEntry> {
        vec![
            ProfilePickerEntry {
                name: "dev".to_owned(),
                label: "Dev Shell".to_owned(),
            },
            ProfilePickerEntry {
                name: "edge".to_owned(),
                label: "Edge".to_owned(),
            },
        ]
    }

    fn open(purpose: ProfilePickerPurpose) -> ProfilePicker {
        let mut picker = ProfilePicker::new();
        picker.open(entries(), purpose);
        picker
    }

    fn type_query(picker: &mut ProfilePicker, query: &str) {
        for ch in query.chars() {
            assert_eq!(
                picker.handle_input(OverlayInput::Char(ch)),
                ProfilePickerOutcome::Consumed
            );
        }
    }

    #[test]
    fn keyboard_navigation_and_new_tab_accept() {
        let mut picker = open(ProfilePickerPurpose::NewTab);
        assert_eq!(
            picker.handle_input(OverlayInput::Down),
            ProfilePickerOutcome::Consumed
        );
        assert_eq!(
            picker.handle_input(OverlayInput::Activate),
            ProfilePickerOutcome::NewTab("edge".to_owned())
        );
    }

    #[test]
    fn new_workspace_accept_carries_profile_name() {
        let mut picker = open(ProfilePickerPurpose::NewWorkspace);
        assert_eq!(
            picker.handle_input(OverlayInput::Activate),
            ProfilePickerOutcome::NewWorkspace("dev".to_owned())
        );
    }

    #[test]
    fn fuzzy_filter_narrows_rows() {
        let mut picker = open(ProfilePickerPurpose::NewTab);
        type_query(&mut picker, "edge");
        assert_eq!(
            picker.handle_input(OverlayInput::Activate),
            ProfilePickerOutcome::NewTab("edge".to_owned())
        );
    }

    #[test]
    fn empty_catalog_shows_teaching_line() {
        let mut picker = ProfilePicker::new();
        picker.open(Vec::new(), ProfilePickerPurpose::NewTab);
        let lines = picker.visible_lines(40, 4);
        assert!(
            lines
                .iter()
                .any(|line| line.text.contains("No launch profiles yet")),
            "empty catalog explains how to add profiles"
        );
        assert_eq!(
            picker.handle_input(OverlayInput::Activate),
            ProfilePickerOutcome::Consumed
        );
    }

    #[test]
    fn escape_closes_without_selection() {
        let mut picker = open(ProfilePickerPurpose::NewTab);
        assert_eq!(
            picker.handle_input(OverlayInput::Close),
            ProfilePickerOutcome::Close
        );
    }
}

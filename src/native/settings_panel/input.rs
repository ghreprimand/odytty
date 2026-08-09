// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl SettingsPanel {
    pub(in crate::native) fn handle_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        // Guard order is load-bearing (T-two-substates):
        // 1. Path picker owns all input while open.
        // 2. Dirty-close prompt owns all input while showing (T8).
        // 3. Text edit owns keystrokes before search.
        // 4. Search active.
        // 5. Level dispatch.
        if self.path_picker.is_some() {
            return self.handle_path_picker_input(input);
        }
        if self.pending_close_prompt {
            return self.handle_close_prompt_input(input);
        }
        if self.editing.is_some() {
            return self.handle_editing_input(input);
        }
        if self.search_active {
            return self.handle_search_input(input);
        }

        match self.level {
            SettingsLevel::SectionList => self.handle_section_list_input(input),
            SettingsLevel::SectionDetail { section_index } => {
                self.handle_section_detail_input(input, section_index)
            }
            SettingsLevel::About => self.handle_about_input(input),
        }
    }

    /// Populate the read-only About data (called when the overlay opens). Cheap
    /// to recompute; held so the About view renders without per-frame work.
    pub(in crate::native) fn set_about(&mut self, about: AboutInfo) {
        self.about = Some(about);
    }

    // ── Level 2 (ABOUT): read-only About view dispatch ─────────────────────

    pub(super) fn handle_about_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        match input {
            OverlayInput::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            OverlayInput::Down => {
                self.selected = (self.selected + 1).min(ABOUT_ACTION_ROWS - 1);
            }
            OverlayInput::Home => self.selected = 0,
            OverlayInput::End => self.selected = ABOUT_ACTION_ROWS - 1,
            OverlayInput::Activate | OverlayInput::Char(' ') => {
                return self.activate_about_row();
            }
            OverlayInput::Close | OverlayInput::Left => {
                self.message = None;
                self.back_to_section_list();
            }
            _ => {}
        }
        SettingsPanelOutcome::Consumed
    }

    /// Act on the focused About row: open a project link, or copy diagnostics.
    pub(super) fn activate_about_row(&mut self) -> SettingsPanelOutcome {
        if self.selected == ABOUT_COPY_ROW {
            let text = self
                .about
                .as_ref()
                .map(AboutInfo::diagnostics_block)
                .unwrap_or_default();
            self.message = Some("Diagnostics copied to clipboard.".to_owned());
            return SettingsPanelOutcome::CopyToClipboard(text);
        }
        if let Some(link) = ABOUT_LINKS.get(self.selected) {
            self.message = Some(format!("Opening {}.", link.label));
            return SettingsPanelOutcome::OpenUrl(link.url.to_owned());
        }
        SettingsPanelOutcome::Consumed
    }

    // ── Level 1: section-list dispatch ──────────────────────────────────────

    pub(super) fn handle_section_list_input(
        &mut self,
        input: OverlayInput,
    ) -> SettingsPanelOutcome {
        match input {
            OverlayInput::Up => self.move_section_selection(-1),
            OverlayInput::Down => self.move_section_selection(1),
            OverlayInput::PageUp => self.move_section_selection(-4),
            OverlayInput::PageDown => self.move_section_selection(4),
            OverlayInput::Home => {
                self.section_selected = 0;
                self.follow_section_selection();
            }
            OverlayInput::End => {
                // Last row is the synthetic "About" row at index SECTIONS.len().
                self.section_selected = SECTIONS.len();
                self.follow_section_selection();
            }
            OverlayInput::Activate | OverlayInput::Right => {
                let idx = self.section_selected;
                self.drill_into_section(idx);
            }
            OverlayInput::Close => {
                // Esc at Level 1 with dirty edits → show save/discard prompt.
                // Esc at Level 1 clean → close.
                if self.edits.changed_count() > 0 {
                    self.pending_close_prompt = true;
                } else {
                    return SettingsPanelOutcome::Close;
                }
            }
            OverlayInput::Save => return self.save_changes(),
            // `/` enters search mode (T-search-vs-level: only at Level 1).
            OverlayInput::Char('/') => {
                self.search_active = true;
                self.query.clear();
                self.apply_search_filter();
            }
            _ => {}
        }
        SettingsPanelOutcome::Consumed
    }

    // ── Level 2: setting-entry dispatch ────────────────────────────────────

    pub(super) fn handle_section_detail_input(
        &mut self,
        input: OverlayInput,
        _section_index: usize,
    ) -> SettingsPanelOutcome {
        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-6),
            OverlayInput::PageDown => self.move_selection(6),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => {
                self.set_selection(self.entries.len().saturating_sub(1));
            }
            OverlayInput::Left => return self.step_or_cycle_selected(-1),
            OverlayInput::Right => return self.step_or_cycle_selected(1),
            OverlayInput::Activate => return self.activate_selected(),
            OverlayInput::Save => return self.save_changes(),
            OverlayInput::Char('b') | OverlayInput::Char('B')
                if self
                    .selected_entry()
                    .is_some_and(|entry| entry.key == "theme") =>
            {
                self.message = Some("Opening theme builder.".to_owned());
                return SettingsPanelOutcome::OpenThemeBuilder;
            }
            OverlayInput::Char(' ') => return self.activate_selected(),
            OverlayInput::Close => {
                // Esc at Level 2: clear edit/picker state and go back to Level 1.
                // T-editing-clears-on-level-change: editing is cleared here.
                // T-changed-count-survives: edits are NOT touched.
                self.editing = None;
                self.path_picker = None;
                self.message = None;
                self.back_to_section_list();
            }
            // T-search-vs-level: `/` is inert at Level 2.
            _ => {}
        }
        SettingsPanelOutcome::Consumed
    }

    /// Return from Level 2 to Level 1, restoring the full entry list.
    pub(super) fn back_to_section_list(&mut self) {
        self.level = SettingsLevel::SectionList;
        self.entries = self.all_entries.clone();
    }

    pub(in crate::native) fn current_level(&self) -> SettingsLevel {
        self.level
    }

    pub(in crate::native) fn set_level(&mut self, level: SettingsLevel) {
        self.level = level;
    }

    // ── Dirty-close prompt ──────────────────────────────────────────────────

    pub(super) fn handle_close_prompt_input(
        &mut self,
        input: OverlayInput,
    ) -> SettingsPanelOutcome {
        // T8: while the prompt is showing, ALL input is consumed here.
        // Ctrl+S maps to Save-and-close (does NOT fire the normal save path).
        match input {
            OverlayInput::Char('s')
            | OverlayInput::Char('S')
            | OverlayInput::Activate
            | OverlayInput::Save => {
                let changes = self.edits.changes();
                self.pending_close_prompt = false;
                SettingsPanelOutcome::SaveAndClose(changes)
            }
            OverlayInput::Char('d') | OverlayInput::Char('D') => {
                self.pending_close_prompt = false;
                SettingsPanelOutcome::DiscardAndClose
            }
            OverlayInput::Char('c') | OverlayInput::Char('C') | OverlayInput::Close => {
                self.pending_close_prompt = false;
                SettingsPanelOutcome::Consumed
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    // ── Path picker ─────────────────────────────────────────────────────────

    pub(super) fn handle_path_picker_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        let Some(mut picker) = self.path_picker.take() else {
            return SettingsPanelOutcome::Consumed;
        };
        let key = picker.key;
        match picker.handle_input(input) {
            PathPickerOutcome::Selected(path_str) => {
                self.path_picker = None;
                self.commit_value(key, &path_str)
            }
            PathPickerOutcome::Cancelled => {
                self.path_picker = None;
                self.message = Some(format!("Cancelled path selection for {key}."));
                SettingsPanelOutcome::Consumed
            }
            PathPickerOutcome::Consumed => {
                self.path_picker = Some(picker);
                SettingsPanelOutcome::Consumed
            }
        }
    }

    // ── Search ──────────────────────────────────────────────────────────────

    /// Handle a key while the search filter is active (OB-SEARCH). Only
    /// available at Level 1. Enter on a result drills into the section that
    /// owns the entry, then selects it at Level 2 (T-search-vs-level).
    pub(super) fn handle_search_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        match input {
            OverlayInput::Close => {
                if self.query.is_empty() {
                    self.search_active = false;
                    self.entries = self.all_entries.clone();
                    self.clamp();
                } else {
                    self.query.clear();
                    self.apply_search_filter();
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Backspace => {
                self.query.pop();
                self.apply_search_filter();
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Up => {
                self.move_selection(-1);
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Down => {
                self.move_selection(1);
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::PageUp => {
                self.move_selection(-6);
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::PageDown => {
                self.move_selection(6);
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Home => {
                self.set_selection(0);
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::End => {
                self.set_selection(self.entries.len().saturating_sub(1));
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Left => self.step_or_cycle_selected(-1),
            OverlayInput::Right => self.step_or_cycle_selected(1),
            OverlayInput::Save => self.save_changes(),
            // Enter/Space on a search result: exit search, drill into the
            // entry's section, and select it at Level 2.
            OverlayInput::Activate | OverlayInput::Char(' ') => {
                if let Some(entry) = self.selected_entry().cloned() {
                    // Find the section that owns this entry's group.
                    if let Some(si) = SECTIONS
                        .iter()
                        .position(|s| s.groups.contains(&entry.group))
                    {
                        self.search_active = false;
                        self.query.clear();
                        self.drill_into_section(si);
                        // Select the entry within the Level-2 list.
                        if let Some(pos) = self.entries.iter().position(|e| e.key == entry.key) {
                            self.selected = pos;
                            self.clamp();
                        }
                        return SettingsPanelOutcome::Consumed;
                    }
                }
                // Fallback: activate the selected entry as usual.
                let key_before = self.selected_entry().map(|e| e.key);
                let outcome = self.activate_selected();
                if self.editing.is_some() {
                    self.exit_search_preserving(key_before);
                }
                outcome
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.apply_search_filter();
                SettingsPanelOutcome::Consumed
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    pub(super) fn apply_search_filter(&mut self) {
        if self.query.is_empty() {
            self.entries = self.all_entries.clone();
        } else {
            let needle = self.query.to_lowercase();
            self.entries = self
                .all_entries
                .iter()
                .filter(|entry| matches_query(entry, &needle))
                .cloned()
                .collect();
        }
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.clamp();
    }

    pub(super) fn exit_search_preserving(&mut self, key: Option<&'static str>) {
        self.search_active = false;
        self.query.clear();
        self.entries = self.all_entries.clone();
        if let Some(key) = key
            && let Some(pos) = self.entries.iter().position(|entry| entry.key == key)
        {
            self.selected = pos;
        }
        self.clamp();
    }

    #[allow(dead_code)]
    pub(in crate::native) fn is_searching(&self) -> bool {
        self.search_active
    }
}

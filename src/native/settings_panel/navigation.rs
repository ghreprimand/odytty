// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl SettingsPanel {
    // ── Drilling and level navigation ───────────────────────────────────────

    /// Build the Level-2 entry list for the section at `section_index`: the
    /// group-filtered real settings, plus any synthetic action rows the section
    /// offers. The Themes section appends an "Open Theme Builder" action at the
    /// end (v0.3.1 discoverability). Both `drill_into_section` and the
    /// post-commit `refresh_entries_after_commit` build through this so the
    /// action row survives a live value-sync rebuild.
    pub(super) fn section_entries(&self, section_index: usize) -> Vec<SettingInfo> {
        let Some(section) = SECTIONS.get(section_index) else {
            return Vec::new();
        };
        let mut entries: Vec<SettingInfo> = self
            .all_entries
            .iter()
            .filter(|e| section.groups.contains(&e.group))
            .cloned()
            .collect();
        if section.name == "Themes" {
            entries.push(theme_builder_action_entry());
        }
        if section.name == "Profiles" {
            entries.push(profile_manager_action_entry());
        }
        entries
    }

    /// Drill into section `section_index`: filter `entries` to the section's
    /// groups, reset Level-2 scroll/selection to the top, and update `level`.
    /// Clears editing, path_picker, and message (T-editing-clears-on-level-change).
    pub(in crate::native) fn drill_into_section(&mut self, section_index: usize) {
        // The synthetic "About" row sits just past the real SECTIONS.
        if section_index == SECTIONS.len() {
            self.selected = 0;
            self.scroll = 0;
            self.editing = None;
            self.path_picker = None;
            self.message = None;
            self.level = SettingsLevel::About;
            return;
        }
        if SECTIONS.get(section_index).is_none() {
            return;
        }
        self.entries = self.section_entries(section_index);
        // Reset Level-2 state (T-scroll-per-level: entering starts at top).
        self.selected = 0;
        self.scroll = 0;
        self.editing = None;
        self.path_picker = None;
        self.message = None;
        self.level = SettingsLevel::SectionDetail { section_index };
        self.clamp();
    }

    /// Open directly inside the named Level-1 section. Context-menu launchers
    /// use this to preserve the panel's normal two-level navigation while
    /// landing on the settings related to the clicked chrome surface.
    pub(in crate::native) fn open_section(&mut self, name: &str) {
        let Some(section_index) = SECTIONS.iter().position(|section| section.name == name) else {
            return;
        };
        self.query.clear();
        self.search_active = false;
        self.section_selected = section_index;
        // Preserve the target as the Level-1 selection without pinning it to
        // the first visible row. The panel is long-lived, so a non-zero scroll
        // here would leak into the next generic open after back navigation.
        self.section_scroll = 0;
        self.pending_close_prompt = false;
        self.drill_into_section(section_index);
    }

    /// Test seam: the value string the panel would RENDER for `key`, read from
    /// the master inventory (`all_entries`) the filtered view derives from. Pins
    /// the panel-coherence bug: an external-chrome mutation applied while the
    /// panel is open (or before it opens) must leave this reflecting the live
    /// value, not a stale pre-toggle copy.
    #[cfg(test)]
    pub(in crate::native) fn displayed_value_for_test(&self, key: &str) -> Option<String> {
        self.all_entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.clone())
    }

    #[cfg(test)]
    pub(in crate::native) fn active_section_name_for_test(&self) -> Option<&'static str> {
        match self.level {
            SettingsLevel::SectionDetail { section_index } => {
                SECTIONS.get(section_index).map(|section| section.name)
            }
            SettingsLevel::SectionList | SettingsLevel::About => None,
        }
    }

    pub(super) fn move_section_selection(&mut self, delta: isize) {
        // +1 for the synthetic "About" row appended after SECTIONS.
        let n = (SECTIONS.len() + 1) as isize;
        let next = (self.section_selected as isize + delta).clamp(0, n - 1) as usize;
        self.section_selected = next;
        self.follow_section_selection();
    }

    /// Whether the body has hidden rows above / below the visible window, for
    /// the scroll affordance (OVERLAY-SMALL-WINDOW). Approximate but stable:
    /// `(false, false)` whenever everything fits, so a normal window draws no
    /// arrows and stays byte-identical. Level 1 reserves one body row for the
    /// footer hint; Level 2 / search compares the entry scroll against the count.
    pub(in crate::native) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        if body_height == 0 {
            return (false, false);
        }
        if self.path_picker.is_some() {
            // The path picker is its own sub-list; it manages its own window and
            // is left without an arrow affordance for now.
            return (false, false);
        }
        if matches!(self.level, SettingsLevel::SectionList) && !self.search_active {
            let window = body_height.saturating_sub(1).max(1);
            // +1 for the synthetic "About" row appended after SECTIONS.
            let total = SECTIONS.len() + 1;
            return (
                self.section_scroll > 0,
                self.section_scroll + window < total,
            );
        }
        let total = self.entries.len();
        // SETTINGS-COMPACT: the fixed help footer steals body rows, so compare
        // the entry scroll against the shrunk content window, not the full body.
        let window = body_height.saturating_sub(settings_detail_footer_reserve(body_height));
        (self.scroll > 0, self.scroll + window < total)
    }

    /// Keep the selected section inside the Level-1 visible window by adjusting
    /// `section_scroll` (OVERLAY-SMALL-WINDOW). Without this, ArrowDown on a
    /// short window walked the selection off-screen while the view stayed put.
    /// The footer hint consumes one body row when there is room, so the section
    /// viewport is `last_body_height - 1` rows (min 1).
    pub(super) fn follow_section_selection(&mut self) {
        let window = self.last_body_height.saturating_sub(1).max(1);
        if self.section_selected < self.section_scroll {
            self.section_scroll = self.section_selected;
        } else if self.section_selected >= self.section_scroll + window {
            self.section_scroll = self.section_selected + 1 - window;
        }
        let max_scroll = SECTIONS.len().saturating_sub(1);
        self.section_scroll = self.section_scroll.min(max_scroll);
    }

    pub(super) fn selected_entry(&self) -> Option<&SettingInfo> {
        self.entries.get(self.selected)
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        let next = self.selected as isize + delta;
        self.set_selection(next.clamp(0, self.entries.len().saturating_sub(1) as isize) as usize);
    }

    pub(super) fn set_selection(&mut self, selected: usize) {
        self.selected = selected.min(self.entries.len().saturating_sub(1));
        self.clamp();
    }

    pub(super) fn clamp(&mut self) {
        // Level 1 fast path: clamp section_selected only; the Level-2
        // selected/scroll are stale but harmless while at Level 1
        // (T-scroll-per-level). `selected_in_window` must not run here because
        // Level-1 rows are SectionRow, not Value/Slider.
        if matches!(self.level, SettingsLevel::SectionList) && !self.search_active {
            if SECTIONS.is_empty() {
                self.section_selected = 0;
                self.section_scroll = 0;
            } else {
                self.section_selected = self.section_selected.min(SECTIONS.len() - 1);
                self.section_scroll = self.section_scroll.min(SECTIONS.len() - 1);
            }
            return;
        }

        // Level 2 / search mode: existing clamp logic.
        if self.entries.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = self.selected.min(self.entries.len() - 1);
        // SLIDER-SCROLL-STABILITY: scroll MINIMALLY, only when the selected row
        // is genuinely off-screen. Never recenter a row that is already visible
        // (the old `visible_slack` reframe yanked the viewport on every press of
        // any row below the top third — that is what jumped a slider to the
        // bottom on adjust). Scroll up to reveal a selection above the window;
        // scroll DOWN one row at a time only until the selection becomes visible
        // (preserves keyboard follow-visible without recentering — see the
        // [follow-lag] trap).
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.last_body_height > 0 {
            while self.scroll < self.selected && !self.selected_in_window(self.last_body_height) {
                self.scroll += 1;
            }
        }
        self.scroll = self.scroll.min(self.entries.len() - 1);
    }
}

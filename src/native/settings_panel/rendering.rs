// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl SettingsPanel {
    // ── Render ───────────────────────────────────────────────────────────────

    pub(in crate::native) fn render_signature(&self) -> SettingsPanelSignature {
        SettingsPanelSignature {
            selected: self.selected,
            scroll: self.scroll,
            editing_key: self.editing.as_ref().map(|edit| edit.key),
            editing_buffer: self.editing.as_ref().map(|edit| edit.buffer.clone()),
            changed_count: self.edits.changed_count(),
            message: self.message.clone(),
            entries: self
                .entries
                .iter()
                .map(|entry| SettingsPanelEntrySignature {
                    key: entry.key,
                    value: entry.value.clone(),
                    description: entry.description,
                })
                .collect(),
            query: self.query.clone(),
            search_active: self.search_active,
            level: self.level,
            section_selected: self.section_selected,
            section_scroll: self.section_scroll,
            pending_close_prompt: self.pending_close_prompt,
            path_picker: self
                .path_picker
                .as_ref()
                .map(PathPickerState::render_signature),
        }
    }

    pub(in crate::native) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        let content_width = self
            .entries
            .iter()
            .map(|entry| entry.name.chars().count() + entry.value.chars().count() + 8)
            .max()
            .unwrap_or(48)
            .max((columns * 3 / 4).max(80));
        content_width.saturating_add(4).min(columns)
    }

    /// The rendered body lines, projected from the shared row walker so they can
    /// never drift from the pointer hit-map.
    pub(in crate::native) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<SettingsPanelLine> {
        self.build_visible_rows(body_width, body_height)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    pub(super) fn display_value(&self, entry: &SettingInfo) -> String {
        if let Some(edit) = self.editing.as_ref().filter(|edit| edit.key == entry.key) {
            return format!("[{}]", edit.buffer);
        }
        let changed = self
            .edits
            .changes()
            .iter()
            .any(|change| change.key == entry.key);
        if changed {
            format!("{} *", entry.value)
        } else {
            entry.value.clone()
        }
    }
}

// ── Private helpers ──────────────────────────────────────────────────────────

pub(super) fn setting_detail(entry: &SettingInfo) -> String {
    let mut detail = entry.description.to_owned();
    detail.push_str(" Env: ");
    detail.push_str(entry.env);
    detail.push('.');
    if let Some(range) = entry.range.as_deref() {
        detail.push_str(" Range: ");
        detail.push_str(range);
        detail.push('.');
    }
    if !entry.options.is_empty() {
        detail.push_str(" Values: ");
        detail.push_str(&entry.options.join(", "));
        detail.push('.');
    }
    if !entry.reloadable {
        detail.push_str(" Startup-only.");
    } else if entry.key == "theme" || entry.key == "font_family" {
        detail.push_str(" Enter opens the picker; Ctrl+S saves.");
    } else {
        detail.push_str(" Enter edits/applies; Ctrl+S saves; Esc cancels an edit.");
    }
    detail
}

/// SETTINGS-COMPACT: how many body rows the fixed help footer reserves at the
/// panel bottom — a divider plus the focused row's wrapped help. Kept a pure
/// function of the body height so the scrolling content window is a constant
/// size and never reflows as focus moves between rows with differing help
/// lengths. Returns 0 on windows too short to spare the rows, which collapses
/// the body back to its full-height (pre-compact) form.
pub(in crate::native) fn settings_detail_footer_reserve(body_height: usize) -> usize {
    const DIVIDER_ROWS: usize = 1;
    const MAX_HELP_ROWS: usize = 4;
    if body_height < 6 {
        return 0;
    }
    (DIVIDER_ROWS + MAX_HELP_ROWS).min(body_height / 2)
}

pub(super) fn matches_query(entry: &SettingInfo, needle: &str) -> bool {
    entry.name.to_lowercase().contains(needle)
        || entry.key.to_lowercase().contains(needle)
        || entry.description.to_lowercase().contains(needle)
        || entry.group.to_lowercase().contains(needle)
}

pub(super) fn edit_options(entry: &SettingInfo) -> Vec<&'static str> {
    match entry.key {
        "theme" => vec!["plain", "odyssey-default", "odyssey", "odyssey-noir"],
        "visual" => vec!["off", "ambient"],
        "subpixel" => vec!["off", "rgb", "bgr"],
        "cursor_style" => vec!["block", "underline", "bar"],
        "cursor_blink" => vec!["auto", "on", "off"],
        _ => entry.options.to_vec(),
    }
}

pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(12);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let separator = usize::from(!current.is_empty());
        if current.chars().count() + separator + word.chars().count() > width {
            if !current.is_empty() {
                lines.push(current);
                current = String::new();
            }
            if word.chars().count() > width {
                lines.push(ellipsize(word, width));
                continue;
            }
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

pub(super) fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width <= 1 {
        return "~".to_owned();
    }
    let mut out = text.chars().take(width - 1).collect::<String>();
    out.push('~');
    out
}

// ── Test-only helpers ────────────────────────────────────────────────────────

#[cfg(test)]
impl SettingsPanel {
    /// Put the panel into Level-2 mode showing ALL entries, bypassing the
    /// section navigation. Used by pointer tests to avoid coupling them to
    /// specific section indices (T-level-hitmap fixture).
    pub(in crate::native) fn set_test_flat_mode(&mut self) {
        // Use usize::MAX as the section_index so SECTIONS.get(usize::MAX) returns
        // None and refresh_entries_after_commit preserves the full entry list.
        self.level = SettingsLevel::SectionDetail {
            section_index: usize::MAX,
        };
        self.entries = self.all_entries.clone();
        self.selected = 0;
        self.scroll = 0;
        self.clamp();
    }
}

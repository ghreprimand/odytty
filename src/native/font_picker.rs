// SPDX-License-Identifier: GPL-3.0-only
//! Font-family picker overlay (FONT-PICKER).
//!
//! Displays the monospace font families available on the host (via
//! [`crate::text::font_families`]), lets the user navigate and filter them, and
//! on Enter emits a [`SettingEdit`] that writes `font_family` to the config —
//! the same path saving any other setting uses.
//!
//! **Real family names**: the list is the distinct real `name`-table families
//! that have a monospace face, read by [`crate::text::font_families`]. There is
//! no filename-stem guessing — italic/variant files of one family collapse into
//! a single entry, and the name the user picks resolves cleanly because the
//! resolver matches the same real family names.
//!
//! **No live preview**: font swaps require an atlas rebuild (re-rasterising
//! every loaded glyph). This is too expensive to do on every highlight move, so
//! the picker is apply-on-Enter only. The user sees the family name, selects
//! it, and presses Enter; the reload path then fires normally.

use crate::settings::{FONT_FAMILY_ENV, SettingEdit, Settings};

use super::overlay::OverlayInput;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One row in the picker's ordered model: a non-selectable group header, or a
/// selectable monospace family. Headers split the list into **Bundled Fonts**
/// and **System Fonts** subgroups; navigation and Enter skip headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PickerEntry {
    /// A group label (e.g. "Bundled Fonts"). Rendered dimmed, never focusable.
    Header(String),
    /// A selectable family name.
    Family(String),
}

impl PickerEntry {
    /// The selectable family name, or `None` for a header.
    fn family(&self) -> Option<&str> {
        match self {
            PickerEntry::Family(name) => Some(name),
            PickerEntry::Header(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct FontPicker {
    /// Ordered model: group headers interleaved with monospace family rows.
    entries: Vec<PickerEntry>,
    /// Indices into `entries` visible under the current `query`: matching
    /// families plus any header whose group still has a match. Headers are
    /// retained for context but are never selection targets.
    filtered: Vec<usize>,
    /// Current type-to-filter query.
    query: String,
    /// Index into `filtered` (NOT into `entries`). Always points at a
    /// `Family` row when one exists (navigation snaps past headers).
    selected: usize,
    scroll: usize,
    /// The font_family value when the picker was opened (restored on cancel).
    original: String,
    message: Option<String>,
}

/// Cache-invalidation key for the font picker overlay render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FontPickerSignature {
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) original: String,
    pub(super) current: String,
    pub(super) query: String,
    pub(super) message: Option<String>,
    pub(super) entries: Vec<String>,
}

/// A single render line in the font picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FontPickerLine {
    pub(super) text: String,
    pub(super) focused: bool,
}

/// What the font picker wants the overlay to do.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum FontPickerOutcome {
    /// Input was consumed; no side effect.
    Consumed,
    /// Enter was pressed: write `font_family = value` to the config.
    Persist(Vec<SettingEdit>),
    /// Esc was pressed: restore `original` (the font was never changed in
    /// memory, so this is a no-op for the App; the overlay just closes).
    Cancel(String),
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl FontPicker {
    /// Build a picker from a live font inventory.
    pub(super) fn new(settings: &Settings) -> Self {
        let mut picker = Self {
            entries: Vec::new(), // populated lazily on open()
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            scroll: 0,
            original: current_font_family(settings),
            message: None,
        };
        picker.rebuild_filter();
        picker
    }

    /// (Re)open the picker: snapshot the current font_family as the restore
    /// point and refresh the grouped family list from a fresh metadata scan.
    ///
    /// `groups` carries the **Bundled Fonts** (always present) and **System
    /// Fonts** (host monospace families) subgroups (from
    /// [`crate::text::font_families_grouped`]); the picker builds its ordered
    /// header+family model from them. A group with no families is omitted (no
    /// empty header is shown).
    pub(super) fn open(&mut self, settings: &Settings, groups: crate::text::FontFamilyGroups) {
        self.original = current_font_family(settings);
        self.entries = build_entries(&groups);
        self.query.clear();
        self.message = Some(
            "Select a font family — type to filter, Enter to apply, Esc to cancel.".to_owned(),
        );
        self.rebuild_filter();
        self.select_family(&self.original.clone());
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> FontPickerOutcome {
        match input {
            OverlayInput::Up => self.move_selection(-1),
            OverlayInput::Down => self.move_selection(1),
            OverlayInput::PageUp => self.move_selection(-6),
            OverlayInput::PageDown => self.move_selection(6),
            OverlayInput::Home => self.set_selection(0),
            OverlayInput::End => self.set_selection(self.filtered.len().saturating_sub(1)),
            OverlayInput::Activate => return self.persist_selected(),
            OverlayInput::Close => return FontPickerOutcome::Cancel(self.original.clone()),
            OverlayInput::Backspace => {
                self.query.pop();
                self.rebuild_filter();
                self.clamp();
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                self.query.push(ch);
                self.rebuild_filter();
                self.clamp();
            }
            _ => {}
        }
        FontPickerOutcome::Consumed
    }

    pub(super) fn save_succeeded(&mut self, _changed: usize) {
        self.original = self.selected_family().unwrap_or(self.original.clone());
        // FONT-PICKER-STAY-OPEN: the picker stays open after applying, so make
        // the model obvious. self.original now tracks the just-applied family,
        // so it renders with the "current" marker and Esc keeps it.
        self.message = Some("Applied — Enter to try another, Esc to close.".to_owned());
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.message = Some(format!("Save failed: {message}"));
    }

    pub(super) fn render_signature(&self) -> FontPickerSignature {
        FontPickerSignature {
            selected: self.selected,
            scroll: self.scroll,
            original: self.original.clone(),
            current: self
                .selected_family()
                .unwrap_or_else(|| self.original.clone()),
            query: self.query.clone(),
            message: self.message.clone(),
            entries: self
                .filtered
                .iter()
                .map(|&i| match &self.entries[i] {
                    PickerEntry::Header(label) => format!("# {label}"),
                    PickerEntry::Family(name) => name.clone(),
                })
                .collect(),
        }
    }

    pub(super) fn desired_width(&self, columns: usize) -> usize {
        if columns == 0 {
            return 0;
        }
        let longest = self
            .filtered
            .iter()
            .map(|&i| match &self.entries[i] {
                PickerEntry::Header(label) => label.chars().count(),
                PickerEntry::Family(name) => name.chars().count(),
            })
            .max()
            .unwrap_or(20);
        longest.saturating_add(10).max(54).min(columns)
    }

    /// Hidden entries above / below the visible window, for the scroll
    /// affordance (OVERLAY-SMALL-WINDOW). One body row is the header/filter
    /// hint, so the entry viewport is `body_height - 1`. `(false, false)` when
    /// everything fits.
    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
        let window = body_height.saturating_sub(1);
        (
            self.scroll > 0,
            window > 0 && self.scroll + window < self.entries.len(),
        )
    }

    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<FontPickerLine> {
        if body_width == 0 || body_height == 0 {
            return Vec::new();
        }

        let mut lines = Vec::new();
        let filter_hint = if self.query.is_empty() {
            String::new()
        } else {
            format!("  filter: {:?}  ", self.query)
        };
        lines.push(FontPickerLine {
            text: ellipsize(
                &format!("  Font picker — type to filter, Enter saves, Esc cancels{filter_hint}"),
                body_width,
            ),
            focused: false,
        });

        if let Some(message) = self.message.as_deref() {
            for wrapped in wrap_words(message, body_width.saturating_sub(4)) {
                if lines.len() >= body_height {
                    return lines;
                }
                lines.push(FontPickerLine {
                    text: format!("    {wrapped}"),
                    focused: false,
                });
            }
        }

        if self.filtered.is_empty() {
            if lines.len() < body_height {
                lines.push(FontPickerLine {
                    text: "  (no monospace fonts found)".to_owned(),
                    focused: false,
                });
            }
            return lines;
        }

        for (vis_index, &entry_index) in self.filtered.iter().enumerate().skip(self.scroll) {
            if lines.len() >= body_height {
                break;
            }
            let text = match &self.entries[entry_index] {
                PickerEntry::Header(label) => {
                    // Group label: never focusable, visually set apart.
                    format!("  ── {label} ──")
                }
                PickerEntry::Family(name) => {
                    let focused = vis_index == self.selected;
                    let marker = if focused { ">" } else { " " };
                    let original_mark = if *name == self.original {
                        " current"
                    } else {
                        ""
                    };
                    format!("{marker}   {name}{original_mark}")
                }
            };
            let focused = vis_index == self.selected
                && matches!(self.entries[entry_index], PickerEntry::Family(_));
            lines.push(FontPickerLine {
                text: ellipsize(&text, body_width),
                focused,
            });
        }

        lines.truncate(body_height);
        lines
    }

    /// The number of non-entry prefix lines [`Self::visible_lines`] draws before
    /// the first family/header row (the header hint plus any wrapped message),
    /// capped by `body_height` exactly as the render loop caps it. The click→row
    /// inverse uses this so hit geometry can never drift from render geometry.
    fn header_line_count(&self, body_width: usize, body_height: usize) -> usize {
        let mut count = 1; // the header hint line is always present
        if let Some(message) = self.message.as_deref() {
            for _ in wrap_words(message, body_width.saturating_sub(4)) {
                if count >= body_height {
                    return count;
                }
                count += 1;
            }
        }
        count
    }

    /// Map a clicked body row to the `filtered` position it represents — the
    /// inverse of [`Self::visible_lines`] (UX4-P1 click→Activate). Returns `None`
    /// for the header/message rows, a click on a non-selectable group header, or
    /// a click past the last row.
    pub(super) fn row_at(
        &self,
        row_in_body: usize,
        body_width: usize,
        body_height: usize,
    ) -> Option<usize> {
        if body_width == 0 || body_height == 0 {
            return None;
        }
        let prefix = self.header_line_count(body_width, body_height);
        if row_in_body < prefix {
            return None;
        }
        let pos = self.scroll + (row_in_body - prefix);
        (pos < self.filtered.len() && self.is_selectable(pos)).then_some(pos)
    }

    /// Select the family row under a left-click, reporting whether it landed on a
    /// selectable family (not a group header) so the caller can route the
    /// existing Activate. Parity with Down×N + Activate by construction.
    pub(super) fn click_row(
        &mut self,
        row_in_body: usize,
        body_width: usize,
        body_height: usize,
    ) -> bool {
        match self.row_at(row_in_body, body_width, body_height) {
            Some(pos) => {
                self.set_selection(pos);
                true
            }
            None => false,
        }
    }

    // --- private helpers ----------------------------------------------------

    fn persist_selected(&mut self) -> FontPickerOutcome {
        let Some(family) = self.selected_family() else {
            return FontPickerOutcome::Consumed;
        };
        FontPickerOutcome::Persist(vec![SettingEdit {
            key: "font_family",
            env: FONT_FAMILY_ENV,
            value: family,
        }])
    }

    fn selected_family(&self) -> Option<String> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.entries[i].family())
            .map(str::to_owned)
    }

    fn select_family(&mut self, family: &str) {
        let norm = family.trim().to_lowercase();
        // Find a filtered family entry whose name matches (case-insensitive);
        // fall back to the first selectable family (never a header).
        self.selected = self
            .filtered
            .iter()
            .position(|&i| {
                self.entries[i]
                    .family()
                    .is_some_and(|n| n.to_lowercase() == norm)
            })
            .unwrap_or_else(|| self.first_selectable().unwrap_or(0));
        self.clamp();
    }

    /// Rebuild the visible (`filtered`) row set for the current query. Includes
    /// every matching family plus any group header that still has ≥1 match, so
    /// the subgroup labels follow the filter. A group filtered down to nothing
    /// drops its header too. Selection snaps to the first selectable family.
    fn rebuild_filter(&mut self) {
        let needle = self.query.to_lowercase();
        let matches = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);

        let mut visible: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < self.entries.len() {
            match &self.entries[i] {
                PickerEntry::Header(_) => {
                    let header_idx = i;
                    let mut group: Vec<usize> = Vec::new();
                    let mut j = i + 1;
                    while j < self.entries.len() {
                        match &self.entries[j] {
                            PickerEntry::Header(_) => break,
                            PickerEntry::Family(name) => {
                                if matches(name) {
                                    group.push(j);
                                }
                            }
                        }
                        j += 1;
                    }
                    if !group.is_empty() {
                        visible.push(header_idx);
                        visible.extend(group);
                    }
                    i = j;
                }
                PickerEntry::Family(name) => {
                    // A family with no preceding header (defensive; not produced
                    // by `build_entries`).
                    if matches(name) {
                        visible.push(i);
                    }
                    i += 1;
                }
            }
        }
        self.filtered = visible;
        self.selected = self.first_selectable().unwrap_or(0);
    }

    /// The first `filtered` position that is a selectable family (skips a
    /// leading header), or `None` when nothing is selectable.
    fn first_selectable(&self) -> Option<usize> {
        self.filtered
            .iter()
            .position(|&i| self.entries[i].family().is_some())
    }

    /// Whether the `filtered` position `pos` is a selectable family row.
    fn is_selectable(&self, pos: usize) -> bool {
        self.filtered
            .get(pos)
            .is_some_and(|&i| self.entries[i].family().is_some())
    }

    /// Move the selection by `delta` **selectable** rows, skipping headers and
    /// stopping at the list edges.
    fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() || delta == 0 {
            return;
        }
        let len = self.filtered.len() as isize;
        let step = delta.signum();
        let mut pos = self.selected as isize;
        let mut remaining = delta.abs();
        while remaining > 0 {
            let mut next = pos + step;
            while next >= 0 && next < len && !self.is_selectable(next as usize) {
                next += step;
            }
            if next < 0 || next >= len {
                break; // edge reached; keep the last selectable position
            }
            pos = next;
            remaining -= 1;
        }
        self.set_selection(pos.max(0) as usize);
    }

    fn set_selection(&mut self, selected: usize) {
        if self.filtered.is_empty() {
            self.selected = 0;
            self.clamp();
            return;
        }
        let clamped = selected.min(self.filtered.len() - 1);
        self.selected = self.snap_to_selectable(clamped);
        self.clamp();
    }

    /// Nearest selectable `filtered` position to `pos`: `pos` itself if it is a
    /// family, else the closest family searching forward, else backward.
    fn snap_to_selectable(&self, pos: usize) -> usize {
        if self.is_selectable(pos) {
            return pos;
        }
        for p in (pos + 1)..self.filtered.len() {
            if self.is_selectable(p) {
                return p;
            }
        }
        for p in (0..pos).rev() {
            if self.is_selectable(p) {
                return p;
            }
        }
        pos
    }

    fn clamp(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
            self.scroll = 0;
            return;
        }
        self.selected = self.selected.min(self.filtered.len() - 1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        let visible_slack = 8;
        if self.selected >= self.scroll + visible_slack {
            self.scroll = self.selected.saturating_sub(visible_slack - 1);
        }
        self.scroll = self.scroll.min(self.filtered.len() - 1);
    }
}

/// Build the picker's ordered header+family model from the grouped inventory.
/// Each non-empty group contributes a header followed by its families; an empty
/// group is omitted so no bare header is ever shown.
fn build_entries(groups: &crate::text::FontFamilyGroups) -> Vec<PickerEntry> {
    let mut entries = Vec::new();
    if !groups.bundled.is_empty() {
        entries.push(PickerEntry::Header("Bundled Fonts".to_owned()));
        entries.extend(groups.bundled.iter().cloned().map(PickerEntry::Family));
    }
    if !groups.system.is_empty() {
        entries.push(PickerEntry::Header("System Fonts".to_owned()));
        entries.extend(groups.system.iter().cloned().map(PickerEntry::Family));
    }
    entries
}

// ---------------------------------------------------------------------------
// Helpers shared with theme_picker
// ---------------------------------------------------------------------------

fn current_font_family(settings: &Settings) -> String {
    settings
        .font_family
        .clone()
        .unwrap_or_else(|| "monospace".to_owned())
}

fn wrap_words(text: &str, width: usize) -> Vec<String> {
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

fn ellipsize(text: &str, width: usize) -> String {
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

// ---------------------------------------------------------------------------
// Tests (picker behaviour over a real-family list)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::write_settings_changes_to_path;

    use crate::text::FontFamilyGroups;

    /// The picker consumes the grouped inventory (from
    /// `crate::text::font_families_grouped`); family derivation/dedup/mono-
    /// filtering is tested at that source in `crate::text`. Most picker tests
    /// only need a flat selectable list, so this helper drops the names into the
    /// **System Fonts** group (one header, no bundled rows).
    fn make_families(names: &[&str]) -> FontFamilyGroups {
        FontFamilyGroups {
            bundled: Vec::new(),
            system: names.iter().map(|n| n.to_string()).collect(),
        }
    }

    /// The selectable family labels among the rendered rows (drops group-header
    /// rows, which `render_signature` prefixes with `# `).
    fn family_entries(sig: &FontPickerSignature) -> Vec<String> {
        sig.entries
            .iter()
            .filter(|e| !e.starts_with("# "))
            .cloned()
            .collect()
    }

    // T-empty: an empty family list opens without panic; list is empty.
    #[test]
    fn t_empty_families_no_panic() {
        let settings = Settings::default();
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, FontFamilyGroups::default());
        let sig = picker.render_signature();
        assert!(sig.entries.is_empty());
        // Pressing Enter on an empty picker is a no-op (Consumed, not panic).
        let outcome = picker.handle_input(OverlayInput::Activate);
        assert_eq!(outcome, FontPickerOutcome::Consumed);
    }

    // T-cancel-identity: Esc emits the original font_family unchanged.
    #[test]
    fn t_cancel_identity() {
        let settings = Settings {
            font_family: Some("Cascadia Code".to_owned()),
            ..Default::default()
        };
        let families = make_families(&["Cascadia Code", "Hack"]);
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, families);

        // Move to a different entry.
        let _ = picker.handle_input(OverlayInput::Down);
        // Esc must restore original.
        let outcome = picker.handle_input(OverlayInput::Close);
        match outcome {
            FontPickerOutcome::Cancel(family) => {
                assert_eq!(family, "Cascadia Code");
            }
            other => panic!("expected Cancel, got {other:?}"),
        }
    }

    // T-cancel-identity: no in-memory font_family mutation on cancel (the
    // picker never writes anything until Enter; the cancel value is the
    // original snapshot).
    #[test]
    fn t_cancel_does_not_mutate_original() {
        let settings = Settings {
            font_family: Some("Hack".to_owned()),
            ..Default::default()
        };
        let families = make_families(&["Fira Code", "Hack"]);
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, families);

        // Navigate and cancel.
        let _ = picker.handle_input(OverlayInput::Down);
        let FontPickerOutcome::Cancel(original) = picker.handle_input(OverlayInput::Close) else {
            panic!("expected Cancel");
        };
        assert_eq!(original, "Hack");
    }

    // T-apply-writes: Enter emits a SettingEdit that routes font_family through
    // the dirty/save path with the correct key and env — and the value is the
    // exact REAL family name (with spaces), which the resolver matches.
    #[test]
    fn t_apply_writes_correct_setting_edit() {
        let settings = Settings::default();
        let families = make_families(&["Cascadia Code", "JetBrains Mono"]);
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, families);

        // Select second entry (JetBrains Mono).
        let _ = picker.handle_input(OverlayInput::Down);
        let outcome = picker.handle_input(OverlayInput::Activate);

        match outcome {
            FontPickerOutcome::Persist(changes) => {
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].key, "font_family");
                assert_eq!(changes[0].env, FONT_FAMILY_ENV);
                assert_eq!(changes[0].value, "JetBrains Mono");
            }
            other => panic!("expected Persist, got {other:?}"),
        }
    }

    // T-apply-writes: the SettingEdit can be applied via the standard writeback
    // path without touching the real home dir.
    #[test]
    fn t_apply_persists_via_writeback_without_touching_home() {
        let base = std::env::temp_dir().join(format!(
            "odytty-font-picker-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("odytty.conf");
        std::fs::write(&path, "# kept\nfont_family = monospace\nfont_size = 16\n").unwrap();

        let settings = Settings::default();
        let families = make_families(&["Fira Code"]);
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, families);

        let FontPickerOutcome::Persist(changes) = picker.handle_input(OverlayInput::Activate)
        else {
            panic!("expected Persist");
        };

        let result = write_settings_changes_to_path(&path, &changes).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(result.changed, 1);
        assert!(written.contains("# kept"));
        assert!(written.contains("font_family = Fira Code"));
        assert!(written.contains("font_size = 16"));
        assert!(!written.contains("/home/"));

        std::fs::remove_dir_all(&base).unwrap();
    }

    // T-filter: type-to-filter narrows the family list; clearing restores it.
    #[test]
    fn t_filter_narrows_and_restores() {
        let settings = Settings::default();
        let families = make_families(&["Cascadia Code", "Hack", "JetBrains Mono", "Fira Code"]);
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, families);

        // All 4 visible initially.
        assert_eq!(family_entries(&picker.render_signature()).len(), 4);

        // Type "hack".
        for ch in "hack".chars() {
            picker.handle_input(OverlayInput::Char(ch));
        }
        let families = family_entries(&picker.render_signature());
        assert_eq!(families.len(), 1);
        assert_eq!(families[0], "Hack");

        // Clear filter.
        for _ in 0..4 {
            picker.handle_input(OverlayInput::Backspace);
        }
        assert_eq!(family_entries(&picker.render_signature()).len(), 4);
    }

    // T-filter: filter is case-insensitive and matches inside multi-word names.
    #[test]
    fn t_filter_is_case_insensitive() {
        let settings = Settings::default();
        let families = make_families(&["Cascadia Code", "Fira Code"]);
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, families);

        for ch in "CASCADIA".chars() {
            picker.handle_input(OverlayInput::Char(ch));
        }
        let families = family_entries(&picker.render_signature());
        assert_eq!(families.len(), 1);
        assert_eq!(families[0], "Cascadia Code");
    }

    // T-groups: both subgroups render with their headers, headers are never
    // focusable, and navigation skips over them.
    #[test]
    fn t_groups_render_with_unselectable_headers() {
        let settings = Settings::default();
        let groups = FontFamilyGroups {
            bundled: vec!["Victor Mono".to_owned(), "JetBrains Mono".to_owned()],
            system: vec!["Cascadia Code".to_owned(), "Hack".to_owned()],
        };
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, groups);

        // Rendered rows include both headers (prefixed with "# ") and 4 families.
        let sig = picker.render_signature();
        assert!(sig.entries.iter().any(|e| e == "# Bundled Fonts"));
        assert!(sig.entries.iter().any(|e| e == "# System Fonts"));
        assert_eq!(family_entries(&sig).len(), 4);

        // Initial selection lands on a family (the first selectable), never the
        // leading header.
        assert_eq!(picker.selected_family().as_deref(), Some("Victor Mono"));

        // Walking down past the end of the bundled group must skip the
        // "System Fonts" header and land on the first system family.
        let _ = picker.handle_input(OverlayInput::Down); // JetBrains Mono
        assert_eq!(picker.selected_family().as_deref(), Some("JetBrains Mono"));
        let _ = picker.handle_input(OverlayInput::Down); // skip header → Cascadia
        assert_eq!(picker.selected_family().as_deref(), Some("Cascadia Code"));
        let _ = picker.handle_input(OverlayInput::Down); // Hack
        assert_eq!(picker.selected_family().as_deref(), Some("Hack"));

        // No rendered line that is a header is ever focused.
        for line in picker.visible_lines(60, 40) {
            if line.text.contains("──") {
                assert!(!line.focused, "header line must not be focusable");
            }
        }
    }

    // T-groups: a filter that matches in both groups keeps both headers; one
    // that matches a single group drops the empty group's header.
    #[test]
    fn t_filter_spans_groups_and_drops_empty_group_headers() {
        let settings = Settings::default();
        let groups = FontFamilyGroups {
            bundled: vec!["Victor Mono".to_owned(), "JetBrains Mono".to_owned()],
            system: vec!["Cascadia Code".to_owned(), "Hack Mono".to_owned()],
        };
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, groups);

        // "mono" matches in both groups → both headers remain.
        for ch in "mono".chars() {
            picker.handle_input(OverlayInput::Char(ch));
        }
        let sig = picker.render_signature();
        assert!(sig.entries.iter().any(|e| e == "# Bundled Fonts"));
        assert!(sig.entries.iter().any(|e| e == "# System Fonts"));
        assert_eq!(family_entries(&sig).len(), 3); // JetBrains Mono, Hack Mono, Victor Mono

        // "cascadia" matches only the system group → the bundled header drops.
        for _ in 0..4 {
            picker.handle_input(OverlayInput::Backspace);
        }
        for ch in "cascadia".chars() {
            picker.handle_input(OverlayInput::Char(ch));
        }
        let sig = picker.render_signature();
        assert!(!sig.entries.iter().any(|e| e == "# Bundled Fonts"));
        assert!(sig.entries.iter().any(|e| e == "# System Fonts"));
        assert_eq!(family_entries(&sig), vec!["Cascadia Code".to_owned()]);
    }

    // T-groups: a bundled pick and a system pick both persist the exact family
    // name, proving either group resolves with zero further config.
    #[test]
    fn t_both_groups_persist_their_family_name() {
        let settings = Settings::default();
        let groups = FontFamilyGroups {
            bundled: vec!["Victor Mono".to_owned(), "JetBrains Mono".to_owned()],
            system: vec!["Cascadia Code".to_owned()],
        };

        // Bundled pick: the first selectable family is Victor Mono.
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, groups.clone());
        let FontPickerOutcome::Persist(changes) = picker.handle_input(OverlayInput::Activate)
        else {
            panic!("expected Persist for bundled pick");
        };
        assert_eq!(changes[0].value, "Victor Mono");

        // System pick: navigate to the system family and apply.
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, groups);
        let _ = picker.handle_input(OverlayInput::End); // last selectable
        let FontPickerOutcome::Persist(changes) = picker.handle_input(OverlayInput::Activate)
        else {
            panic!("expected Persist for system pick");
        };
        assert_eq!(changes[0].value, "Cascadia Code");
    }

    // ── UX4-P1 click→Activate parity ───────────────────────────────────────

    #[test]
    fn click_family_row_persists_that_family() {
        let settings = Settings::default();
        let families = make_families(&["Cascadia Code", "Hack", "Fira Code"]);
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, families);
        let lines = picker.visible_lines(70, 40);
        // The focused row is the first selectable family (Cascadia Code).
        let focused_row = lines
            .iter()
            .position(|line| line.focused)
            .expect("a focused family row");
        assert!(picker.click_row(focused_row, 70, 40));
        let FontPickerOutcome::Persist(changes) = picker.handle_input(OverlayInput::Activate)
        else {
            panic!("click must persist a family");
        };
        assert_eq!(changes[0].value, "Cascadia Code");
    }

    #[test]
    fn click_group_header_row_is_inert() {
        let settings = Settings::default();
        let groups = FontFamilyGroups {
            bundled: vec!["Victor Mono".to_owned()],
            system: vec!["Hack".to_owned()],
        };
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, groups);
        let lines = picker.visible_lines(70, 40);
        // A group-label row (rendered with the "──" rule) is never selectable.
        let header_row = lines
            .iter()
            .position(|line| line.text.contains("──"))
            .expect("a group header row");
        assert!(picker.row_at(header_row, 70, 40).is_none());
        assert!(!picker.click_row(header_row, 70, 40));
    }

    #[test]
    fn click_matches_navigation_for_second_family() {
        // Clicking the second family must persist the same value as Down+Activate.
        let settings = Settings::default();
        let families = make_families(&["Cascadia Code", "Hack", "Fira Code"]);

        let mut by_click = FontPicker::new(&settings);
        by_click.open(&settings, families.clone());
        let lines = by_click.visible_lines(70, 40);
        let focused_row = lines.iter().position(|l| l.focused).unwrap();
        // The next family row is directly below the focused first family.
        assert!(by_click.click_row(focused_row + 1, 70, 40));
        let FontPickerOutcome::Persist(click) = by_click.handle_input(OverlayInput::Activate)
        else {
            panic!("click persist");
        };

        let mut by_key = FontPicker::new(&settings);
        by_key.open(&settings, families);
        by_key.handle_input(OverlayInput::Down);
        let FontPickerOutcome::Persist(key) = by_key.handle_input(OverlayInput::Activate) else {
            panic!("key persist");
        };
        assert_eq!(click[0].value, key[0].value);
        assert_eq!(click[0].value, "Hack");
    }
}

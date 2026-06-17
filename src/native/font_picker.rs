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

#[derive(Debug, Clone)]
pub(super) struct FontPicker {
    /// Monospace-only real family names (unique, sorted).
    all_families: Vec<String>,
    /// Indices into `all_families` that match the current `query`.
    filtered: Vec<usize>,
    /// Current type-to-filter query.
    query: String,
    /// Index into `filtered` (NOT into `all_families`).
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
        let all_families = Vec::new(); // populated lazily on open()
        let mut picker = Self {
            all_families,
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
    /// point and refresh the family list from a fresh metadata scan.
    ///
    /// `families` is the distinct, sorted, monospace-only real family list (from
    /// [`crate::text::font_families`]); the picker stores it verbatim.
    pub(super) fn open(&mut self, settings: &Settings, families: Vec<String>) {
        self.original = current_font_family(settings);
        self.all_families = families;
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
        self.message = Some("Font family saved.".to_owned());
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
                .map(|&i| self.all_families[i].clone())
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
            .map(|&i| self.all_families[i].chars().count())
            .max()
            .unwrap_or(20);
        longest.saturating_add(10).max(54).min(columns)
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

        for (vis_index, &family_index) in self.filtered.iter().enumerate().skip(self.scroll) {
            if lines.len() >= body_height {
                break;
            }
            let focused = vis_index == self.selected;
            let marker = if focused { ">" } else { " " };
            let original_mark = if self.all_families[family_index] == self.original {
                " current"
            } else {
                ""
            };
            let text = format!(
                "{marker} {}{original_mark}",
                self.all_families[family_index]
            );
            lines.push(FontPickerLine {
                text: ellipsize(&text, body_width),
                focused,
            });
        }

        lines.truncate(body_height);
        lines
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
            .map(|&i| self.all_families[i].clone())
    }

    fn select_family(&mut self, family: &str) {
        let norm = family.trim().to_lowercase();
        // Try to find a filtered entry whose collapsed name matches
        // (case-insensitive); fall back to index 0.
        self.selected = self
            .filtered
            .iter()
            .position(|&i| self.all_families[i].to_lowercase() == norm)
            .unwrap_or(0);
        self.clamp();
    }

    fn rebuild_filter(&mut self) {
        let needle = self.query.to_lowercase();
        self.filtered = self
            .all_families
            .iter()
            .enumerate()
            .filter(|(_, name)| needle.is_empty() || name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        // Reset selection to 0 after filter rebuild (clamp will adjust scroll).
        self.selected = 0;
    }

    fn move_selection(&mut self, delta: isize) {
        let next = self.selected as isize + delta;
        self.set_selection(next.clamp(0, self.filtered.len().saturating_sub(1) as isize) as usize);
    }

    fn set_selection(&mut self, selected: usize) {
        self.selected = selected.min(self.filtered.len().saturating_sub(1));
        self.clamp();
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

    /// The picker now consumes a ready-made real-family list (from
    /// `crate::text::font_families`); family derivation/dedup/mono-filtering is
    /// tested at that source in `crate::text`. These tests feed the list
    /// directly to exercise navigation, filtering, cancel, and apply.
    fn make_families(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    // T-empty: an empty family list opens without panic; list is empty.
    #[test]
    fn t_empty_families_no_panic() {
        let settings = Settings::default();
        let mut picker = FontPicker::new(&settings);
        picker.open(&settings, Vec::new());
        let sig = picker.render_signature();
        assert!(sig.entries.is_empty());
        // Pressing Enter on an empty picker is a no-op (Consumed, not panic).
        let outcome = picker.handle_input(OverlayInput::Activate);
        assert_eq!(outcome, FontPickerOutcome::Consumed);
    }

    // T-cancel-identity: Esc emits the original font_family unchanged.
    #[test]
    fn t_cancel_identity() {
        let mut settings = Settings::default();
        settings.font_family = Some("Cascadia Code".to_owned());
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
        let mut settings = Settings::default();
        settings.font_family = Some("Hack".to_owned());
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
        assert_eq!(picker.render_signature().entries.len(), 4);

        // Type "hack".
        for ch in "hack".chars() {
            picker.handle_input(OverlayInput::Char(ch));
        }
        let sig = picker.render_signature();
        assert_eq!(sig.entries.len(), 1);
        assert_eq!(sig.entries[0], "Hack");

        // Clear filter.
        for _ in 0..4 {
            picker.handle_input(OverlayInput::Backspace);
        }
        assert_eq!(picker.render_signature().entries.len(), 4);
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
        let sig = picker.render_signature();
        assert_eq!(sig.entries.len(), 1);
        assert_eq!(sig.entries[0], "Cascadia Code");
    }
}

// SPDX-License-Identifier: GPL-3.0-only
use crate::settings::{
    DEFAULT_CELL_BG_OPACITY, SettingEdit, SettingInfo, SettingKind, Settings, SettingsEditOverlay,
};

use super::overlay::OverlayInput;

mod path_picker;
mod pointer;
mod sections;

use path_picker::{PathPickerOutcome, PathPickerSignature, PathPickerState, resolve_start_dir};
use sections::SECTIONS;

/// Synthetic entry key for the "Open Theme Builder" action row injected at the
/// end of the Themes section's Level-2 list (v0.3.1 discoverability). It is not
/// a real setting: it carries no value, is excluded from live value-sync, and on
/// Activate emits [`SettingsPanelOutcome::OpenThemeBuilder`]. The sentinel key
/// must not collide with any real setting key.
const THEME_BUILDER_ACTION_KEY: &str = "__action_open_theme_builder";

/// Build the synthetic "Open Theme Builder" action entry. Rendered as an action
/// row (a `→` affordance in the value column) rather than an editable setting;
/// `reloadable` is `true` so the activation is not blocked by the startup-only
/// guard.
fn theme_builder_action_entry() -> SettingInfo {
    SettingInfo {
        group: "Theme",
        key: THEME_BUILDER_ACTION_KEY,
        env: "",
        name: "Open Theme Builder",
        value: "\u{2192}".to_owned(),
        description: "Clone the active theme and edit its colors, then save it as a new theme.",
        kind: SettingKind::String,
        range: None,
        numeric: None,
        options: &[],
        reloadable: true,
    }
}

/// Two-level navigation state for `SettingsPanel`.
///
/// Level 1 (`SectionList`) renders the short section menu; Level 2
/// (`SectionDetail`) renders the group-filtered setting entries for one
/// section. Entering Level 2 resets the entry scroll/selection to the top;
/// returning to Level 1 restores `section_selected`/`section_scroll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsLevel {
    /// Level 1: the section list (`SECTIONS.len()` rows).
    SectionList,
    /// Level 2: the entry list for the section at `section_index`.
    SectionDetail { section_index: usize },
}

#[derive(Debug, Clone)]
pub(super) struct SettingsPanel {
    edits: SettingsEditOverlay,
    /// Full setting roster; the base for both the section filter and the search
    /// filter. With no active filters, `entries == all_entries` (identical).
    all_entries: Vec<SettingInfo>,
    /// The active visible entry list. At Level 1 this is `all_entries`; at
    /// Level 2 it is filtered to the current section's groups.
    entries: Vec<SettingInfo>,
    /// Two-level navigation state (SETTINGS-REDESIGN).
    level: SettingsLevel,
    /// Focused section row in Level 1 (index into `SECTIONS`).
    section_selected: usize,
    /// Scroll offset for Level 1 (usually 0 since sections fit in one screen).
    section_scroll: usize,
    /// Whether the save-or-discard close prompt is showing (dirty Esc at L1).
    pending_close_prompt: bool,
    /// Active path-picker sub-state (for `Path`-kind rows). `None` when not in
    /// use. T-two-substates: `path_picker` and `editing` are mutually exclusive.
    path_picker: Option<PathPickerState>,
    selected: usize,
    scroll: usize,
    editing: Option<RowEdit>,
    message: Option<String>,
    /// In-overlay search state (OB-SEARCH).
    query: String,
    search_active: bool,
    /// Body dimensions from the most recent render call (VIEWPORT-FOLLOW-LAG).
    last_body_height: usize,
    last_body_width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsPanelSignature {
    pub(super) selected: usize,
    pub(super) scroll: usize,
    pub(super) editing_key: Option<&'static str>,
    pub(super) changed_count: usize,
    pub(super) message: Option<String>,
    pub(super) entries: Vec<SettingsPanelEntrySignature>,
    pub(super) query: String,
    pub(super) search_active: bool,
    /// Two-level navigation state (SETTINGS-REDESIGN).
    pub(super) level: SettingsLevel,
    pub(super) section_selected: usize,
    /// Level-1 section-list scroll offset. MUST be in the signature so a
    /// wheel/keyboard scroll that moves only the view (not the selection)
    /// still reclassifies the render cache to a repaint (OVERLAY-SMALL-WINDOW:
    /// its absence left the Level-1 list visually frozen on a small window).
    pub(super) section_scroll: usize,
    pub(super) pending_close_prompt: bool,
    pub(super) path_picker: Option<PathPickerSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsPanelEntrySignature {
    pub(super) key: &'static str,
    pub(super) value: String,
    pub(super) description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SettingsPanelLine {
    pub(super) text: String,
    pub(super) focused: bool,
    /// Whether to render this line in bold. True for primary setting name/value
    /// rows; false for group headers, detail/help text, and notice lines.
    pub(super) bold: bool,
}

// `Apply` carries a full `Settings` by value; boxing it would ripple through
// every construction and match site for no runtime gain on this cold path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SettingsPanelOutcome {
    Consumed,
    Apply(Settings),
    Save(Vec<SettingEdit>),
    OpenThemePicker,
    OpenThemeBuilder,
    OpenKeyBindings,
    /// Open the font-family picker (FONT-PICKER). Emitted from the Fonts
    /// section's `font_family` row. The picker overlay
    /// is sequenced in the FONT-PICKER packet; the variant is wired here.
    OpenFontPicker,
    /// Save all pending changes and close the overlay.
    SaveAndClose(Vec<SettingEdit>),
    /// Discard all pending changes and close the overlay.
    DiscardAndClose,
    /// Close the overlay (emitted at Level 1 with no pending edits / dirty
    /// prompt already shown).
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowEdit {
    key: &'static str,
    buffer: String,
}

impl SettingsPanel {
    pub(super) fn new(settings: &Settings) -> Self {
        let edits = SettingsEditOverlay::new(settings);
        let entries = edits.settings().setting_info();
        let mut panel = Self {
            all_entries: entries.clone(),
            entries,
            level: SettingsLevel::SectionList,
            section_selected: 0,
            section_scroll: 0,
            pending_close_prompt: false,
            path_picker: None,
            edits,
            selected: 0,
            scroll: 0,
            editing: None,
            message: None,
            query: String::new(),
            search_active: false,
            last_body_height: 18,
            last_body_width: 76,
        };
        panel.clamp();
        panel
    }

    /// Called by the render path immediately before `visible_lines` so that
    /// keyboard navigation (`clamp`) knows the real visible window dimensions.
    pub(super) fn update_body_height(&mut self, body_height: usize) {
        if let Some(picker) = self.path_picker.as_mut() {
            picker.poll_pending();
        }
        if body_height > 0 {
            self.last_body_height = body_height;
        }
    }

    pub(super) fn update_body_width(&mut self, body_width: usize) {
        if let Some(picker) = self.path_picker.as_mut() {
            picker.poll_pending();
        }
        if body_width > 0 {
            self.last_body_width = body_width;
        }
    }

    /// Returns `true` when the selected entry's primary row appears within the
    /// rendered window (VIEWPORT-FOLLOW-LAG fix). Only meaningful at Level 2.
    fn selected_in_window(&self, body_height: usize) -> bool {
        if body_height == 0 || self.entries.is_empty() {
            return true;
        }
        use crate::native::settings_panel::pointer::RowZone;
        self.build_settings_rows(self.last_body_width, body_height)
            .iter()
            .any(|(_, hit)| {
                hit.entry_index == Some(self.selected)
                    && matches!(hit.zone, RowZone::Value | RowZone::Stepper { .. })
            })
    }

    pub(super) fn refresh(&mut self, settings: &Settings) {
        let selected_key = self
            .entries
            .get(self.selected)
            .map(|entry| entry.key)
            .unwrap_or("theme");
        self.edits = SettingsEditOverlay::new(settings);
        self.query.clear();
        self.search_active = false;
        self.all_entries = self.edits.settings().setting_info();
        self.entries = self.all_entries.clone();
        // Reset to Level 1 on a config reload.
        self.level = SettingsLevel::SectionList;
        self.section_selected = 0;
        self.section_scroll = 0;
        self.pending_close_prompt = false;
        self.path_picker = None;
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.key == selected_key)
            .unwrap_or(0);
        self.editing = None;
        self.message = None;
        self.clamp();
    }

    /// Live-apply seam (`SettingsApplySource::OverlayEdit`): a value committed in
    /// the panel (step/cycle/slider) or a Save re-read of the config is routed
    /// back here so the preview/save takes effect immediately. This MUST preserve
    /// the panel's navigation state — the current level, the drilled-into section
    /// filter, an active search, and any unsaved dirty edits in `self.edits`.
    ///
    /// SETTINGS-PANEL-STATE-FIX:
    ///   - Bug B: do NOT call `apply_search_filter()` unconditionally. With no
    ///     active query it replaces the section-filtered list with ALL settings,
    ///     leaking the user out of their section. Use the section/search-aware
    ///     `refresh_entries_after_commit()` instead (the same rebuild the commit
    ///     path uses), which preserves the SectionDetail filter at Level 2, the
    ///     search filter in search mode, and the full list only at Level 1.
    ///   - Bug C: do NOT call the level-resetting `refresh()`. On a live apply the
    ///     incoming `settings` (re-read via `Settings::from_env` on Save) can
    ///     differ from the in-panel edit overlay, so the old
    ///     `if self.edits.settings() != settings { self.refresh(settings); }`
    ///     fired spuriously and yanked the user back to Level 1. The applied
    ///     values are already reflected in `self.edits` (the commit path updated
    ///     it; Save calls `save_succeeded`/`mark_saved`), so we keep `self.edits`
    ///     as the source of truth and never touch `self.level`,
    ///     `self.section_selected`, or `self.search_active` here.
    pub(super) fn apply_settings(&mut self, _settings: &Settings) {
        // Avoid rebuilding the settings inventory during repeated live edits: the
        // OverlayEdit echo carries values the panel already committed into
        // `self.edits`, and `commit_value` already patched into
        // `all_entries`/`entries` in place. Re-derive every entry value from the
        // edit overlay in place rather than rebuilding the full `setting_info()`
        // table on every echo. A full rebuild is the fallback only if a key is
        // unknown to `display_value_for_key` (inventory shape changed).
        let needs_full_rebuild = self.sync_all_entry_values_in_place();
        if needs_full_rebuild {
            self.all_entries = self.edits.settings().setting_info();
            self.refresh_entries_after_commit();
        }
        self.clamp();
    }

    /// Patch every entry's `value` field in `all_entries` and the filtered
    /// `entries` from the current edit-overlay settings, in place. Returns
    /// `true` if any key was unknown to [`Settings::display_value_for_key`],
    /// signalling the caller should fall back to a full `setting_info()`
    /// rebuild.
    fn sync_all_entry_values_in_place(&mut self) -> bool {
        let settings = self.edits.settings().clone();
        let mut unknown = false;
        for entry in &mut self.all_entries {
            match settings.display_value_for_key(entry.key) {
                Some(value) => entry.value = value,
                None => unknown = true,
            }
        }
        for entry in &mut self.entries {
            // The synthetic action row carries no live value; skip it so it never
            // forces a spurious full rebuild (its value is static by design).
            if entry.key == THEME_BUILDER_ACTION_KEY {
                continue;
            }
            match settings.display_value_for_key(entry.key) {
                Some(value) => entry.value = value,
                None => unknown = true,
            }
        }
        unknown
    }

    /// Reconcile an externally-applied `Settings` from a picker into the edit
    /// overlay as the new clean baseline, while preserving pending panel edits
    /// and navigation state.
    pub(super) fn rebase_onto_external(&mut self, settings: &Settings) {
        self.edits.rebase_onto(settings);
        let selected_key = self
            .entries
            .get(self.selected)
            .map(|entry| entry.key)
            .unwrap_or("theme");
        self.all_entries = self.edits.settings().setting_info();
        self.refresh_entries_after_commit();
        if let Some(pos) = self.entries.iter().position(|e| e.key == selected_key) {
            self.selected = pos;
        }
        self.clamp();
    }

    pub(super) fn save_succeeded(&mut self, changed: usize) {
        self.edits.mark_saved();
        self.all_entries = self.edits.settings().setting_info();
        self.refresh_entries_after_commit();
        self.message = Some(format!("Saved {changed} setting change(s) to odytty.conf."));
        self.clamp();
    }

    pub(super) fn save_failed(&mut self, message: String) {
        self.message = Some(format!("Save failed: {message}"));
    }

    // NOTE: `is_editing` and `is_searching` were previously used by the
    // overlay.rs Close guard; the two-level model removed that guard (the panel
    // now handles all Close inputs internally). Kept for future callers.
    #[allow(dead_code)]
    pub(super) fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    pub(super) fn is_dragging(&self) -> bool {
        false
    }

    /// The current panel title (used by `apply_overlay` to render the title
    /// bar, which changes based on the active level and editing state).
    pub(super) fn panel_title(&self) -> String {
        match &self.level {
            SettingsLevel::SectionList => {
                if self.search_active {
                    return format!("OdyTTY Settings — Search: {}", self.query);
                }
                "OdyTTY Settings".to_owned()
            }
            SettingsLevel::SectionDetail { section_index } => {
                if let Some(edit) = &self.editing {
                    return format!(
                        "\u{270e} EDITING {} \u{2014} Enter applies \u{00b7} Esc cancels",
                        edit.key
                    );
                }
                let name = SECTIONS
                    .get(*section_index)
                    .map(|s| s.name)
                    .unwrap_or("Settings");
                format!("\u{2190} {name}  (Esc = back)")
            }
        }
    }

    pub(super) fn handle_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
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
        }
    }

    // ── Level 1: section-list dispatch ──────────────────────────────────────

    fn handle_section_list_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
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
                self.section_selected = SECTIONS.len().saturating_sub(1);
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

    fn handle_section_detail_input(
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
    fn back_to_section_list(&mut self) {
        self.level = SettingsLevel::SectionList;
        self.entries = self.all_entries.clone();
    }

    pub(super) fn current_level(&self) -> SettingsLevel {
        self.level
    }

    pub(super) fn set_level(&mut self, level: SettingsLevel) {
        self.level = level;
    }

    // ── Dirty-close prompt ──────────────────────────────────────────────────

    fn handle_close_prompt_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
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

    fn handle_path_picker_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
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
    fn handle_search_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
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

    fn apply_search_filter(&mut self) {
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

    fn exit_search_preserving(&mut self, key: Option<&'static str>) {
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
    pub(super) fn is_searching(&self) -> bool {
        self.search_active
    }

    // ── Drilling and level navigation ───────────────────────────────────────

    /// Build the Level-2 entry list for the section at `section_index`: the
    /// group-filtered real settings, plus any synthetic action rows the section
    /// offers. The Themes section appends an "Open Theme Builder" action at the
    /// end (v0.3.1 discoverability). Both `drill_into_section` and the
    /// post-commit `refresh_entries_after_commit` build through this so the
    /// action row survives a live value-sync rebuild.
    fn section_entries(&self, section_index: usize) -> Vec<SettingInfo> {
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
        entries
    }

    /// Drill into section `section_index`: filter `entries` to the section's
    /// groups, reset Level-2 scroll/selection to the top, and update `level`.
    /// Clears editing, path_picker, and message (T-editing-clears-on-level-change).
    pub(super) fn drill_into_section(&mut self, section_index: usize) {
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

    fn move_section_selection(&mut self, delta: isize) {
        let n = SECTIONS.len() as isize;
        let next = (self.section_selected as isize + delta).clamp(0, n - 1) as usize;
        self.section_selected = next;
        self.follow_section_selection();
    }

    /// Whether the body has hidden rows above / below the visible window, for
    /// the scroll affordance (OVERLAY-SMALL-WINDOW). Approximate but stable:
    /// `(false, false)` whenever everything fits, so a normal window draws no
    /// arrows and stays byte-identical. Level 1 reserves one body row for the
    /// footer hint; Level 2 / search compares the entry scroll against the count.
    pub(super) fn scroll_indicator(&self, body_height: usize) -> (bool, bool) {
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
            let total = SECTIONS.len();
            return (
                self.section_scroll > 0,
                self.section_scroll + window < total,
            );
        }
        let total = self.entries.len();
        (self.scroll > 0, self.scroll + body_height < total)
    }

    /// Keep the selected section inside the Level-1 visible window by adjusting
    /// `section_scroll` (OVERLAY-SMALL-WINDOW). Without this, ArrowDown on a
    /// short window walked the selection off-screen while the view stayed put.
    /// The footer hint consumes one body row when there is room, so the section
    /// viewport is `last_body_height - 1` rows (min 1).
    fn follow_section_selection(&mut self) {
        let window = self.last_body_height.saturating_sub(1).max(1);
        if self.section_selected < self.section_scroll {
            self.section_scroll = self.section_selected;
        } else if self.section_selected >= self.section_scroll + window {
            self.section_scroll = self.section_selected + 1 - window;
        }
        let max_scroll = SECTIONS.len().saturating_sub(1);
        self.section_scroll = self.section_scroll.min(max_scroll);
    }

    // ── Render ───────────────────────────────────────────────────────────────

    pub(super) fn render_signature(&self) -> SettingsPanelSignature {
        SettingsPanelSignature {
            selected: self.selected,
            scroll: self.scroll,
            editing_key: self.editing.as_ref().map(|edit| edit.key),
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

    pub(super) fn desired_width(&self, columns: usize) -> usize {
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
    pub(super) fn visible_lines(
        &self,
        body_width: usize,
        body_height: usize,
    ) -> Vec<SettingsPanelLine> {
        self.build_visible_rows(body_width, body_height)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    fn display_value(&self, entry: &SettingInfo) -> String {
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

    // ── Value activation ────────────────────────────────────────────────────

    fn activate_selected(&mut self) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        if !entry.reloadable {
            self.message = Some("Startup-only setting; edit odytty.conf and restart.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }
        // The synthetic "Open Theme Builder" action row opens the builder
        // directly (v0.3.1 discoverability) — no `b` press, no row edit.
        if entry.key == THEME_BUILDER_ACTION_KEY {
            self.message = Some("Opening theme builder.".to_owned());
            return SettingsPanelOutcome::OpenThemeBuilder;
        }
        // Key-specific overrides (run before kind dispatch):
        // - theme: Enter opens the theme picker (not RowEdit) in the two-level model.
        // - font_family: Enter opens the font picker (key is String kind, not Enum).
        if entry.key == "theme" {
            self.message = Some("Opening built-in theme picker.".to_owned());
            return SettingsPanelOutcome::OpenThemePicker;
        }
        if entry.key == "font_family" {
            self.message = Some("Opening font picker.".to_owned());
            return SettingsPanelOutcome::OpenFontPicker;
        }
        match entry.kind {
            SettingKind::Bool => {
                let next = if entry.value == "on" { "off" } else { "on" };
                self.commit_value(entry.key, next)
            }
            SettingKind::Enum => self.cycle_selected(1),
            SettingKind::List if entry.key == "keybinds" => SettingsPanelOutcome::OpenKeyBindings,
            // Path rows open the inline path picker (SETTINGS-REDESIGN §8).
            SettingKind::Path => {
                let original = entry.value.clone();
                let start_dir = resolve_start_dir(&original);
                // T-two-substates: clear editing before opening the picker.
                self.editing = None;
                self.path_picker = Some(PathPickerState::new(entry.key, start_dir, original));
                SettingsPanelOutcome::Consumed
            }
            SettingKind::Number | SettingKind::String | SettingKind::List => {
                self.editing = Some(RowEdit {
                    key: entry.key,
                    buffer: entry.value,
                });
                self.message =
                    Some("Editing: type a value, Enter applies, Esc cancels.".to_owned());
                SettingsPanelOutcome::Consumed
            }
        }
    }

    fn handle_editing_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
        match input {
            OverlayInput::Close => {
                if let Some(edit) = self.editing.take() {
                    self.message = Some(format!("Cancelled edit for {}.", edit.key));
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Activate => {
                let Some(edit) = self.editing.take() else {
                    return SettingsPanelOutcome::Consumed;
                };
                let key = edit.key;
                let value = edit.buffer;
                self.commit_value(key, &value)
            }
            OverlayInput::Backspace => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.buffer.pop();
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                if let Some(edit) = self.editing.as_mut() {
                    edit.buffer.push(ch);
                }
                SettingsPanelOutcome::Consumed
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    fn step_or_cycle_selected(&mut self, direction: isize) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        if entry.key == "background_image_scrim" {
            let parsed =
                entry
                    .value
                    .parse::<f32>()
                    .unwrap_or(if direction < 0 { 1.0 } else { 0.0 });
            let next = if let Some(spec) = entry.numeric {
                let step = spec.step * direction as f32;
                (parsed + step).clamp(spec.min, spec.max)
            } else {
                parsed
            };
            return self.commit_value(entry.key, &format!("{next:.3}"));
        }
        match entry.kind {
            SettingKind::Enum => self.cycle_selected(direction),
            SettingKind::Number => {
                let parsed = entry.value.parse::<f32>().unwrap_or(0.0);
                let next = if let Some(spec) = entry.numeric {
                    let step = spec.step * direction as f32;
                    (parsed + step).clamp(spec.min, spec.max)
                } else {
                    parsed
                };
                self.commit_value(entry.key, &format!("{:.3}", next))
            }
            _ => SettingsPanelOutcome::Consumed,
        }
    }

    fn cycle_selected(&mut self, direction: isize) -> SettingsPanelOutcome {
        let Some(entry) = self.selected_entry().cloned() else {
            return SettingsPanelOutcome::Consumed;
        };
        let options = edit_options(&entry);
        let Some(current) = options.iter().position(|value| *value == entry.value) else {
            self.message = Some("Type a custom value with Enter.".to_owned());
            return SettingsPanelOutcome::Consumed;
        };
        let len = options.len() as isize;
        let next = (current as isize + direction).rem_euclid(len) as usize;
        self.commit_value(entry.key, options[next])
    }

    fn commit_value(&mut self, key: &'static str, value: &str) -> SettingsPanelOutcome {
        let before_scroll = self.scroll;
        if key == "background_image" && !value.trim().is_empty() && value.trim() != "none" {
            let _ = self.edits.apply_raw("background_treatment", "image");
            if self.edits.settings().cell_bg_opacity >= DEFAULT_CELL_BG_OPACITY - 0.001 {
                let _ = self.edits.apply_raw("cell_bg_opacity", "0.850");
            }
        }
        let commit_value;
        let value = if key == "cell_bg_opacity" {
            let visibility = value.trim().parse::<f32>().unwrap_or(0.0).clamp(0.0, 1.0);
            commit_value = format!("{:.3}", 1.0 - visibility);
            commit_value.as_str()
        } else {
            value
        };
        if key == "background_image" {
            self.update_entry_value_in_place("background_treatment");
            self.update_entry_value_in_place("cell_bg_opacity");
        }
        match self.edits.apply_raw(key, value) {
            Ok(Some(settings)) => {
                // Update only the changed row's display value in place instead
                // of rebuilding the full `setting_info()` table on every
                // repeated edit.
                self.update_entry_value_in_place(key);
                if key == "background_image" {
                    self.update_entry_value_in_place("background_treatment");
                    self.update_entry_value_in_place("cell_bg_opacity");
                }
                self.restore_scroll_after_commit(before_scroll);
                self.message = Some(format!("Applied {key}."));
                SettingsPanelOutcome::Apply(settings)
            }
            Ok(None) => {
                self.update_entry_value_in_place(key);
                if key == "background_image" {
                    self.update_entry_value_in_place("background_treatment");
                    self.update_entry_value_in_place("cell_bg_opacity");
                }
                self.restore_scroll_after_commit(before_scroll);
                self.message = Some("No setting change.".to_owned());
                SettingsPanelOutcome::Consumed
            }
            Err(error) => {
                self.message = Some(error.message);
                SettingsPanelOutcome::Consumed
            }
        }
    }

    /// Re-derive the display `value` for a single setting key from the current
    /// edit-overlay settings and patch it into `all_entries` and the filtered
    /// `entries` list in place. Falls back to a
    /// full `setting_info()` rebuild if the key is not found or the single-key
    /// derivation is unavailable, so the panel stays correct if the inventory
    /// shape ever changes. Only the `value` field can change from a live edit;
    /// group/key/name/description/kind/numeric/options/reloadable are static.
    fn update_entry_value_in_place(&mut self, key: &'static str) {
        let Some(new_value) = self.edits.settings().display_value_for_key(key) else {
            self.all_entries = self.edits.settings().setting_info();
            self.refresh_entries_after_commit();
            return;
        };
        for entry in &mut self.all_entries {
            if entry.key == key {
                entry.value.clone_from(&new_value);
            }
        }
        for entry in &mut self.entries {
            if entry.key == key {
                entry.value = new_value.clone();
            }
        }
    }

    fn restore_scroll_after_commit(&mut self, before_scroll: usize) {
        if self.entries.is_empty() {
            self.scroll = 0;
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.scroll = before_scroll.min(self.entries.len().saturating_sub(1));
    }

    /// Rebuild `entries` after a commit that updated `all_entries`. Preserves
    /// the section filter at Level 2 and the search filter in search mode.
    fn refresh_entries_after_commit(&mut self) {
        if self.search_active {
            self.apply_search_filter();
            return;
        }
        if let SettingsLevel::SectionDetail { section_index } = &self.level.clone() {
            let si = *section_index;
            if SECTIONS.get(si).is_none() {
                return;
            }
            let key = self.entries.get(self.selected).map(|e| e.key);
            self.entries = self.section_entries(si);
            // Re-find the selected key in the new list (values may have changed).
            if let Some(key) = key
                && let Some(pos) = self.entries.iter().position(|e| e.key == key)
            {
                self.selected = pos;
            }
            self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        } else {
            // Level 1: restore the full list.
            self.entries = self.all_entries.clone();
        }
    }

    fn save_changes(&mut self) -> SettingsPanelOutcome {
        let changes = self.edits.changes();
        if changes.is_empty() {
            self.message = Some("No unsaved setting changes.".to_owned());
            return SettingsPanelOutcome::Consumed;
        }
        SettingsPanelOutcome::Save(changes)
    }

    fn selected_entry(&self) -> Option<&SettingInfo> {
        self.entries.get(self.selected)
    }

    fn move_selection(&mut self, delta: isize) {
        let next = self.selected as isize + delta;
        self.set_selection(next.clamp(0, self.entries.len().saturating_sub(1) as isize) as usize);
    }

    fn set_selection(&mut self, selected: usize) {
        self.selected = selected.min(self.entries.len().saturating_sub(1));
        self.clamp();
    }

    fn clamp(&mut self) {
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

// ── Private helpers ──────────────────────────────────────────────────────────

fn setting_detail(entry: &SettingInfo) -> String {
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

fn matches_query(entry: &SettingInfo, needle: &str) -> bool {
    entry.name.to_lowercase().contains(needle)
        || entry.key.to_lowercase().contains(needle)
        || entry.description.to_lowercase().contains(needle)
        || entry.group.to_lowercase().contains(needle)
}

fn edit_options(entry: &SettingInfo) -> Vec<&'static str> {
    match entry.key {
        "theme" => vec!["plain", "odyssey", "odyssey-noir"],
        "visual" => vec!["off", "ambient"],
        "subpixel" => vec!["off", "rgb", "bgr"],
        "cursor_style" => vec!["block", "underline", "bar"],
        "cursor_blink" => vec!["auto", "on", "off"],
        _ => entry.options.to_vec(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{FONT_SIZE_ENV, Settings};
    use std::collections::BTreeSet;

    const EXPERT_ONLY_GROUPS: &[&str] = &[];

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Navigate to Level 2 for the section containing `key`, then select that
    /// entry. Keeps the rest of the panel state (edits, etc.) intact, so callers
    /// can test value changes without re-creating the panel.
    fn select_key(panel: &mut SettingsPanel, key: &str) {
        let group = panel
            .all_entries
            .iter()
            .find(|e| e.key == key)
            .expect("known key")
            .group;
        let section_index = SECTIONS
            .iter()
            .position(|s| s.groups.contains(&group))
            .expect("known group in SECTIONS");
        // Only drill in if not already in the right section.
        match &panel.level {
            SettingsLevel::SectionDetail { section_index: si } if *si == section_index => {}
            _ => panel.drill_into_section(section_index),
        }
        let idx = panel
            .entries
            .iter()
            .position(|e| e.key == key)
            .expect("key in section entries");
        panel.set_selection(idx);
    }

    fn clear_edit_buffer(panel: &mut SettingsPanel) {
        let len = panel
            .editing
            .as_ref()
            .map(|edit| edit.buffer.chars().count())
            .unwrap_or(0);
        for _ in 0..len {
            let _ = panel.handle_input(OverlayInput::Backspace);
        }
    }

    fn poll_path_picker(panel: &mut SettingsPanel) {
        for _ in 0..50 {
            panel.update_body_height(20);
            panel.update_body_width(80);
            if panel
                .build_visible_rows(80, 20)
                .iter()
                .all(|(line, _)| !line.text.contains("Loading..."))
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    // ── Existing tests (updated for two-level model) ─────────────────────────

    #[test]
    fn descriptions_are_complete_for_every_setting() {
        let settings = Settings::default();
        let entries = settings.setting_info();
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|entry| !entry.description.trim().is_empty())
        );
    }

    #[test]
    fn every_setting_group_has_one_visible_section_unless_expert_only() {
        let settings = Settings::default();
        let catalog_groups = settings
            .setting_info()
            .into_iter()
            .map(|entry| entry.group)
            .collect::<BTreeSet<_>>();

        assert!(!catalog_groups.is_empty());

        for group in EXPERT_ONLY_GROUPS {
            assert!(
                catalog_groups.contains(*group),
                "expert-only group {group:?} is not present in the settings catalog"
            );
            let section_count = SECTIONS
                .iter()
                .filter(|section| section.groups.contains(group))
                .count();
            assert_eq!(
                section_count, 0,
                "expert-only group {group:?} should not map to a visible section"
            );
        }

        for group in catalog_groups {
            if EXPERT_ONLY_GROUPS.contains(&group) {
                continue;
            }
            let section_count = SECTIONS
                .iter()
                .filter(|section| section.groups.contains(&group))
                .count();
            assert_eq!(
                section_count, 1,
                "setting group {group:?} should map to exactly one visible section"
            );
        }
    }

    #[test]
    fn panel_navigation_is_bounded_and_scrolls() {
        let mut panel = SettingsPanel::new(&Settings::default());
        // At Level 1, Down moves section_selected.
        assert_eq!(panel.render_signature().section_selected, 0);
        let _ = panel.handle_input(OverlayInput::Down);
        assert_eq!(panel.render_signature().section_selected, 1);

        // Drill into a section; Level-2 navigation uses selected/scroll.
        panel.drill_into_section(2); // Rendering (many entries)
        assert_eq!(panel.render_signature().selected, 0);
        let _ = panel.handle_input(OverlayInput::Down);
        assert_eq!(panel.render_signature().selected, 1);
        let _ = panel.handle_input(OverlayInput::End);
        let end = panel.render_signature();
        assert_eq!(end.selected, end.entries.len() - 1);
        assert!(end.scroll > 0);
        let _ = panel.handle_input(OverlayInput::Home);
        assert_eq!(panel.render_signature().selected, 0);
    }

    #[test]
    fn display_rows_include_current_values_and_help_text() {
        let settings = Settings {
            font_size_px: 18.0,
            ..Settings::default()
        };
        let mut panel = SettingsPanel::new(&settings);
        // Drill into Fonts section to see font_size.
        select_key(&mut panel, "font_size");
        let lines = panel.visible_lines(70, 80);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        let font_size_line = lines
            .iter()
            .find(|line| line.text.contains("Font size:"))
            .expect("font size row present");
        assert!(
            font_size_line.text.contains("[<]  18  [>]"),
            "stepper readout shows the live value: {:?}",
            font_size_line.text
        );
        assert!(text.contains(FONT_SIZE_ENV));
    }

    #[test]
    fn bool_toggle_applies_and_revert_clears_diff() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "synthetic_styles");

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected bool toggle to apply");
        };
        assert!(!settings.synthetic_styles);
        assert_eq!(panel.render_signature().changed_count, 1);

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected bool revert to apply");
        };
        assert!(settings.synthetic_styles);
        assert_eq!(panel.render_signature().changed_count, 0);
    }

    #[test]
    fn themed_ui_roles_row_is_documented_and_editable() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "themed_ui_roles");
        let lines = panel.visible_lines(80, 80);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Themed UI roles: on"));
        assert!(text.contains(crate::settings::THEMED_UI_ROLES_ENV));
        assert!(text.contains("legacy foreground cursor"));

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected bool toggle to apply");
        };
        assert!(!settings.themed_ui_roles);
        assert_eq!(panel.render_signature().changed_count, 1);
    }

    #[test]
    fn symbol_fallback_rows_are_documented_and_editable() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "symbol_fallback");
        let lines = panel.visible_lines(96, 80);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("Symbol fallback: on"));
        assert!(text.contains(crate::settings::SYMBOL_FALLBACK_ENV));
        assert!(text.contains("plain missing-glyph path"));

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected bool toggle to apply");
        };
        assert!(!settings.symbol_fallback);
        assert_eq!(panel.render_signature().changed_count, 1);

        // symbol_font is a Path row → opens path picker in the new model.
        select_key(&mut panel, "symbol_font");
        let lines = panel.visible_lines(96, 80);
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Symbol font file: auto"));
        assert!(text.contains(crate::settings::SYMBOL_FONT_ENV));
        assert!(text.contains("bundled symbols face"));

        // Enter opens the path picker (new behaviour; was RowEdit).
        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::Consumed
        );
        assert!(
            panel.path_picker.is_some(),
            "path picker opened for symbol_font"
        );
        // Esc cancels without changing the value.
        assert_eq!(
            panel.handle_input(OverlayInput::Close),
            SettingsPanelOutcome::Consumed
        );
        assert!(panel.path_picker.is_none(), "picker closed on Esc");
    }

    #[test]
    fn save_reports_changes_and_success_clears_diff() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "visual");
        let SettingsPanelOutcome::Apply(_) = panel.handle_input(OverlayInput::Right) else {
            panic!("expected enum cycle to apply");
        };

        let SettingsPanelOutcome::Save(changes) = panel.handle_input(OverlayInput::Save) else {
            panic!("expected save request");
        };
        assert_eq!(changes.len(), 1);
        panel.save_succeeded(changes.len());
        let signature = panel.render_signature();
        assert_eq!(signature.changed_count, 0);
        assert!(
            signature
                .message
                .as_deref()
                .is_some_and(|message| message.contains("Saved 1"))
        );
    }

    #[test]
    fn enum_cycle_applies_next_value() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "visual");

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Right) else {
            panic!("expected enum cycle to apply");
        };
        assert_eq!(settings.visual.as_str(), "off");
        assert_eq!(panel.render_signature().changed_count, 1);
    }

    #[test]
    fn theme_enter_opens_theme_picker_in_two_level_model() {
        // In the two-level model, Enter on the theme row opens the theme picker,
        // not a text editor (D-S2L-3 decision).
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "theme");

        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::OpenThemePicker
        );
        // No editing started.
        assert_eq!(panel.render_signature().editing_key, None);
    }

    #[test]
    fn theme_row_left_right_cycle_theme_without_opening_picker() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "theme");

        assert!(matches!(
            panel.handle_input(OverlayInput::Right),
            SettingsPanelOutcome::Apply(_)
        ));
        assert!(matches!(
            panel.handle_input(OverlayInput::Left),
            SettingsPanelOutcome::Apply(_)
        ));
    }

    #[test]
    fn theme_row_b_opens_builder() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "theme");

        assert_eq!(
            panel.handle_input(OverlayInput::Char('b')),
            SettingsPanelOutcome::OpenThemeBuilder
        );
    }

    #[test]
    fn themes_section_has_open_theme_builder_action_entry() {
        // v0.3.1 discoverability: the Themes Level-2 list ends with a selectable
        // "Open Theme Builder" action row that emits OpenThemeBuilder on Enter —
        // no `b` press, no row edit (the operator couldn't find the builder).
        let mut panel = SettingsPanel::new(&Settings::default());
        let themes = SECTIONS
            .iter()
            .position(|s| s.name == "Themes")
            .expect("Themes section");
        panel.drill_into_section(themes);

        // The action entry is present and last in the list.
        let action_pos = panel
            .entries
            .iter()
            .position(|e| e.key == THEME_BUILDER_ACTION_KEY)
            .expect("action entry present in Themes");
        assert_eq!(
            action_pos,
            panel.entries.len() - 1,
            "the action row sits at the end of the Themes entries"
        );
        assert_eq!(panel.entries[action_pos].name, "Open Theme Builder");

        // Activating it opens the builder directly.
        panel.set_selection(action_pos);
        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::OpenThemeBuilder
        );
    }

    #[test]
    fn theme_builder_action_survives_live_value_sync() {
        // A live settings echo (apply_settings) must not drop the synthetic
        // action row — it has no real value, so a naive value-sync would force a
        // group-filter rebuild that loses it.
        let mut panel = SettingsPanel::new(&Settings::default());
        let themes = SECTIONS
            .iter()
            .position(|s| s.name == "Themes")
            .expect("Themes section");
        panel.drill_into_section(themes);
        assert!(
            panel
                .entries
                .iter()
                .any(|e| e.key == THEME_BUILDER_ACTION_KEY)
        );

        panel.apply_settings(&Settings::default());
        assert!(
            panel
                .entries
                .iter()
                .any(|e| e.key == THEME_BUILDER_ACTION_KEY),
            "the action row survives a live value-sync"
        );
    }

    #[test]
    fn font_family_enter_opens_font_picker() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_family");

        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::OpenFontPicker
        );
    }

    #[test]
    fn font_family_left_right_are_no_op_for_picker() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_family");

        assert_eq!(
            panel.handle_input(OverlayInput::Right),
            SettingsPanelOutcome::Consumed
        );
        assert_eq!(
            panel.handle_input(OverlayInput::Left),
            SettingsPanelOutcome::Consumed
        );
    }

    #[test]
    fn number_entry_uses_parser_clamp() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_size");
        let _ = panel.handle_input(OverlayInput::Activate);
        clear_edit_buffer(&mut panel);
        for ch in "200".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Activate)
        else {
            panic!("expected number edit to apply");
        };
        assert_eq!(settings.font_size_px, crate::settings::MAX_FONT_SIZE_PX);
    }

    #[test]
    fn number_step_is_clamped_and_does_not_reframe_scroll() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_size");
        panel.update_body_height(18);

        for _ in 0..200 {
            let _ = panel.handle_input(OverlayInput::Right);
        }
        let max = panel
            .entries
            .iter()
            .find(|entry| entry.key == "font_size")
            .expect("font_size entry")
            .value
            .parse::<f32>()
            .unwrap_or(f32::NAN);
        assert_eq!(max, crate::settings::MAX_FONT_SIZE_PX);

        let before_scroll = panel.render_signature().scroll;
        let _ = panel.handle_input(OverlayInput::Right);
        assert_eq!(
            panel.render_signature().scroll,
            before_scroll,
            "scroll should not move when a number is clamped at max"
        );

        for _ in 0..220 {
            let _ = panel.handle_input(OverlayInput::Left);
        }
        let min = panel
            .entries
            .iter()
            .find(|entry| entry.key == "font_size")
            .expect("font_size entry")
            .value
            .parse::<f32>()
            .unwrap_or(f32::NAN);
        assert_eq!(min, crate::settings::MIN_FONT_SIZE_PX);

        let before_scroll = panel.render_signature().scroll;
        let _ = panel.handle_input(OverlayInput::Left);
        assert_eq!(
            panel.render_signature().scroll,
            before_scroll,
            "scroll should stay stable at min clamp"
        );
    }

    #[test]
    fn path_entry_opens_path_picker_and_cancel_is_clean() {
        // Path rows open the inline path picker in the two-level model.
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font");
        // Enter opens picker.
        let outcome = panel.handle_input(OverlayInput::Activate);
        assert_eq!(outcome, SettingsPanelOutcome::Consumed);
        assert!(panel.path_picker.is_some(), "path picker opened");
        assert_eq!(
            panel.render_signature().editing_key,
            None,
            "not in text edit"
        );
        // Esc cancels without a value change.
        let _ = panel.handle_input(OverlayInput::Close);
        assert!(panel.path_picker.is_none(), "picker closed on Esc");
        assert_eq!(
            panel.render_signature().changed_count,
            0,
            "no change after cancel"
        );
    }

    #[test]
    fn path_picker_state_is_render_observable() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("odytty-path-sig-{unique}"));
        fs::create_dir(&dir).expect("create temp dir");
        fs::create_dir(dir.join("child")).expect("create child dir");
        fs::write(dir.join("wall.png"), b"not a real png").expect("write image path");

        let mut panel = SettingsPanel::new(&Settings {
            background_image: Some(dir.clone()),
            ..Settings::default()
        });
        select_key(&mut panel, "background_image");

        let before_open = panel.render_signature();
        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::Consumed
        );
        let opened = panel.render_signature();
        assert_ne!(before_open, opened, "opening picker must repaint overlay");
        assert!(
            opened.path_picker.is_some(),
            "picker state participates in render signature"
        );

        poll_path_picker(&mut panel);
        let loaded = panel.render_signature();
        assert!(
            loaded.path_picker.as_ref().is_some_and(|sig| !sig.loading),
            "loaded picker state is observable"
        );

        let _ = panel.handle_input(OverlayInput::Down);
        assert_ne!(
            loaded,
            panel.render_signature(),
            "picker selection changes must repaint overlay"
        );

        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[test]
    fn path_picker_pointer_click_activates_picker_entry() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("odytty-path-click-{unique}"));
        fs::create_dir(&dir).expect("create temp dir");
        let image_path = dir.join("wall.png");
        fs::write(&image_path, b"not a real png").expect("write image path");

        let mut panel = SettingsPanel::new(&Settings {
            background_image: Some(dir.clone()),
            ..Settings::default()
        });
        select_key(&mut panel, "background_image");
        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::Consumed
        );
        assert!(panel.path_picker.is_some(), "path picker opened");
        poll_path_picker(&mut panel);

        let rows = panel.build_visible_rows(80, 20);
        let image_row = rows
            .iter()
            .enumerate()
            .find_map(|(row, (line, hit))| {
                (line.text.contains("wall.png") && hit.entry_index.is_some()).then_some(row)
            })
            .expect("image file row visible");

        let SettingsPanelOutcome::Apply(settings) = panel.handle_pointer_press(
            80,
            20,
            image_row,
            0,
            crate::native::overlay::PointerButton::Left,
            None,
        ) else {
            panic!("path-picker click should commit the clicked path");
        };
        assert_eq!(
            settings.background_image.as_deref(),
            Some(image_path.as_path())
        );
        assert!(panel.path_picker.is_none(), "picker closes after selection");

        fs::remove_dir_all(dir).expect("remove temp dir");
    }

    #[test]
    fn committing_background_image_also_enables_image_treatment() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "background_image");

        let SettingsPanelOutcome::Apply(settings) =
            panel.commit_value("background_image", "/tmp/wall.jpg")
        else {
            panic!("background image commit should apply");
        };

        assert_eq!(
            settings.background_treatment,
            crate::settings::BackgroundTreatment::Image
        );
        assert_eq!(
            settings.background_image.as_deref(),
            Some(std::path::Path::new("/tmp/wall.jpg"))
        );
        assert!(
            (settings.cell_bg_opacity - 0.85).abs() < 1e-3,
            "new wallpapers get a visible default"
        );
        let treatment = panel
            .render_signature()
            .entries
            .into_iter()
            .find(|entry| entry.key == "background_treatment")
            .expect("background treatment entry present");
        assert_eq!(treatment.value, "image");
    }

    #[test]
    fn background_scrim_auto_steps_to_numeric_override() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "background_image_scrim");

        let SettingsPanelOutcome::Apply(settings) = panel.handle_input(OverlayInput::Right) else {
            panic!("scrim step should apply");
        };

        assert_eq!(settings.background_image_scrim, Some(0.05));
        assert_eq!(
            panel.selected_entry().expect("selected entry").value,
            "0.05"
        );
    }

    #[test]
    fn font_family_failure_surfaces_clear_overlay_message() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_family");
        // font_family is now an Enum-like row that opens the font picker (not
        // RowEdit). Activating it emits OpenFontPicker.
        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::OpenFontPicker
        );
        // No message about a failed family name — the picker is the UX.
        // Verify the changed_count is still 0.
        assert_eq!(panel.render_signature().changed_count, 0);
    }

    #[test]
    fn invalid_edit_is_rejected_in_panel() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_size");
        let _ = panel.handle_input(OverlayInput::Activate);
        clear_edit_buffer(&mut panel);
        for ch in "nope".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }

        assert_eq!(
            panel.handle_input(OverlayInput::Activate),
            SettingsPanelOutcome::Consumed
        );
        let signature = panel.render_signature();
        assert_eq!(signature.changed_count, 0);
        assert!(
            signature
                .message
                .as_deref()
                .is_some_and(|message| message.contains("valid pixel size"))
        );
    }

    #[test]
    fn slash_enters_search_and_filters_to_matches() {
        let mut panel = SettingsPanel::new(&Settings::default());
        let total = panel.render_signature().entries.len();
        // `/` at Level 1 enters search.
        assert!(panel.handle_input(OverlayInput::Char('/')) == SettingsPanelOutcome::Consumed);
        assert!(panel.is_searching());
        for ch in "cursor".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        let sig = panel.render_signature();
        assert!(sig.search_active);
        assert_eq!(sig.query, "cursor");
        assert!(!sig.entries.is_empty() && sig.entries.len() < total);
        assert!(
            sig.entries
                .iter()
                .all(|entry| entry.key.contains("cursor") || entry.key == "cursor_blink")
                || sig.entries.iter().any(|entry| entry.key.contains("cursor"))
        );
        let text = panel
            .visible_lines(80, 80)
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Search: cursor"));
    }

    #[test]
    fn search_matches_against_description_text() {
        let mut panel = SettingsPanel::new(&Settings::default());
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "legacy".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        let sig = panel.render_signature();
        assert!(
            !sig.entries.is_empty(),
            "a description-only match is surfaced"
        );
    }

    #[test]
    fn two_step_escape_clears_then_exits_search() {
        let mut panel = SettingsPanel::new(&Settings::default());
        let total = panel.render_signature().entries.len();
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "font".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        assert!(!panel.render_signature().query.is_empty());
        // First Esc clears query, stays in search.
        let _ = panel.handle_input(OverlayInput::Close);
        let sig = panel.render_signature();
        assert!(sig.search_active);
        assert!(sig.query.is_empty());
        assert_eq!(sig.entries.len(), total);
        // Second Esc exits search entirely.
        let _ = panel.handle_input(OverlayInput::Close);
        let sig = panel.render_signature();
        assert!(!sig.search_active);
        assert_eq!(sig.entries.len(), total);
    }

    #[test]
    fn backspace_trims_query_and_refilters() {
        let mut panel = SettingsPanel::new(&Settings::default());
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "cursor".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        let narrowed = panel.render_signature().entries.len();
        let _ = panel.handle_input(OverlayInput::Backspace);
        let sig = panel.render_signature();
        assert_eq!(sig.query, "curso");
        assert!(sig.entries.len() >= narrowed);
    }

    #[test]
    fn no_match_query_shows_notice_and_keeps_overlay() {
        let mut panel = SettingsPanel::new(&Settings::default());
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "zzzznosuchsetting".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        let sig = panel.render_signature();
        assert!(sig.search_active);
        assert!(sig.entries.is_empty());
        let text = panel
            .visible_lines(80, 80)
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("No settings match"));
    }

    #[test]
    fn selection_clamps_after_filter_narrows_list() {
        let mut panel = SettingsPanel::new(&Settings::default());
        let _ = panel.handle_input(OverlayInput::End);
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "theme".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        let sig = panel.render_signature();
        assert!(sig.selected < sig.entries.len().max(1));
    }

    #[test]
    fn empty_query_signature_matches_unsearched_panel() {
        let baseline = SettingsPanel::new(&Settings::default())
            .render_signature()
            .entries;
        let mut panel = SettingsPanel::new(&Settings::default());
        let _ = panel.handle_input(OverlayInput::Char('/'));
        let entries = panel.render_signature().entries;
        assert_eq!(entries, baseline);
    }

    #[test]
    fn editing_a_filtered_row_exits_search_cleanly() {
        // In the new model, Enter in search drills into the entry's section
        // instead of starting a text edit. For non-bool/enum entries, this
        // moves to Level 2 and selects the entry.
        let mut panel = SettingsPanel::new(&Settings::default());
        let total = panel.render_signature().entries.len();
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "bloom intensity".chars() {
            if ch == ' ' {
                break; // avoid space activating
            }
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        // Move to a numeric row and activate → drills into its section.
        select_key(&mut panel, "font_size");
        let _ = panel.handle_input(OverlayInput::Activate);
        // After drilling in, search should be exited.
        let sig = panel.render_signature();
        assert!(!sig.search_active, "drill exits search");
        assert!(
            sig.entries.len() < total,
            "section-filtered roster is shorter than full list"
        );
    }

    #[test]
    fn refresh_clears_active_search() {
        let mut panel = SettingsPanel::new(&Settings::default());
        let total = panel.render_signature().entries.len();
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "cursor".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        assert!(panel.is_searching());
        panel.refresh(&Settings::default());
        let sig = panel.render_signature();
        assert!(!sig.search_active);
        assert!(sig.query.is_empty());
        assert_eq!(sig.entries.len(), total);
    }

    #[test]
    fn arrowing_to_last_entry_keeps_it_visible() {
        let mut panel = SettingsPanel::new(&Settings::default());
        // Drill into Rendering (many entries) for Level-2 behavior.
        panel.drill_into_section(2);
        let body_height = 24;
        panel.update_body_height(body_height);
        let _ = panel.handle_input(OverlayInput::End);
        let sig = panel.render_signature();
        let last = sig.entries.len() - 1;
        assert_eq!(sig.selected, last, "End navigates to the last entry");

        let body_width = 80;
        let lines = panel.visible_lines(body_width, body_height);
        let selected_key = panel.entries[panel.selected].key;
        let hit_map = panel.visible_hit_map(body_width, body_height);
        assert_eq!(lines.len(), hit_map.len());
        let visible_value = hit_map.iter().enumerate().any(|(row_i, hit)| {
            use crate::native::settings_panel::pointer::RowZone;
            hit.entry_index == Some(last)
                && matches!(hit.zone, RowZone::Value | RowZone::Stepper { .. })
                && lines[row_i].focused
        });
        assert!(
            visible_value,
            "selected entry '{selected_key}' value/stepper row must be in the rendered window \
             (scroll={}, body_height={body_height})",
            sig.scroll,
        );
    }

    #[test]
    fn setting_value_rows_are_bold_and_headers_are_not() {
        let mut panel = SettingsPanel::new(&Settings::default());
        // Drill into Rendering to get a mix of header + value rows.
        panel.drill_into_section(2);
        let lines = panel.visible_lines(80, 40);
        assert!(
            !lines[0].bold,
            "first line (group header) must not be bold: {:?}",
            lines[0].text
        );
        let has_bold = lines.iter().any(|line| line.bold);
        assert!(has_bold, "no bold rows found in settings panel lines");
        let detail_line = lines
            .iter()
            .find(|line| line.text.starts_with("    "))
            .expect("at least one detail line present");
        assert!(!detail_line.bold, "detail lines must not be bold");
    }

    // ── Two-level model trap tests ────────────────────────────────────────────

    /// T-level-hitmap: Level-1 section rows use `SectionRow` zone; Level-2
    /// setting rows use `Value`/`Stepper`. Hit-map switches correctly with level.
    #[test]
    fn level_hitmap_switches_on_level_change() {
        use crate::native::settings_panel::pointer::RowZone;
        let panel = SettingsPanel::new(&Settings::default());
        // Level 1: expect SectionRow zones.
        let hits = panel.visible_hit_map(80, 20);
        assert!(
            hits.iter().any(|h| h.zone == RowZone::SectionRow),
            "Level 1 must emit SectionRow zones"
        );
        assert!(
            !hits
                .iter()
                .any(|h| matches!(h.zone, RowZone::Value | RowZone::Stepper { .. })),
            "Level 1 must not emit Value/Stepper zones"
        );

        // Level 2: expect Value/Stepper zones, no SectionRow.
        let mut panel2 = SettingsPanel::new(&Settings::default());
        panel2.drill_into_section(0); // Themes
        let hits2 = panel2.visible_hit_map(80, 20);
        assert!(
            !hits2.iter().any(|h| h.zone == RowZone::SectionRow),
            "Level 2 must not emit SectionRow zones"
        );
        assert!(
            hits2
                .iter()
                .any(|h| matches!(h.zone, RowZone::Value | RowZone::Stepper { .. })),
            "Level 2 must emit Value/Stepper zones"
        );
    }

    /// T-scroll-per-level: Level-1 section_scroll and Level-2 scroll are
    /// independent; entering Level 2 starts at top; returning to Level 1
    /// restores section_scroll.
    #[test]
    fn scroll_is_independent_per_level() {
        let mut panel = SettingsPanel::new(&Settings::default());
        // Move section_selected so section_scroll might change (navigate down).
        for _ in 0..5 {
            let _ = panel.handle_input(OverlayInput::Down);
        }
        let l1_selected = panel.render_signature().section_selected;

        // Drill into Rendering.
        panel.drill_into_section(2);
        assert_eq!(panel.render_signature().scroll, 0, "Level 2 starts at top");

        // Scroll down at Level 2.
        panel.scroll_lines(3);
        assert_eq!(
            panel.render_signature().scroll,
            3,
            "Level 2 scroll advanced"
        );

        // Back to Level 1 (Esc).
        let _ = panel.handle_input(OverlayInput::Close);
        let sig = panel.render_signature();
        assert_eq!(
            sig.level,
            SettingsLevel::SectionList,
            "Esc at Level 2 returns to Level 1"
        );
        assert_eq!(
            sig.section_selected, l1_selected,
            "Level 1 selection is restored"
        );
    }

    /// T-editing-clears-on-level-change: Esc at Level 2 while editing clears
    /// editing before returning to Level 1.
    #[test]
    fn editing_is_cleared_on_level_change() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_size");
        let _ = panel.handle_input(OverlayInput::Activate); // opens RowEdit
        assert!(panel.render_signature().editing_key.is_some(), "edit open");
        // Esc cancels the edit (stays at Level 2).
        let _ = panel.handle_input(OverlayInput::Close);
        assert!(
            panel.render_signature().editing_key.is_none(),
            "edit cleared after first Esc"
        );
        // Still at Level 2.
        assert!(
            matches!(
                panel.render_signature().level,
                SettingsLevel::SectionDetail { .. }
            ),
            "still at Level 2 after edit cancel"
        );
        // Second Esc returns to Level 1.
        let _ = panel.handle_input(OverlayInput::Close);
        assert_eq!(
            panel.render_signature().level,
            SettingsLevel::SectionList,
            "second Esc returns to Level 1"
        );
        // Editing must be None at Level 1 too.
        assert!(panel.render_signature().editing_key.is_none());
    }

    /// T-changed-count-survives: pending edits survive drill-in and back.
    #[test]
    fn changed_count_survives_level_transitions() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "synthetic_styles");
        let _ = panel.handle_input(OverlayInput::Activate); // toggle bool
        assert_eq!(panel.render_signature().changed_count, 1, "1 edit recorded");

        // Back to Level 1.
        let _ = panel.handle_input(OverlayInput::Close);
        assert_eq!(
            panel.render_signature().changed_count,
            1,
            "edit survives return to Level 1"
        );

        // Drill into another section.
        panel.drill_into_section(4); // Cursor
        assert_eq!(
            panel.render_signature().changed_count,
            1,
            "edit survives drill into another section"
        );

        // Back to Level 1 again.
        let _ = panel.handle_input(OverlayInput::Close);
        assert_eq!(
            panel.render_signature().changed_count,
            1,
            "edit survives multiple level transitions"
        );
    }

    /// T-two-substates: path_picker and pending_close_prompt are mutually
    /// exclusive; activating one clears the other.
    #[test]
    fn two_substates_are_mutually_exclusive() {
        let mut panel = SettingsPanel::new(&Settings::default());
        // Open the dirty-close prompt.
        select_key(&mut panel, "synthetic_styles");
        let _ = panel.handle_input(OverlayInput::Activate); // make it dirty
        let _ = panel.handle_input(OverlayInput::Close); // Esc at Level 2 → Level 1
        let _ = panel.handle_input(OverlayInput::Close); // Esc at Level 1 dirty → prompt
        assert!(
            panel.render_signature().pending_close_prompt,
            "dirty prompt opened"
        );
        // While the prompt is showing, path_picker must not be active.
        assert!(
            panel.path_picker.is_none(),
            "no path_picker while prompt is showing"
        );

        // Cancel the prompt.
        let _ = panel.handle_input(OverlayInput::Char('c'));
        assert!(
            !panel.render_signature().pending_close_prompt,
            "prompt cancelled"
        );

        // Open a path picker.
        select_key(&mut panel, "font");
        let _ = panel.handle_input(OverlayInput::Activate);
        assert!(panel.path_picker.is_some(), "path picker opened");
        // pending_close_prompt must not be active.
        assert!(
            !panel.render_signature().pending_close_prompt,
            "no dirty prompt while path picker is open"
        );
    }

    /// T-search-vs-level: `/` is inert at Level 2; it only opens search at Level 1.
    #[test]
    fn slash_is_inert_at_level_two() {
        let mut panel = SettingsPanel::new(&Settings::default());
        panel.drill_into_section(2); // Rendering
        // `/` at Level 2 must not enter search mode.
        let _ = panel.handle_input(OverlayInput::Char('/'));
        assert!(
            !panel.render_signature().search_active,
            "search must not activate at Level 2"
        );
    }

    /// T-identity: fresh panel + no edits → Esc emits Close (not consumed);
    /// Ctrl+S with no changes shows a "no unsaved" message.
    #[test]
    fn identity_esc_closes_and_save_nops_when_clean() {
        let mut panel = SettingsPanel::new(&Settings::default());
        // Level 1, clean → Esc should return Close.
        assert_eq!(
            panel.handle_input(OverlayInput::Close),
            SettingsPanelOutcome::Close,
            "Esc at Level 1 clean must emit Close"
        );
        // Ctrl+S with no changes.
        let outcome = panel.handle_input(OverlayInput::Save);
        assert_eq!(
            outcome,
            SettingsPanelOutcome::Consumed,
            "Ctrl+S with no changes must be Consumed"
        );
        let msg = panel.render_signature().message;
        assert!(
            msg.as_deref().is_some_and(|m| m.contains("No unsaved")),
            "no-op save shows a message"
        );
    }

    /// Dirty-close prompt: Esc at Level 1 with pending edits opens the prompt;
    /// S saves-and-closes; D discards-and-closes; C cancels.
    #[test]
    fn dirty_close_prompt_flow() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "synthetic_styles");
        let _ = panel.handle_input(OverlayInput::Activate); // make dirty
        let _ = panel.handle_input(OverlayInput::Close); // Esc at Level 2 → Level 1
        assert_eq!(panel.render_signature().changed_count, 1);

        // Esc at Level 1 with dirty edits → shows the prompt.
        let outcome = panel.handle_input(OverlayInput::Close);
        assert_eq!(outcome, SettingsPanelOutcome::Consumed);
        assert!(
            panel.render_signature().pending_close_prompt,
            "prompt is showing"
        );

        // C (cancel) clears the prompt and returns to settings.
        let _ = panel.handle_input(OverlayInput::Char('c'));
        assert!(
            !panel.render_signature().pending_close_prompt,
            "prompt dismissed"
        );
        assert_eq!(
            panel.render_signature().changed_count,
            1,
            "edits still present"
        );

        // Re-show the prompt.
        let _ = panel.handle_input(OverlayInput::Close); // Level 1 dirty → prompt
        // D discards and closes.
        let outcome = panel.handle_input(OverlayInput::Char('d'));
        assert_eq!(outcome, SettingsPanelOutcome::DiscardAndClose);
        assert!(!panel.render_signature().pending_close_prompt);

        // Re-show the prompt with a fresh edit. Use a different setting to
        // avoid the double-toggle cancellation (the previous edit is still in
        // the edits field since DiscardAndClose doesn't reset the panel edits —
        // that's the overlay/App layer's job). Use `visual` which starts at
        // "off" and hasn't been toggled yet, giving a net 1 change.
        // First reset the edits to a clean state.
        panel.refresh(&Settings::default());
        select_key(&mut panel, "visual");
        let _ = panel.handle_input(OverlayInput::Right); // cycle visual → "ambient"
        let _ = panel.handle_input(OverlayInput::Close); // Level 2 → Level 1
        let _ = panel.handle_input(OverlayInput::Close); // Level 1 dirty → prompt
        assert!(
            panel.render_signature().pending_close_prompt,
            "prompt appeared again"
        );

        // S saves-and-closes.
        let outcome = panel.handle_input(OverlayInput::Char('s'));
        let SettingsPanelOutcome::SaveAndClose(changes) = outcome else {
            panic!("expected SaveAndClose from S key in prompt");
        };
        assert_eq!(changes.len(), 1);
        assert!(!panel.render_signature().pending_close_prompt);
    }

    /// Level-1 Enter drills into the focused section; Level-2 Esc backs out.
    #[test]
    fn level1_enter_drills_and_level2_esc_backs_out() {
        let mut panel = SettingsPanel::new(&Settings::default());
        assert_eq!(
            panel.render_signature().level,
            SettingsLevel::SectionList,
            "starts at Level 1"
        );

        // Down to Fonts (index 1), then Enter.
        let _ = panel.handle_input(OverlayInput::Down); // section_selected = 1 (Fonts)
        let _ = panel.handle_input(OverlayInput::Activate);
        assert_eq!(
            panel.render_signature().level,
            SettingsLevel::SectionDetail { section_index: 1 },
            "Enter at Level 1 drills into Fonts"
        );
        // Entries should be the Fonts group only.
        assert!(
            panel
                .render_signature()
                .entries
                .iter()
                .all(|e| e.key == "font"
                    || e.key == "font_family"
                    || e.key == "font_size"
                    || e.key == "font_weight"
                    || e.key == "line_height"
                    || e.key == "synthetic_styles"
                    || e.key == "symbol_fallback"
                    || e.key == "symbol_font"
                    || e.key == "symbol_map"),
            "Level 2 Fonts shows Font-group entries"
        );

        // Esc at Level 2 → Level 1.
        let outcome = panel.handle_input(OverlayInput::Close);
        assert_eq!(
            outcome,
            SettingsPanelOutcome::Consumed,
            "Esc at Level 2 is Consumed"
        );
        assert_eq!(
            panel.render_signature().level,
            SettingsLevel::SectionList,
            "Esc at Level 2 returns to Level 1"
        );
    }

    // ── SETTINGS-PANEL-STATE-FIX regression tests ────────────────────────────

    /// Entry indices whose Value/Stepper row is currently visible at the active
    /// scroll, read from the shared row walker (the same source the pointer
    /// hit-map and `selected_in_window` consume).
    fn visible_entry_indices(panel: &SettingsPanel, w: usize, h: usize) -> Vec<usize> {
        use crate::native::settings_panel::pointer::RowZone;
        panel
            .build_settings_rows(w, h)
            .into_iter()
            .filter_map(|(_, hit)| match hit.zone {
                RowZone::Value | RowZone::Stepper { .. } => hit.entry_index,
                _ => None,
            })
            .collect()
    }

    /// Bug A: selecting (e.g. pressing) a row that is already on-screen must NOT
    /// recenter the viewport. The old `visible_slack` reframe yanked the view on
    /// any selection below the top third — this is what jumped a slider to the
    /// bottom the instant you adjusted it.
    #[test]
    fn selecting_a_visible_row_does_not_recenter_scroll() {
        let mut panel = SettingsPanel::new(&Settings::default());
        panel.drill_into_section(2); // Rendering (many entries)
        panel.update_body_width(90);
        panel.update_body_height(28);
        let _ = panel.visible_lines(90, 28);
        let vis = visible_entry_indices(&panel, 90, 28);
        let last_visible = *vis.iter().max().expect("some rows visible at top");
        assert!(
            last_visible >= 1,
            "need a non-top visible row to be meaningful"
        );
        // Pointer press path is `set_selection`; start from the top of scroll.
        panel.scroll = 0;
        panel.set_selection(last_visible);
        assert_eq!(
            panel.scroll, 0,
            "an already-visible row must not move the viewport"
        );
        assert!(
            panel.selected_in_window(28),
            "the selected row stays visible"
        );
    }

    /// Bug A [follow-lag] trap: arrowing the selection BELOW the visible window
    /// must still scroll — minimally — to reveal it (no VIEWPORT-FOLLOW-LAG
    /// regression), while arrowing within the window does not scroll.
    #[test]
    fn offscreen_selection_scrolls_minimally_within_window_does_not() {
        let mut panel = SettingsPanel::new(&Settings::default());
        panel.drill_into_section(2); // Rendering
        panel.update_body_width(90);
        panel.update_body_height(24);
        let _ = panel.visible_lines(90, 24);

        // Arrow within the visible window: no scroll.
        let vis = visible_entry_indices(&panel, 90, 24);
        let last_visible = *vis.iter().max().expect("rows visible");
        panel.set_selection(last_visible);
        assert_eq!(panel.scroll, 0, "still within window: no scroll");

        // Jump to the end: an off-screen selection must be revealed.
        panel.set_selection(panel.entries.len() - 1);
        assert!(
            panel.scroll > 0,
            "off-screen selection scrolls to reveal it"
        );
        assert!(
            panel.scroll <= panel.selected,
            "scroll never overshoots past selection"
        );
        assert!(panel.selected_in_window(24), "End selection is revealed");
    }

    /// Bug A end-clamp trap: keyboard steps at the numeric min/max saturate the
    /// value exactly and never jump the scroll.
    #[test]
    fn arrow_steps_saturate_at_min_and_max_without_scroll_jump() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "font_size");
        panel.update_body_width(90);
        panel.update_body_height(28);
        let _ = panel.visible_lines(90, 28);
        let spec = panel.selected_entry().unwrap().numeric.unwrap();

        // Drive to the minimum.
        for _ in 0..200 {
            let _ = panel.step_or_cycle_selected(-1);
        }
        let at_min = panel
            .selected_entry()
            .unwrap()
            .value
            .parse::<f32>()
            .unwrap();
        assert!((at_min - spec.min).abs() < 1e-3, "value saturates at min");
        let scroll_at_min = panel.scroll;
        let _ = panel.step_or_cycle_selected(-1); // already at the floor
        assert_eq!(panel.scroll, scroll_at_min, "no scroll jump at min");
        let still_min = panel
            .selected_entry()
            .unwrap()
            .value
            .parse::<f32>()
            .unwrap();
        assert!((still_min - spec.min).abs() < 1e-3, "value held at min");

        // Drive to the maximum.
        for _ in 0..400 {
            let _ = panel.step_or_cycle_selected(1);
        }
        let at_max = panel
            .selected_entry()
            .unwrap()
            .value
            .parse::<f32>()
            .unwrap();
        assert!((at_max - spec.max).abs() < 1e-3, "value saturates at max");
        let scroll_at_max = panel.scroll;
        let _ = panel.step_or_cycle_selected(1); // already at the ceiling
        assert_eq!(panel.scroll, scroll_at_max, "no scroll jump at max");
        let still_max = panel
            .selected_entry()
            .unwrap()
            .value
            .parse::<f32>()
            .unwrap();
        assert!((still_max - spec.max).abs() < 1e-3, "value held at max");
    }

    /// Bug B + Bug C: a live apply (the OverlayEdit round-trip seam) while
    /// drilled into a section must PRESERVE the section filter (Bug B) and the
    /// current level (Bug C), and must not clobber pending dirty edits. This is
    /// the bloom-threshold "multi-line slider row" shape the operator hit.
    #[test]
    fn live_apply_preserves_section_filter_level_and_dirty_state() {
        let mut panel = SettingsPanel::new(&Settings::default());
        select_key(&mut panel, "bloom_threshold");
        let section_keys: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
        assert!(section_keys.contains(&"bloom_threshold"));
        assert!(
            section_keys.len() < panel.all_entries.len(),
            "section view is a strict subset of all settings"
        );
        let level_before = panel.render_signature().level;
        assert!(matches!(level_before, SettingsLevel::SectionDetail { .. }));

        // Commit a value change in this section (creates a pending dirty edit).
        let entry = panel.selected_entry().unwrap().clone();
        let spec = entry.numeric.unwrap();
        let cur = entry.value.parse::<f32>().unwrap();
        let target = if (cur - spec.min).abs() > spec.step {
            spec.min
        } else {
            spec.max
        };
        let outcome = panel.commit_value(entry.key, &format!("{target:.3}"));
        assert!(
            matches!(outcome, SettingsPanelOutcome::Apply(_)),
            "a real value change applies"
        );
        assert_eq!(panel.render_signature().changed_count, 1, "one dirty edit");

        // Simulate the live-apply round-trip seam. The incoming `settings` is the
        // unedited baseline (as a Save's `Settings::from_env` re-read can differ
        // from the in-panel edit overlay) — pre-fix this triggered the spurious
        // level-resetting refresh().
        panel.apply_settings(&Settings::default());

        // Bug B: still only this section's settings.
        let after: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
        assert_eq!(
            after, section_keys,
            "section filter preserved after live apply"
        );
        // Bug C: level unchanged.
        assert_eq!(
            panel.render_signature().level,
            level_before,
            "drilled-in level preserved after live apply"
        );
        // [dirty-preserve]: the pending edit is not silently discarded.
        assert_eq!(
            panel.render_signature().changed_count,
            1,
            "pending dirty edit preserved across live apply"
        );
    }

    /// Bug B [search-preserve] trap: a live apply while searching must keep the
    /// search filter, not reset to the full list or drop search mode.
    #[test]
    fn live_apply_preserves_active_search_filter() {
        let mut panel = SettingsPanel::new(&Settings::default());
        // Enter search and type a needle that matches a known subset.
        let _ = panel.handle_input(OverlayInput::Char('/'));
        for ch in "bloom".chars() {
            let _ = panel.handle_input(OverlayInput::Char(ch));
        }
        assert!(panel.render_signature().search_active, "search is active");
        let filtered: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
        assert!(!filtered.is_empty() && filtered.len() < panel.all_entries.len());

        panel.apply_settings(&Settings::default());

        assert!(
            panel.render_signature().search_active,
            "search stays active after live apply"
        );
        let after: Vec<&'static str> = panel.entries.iter().map(|e| e.key).collect();
        assert_eq!(after, filtered, "search filter preserved after live apply");
    }

    #[test]
    fn rebase_onto_external_then_commit_does_not_revert_theme() {
        use crate::theme::Theme;

        let base = Settings {
            theme: Theme::PLAIN,
            ..Settings::default()
        };
        let mut panel = SettingsPanel::new(&base);
        select_key(&mut panel, "font_size");

        // Snapshot nav state before the external theme application.
        let (level_before, section_before) = (panel.level, panel.section_selected);
        assert!(
            matches!(level_before, SettingsLevel::SectionDetail { .. }),
            "precondition: drilled into a section"
        );

        // External theme application reconciles into the panel.
        panel.rebase_onto_external(&Settings {
            theme: Theme::ODYSSEY_NOIR,
            ..Settings::default()
        });

        assert_eq!(panel.level, level_before, "level preserved by rebase");
        assert_eq!(
            panel.section_selected, section_before,
            "section preserved by rebase"
        );
        // font_size should still be selected (re-find by key).
        assert_eq!(
            panel.entries.get(panel.selected).map(|e| e.key),
            Some("font_size"),
            "selected key preserved by rebase"
        );

        // Commit a different setting in the panel; this used to rebuild from a
        // stale theme baseline.
        let SettingsPanelOutcome::Apply(first) = panel.handle_input(OverlayInput::Right) else {
            panic!("font_size step should apply");
        };
        let SettingsPanelOutcome::Apply(second) = panel.handle_input(OverlayInput::Right) else {
            panic!("second font_size step should apply");
        };
        // Both commit rounds must carry the new theme (the bug reverted it).
        assert_eq!(first.theme, Theme::ODYSSEY_NOIR);

        assert_eq!(
            second.theme,
            Theme::ODYSSEY_NOIR,
            "panel commit must not revert the externally-applied theme"
        );
        assert_eq!(
            panel.render_signature().changed_count,
            1,
            "dirty count is font_size only, theme is clean baseline"
        );
        // Nav still intact after the commit too.
        assert_eq!(panel.level, level_before);
    }

    #[test]
    fn rebase_onto_external_preserves_pending_dirty_edit() {
        use crate::theme::Theme;

        let mut panel = SettingsPanel::new(&Settings {
            theme: Theme::PLAIN,
            ..Settings::default()
        });
        select_key(&mut panel, "font_size");
        let SettingsPanelOutcome::Apply(_) = panel.handle_input(OverlayInput::Right) else {
            panic!("font_size step should apply");
        };
        assert_eq!(panel.render_signature().changed_count, 1);

        panel.rebase_onto_external(&Settings {
            theme: Theme::ODYSSEY,
            ..Settings::default()
        });

        assert_eq!(panel.render_signature().changed_count, 1, "edit survived");
        assert_eq!(
            panel.edits.settings().theme,
            Theme::ODYSSEY,
            "theme adopted as clean baseline"
        );
        assert_eq!(
            panel
                .edits
                .changes()
                .iter()
                .map(|c| c.key)
                .collect::<Vec<_>>(),
            vec!["font_size"]
        );
    }
}

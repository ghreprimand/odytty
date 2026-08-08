// SPDX-License-Identifier: GPL-3.0-only
use crate::settings::{
    DEFAULT_CELL_BG_OPACITY, MAX_TAB_BAR_ROWS, MIN_TAB_BAR_ROWS, SettingEdit, SettingInfo,
    SettingKind, Settings, SettingsEditOverlay, TabBarHeight,
};

use super::about::{ABOUT_LINKS, AboutInfo};
use super::overlay::OverlayInput;

mod path_picker;
mod pointer;
mod sections;

use path_picker::{PathPickerOutcome, PathPickerSignature, PathPickerState, resolve_start_dir};
use sections::SECTIONS;

/// Count of actionable rows in the About view: one per project link plus the
/// Copy-diagnostics row. `selected` indexes these while at `SettingsLevel::About`.
const ABOUT_ACTION_ROWS: usize = ABOUT_LINKS.len() + 1;
/// The index of the Copy-diagnostics action row (last actionable row).
const ABOUT_COPY_ROW: usize = ABOUT_LINKS.len();

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
    /// Level 2 (ABOUT): the read-only About view. Reached by drilling into the
    /// synthetic "About" row appended after `SECTIONS`. Has no setting entries;
    /// `selected` indexes its actionable rows (links + Copy diagnostics).
    About,
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
    /// Read-only About data, populated when the overlay opens (ABOUT). `None`
    /// until set; the About row still renders, showing a not-initialized note.
    about: Option<AboutInfo>,
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
    /// Live contents of the in-progress row edit. MUST be in the signature so
    /// each typed character (and Backspace) reclassifies the render cache to a
    /// repaint; without it the edited row's `[buffer]` echo stayed frozen until
    /// Enter/Esc because `entries` carries the committed value, not the buffer.
    pub(super) editing_buffer: Option<String>,
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
    /// is sequenced in the font picker; the variant is wired here.
    OpenFontPicker,
    /// Save all pending changes and close the overlay.
    SaveAndClose(Vec<SettingEdit>),
    /// Discard all pending changes and close the overlay.
    DiscardAndClose,
    /// Open a project URL from the About view (ABOUT). The host opens it through
    /// the same allowlisted opener the bare-URL/OSC 8 paths use.
    OpenUrl(String),
    /// Copy the About diagnostics block to the clipboard (ABOUT). The host
    /// writes it via the native clipboard.
    CopyToClipboard(String),
    /// Close the overlay (emitted at Level 1 with no pending edits / dirty
    /// prompt already shown).
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowEdit {
    key: &'static str,
    buffer: String,
    /// Replace the pre-filled current value on the first typed character.
    /// The buffer opens holding the current value as a hint; the first
    /// keystroke (or a Backspace) clears it, giving a select-all-on-focus
    /// feel. Without this, typed characters append to the pre-filled value
    /// and a re-typed numeric value concatenates and clamps to the range max.
    replace_on_char: bool,
}

impl RowEdit {
    fn for_entry(entry: &SettingInfo) -> Self {
        Self {
            key: entry.key,
            buffer: entry.value.clone(),
            replace_on_char: true,
        }
    }
}

fn stepped_tab_bar_height(value: &str, direction: isize) -> String {
    let min = MIN_TAB_BAR_ROWS as u16;
    let max = MAX_TAB_BAR_ROWS as u16;
    let manual = value
        .trim()
        .parse::<u16>()
        .ok()
        .map(|rows| rows.clamp(min, max));
    let next = match (manual, direction.cmp(&0)) {
        (None, std::cmp::Ordering::Greater) => TabBarHeight::Manual(min),
        (None, _) => TabBarHeight::Auto,
        (Some(rows), std::cmp::Ordering::Less) if rows <= min => TabBarHeight::Auto,
        (Some(rows), std::cmp::Ordering::Less) => TabBarHeight::Manual(rows - 1),
        (Some(rows), std::cmp::Ordering::Greater) => {
            TabBarHeight::Manual(rows.saturating_add(1).min(max))
        }
        (Some(rows), std::cmp::Ordering::Equal) => TabBarHeight::Manual(rows),
    };
    next.as_config_string()
}

impl SettingsPanel {
    pub(super) fn new(settings: &Settings) -> Self {
        let edits = SettingsEditOverlay::new(settings);
        let entries = edits.settings().setting_info();
        let mut panel = Self {
            all_entries: entries.clone(),
            entries,
            level: SettingsLevel::SectionList,
            about: None,
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
                    && matches!(hit.zone, RowZone::Value { .. } | RowZone::Stepper { .. })
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
            SettingsLevel::About => "\u{2190} About  (Esc = back)".to_owned(),
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
            SettingsLevel::About => self.handle_about_input(input),
        }
    }

    /// Populate the read-only About data (called when the overlay opens). Cheap
    /// to recompute; held so the About view renders without per-frame work.
    pub(super) fn set_about(&mut self, about: AboutInfo) {
        self.about = Some(about);
    }

    // ── Level 2 (ABOUT): read-only About view dispatch ─────────────────────

    fn handle_about_input(&mut self, input: OverlayInput) -> SettingsPanelOutcome {
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
    fn activate_about_row(&mut self) -> SettingsPanelOutcome {
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
    pub(super) fn open_section(&mut self, name: &str) {
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
    pub(super) fn displayed_value_for_test(&self, key: &str) -> Option<String> {
        self.all_entries
            .iter()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.clone())
    }

    #[cfg(test)]
    pub(super) fn active_section_name_for_test(&self) -> Option<&'static str> {
        match self.level {
            SettingsLevel::SectionDetail { section_index } => {
                SECTIONS.get(section_index).map(|section| section.name)
            }
            SettingsLevel::SectionList | SettingsLevel::About => None,
        }
    }

    fn move_section_selection(&mut self, delta: isize) {
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
                self.editing = Some(RowEdit::for_entry(&entry));
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
                    if edit.replace_on_char {
                        edit.buffer.clear();
                        edit.replace_on_char = false;
                    } else {
                        edit.buffer.pop();
                    }
                }
                SettingsPanelOutcome::Consumed
            }
            OverlayInput::Char(ch) if !ch.is_control() => {
                if let Some(edit) = self.editing.as_mut() {
                    if edit.replace_on_char {
                        edit.buffer.clear();
                        edit.replace_on_char = false;
                    }
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
        if entry.key == "tab_bar_height" {
            let next = stepped_tab_bar_height(&entry.value, direction);
            return self.commit_value(entry.key, &next);
        }
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

/// SETTINGS-COMPACT: how many body rows the fixed help footer reserves at the
/// panel bottom — a divider plus the focused row's wrapped help. Kept a pure
/// function of the body height so the scrolling content window is a constant
/// size and never reflows as focus moves between rows with differing help
/// lengths. Returns 0 on windows too short to spare the rows, which collapses
/// the body back to its full-height (pre-compact) form.
pub(super) fn settings_detail_footer_reserve(body_height: usize) -> usize {
    const DIVIDER_ROWS: usize = 1;
    const MAX_HELP_ROWS: usize = 4;
    if body_height < 6 {
        return 0;
    }
    (DIVIDER_ROWS + MAX_HELP_ROWS).min(body_height / 2)
}

fn matches_query(entry: &SettingInfo, needle: &str) -> bool {
    entry.name.to_lowercase().contains(needle)
        || entry.key.to_lowercase().contains(needle)
        || entry.description.to_lowercase().contains(needle)
        || entry.group.to_lowercase().contains(needle)
}

fn edit_options(entry: &SettingInfo) -> Vec<&'static str> {
    match entry.key {
        "theme" => vec!["plain", "odyssey-default", "odyssey", "odyssey-noir"],
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
mod tests;

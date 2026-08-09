// SPDX-License-Identifier: GPL-3.0-only
use crate::settings::{
    DEFAULT_CELL_BG_OPACITY, MAX_TAB_BAR_ROWS, MIN_TAB_BAR_ROWS, SettingEdit, SettingInfo,
    SettingKind, Settings, SettingsEditOverlay, TabBarHeight,
};

use super::about::{ABOUT_LINKS, AboutInfo};
use super::overlay::OverlayInput;

mod coordination;
mod editing;
mod input;
mod navigation;
mod path_picker;
mod pointer;
mod rendering;
mod sections;

use path_picker::{PathPickerOutcome, PathPickerSignature, PathPickerState, resolve_start_dir};
use rendering::{
    edit_options, ellipsize, matches_query, setting_detail, settings_detail_footer_reserve,
    wrap_words,
};
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

#[cfg(test)]
mod tests;

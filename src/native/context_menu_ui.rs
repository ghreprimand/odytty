// SPDX-License-Identifier: GPL-3.0-only
//! Right-click context menu (IN2). A small, cell-rendered popup offering Copy /
//! Cut / Paste / Delete / Select All / New Tab / Close Tab / Settings, spawned
//! at the pointer cell and edge-clamped to the grid.
//!
//! The menu is *always available* — there is no `context_menu` enable bool. The
//! TUI mouse-reporting passthrough guard in [`super::app`] is the effective off
//! switch: inside a TUI that requests mouse reporting, a right-click is reported
//! to the PTY and the menu never opens (the report gate returns before the
//! menu-open step). Shift+right-click bypasses that gate — exactly as Shift+drag
//! overrides reporting for local selection — so the menu is still reachable in a
//! TUI when the user explicitly asks for it (D-IN2-1, D-IN2-3).
//!
//! Like every other [`super::overlay::OverlayMode`], the menu never blocks the
//! PTY: the terminal stays live behind it. It is off-path-identical when no
//! right-click has occurred — the `ContextMenu` mode is simply never entered, so
//! the default render path is byte-for-byte unchanged.
//!
//! Item gating (D-IN2-6): Copy is enabled only when a selection exists, Cut and
//! Delete only when that selection intersects editable prompt input, Paste only
//! when the clipboard holds text, and Rename Tab only when the menu was opened
//! from a specific tab; states are snapshotted at open time. Select All, New
//! Tab, Close Tab, and Settings are always enabled. A disabled item renders dim
//! and its activation is a no-op.
//!
//! Three visual separator lines partition the menu into editing/selection
//! commands (Copy…Select All), tab actions (New Tab / Rename Tab / Close Tab),
//! split actions (Split Right / Split Down), and the Settings launcher.
//! Separators occupy body rows but are neither selectable nor focusable
//! (D-IN2-SETTINGS).
//!
//! Each selectable item renders its *effective* keybind (reverse action→chord
//! lookup against the live `KeyBindings`, so it tracks user rebinds) right-
//! aligned beside its label; items with no bound chord render no accelerator.
//! The App computes the accelerator strings at open time (it owns the
//! `KeyBindings`) and threads them in via [`ContextMenuUi::set_accelerators`].

use super::overlay::{OverlayInput, OverlayRect, PointerButton};
use super::session::SessionToken;
use crate::selection::CellPoint;
use crate::settings::BindableAction;

/// Number of selectable items (Copy / Cut / Paste / Delete / Select All / New
/// Tab / Rename Tab / Close Tab / Split Right / Split Down / Settings).
pub(super) const CONTEXT_MENU_ITEMS: usize = 11;

/// Body row index of the first visual separator, between Select All and New Tab.
pub(super) const CONTEXT_MENU_SEPARATOR_ROW: usize = 5;

/// Body row index of the second visual separator, between Close Tab and the
/// split actions.
pub(super) const CONTEXT_MENU_SECOND_SEPARATOR_ROW: usize = 9;

/// Body row index of the third visual separator, between the split actions and
/// Settings.
pub(super) const CONTEXT_MENU_THIRD_SEPARATOR_ROW: usize = 12;

/// Total body rows: eleven selectable items plus three separator lines.
pub(super) const CONTEXT_MENU_BODY_ROWS: usize = CONTEXT_MENU_ITEMS + 3;

/// Minimum gap (in cells) between the longest label and the right-aligned
/// accelerator column, so labels and accelerators never abut (Part C).
pub(super) const ACCELERATOR_GAP: usize = 2;

/// The selectable actions in the menu, in display order (separator excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextMenuItem {
    Copy,
    Cut,
    Paste,
    Delete,
    SelectAll,
    NewTab,
    RenameTab,
    CloseTab,
    /// Split the focused pane into side-by-side columns (new pane right). Same
    /// action as the keyboard `Ctrl+Shift+E` / tmux `Ctrl-b %` path.
    SplitColumns,
    /// Split the focused pane into stacked rows (new pane below). Same action as
    /// the keyboard `Ctrl+Shift+O` / tmux `Ctrl-b "` path.
    SplitRows,
    /// Open the settings panel (always enabled, D-IN2-SETTINGS).
    Settings,
}

impl ContextMenuItem {
    /// Selectable items in display order; index maps to `focused` state.
    pub(super) const ALL: [ContextMenuItem; CONTEXT_MENU_ITEMS] = [
        Self::Copy,
        Self::Cut,
        Self::Paste,
        Self::Delete,
        Self::SelectAll,
        Self::NewTab,
        Self::RenameTab,
        Self::CloseTab,
        Self::SplitColumns,
        Self::SplitRows,
        Self::Settings,
    ];

    /// The label painted for this item.
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Copy => "Copy",
            Self::Cut => "Cut",
            Self::Paste => "Paste",
            Self::Delete => "Delete",
            Self::SelectAll => "Select All",
            Self::NewTab => "New Tab",
            Self::RenameTab => "Rename Tab",
            Self::CloseTab => "Close Tab",
            Self::SplitColumns => "Split Right",
            Self::SplitRows => "Split Down",
            Self::Settings => "Settings",
        }
    }

    /// The [`BindableAction`] whose effective chord is shown as this item's
    /// accelerator (Part C), or `None` for items with no keyboard binding (Cut /
    /// Delete / Select All / Rename Tab). Copy/Paste/New Tab/Close Tab/Settings
    /// map onto their existing global actions; the splits map onto the direct
    /// split chords added to the global table.
    pub(super) fn bindable_action(self) -> Option<BindableAction> {
        match self {
            Self::Copy => Some(BindableAction::Copy),
            Self::Paste => Some(BindableAction::Paste),
            Self::NewTab => Some(BindableAction::NewTab),
            Self::CloseTab => Some(BindableAction::CloseTab),
            Self::SplitColumns => Some(BindableAction::SplitColumns),
            Self::SplitRows => Some(BindableAction::SplitRows),
            Self::Settings => Some(BindableAction::SettingsPanel),
            Self::Cut | Self::Delete | Self::SelectAll | Self::RenameTab => None,
        }
    }
}

/// Map a selectable item index to its body row, accounting for the three
/// separators. Items 0–4 sit at body rows 0–4; items 5–7 (tab actions) sit at
/// body rows 6–8; items 8–9 (splits) sit at body rows 10–11; Settings (index
/// 10) sits at body row 13.
#[cfg(test)]
fn item_to_body_row(item_index: usize) -> usize {
    if item_index >= 10 {
        item_index + 3
    } else if item_index >= 8 {
        item_index + 2
    } else if item_index >= CONTEXT_MENU_SEPARATOR_ROW {
        item_index + 1
    } else {
        item_index
    }
}

/// Map a body row to a selectable item index, or `None` for a separator row.
fn body_row_to_item(body_row: usize) -> Option<usize> {
    if body_row == CONTEXT_MENU_SEPARATOR_ROW
        || body_row == CONTEXT_MENU_SECOND_SEPARATOR_ROW
        || body_row == CONTEXT_MENU_THIRD_SEPARATOR_ROW
    {
        None
    } else if body_row > CONTEXT_MENU_THIRD_SEPARATOR_ROW {
        Some(body_row - 3)
    } else if body_row > CONTEXT_MENU_SECOND_SEPARATOR_ROW {
        Some(body_row - 2)
    } else if body_row > CONTEXT_MENU_SEPARATOR_ROW {
        Some(body_row - 1)
    } else {
        Some(body_row)
    }
}

/// What an input/pointer event did to the menu. The overlay lifts this into an
/// [`super::overlay::OverlayOutcome`] (closing the menu and emitting the matching
/// App-side action for `Activate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextMenuOutcome {
    /// Event handled, menu stays open (focus move, hover, disabled-item click).
    Consumed,
    /// Dismiss the menu (Esc / click-outside).
    Close,
    /// Run this item's action, then close the menu.
    Activate(ContextMenuItem),
}

/// One rendered body row: either an item (with label/accelerator/focus/enabled
/// state) or a visual separator. `accelerator` is the human-readable effective
/// keybind shown right-aligned beside the label (`None` when the item has no
/// bound chord). Owns its accelerator string, so the variant is not `Copy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ContextMenuRow {
    Item {
        label: &'static str,
        accelerator: Option<String>,
        focused: bool,
        enabled: bool,
    },
    Separator,
}

/// Render-cache signature for the menu: the raw spawn cell, the focused row, and
/// the per-item enabled state (which drives the dim/normal attrs). The clamp to
/// the grid is deterministic from `spawn` + grid size, so the raw spawn fully
/// describes the render at a given grid size. `Default` (closed, nothing
/// focused, all disabled) backs the test fixtures' closed-overlay signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct ContextMenuSignature {
    /// Raw (pre-clamp) spawn cell as `(row, column)`.
    pub(super) spawn: (usize, usize),
    /// Focused item index (0-based, index into `ContextMenuItem::ALL`).
    pub(super) focused: u8,
    pub(super) copy_enabled: bool,
    pub(super) cut_enabled: bool,
    pub(super) paste_enabled: bool,
    pub(super) delete_enabled: bool,
    pub(super) rename_enabled: bool,
}

/// The right-click context menu state. Holds the spawn cell, the focused item,
/// and the snapshot of which items are enabled. No scroll state — the items
/// always fit.
#[derive(Debug, Clone)]
pub(super) struct ContextMenuUi {
    spawn: CellPoint,
    focused: usize,
    copy_enabled: bool,
    cut_enabled: bool,
    paste_enabled: bool,
    delete_enabled: bool,
    rename_target: Option<SessionToken>,
    /// Per-item effective-keybind labels (Part C), indexed by
    /// [`ContextMenuItem::ALL`] order. `None` means the item shows no
    /// accelerator. Reset to all-`None` on `open`; the App overwrites via
    /// [`Self::set_accelerators`] from the live `KeyBindings`.
    accelerators: [Option<String>; CONTEXT_MENU_ITEMS],
}

impl Default for ContextMenuUi {
    fn default() -> Self {
        Self {
            spawn: CellPoint { row: 0, column: 0 },
            focused: 0,
            copy_enabled: false,
            cut_enabled: false,
            paste_enabled: false,
            delete_enabled: false,
            rename_target: None,
            accelerators: Default::default(),
        }
    }
}

impl ContextMenuUi {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Arm the menu at `spawn` with the given item-enabled snapshot, resetting
    /// the focus to the first item. The caller (the App) computes the enabled
    /// flags from the live selection / clipboard before opening.
    pub(super) fn open(
        &mut self,
        spawn: CellPoint,
        copy_enabled: bool,
        cut_enabled: bool,
        paste_enabled: bool,
        delete_enabled: bool,
        rename_target: Option<SessionToken>,
    ) {
        self.spawn = spawn;
        self.copy_enabled = copy_enabled;
        self.cut_enabled = cut_enabled;
        self.paste_enabled = paste_enabled;
        self.delete_enabled = delete_enabled;
        self.rename_target = rename_target;
        self.focused = 0;
        // Clear any stale accelerators; the App repopulates immediately via
        // `set_accelerators`. A bare `open` (the unit-test path) shows no
        // accelerators, which is the label-only legacy layout.
        self.accelerators = Default::default();
    }

    /// Set the per-item effective-keybind labels (Part C), in
    /// [`ContextMenuItem::ALL`] order. Called by the App right after `open`,
    /// since the App owns the live `KeyBindings`. Keeping the lookup App-side
    /// leaves this menu a pure presentation struct.
    pub(super) fn set_accelerators(&mut self, accelerators: [Option<String>; CONTEXT_MENU_ITEMS]) {
        self.accelerators = accelerators;
    }

    pub(super) fn rename_target(&self) -> Option<SessionToken> {
        self.rename_target
    }

    fn item_enabled(&self, item: ContextMenuItem) -> bool {
        match item {
            ContextMenuItem::Copy => self.copy_enabled,
            ContextMenuItem::Cut => self.cut_enabled,
            ContextMenuItem::Paste => self.paste_enabled,
            ContextMenuItem::Delete => self.delete_enabled,
            ContextMenuItem::SelectAll => true,
            ContextMenuItem::NewTab => true,
            ContextMenuItem::RenameTab => self.rename_target.is_some(),
            ContextMenuItem::CloseTab => true,
            ContextMenuItem::SplitColumns => true,
            ContextMenuItem::SplitRows => true,
            ContextMenuItem::Settings => true,
        }
    }

    /// The accelerator label for a selectable item index, or `None` when the
    /// item has no bound chord.
    fn accelerator_for(&self, item_index: usize) -> Option<&str> {
        self.accelerators
            .get(item_index)
            .and_then(|slot| slot.as_deref())
    }

    /// Menu width in cells: the longest label, plus (when any item has an
    /// accelerator) a two-column gap and the longest accelerator, plus a border
    /// and one pad column on each side. Falls back to label-only width when no
    /// accelerators are set (the unit-test / legacy layout).
    pub(super) fn menu_width(&self) -> usize {
        let longest_label = ContextMenuItem::ALL
            .iter()
            .map(|item| item.label().chars().count())
            .max()
            .unwrap_or(0);
        let longest_accel = self
            .accelerators
            .iter()
            .filter_map(|slot| slot.as_deref())
            .map(|accel| accel.chars().count())
            .max()
            .unwrap_or(0);
        // Two-column gap between the longest label and the accelerator column so
        // the right-aligned accelerators never touch the labels.
        let content = if longest_accel > 0 {
            longest_label + ACCELERATOR_GAP + longest_accel
        } else {
            longest_label
        };
        content + 4
    }

    /// The menu's cell geometry for a `columns`×`rows` grid: a fixed-size box
    /// whose top-left tracks the (clamped) spawn cell so it always fits on
    /// screen. No title row (D-IN2-10): the body starts one row below the top
    /// border. Shares [`OverlayRect`] with the centered overlays so the App's
    /// pointer routing and click-outside dismissal work unchanged.
    pub(super) fn rect(&self, columns: usize, rows: usize) -> OverlayRect {
        let width = self.menu_width().min(columns.max(1));
        let height = (CONTEXT_MENU_BODY_ROWS + 2).min(rows.max(1));
        let left = self.spawn.column.min(columns.saturating_sub(width));
        let top = self.spawn.row.min(rows.saturating_sub(height));
        OverlayRect {
            left,
            top,
            width,
            height,
            body_left: left + 2,
            body_top: top + 1,
            body_width: width.saturating_sub(4),
            body_height: CONTEXT_MENU_BODY_ROWS,
        }
    }

    fn focus_prev(&mut self) {
        self.focused = (self.focused + CONTEXT_MENU_ITEMS - 1) % CONTEXT_MENU_ITEMS;
    }

    fn focus_next(&mut self) {
        self.focused = (self.focused + 1) % CONTEXT_MENU_ITEMS;
    }

    fn activate_focused(&self) -> ContextMenuOutcome {
        let item = ContextMenuItem::ALL[self.focused];
        if self.item_enabled(item) {
            ContextMenuOutcome::Activate(item)
        } else {
            // A disabled focused item swallows the activation (D-IN2-6).
            ContextMenuOutcome::Consumed
        }
    }

    /// Handle a keyboard event: Esc closes; Up/Down cycle focus with wrap
    /// (skipping the separator — focus cycles only through selectable items);
    /// Enter/Space activate the focused item; everything else is swallowed so
    /// nothing leaks to the PTY behind the menu (D-IN2-8).
    pub(super) fn handle_input(&mut self, input: OverlayInput) -> ContextMenuOutcome {
        match input {
            OverlayInput::Close => ContextMenuOutcome::Close,
            OverlayInput::Up => {
                self.focus_prev();
                ContextMenuOutcome::Consumed
            }
            OverlayInput::Down => {
                self.focus_next();
                ContextMenuOutcome::Consumed
            }
            OverlayInput::Activate | OverlayInput::Char(' ') => self.activate_focused(),
            _ => ContextMenuOutcome::Consumed,
        }
    }

    /// Handle a press on a body row (already resolved to a body-relative row by
    /// the overlay). Activation happens on PRESS. A press on the separator row
    /// or a row past the last body row is inert. The pressed item also takes
    /// focus. Disabled items swallow the press (D-IN2-6).
    pub(super) fn handle_press(
        &mut self,
        row_in_body: usize,
        _button: PointerButton,
    ) -> ContextMenuOutcome {
        if row_in_body >= CONTEXT_MENU_BODY_ROWS {
            return ContextMenuOutcome::Consumed;
        }
        let Some(item_index) = body_row_to_item(row_in_body) else {
            // Separator row: inert.
            return ContextMenuOutcome::Consumed;
        };
        if item_index >= CONTEXT_MENU_ITEMS {
            return ContextMenuOutcome::Consumed;
        }
        self.focused = item_index;
        let item = ContextMenuItem::ALL[item_index];
        if self.item_enabled(item) {
            ContextMenuOutcome::Activate(item)
        } else {
            ContextMenuOutcome::Consumed
        }
    }

    /// Move focus to the item under a hovering pointer (D-IN2-6). `row_in_body`
    /// is `None` when the pointer is on the border / off a body row, leaving
    /// focus unchanged. The separator row is skipped (hover over it leaves focus
    /// on its last position).
    pub(super) fn handle_hover(&mut self, row_in_body: Option<usize>) {
        if let Some(row) = row_in_body
            && let Some(item_index) = body_row_to_item(row)
            && item_index < CONTEXT_MENU_ITEMS
        {
            self.focused = item_index;
        }
    }

    /// The rendered body rows in display order. Each entry is either an
    /// [`ContextMenuRow::Item`] (with label, focus, and enabled state) or
    /// [`ContextMenuRow::Separator`] (the visual divider). The renderer decides
    /// how to paint each row type.
    pub(super) fn rows(&self) -> Vec<ContextMenuRow> {
        let mut out = Vec::with_capacity(CONTEXT_MENU_BODY_ROWS);
        for (item_index, item) in ContextMenuItem::ALL.iter().enumerate() {
            // Insert a separator before each new section: the tab actions
            // (index 5), the split actions (index 8), and Settings (index 10).
            if item_index == 5 || item_index == 8 || item_index == 10 {
                out.push(ContextMenuRow::Separator);
            }
            out.push(ContextMenuRow::Item {
                label: item.label(),
                accelerator: self.accelerator_for(item_index).map(str::to_owned),
                focused: item_index == self.focused,
                enabled: self.item_enabled(*item),
            });
        }
        out
    }

    pub(super) fn render_signature(&self) -> ContextMenuSignature {
        ContextMenuSignature {
            spawn: (self.spawn.row, self.spawn.column),
            focused: self.focused as u8,
            copy_enabled: self.copy_enabled,
            cut_enabled: self.cut_enabled,
            paste_enabled: self.paste_enabled,
            delete_enabled: self.delete_enabled,
            rename_enabled: self.rename_target.is_some(),
        }
    }
}

/// Human-readable accelerator label from the canonical config-token chord
/// string produced by [`crate::settings::format_key_chord`] (Part C). Reuses
/// that formatter for the modifier/key decomposition (no duplication) and only
/// title-cases each `+`-separated token: `ctrl+shift+e` → `Ctrl+Shift+E`.
pub(super) fn humanize_chord(token: String) -> String {
    token
        .split('+')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(copy: bool, paste: bool) -> ContextMenuUi {
        let mut m = ContextMenuUi::new();
        m.open(
            CellPoint { row: 4, column: 7 },
            copy,
            copy,
            paste,
            copy,
            None,
        );
        m
    }

    #[test]
    fn rect_clamps_to_fit_grid() {
        let mut m = ContextMenuUi::new();
        // Spawn far past the right/bottom edge; the box must shift to fit.
        m.open(
            CellPoint {
                row: 100,
                column: 100,
            },
            true,
            true,
            true,
            true,
            None,
        );
        let rect = m.rect(40, 20);
        assert!(rect.left + rect.width <= 40);
        assert!(rect.top + rect.height <= 20);
    }

    #[test]
    fn rect_tracks_spawn_when_it_fits() {
        let m = menu(true, true);
        let rect = m.rect(80, 24);
        assert_eq!(rect.left, 7);
        assert_eq!(rect.top, 4);
        assert_eq!(rect.body_top, 5);
        assert_eq!(rect.body_height, CONTEXT_MENU_BODY_ROWS);
    }

    #[test]
    fn focus_cycles_with_wrap() {
        let mut m = menu(true, true);
        assert_eq!(m.focused, 0);
        m.handle_input(OverlayInput::Up);
        // Wraps from 0 to the last item (Settings, the last index).
        assert_eq!(m.focused, CONTEXT_MENU_ITEMS - 1);
        m.handle_input(OverlayInput::Down);
        assert_eq!(m.focused, 0);
        m.handle_input(OverlayInput::Down);
        assert_eq!(m.focused, 1);
    }

    #[test]
    fn copy_disabled_swallows_activation() {
        let mut m = menu(false, true);
        // Focus Copy (index 0, body row 0) and press it — disabled, so no Activate.
        assert_eq!(
            m.handle_press(0, PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
        // Focus + activate via keyboard is also a no-op.
        assert_eq!(
            m.handle_input(OverlayInput::Activate),
            ContextMenuOutcome::Consumed
        );
    }

    #[test]
    fn paste_disabled_swallows_activation() {
        let mut m = menu(true, false);
        // Paste is at item index 2 → body row 2.
        assert_eq!(
            m.handle_press(2, PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
    }

    #[test]
    fn select_all_always_activates() {
        let mut m = menu(false, false);
        // SelectAll is at item index 4 → body row 4.
        assert_eq!(
            m.handle_press(4, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::SelectAll)
        );
    }

    #[test]
    fn new_tab_rename_tab_and_close_tab_gating() {
        let mut m = menu(false, false);
        assert_eq!(item_to_body_row(5), 6, "New Tab item is at body row 6");
        assert_eq!(
            m.handle_press(6, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::NewTab)
        );
        assert_eq!(item_to_body_row(6), 7, "Rename Tab item is at body row 7");
        assert_eq!(
            m.handle_press(7, PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
        m.open(
            CellPoint { row: 4, column: 7 },
            false,
            false,
            false,
            false,
            Some(SessionToken(9)),
        );
        assert_eq!(
            m.handle_press(7, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::RenameTab)
        );
        assert_eq!(m.rename_target(), Some(SessionToken(9)));
        assert_eq!(item_to_body_row(7), 8, "Close Tab item is at body row 8");
        assert_eq!(
            m.handle_press(8, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::CloseTab)
        );
    }

    #[test]
    fn split_items_always_activate() {
        let mut m = menu(false, false);
        // SplitColumns is item index 8 → body row 10; SplitRows index 9 → row 11.
        assert_eq!(
            item_to_body_row(8),
            10,
            "Split Right item is at body row 10"
        );
        assert_eq!(
            m.handle_press(10, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::SplitColumns)
        );
        assert_eq!(item_to_body_row(9), 11, "Split Down item is at body row 11");
        assert_eq!(
            m.handle_press(11, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::SplitRows)
        );
    }

    #[test]
    fn settings_always_activates() {
        let mut m = menu(false, false);
        // Settings is at item index 10 → body row 13.
        assert_eq!(item_to_body_row(10), 13, "Settings item is at body row 13");
        assert_eq!(
            m.handle_press(13, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::Settings)
        );
    }

    #[test]
    fn separator_row_is_inert() {
        let mut m = menu(true, true);
        let before = m.focused;
        for sep in [
            CONTEXT_MENU_SEPARATOR_ROW,
            CONTEXT_MENU_SECOND_SEPARATOR_ROW,
            CONTEXT_MENU_THIRD_SEPARATOR_ROW,
        ] {
            assert_eq!(
                m.handle_press(sep, PointerButton::Left),
                ContextMenuOutcome::Consumed
            );
            assert_eq!(m.focused, before, "separator press does not move focus");
        }
    }

    #[test]
    fn hover_skips_separator() {
        let mut m = menu(true, true);
        m.handle_hover(Some(2)); // Paste
        assert_eq!(m.focused, 2);
        // Hovering each separator leaves focus unchanged.
        for sep in [
            CONTEXT_MENU_SEPARATOR_ROW,
            CONTEXT_MENU_SECOND_SEPARATOR_ROW,
            CONTEXT_MENU_THIRD_SEPARATOR_ROW,
        ] {
            m.handle_hover(Some(sep));
            assert_eq!(m.focused, 2, "separator hover is inert");
        }
        // Hovering Settings (body row 13) focuses it (item index 10).
        m.handle_hover(Some(13));
        assert_eq!(m.focused, 10, "hover Settings focuses it");
    }

    #[test]
    fn enabled_items_activate_on_press() {
        let mut m = menu(true, true);
        assert_eq!(
            m.handle_press(0, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::Copy)
        );
        assert_eq!(
            m.handle_press(2, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::Paste)
        );
    }

    #[test]
    fn cut_and_delete_disabled_swallow_activation() {
        let mut m = ContextMenuUi::new();
        m.open(
            CellPoint { row: 4, column: 7 },
            true,
            false,
            true,
            false,
            None,
        );
        assert_eq!(
            m.handle_press(1, PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
        assert_eq!(
            m.handle_press(3, PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
    }

    #[test]
    fn press_on_border_row_is_inert() {
        let mut m = menu(true, true);
        // row_in_body == CONTEXT_MENU_BODY_ROWS is past the bottom border.
        assert_eq!(
            m.handle_press(CONTEXT_MENU_BODY_ROWS, PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
    }

    #[test]
    fn escape_closes() {
        let mut m = menu(true, true);
        assert_eq!(
            m.handle_input(OverlayInput::Close),
            ContextMenuOutcome::Close
        );
    }

    #[test]
    fn hover_moves_focus_only_over_items() {
        let mut m = menu(true, true);
        m.handle_hover(Some(2));
        assert_eq!(m.focused, 2);
        // Off-item hover leaves focus unchanged.
        m.handle_hover(None);
        assert_eq!(m.focused, 2);
        // Row past all body rows is inert.
        m.handle_hover(Some(CONTEXT_MENU_BODY_ROWS + 5));
        assert_eq!(m.focused, 2);
    }

    #[test]
    fn signature_tracks_state() {
        let plain = menu(false, false).render_signature();
        let with_copy = menu(true, false).render_signature();
        assert_ne!(plain, with_copy);
        let mut m = menu(true, true);
        let before = m.render_signature();
        m.handle_input(OverlayInput::Down);
        assert_ne!(before, m.render_signature());
    }

    /// An `Item` row with the given label/focus/enabled and no accelerator (the
    /// bare-`open` / unit-test layout).
    fn item(label: &'static str, focused: bool, enabled: bool) -> ContextMenuRow {
        ContextMenuRow::Item {
            label,
            accelerator: None,
            focused,
            enabled,
        }
    }

    #[test]
    fn rows_report_label_focus_enabled() {
        let m = menu(true, false);
        let rows = m.rows();
        assert_eq!(rows.len(), CONTEXT_MENU_BODY_ROWS);
        assert_eq!(rows[0], item("Copy", true, true));
        assert_eq!(rows[1], item("Cut", false, true));
        assert_eq!(rows[2], item("Paste", false, false));
        assert_eq!(rows[3], item("Delete", false, true));
        assert_eq!(rows[4], item("Select All", false, true));
        assert_eq!(rows[5], ContextMenuRow::Separator);
        assert_eq!(rows[6], item("New Tab", false, true));
        assert_eq!(rows[7], item("Rename Tab", false, false));
        assert_eq!(rows[8], item("Close Tab", false, true));
        assert_eq!(rows[9], ContextMenuRow::Separator);
        assert_eq!(rows[10], item("Split Right", false, true));
        assert_eq!(rows[11], item("Split Down", false, true));
        assert_eq!(rows[12], ContextMenuRow::Separator);
        assert_eq!(rows[13], item("Settings", false, true));
    }

    #[test]
    fn accelerators_render_and_widen_the_menu() {
        let mut m = menu(true, true);
        let narrow = m.menu_width();
        // Populate a couple of accelerators in ALL order: Copy and Split Right.
        let mut accels: [Option<String>; CONTEXT_MENU_ITEMS] = Default::default();
        accels[0] = Some("Ctrl+Shift+C".to_owned());
        accels[8] = Some("Ctrl+Shift+E".to_owned());
        m.set_accelerators(accels);

        let rows = m.rows();
        assert_eq!(
            rows[0],
            ContextMenuRow::Item {
                label: "Copy",
                accelerator: Some("Ctrl+Shift+C".to_owned()),
                focused: true,
                enabled: true,
            }
        );
        // Split Right (item 8) sits at body row 10 and carries its accelerator.
        assert_eq!(
            rows[10],
            ContextMenuRow::Item {
                label: "Split Right",
                accelerator: Some("Ctrl+Shift+E".to_owned()),
                focused: false,
                enabled: true,
            }
        );
        // An item with no chord (Cut, item 1) renders a blank accelerator.
        assert_eq!(rows[1], item("Cut", false, true));
        // The menu grew to fit "longest label + gap + longest accelerator".
        assert!(
            m.menu_width() > narrow,
            "accelerators widen the menu: {} !> {narrow}",
            m.menu_width()
        );
    }

    #[test]
    fn humanize_chord_title_cases_tokens() {
        assert_eq!(humanize_chord("ctrl+shift+e".to_owned()), "Ctrl+Shift+E");
        assert_eq!(
            humanize_chord("ctrl+shift+comma".to_owned()),
            "Ctrl+Shift+Comma"
        );
        assert_eq!(humanize_chord("c".to_owned()), "C");
    }

    #[test]
    fn split_items_map_to_pane_actions() {
        assert_eq!(
            ContextMenuItem::SplitColumns.bindable_action(),
            Some(BindableAction::SplitColumns)
        );
        assert_eq!(
            ContextMenuItem::SplitRows.bindable_action(),
            Some(BindableAction::SplitRows)
        );
        // Items with no keyboard binding map to None.
        assert_eq!(ContextMenuItem::Cut.bindable_action(), None);
        assert_eq!(ContextMenuItem::SelectAll.bindable_action(), None);
    }

    #[test]
    fn body_row_mapping_is_consistent() {
        // Items 0-4 map 1:1 to body rows 0-4.
        for i in 0..CONTEXT_MENU_SEPARATOR_ROW {
            assert_eq!(item_to_body_row(i), i);
            assert_eq!(body_row_to_item(i), Some(i));
        }
        // The three separator rows map to no item.
        assert_eq!(body_row_to_item(CONTEXT_MENU_SEPARATOR_ROW), None);
        assert_eq!(body_row_to_item(CONTEXT_MENU_SECOND_SEPARATOR_ROW), None);
        assert_eq!(body_row_to_item(CONTEXT_MENU_THIRD_SEPARATOR_ROW), None);
        // Tab actions (5-7) shift past the first separator.
        assert_eq!(item_to_body_row(5), 6);
        assert_eq!(body_row_to_item(6), Some(5));
        assert_eq!(item_to_body_row(6), 7);
        assert_eq!(body_row_to_item(7), Some(6));
        assert_eq!(item_to_body_row(7), 8);
        assert_eq!(body_row_to_item(8), Some(7));
        // Split actions (8-9) shift past the first two separators.
        assert_eq!(item_to_body_row(8), 10);
        assert_eq!(body_row_to_item(10), Some(8));
        assert_eq!(item_to_body_row(9), 11);
        assert_eq!(body_row_to_item(11), Some(9));
        // Settings (10) shifts past all three separators.
        assert_eq!(item_to_body_row(10), 13);
        assert_eq!(body_row_to_item(13), Some(10));
    }
}

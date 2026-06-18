// SPDX-License-Identifier: GPL-3.0-only
//! Right-click context menu (IN2). A small, cell-rendered popup offering Copy /
//! Cut / Paste / Delete / Select All / Settings, spawned at the pointer cell and
//! edge-clamped to the grid.
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
//! when the clipboard holds text; states are snapshotted at open time. Select
//! All and Settings are always enabled. A disabled item renders dim and its
//! activation is a no-op.
//!
//! A visual separator line sits between the editing/selection commands (Copy…
//! Select All) and the Settings launcher. The separator occupies one body row
//! but is neither selectable nor focusable (D-IN2-SETTINGS).

use crate::selection::CellPoint;

use super::overlay::{OverlayInput, OverlayRect, PointerButton};

/// Number of selectable items (Copy / Cut / Paste / Delete / Select All /
/// Settings).
pub(super) const CONTEXT_MENU_ITEMS: usize = 6;

/// Body row index of the visual separator between Select All and Settings.
/// Rows 0–4 are the five edit/selection items; row 5 is the separator; row 6 is
/// the Settings item.
pub(super) const CONTEXT_MENU_SEPARATOR_ROW: usize = 5;

/// Total body rows: six selectable items plus one separator line.
pub(super) const CONTEXT_MENU_BODY_ROWS: usize = CONTEXT_MENU_ITEMS + 1;

/// The selectable actions in the menu, in display order (separator excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextMenuItem {
    Copy,
    Cut,
    Paste,
    Delete,
    SelectAll,
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
            Self::Settings => "Settings",
        }
    }
}

/// Map a selectable item index (0–5) to its body row, accounting for the
/// separator at row 5. Items 0–4 sit at body rows 0–4; Settings (index 5)
/// sits at body row 6.
#[cfg(test)]
fn item_to_body_row(item_index: usize) -> usize {
    if item_index >= CONTEXT_MENU_SEPARATOR_ROW {
        item_index + 1
    } else {
        item_index
    }
}

/// Map a body row to a selectable item index, or `None` for the separator row.
fn body_row_to_item(body_row: usize) -> Option<usize> {
    if body_row == CONTEXT_MENU_SEPARATOR_ROW {
        None
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

/// One rendered body row: either an item (with label/focus/enabled state) or the
/// visual separator between Select All and Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextMenuRow {
    Item {
        label: &'static str,
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
    ) {
        self.spawn = spawn;
        self.copy_enabled = copy_enabled;
        self.cut_enabled = cut_enabled;
        self.paste_enabled = paste_enabled;
        self.delete_enabled = delete_enabled;
        self.focused = 0;
    }

    fn item_enabled(&self, item: ContextMenuItem) -> bool {
        match item {
            ContextMenuItem::Copy => self.copy_enabled,
            ContextMenuItem::Cut => self.cut_enabled,
            ContextMenuItem::Paste => self.paste_enabled,
            ContextMenuItem::Delete => self.delete_enabled,
            ContextMenuItem::SelectAll => true,
            ContextMenuItem::Settings => true,
        }
    }

    /// Menu width in cells: the longest label plus a border + one pad column on
    /// each side.
    pub(super) fn menu_width(&self) -> usize {
        let longest = ContextMenuItem::ALL
            .iter()
            .map(|item| item.label().chars().count())
            .max()
            .unwrap_or(0);
        longest + 4
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
            // Insert the separator before the Settings item.
            if item_index == CONTEXT_MENU_SEPARATOR_ROW {
                out.push(ContextMenuRow::Separator);
            }
            out.push(ContextMenuRow::Item {
                label: item.label(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(copy: bool, paste: bool) -> ContextMenuUi {
        let mut m = ContextMenuUi::new();
        m.open(CellPoint { row: 4, column: 7 }, copy, copy, paste, copy);
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
        // Wraps from 0 to the last item (Settings, index 5).
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
    fn settings_always_activates() {
        let mut m = menu(false, false);
        // Settings is at item index 5 → body row 6 (separator at row 5).
        assert_eq!(item_to_body_row(5), 6, "Settings item is at body row 6");
        assert_eq!(
            m.handle_press(6, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::Settings)
        );
    }

    #[test]
    fn separator_row_is_inert() {
        let mut m = menu(true, true);
        // Body row 5 is the separator — press is consumed without changing focus.
        let before = m.focused;
        assert_eq!(
            m.handle_press(CONTEXT_MENU_SEPARATOR_ROW, PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
        assert_eq!(m.focused, before, "separator press does not move focus");
    }

    #[test]
    fn hover_skips_separator() {
        let mut m = menu(true, true);
        m.handle_hover(Some(2)); // Paste
        assert_eq!(m.focused, 2);
        // Hovering the separator leaves focus unchanged.
        m.handle_hover(Some(CONTEXT_MENU_SEPARATOR_ROW));
        assert_eq!(m.focused, 2, "separator hover is inert");
        // Hovering Settings (body row 6) focuses it (item index 5).
        m.handle_hover(Some(6));
        assert_eq!(m.focused, 5, "hover Settings focuses it");
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
        m.open(CellPoint { row: 4, column: 7 }, true, false, true, false);
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

    #[test]
    fn rows_report_label_focus_enabled() {
        let m = menu(true, false);
        let rows = m.rows();
        assert_eq!(rows.len(), CONTEXT_MENU_BODY_ROWS);
        assert_eq!(
            rows[0],
            ContextMenuRow::Item {
                label: "Copy",
                focused: true,
                enabled: true,
            }
        );
        assert_eq!(
            rows[1],
            ContextMenuRow::Item {
                label: "Cut",
                focused: false,
                enabled: true,
            }
        );
        assert_eq!(
            rows[2],
            ContextMenuRow::Item {
                label: "Paste",
                focused: false,
                enabled: false,
            }
        );
        assert_eq!(
            rows[3],
            ContextMenuRow::Item {
                label: "Delete",
                focused: false,
                enabled: true,
            }
        );
        assert_eq!(
            rows[4],
            ContextMenuRow::Item {
                label: "Select All",
                focused: false,
                enabled: true,
            }
        );
        assert_eq!(rows[5], ContextMenuRow::Separator);
        assert_eq!(
            rows[6],
            ContextMenuRow::Item {
                label: "Settings",
                focused: false,
                enabled: true,
            }
        );
    }

    #[test]
    fn body_row_mapping_is_consistent() {
        // Items 0-4 map 1:1 to body rows 0-4.
        for i in 0..CONTEXT_MENU_SEPARATOR_ROW {
            assert_eq!(item_to_body_row(i), i);
            assert_eq!(body_row_to_item(i), Some(i));
        }
        // Body row 5 is the separator.
        assert_eq!(body_row_to_item(CONTEXT_MENU_SEPARATOR_ROW), None);
        // Settings (item 5) is at body row 6.
        assert_eq!(item_to_body_row(5), 6);
        assert_eq!(body_row_to_item(6), Some(5));
    }
}

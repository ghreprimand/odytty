// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::native::overlay::OverlayRect;

impl ContextMenuUi {
    /// The items currently visible, in display order. Close Pane is included
    /// only in a multi-pane tab; everything else is always present. The visible
    /// list is the single source of truth for focus indices, separator
    /// placement, body-row mapping, and rendering, so the menu reflows cleanly
    /// when the pane count changes.
    pub(super) fn visible_items(&self) -> Vec<ContextMenuItem> {
        // F7: the tab surfaces carry a tight, tab-scoped composition with no
        // selection/split/launcher tail. They ignore the path/selection
        // snapshots entirely (a tab has no grid path under it).
        match self.surface {
            ContextMenuSurface::TabSlot(_) => {
                let mut items = vec![ContextMenuItem::NewTab];
                // F6-W5: when the workspace is bound to a host, New Tab opens a
                // remote tab, so offer a local-shell escape right beside it.
                if self.bound_workspace {
                    items.push(ContextMenuItem::NewLocalTab);
                }
                // Duplicate Tab rides beside New Tab: a fresh local shell in the
                // active pane's cwd.
                items.push(ContextMenuItem::DuplicateTab);
                items.extend([
                    ContextMenuItem::RenameTab,
                    ContextMenuItem::CloseTab,
                    ContextMenuItem::CloseOtherTabs,
                ]);
                // ODP-5D: a host is reachable straight from the tab strip. The
                // default opens a remote tab adjacent to the clicked one; the
                // consent-gated replace sits right under it.
                items.push(ContextMenuItem::ConnectToHost);
                items.push(ContextMenuItem::ReplaceTabWithHost);
                // ODP-7: the move item only appears when there is a destination
                // workspace. Single-workspace tab menus are unchanged.
                if self.multi_workspace {
                    items.push(ContextMenuItem::MoveToWorkspace);
                }
                items.push(ContextMenuItem::NewWindow);
                return items;
            }
            ContextMenuSurface::TabStripEmpty => {
                return vec![
                    ContextMenuItem::NewTab,
                    ContextMenuItem::NewWorkspace,
                    // LAYOUT-SURFACE: Open Layout is reachable from the empty strip.
                    ContextMenuItem::OpenLayout,
                    ContextMenuItem::CommandPalette,
                    ContextMenuItem::Settings,
                ];
            }
            // The workspace rail surfaces carry their own tight compositions
            // (§3.5): a slot offers New/Rename/Close Workspace; the empty rail
            // offers New Workspace only.
            ContextMenuSurface::WorkspaceSlot(idx) => {
                let mut items = vec![
                    ContextMenuItem::NewWorkspace,
                    // DUPLICATE-WORKSPACE rides beside New Workspace: a fresh
                    // workspace whose shell opens in the active pane's cwd.
                    ContextMenuItem::DuplicateWorkspace,
                    ContextMenuItem::RenameWorkspace,
                ];
                // RAIL-REORDER: the move rows gate on the clicked slot's
                // position -- Move Up hides on the first slot, Move Down on the
                // last (workspace_count is snapshotted at open by the App). With
                // a single workspace neither appears.
                if idx > 0 && idx < self.workspace_count {
                    items.push(ContextMenuItem::MoveWorkspaceUp);
                }
                if idx + 1 < self.workspace_count {
                    items.push(ContextMenuItem::MoveWorkspaceDown);
                }
                items.push(ContextMenuItem::CloseWorkspace);
                // RAIL-BIND: exactly one of the host bind/unbind pair, keyed to
                // whether the CLICKED workspace is bound (`bound_workspace` is set
                // to the clicked slot's state for this surface). Bind opens the
                // shared host picker targeting the slot; Unbind clears it.
                if self.bound_workspace {
                    items.push(ContextMenuItem::UnbindWorkspace);
                } else {
                    items.push(ContextMenuItem::BindWorkspaceToHost);
                }
                // LAYOUT-SURFACE + RAIL-SAVE-ALL: the whole-app save leads (a
                // layout means the whole session), then the single-workspace
                // save of the CLICKED slot — same order as the content menu.
                items.push(ContextMenuItem::SaveAllLayout);
                items.push(ContextMenuItem::SaveAsLayout);
                items.push(ContextMenuItem::Settings);
                return items;
            }
            ContextMenuSurface::WorkspaceRailEmpty => {
                // LAYOUT-SURFACE + SAVE-ALL-LAYOUT: the empty rail offers the
                // whole-app Save as Layout and Open Layout so saving/restoring a
                // full session is reachable from a bare rail.
                return vec![
                    ContextMenuItem::NewWorkspace,
                    ContextMenuItem::SaveAllLayout,
                    ContextMenuItem::OpenLayout,
                    ContextMenuItem::Settings,
                ];
            }
            // ODP-2C: a connection-manager row offers Open in New Tab / Open in
            // New Workspace / Bind Current Workspace always; Edit + Remove only
            // for OdyTTY-owned rows (an ssh-config-imported row is read-only, so
            // those two mutating items are hidden — never write ~/.ssh/config).
            ContextMenuSurface::ConnectionRow(_) => {
                let mut items = vec![
                    ContextMenuItem::ConnRowOpenInTab,
                    ContextMenuItem::ConnRowOpenInWorkspace,
                    ContextMenuItem::ConnRowBindWorkspace,
                ];
                if self.connection_is_odytty() {
                    items.push(ContextMenuItem::ConnRowEdit);
                    items.push(ContextMenuItem::ConnRowRemove);
                }
                return items;
            }
            // The provisional pane-divider surface is not constructed yet; fall
            // through to the Content composition defensively so an unexpected
            // open can never index an empty item list.
            ContextMenuSurface::Content | ContextMenuSurface::PaneDivider => {}
        }
        let has_path = self.path_target.is_some();
        let is_image = self.is_image_target();
        let is_file = self.is_file_target();
        if has_path {
            let mut items = vec![ContextMenuItem::OpenPath];
            if is_image {
                items.push(ContextMenuItem::OpenInOdytty);
            }
            if is_file {
                items.push(ContextMenuItem::OpenWith);
            }
            items.extend([
                ContextMenuItem::CopyPath,
                ContextMenuItem::CopyFile,
                ContextMenuItem::RevealPath,
                ContextMenuItem::Copy,
                ContextMenuItem::Paste,
            ]);
            return items;
        }
        // Content surface. `Copy` is hoisted to the very top when a selection
        // exists (ODP-8); `ContextMenuItem::ALL` keeps `Copy` in its historical
        // first slot, so the hoist is a no-op on ordering — the list already
        // leads with Copy — and this branch only needs to DROP the editing rows
        // that no longer apply.
        let has_selection = self.copy_enabled;
        ContextMenuItem::ALL
            .into_iter()
            .filter(|item| !matches!(item, ContextMenuItem::ClosePane) || self.multi_pane)
            // `Close Other Tabs` and `Move to Workspace` are tab-scoped:
            // never on content. New/Rename/Close Workspace ARE on content (their
            // own section after Split); Rename/Close target the active workspace.
            .filter(|item| {
                !matches!(
                    item,
                    ContextMenuItem::CloseOtherTabs
                        | ContextMenuItem::MoveToWorkspace
                        // RAIL-REORDER: the workspace move rows are
                        // WorkspaceSlot-only (they need a clicked slot); the
                        // content-grid workspace section acts on the active one.
                        | ContextMenuItem::MoveWorkspaceUp
                        | ContextMenuItem::MoveWorkspaceDown
                        // F6-W5: the New Local Tab escape is a bound-workspace
                        // tab-menu row only; never on the content surface.
                        | ContextMenuItem::NewLocalTab
                        // DUPLICATE-TAB: a tab-menu row only; never on content.
                        | ContextMenuItem::DuplicateTab
                        // DUPLICATE-WORKSPACE: a WorkspaceSlot-only row; never on
                        // content (the content workspace section acts on the
                        // active workspace, and New Workspace already covers it).
                        | ContextMenuItem::DuplicateWorkspace
                        // ODP-5D: the tab host actions are TabSlot-only.
                        | ContextMenuItem::ConnectToHost
                        | ContextMenuItem::ReplaceTabWithHost
                        // ODP-2C: the connection-row actions are ConnectionRow-only.
                        | ContextMenuItem::ConnRowOpenInTab
                        | ContextMenuItem::ConnRowOpenInWorkspace
                        | ContextMenuItem::ConnRowBindWorkspace
                        | ContextMenuItem::ConnRowEdit
                        | ContextMenuItem::ConnRowRemove
                )
            })
            // Drop the always-disabled `Rename Tab` row on the content surface —
            // it only has a target on a tab right-click (kept on `TabSlot`).
            .filter(|item| !matches!(item, ContextMenuItem::RenameTab))
            // ODP-6B: exactly one of the workspace host bind/unbind pair shows,
            // keyed to whether the active workspace is bound. Bind opens the
            // shared host picker; Unbind clears the binding.
            .filter(|item| {
                !matches!(item, ContextMenuItem::BindWorkspaceToHost) || !self.bound_workspace
            })
            .filter(|item| {
                !matches!(item, ContextMenuItem::UnbindWorkspace) || self.bound_workspace
            })
            // ODP-5: hide Copy/Cut/Delete entirely with no selection (cleaner
            // than rendering them dim); Paste/Select All stay the always-present
            // editing anchors.
            .filter(|item| {
                !matches!(
                    item,
                    ContextMenuItem::Copy | ContextMenuItem::Cut | ContextMenuItem::Delete
                ) || has_selection
            })
            .filter(|item| {
                !matches!(
                    item,
                    ContextMenuItem::OpenPath
                        | ContextMenuItem::CopyPath
                        | ContextMenuItem::CopyFile
                        | ContextMenuItem::RevealPath
                ) || has_path
            })
            // "Open in OdyTTY" (C4) appears only on a resolved IMAGE span, so a
            // non-image path (or no path) keeps the menu byte-identical to C3.
            .filter(|item| !matches!(item, ContextMenuItem::OpenInOdytty) || is_image)
            // "Open With…" (C3b) appears only on a resolved regular FILE span, so
            // a directory (or no path) does not show it.
            .filter(|item| !matches!(item, ContextMenuItem::OpenWith) || is_file)
            .collect()
    }

    /// The number of selectable (focusable) items currently visible.
    pub(super) fn item_count(&self) -> usize {
        self.visible_items().len()
    }

    /// The body rows in display order: `Some(item)` for a selectable row,
    /// `None` for a separator. A separator is inserted wherever consecutive
    /// visible items cross a section boundary ([`ContextMenuItem::section`]).
    pub(super) fn body_layout(&self) -> Vec<Option<ContextMenuItem>> {
        let mut out = Vec::new();
        let mut prev_section: Option<u8> = None;
        for item in self.visible_items() {
            let section = self.section_of(item);
            if prev_section.is_some_and(|p| p != section) {
                out.push(None);
            }
            prev_section = Some(section);
            out.push(Some(item));
        }
        out
    }

    /// The live body-row count (selectable items plus separators), accounting
    /// for the multi-pane Close Pane item.
    pub(super) fn body_row_count(&self) -> usize {
        self.body_layout().len()
    }

    /// Map a body row to its index in the visible item list, or `None` for a
    /// separator / out-of-range row. The multi-pane-aware production analogue of
    /// the single-pane `body_row_to_item` test reference.
    pub(super) fn body_row_to_item_index(&self, body_row: usize) -> Option<usize> {
        let layout = self.body_layout();
        let item = (*layout.get(body_row)?)?;
        self.visible_items().iter().position(|it| *it == item)
    }

    /// The accelerator label for an item, or `None` when the item has no bound
    /// chord. Looked up by the item's position in [`ContextMenuItem::ALL`] (the
    /// order the App fills the accelerator array), so it is stable regardless of
    /// which items are currently visible.
    pub(super) fn accelerator_for_item(&self, item: ContextMenuItem) -> Option<&str> {
        if self.prompt_editing_hint
            && !self.item_enabled(item)
            && matches!(item, ContextMenuItem::Cut | ContextMenuItem::Delete)
        {
            return Some(SHELL_INTEGRATION_DISABLED_HINT);
        }
        let all_index = ContextMenuItem::ALL.iter().position(|it| *it == item)?;
        self.accelerators
            .get(all_index)
            .and_then(|slot| slot.as_deref())
    }

    /// Menu width in cells: the longest label, plus (when any item has an
    /// accelerator) a two-column gap and the longest accelerator, plus a border
    /// and one pad column on each side. Falls back to label-only width when no
    /// accelerators are set (the unit-test / legacy layout).
    pub(in crate::native) fn menu_width(&self) -> usize {
        let longest_label = ContextMenuItem::ALL
            .iter()
            .map(|item| item.label().chars().count())
            .max()
            .unwrap_or(0);
        let longest_accel = self
            .accelerators
            .iter()
            .filter_map(|slot| slot.as_deref())
            .chain(
                self.prompt_editing_hint
                    .then_some(SHELL_INTEGRATION_DISABLED_HINT),
            )
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
    pub(in crate::native) fn rect(&self, columns: usize, rows: usize) -> OverlayRect {
        let body_rows = self.body_row_count();
        // MENU-Z-ORDER: constrain the box to the column span left of the reserved
        // rail band(s). `avail_left`..`avail_right` is the full grid when both
        // reserves are 0 (every non-rail open), so the default path is
        // byte-identical. A reserve wider than the grid degrades to the whole
        // grid rather than vanishing the menu.
        let avail_left = self.reserved_cols_left.min(columns.saturating_sub(1));
        let avail_right = columns
            .saturating_sub(self.reserved_cols_right)
            .max(avail_left + 1);
        let span = avail_right - avail_left;
        let width = self.menu_width().min(span.max(1));
        let height = (body_rows + 2).min(rows.max(1));
        let max_left = avail_right.saturating_sub(width).max(avail_left);
        let left = self.spawn.column.clamp(avail_left, max_left);
        let top = self.spawn.row.min(rows.saturating_sub(height));
        // Clamp the body window to the box interior (height minus the top and
        // bottom border rows). When the menu fits, `height == body_rows + 2` so
        // this equals `body_rows` and the layout is byte-identical to before
        // scrolling existed; only a too-short window produces a smaller window.
        let body_height = height.saturating_sub(2).min(body_rows);
        OverlayRect {
            left,
            top,
            width,
            height,
            body_left: left + 2,
            body_top: top + 1,
            body_width: width.saturating_sub(4),
            body_height,
        }
    }

    /// The body row of the currently focused item (separators are never
    /// focused), used to keep the focus inside the visible scroll window.
    pub(super) fn focused_body_row(&self) -> usize {
        let focused_item = self.visible_items()[self.focused];
        self.body_layout()
            .iter()
            .position(|row| *row == Some(focused_item))
            .unwrap_or(0)
    }

    /// The first body row to render, given the box-clamped `body_height`. When
    /// every row fits (`body_height >= body_row_count`) this is always `0` — the
    /// normal case, byte-identical to the pre-scroll layout. Otherwise it is the
    /// smallest offset that keeps the focused item inside the
    /// `[offset, offset + body_height)` window, clamped so the last row is
    /// reachable and the window never scrolls past the final body row.
    pub(in crate::native) fn scroll_offset(&self, body_height: usize) -> usize {
        let total = self.body_row_count();
        if body_height == 0 || total <= body_height {
            return 0;
        }
        let max_scroll = total - body_height;
        let focused_row = self.focused_body_row();
        let desired = focused_row.saturating_sub(body_height - 1);
        desired.min(max_scroll)
    }
}

// SPDX-License-Identifier: GPL-3.0-only
use super::*;

impl Default for ContextMenuUi {
    fn default() -> Self {
        Self {
            spawn: CellPoint { row: 0, column: 0 },
            focused: 0,
            copy_enabled: false,
            cut_enabled: false,
            paste_enabled: false,
            delete_enabled: false,
            command_actions_enabled: false,
            prompt_editing_hint: false,
            rename_target: None,
            multi_pane: false,
            multi_tab: false,
            multi_workspace: false,
            bound_workspace: false,
            workspace_count: 0,
            surface: ContextMenuSurface::Content,
            path_target: None,
            connection_target: None,
            // `[T; N]: Default` only exists up to N == 32; the item set is now
            // larger, so build the all-`None` array element-wise.
            accelerators: std::array::from_fn(|_| None),
            reserved_cols_left: 0,
            reserved_cols_right: 0,
        }
    }
}

impl ContextMenuUi {
    pub(in crate::native) fn new() -> Self {
        Self::default()
    }

    /// Arm the menu at `spawn` with the given item-enabled snapshot, resetting
    /// the focus to the first item. The caller (the App) computes the enabled
    /// flags from the live selection / clipboard before opening. The arg list
    /// mirrors that snapshot rather than wrapping it in a one-use struct.
    ///
    /// Retained for tests that call the pre-hint form; production opens via
    /// [`Self::open_with_prompt_editing_hint`] (this defaults the hint off).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn open(
        &mut self,
        spawn: CellPoint,
        copy_enabled: bool,
        cut_enabled: bool,
        paste_enabled: bool,
        delete_enabled: bool,
        rename_target: Option<SessionToken>,
        multi_pane: bool,
        path_target: Option<Resolved>,
    ) {
        self.open_with_prompt_editing_hint(
            spawn,
            copy_enabled,
            cut_enabled,
            paste_enabled,
            delete_enabled,
            false,
            rename_target,
            multi_pane,
            false,
            false,
            false,
            ContextMenuSurface::Content,
            path_target,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn open_with_prompt_editing_hint(
        &mut self,
        spawn: CellPoint,
        copy_enabled: bool,
        cut_enabled: bool,
        paste_enabled: bool,
        delete_enabled: bool,
        prompt_editing_hint: bool,
        rename_target: Option<SessionToken>,
        multi_pane: bool,
        multi_tab: bool,
        multi_workspace: bool,
        bound_workspace: bool,
        surface: ContextMenuSurface,
        path_target: Option<Resolved>,
    ) {
        self.spawn = spawn;
        self.copy_enabled = copy_enabled;
        self.cut_enabled = cut_enabled;
        self.paste_enabled = paste_enabled;
        self.delete_enabled = delete_enabled;
        self.command_actions_enabled = false;
        self.prompt_editing_hint = prompt_editing_hint;
        self.rename_target = rename_target;
        self.multi_pane = multi_pane;
        self.multi_tab = multi_tab;
        self.multi_workspace = multi_workspace;
        self.bound_workspace = bound_workspace;
        // RAIL-REORDER: reset to 0 on every open; the App sets the real count
        // via `set_workspace_count` only for a rail-slot menu.
        self.workspace_count = 0;
        self.surface = surface;
        self.path_target = path_target;
        // Cleared on every non-ConnectionRow open; the connection-row opener
        // sets it explicitly. Keeps the target from leaking across surfaces.
        self.connection_target = None;
        self.focused = 0;
        // Rail clearance is opt-in per open: the App re-applies it via
        // `set_rail_clearance` only for a rail-anchored menu under auto-hide.
        // Reset here so a stale reserve never leaks into an unrelated menu.
        self.reserved_cols_left = 0;
        self.reserved_cols_right = 0;
        // Clear any stale accelerators; the App repopulates immediately via
        // `set_accelerators`. A bare `open` (the unit-test path) shows no
        // accelerators, which is the label-only legacy layout.
        self.accelerators = std::array::from_fn(|_| None);
    }

    /// Arm the menu for a right-clicked connection-manager row (ODP-2C). Unlike
    /// the grid/tab opens this is spawned from WITHIN the connection overlay, so
    /// it takes only the spawn cell, the row's filtered-list index, and the
    /// clicked host — the selection/clipboard/pane snapshots are irrelevant to
    /// the five connection-row actions and are left at their inert defaults. The
    /// connection-row items carry no chord, so the accelerator array stays
    /// all-`None` (label-only rendering).
    pub(in crate::native) fn open_connection_row(
        &mut self,
        spawn: CellPoint,
        row_index: usize,
        host: ConnectionHost,
    ) {
        self.spawn = spawn;
        self.copy_enabled = false;
        self.cut_enabled = false;
        self.paste_enabled = false;
        self.delete_enabled = false;
        self.command_actions_enabled = false;
        self.prompt_editing_hint = false;
        self.rename_target = None;
        self.multi_pane = false;
        self.multi_tab = false;
        self.multi_workspace = false;
        self.bound_workspace = false;
        self.workspace_count = 0;
        self.surface = ContextMenuSurface::ConnectionRow(row_index);
        self.path_target = None;
        self.connection_target = Some(Box::new(host));
        self.focused = 0;
        // The connection-row menu spawns over the full-screen manager (the rail
        // is hidden while that overlay is open), so it needs no rail clearance.
        self.reserved_cols_left = 0;
        self.reserved_cols_right = 0;
        self.accelerators = std::array::from_fn(|_| None);
    }

    /// Reserve `left`/`right` columns the menu box must stay clear of
    /// (MENU-Z-ORDER). Applied by the App immediately after opening a
    /// rail-anchored menu while the auto-hide rail is revealed, so the box lands
    /// beside the floating rail band rather than under it. A no-op reserve
    /// (`0`, `0`) leaves the layout byte-identical.
    pub(in crate::native) fn set_rail_clearance(&mut self, left: usize, right: usize) {
        self.reserved_cols_left = left;
        self.reserved_cols_right = right;
    }

    /// Snapshot the total workspace count for a rail-slot menu (RAIL-REORDER).
    /// Applied by the App immediately after opening a `WorkspaceSlot` menu so
    /// the Move Up/Down rows can gate on the clicked slot's position (Move Down
    /// hides on the last slot; Move Up hides on the first). Every non-rail menu
    /// leaves it at `0`, where no Move rows are composed anyway.
    pub(in crate::native) fn set_workspace_count(&mut self, count: usize) {
        self.workspace_count = count;
    }

    pub(in crate::native) fn set_command_actions_enabled(&mut self, enabled: bool) {
        self.command_actions_enabled = enabled;
    }

    /// The saved host snapshotted for a `ConnectionRow` menu (ODP-2C), if any.
    /// Read by the overlay when a connection-row item activates so each outcome
    /// carries the clicked host without re-reading any file.
    pub(in crate::native) fn connection_target(&self) -> Option<&ConnectionHost> {
        self.connection_target.as_deref()
    }

    /// Whether the connection-row target is an OdyTTY-owned host (ODP-2C). Gates
    /// Edit/Remove visibility: an ssh-config-imported row is read-only (OdyTTY
    /// never writes `~/.ssh/config`), so those two items are hidden for it.
    pub(super) fn connection_is_odytty(&self) -> bool {
        self.connection_target
            .as_deref()
            .is_some_and(|host| host.source == ConnectionHostSource::Odytty)
    }

    /// Set the per-item effective-keybind labels (Part C), in
    /// [`ContextMenuItem::ALL`] order. Called by the App right after `open`,
    /// since the App owns the live `KeyBindings`. Keeping the lookup App-side
    /// leaves this menu a pure presentation struct.
    pub(in crate::native) fn set_accelerators(
        &mut self,
        accelerators: [Option<String>; CONTEXT_MENU_ITEMS],
    ) {
        self.accelerators = accelerators;
    }

    pub(in crate::native) fn rename_target(&self) -> Option<SessionToken> {
        self.rename_target
    }

    /// The resolved interactive path snapshotted at open time, if any. Used by
    /// the overlay to build the file-item outcomes (Open / Copy Path / Copy
    /// File / Reveal) when one of those items activates.
    pub(in crate::native) fn path_target(&self) -> Option<&Resolved> {
        self.path_target.as_ref()
    }

    pub(super) fn item_enabled(&self, item: ContextMenuItem) -> bool {
        match item {
            ContextMenuItem::Copy => self.copy_enabled,
            ContextMenuItem::Cut => self.cut_enabled,
            ContextMenuItem::Paste => self.paste_enabled,
            ContextMenuItem::Delete => self.delete_enabled,
            ContextMenuItem::SelectAll => true,
            ContextMenuItem::SelectCommandOutput
            | ContextMenuItem::SelectCommandWithPrompt
            | ContextMenuItem::CopyCommandOutput
            | ContextMenuItem::CopyCommandWithPrompt
            | ContextMenuItem::SearchCommandOutput
            | ContextMenuItem::JumpFailedCommandPrev
            | ContextMenuItem::JumpFailedCommandNext
            | ContextMenuItem::ExportCommandOutput => self.command_actions_enabled,
            ContextMenuItem::NewTab => true,
            ContextMenuItem::NewWindow => true,
            ContextMenuItem::RenameTab => self.rename_target.is_some(),
            ContextMenuItem::CloseTab => true,
            ContextMenuItem::DuplicateTab => true,
            // Nothing else to close when a lone tab is open.
            ContextMenuItem::CloseOtherTabs => self.multi_tab,
            // Only visible when >1 workspace exists; always enabled once shown.
            ContextMenuItem::MoveToWorkspace => self.multi_workspace,
            ContextMenuItem::SplitColumns => true,
            ContextMenuItem::SplitRows => true,
            ContextMenuItem::ClosePane => true,
            ContextMenuItem::Settings => true,
            ContextMenuItem::KeyboardShortcuts => true,
            ContextMenuItem::ConnectionManager => true,
            ContextMenuItem::CommandPalette => true,
            ContextMenuItem::SessionReplay => true,
            ContextMenuItem::SessionAttach => true,
            // Detach & switch acts on the focused pane, which always exists.
            ContextMenuItem::DetachSwitch => true,
            // The file items are only ever visible when a path resolved, so
            // they are enabled whenever shown.
            ContextMenuItem::OpenPath
            | ContextMenuItem::CopyPath
            | ContextMenuItem::CopyFile
            | ContextMenuItem::RevealPath => self.path_target.is_some(),
            // "Open in OdyTTY" is enabled only when the resolved path is an
            // image file — the same condition that makes it visible.
            ContextMenuItem::OpenInOdytty => self.is_image_target(),
            // "Open With…" is enabled only when the resolved path is a regular
            // file — the same condition that makes it visible.
            ContextMenuItem::OpenWith => self.is_file_target(),
            // Workspace actions are always available on their surfaces.
            ContextMenuItem::NewWorkspace
            | ContextMenuItem::DuplicateWorkspace
            | ContextMenuItem::RenameWorkspace
            | ContextMenuItem::CloseWorkspace
            // Move Up/Down are only pushed when the slot can move that way, so
            // they are enabled whenever shown (the guard is at composition).
            | ContextMenuItem::MoveWorkspaceUp
            | ContextMenuItem::MoveWorkspaceDown
            // Bind/Unbind are only visible on the matching bind state, so they
            // are enabled whenever shown.
            | ContextMenuItem::BindWorkspaceToHost
            | ContextMenuItem::UnbindWorkspace
            // LAYOUT-SURFACE: Save/Open Layout are always available on their
            // surfaces (Open Layout stays enabled even with no saved layouts —
            // the picker teaches the feature).
            | ContextMenuItem::SaveAllLayout
            | ContextMenuItem::SaveAsLayout
            | ContextMenuItem::OpenLayout => true,
            // Only shown on a bound-workspace tab menu; always enabled there.
            ContextMenuItem::NewLocalTab => true,
            // ODP-5D: always available on the tab surface (the destructive
            // replace is consent-gated at activation, not by disabling here).
            ContextMenuItem::ConnectToHost | ContextMenuItem::ReplaceTabWithHost => true,
            // ODP-2C: connection-row actions are enabled whenever shown; Edit
            // and Remove are gated by visibility (OdyTTY-owned only), not here,
            // and the destructive Remove is consent-gated at activation.
            ContextMenuItem::ConnRowOpenInTab
            | ContextMenuItem::ConnRowOpenInWorkspace
            | ContextMenuItem::ConnRowBindWorkspace
            | ContextMenuItem::ConnRowEdit
            | ContextMenuItem::ConnRowRemove => true,
        }
    }

    /// Whether the resolved path under the click is a regular file. Drives the
    /// visibility + enablement of the C3b "Open With…" item (a file-only
    /// affordance; a directory has no application handler list here).
    pub(super) fn is_file_target(&self) -> bool {
        self.path_target
            .as_ref()
            .is_some_and(|resolved| resolved.kind == crate::paths::FsKind::File)
    }

    /// Whether the resolved path under the click is an image file (a regular
    /// file whose extension is in [`crate::paths::IMAGE_EXTENSIONS`]). Drives
    /// the visibility + enablement of the C4 "Open in OdyTTY" item. Pure: trusts
    /// only the extension (the real decode confirm happens native at open time).
    pub(super) fn is_image_target(&self) -> bool {
        self.path_target.as_ref().is_some_and(|resolved| {
            resolved.kind == crate::paths::FsKind::File
                && crate::paths::is_image_path(&resolved.abs)
        })
    }

    /// The section an item belongs to *on the current surface*. On the
    /// `Content` surface this delegates to the item's global section (verbatim
    /// separator placement). The tab surfaces regroup the shared items into
    /// their own tight sections so the separators land where the composition
    /// wants them, independent of the global grouping.
    pub(super) fn section_of(&self, item: ContextMenuItem) -> u8 {
        match self.surface {
            ContextMenuSurface::TabSlot(_) => match item {
                ContextMenuItem::NewTab
                | ContextMenuItem::NewLocalTab
                | ContextMenuItem::DuplicateTab
                | ContextMenuItem::RenameTab => 0,
                ContextMenuItem::CloseTab | ContextMenuItem::CloseOtherTabs => 1,
                // ODP-5D host actions form their own group between the close
                // section and the workspace/window tail.
                ContextMenuItem::ConnectToHost | ContextMenuItem::ReplaceTabWithHost => 2,
                ContextMenuItem::MoveToWorkspace => 3,
                ContextMenuItem::NewWindow => 4,
                // Not part of the tab composition; grouped with New Window so a
                // stray item never forces a spurious separator.
                _ => 4,
            },
            ContextMenuSurface::TabStripEmpty => match item {
                // New Tab / New Workspace / Open Layout are the creation/restore
                // group; Command Palette / Settings sit below a separator.
                ContextMenuItem::NewTab
                | ContextMenuItem::NewWorkspace
                | ContextMenuItem::OpenLayout => 0,
                ContextMenuItem::CommandPalette | ContextMenuItem::Settings => 1,
                _ => 1,
            },
            ContextMenuSurface::WorkspaceSlot(_) => match item {
                // New/Rename group; Close in its own destructive group (one
                // separator before it) — the TabSlot pattern one level up.
                ContextMenuItem::NewWorkspace
                | ContextMenuItem::DuplicateWorkspace
                | ContextMenuItem::RenameWorkspace
                // RAIL-REORDER: the reorder actions are non-destructive edits to
                // the slot, so they group with New/Rename above the Close separator.
                | ContextMenuItem::MoveWorkspaceUp
                | ContextMenuItem::MoveWorkspaceDown => 0,
                ContextMenuItem::CloseWorkspace => 1,
                // RAIL-BIND: the host bind/unbind action sits in its own group
                // below the destructive Close.
                ContextMenuItem::BindWorkspaceToHost | ContextMenuItem::UnbindWorkspace => 2,
                // LAYOUT-SURFACE + RAIL-SAVE-ALL: both save actions share a
                // trailing group of their own (one separator above them).
                ContextMenuItem::SaveAllLayout | ContextMenuItem::SaveAsLayout => 3,
                ContextMenuItem::Settings => 4,
                _ => 4,
            },
            ContextMenuSurface::ConnectionRow(_) => match item {
                // Open/Bind group; Edit/Remove in their own group (a separator
                // before the mutating actions) — same shape as WorkspaceSlot.
                ContextMenuItem::ConnRowOpenInTab
                | ContextMenuItem::ConnRowOpenInWorkspace
                | ContextMenuItem::ConnRowBindWorkspace => 0,
                ContextMenuItem::ConnRowEdit | ContextMenuItem::ConnRowRemove => 1,
                _ => 1,
            },
            // LAYOUT-SURFACE: the empty rail offers New Workspace, then Open
            // Layout below a separator.
            ContextMenuSurface::WorkspaceRailEmpty => match item {
                ContextMenuItem::NewWorkspace => 0,
                ContextMenuItem::SaveAllLayout | ContextMenuItem::OpenLayout => 1,
                ContextMenuItem::Settings => 2,
                _ => 2,
            },
            _ => item.section(),
        }
    }

    /// The right-clicked surface this menu was opened on (F7).
    pub(in crate::native) fn surface(&self) -> ContextMenuSurface {
        self.surface
    }
}

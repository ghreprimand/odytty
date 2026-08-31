// SPDX-License-Identifier: GPL-3.0-only
use super::CONTEXT_MENU_ITEMS;
use crate::settings::BindableAction;

/// The selectable actions in the menu, in display order (separator excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum ContextMenuItem {
    Copy,
    Cut,
    Paste,
    Delete,
    SelectAll,
    SelectCommandOutput,
    SelectCommandWithPrompt,
    CopyCommandOutput,
    CopyCommandWithPrompt,
    SearchCommandOutput,
    JumpFailedCommandPrev,
    JumpFailedCommandNext,
    ExportCommandOutput,
    NewTab,
    /// Launch another top-level OdyTTY window (F1). Same action as the
    /// `Ctrl+Shift+N` chord; a fresh process instance, not a tab.
    NewWindow,
    RenameTab,
    CloseTab,
    /// Close every tab except the right-clicked one (F7). Tab-scoped: shown only
    /// on the `TabSlot` surface, targeting the clicked tab's token. Disabled
    /// when only one tab exists (nothing else to close).
    CloseOtherTabs,
    /// Open the "Move to Workspace" destination picker for the right-clicked
    /// tab (W4-v2). Tab-scoped: shown only on the `TabSlot` surface and only when
    /// more than one workspace exists. The item closes the menu and opens a
    /// type-to-filter picker seeded with every workspace but the source; the
    /// chosen destination drives a `Tab` value splice, the sessions never leave
    /// the arena.
    MoveToWorkspace,
    /// Split the focused pane into side-by-side columns (new pane right). Same
    /// action as the keyboard `Ctrl+Shift+E` / tmux `Ctrl-b %` path.
    SplitColumns,
    /// Split the focused pane into stacked rows (new pane below). Same action as
    /// the keyboard `Ctrl+Shift+O` / tmux `Ctrl-b "` path.
    SplitRows,
    /// Close the focused pane within a multi-pane tab. Same action as the tmux
    /// `Ctrl-b x` prefix / palette `close-pane`. Hidden in a single-pane tab
    /// (there is no pane to close short of closing the whole tab).
    ClosePane,
    /// Open the settings panel (always enabled, D-IN2-SETTINGS).
    Settings,
    /// Open the key-remap editor overlay directly (F3). Always enabled; same
    /// destination as Settings → Input → "keybinds" (`OverlayMode::KeyBindings`),
    /// surfaced here for discoverability. Placed first in the launcher section
    /// (right below Settings) so it groups with the other overlay-openers. No
    /// default chord — the editor is a config surface, not a daily action.
    KeyboardShortcuts,
    /// Open the connection manager overlay (v0.3.1 discoverability). Always
    /// enabled; same destination as the `Ctrl+Shift+S` chord.
    ConnectionManager,
    /// Open the command palette overlay (v0.3.1 discoverability). Always
    /// enabled; same destination as the `Ctrl+Shift+P` chord.
    CommandPalette,
    /// Open the session-replay overlay (v0.3.1 discoverability). Always enabled;
    /// same destination as the `Ctrl+Shift+R` chord.
    SessionReplay,
    /// Open the session-attach summon overlay (Phase 5 / B2). Always enabled;
    /// same destination as the `Ctrl+Shift+A` chord.
    SessionAttach,
    /// Open the resolved interactive path under the click (Phase 8 / C3). Shown
    /// only when a path resolved at the click cell; dispatches through the same
    /// argv-only open the Ctrl+click path uses.
    OpenPath,
    /// Open a resolved **image** span in the in-terminal viewer (Phase 9 / C4).
    /// Shown only when the resolved path is an image file (extension in
    /// [`crate::paths::IMAGE_EXTENSIONS`]); decodes + renders it through the GPU
    /// image layer as a presentation-only overlay.
    OpenInOdytty,
    /// Open the resolved file with a chosen application (Phase 8b / C3b). Shown
    /// only when the resolved path is a regular file; activating it opens the
    /// "Open With…" app-picker overlay (`OverlayMode::OpenWith`).
    OpenWith,
    /// Copy the resolved absolute path to the clipboard as text (C3).
    CopyPath,
    /// Copy a `file://<abs>` URI to the clipboard as text (C3).
    CopyFile,
    /// Reveal the resolved path in the desktop file manager (C3).
    RevealPath,
    /// Convert the focused pane into a fresh **managed** session in the same cwd
    /// and switch to it (Detach & switch). Always available (there is
    /// always a focused pane). HONEST framing: this is a SPAWN of a new shell in
    /// the same directory, not a live-process migration — the running shell is
    /// the window's child and cannot be losslessly handed to a survivable host.
    /// Activating it reads the focused pane's cwd and opens a 3-way choice
    /// dialog (Swap / Keep both / Cancel). Placed last in [`Self::ALL`] so the
    /// file-section indices stay stable; rendered in the session-management
    /// section (4), right after "Manage Sessions", in the non-path menu.
    DetachSwitch,
    /// Create a fresh workspace (its own single-pane tab) and switch to it.
    /// Workspace-scoped: shown on the `WorkspaceSlot` / `WorkspaceRailEmpty`
    /// surfaces (rail `+` slot / rail right-click). No default chord.
    NewWorkspace,
    /// Rename the right-clicked workspace in place. Workspace-scoped
    /// (`WorkspaceSlot`); opens the shared rename field targeting the slot.
    RenameWorkspace,
    /// Close the right-clicked workspace entirely — every tab, every pane.
    /// Workspace-scoped (`WorkspaceSlot`). Closing the last workspace exits.
    CloseWorkspace,
    /// Move the right-clicked workspace one slot toward the front of the rail
    /// (RAIL-REORDER). Workspace-scoped (`WorkspaceSlot`); shown only when the
    /// slot is not already first. Appended last in [`Self::ALL`] so existing
    /// accelerator-array indices stay stable; carries no chord.
    MoveWorkspaceUp,
    /// Move the right-clicked workspace one slot toward the back of the rail
    /// (RAIL-REORDER). Workspace-scoped (`WorkspaceSlot`); shown only when the
    /// slot is not already last. Appended last in [`Self::ALL`]; carries no chord.
    MoveWorkspaceDown,
    /// Bind the active workspace to a saved host (F6-W5 / ODP-6B). Content-grid
    /// workspace section, shown ONLY when the active workspace is unbound;
    /// activating it opens the shared host picker (ODP-1B) so New Tab there
    /// routes through the SSH connect path. No default chord.
    BindWorkspaceToHost,
    /// Unbind the active workspace (ODP-6B), returning its New Tab to a local
    /// shell. Content-grid workspace section, shown ONLY when the active
    /// workspace is bound — the conditional counterpart to Bind to Host.
    UnbindWorkspace,
    /// Save the WHOLE application (every workspace) as one named layout
    /// (SAVE-ALL-LAYOUT). This is the primary "Save as Layout" surface — a layout
    /// means the whole session. Shown on the content-grid workspace section and
    /// the empty rail; opens the "Layout name:" prompt, then the whole-app save
    /// path writes the file. No chord.
    SaveAllLayout,
    /// Save a SINGLE workspace as a named layout (LAYOUT-SURFACE). Workspace-
    /// scoped: on a `WorkspaceSlot` it saves the CLICKED workspace; in the
    /// content-grid workspace section it saves the ACTIVE one. Labelled "Save
    /// Workspace as Layout…" to distinguish it from the whole-app save. Opens the
    /// "Layout name:" prompt (the rename-modal idiom); the WP3 save path writes
    /// the file. No chord.
    SaveAsLayout,
    /// Open a saved layout by name (LAYOUT-SURFACE). Shown on the empty rail, the
    /// empty tab strip, and the content-grid workspace section; opens the shared
    /// picker seeded with the saved layout names. Instantiating APPENDS a new
    /// workspace (never clobbers). Stays visible with no saved layouts so the
    /// feature is discoverable (the picker then explains it). No chord.
    OpenLayout,
    /// Open a local shell in a new tab regardless of the active workspace's host
    /// binding (F6-W5 escape hatch). Shown on the `TabSlot` surface ONLY when the
    /// active workspace is bound to a host (an unbound workspace's New Tab is
    /// already local, so the row would be a duplicate). Placed last in
    /// [`Self::ALL`] so the existing accelerator-array indices stay stable.
    NewLocalTab,
    /// Duplicate the right-clicked tab: open a new local tab in the active
    /// pane's working directory (F1 cwd inheritance). HONEST framing: this is a
    /// fresh shell in the same directory, not a process fork — scrollback and the
    /// running program are not copied. Tab-scoped (`TabSlot`); bindable via
    /// [`BindableAction::DuplicateTab`]. Appended last in [`Self::ALL`] so the
    /// existing accelerator-array indices stay stable.
    DuplicateTab,
    /// Duplicate the right-clicked workspace: open a fresh workspace whose
    /// first shell spawns in the active pane's working directory (F1 cwd
    /// inheritance). HONEST framing: a fresh shell in the same directory, not a
    /// process/state fork -- scrollback and running programs are not copied.
    /// Workspace-scoped (`WorkspaceSlot`); bindable via
    /// [`BindableAction::DuplicateWorkspace`]. Appended last in [`Self::ALL`] so
    /// the existing accelerator-array indices stay stable.
    DuplicateWorkspace,
    /// Open a saved host in a NEW tab positioned right after the right-clicked
    /// tab (ODP-5D "Connect to host ▸"). Tab-scoped: shown only on the `TabSlot`
    /// surface. Activating it opens the shared host picker (ODP-1B) seeded with
    /// the clicked tab's token; the picked host spawns adjacent to that tab, so
    /// it reads as "connect from here" and never disturbs the clicked shell.
    ConnectToHost,
    /// Replace the right-clicked tab with a saved host (ODP-5D "Replace this tab
    /// with ▸"). Tab-scoped (`TabSlot`). Opens the same shared host picker for
    /// the clicked tab's token; on pick, the App closes that tab and opens the
    /// remote in its slot — gated behind a confirm when the tab holds a running
    /// foreground child (idle shells replace directly, no confirm).
    ReplaceTabWithHost,
    /// Open the right-clicked connection-manager row's host in a NEW tab in the
    /// current workspace (ODP-2C). Connection-row-scoped: shown only on the
    /// `ConnectionRow` surface. Routes through the same connect path the manager
    /// itself uses; the manager closes.
    ConnRowOpenInTab,
    /// Open the right-clicked connection-manager row's host in a NEW workspace
    /// pre-bound to that host (ODP-2C): the fresh workspace's first tab connects
    /// and its `default_profile` is set, so New Tab there routes remote.
    ConnRowOpenInWorkspace,
    /// Bind the CURRENT workspace to the right-clicked connection-manager row's
    /// host (ODP-2C). Reuses the frozen `set_active_workspace_default_profile`
    /// path and emits the bind toast; the host is already chosen (the clicked
    /// row), so no picker is needed.
    ConnRowBindWorkspace,
    /// Open the Add/Edit connection form pre-filled from the right-clicked
    /// connection-manager row (ODP-2C, defers to the P4 form). Shown ONLY for
    /// OdyTTY-owned rows — an ssh-config-imported row is read-only and never
    /// offers Edit (OdyTTY never writes `~/.ssh/config`).
    ConnRowEdit,
    /// Remove the right-clicked connection-manager row's `hosts.conf` block
    /// (ODP-2C, P1 byte-splice). Shown ONLY for OdyTTY-owned rows; gated behind
    /// a confirm dialog because it deletes a saved host.
    ConnRowRemove,
}

impl ContextMenuItem {
    /// The full item set in display order — the accelerator-array order the App
    /// fills. `ClosePane` is included here but filtered out of the *visible*
    /// list in a single-pane tab (see [`ContextMenuUi::visible_items`]).
    pub(in crate::native) const ALL: [ContextMenuItem; CONTEXT_MENU_ITEMS] = [
        Self::Copy,
        Self::Cut,
        Self::Paste,
        Self::Delete,
        Self::SelectAll,
        Self::SelectCommandOutput,
        Self::SelectCommandWithPrompt,
        Self::CopyCommandOutput,
        Self::CopyCommandWithPrompt,
        Self::SearchCommandOutput,
        Self::JumpFailedCommandPrev,
        Self::JumpFailedCommandNext,
        Self::ExportCommandOutput,
        Self::NewTab,
        Self::NewWindow,
        Self::RenameTab,
        Self::CloseTab,
        Self::SplitColumns,
        Self::SplitRows,
        Self::ClosePane,
        // Workspace section: on the content surface these render after the split
        // section and before Settings (their ALL position drives that order),
        // giving a distinct workspace group between panes and Settings.
        Self::NewWorkspace,
        Self::RenameWorkspace,
        Self::CloseWorkspace,
        // ODP-6B: the conditional bind/unbind pair sits in the workspace section
        // (exactly one is ever visible), right after Close Workspace.
        Self::BindWorkspaceToHost,
        Self::UnbindWorkspace,
        // LAYOUT-SURFACE: Save/Open Layout round out the workspace section, so
        // on the content surface they render right after the bind/unbind row. The
        // whole-app "Save as Layout" leads the single-workspace variant.
        Self::SaveAllLayout,
        Self::SaveAsLayout,
        Self::OpenLayout,
        Self::Settings,
        Self::KeyboardShortcuts,
        Self::ConnectionManager,
        Self::CommandPalette,
        Self::SessionReplay,
        Self::SessionAttach,
        Self::OpenPath,
        Self::OpenInOdytty,
        Self::OpenWith,
        Self::CopyPath,
        Self::CopyFile,
        Self::RevealPath,
        Self::DetachSwitch,
        Self::CloseOtherTabs,
        Self::MoveToWorkspace,
        Self::NewLocalTab,
        // ODP-5D tab-scoped host actions; appended last so the existing
        // accelerator-array indices stay stable (they carry no chord anyway).
        Self::ConnectToHost,
        Self::ReplaceTabWithHost,
        // ODP-2C connection-row actions; appended after the tab host actions so
        // every existing index stays stable (all carry no chord).
        Self::ConnRowOpenInTab,
        Self::ConnRowOpenInWorkspace,
        Self::ConnRowBindWorkspace,
        Self::ConnRowEdit,
        Self::ConnRowRemove,
        // DUPLICATE-TAB: appended last so every existing accelerator-array index
        // stays stable (its accelerator is filled from the flat table).
        Self::DuplicateTab,
        // RAIL-REORDER: workspace move actions; appended last so every existing
        // accelerator-array index stays stable (both carry no chord).
        Self::MoveWorkspaceUp,
        Self::MoveWorkspaceDown,
        // DUPLICATE-WORKSPACE: appended last so every existing accelerator-array
        // index stays stable (its accelerator is filled from the flat table).
        Self::DuplicateWorkspace,
    ];

    /// The visual section this item belongs to (0-based). A separator is drawn
    /// wherever consecutive *visible* items cross a section boundary, so the
    /// separator placement reflows automatically when Close Pane appears or
    /// disappears: editing (0), tab actions (1), split/pane actions (2),
    /// workspace actions (3), Settings (4), launchers (5), file/path (6).
    pub(super) fn section(self) -> u8 {
        match self {
            Self::Copy
            | Self::Cut
            | Self::Paste
            | Self::Delete
            | Self::SelectAll
            | Self::SelectCommandOutput
            | Self::SelectCommandWithPrompt
            | Self::CopyCommandOutput
            | Self::CopyCommandWithPrompt
            | Self::SearchCommandOutput
            | Self::JumpFailedCommandPrev
            | Self::JumpFailedCommandNext
            | Self::ExportCommandOutput => 0,
            Self::NewTab
            | Self::NewLocalTab
            | Self::DuplicateTab
            | Self::NewWindow
            | Self::RenameTab
            | Self::CloseTab
            | Self::CloseOtherTabs
            | Self::MoveToWorkspace
            // Tab-scoped host actions; grouped with the tab section for the
            // global fallback (they are TabSlot-only, so this only matters for
            // section() completeness — section_of gives them their own group).
            | Self::ConnectToHost
            | Self::ReplaceTabWithHost => 1,
            Self::SplitColumns | Self::SplitRows | Self::ClosePane => 2,
            Self::NewWorkspace
            | Self::DuplicateWorkspace
            | Self::RenameWorkspace
            | Self::CloseWorkspace
            | Self::MoveWorkspaceUp
            | Self::MoveWorkspaceDown
            | Self::BindWorkspaceToHost
            | Self::UnbindWorkspace
            // LAYOUT-SURFACE: Save/Open Layout live in the workspace section.
            | Self::SaveAllLayout
            | Self::SaveAsLayout
            | Self::OpenLayout
            // ODP-2C connection-row actions; grouped with the workspace section
            // for the global fallback (they are ConnectionRow-only, so this only
            // matters for section() completeness — section_of gives them their
            // own tight groups).
            | Self::ConnRowOpenInTab
            | Self::ConnRowOpenInWorkspace
            | Self::ConnRowBindWorkspace
            | Self::ConnRowEdit
            | Self::ConnRowRemove => 3,
            Self::Settings => 4,
            Self::KeyboardShortcuts
            | Self::ConnectionManager
            | Self::CommandPalette
            | Self::SessionReplay
            | Self::SessionAttach
            | Self::DetachSwitch => 5,
            Self::OpenPath
            | Self::OpenInOdytty
            | Self::OpenWith
            | Self::CopyPath
            | Self::CopyFile
            | Self::RevealPath => 6,
        }
    }

    /// The label painted for this item.
    pub(in crate::native) fn label(self) -> &'static str {
        match self {
            Self::Copy => "Copy Text",
            Self::Cut => "Cut",
            Self::Paste => "Paste Text",
            Self::Delete => "Delete",
            Self::SelectAll => "Select All",
            Self::SelectCommandOutput => "Select Command Output",
            Self::SelectCommandWithPrompt => "Select Command With Prompt",
            Self::CopyCommandOutput => "Copy Command Output",
            Self::CopyCommandWithPrompt => "Copy Command With Prompt",
            Self::SearchCommandOutput => "Search Command Output",
            Self::JumpFailedCommandPrev => "Previous Failed Command",
            Self::JumpFailedCommandNext => "Next Failed Command",
            Self::ExportCommandOutput => "Export Command Output\u{2026}",
            Self::NewTab => "New Tab",
            Self::NewWindow => "New Window",
            Self::RenameTab => "Rename Tab",
            Self::CloseTab => "Close Tab",
            Self::CloseOtherTabs => "Close Other Tabs",
            Self::MoveToWorkspace => "Move to Workspace…",
            Self::NewWorkspace => "New Workspace",
            Self::DuplicateWorkspace => "Duplicate Workspace",
            Self::RenameWorkspace => "Rename Workspace",
            Self::CloseWorkspace => "Close Workspace",
            Self::MoveWorkspaceUp => "Move Up",
            Self::MoveWorkspaceDown => "Move Down",
            Self::BindWorkspaceToHost => "Bind to Host\u{2026}",
            Self::UnbindWorkspace => "Unbind from Host",
            Self::SaveAllLayout => "Save as Layout\u{2026}",
            Self::SaveAsLayout => "Save Workspace as Layout\u{2026}",
            Self::OpenLayout => "Open Layout\u{2026}",
            Self::NewLocalTab => "New Local Tab",
            Self::DuplicateTab => "Duplicate Tab",
            Self::ConnectToHost => "Connect to Host\u{2026}",
            Self::ReplaceTabWithHost => "Replace with Host\u{2026}",
            Self::ConnRowOpenInTab => "Open in New Tab",
            Self::ConnRowOpenInWorkspace => "Open in New Workspace",
            Self::ConnRowBindWorkspace => "Bind Current Workspace",
            Self::ConnRowEdit => "Edit\u{2026}",
            Self::ConnRowRemove => "Remove\u{2026}",
            Self::SplitColumns => "Split Right",
            Self::SplitRows => "Split Down",
            Self::ClosePane => "Close Pane",
            Self::Settings => "Settings",
            Self::KeyboardShortcuts => "Keyboard Shortcuts",
            Self::ConnectionManager => "Connection Manager",
            Self::CommandPalette => "Command Palette",
            Self::SessionReplay => "Session Replay",
            Self::SessionAttach => "Manage Sessions",
            Self::OpenPath => "Open",
            Self::OpenInOdytty => "Open in OdyTTY",
            Self::OpenWith => "Open With\u{2026}",
            Self::CopyPath => "Copy Path",
            Self::CopyFile => "Copy File",
            Self::RevealPath => "Reveal in File Manager",
            Self::DetachSwitch => "Detach & switch",
        }
    }

    /// The [`BindableAction`] whose effective chord is shown as this item's
    /// accelerator (Part C), or `None` for items with no keyboard binding (Cut /
    /// Delete / Select All / Rename Tab). Copy/Paste/New Tab/Close Tab/Settings
    /// map onto their existing global actions; the splits map onto the direct
    /// split chords added to the global table.
    pub(in crate::native) fn bindable_action(self) -> Option<BindableAction> {
        match self {
            Self::Copy => Some(BindableAction::Copy),
            Self::Paste => Some(BindableAction::Paste),
            Self::SelectCommandOutput => Some(BindableAction::SelectCommandOutput),
            Self::SelectCommandWithPrompt => Some(BindableAction::SelectCommandWithPrompt),
            Self::CopyCommandOutput => Some(BindableAction::CopyCommandOutput),
            Self::CopyCommandWithPrompt => Some(BindableAction::CopyCommandWithPrompt),
            Self::SearchCommandOutput => Some(BindableAction::SearchCommandOutput),
            Self::JumpFailedCommandPrev => Some(BindableAction::JumpFailedCommandPrev),
            Self::JumpFailedCommandNext => Some(BindableAction::JumpFailedCommandNext),
            Self::ExportCommandOutput => Some(BindableAction::ExportCommandOutput),
            Self::NewTab => Some(BindableAction::NewTab),
            Self::NewWindow => Some(BindableAction::NewWindow),
            Self::CloseTab => Some(BindableAction::CloseTab),
            Self::DuplicateTab => Some(BindableAction::DuplicateTab),
            Self::DuplicateWorkspace => Some(BindableAction::DuplicateWorkspace),
            Self::SplitColumns => Some(BindableAction::SplitColumns),
            Self::SplitRows => Some(BindableAction::SplitRows),
            Self::Settings => Some(BindableAction::SettingsPanel),
            Self::ConnectionManager => Some(BindableAction::ConnectionManager),
            Self::CommandPalette => Some(BindableAction::CommandPalette),
            Self::SessionReplay => Some(BindableAction::SessionReplay),
            Self::SessionAttach => Some(BindableAction::SessionAttach),
            // Close Pane has no chord in the flat global table — it resolves only
            // on the multiplexer prefix (`Ctrl-b x`), which the flat
            // `chord_for_action` lookup cannot represent. The App fills its
            // accelerator slot specially from the prefix table, so this returns
            // `None` to skip the generic flat-table lookup. The file items
            // (Open … Reveal) are pointer-only and carry no chord.
            Self::Cut
            | Self::Delete
            | Self::SelectAll
            | Self::RenameTab
            | Self::CloseOtherTabs
            | Self::MoveToWorkspace
            | Self::ClosePane
            | Self::OpenPath
            | Self::OpenInOdytty
            | Self::OpenWith
            | Self::CopyPath
            | Self::CopyFile
            | Self::RevealPath
            // Keyboard Shortcuts (F3) has no default chord — the editor is a
            // config surface reached from Settings, so its accelerator slot is
            // intentionally empty.
            | Self::KeyboardShortcuts
            // Detach & switch is pointer-only (no global chord); skip the
            // flat-table lookup.
            | Self::DetachSwitch
            // Workspace actions have no default chord (rail / menu / palette
            // cover them; ODP-5).
            | Self::NewWorkspace
            | Self::RenameWorkspace
            | Self::CloseWorkspace
            // RAIL-REORDER: reorder actions are menu-only; no chord.
            | Self::MoveWorkspaceUp
            | Self::MoveWorkspaceDown
            // Workspace host bind/unbind (ODP-6B); reached from the menu, no chord.
            | Self::BindWorkspaceToHost
            | Self::UnbindWorkspace
            // New Local Tab is the F6-W5 escape hatch; no default chord.
            | Self::NewLocalTab
            // ODP-5D tab host actions are pointer/menu-only; no default chord.
            | Self::ConnectToHost
            | Self::ReplaceTabWithHost
            // LAYOUT-SURFACE save/open are pointer/menu-only; no chord.
            | Self::SaveAllLayout
            | Self::SaveAsLayout
            | Self::OpenLayout
            // ODP-2C connection-row actions are pointer/menu-only; no chord.
            | Self::ConnRowOpenInTab
            | Self::ConnRowOpenInWorkspace
            | Self::ConnRowBindWorkspace
            | Self::ConnRowEdit
            | Self::ConnRowRemove => None,
        }
    }
}

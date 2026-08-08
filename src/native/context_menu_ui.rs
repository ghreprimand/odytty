// SPDX-License-Identifier: GPL-3.0-only
//! Right-click context menu (IN2). A small, cell-rendered popup offering Copy /
//! Cut / Paste / Delete / Select All / New Tab / Close Tab / Settings and the
//! v0.3.1 launcher section (Connection Manager / Command Palette / Session
//! Replay), spawned at the pointer cell and edge-clamped to the grid.
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
//! Four visual separator lines partition the menu into editing/selection
//! commands (Copy…Select All), tab actions (New Tab / Rename Tab / Close Tab),
//! split/pane actions (Split Right / Split Down / Close Pane), the Settings
//! launcher, and the overlay launcher section (Connection Manager / Command
//! Palette / Session Replay). Separators occupy body rows but are neither
//! selectable nor focusable (D-IN2-SETTINGS).
//!
//! Pane-count gating: the **Close Pane** item is shown only when the active tab
//! is multi-pane (the App passes `multi_pane` at open time). In a single-pane
//! tab it is hidden entirely — not rendered, not navigable — so the single-pane
//! menu layout (and its render cache signature) is byte-identical to before the
//! item existed. The item set, separator placement, focus cycling, and body-row
//! mapping are all derived from the visible item list, so the layout reflows
//! cleanly when the item appears or disappears.
//!
//! Each selectable item renders its *effective* keybind (reverse action→chord
//! lookup against the live `KeyBindings`, so it tracks user rebinds) right-
//! aligned beside its label; items with no bound chord render no accelerator.
//! The App computes the accelerator strings at open time (it owns the
//! `KeyBindings`) and threads them in via [`ContextMenuUi::set_accelerators`].

use super::overlay::{OverlayInput, OverlayRect, PointerButton};
use super::session::SessionToken;
use crate::connection_hosts::{ConnectionHost, ConnectionHostSource};
use crate::paths::Resolved;
use crate::selection::CellPoint;
use crate::settings::BindableAction;

/// Number of entries in [`ContextMenuItem::ALL`] (Copy / Cut / Paste / Delete /
/// Select All / New Tab / New Window / Rename Tab / Close Tab / Split Right /
/// Split Down / Close Pane / Settings / Connection Manager / Command Palette /
/// Session Replay / Manage Sessions / Detach & switch / Open / Open in OdyTTY /
/// Open With… / Copy Path / Copy File / Reveal in File Manager / Connect to
/// Host / Replace with Host) plus the F3 "Keyboard Shortcuts" launcher item —
/// the size of the accelerator array the App fills in `ALL` order. NOT the
/// number of *visible* items: Close Pane is
/// hidden in a single-pane tab; path-scoped menus show only file actions plus
/// pinned Copy/Paste text actions; "Open in OdyTTY" is shown only when the
/// resolved path is an image file (C4); and "Open With…" is shown only when the
/// resolved path is a regular file (C3b). With no path and a selection the
/// single-pane content menu shows 23 visible items (New / Rename / Close
/// Workspace sit in their own section after Split, followed by the conditional
/// Bind-to-Host XOR Unbind row, then Save as Layout / Open Layout); multi-pane
/// adds Close Pane for 24. The five
/// ODP-2C connection-row actions (Open in New Tab / Open in New Workspace / Bind
/// Current Workspace / Edit / Remove) show ONLY on the `ConnectionRow` surface.
pub(super) const CONTEXT_MENU_ITEMS: usize = 47;

/// Body row index of the first visual separator in the single-pane content
/// reference, between Select All and New Tab. The reference is the
/// **with-selection** content menu (Copy/Cut/Paste/Delete/Select All present;
/// F7 hides Copy/Cut/Delete only with no selection, and drops the tab-only
/// Rename Tab / Close Other Tabs rows). Separators reflow with the visible item
/// list; these consts describe the reference layout the unit tests assert
/// against, so they are test-only.
#[cfg(test)]
pub(super) const CONTEXT_MENU_SEPARATOR_ROW: usize = 5;

/// Body row index of the second visual separator (with-selection reference),
/// between Close Tab and the split actions. F7 dropped the content-menu Rename
/// Tab row, tightening the tab-actions section to New Tab / New Window / Close
/// Tab.
#[cfg(test)]
pub(super) const CONTEXT_MENU_SECOND_SEPARATOR_ROW: usize = 9;

/// Body row index of the third visual separator (with-selection reference),
/// between the split actions and the workspace section (New / Rename / Close
/// Workspace).
#[cfg(test)]
pub(super) const CONTEXT_MENU_THIRD_SEPARATOR_ROW: usize = 12;

/// Body row index of the fourth visual separator (with-selection reference),
/// between the workspace section and Settings. The unbound reference shows the
/// Bind-to-Host row plus the whole-app Save as Layout, the single-workspace Save
/// Workspace as Layout, and Open Layout, so the section is seven items long
/// (LAYOUT-SURFACE + SAVE-ALL-LAYOUT).
#[cfg(test)]
pub(super) const CONTEXT_MENU_FOURTH_SEPARATOR_ROW: usize = 20;

/// Body row index of the fifth visual separator (with-selection reference),
/// between Settings and the launcher section (Connection Manager / Command
/// Palette / Session Replay).
#[cfg(test)]
pub(super) const CONTEXT_MENU_FIFTH_SEPARATOR_ROW: usize = 22;

/// Total body rows in the **with-selection** single-pane content reference:
/// twenty-four visible items plus five separator lines (Close Pane hidden;
/// Rename Tab dropped from the content menu; the workspace section adds New /
/// Rename / Close Workspace, the conditional Bind-to-Host row, the whole-app
/// Save as Layout, Save Workspace as Layout, Open Layout, and one separator).
/// Production uses [`ContextMenuUi::body_row_count`] for the live count.
#[cfg(test)]
pub(super) const CONTEXT_MENU_BODY_ROWS: usize = 29;

/// Minimum gap (in cells) between the longest label and the right-aligned
/// accelerator column, so labels and accelerators never abut (Part C).
pub(super) const ACCELERATOR_GAP: usize = 2;

pub(super) const SHELL_INTEGRATION_DISABLED_HINT: &str = "Enable shell integration in Settings";

/// The selectable actions in the menu, in display order (separator excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextMenuItem {
    Copy,
    Cut,
    Paste,
    Delete,
    SelectAll,
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
    pub(super) const ALL: [ContextMenuItem; CONTEXT_MENU_ITEMS] = [
        Self::Copy,
        Self::Cut,
        Self::Paste,
        Self::Delete,
        Self::SelectAll,
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
    fn section(self) -> u8 {
        match self {
            Self::Copy | Self::Cut | Self::Paste | Self::Delete | Self::SelectAll => 0,
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
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Copy => "Copy Text",
            Self::Cut => "Cut",
            Self::Paste => "Paste Text",
            Self::Delete => "Delete",
            Self::SelectAll => "Select All",
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
    pub(super) fn bindable_action(self) -> Option<BindableAction> {
        match self {
            Self::Copy => Some(BindableAction::Copy),
            Self::Paste => Some(BindableAction::Paste),
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

/// Which UI surface a right-click landed on. Drives the menu composition: the
/// selection / clipboard / path / pane-count / tab-count are still snapshotted
/// from live app state at open time; the surface selects WHICH composition
/// consumes them. This generalizes the pre-existing path-vs-global branch in
/// [`ContextMenuUi::visible_items`] to the tab and empty-strip surfaces, fixing
/// the "full global menu on a tab" and "grid menu leaking over the empty bar"
/// findings (F7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ContextMenuSurface {
    /// A specific tab slot (top bar; and the vertical rail in the current build
    /// until the workspace reframe moves tabs to top-only). Carries the tab's
    /// token so Rename AND Close act on the RIGHT-CLICKED tab, not the active
    /// one.
    TabSlot(SessionToken),
    /// Empty region of the tab strip / rail (the `+` button area, gaps).
    TabStripEmpty,
    /// The terminal content grid — the selection/path/pane-aware menu.
    #[default]
    Content,

    /// The inter-pane divider (ODP-7). Reserved; not constructed yet.
    #[allow(dead_code)]
    PaneDivider,
    /// A workspace slot in the rail-as-workspaces sidebar (§7.4 / NF-F7-4).
    /// Carries the rail index so Rename/Close act on the RIGHT-CLICKED workspace.
    WorkspaceSlot(usize),
    /// Empty area of the workspace rail (the `+` slot, gaps). New Workspace.
    WorkspaceRailEmpty,
    /// A saved-host row inside the connection-manager overlay (ODP-2C). Carries
    /// the row's cursor index into the filtered list; the clicked host itself is
    /// snapshotted separately (see [`ContextMenuUi::connection_target`]) so the
    /// composition can gate Edit/Remove on its source. This is the only
    /// menu-over-overlay surface — the menu spawns while the connection manager
    /// stays loaded underneath, and dismissing it returns to the manager.
    ConnectionRow(usize),
}

impl ContextMenuSurface {
    /// A stable discriminant for the render-cache signature so a surface change
    /// repaints the menu. The `TabSlot` token is not part of it — the rendered
    /// rows are identical across tokens (only Rename/Close targeting differs,
    /// which the spawn cell already distinguishes).
    fn discriminant(self) -> u8 {
        // `Content` is 0 so a default (closed) menu's signature equals
        // `ContextMenuSignature::default()` — the off-path identity invariant.
        match self {
            Self::Content => 0,
            Self::TabSlot(_) => 1,
            Self::TabStripEmpty => 2,
            Self::PaneDivider => 3,
            Self::WorkspaceSlot(_) => 4,
            Self::WorkspaceRailEmpty => 5,
            Self::ConnectionRow(_) => 6,
        }
    }
}

/// Map a selectable item index to its body row, accounting for the five
/// separators. With-selection single-pane reference: items 0–4 (editing) sit at
/// body rows 0–4; items 5–7 (tab actions: New Tab / New Window / Close Tab) sit
/// at body rows 6–8; items 8–9 (splits) sit at body rows 10–11; items 10–16
/// (workspace: New / Rename / Close Workspace / Bind to Host / Save as Layout /
/// Save Workspace as Layout / Open Layout) sit at body rows 13–19; Settings
/// (index 17) sits at body row 21; the launcher items 18–23 sit at body rows
/// 23–28.
#[cfg(test)]
fn item_to_body_row(item_index: usize) -> usize {
    // With-selection reference: five separators at body rows 5, 9, 12, 20, 22,
    // so the launcher section (items 18+) shifts by five, Settings (item 17) by
    // four, the workspace section (items 10-16) by three, the splits (items 8-9)
    // by two, and the tab actions (items 5-7) by one.
    if item_index >= 18 {
        item_index + 5
    } else if item_index >= 17 {
        item_index + 4
    } else if item_index >= 10 {
        item_index + 3
    } else if item_index >= 8 {
        item_index + 2
    } else if item_index >= CONTEXT_MENU_SEPARATOR_ROW {
        item_index + 1
    } else {
        item_index
    }
}

/// Map a body row to a selectable item index, or `None` for a separator row
/// (single-pane layout reference; production uses
/// [`ContextMenuUi::body_row_to_item_index`] for the live multi-pane-aware
/// mapping).
#[cfg(test)]
fn body_row_to_item(body_row: usize) -> Option<usize> {
    if body_row == CONTEXT_MENU_SEPARATOR_ROW
        || body_row == CONTEXT_MENU_SECOND_SEPARATOR_ROW
        || body_row == CONTEXT_MENU_THIRD_SEPARATOR_ROW
        || body_row == CONTEXT_MENU_FOURTH_SEPARATOR_ROW
        || body_row == CONTEXT_MENU_FIFTH_SEPARATOR_ROW
    {
        None
    } else if body_row > CONTEXT_MENU_FIFTH_SEPARATOR_ROW {
        Some(body_row - 5)
    } else if body_row > CONTEXT_MENU_FOURTH_SEPARATOR_ROW {
        Some(body_row - 4)
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
    pub(super) prompt_editing_hint: bool,
    pub(super) rename_enabled: bool,
    /// Whether the active tab is multi-pane (drives the Close Pane item's
    /// visibility, so a pane-count change must repaint the menu).
    pub(super) multi_pane: bool,
    /// Whether more than one tab is open (drives the `Close Other Tabs` item's
    /// enablement on the tab surface, so a tab-count change repaints).
    pub(super) multi_tab: bool,
    /// Whether more than one workspace exists (drives the `Move to Workspace`
    /// item's visibility on the tab surface, so a workspace-count change
    /// repaints).
    pub(super) multi_workspace: bool,
    /// Whether the active workspace is bound to a host (F6-W5): drives the
    /// `New Local Tab` escape row's visibility on the tab surface, so a
    /// bind/unbind must repaint the menu.
    pub(super) bound_workspace: bool,
    /// The total workspace count at open time (RAIL-REORDER): drives the Move
    /// Up/Down items' visibility on a `WorkspaceSlot` menu (a slot can move down
    /// only when it is not last), so a workspace-count change must repaint.
    pub(super) workspace_count: usize,
    /// The surface discriminant (F7): a surface change swaps the whole
    /// composition, so it must repaint.
    pub(super) surface: u8,
    /// Whether a resolved interactive path sits under the click (drives the file
    /// section's visibility, so its presence must repaint the menu). The
    /// resolved path itself is not part of the signature — the labels are
    /// static, so a bool fully describes the layout change.
    pub(super) has_path_target: bool,
    /// Whether the resolved path is an image file (drives the "Open in OdyTTY"
    /// item's visibility, so its presence must repaint the menu). C4.
    pub(super) is_image_target: bool,
    /// Whether the resolved path is a regular file (drives the "Open With…"
    /// item's visibility — file-only — so a file-vs-directory target repaints
    /// the menu). C3b.
    pub(super) is_file_target: bool,
    /// Whether the connection-row target is OdyTTY-owned (ODP-2C): drives the
    /// Edit/Remove rows' visibility, so an OdyTTY-vs-ssh-config row repaints the
    /// menu even though both share the `ConnectionRow` surface discriminant.
    pub(super) connection_is_odytty: bool,
}

/// The right-click context menu state. Holds the spawn cell, the focused item,
/// and the snapshot of which items are enabled. No stored scroll state: when the
/// window is too short to show every row, the visible window is derived purely
/// from the focused item and the box-clamped body height (see
/// [`ContextMenuUi::scroll_offset`]), so it stays unit-testable without a GPU.
#[derive(Debug, Clone)]
pub(super) struct ContextMenuUi {
    spawn: CellPoint,
    focused: usize,
    copy_enabled: bool,
    cut_enabled: bool,
    paste_enabled: bool,
    delete_enabled: bool,
    prompt_editing_hint: bool,
    rename_target: Option<SessionToken>,
    /// Whether the active tab is multi-pane, snapshotted at open time. Gates the
    /// visibility of the Close Pane item: `false` hides it entirely (single-pane
    /// layout is byte-identical to before the item existed).
    multi_pane: bool,
    /// Whether more than one tab is open, snapshotted at open time. Gates the
    /// enablement of the `Close Other Tabs` item (disabled when a lone tab).
    multi_tab: bool,
    /// Whether more than one workspace exists, snapshotted at open time. Gates
    /// the visibility of `Move to Workspace` on the tab surface -- with a
    /// single workspace there is nowhere to move a tab, so the item is hidden
    /// (W4-v2: hide the move item when only one workspace exists).
    multi_workspace: bool,
    /// Whether the active workspace is bound to a host (F6-W5). Drives the
    /// `New Local Tab` escape row on the tab surface.
    bound_workspace: bool,
    /// The total workspace count, snapshotted at open time (RAIL-REORDER). On a
    /// `WorkspaceSlot(idx)` menu it decides whether the clicked slot can move
    /// down (`idx + 1 < workspace_count`); `move up` keys off `idx > 0`. The App
    /// sets it via [`Self::set_workspace_count`] right after opening a rail menu;
    /// every other surface leaves it at `0` (no Move rows there anyway).
    workspace_count: usize,
    /// Which surface the right-click landed on (F7). Selects the menu
    /// composition; defaults to [`ContextMenuSurface::Content`] (the historical
    /// single menu) so a bare `open` keeps today's behavior.
    surface: ContextMenuSurface,
    /// The resolved interactive path under the click cell, snapshotted at open
    /// time (re-detected by the App, not reused from the hover state). `Some`
    /// shows the file section (Open / Copy Path / Copy File / Reveal); `None`
    /// hides it entirely so the menu is byte-identical to before C3.
    path_target: Option<Resolved>,
    /// The saved host under the right-clicked connection-manager row (ODP-2C),
    /// snapshotted at open time on the `ConnectionRow` surface. `Some` gates
    /// Edit/Remove on its source (OdyTTY-owned only) and supplies the host to
    /// each connection-row outcome; `None` on every other surface, so the menu
    /// is byte-identical to before ODP-2C off that surface.
    connection_target: Option<Box<ConnectionHost>>,
    /// Per-item effective-keybind labels (Part C), indexed by
    /// [`ContextMenuItem::ALL`] order. `None` means the item shows no
    /// accelerator. Reset to all-`None` on `open`; the App overwrites via
    /// [`Self::set_accelerators`] from the live `KeyBindings`.
    accelerators: [Option<String>; CONTEXT_MENU_ITEMS],
    /// Column bands to keep the menu box clear of, on the left and right edges
    /// respectively (MENU-Z-ORDER). A rail-anchored menu opened while the
    /// auto-hide workspace rail is revealed must not paint UNDER the floating
    /// rail band (the rail composites topmost, so it would occlude the menu's
    /// edge). Reserving the rail band's columns positions the box beside the
    /// rail instead of under it — the rail stays visible (RAIL-PIN) and the menu
    /// is fully clickable. `0`/`0` (every non-rail open) is byte-identical to
    /// the pre-clearance layout.
    reserved_cols_left: usize,
    reserved_cols_right: usize,
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
    pub(super) fn new() -> Self {
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
    pub(super) fn open(
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
    pub(super) fn open_with_prompt_editing_hint(
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
    pub(super) fn open_connection_row(
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
    pub(super) fn set_rail_clearance(&mut self, left: usize, right: usize) {
        self.reserved_cols_left = left;
        self.reserved_cols_right = right;
    }

    /// Snapshot the total workspace count for a rail-slot menu (RAIL-REORDER).
    /// Applied by the App immediately after opening a `WorkspaceSlot` menu so
    /// the Move Up/Down rows can gate on the clicked slot's position (Move Down
    /// hides on the last slot; Move Up hides on the first). Every non-rail menu
    /// leaves it at `0`, where no Move rows are composed anyway.
    pub(super) fn set_workspace_count(&mut self, count: usize) {
        self.workspace_count = count;
    }

    /// The saved host snapshotted for a `ConnectionRow` menu (ODP-2C), if any.
    /// Read by the overlay when a connection-row item activates so each outcome
    /// carries the clicked host without re-reading any file.
    pub(super) fn connection_target(&self) -> Option<&ConnectionHost> {
        self.connection_target.as_deref()
    }

    /// Whether the connection-row target is an OdyTTY-owned host (ODP-2C). Gates
    /// Edit/Remove visibility: an ssh-config-imported row is read-only (OdyTTY
    /// never writes `~/.ssh/config`), so those two items are hidden for it.
    fn connection_is_odytty(&self) -> bool {
        self.connection_target
            .as_deref()
            .is_some_and(|host| host.source == ConnectionHostSource::Odytty)
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

    /// The resolved interactive path snapshotted at open time, if any. Used by
    /// the overlay to build the file-item outcomes (Open / Copy Path / Copy
    /// File / Reveal) when one of those items activates.
    pub(super) fn path_target(&self) -> Option<&Resolved> {
        self.path_target.as_ref()
    }

    fn item_enabled(&self, item: ContextMenuItem) -> bool {
        match item {
            ContextMenuItem::Copy => self.copy_enabled,
            ContextMenuItem::Cut => self.cut_enabled,
            ContextMenuItem::Paste => self.paste_enabled,
            ContextMenuItem::Delete => self.delete_enabled,
            ContextMenuItem::SelectAll => true,
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
    fn is_file_target(&self) -> bool {
        self.path_target
            .as_ref()
            .is_some_and(|resolved| resolved.kind == crate::paths::FsKind::File)
    }

    /// Whether the resolved path under the click is an image file (a regular
    /// file whose extension is in [`crate::paths::IMAGE_EXTENSIONS`]). Drives
    /// the visibility + enablement of the C4 "Open in OdyTTY" item. Pure: trusts
    /// only the extension (the real decode confirm happens native at open time).
    fn is_image_target(&self) -> bool {
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
    fn section_of(&self, item: ContextMenuItem) -> u8 {
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
    pub(super) fn surface(&self) -> ContextMenuSurface {
        self.surface
    }

    /// The items currently visible, in display order. Close Pane is included
    /// only in a multi-pane tab; everything else is always present. The visible
    /// list is the single source of truth for focus indices, separator
    /// placement, body-row mapping, and rendering, so the menu reflows cleanly
    /// when the pane count changes.
    fn visible_items(&self) -> Vec<ContextMenuItem> {
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
    fn item_count(&self) -> usize {
        self.visible_items().len()
    }

    /// The body rows in display order: `Some(item)` for a selectable row,
    /// `None` for a separator. A separator is inserted wherever consecutive
    /// visible items cross a section boundary ([`ContextMenuItem::section`]).
    fn body_layout(&self) -> Vec<Option<ContextMenuItem>> {
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
    fn body_row_count(&self) -> usize {
        self.body_layout().len()
    }

    /// Map a body row to its index in the visible item list, or `None` for a
    /// separator / out-of-range row. The multi-pane-aware production analogue of
    /// the single-pane `body_row_to_item` test reference.
    fn body_row_to_item_index(&self, body_row: usize) -> Option<usize> {
        let layout = self.body_layout();
        let item = (*layout.get(body_row)?)?;
        self.visible_items().iter().position(|it| *it == item)
    }

    /// The accelerator label for an item, or `None` when the item has no bound
    /// chord. Looked up by the item's position in [`ContextMenuItem::ALL`] (the
    /// order the App fills the accelerator array), so it is stable regardless of
    /// which items are currently visible.
    fn accelerator_for_item(&self, item: ContextMenuItem) -> Option<&str> {
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
    pub(super) fn rect(&self, columns: usize, rows: usize) -> OverlayRect {
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
    fn focused_body_row(&self) -> usize {
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
    pub(super) fn scroll_offset(&self, body_height: usize) -> usize {
        let total = self.body_row_count();
        if body_height == 0 || total <= body_height {
            return 0;
        }
        let max_scroll = total - body_height;
        let focused_row = self.focused_body_row();
        let desired = focused_row.saturating_sub(body_height - 1);
        desired.min(max_scroll)
    }

    fn focus_prev(&mut self) {
        let n = self.item_count();
        self.focused = (self.focused + n - 1) % n;
    }

    fn focus_next(&mut self) {
        let n = self.item_count();
        self.focused = (self.focused + 1) % n;
    }

    fn activate_focused(&self) -> ContextMenuOutcome {
        let item = self.visible_items()[self.focused];
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
    /// the overlay, i.e. relative to the *visible* window). `body_height` is the
    /// box-clamped visible row count, so the press is offset by the current
    /// [`Self::scroll_offset`] to reach the true body row. Activation happens on
    /// PRESS. A press past the visible window, on the separator row, or past the
    /// last body row is inert. The pressed item also takes focus. Disabled items
    /// swallow the press (D-IN2-6).
    pub(super) fn handle_press(
        &mut self,
        row_in_body: usize,
        body_height: usize,
        _button: PointerButton,
    ) -> ContextMenuOutcome {
        if row_in_body >= body_height {
            return ContextMenuOutcome::Consumed;
        }
        let body_row = self.scroll_offset(body_height) + row_in_body;
        if body_row >= self.body_row_count() {
            return ContextMenuOutcome::Consumed;
        }
        let Some(item_index) = self.body_row_to_item_index(body_row) else {
            // Separator row: inert.
            return ContextMenuOutcome::Consumed;
        };
        self.focused = item_index;
        let item = self.visible_items()[item_index];
        if self.item_enabled(item) {
            ContextMenuOutcome::Activate(item)
        } else {
            ContextMenuOutcome::Consumed
        }
    }

    /// Move focus to the item under a hovering pointer (D-IN2-6). `row_in_body`
    /// is `None` when the pointer is on the border / off a body row, leaving
    /// focus unchanged. `body_height` is the box-clamped visible row count; the
    /// hovered row is offset by the current [`Self::scroll_offset`] to reach the
    /// true body row. A hover past the visible window or on a separator row is
    /// skipped (focus stays on its last position).
    pub(super) fn handle_hover(&mut self, row_in_body: Option<usize>, body_height: usize) {
        if let Some(row) = row_in_body
            && row < body_height
            && let Some(item_index) =
                self.body_row_to_item_index(self.scroll_offset(body_height) + row)
        {
            self.focused = item_index;
        }
    }

    /// The rendered body rows in display order. Each entry is either an
    /// [`ContextMenuRow::Item`] (with label, focus, and enabled state) or
    /// [`ContextMenuRow::Separator`] (the visual divider). The renderer decides
    /// how to paint each row type.
    pub(super) fn rows(&self) -> Vec<ContextMenuRow> {
        let items = self.visible_items();
        let mut out = Vec::with_capacity(self.body_row_count());
        let mut prev_section: Option<u8> = None;
        for (item_index, item) in items.iter().enumerate() {
            // Insert a separator wherever consecutive visible items cross a
            // section boundary, so the layout reflows when Close Pane appears.
            let section = self.section_of(*item);
            if prev_section.is_some_and(|p| p != section) {
                out.push(ContextMenuRow::Separator);
            }
            prev_section = Some(section);
            out.push(ContextMenuRow::Item {
                label: item.label(),
                accelerator: self.accelerator_for_item(*item).map(str::to_owned),
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
            prompt_editing_hint: self.prompt_editing_hint,
            rename_enabled: self.rename_target.is_some(),
            multi_pane: self.multi_pane,
            multi_tab: self.multi_tab,
            multi_workspace: self.multi_workspace,
            bound_workspace: self.bound_workspace,
            workspace_count: self.workspace_count,
            surface: self.surface.discriminant(),
            has_path_target: self.path_target.is_some(),
            is_image_target: self.is_image_target(),
            is_file_target: self.is_file_target(),
            connection_is_odytty: self.connection_is_odytty(),
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
#[path = "context_menu_ui_tests.rs"]
mod tests;

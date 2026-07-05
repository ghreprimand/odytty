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
/// single-pane content menu shows 21 visible items (New / Rename / Close
/// Workspace sit in their own section after Split, followed by the conditional
/// Bind-to-Host XOR Unbind row); multi-pane adds Close Pane for 22.
pub(super) const CONTEXT_MENU_ITEMS: usize = 35;

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
/// Bind-to-Host row in the workspace section, so the section is four items long.
#[cfg(test)]
pub(super) const CONTEXT_MENU_FOURTH_SEPARATOR_ROW: usize = 17;

/// Body row index of the fifth visual separator (with-selection reference),
/// between Settings and the launcher section (Connection Manager / Command
/// Palette / Session Replay).
#[cfg(test)]
pub(super) const CONTEXT_MENU_FIFTH_SEPARATOR_ROW: usize = 19;

/// Total body rows in the **with-selection** single-pane content reference:
/// twenty-one visible items plus five separator lines (Close Pane hidden;
/// Rename Tab dropped from the content menu; the workspace section adds three
/// items, the conditional Bind-to-Host row, and one separator). Production uses
/// [`ContextMenuUi::body_row_count`] for the live count.
#[cfg(test)]
pub(super) const CONTEXT_MENU_BODY_ROWS: usize = 26;

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
    /// Move the right-clicked tab to the next workspace in rail order, wrapping
    /// (ODP-7, "Move to workspace" v1). Tab-scoped: shown only on the `TabSlot`
    /// surface and only when more than one workspace exists — a `Tab` value
    /// splice, the sessions never leave the arena. A destination-picker submenu
    /// listing every workspace is a v2 follow-up (the menu has no dynamic-label
    /// / flyout surface yet); this next-workspace step is exact for the common
    /// two-workspace case and cycles for more.
    MoveToNextWorkspace,
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
    /// and switch to it (Packet 2 / Detach & switch). Always available (there is
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
    /// Bind the active workspace to a saved host (F6-W5 / ODP-6B). Content-grid
    /// workspace section, shown ONLY when the active workspace is unbound;
    /// activating it opens the shared host picker (ODP-1B) so New Tab there
    /// routes through the SSH connect path. No default chord.
    BindWorkspaceToHost,
    /// Unbind the active workspace (ODP-6B), returning its New Tab to a local
    /// shell. Content-grid workspace section, shown ONLY when the active
    /// workspace is bound — the conditional counterpart to Bind to Host.
    UnbindWorkspace,
    /// Open a local shell in a new tab regardless of the active workspace's host
    /// binding (F6-W5 escape hatch). Shown on the `TabSlot` surface ONLY when the
    /// active workspace is bound to a host (an unbound workspace's New Tab is
    /// already local, so the row would be a duplicate). Placed last in
    /// [`Self::ALL`] so the existing accelerator-array indices stay stable.
    NewLocalTab,
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
        Self::MoveToNextWorkspace,
        Self::NewLocalTab,
        // ODP-5D tab-scoped host actions; appended last so the existing
        // accelerator-array indices stay stable (they carry no chord anyway).
        Self::ConnectToHost,
        Self::ReplaceTabWithHost,
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
            | Self::NewWindow
            | Self::RenameTab
            | Self::CloseTab
            | Self::CloseOtherTabs
            | Self::MoveToNextWorkspace
            // Tab-scoped host actions; grouped with the tab section for the
            // global fallback (they are TabSlot-only, so this only matters for
            // section() completeness — section_of gives them their own group).
            | Self::ConnectToHost
            | Self::ReplaceTabWithHost => 1,
            Self::SplitColumns | Self::SplitRows | Self::ClosePane => 2,
            Self::NewWorkspace
            | Self::RenameWorkspace
            | Self::CloseWorkspace
            | Self::BindWorkspaceToHost
            | Self::UnbindWorkspace => 3,
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
            Self::MoveToNextWorkspace => "Move to Next Workspace",
            Self::NewWorkspace => "New Workspace",
            Self::RenameWorkspace => "Rename Workspace",
            Self::CloseWorkspace => "Close Workspace",
            Self::BindWorkspaceToHost => "Bind to Host\u{2026}",
            Self::UnbindWorkspace => "Unbind from Host",
            Self::NewLocalTab => "New Local Tab",
            Self::ConnectToHost => "Connect to Host\u{2026}",
            Self::ReplaceTabWithHost => "Replace with Host\u{2026}",
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
            | Self::MoveToNextWorkspace
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
            // Workspace host bind/unbind (ODP-6B); reached from the menu, no chord.
            | Self::BindWorkspaceToHost
            | Self::UnbindWorkspace
            // New Local Tab is the F6-W5 escape hatch; no default chord.
            | Self::NewLocalTab
            // ODP-5D tab host actions are pointer/menu-only; no default chord.
            | Self::ConnectToHost
            | Self::ReplaceTabWithHost => None,
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
        }
    }
}

/// Map a selectable item index to its body row, accounting for the five
/// separators. With-selection single-pane reference: items 0–4 (editing) sit at
/// body rows 0–4; items 5–7 (tab actions: New Tab / New Window / Close Tab) sit
/// at body rows 6–8; items 8–9 (splits) sit at body rows 10–11; items 10–13
/// (workspace: New / Rename / Close Workspace / Bind to Host) sit at body rows
/// 13–16; Settings (index 14) sits at body row 18; the launcher items 15–20 sit
/// at body rows 20–25.
#[cfg(test)]
fn item_to_body_row(item_index: usize) -> usize {
    // With-selection reference: five separators at body rows 5, 9, 12, 17, 19,
    // so the launcher section (items 15+) shifts by five, Settings (item 14) by
    // four, the workspace section (items 10-13) by three, the splits (items 8-9)
    // by two, and the tab actions (items 5-7) by one.
    if item_index >= 15 {
        item_index + 5
    } else if item_index >= 14 {
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
    /// Whether more than one workspace exists (drives the `Move to Next
    /// Workspace` item's visibility on the tab surface, so a workspace-count
    /// change repaints).
    pub(super) multi_workspace: bool,
    /// Whether the active workspace is bound to a host (F6-W5): drives the
    /// `New Local Tab` escape row's visibility on the tab surface, so a
    /// bind/unbind must repaint the menu.
    pub(super) bound_workspace: bool,
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
    /// the visibility of `Move to Next Workspace` on the tab surface — with a
    /// single workspace there is nowhere to move a tab, so the item is hidden
    /// (ODP-7: hide the move submenu when only one workspace exists).
    multi_workspace: bool,
    /// Whether the active workspace is bound to a host (F6-W5). Drives the
    /// `New Local Tab` escape row on the tab surface.
    bound_workspace: bool,
    /// Which surface the right-click landed on (F7). Selects the menu
    /// composition; defaults to [`ContextMenuSurface::Content`] (the historical
    /// single menu) so a bare `open` keeps today's behavior.
    surface: ContextMenuSurface,
    /// The resolved interactive path under the click cell, snapshotted at open
    /// time (re-detected by the App, not reused from the hover state). `Some`
    /// shows the file section (Open / Copy Path / Copy File / Reveal); `None`
    /// hides it entirely so the menu is byte-identical to before C3.
    path_target: Option<Resolved>,
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
            prompt_editing_hint: false,
            rename_target: None,
            multi_pane: false,
            multi_tab: false,
            multi_workspace: false,
            bound_workspace: false,
            surface: ContextMenuSurface::Content,
            path_target: None,
            // `[T; N]: Default` only exists up to N == 32; the item set is now
            // larger, so build the all-`None` array element-wise.
            accelerators: std::array::from_fn(|_| None),
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
        self.surface = surface;
        self.path_target = path_target;
        self.focused = 0;
        // Clear any stale accelerators; the App repopulates immediately via
        // `set_accelerators`. A bare `open` (the unit-test path) shows no
        // accelerators, which is the label-only legacy layout.
        self.accelerators = std::array::from_fn(|_| None);
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
            // Nothing else to close when a lone tab is open.
            ContextMenuItem::CloseOtherTabs => self.multi_tab,
            // Only visible when >1 workspace exists; always enabled once shown.
            ContextMenuItem::MoveToNextWorkspace => self.multi_workspace,
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
            | ContextMenuItem::RenameWorkspace
            | ContextMenuItem::CloseWorkspace
            // Bind/Unbind are only visible on the matching bind state, so they
            // are enabled whenever shown.
            | ContextMenuItem::BindWorkspaceToHost
            | ContextMenuItem::UnbindWorkspace => true,
            // Only shown on a bound-workspace tab menu; always enabled there.
            ContextMenuItem::NewLocalTab => true,
            // ODP-5D: always available on the tab surface (the destructive
            // replace is consent-gated at activation, not by disabling here).
            ContextMenuItem::ConnectToHost | ContextMenuItem::ReplaceTabWithHost => true,
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
                | ContextMenuItem::RenameTab => 0,
                ContextMenuItem::CloseTab | ContextMenuItem::CloseOtherTabs => 1,
                // ODP-5D host actions form their own group between the close
                // section and the workspace/window tail.
                ContextMenuItem::ConnectToHost | ContextMenuItem::ReplaceTabWithHost => 2,
                ContextMenuItem::MoveToNextWorkspace => 3,
                ContextMenuItem::NewWindow => 4,
                // Not part of the tab composition; grouped with New Window so a
                // stray item never forces a spurious separator.
                _ => 4,
            },
            ContextMenuSurface::TabStripEmpty => match item {
                // New Tab / New Workspace are the creation group; Command Palette
                // / Settings sit below a separator.
                ContextMenuItem::NewTab | ContextMenuItem::NewWorkspace => 0,
                ContextMenuItem::CommandPalette | ContextMenuItem::Settings => 1,
                _ => 1,
            },
            ContextMenuSurface::WorkspaceSlot(_) => match item {
                // New/Rename group; Close in its own destructive group (one
                // separator before it) — the TabSlot pattern one level up.
                ContextMenuItem::NewWorkspace | ContextMenuItem::RenameWorkspace => 0,
                ContextMenuItem::CloseWorkspace => 1,
                _ => 1,
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
                    items.push(ContextMenuItem::MoveToNextWorkspace);
                }
                items.push(ContextMenuItem::NewWindow);
                return items;
            }
            ContextMenuSurface::TabStripEmpty => {
                return vec![
                    ContextMenuItem::NewTab,
                    ContextMenuItem::NewWorkspace,
                    ContextMenuItem::CommandPalette,
                    ContextMenuItem::Settings,
                ];
            }
            // The workspace rail surfaces carry their own tight compositions
            // (§3.5): a slot offers New/Rename/Close Workspace; the empty rail
            // offers New Workspace only.
            ContextMenuSurface::WorkspaceSlot(_) => {
                return vec![
                    ContextMenuItem::NewWorkspace,
                    ContextMenuItem::RenameWorkspace,
                    ContextMenuItem::CloseWorkspace,
                ];
            }
            ContextMenuSurface::WorkspaceRailEmpty => {
                return vec![ContextMenuItem::NewWorkspace];
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
            // `Close Other Tabs` and `Move to Next Workspace` are tab-scoped:
            // never on content. New/Rename/Close Workspace ARE on content (their
            // own section after Split); Rename/Close target the active workspace.
            .filter(|item| {
                !matches!(
                    item,
                    ContextMenuItem::CloseOtherTabs
                        | ContextMenuItem::MoveToNextWorkspace
                        // F6-W5: the New Local Tab escape is a bound-workspace
                        // tab-menu row only; never on the content surface.
                        | ContextMenuItem::NewLocalTab
                        // ODP-5D: the tab host actions are TabSlot-only.
                        | ContextMenuItem::ConnectToHost
                        | ContextMenuItem::ReplaceTabWithHost
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
        let width = self.menu_width().min(columns.max(1));
        let height = (body_rows + 2).min(rows.max(1));
        let left = self.spawn.column.min(columns.saturating_sub(width));
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
            surface: self.surface.discriminant(),
            has_path_target: self.path_target.is_some(),
            is_image_target: self.is_image_target(),
            is_file_target: self.is_file_target(),
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
            false,
            None,
        );
        m
    }

    /// A multi-pane menu (Close Pane visible), no selection / clipboard.
    fn multipane_menu() -> ContextMenuUi {
        let mut m = ContextMenuUi::new();
        m.open(
            CellPoint { row: 4, column: 7 },
            false,
            false,
            false,
            false,
            None,
            true,
            None,
        );
        m
    }

    /// A synthetic resolved file with a line/col, for the path-present file
    /// section variants. No real filesystem — a fixed `Resolved`.
    fn resolved_file() -> Resolved {
        Resolved {
            abs: "/proj/src/main.rs".to_owned(),
            kind: crate::paths::FsKind::File,
            line: Some(42),
            col: Some(7),
        }
    }

    /// A single-pane menu with a resolved path under the click (file section
    /// visible).
    fn menu_with_path() -> ContextMenuUi {
        let mut m = ContextMenuUi::new();
        m.open(
            CellPoint { row: 4, column: 7 },
            false,
            false,
            false,
            false,
            None,
            false,
            Some(resolved_file()),
        );
        m
    }

    /// A synthetic resolved IMAGE file (C4): drives the "Open in OdyTTY" item.
    fn resolved_image() -> Resolved {
        Resolved {
            abs: "/proj/assets/diagram.png".to_owned(),
            kind: crate::paths::FsKind::File,
            line: None,
            col: None,
        }
    }

    /// A single-pane menu with a resolved image path under the click (the full
    /// file section, including "Open in OdyTTY", is visible).
    fn menu_with_image() -> ContextMenuUi {
        let mut m = ContextMenuUi::new();
        m.open(
            CellPoint { row: 4, column: 7 },
            false,
            false,
            false,
            false,
            None,
            false,
            Some(resolved_image()),
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
            false,
            None,
        );
        let rect = m.rect(40, 20);
        assert!(rect.left + rect.width <= 40);
        assert!(rect.top + rect.height <= 20);
    }

    #[test]
    fn rect_tracks_spawn_when_it_fits() {
        let m = menu(true, true);
        // The 25-row single-pane menu (27 rows with borders) needs a grid tall
        // enough to host it at the spawn row without clamping upward.
        let rect = m.rect(80, 34);
        assert_eq!(rect.left, 7);
        assert_eq!(rect.top, 4);
        assert_eq!(rect.body_top, 5);
        assert_eq!(rect.body_height, CONTEXT_MENU_BODY_ROWS);
    }

    #[test]
    fn tall_window_has_no_scroll_and_full_body() {
        // The fits-on-screen case: the whole body is visible, the scroll offset
        // is zero, and the layout is byte-identical to the pre-scroll menu.
        let m = menu(true, true);
        let rect = m.rect(80, 34);
        assert_eq!(
            rect.body_height,
            m.body_row_count(),
            "full body visible when it fits"
        );
        assert_eq!(
            m.scroll_offset(rect.body_height),
            0,
            "no scroll when the menu fits"
        );
    }

    #[test]
    fn rect_clamps_body_window_to_short_window() {
        // A window too short for the 14-row body: the body window is clamped to
        // the box interior and never overflows past the bottom border / window.
        let m = menu(true, true);
        let rect = m.rect(40, 8);
        assert!(rect.top + rect.height <= 8, "box stays on screen");
        assert!(
            rect.body_height <= rect.height.saturating_sub(2),
            "body window fits inside the top/bottom borders"
        );
        assert!(
            rect.body_top + rect.body_height < rect.top + rect.height,
            "body window must not overflow into/below the bottom border"
        );
        assert!(
            rect.body_height < m.body_row_count(),
            "the window really is shorter than the menu"
        );
    }

    #[test]
    fn focus_down_scrolls_last_item_into_view() {
        // Walking focus down to the last item must scroll the window so the last
        // item becomes visible and reachable by both keyboard and pointer.
        let mut m = menu(true, true);
        let rect = m.rect(40, 8);
        let body_height = rect.body_height;
        assert_eq!(
            m.scroll_offset(body_height),
            0,
            "starts unscrolled at item 0"
        );

        for _ in 0..(m.item_count() - 1) {
            m.handle_input(OverlayInput::Down);
        }
        assert_eq!(m.focused, m.item_count() - 1, "focus reached the last item");

        let scroll = m.scroll_offset(body_height);
        let last_body_row = m.focused_body_row();
        assert!(
            scroll <= last_body_row && last_body_row < scroll + body_height,
            "focused last item must sit inside the visible window \
             (scroll={scroll}, last_body_row={last_body_row}, body_height={body_height})"
        );

        // A press on the visible row holding the last item activates it. The
        // last visible single-pane item is now Detach & switch (appended to the
        // launcher section after Manage Sessions in Packet 2).
        let visible_row = last_body_row - scroll;
        assert_eq!(
            m.handle_press(visible_row, body_height, PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::DetachSwitch),
            "last item reachable via the scrolled visible window"
        );
    }

    #[test]
    fn focus_cycles_with_wrap() {
        let mut m = menu(true, true);
        assert_eq!(m.focused, 0);
        m.handle_input(OverlayInput::Up);
        // Wraps from 0 to the last *visible* item (Detach & switch). With a
        // selection the single-pane content menu shows 21 items: Copy/Cut/Paste/
        // Delete/Select All, New Tab/New Window/Close Tab (Rename Tab dropped),
        // the two splits, New/Rename/Close Workspace + Bind to Host (unbound),
        // Settings, and the six launcher items (Close Pane hidden single-pane).
        assert_eq!(m.focused, m.item_count() - 1);
        assert_eq!(m.item_count(), 21);
        m.handle_input(OverlayInput::Down);
        assert_eq!(m.focused, 0);
        m.handle_input(OverlayInput::Down);
        assert_eq!(m.focused, 1);
    }

    #[test]
    fn copy_cut_delete_hidden_without_selection() {
        // ODP-5: with no selection the content menu omits Copy/Cut/Delete
        // entirely (rather than showing them dim); Paste/Select All remain the
        // editing anchors, so Paste leads the menu at body row 0.
        let m = menu(false, true);
        let rows = m.rows();
        for label in ["Copy Text", "Cut", "Delete"] {
            assert!(
                !rows.iter().any(|r| matches!(
                    r,
                    ContextMenuRow::Item { label: l, .. } if *l == label
                )),
                "{label} must be hidden with no selection"
            );
        }
        assert_eq!(rows[0], item("Paste Text", true, true));
        assert_eq!(rows[1], item("Select All", false, true));
    }

    #[test]
    fn copy_present_and_hoisted_with_selection() {
        // ODP-8: a selection surfaces Copy at the very top of the content menu.
        let m = menu(true, false);
        let rows = m.rows();
        assert_eq!(rows[0], item("Copy Text", true, true));
        assert_eq!(rows[1], item("Cut", false, true));
    }

    #[test]
    fn paste_disabled_swallows_activation() {
        let mut m = menu(true, false);
        // Paste is at item index 2 → body row 2.
        assert_eq!(
            m.handle_press(2, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
    }

    #[test]
    fn select_all_always_activates() {
        let mut m = menu(false, false);
        // With no selection Copy/Cut/Delete are hidden, so Select All sits at
        // item index 1 → body row 1 (right after Paste).
        assert_eq!(
            m.handle_press(1, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::SelectAll)
        );
    }

    #[test]
    fn content_menu_tab_actions_and_no_rename_row() {
        // F7: the content menu drops the always-disabled Rename Tab row (it only
        // has a target on a tab right-click). With no selection the tab-actions
        // section sits at body rows 3-5: New Tab / New Window / Close Tab.
        let mut m = menu(false, false);
        let rows = m.rows();
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Rename Tab",
                    ..
                }
            )),
            "Rename Tab must not appear on the content menu"
        );
        assert_eq!(rows[3], item("New Tab", false, true));
        assert_eq!(rows[4], item("New Window", false, true));
        assert_eq!(rows[5], item("Close Tab", false, true));
        assert_eq!(
            m.handle_press(3, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::NewTab)
        );
        assert_eq!(
            m.handle_press(4, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::NewWindow)
        );
        assert_eq!(
            m.handle_press(5, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::CloseTab)
        );
    }

    #[test]
    fn split_items_always_activate() {
        let mut m = menu(false, false);
        // With no selection the splits sit at body rows 7 (Split Right) and 8
        // (Split Down): Paste/Select All · New Tab/New Window/Close Tab · splits.
        assert_eq!(
            m.handle_press(7, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::SplitColumns)
        );
        assert_eq!(
            m.handle_press(8, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::SplitRows)
        );
    }

    #[test]
    fn settings_always_activates() {
        let mut m = menu(false, false);
        // With no selection Settings sits at body row 15 (2 editing anchors +
        // sep + 3 tab actions + sep + 2 splits + sep + 4 workspace (incl. Bind
        // to Host) + sep = 15).
        assert_eq!(
            m.handle_press(15, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::Settings)
        );
    }

    #[test]
    fn single_pane_menu_hides_close_pane() {
        // Single-pane, no selection: Close Pane is absent; Copy/Cut/Delete are
        // hidden (no selection) and Rename Tab is dropped, so the content menu is
        // 18 items / 23 body rows — Paste/Select All · New Tab/New Window/Close
        // Tab · the two splits · New/Rename/Close Workspace + Bind to Host
        // (unbound) · Settings · the six launcher items.
        let m = menu(false, false);
        assert_eq!(m.item_count(), 18);
        let rows = m.rows();
        assert_eq!(rows.len(), 23);
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Close Pane",
                    ..
                }
            )),
            "Close Pane must not appear in a single-pane menu"
        );
        assert_eq!(rows[3], item("New Tab", false, true));
        assert_eq!(rows[4], item("New Window", false, true));
        assert_eq!(rows[5], item("Close Tab", false, true));
        assert_eq!(rows[10], item("New Workspace", false, true));
        assert_eq!(rows[11], item("Rename Workspace", false, true));
        assert_eq!(rows[12], item("Close Workspace", false, true));
        assert_eq!(
            rows[13],
            item("Bind to Host\u{2026}", false, true),
            "an unbound workspace shows Bind to Host in the workspace section"
        );
        assert_eq!(
            rows[15],
            item("Settings", false, true),
            "Settings sits at body row 15 (no selection, workspace + bind added)"
        );
        assert_eq!(rows[16], ContextMenuRow::Separator);
        assert_eq!(rows[17], item("Keyboard Shortcuts", false, true));
        assert_eq!(rows[18], item("Connection Manager", false, true));
        assert_eq!(rows[19], item("Command Palette", false, true));
        assert_eq!(rows[20], item("Session Replay", false, true));
        assert_eq!(rows[21], item("Manage Sessions", false, true));
        assert_eq!(rows[22], item("Detach & switch", false, true));
    }

    #[test]
    fn no_path_menu_hides_the_file_section() {
        // C3: with no resolved path under the click, the four file items are
        // absent and the layout is the 23-row single-pane content menu (no
        // selection). This is the no-file-section guarantee.
        let m = menu(false, false);
        assert_eq!(m.item_count(), 18);
        let rows = m.rows();
        assert_eq!(rows.len(), 23);
        for label in ["Open", "Copy Path", "Copy File", "Reveal in File Manager"] {
            assert!(
                !rows.iter().any(|r| matches!(
                    r,
                    ContextMenuRow::Item { label: l, .. } if *l == label
                )),
                "file item {label} must be absent with no path target"
            );
        }
    }

    /// A single-pane content menu with the active workspace bound to a host
    /// (ODP-6B): the workspace section shows Unbind from Host instead of Bind.
    fn menu_bound_workspace() -> ContextMenuUi {
        let mut m = ContextMenuUi::new();
        m.open_with_prompt_editing_hint(
            CellPoint { row: 4, column: 7 },
            true,
            true,
            true,
            true,
            false,
            None,
            false,
            false,
            false,
            true, // bound_workspace
            ContextMenuSurface::Content,
            None,
        );
        m
    }

    #[test]
    fn workspace_bind_toggle_is_conditional_on_bind_state() {
        // ODP-6B: exactly one of the bind/unbind pair shows on the content menu,
        // keyed to whether the active workspace is bound.
        let unbound = menu(true, true);
        let unbound_rows = unbound.rows();
        assert!(
            unbound_rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Bind to Host\u{2026}",
                    ..
                }
            )),
            "an unbound workspace shows Bind to Host"
        );
        assert!(
            !unbound_rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Unbind from Host",
                    ..
                }
            )),
            "an unbound workspace hides Unbind"
        );

        let bound = menu_bound_workspace();
        let bound_rows = bound.rows();
        assert!(
            bound_rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Unbind from Host",
                    ..
                }
            )),
            "a bound workspace shows Unbind from Host"
        );
        assert!(
            !bound_rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Bind to Host\u{2026}",
                    ..
                }
            )),
            "a bound workspace hides Bind to Host"
        );
        // The pair swaps 1-for-1, so the item count is unchanged either way.
        assert_eq!(unbound.item_count(), bound.item_count());
    }

    #[test]
    fn path_present_menu_appends_the_file_section() {
        // UX-B: a resolved file shows only path actions plus pinned terminal
        // Copy/Paste text actions. Global tab/split/settings rows are filtered
        // out; a separator reflows between the file section and text actions.
        let m = menu_with_path();
        assert_eq!(m.item_count(), 7, "5 file items + 2 pinned text items");
        let rows = m.rows();
        assert_eq!(rows.len(), 8, "5 file rows + separator + 2 text rows");
        assert_eq!(rows[0], item("Open", true, true));
        assert_eq!(rows[1], item("Open With\u{2026}", false, true));
        assert_eq!(rows[2], item("Copy Path", false, true));
        assert_eq!(rows[3], item("Copy File", false, true));
        assert_eq!(rows[4], item("Reveal in File Manager", false, true));
        assert_eq!(rows[5], ContextMenuRow::Separator);
        assert_eq!(rows[6], item("Copy Text", false, false));
        assert_eq!(rows[7], item("Paste Text", false, false));
        for label in [
            "Cut",
            "Delete",
            "Select All",
            "New Tab",
            "Rename Tab",
            "Close Tab",
            "Split Right",
            "Split Down",
            "Settings",
            "Connection Manager",
            "Command Palette",
            "Session Replay",
            "Manage Sessions",
        ] {
            assert!(
                !rows.iter().any(|r| matches!(
                    r,
                    ContextMenuRow::Item { label: l, .. } if *l == label
                )),
                "global item {label} must be absent in a path-scoped menu"
            );
        }
    }

    #[test]
    fn non_image_path_hides_open_in_odytty() {
        // C4: a resolved *non-image* file shows the path-scoped file section
        // but NOT "Open in OdyTTY".
        let m = menu_with_path();
        assert_eq!(
            m.item_count(),
            7,
            "5 file/text items (incl. Open With…), no C4 item"
        );
        let rows = m.rows();
        assert_eq!(rows.len(), 8);
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Open in OdyTTY",
                    ..
                }
            )),
            "Open in OdyTTY must be absent on a non-image path"
        );
    }

    #[test]
    fn image_path_shows_open_in_odytty_after_open() {
        // C4: a resolved image span adds "Open in OdyTTY" right after "Open" in
        // the path-scoped file section. "Open With…" follows "Open in OdyTTY".
        let m = menu_with_image();
        assert_eq!(
            m.item_count(),
            8,
            "6 file items (incl. C4 + C3b) + 2 pinned text items"
        );
        let rows = m.rows();
        assert_eq!(rows.len(), 9, "6 file rows + separator + 2 text rows");
        assert_eq!(rows[0], item("Open", true, true));
        assert_eq!(rows[1], item("Open in OdyTTY", false, true));
        assert_eq!(rows[2], item("Open With\u{2026}", false, true));
        assert_eq!(rows[3], item("Copy Path", false, true));
        assert_eq!(rows[4], item("Copy File", false, true));
        assert_eq!(rows[5], item("Reveal in File Manager", false, true));
        assert_eq!(rows[6], ContextMenuRow::Separator);
        assert_eq!(rows[7], item("Copy Text", false, false));
        assert_eq!(rows[8], item("Paste Text", false, false));
    }

    #[test]
    fn open_in_odytty_activates_on_press_for_an_image() {
        let mut m = menu_with_image();
        // "Open in OdyTTY" sits at body row 1.
        assert_eq!(
            m.handle_press(1, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::OpenInOdytty)
        );
    }

    #[test]
    fn open_with_shows_for_a_file_activates_and_carries_no_accelerator() {
        // C3b: "Open With…" is a file affordance; it activates on press and is
        // pointer-only (no chord).
        let mut m = menu_with_path();
        assert_eq!(
            m.handle_press(1, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::OpenWith)
        );
        assert_eq!(ContextMenuItem::OpenWith.bindable_action(), None);
    }

    #[test]
    fn open_with_hidden_for_a_directory_target() {
        // C3b: a resolved *directory* keeps the C3 copy/reveal items but NOT
        // "Open With…" (a file-only affordance).
        let mut m = ContextMenuUi::new();
        m.open(
            CellPoint { row: 4, column: 7 },
            false,
            false,
            false,
            false,
            None,
            false,
            Some(Resolved {
                abs: "/proj/src".to_owned(),
                kind: crate::paths::FsKind::Dir,
                line: None,
                col: None,
            }),
        );
        let rows = m.rows();
        assert_eq!(
            rows,
            vec![
                item("Open", true, true),
                item("Copy Path", false, true),
                item("Copy File", false, true),
                item("Reveal in File Manager", false, true),
                ContextMenuRow::Separator,
                item("Copy Text", false, false),
                item("Paste Text", false, false),
            ]
        );
        assert!(
            !rows.iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Open With\u{2026}",
                    ..
                }
            )),
            "Open With… must be absent on a directory target"
        );
        // The signature distinguishes file from directory targets.
        assert!(!m.render_signature().is_file_target);
        assert!(menu_with_path().render_signature().is_file_target);
    }

    #[test]
    fn image_target_changes_the_signature() {
        // The C4 item changes the rendered rows, so its presence must alter the
        // render-cache signature (repaint when the item appears).
        let image_sig = menu_with_image().render_signature();
        let file_sig = menu_with_path().render_signature();
        assert_ne!(image_sig, file_sig);
        assert!(image_sig.is_image_target, "image span sets the flag");
        assert!(!file_sig.is_image_target, "a .rs file does not");
        assert!(!menu(false, false).render_signature().is_image_target);
    }

    #[test]
    fn open_in_odytty_carries_no_accelerator() {
        // The C4 item is pointer-only.
        assert_eq!(ContextMenuItem::OpenInOdytty.bindable_action(), None);
    }

    #[test]
    fn file_items_carry_no_accelerator() {
        // The file items are pointer-only; even with accelerators populated for
        // other items, the file rows render a blank accelerator.
        let mut m = menu_with_path();
        let mut accels: [Option<String>; CONTEXT_MENU_ITEMS] = std::array::from_fn(|_| None);
        accels[0] = Some("Ctrl+Shift+C".to_owned());
        m.set_accelerators(accels);
        let rows = m.rows();
        for (body_row, row) in rows.iter().enumerate().take(5) {
            assert!(
                matches!(
                    row,
                    ContextMenuRow::Item {
                        accelerator: None,
                        ..
                    }
                ),
                "file item at body row {body_row} must have no accelerator"
            );
        }
    }

    #[test]
    fn file_items_activate_on_press() {
        let mut m = menu_with_path();
        // Open is at body row 0, then Open With… / Copy Path / Copy File /
        // Reveal (non-image file, so no "Open in OdyTTY").
        assert_eq!(
            m.handle_press(0, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::OpenPath)
        );
        assert_eq!(
            m.handle_press(1, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::OpenWith)
        );
        assert_eq!(
            m.handle_press(2, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::CopyPath)
        );
        assert_eq!(
            m.handle_press(3, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::CopyFile)
        );
        assert_eq!(
            m.handle_press(4, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::RevealPath)
        );
    }

    #[test]
    fn path_target_presence_changes_the_signature() {
        // The file section changes the rendered rows, so its presence must alter
        // the render-cache signature (repaint when the section appears).
        assert_ne!(
            menu(false, false).render_signature(),
            menu_with_path().render_signature()
        );
        assert!(menu_with_path().render_signature().has_path_target);
        assert!(!menu(false, false).render_signature().has_path_target);
    }

    #[test]
    fn path_target_accessor_returns_the_resolved() {
        let m = menu_with_path();
        let target = m.path_target().expect("path target present");
        assert_eq!(target.abs, "/proj/src/main.rs");
        assert_eq!(target.line, Some(42));
        assert!(menu(false, false).path_target().is_none());
    }

    #[test]
    fn multi_pane_menu_shows_close_pane_in_the_split_section() {
        // Multi-pane: Close Pane appears after Split Down in the split/pane
        // section; the workspace section, Settings, and the v0.3.1 launcher
        // section follow below.
        // Multi-pane, no selection: 19 items / 24 body rows. Paste/Select All ·
        // New Tab/New Window/Close Tab · Split Right/Split Down/Close Pane ·
        // New/Rename/Close Workspace + Bind to Host · Settings · six launchers.
        let m = multipane_menu();
        assert_eq!(m.item_count(), 19);
        let rows = m.rows();
        assert_eq!(
            rows.len(),
            24,
            "one more row than the single-pane content menu"
        );
        assert_eq!(rows[7], item("Split Right", false, true));
        assert_eq!(rows[8], item("Split Down", false, true));
        assert_eq!(
            rows[9],
            item("Close Pane", false, true),
            "Close Pane sits at body row 9, alongside the splits"
        );
        assert_eq!(rows[10], ContextMenuRow::Separator);
        assert_eq!(rows[11], item("New Workspace", false, true));
        assert_eq!(rows[12], item("Rename Workspace", false, true));
        assert_eq!(rows[13], item("Close Workspace", false, true));
        assert_eq!(rows[14], item("Bind to Host\u{2026}", false, true));
        assert_eq!(rows[15], ContextMenuRow::Separator);
        assert_eq!(
            rows[16],
            item("Settings", false, true),
            "Settings sits at body row 16 in the multi-pane content menu"
        );
        assert_eq!(rows[17], ContextMenuRow::Separator);
        assert_eq!(rows[18], item("Keyboard Shortcuts", false, true));
        assert_eq!(rows[19], item("Connection Manager", false, true));
        assert_eq!(rows[20], item("Command Palette", false, true));
        assert_eq!(rows[21], item("Session Replay", false, true));
        assert_eq!(rows[22], item("Manage Sessions", false, true));
        assert_eq!(rows[23], item("Detach & switch", false, true));
    }

    #[test]
    fn multi_pane_close_pane_activates_on_press() {
        // Pressing the Close Pane body row (9) activates the item.
        let mut m = multipane_menu();
        assert_eq!(
            m.handle_press(9, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::ClosePane)
        );
    }

    #[test]
    fn multi_pane_focus_wraps_through_all_items() {
        // Up from item 0 wraps to the last visible item (Detach & switch, index
        // 18), proving Close Pane is in the focus cycle only when multi-pane and
        // the workspace + launcher items extend the cycle.
        let mut m = multipane_menu();
        assert_eq!(m.focused, 0);
        m.handle_input(OverlayInput::Up);
        assert_eq!(m.focused, 18);
        assert_eq!(m.item_count(), 19);
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
                m.handle_press(sep, m.body_row_count(), PointerButton::Left),
                ContextMenuOutcome::Consumed
            );
            assert_eq!(m.focused, before, "separator press does not move focus");
        }
    }

    #[test]
    fn hover_skips_separator() {
        let mut m = menu(true, true);
        m.handle_hover(Some(2), m.body_row_count()); // Paste
        assert_eq!(m.focused, 2);
        // Hovering each separator leaves focus unchanged.
        for sep in [
            CONTEXT_MENU_SEPARATOR_ROW,
            CONTEXT_MENU_SECOND_SEPARATOR_ROW,
            CONTEXT_MENU_THIRD_SEPARATOR_ROW,
        ] {
            m.handle_hover(Some(sep), m.body_row_count());
            assert_eq!(m.focused, 2, "separator hover is inert");
        }
        // Hovering Settings (body row 18, item index 14 in the with-selection
        // reference — the workspace section + Bind row now sit above Settings)
        // focuses it.
        m.handle_hover(Some(18), m.body_row_count());
        assert_eq!(m.focused, 14, "hover Settings focuses it");
    }

    #[test]
    fn enabled_items_activate_on_press() {
        let mut m = menu(true, true);
        assert_eq!(
            m.handle_press(0, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::Copy)
        );
        assert_eq!(
            m.handle_press(2, m.body_row_count(), PointerButton::Left),
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
            false,
            None,
        );
        assert_eq!(
            m.handle_press(1, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
        assert_eq!(
            m.handle_press(3, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Consumed
        );
    }

    #[test]
    fn cut_and_delete_disabled_can_carry_shell_integration_hint() {
        let mut m = ContextMenuUi::new();
        m.open_with_prompt_editing_hint(
            CellPoint { row: 4, column: 7 },
            true,
            false,
            true,
            false,
            true,
            None,
            false,
            false,
            false,
            false,
            ContextMenuSurface::Content,
            None,
        );
        let rows = m.rows();
        assert_eq!(
            rows[1],
            ContextMenuRow::Item {
                label: "Cut",
                accelerator: Some(SHELL_INTEGRATION_DISABLED_HINT.to_owned()),
                focused: false,
                enabled: false,
            }
        );
        assert_eq!(
            rows[3],
            ContextMenuRow::Item {
                label: "Delete",
                accelerator: Some(SHELL_INTEGRATION_DISABLED_HINT.to_owned()),
                focused: false,
                enabled: false,
            }
        );
        assert!(m.render_signature().prompt_editing_hint);
    }

    #[test]
    fn press_on_border_row_is_inert() {
        let mut m = menu(true, true);
        // row_in_body == CONTEXT_MENU_BODY_ROWS is past the bottom border.
        assert_eq!(
            m.handle_press(
                CONTEXT_MENU_BODY_ROWS,
                m.body_row_count(),
                PointerButton::Left
            ),
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
        m.handle_hover(Some(2), m.body_row_count());
        assert_eq!(m.focused, 2);
        // Off-item hover leaves focus unchanged.
        m.handle_hover(None, m.body_row_count());
        assert_eq!(m.focused, 2);
        // Row past all body rows is inert.
        m.handle_hover(Some(CONTEXT_MENU_BODY_ROWS + 5), m.body_row_count());
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
        assert_eq!(rows[0], item("Copy Text", true, true));
        assert_eq!(rows[1], item("Cut", false, true));
        assert_eq!(rows[2], item("Paste Text", false, false));
        assert_eq!(rows[3], item("Delete", false, true));
        assert_eq!(rows[4], item("Select All", false, true));
        assert_eq!(rows[5], ContextMenuRow::Separator);
        assert_eq!(rows[6], item("New Tab", false, true));
        assert_eq!(rows[7], item("New Window", false, true));
        // F7: Rename Tab is no longer on the content menu; Close Tab follows
        // New Window directly.
        assert_eq!(rows[8], item("Close Tab", false, true));
        assert_eq!(rows[9], ContextMenuRow::Separator);
        assert_eq!(rows[10], item("Split Right", false, true));
        assert_eq!(rows[11], item("Split Down", false, true));
        assert_eq!(rows[12], ContextMenuRow::Separator);
        // Workspace section sits between Split and Settings; an unbound
        // workspace shows Bind to Host as its last row (ODP-6B).
        assert_eq!(rows[13], item("New Workspace", false, true));
        assert_eq!(rows[14], item("Rename Workspace", false, true));
        assert_eq!(rows[15], item("Close Workspace", false, true));
        assert_eq!(rows[16], item("Bind to Host\u{2026}", false, true));
        assert_eq!(rows[17], ContextMenuRow::Separator);
        assert_eq!(rows[18], item("Settings", false, true));
    }

    #[test]
    fn copy_and_paste_are_labeled_as_text_actions() {
        assert_eq!(ContextMenuItem::Copy.label(), "Copy Text");
        assert_eq!(ContextMenuItem::Paste.label(), "Paste Text");
    }

    #[test]
    fn accelerators_render_and_widen_the_menu() {
        let mut m = menu(true, true);
        let narrow = m.menu_width();
        // Populate a couple of accelerators in ALL order: Copy and Split Right.
        let mut accels: [Option<String>; CONTEXT_MENU_ITEMS] = std::array::from_fn(|_| None);
        accels[0] = Some("Ctrl+Shift+C".to_owned());
        // Split Right is item index 9 in ALL order after F1's New Window (index 6).
        accels[9] = Some("Ctrl+Shift+E".to_owned());
        m.set_accelerators(accels);

        let rows = m.rows();
        assert_eq!(
            rows[0],
            ContextMenuRow::Item {
                label: "Copy Text",
                accelerator: Some("Ctrl+Shift+C".to_owned()),
                focused: true,
                enabled: true,
            }
        );
        // Split Right sits at body row 10 in the with-selection menu and carries
        // its accelerator (accelerators are keyed by ALL order, index 9).
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

    /// Open a menu on a specific surface (F7) with the given tab-count state.
    /// Single-workspace (no `Move to Next Workspace` row); use
    /// [`open_surface_ws`] to exercise the multi-workspace composition.
    fn open_surface(surface: ContextMenuSurface, multi_tab: bool) -> ContextMenuUi {
        open_surface_ws(surface, multi_tab, false)
    }

    /// Open a menu on a specific surface with explicit tab- and workspace-count
    /// state (W4). `multi_workspace` drives the `Move to Next Workspace` row.
    fn open_surface_ws(
        surface: ContextMenuSurface,
        multi_tab: bool,
        multi_workspace: bool,
    ) -> ContextMenuUi {
        let mut m = ContextMenuUi::new();
        m.open_with_prompt_editing_hint(
            CellPoint { row: 4, column: 7 },
            true,
            true,
            true,
            true,
            false,
            match surface {
                ContextMenuSurface::TabSlot(token) => Some(token),
                _ => None,
            },
            false,
            multi_tab,
            multi_workspace,
            false,
            surface,
            None,
        );
        m
    }

    #[test]
    fn tab_slot_menu_is_tab_scoped() {
        // F7: a tab right-click opens a tight, tab-scoped menu — no Copy / Split /
        // Settings / launcher tail. New Tab / Rename Tab · Close Tab / Close Other
        // Tabs · Connect to Host / Replace with Host (ODP-5D) · New Window, split
        // by three separators.
        let m = open_surface(ContextMenuSurface::TabSlot(SessionToken(3)), true);
        let rows = m.rows();
        assert_eq!(
            rows,
            vec![
                item("New Tab", true, true),
                item("Rename Tab", false, true),
                ContextMenuRow::Separator,
                item("Close Tab", false, true),
                item("Close Other Tabs", false, true),
                ContextMenuRow::Separator,
                item("Connect to Host\u{2026}", false, true),
                item("Replace with Host\u{2026}", false, true),
                ContextMenuRow::Separator,
                item("New Window", false, true),
            ]
        );
        for label in [
            "Copy Text",
            "Paste Text",
            "Select All",
            "Split Right",
            "Split Down",
            "Settings",
            "Command Palette",
        ] {
            assert!(
                !rows.iter().any(|r| matches!(
                    r,
                    ContextMenuRow::Item { label: l, .. } if *l == label
                )),
                "tab menu must not contain {label}"
            );
        }
    }

    /// F6-W5: on a workspace bound to a host, the tab menu gains a "New Local
    /// Tab" escape row beside New Tab; an unbound workspace's tab menu does not.
    #[test]
    fn tab_slot_new_local_tab_appears_only_when_workspace_bound() {
        let mut bound = ContextMenuUi::new();
        bound.open_with_prompt_editing_hint(
            CellPoint { row: 0, column: 0 },
            true,
            true,
            true,
            true,
            false,
            Some(SessionToken(3)),
            false,
            true,
            false,
            true,
            ContextMenuSurface::TabSlot(SessionToken(3)),
            None,
        );
        let has_local = |m: &ContextMenuUi| {
            m.rows().iter().any(|r| {
                matches!(
                    r,
                    ContextMenuRow::Item { label, .. } if *label == "New Local Tab"
                )
            })
        };
        assert!(
            has_local(&bound),
            "bound-workspace tab menu offers New Local Tab"
        );

        let unbound = open_surface(ContextMenuSurface::TabSlot(SessionToken(3)), true);
        assert!(
            !has_local(&unbound),
            "unbound-workspace tab menu has no New Local Tab row"
        );
    }

    #[test]
    fn tab_slot_rename_is_enabled_and_targets_the_token() {
        let m = open_surface(ContextMenuSurface::TabSlot(SessionToken(9)), true);
        assert_eq!(m.surface(), ContextMenuSurface::TabSlot(SessionToken(9)));
        assert_eq!(m.rename_target(), Some(SessionToken(9)));
    }

    #[test]
    fn move_to_workspace_row_hidden_with_one_workspace() {
        // ODP-7: no destination workspace ⇒ no move row (single-workspace tab
        // menu is byte-identical to before W4).
        let m = open_surface_ws(ContextMenuSurface::TabSlot(SessionToken(3)), true, false);
        assert!(
            !m.rows().iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Move to Next Workspace",
                    ..
                }
            )),
            "single-workspace tab menu must not offer the move row"
        );
    }

    #[test]
    fn move_to_workspace_row_shown_and_bracketed_with_multiple_workspaces() {
        // ODP-7: with >1 workspace the move row appears between the host-actions
        // group (ODP-5D) and New Window, each in its own section (separators
        // bracket it).
        let m = open_surface_ws(ContextMenuSurface::TabSlot(SessionToken(3)), true, true);
        let rows = m.rows();
        assert_eq!(
            rows,
            vec![
                item("New Tab", true, true),
                item("Rename Tab", false, true),
                ContextMenuRow::Separator,
                item("Close Tab", false, true),
                item("Close Other Tabs", false, true),
                ContextMenuRow::Separator,
                item("Connect to Host\u{2026}", false, true),
                item("Replace with Host\u{2026}", false, true),
                ContextMenuRow::Separator,
                item("Move to Next Workspace", false, true),
                ContextMenuRow::Separator,
                item("New Window", false, true),
            ]
        );
    }

    #[test]
    fn move_to_workspace_row_never_on_the_content_menu() {
        // The move item is tab-scoped: even with multiple workspaces, the grid
        // menu never shows it.
        let m = open_surface_ws(ContextMenuSurface::Content, true, true);
        assert!(
            !m.rows().iter().any(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Move to Next Workspace",
                    ..
                }
            )),
            "content menu must not offer the move row"
        );
    }

    #[test]
    fn tab_slot_close_other_tabs_disabled_at_one_tab() {
        // "Close Other Tabs" is inert when only one tab is open.
        let lone = open_surface(ContextMenuSurface::TabSlot(SessionToken(1)), false);
        let row = lone
            .rows()
            .into_iter()
            .find(|r| {
                matches!(
                    r,
                    ContextMenuRow::Item {
                        label: "Close Other Tabs",
                        ..
                    }
                )
            })
            .expect("Close Other Tabs present");
        assert_eq!(row, item("Close Other Tabs", false, false));
        // With a second tab it activates.
        let many = open_surface(ContextMenuSurface::TabSlot(SessionToken(1)), true);
        assert!(matches!(
            many.rows().into_iter().find(|r| matches!(
                r,
                ContextMenuRow::Item {
                    label: "Close Other Tabs",
                    ..
                }
            )),
            Some(ContextMenuRow::Item { enabled: true, .. })
        ));
    }

    #[test]
    fn tab_slot_close_other_tabs_activates_on_press() {
        let mut m = open_surface(ContextMenuSurface::TabSlot(SessionToken(4)), true);
        // Body rows: New Tab(0) Rename Tab(1) sep(2) Close Tab(3) Close Other
        // Tabs(4) sep(5) New Window(6).
        assert_eq!(
            m.handle_press(3, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::CloseTab)
        );
        let mut m = open_surface(ContextMenuSurface::TabSlot(SessionToken(4)), true);
        assert_eq!(
            m.handle_press(4, m.body_row_count(), PointerButton::Left),
            ContextMenuOutcome::Activate(ContextMenuItem::CloseOtherTabs)
        );
    }

    #[test]
    fn tab_strip_empty_menu_is_minimal() {
        // F7: an empty-strip right-click opens New Tab · New Workspace ·
        // Command Palette · Settings, with one separator between the creation
        // group (New Tab / New Workspace) and the two launchers.
        let m = open_surface(ContextMenuSurface::TabStripEmpty, true);
        assert_eq!(
            m.rows(),
            vec![
                item("New Tab", true, true),
                item("New Workspace", false, true),
                ContextMenuRow::Separator,
                item("Command Palette", false, true),
                item("Settings", false, true),
            ]
        );
    }

    #[test]
    fn workspace_slot_menu_offers_workspace_actions() {
        // §3.5 / §7.4: a workspace-slot right-click opens New / Rename / Close
        // Workspace, with a separator before the destructive Close.
        let m = open_surface(ContextMenuSurface::WorkspaceSlot(0), true);
        assert_eq!(
            m.rows(),
            vec![
                item("New Workspace", true, true),
                item("Rename Workspace", false, true),
                ContextMenuRow::Separator,
                item("Close Workspace", false, true),
            ]
        );
    }

    #[test]
    fn workspace_rail_empty_menu_is_new_workspace_only() {
        // §3.5: the empty rail area offers New Workspace only.
        let m = open_surface(ContextMenuSurface::WorkspaceRailEmpty, true);
        assert_eq!(m.rows(), vec![item("New Workspace", true, true)]);
    }

    #[test]
    fn workspace_actions_appear_on_the_content_menu() {
        // The content menu carries a workspace section (New / Rename / Close
        // Workspace) after the split section, before Settings. Rename/Close
        // target the active workspace (the content surface has no per-workspace
        // click target).
        let m = open_surface(ContextMenuSurface::Content, true);
        let labels: Vec<&str> = m
            .rows()
            .into_iter()
            .filter_map(|row| match row {
                ContextMenuRow::Item { label, .. } => Some(label),
                ContextMenuRow::Separator => None,
            })
            .collect();
        for expected in ["New Workspace", "Rename Workspace", "Close Workspace"] {
            assert!(
                labels.contains(&expected),
                "content menu must contain {expected:?}"
            );
        }
        // They sit after the split section and before Settings.
        let split = labels.iter().position(|l| *l == "Split Right").unwrap();
        let new_ws = labels.iter().position(|l| *l == "New Workspace").unwrap();
        let settings = labels.iter().position(|l| *l == "Settings").unwrap();
        assert!(
            split < new_ws && new_ws < settings,
            "workspace section sits between Split and Settings"
        );
    }

    #[test]
    fn surface_change_repaints_the_menu() {
        // Different surfaces swap the whole composition, so their render-cache
        // signatures must differ (a surface change repaints).
        let content = open_surface(ContextMenuSurface::Content, true).render_signature();
        let tab =
            open_surface(ContextMenuSurface::TabSlot(SessionToken(1)), true).render_signature();
        let empty = open_surface(ContextMenuSurface::TabStripEmpty, true).render_signature();
        assert_ne!(content, tab);
        assert_ne!(content, empty);
        assert_ne!(tab, empty);
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
        // The separator rows map to no item.
        assert_eq!(body_row_to_item(CONTEXT_MENU_SEPARATOR_ROW), None);
        assert_eq!(body_row_to_item(CONTEXT_MENU_SECOND_SEPARATOR_ROW), None);
        assert_eq!(body_row_to_item(CONTEXT_MENU_THIRD_SEPARATOR_ROW), None);
        assert_eq!(body_row_to_item(CONTEXT_MENU_FOURTH_SEPARATOR_ROW), None);
        assert_eq!(body_row_to_item(CONTEXT_MENU_FIFTH_SEPARATOR_ROW), None);
        // Tab actions (5-7: New Tab / New Window / Close Tab — Rename Tab dropped)
        // shift past the first separator.
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
        // Workspace actions (10-13: New / Rename / Close + Bind to Host) shift
        // past the first three separators.
        assert_eq!(item_to_body_row(10), 13);
        assert_eq!(body_row_to_item(13), Some(10));
        assert_eq!(item_to_body_row(13), 16);
        assert_eq!(body_row_to_item(16), Some(13));
        // Settings (14) shifts past the first four separators.
        assert_eq!(item_to_body_row(14), 18);
        assert_eq!(body_row_to_item(18), Some(14));
        // A launcher item (15) shifts past all five separators.
        assert_eq!(item_to_body_row(15), 20);
        assert_eq!(body_row_to_item(20), Some(15));
    }
}

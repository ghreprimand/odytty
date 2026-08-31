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

use super::session::SessionToken;
use crate::connection_hosts::{ConnectionHost, ConnectionHostSource};
use crate::paths::Resolved;
use crate::selection::CellPoint;

mod actions;
mod coordination;
mod input;
mod layout;
mod rendering;

pub(super) use actions::ContextMenuItem;
pub(super) use rendering::humanize_chord;

#[cfg(test)]
use super::overlay::{OverlayInput, PointerButton};
#[cfg(test)]
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
pub(super) const CONTEXT_MENU_ITEMS: usize = 55;

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
    command_actions_enabled: bool,
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

#[cfg(test)]
#[path = "context_menu_ui_tests.rs"]
mod tests;

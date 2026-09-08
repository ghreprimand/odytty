// SPDX-License-Identifier: GPL-3.0-only
//! Overlay geometry: the single rectangle calculation shared by drawing and
//! hit-testing, plus the fixed dialog widths and the static dialog lines those
//! widths are sized for.
//!
//! The dialog width constants live beside their action lines because each width
//! is chosen to fit the longest static line of the same dialog; separating them
//! would let one drift without the other.

use crate::selection::CellPoint;

use super::contracts::OverlayMode;
use super::state::OverlayUi;

/// Geometry of the open overlay panel, in terminal cells. The single source of
/// truth shared by rendering ([`apply_overlay`]) and pointer hit-testing
/// ([`overlay_rect`]) so a resize can never desync the two. Computed on demand
/// from the current grid dimensions; never cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct OverlayRect {
    /// Outer panel box (includes border + title row).
    pub(in crate::native) left: usize,
    pub(in crate::native) top: usize,
    pub(in crate::native) width: usize,
    pub(in crate::native) height: usize,
    /// First body cell (inside the border, below the title).
    pub(in crate::native) body_left: usize,
    pub(in crate::native) body_top: usize,
    /// Body content extent (matches the args passed to `visible_lines`).
    pub(in crate::native) body_width: usize,
    pub(in crate::native) body_height: usize,
}

impl OverlayRect {
    /// Whether a grid cell falls inside the outer panel box.
    pub(in crate::native) fn contains(&self, cell: CellPoint) -> bool {
        cell.row >= self.top
            && cell.row < self.top + self.height
            && cell.column >= self.left
            && cell.column < self.left + self.width
    }
}

/// Compute the open overlay's cell geometry for a grid of `columns`×`rows`, or
/// `None` when the overlay is closed or the grid is empty. The math is the exact
/// rect [`apply_overlay`] draws into, so render and hit-test stay in lockstep.
/// Fixed body width (cells) for the close-confirmation dialog (CLOSE-CONFIRM).
/// Wide enough for the longest static line plus the panel border inset; the
/// `.max(36)` floor in [`overlay_rect`] keeps small grids sane.
pub(super) const CONFIRM_CLOSE_WIDTH: usize = 52;

pub(super) const RISKY_PASTE_WIDTH: usize = 78;
pub(super) const RISKY_PASTE_ACTION_LINE: &str =
    "Choose: [Enter / P] Paste   [O] Paste as One Line   [Esc / C] Cancel";
pub(super) const RISKY_PASTE_ACTION_LINE_NO_ONE_LINE: &str =
    "Choose: [Enter / P] Paste   [Esc / C] Cancel";

/// The close-confirmation dialog's action line, shared by the body builder and
/// the click hit-test ([`OverlayUi::confirm_close_click`]) so the two can never
/// drift. The `[Enter` and `[Esc` bracket tokens anchor the Yes / No regions.
pub(super) const CONFIRM_CLOSE_ACTION_LINE: &str =
    "Close anyway?   [Enter / Y] Yes     [Esc / N] No";

/// Fixed body width (cells) for the attach-choice dialog (Phase 14). Sized for
/// the longest static line plus the panel border inset; the `.max(36)` floor in
/// [`overlay_rect`] keeps small grids sane.
pub(super) const ATTACH_CHOICE_WIDTH: usize = 52;

/// The attach-choice dialog's action line, shared by the body builder and the
/// click hit-test ([`OverlayUi::attach_choice_click`]) so the two can never
/// drift. A leading prompt gives an inert region at col 0 (a stray click there
/// never attaches, mirroring the ConfirmClose guard); the `[N` and `[R` bracket
/// tokens anchor the New-tab / Replace regions.
pub(super) const ATTACH_CHOICE_ACTION_LINE: &str = "Open where?  [N / Enter] New tab   [R] Replace";

/// Fixed body width (cells) for the kill-confirmation dialog (Manage Sessions).
/// Sized for the longest static line plus the panel border inset; the `.max(36)`
/// floor in [`overlay_rect`] keeps small grids sane.
pub(super) const CONFIRM_KILL_SESSION_WIDTH: usize = 52;

/// The kill-confirmation dialog's action line, shared by the body builder and
/// the click hit-test ([`OverlayUi::confirm_kill_session_click`]) so the two can
/// never drift. A leading prompt gives an inert region at col 0 (a stray click
/// there never kills, mirroring the ConfirmClose guard); the `[Enter` and `[Esc`
/// bracket tokens anchor the Kill / Cancel regions.
pub(super) const CONFIRM_KILL_SESSION_ACTION_LINE: &str =
    "Kill it?   [Enter / Y] Kill     [Esc / N] Cancel";

pub(super) const CONFIRM_NAVIGATOR_CLOSE_ACTION_LINE: &str =
    "Close it?  [Enter / Y] Close   [Esc / N] Cancel";

/// Fixed body width (cells) for the Detach & switch dialog. Sized for
/// the action line plus the panel border inset; the `.max(36)` floor in
/// [`overlay_rect`] keeps small grids sane.
pub(super) const DETACH_SWITCH_WIDTH: usize = 64;

/// The Detach & switch dialog's action line, shared by the body builder and the
/// click hit-test ([`OverlayUi::detach_switch_click`]) so the two can never
/// drift. A leading prompt gives an inert region at col 0 (a stray click there
/// never spawns or closes a pane); the `[S`, `[K`, and `[Esc` bracket tokens
/// anchor the Swap / Keep-both / Cancel regions (ordered S < K < Esc, so the
/// click hit-test scans them right-to-left).
pub(super) const DETACH_SWITCH_ACTION_LINE: &str =
    "Swap closes this.  [S] Swap   [K] Keep both   [Esc] Cancel";

/// Fixed body width (cells) for the replace-tab confirm dialog (ODP-5D). Sized
/// so the body (`width - 4` in [`overlay_rect`]) is at least as wide as the
/// 54-column action line, which the non-wrapping `write_text` would otherwise
/// truncate; the `.max(36)` floor in [`overlay_rect`] keeps small grids sane.
pub(super) const CONFIRM_REPLACE_TAB_WIDTH: usize = 58;

/// The replace-tab confirm dialog's action line, shared by the body builder and
/// the click hit-test ([`OverlayUi::confirm_replace_tab_click`]) so the two can
/// never drift. A leading prompt gives an inert region at col 0 (a stray click
/// there never destroys the shell, mirroring the ConfirmClose guard); the
/// `[Enter` and `[Esc` bracket tokens anchor the Replace / Cancel regions.
pub(super) const CONFIRM_REPLACE_TAB_ACTION_LINE: &str =
    "Replace it?   [Enter / Y] Replace     [Esc / N] Cancel";

/// Fixed body width (cells) for the remove-host confirm dialog (ODP-2C). Sized
/// for the longest static line plus the panel border inset; the `.max(36)` floor
/// in [`overlay_rect`] keeps small grids sane.
pub(super) const CONFIRM_REMOVE_HOST_WIDTH: usize = 56;

/// The remove-host confirm dialog's action line, shared by the body builder and
/// the click hit-test ([`OverlayUi::confirm_remove_host_click`]) so the two can
/// never drift. A leading prompt gives an inert region at col 0 (a stray click
/// there never deletes the host, mirroring the ConfirmClose guard); the `[Enter`
/// and `[Esc` bracket tokens anchor the Remove / Cancel regions.
pub(super) const CONFIRM_REMOVE_HOST_ACTION_LINE: &str =
    "Remove it?   [Enter / Y] Remove     [Esc / N] Cancel";

/// Fixed body width (cells) for the overwrite-layout confirm dialog
/// (OVERWRITE-WARN). Sized for the three-way action line plus the panel border
/// inset; the `.max(36)` floor in [`overlay_rect`] keeps small grids sane.
pub(super) const CONFIRM_OVERWRITE_LAYOUT_WIDTH: usize = 62;

/// The overwrite-layout confirm dialog's three-way action line, shared by the
/// body builder and the click hit-test
/// ([`OverlayUi::confirm_overwrite_layout_click`]) so the two can never drift. A
/// leading prompt gives an inert region at col 0 (a stray click there neither
/// overwrites nor reopens the prompt); the `[Enter`, `[R]` and `[Esc` bracket
/// tokens anchor the Replace / Rename / Cancel regions, in that column order.
pub(super) const CONFIRM_OVERWRITE_LAYOUT_ACTION_LINE: &str =
    "Overwrite?   [Enter] Replace   [R] Rename   [Esc] Cancel";

/// Fixed body width (cells) for the open-layout mode dialog (LAYOUT-OPEN-MODE).
/// Sized so the body (`width - 4` in [`overlay_rect`]) is at least as wide as
/// the 61-column three-way action line, which the non-wrapping `write_text`
/// would otherwise truncate; the `.max(36)` floor in [`overlay_rect`] keeps
/// small grids sane.
pub(super) const CONFIRM_OPEN_LAYOUT_WIDTH: usize = 66;

/// The open-layout mode dialog's three-way action line, shared by the body
/// builder and the click hit-test ([`OverlayUi::confirm_open_layout_click`]) so
/// the two can never drift. A leading prompt gives an inert region at col 0 (a
/// stray click there neither replaces nor appends); the `[Enter`, `[A]` and
/// `[Esc` bracket tokens anchor the Replace / Add / Cancel regions, in that
/// column order.
pub(super) const CONFIRM_OPEN_LAYOUT_ACTION_LINE: &str =
    "Open onto current?   [Enter] Replace   [A] Add   [Esc] Cancel";

pub(in crate::native) fn overlay_rect(
    overlay: &OverlayUi,
    columns: usize,
    rows: usize,
) -> Option<OverlayRect> {
    if !overlay.open || rows == 0 || columns == 0 {
        return None;
    }
    // The context menu spawns at the pointer cell (not centered) and is sized to
    // its three items, so it bypasses the centered-panel geometry below (IN2).
    if overlay.mode == OverlayMode::ContextMenu {
        return Some(overlay.context_menu.rect(columns, rows));
    }
    let width = match overlay.mode {
        OverlayMode::Settings => overlay.panel.desired_width(columns),
        OverlayMode::ThemePicker => overlay.theme_picker.desired_width(columns),
        OverlayMode::ThemeBuilder => overlay.theme_builder.desired_width(columns),
        OverlayMode::ProfileManager => overlay.profile_manager.desired_width(columns),
        OverlayMode::FontPicker => overlay.font_picker.desired_width(columns),
        OverlayMode::KeyBindings => overlay.key_remap.desired_width(columns),
        OverlayMode::Onboarding => overlay.onboarding.desired_width(columns),
        OverlayMode::CommandPalette => overlay.command_palette.desired_width(columns),
        OverlayMode::Replay => overlay.replay.desired_width(columns),
        OverlayMode::Connections => overlay.connections.desired_width(columns),
        OverlayMode::ConnectionForm => overlay.connection_form.desired_width(columns),
        OverlayMode::SessionAttach => overlay.session_attach.desired_width(columns),
        OverlayMode::OpenWith => overlay.open_with.desired_width(columns),
        OverlayMode::WorkspacePicker => overlay.workspace_picker.desired_width(columns),
        OverlayMode::ProfilePicker => overlay.profile_picker.desired_width(columns),
        // The image viewer (C4) uses a full-width backdrop panel; the decoded
        // image is drawn centered over it by the GPU image layer, so the panel
        // is just the dimmed frame behind the picture.
        OverlayMode::ImageView => columns,
        // Unreachable: handled by the early return above.
        OverlayMode::ContextMenu => overlay.context_menu.menu_width(),
        // Static two-line dialog; the `.max(36)` floor below gives it room and
        // the body text fits comfortably (CLOSE-CONFIRM).
        OverlayMode::ConfirmClose => CONFIRM_CLOSE_WIDTH,
        OverlayMode::RiskyPaste => RISKY_PASTE_WIDTH,
        // Static choice dialog (Phase 14); same fixed-width/floor treatment.
        OverlayMode::AttachChoice => ATTACH_CHOICE_WIDTH,
        // Static kill-confirmation dialog (Manage Sessions); same treatment.
        OverlayMode::ConfirmKillSession => CONFIRM_KILL_SESSION_WIDTH,
        OverlayMode::ConfirmNavigatorClose => CONFIRM_KILL_SESSION_WIDTH,
        // Static Detach & switch choice dialog; same treatment.
        OverlayMode::DetachSwitchChoice => DETACH_SWITCH_WIDTH,
        // Static replace-tab confirm dialog (ODP-5D); same treatment.
        OverlayMode::ConfirmReplaceTab => CONFIRM_REPLACE_TAB_WIDTH,
        // Static remove-host confirm dialog (ODP-2C); same treatment.
        OverlayMode::ConfirmRemoveHost => CONFIRM_REMOVE_HOST_WIDTH,
        // Static overwrite-layout confirm dialog (OVERWRITE-WARN); same treatment.
        OverlayMode::ConfirmOverwriteLayout => CONFIRM_OVERWRITE_LAYOUT_WIDTH,
        // Static open-layout mode dialog (LAYOUT-OPEN-MODE); same treatment.
        OverlayMode::ConfirmOpenLayout => CONFIRM_OPEN_LAYOUT_WIDTH,
    }
    .max(36)
    .min(columns);
    // Target ~80 % of rows so the panel is tall enough to show many settings
    // at once; still capped at `rows - 2` to leave at least one terminal row
    // above/below, and floored at 22 for small terminals (OVERLAY-SIZE).
    let height = (rows * 4 / 5).max(22).min(rows.saturating_sub(2)).max(1);
    let left = (columns - width) / 2;
    let top = (rows.saturating_sub(height)) / 2;
    Some(OverlayRect {
        left,
        top,
        width,
        height,
        body_left: left + 2,
        body_top: top + 2,
        body_width: width.saturating_sub(4),
        body_height: height.saturating_sub(3),
    })
}

/// The rect the MULTI-PANE render path ([`build_overlay_top`]) must crop and
/// composite to. For a menu-over-overlay context menu (connection-row or
/// navigator-row) this is the UNION of the still-loaded underlay panel and the
/// small menu box, so the underlay panel survives the crop; for every other
/// overlay it is the plain [`overlay_rect`].
///
/// Why this exists: the single-pane path paints the overlay onto the full
/// terminal snapshot and never crops, so an underlay painted by [`apply_overlay`]
/// survives there. The multi-pane path instead crops the overlay snapshot to
/// `overlay_rect`, which for a `ContextMenu` returns ONLY the small menu box -
/// discarding every underlay cell outside it and making the navigator (or
/// connection manager) vanish the instant its row menu opened. Cropping to the
/// union keeps both render paths visually identical. Hit-testing is unaffected:
/// pointer routing still uses `overlay_rect` (the menu box), so menu clicks hit
/// items and clicks on the underlay dismiss the menu.
///
/// Takes `&mut` only to briefly view the overlay AS the underlay mode while its
/// panel rect is computed (the exact pattern `apply_overlay` uses); the mode is
/// always restored before returning.
pub(in crate::native) fn overlay_composite_rect(
    overlay: &mut OverlayUi,
    columns: usize,
    rows: usize,
) -> Option<OverlayRect> {
    let menu_rect = overlay_rect(overlay, columns, rows)?;
    let Some(underlay) = overlay.menu_underlay_mode() else {
        return Some(menu_rect);
    };
    let restore = overlay.mode;
    overlay.mode = underlay;
    let under = overlay_rect(overlay, columns, rows);
    overlay.mode = restore;
    let Some(under) = under else {
        return Some(menu_rect);
    };
    // Union the two outer boxes. The body fields carry the underlay panel's, as
    // the composite crop reads only the outer left/top/width/height.
    let left = under.left.min(menu_rect.left);
    let top = under.top.min(menu_rect.top);
    let right = (under.left + under.width).max(menu_rect.left + menu_rect.width);
    let bottom = (under.top + under.height).max(menu_rect.top + menu_rect.height);
    Some(OverlayRect {
        left,
        top,
        width: right - left,
        height: bottom - top,
        body_left: under.body_left,
        body_top: under.body_top,
        body_width: under.body_width,
        body_height: under.body_height,
    })
}

// SPDX-License-Identifier: GPL-3.0-only
//! Overlay confirmation dialogs and context-menu transitions.
//!
//! Every dialog keeps its key handler and its click hit-test side by side so
//! the two can never drift: click and key parity is a property of this module.

use crate::connection_hosts::ConnectionHost;
use crate::native::context_menu_ui::{CONTEXT_MENU_ITEMS, ContextMenuItem, ContextMenuOutcome};
use crate::native::session::SessionToken;
use crate::selection::CellPoint;

use super::contracts::{
    LayoutSaveKind, OverlayInput, OverlayMode, OverlayOutcome, RiskyPasteDialog, SettingsTarget,
};
use super::layout::*;
use super::state::OverlayUi;

impl OverlayUi {
    /// Open the right-click context menu (IN2) at `spawn` (a grid cell), with
    /// the item-enabled snapshot the App computed from the live selection /
    /// clipboard. Unlike the other openers this does NOT clear the selection —
    /// the Copy item needs it — so the App must not route through
    /// `reset_pointer_state_for_overlay` here.
    // A thin forwarding shim retained for the existing overlay tests, which call
    // the pre-hint 9-arg form. Production opens via
    // `open_context_menu_with_prompt_editing_hint`; this defaults the hint off.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn open_context_menu(
        &mut self,
        spawn: CellPoint,
        copy: bool,
        cut: bool,
        paste: bool,
        delete: bool,
        rename_target: Option<SessionToken>,
        multi_pane: bool,
        path_target: Option<crate::paths::Resolved>,
        accelerators: [Option<String>; CONTEXT_MENU_ITEMS],
    ) {
        self.open_context_menu_with_prompt_editing_hint(
            spawn,
            copy,
            cut,
            paste,
            delete,
            false,
            rename_target,
            multi_pane,
            false,
            false,
            false,
            crate::native::context_menu_ui::ContextMenuSurface::Content,
            path_target,
            accelerators,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn open_context_menu_with_prompt_editing_hint(
        &mut self,
        spawn: CellPoint,
        copy: bool,
        cut: bool,
        paste: bool,
        delete: bool,
        prompt_editing_hint: bool,
        rename_target: Option<SessionToken>,
        multi_pane: bool,
        multi_tab: bool,
        multi_workspace: bool,
        bound_workspace: bool,
        surface: crate::native::context_menu_ui::ContextMenuSurface,
        path_target: Option<crate::paths::Resolved>,
        accelerators: [Option<String>; CONTEXT_MENU_ITEMS],
    ) {
        self.panel.end_slider_drag();
        self.context_menu.open_with_prompt_editing_hint(
            spawn,
            copy,
            cut,
            paste,
            delete,
            prompt_editing_hint,
            rename_target,
            multi_pane,
            multi_tab,
            multi_workspace,
            bound_workspace,
            surface,
            path_target,
        );
        self.context_menu.set_accelerators(accelerators);
        self.mode = OverlayMode::ContextMenu;
        self.open = true;
    }

    /// Reserve `left`/`right` columns the open context menu box must stay clear
    /// of (MENU-Z-ORDER). The App calls this immediately after opening a
    /// rail-anchored menu while the auto-hide rail is revealed, so the box lands
    /// beside the floating rail band rather than under it (the rail composites
    /// topmost). A no-op reserve leaves the layout byte-identical.
    pub(in crate::native) fn set_context_menu_rail_clearance(&mut self, left: usize, right: usize) {
        self.context_menu.set_rail_clearance(left, right);
    }

    /// Snapshot the total workspace count on the open context menu (RAIL-REORDER)
    /// so a `WorkspaceSlot` menu can gate its Move Up/Down rows on the clicked
    /// slot's position. The App calls this right after opening a rail-slot menu.
    pub(in crate::native) fn set_context_menu_workspace_count(&mut self, count: usize) {
        self.context_menu.set_workspace_count(count);
    }

    pub(in crate::native) fn set_context_menu_command_actions_enabled(&mut self, enabled: bool) {
        self.context_menu.set_command_actions_enabled(enabled);
    }

    /// Open the connection-row context menu (ODP-2C) at `spawn` for the row at
    /// filtered index `row_index`, snapshotting `host` so the menu can gate
    /// Edit/Remove (OdyTTY-owned only) and route each of the five actions. This
    /// is the one menu-over-overlay surface: it is spawned from WITHIN the
    /// connection manager, so it does NOT `close()` — only the context-menu
    /// state and the mode change, leaving the `connections` overlay loaded
    /// underneath so dismissing the menu returns to it with selection intact.
    pub(super) fn open_connection_row_menu(
        &mut self,
        spawn: CellPoint,
        row_index: usize,
        host: ConnectionHost,
    ) {
        self.panel.end_slider_drag();
        self.context_menu
            .open_connection_row(spawn, row_index, host);
        self.mode = OverlayMode::ContextMenu;
        self.open = true;
    }

    /// Open the close-confirmation dialog (CLOSE-CONFIRM). Called from the App's
    /// `CloseRequested` handler when `confirm_close` is on and a foreground job
    /// is running. Idempotent: starts with `close()` so a repeated close request
    /// (some window managers fire it twice) cannot stack dialogs (TRAP-3).
    pub(in crate::native) fn open_confirm_close(&mut self) {
        self.close();
        self.mode = OverlayMode::ConfirmClose;
        self.open = true;
    }

    pub(in crate::native) fn open_risky_paste(&mut self, dialog: RiskyPasteDialog) {
        self.close();
        self.risky_paste = dialog;
        self.mode = OverlayMode::RiskyPaste;
        self.open = true;
    }

    pub(super) fn handle_risky_paste_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('p') | OverlayInput::Char('P') => {
                self.close();
                OverlayOutcome::RiskyPaste
            }
            OverlayInput::Char('o') | OverlayInput::Char('O')
                if self.risky_paste.one_line_available =>
            {
                self.close();
                OverlayOutcome::RiskyPasteOneLine
            }
            OverlayInput::Close | OverlayInput::Char('c') | OverlayInput::Char('C') => {
                self.close();
                OverlayOutcome::RiskyPasteCancel
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    pub(super) fn risky_paste_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 6;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = if self.risky_paste.one_line_available {
            RISKY_PASTE_ACTION_LINE
        } else {
            RISKY_PASTE_ACTION_LINE_NO_ONE_LINE
        };
        let paste_start = text.find("[Enter").unwrap_or(0);
        let one_line_start = text.find("[O]");
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            self.close();
            OverlayOutcome::RiskyPasteCancel
        } else if one_line_start.is_some_and(|start| col_in_body >= start) {
            self.close();
            OverlayOutcome::RiskyPasteOneLine
        } else if col_in_body >= paste_start {
            self.close();
            OverlayOutcome::RiskyPaste
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Keyboard contract for the image viewer (C4): Esc / Enter dismisses; every
    /// other key is swallowed so nothing leaks to the PTY behind the overlay.
    /// Dismissal emits `Close`; the App's per-frame sync clears the GPU overlay
    /// image so the next frame is byte-identical.
    pub(super) fn handle_image_view_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Close | OverlayInput::Activate => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Keyboard contract for the close-confirmation dialog (CLOSE-CONFIRM).
    /// Enter or Y confirms the close (`ForceClose`); Esc or N cancels (closes the
    /// dialog, the window stays open); every other key is swallowed so nothing
    /// leaks to the PTY behind the modal. The `Close` arm must emit `Close`, not
    /// `ForceClose`, so dismissing never exits (TRAP-2).
    pub(super) fn handle_confirm_close_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('y') | OverlayInput::Char('Y') => {
                self.close();
                OverlayOutcome::ForceClose
            }
            OverlayInput::Close | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                OverlayOutcome::Close
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the close-confirmation dialog body (UX4-P1
    /// click→Activate parity). The action line ([`CONFIRM_CLOSE_ACTION_LINE`])
    /// is the 3rd body row (index 2); a click on the "Yes" region confirms
    /// (`ForceClose`), the "No" region cancels (`Close`), and anywhere else —
    /// including the leading prompt text and the other two rows — is inert so a
    /// stray click never destroys a running job (TRAP-2: the dismiss path never
    /// emits `ForceClose`). `col_in_body` indexes the left-aligned body line; the
    /// line is ASCII, so byte offsets equal columns.
    pub(super) fn confirm_close_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_CLOSE_ACTION_LINE;
        let yes_start = text.find("[Enter").unwrap_or(0);
        let no_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= no_start {
            OverlayOutcome::Close
        } else if col_in_body >= yes_start {
            self.close();
            OverlayOutcome::ForceClose
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Open the attach-choice dialog (Phase 14) for host `session_id`. Called by
    /// the App when the selected session is NOT already open in a tab (the
    /// already-open case dedups by switching, never reaching here). Idempotent:
    /// starts with `close()` so a repeated open cannot stack dialogs. The id is
    /// stashed so the New-tab / Replace arms can emit it back to the App.
    pub(in crate::native) fn open_attach_choice(&mut self, session_id: String) {
        self.close();
        self.attach_choice_session_id = session_id;
        self.mode = OverlayMode::AttachChoice;
        self.open = true;
    }

    /// Keyboard contract for the attach-choice dialog (Phase 14). `N`/Enter
    /// attaches in a new tab; `R` replaces the current tab; Esc/`Close` cancels;
    /// every other key is swallowed so nothing leaks to the PTY behind the modal.
    /// Both action arms close the dialog before emitting, mirroring
    /// `ConfirmClose`, and carry the stashed host session-id.
    pub(super) fn handle_attach_choice_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                let id = std::mem::take(&mut self.attach_choice_session_id);
                self.close();
                OverlayOutcome::AttachChoiceNewTab(id)
            }
            OverlayInput::Char('r') | OverlayInput::Char('R') => {
                let id = std::mem::take(&mut self.attach_choice_session_id);
                self.close();
                OverlayOutcome::AttachChoiceReplace(id)
            }
            OverlayInput::Close => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the attach-choice dialog body (Phase 14;
    /// click→key parity, mirroring `confirm_close_click`). The action line
    /// ([`ATTACH_CHOICE_ACTION_LINE`]) is the 3rd body row (index 2); a click on
    /// the "New tab" region attaches in a new tab, the "Replace" region replaces
    /// the current tab, and anywhere else — including the leading prompt text and
    /// the other rows — is inert so a stray click never attaches. ASCII line, so
    /// byte offsets equal columns.
    pub(super) fn attach_choice_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = ATTACH_CHOICE_ACTION_LINE;
        let new_start = text.find("[N").unwrap_or(0);
        let replace_start = text.find("[R").unwrap_or(text.len());
        if col_in_body >= replace_start {
            let id = std::mem::take(&mut self.attach_choice_session_id);
            self.close();
            OverlayOutcome::AttachChoiceReplace(id)
        } else if col_in_body >= new_start {
            let id = std::mem::take(&mut self.attach_choice_session_id);
            self.close();
            OverlayOutcome::AttachChoiceNewTab(id)
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Open the kill-confirmation dialog (Manage Sessions) for host
    /// `session_id`. Called by the App when the user right-clicks a session row
    /// in the manager. Idempotent: starts with `close()` so a repeated open
    /// cannot stack dialogs. The id is stashed so the confirm arm can emit it
    /// back to the App, which runs `session_host::kill_session`.
    pub(in crate::native) fn open_confirm_kill_session(&mut self, session_id: String) {
        self.close();
        self.confirm_kill_session_id = session_id;
        self.mode = OverlayMode::ConfirmKillSession;
        self.open = true;
    }

    /// Keyboard contract for the kill-confirmation dialog (Manage Sessions).
    /// Enter or Y confirms (emits [`OverlayOutcome::KillSessionConfirmed`] with
    /// the stashed id); Esc or N cancels (closes the dialog, kills nothing);
    /// every other key is swallowed so nothing leaks to the PTY behind the
    /// modal. The confirm arm closes the dialog before emitting, mirroring
    /// `ConfirmClose`.
    pub(super) fn handle_confirm_kill_session_input(
        &mut self,
        input: OverlayInput,
    ) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('y') | OverlayInput::Char('Y') => {
                let id = std::mem::take(&mut self.confirm_kill_session_id);
                self.close();
                OverlayOutcome::KillSessionConfirmed(id)
            }
            OverlayInput::Close | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                OverlayOutcome::Close
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the kill-confirmation dialog body (click→key
    /// parity, mirroring `confirm_close_click`). The action line
    /// ([`CONFIRM_KILL_SESSION_ACTION_LINE`]) is the 3rd body row (index 2); a
    /// click on the "Kill" region confirms, the "Cancel" region cancels, and
    /// anywhere else — including the leading prompt text and the other rows — is
    /// inert so a stray click never kills a session. ASCII line, so byte offsets
    /// equal columns.
    pub(super) fn confirm_kill_session_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_KILL_SESSION_ACTION_LINE;
        let kill_start = text.find("[Enter").unwrap_or(0);
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            OverlayOutcome::Close
        } else if col_in_body >= kill_start {
            let id = std::mem::take(&mut self.confirm_kill_session_id);
            self.close();
            OverlayOutcome::KillSessionConfirmed(id)
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Open the replace-tab confirm dialog (ODP-5D) for the picked `host` and
    /// the `target` tab it will replace. Called by the App only when that tab
    /// holds a running foreground child (an idle tab replaces directly, no
    /// prompt). Idempotent: starts with `close()` so a repeated open cannot
    /// stack dialogs. The host + token are stashed so the confirm arm can emit
    /// them back to the App.
    pub(in crate::native) fn open_confirm_replace_tab(
        &mut self,
        host: Box<ConnectionHost>,
        target: SessionToken,
    ) {
        self.close();
        self.confirm_replace_tab = Some((host, target));
        self.mode = OverlayMode::ConfirmReplaceTab;
        self.open = true;
    }

    /// Keyboard contract for the replace-tab confirm dialog (ODP-5D). Enter or Y
    /// confirms (emits [`OverlayOutcome::ReplaceTabWithHostConfirmed`] with the
    /// stashed host + token); Esc or N cancels (closes the dialog, replaces
    /// nothing); every other key is swallowed so nothing leaks to the PTY behind
    /// the modal. The confirm arm closes the dialog before emitting, mirroring
    /// `ConfirmClose`.
    pub(super) fn handle_confirm_replace_tab_input(
        &mut self,
        input: OverlayInput,
    ) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('y') | OverlayInput::Char('Y') => {
                match self.confirm_replace_tab.take() {
                    Some((host, token)) => {
                        self.close();
                        OverlayOutcome::ReplaceTabWithHostConfirmed(host, token)
                    }
                    None => OverlayOutcome::Close,
                }
            }
            OverlayInput::Close | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                OverlayOutcome::Close
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the replace-tab confirm dialog body (click→key
    /// parity, mirroring `confirm_kill_session_click`). The action line
    /// ([`CONFIRM_REPLACE_TAB_ACTION_LINE`]) is the 3rd body row (index 2); a
    /// click on the "Replace" region confirms, the "Cancel" region cancels, and
    /// anywhere else — including the leading prompt text and the other rows — is
    /// inert so a stray click never destroys a running shell. ASCII line, so
    /// byte offsets equal columns.
    pub(super) fn confirm_replace_tab_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_REPLACE_TAB_ACTION_LINE;
        let replace_start = text.find("[Enter").unwrap_or(0);
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            OverlayOutcome::Close
        } else if col_in_body >= replace_start {
            match self.confirm_replace_tab.take() {
                Some((host, token)) => {
                    self.close();
                    OverlayOutcome::ReplaceTabWithHostConfirmed(host, token)
                }
                None => OverlayOutcome::Close,
            }
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Open the remove-host confirm dialog (ODP-2C) for the connection-manager
    /// row `host`. Called when "Remove…" is chosen; the confirm arm emits the
    /// host back so the App deletes its `hosts.conf` block. The dialog is opened
    /// FROM the connection-row context menu, which lives over the still-loaded
    /// connection manager — so this does NOT call `close()` (that would reset
    /// the return path); it flips the mode and stashes the host directly, and
    /// the retained `connections` state backs the cancel-returns-to-manager arm.
    pub(in crate::native) fn open_confirm_remove_host(&mut self, host: Box<ConnectionHost>) {
        self.confirm_remove_host = Some(host);
        self.mode = OverlayMode::ConfirmRemoveHost;
        self.open = true;
    }

    /// Keyboard contract for the remove-host confirm dialog (ODP-2C). Enter or Y
    /// confirms (emits [`OverlayOutcome::RemoveConnectionConfirmed`] with the
    /// stashed host; the App deletes the block and reopens the manager); Esc or
    /// N cancels back to the connection manager with its selection intact (the
    /// manager state was never torn down). Every other key is swallowed. The
    /// confirm arm does NOT `close()` — the App reopens the manager fresh — so
    /// nothing leaks to the PTY.
    pub(super) fn handle_confirm_remove_host_input(
        &mut self,
        input: OverlayInput,
    ) -> OverlayOutcome {
        match input {
            OverlayInput::Activate | OverlayInput::Char('y') | OverlayInput::Char('Y') => {
                match self.confirm_remove_host.take() {
                    Some(host) => OverlayOutcome::RemoveConnectionConfirmed(host),
                    None => self.return_to_connection_manager(),
                }
            }
            OverlayInput::Close | OverlayInput::Char('n') | OverlayInput::Char('N') => {
                self.return_to_connection_manager()
            }
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the remove-host confirm dialog body (click→key
    /// parity, mirroring `confirm_replace_tab_click`). The action line
    /// ([`CONFIRM_REMOVE_HOST_ACTION_LINE`]) is the 3rd body row (index 2); a
    /// click on the "Remove" region confirms, the "Cancel" region cancels back
    /// to the manager, and anywhere else is inert so a stray click never deletes
    /// a saved host. ASCII line, so byte offsets equal columns.
    pub(super) fn confirm_remove_host_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_REMOVE_HOST_ACTION_LINE;
        let remove_start = text.find("[Enter").unwrap_or(0);
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            self.return_to_connection_manager()
        } else if col_in_body >= remove_start {
            match self.confirm_remove_host.take() {
                Some(host) => OverlayOutcome::RemoveConnectionConfirmed(host),
                None => self.return_to_connection_manager(),
            }
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Return to the connection manager from a connection-row menu or its
    /// remove-confirm dialog (ODP-2C) without reloading — the `connections`
    /// overlay state (entries, query, selection) survived the mode switch, so
    /// flipping the mode back restores the manager exactly as it was left.
    pub(super) fn return_to_connection_manager(&mut self) -> OverlayOutcome {
        self.confirm_remove_host = None;
        self.mode = OverlayMode::Connections;
        self.open = true;
        OverlayOutcome::Consumed
    }

    /// Open the overwrite-layout confirm dialog (OVERWRITE-WARN) for the resolved
    /// layout `name` that already exists on disk. `kind` records which save it
    /// was so the confirm arm re-drives the right path. Idempotent: starts with
    /// `close()` so a repeated open cannot stack dialogs.
    pub(in crate::native) fn open_confirm_overwrite_layout(
        &mut self,
        name: String,
        kind: LayoutSaveKind,
    ) {
        self.close();
        self.confirm_overwrite_layout = Some((name, kind));
        self.mode = OverlayMode::ConfirmOverwriteLayout;
        self.open = true;
    }

    /// Keyboard contract for the overwrite-layout confirm dialog (OVERWRITE-WARN).
    /// Enter replaces the existing layout (emits
    /// [`OverlayOutcome::OverwriteLayoutConfirmed`]); `R` reopens the name prompt
    /// for a different name (emits [`OverlayOutcome::RenameLayoutInstead`]); Esc
    /// cancels the save; every other key is swallowed so nothing leaks to the PTY
    /// behind the modal. Both action arms close the dialog before emitting.
    pub(super) fn handle_confirm_overwrite_layout_input(
        &mut self,
        input: OverlayInput,
    ) -> OverlayOutcome {
        match input {
            OverlayInput::Activate => match self.confirm_overwrite_layout.take() {
                Some((name, kind)) => {
                    self.close();
                    OverlayOutcome::OverwriteLayoutConfirmed { name, kind }
                }
                None => OverlayOutcome::Close,
            },
            OverlayInput::Char('r') | OverlayInput::Char('R') => {
                match self.confirm_overwrite_layout.take() {
                    Some((name, kind)) => {
                        self.close();
                        OverlayOutcome::RenameLayoutInstead { name, kind }
                    }
                    None => OverlayOutcome::Close,
                }
            }
            OverlayInput::Close => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the overwrite-layout confirm dialog body
    /// (click→key parity, mirroring `confirm_replace_tab_click`). The action line
    /// ([`CONFIRM_OVERWRITE_LAYOUT_ACTION_LINE`]) is the 3rd body row (index 2);
    /// the three bracket regions map, left-to-right, to Replace / Rename / Cancel,
    /// and anywhere before the first bracket — the leading prompt — is inert. ASCII
    /// line, so byte offsets equal columns.
    pub(super) fn confirm_overwrite_layout_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_OVERWRITE_LAYOUT_ACTION_LINE;
        let replace_start = text.find("[Enter").unwrap_or(0);
        let rename_start = text.find("[R]").unwrap_or(text.len());
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            OverlayOutcome::Close
        } else if col_in_body >= rename_start {
            match self.confirm_overwrite_layout.take() {
                Some((name, kind)) => {
                    self.close();
                    OverlayOutcome::RenameLayoutInstead { name, kind }
                }
                None => OverlayOutcome::Close,
            }
        } else if col_in_body >= replace_start {
            match self.confirm_overwrite_layout.take() {
                Some((name, kind)) => {
                    self.close();
                    OverlayOutcome::OverwriteLayoutConfirmed { name, kind }
                }
                None => OverlayOutcome::Close,
            }
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Open the open-layout mode dialog (LAYOUT-OPEN-MODE) for the saved layout
    /// `name`, shown when a layout is opened onto a window that already holds
    /// real state (more than a single pristine workspace). Idempotent: starts
    /// with `close()` so a repeated open cannot stack dialogs.
    pub(in crate::native) fn open_confirm_open_layout(&mut self, name: String) {
        self.close();
        self.confirm_open_layout = Some(name);
        self.mode = OverlayMode::ConfirmOpenLayout;
        self.open = true;
    }

    /// Keyboard contract for the open-layout mode dialog (LAYOUT-OPEN-MODE).
    /// Enter replaces the current workspaces with the saved set (emits
    /// [`OverlayOutcome::OpenLayoutReplace`]); `A` appends the saved set beside
    /// the current one (emits [`OverlayOutcome::OpenLayoutAdd`]); Esc cancels the
    /// open; every other key is swallowed so nothing leaks to the PTY behind the
    /// modal. Both action arms close the dialog before emitting.
    pub(super) fn handle_confirm_open_layout_input(
        &mut self,
        input: OverlayInput,
    ) -> OverlayOutcome {
        match input {
            OverlayInput::Activate => match self.confirm_open_layout.take() {
                Some(name) => {
                    self.close();
                    OverlayOutcome::OpenLayoutReplace(name)
                }
                None => OverlayOutcome::Close,
            },
            OverlayInput::Char('a') | OverlayInput::Char('A') => {
                match self.confirm_open_layout.take() {
                    Some(name) => {
                        self.close();
                        OverlayOutcome::OpenLayoutAdd(name)
                    }
                    None => OverlayOutcome::Close,
                }
            }
            OverlayInput::Close => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the open-layout mode dialog body (click→key
    /// parity, mirroring `confirm_overwrite_layout_click`). The action line
    /// ([`CONFIRM_OPEN_LAYOUT_ACTION_LINE`]) is the 3rd body row (index 2); the
    /// three bracket regions map, left-to-right, to Replace / Add / Cancel, and
    /// anywhere before the first bracket — the leading prompt — is inert. ASCII
    /// line, so byte offsets equal columns.
    pub(super) fn confirm_open_layout_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 2;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = CONFIRM_OPEN_LAYOUT_ACTION_LINE;
        let replace_start = text.find("[Enter").unwrap_or(0);
        let add_start = text.find("[A]").unwrap_or(text.len());
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            OverlayOutcome::Close
        } else if col_in_body >= add_start {
            match self.confirm_open_layout.take() {
                Some(name) => {
                    self.close();
                    OverlayOutcome::OpenLayoutAdd(name)
                }
                None => OverlayOutcome::Close,
            }
        } else if col_in_body >= replace_start {
            match self.confirm_open_layout.take() {
                Some(name) => {
                    self.close();
                    OverlayOutcome::OpenLayoutReplace(name)
                }
                None => OverlayOutcome::Close,
            }
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Open the Detach & switch choice dialog for the focused pane's
    /// `cwd` (empty = unknown → spawn in the default directory). Called by the
    /// App after it reads the focused pane's cwd (the overlay cannot read the
    /// terminal). Idempotent: starts with `close()` so a repeated open cannot
    /// stack dialogs. The cwd is stashed so the Swap / Keep-both arms can emit
    /// it back to the App.
    pub(in crate::native) fn open_detach_switch_choice(&mut self, cwd: String) {
        self.close();
        self.detach_switch_cwd = cwd;
        self.mode = OverlayMode::DetachSwitchChoice;
        self.open = true;
    }

    /// Keyboard contract for the Detach & switch dialog. `S` swaps
    /// (spawn + close this pane); `K` keeps both (spawn + leave this pane); Esc
    /// cancels; every other key — INCLUDING Enter — is swallowed. There is no
    /// Enter default on purpose: Swap is destructive (it closes the focused
    /// pane), so the user must explicitly choose S or K. Both action arms close
    /// the dialog before emitting, mirroring `ConfirmClose`, and carry the
    /// stashed cwd.
    pub(super) fn handle_detach_switch_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        match input {
            OverlayInput::Char('s') | OverlayInput::Char('S') => {
                let cwd = std::mem::take(&mut self.detach_switch_cwd);
                self.close();
                OverlayOutcome::DetachSwitchSwap(cwd)
            }
            OverlayInput::Char('k') | OverlayInput::Char('K') => {
                let cwd = std::mem::take(&mut self.detach_switch_cwd);
                self.close();
                OverlayOutcome::DetachSwitchKeepBoth(cwd)
            }
            OverlayInput::Close => OverlayOutcome::Close,
            _ => OverlayOutcome::Consumed,
        }
    }

    /// Hit-test a left-click in the Detach & switch dialog body (click→key
    /// parity, mirroring `attach_choice_click`). The action line
    /// ([`DETACH_SWITCH_ACTION_LINE`]) is the 4th body row (index 3); a click on
    /// the "Swap" region swaps, the "Keep both" region keeps both, the "Cancel"
    /// region cancels, and anywhere else — including the leading prompt text and
    /// the other rows — is inert so a stray click never spawns or closes a pane.
    /// ASCII line, so byte offsets equal columns. Regions are tested right-to-
    /// left (Cancel, then Keep, then Swap) since their anchors are ordered S < K
    /// < Esc in the line.
    pub(super) fn detach_switch_click(
        &mut self,
        row_in_body: usize,
        col_in_body: usize,
    ) -> OverlayOutcome {
        const ACTION_ROW: usize = 3;
        if row_in_body != ACTION_ROW {
            return OverlayOutcome::Consumed;
        }
        let text = DETACH_SWITCH_ACTION_LINE;
        let swap_start = text.find("[S").unwrap_or(0);
        let keep_start = text.find("[K").unwrap_or(text.len());
        let cancel_start = text.find("[Esc").unwrap_or(text.len());
        if col_in_body >= cancel_start {
            OverlayOutcome::Close
        } else if col_in_body >= keep_start {
            let cwd = std::mem::take(&mut self.detach_switch_cwd);
            self.close();
            OverlayOutcome::DetachSwitchKeepBoth(cwd)
        } else if col_in_body >= swap_start {
            let cwd = std::mem::take(&mut self.detach_switch_cwd);
            self.close();
            OverlayOutcome::DetachSwitchSwap(cwd)
        } else {
            OverlayOutcome::Consumed
        }
    }

    /// Lift a [`ContextMenuOutcome`] into an [`OverlayOutcome`] (IN2). An
    /// `Activate` closes the menu and emits the matching App-side action; the
    /// App runs it after the overlay has closed.
    pub(super) fn apply_context_menu_outcome(
        &mut self,
        outcome: ContextMenuOutcome,
    ) -> OverlayOutcome {
        match outcome {
            ContextMenuOutcome::Consumed => OverlayOutcome::Consumed,
            // ODP-2C: dismissing a connection-row menu returns to the still-
            // loaded manager (selection intact), not to the grid.
            ContextMenuOutcome::Close => {
                if matches!(
                    self.context_menu.surface(),
                    crate::native::context_menu_ui::ContextMenuSurface::ConnectionRow(_)
                ) {
                    self.return_to_connection_manager()
                } else {
                    OverlayOutcome::Close
                }
            }
            ContextMenuOutcome::Activate(item) => {
                // ODP-2C connection-row items need bespoke transitions (Edit opens
                // the form in place, Remove opens a confirm, the rest close and
                // route App-side), so handle them before the generic close.
                if matches!(
                    self.context_menu.surface(),
                    crate::native::context_menu_ui::ContextMenuSurface::ConnectionRow(_)
                ) {
                    return self.apply_connection_row_menu_item(item);
                }
                let surface = self.context_menu.surface();
                self.close();
                match item {
                    ContextMenuItem::Copy => OverlayOutcome::ContextMenuCopy,
                    ContextMenuItem::Cut => OverlayOutcome::ContextMenuCut,
                    ContextMenuItem::Paste => OverlayOutcome::ContextMenuPaste,
                    ContextMenuItem::Delete => OverlayOutcome::ContextMenuDelete,
                    ContextMenuItem::SelectAll => OverlayOutcome::ContextMenuSelectAll,
                    ContextMenuItem::SelectCommandOutput => {
                        OverlayOutcome::ContextMenuSelectCommandOutput
                    }
                    ContextMenuItem::SelectCommandWithPrompt => {
                        OverlayOutcome::ContextMenuSelectCommandWithPrompt
                    }
                    ContextMenuItem::CopyCommandOutput => {
                        OverlayOutcome::ContextMenuCopyCommandOutput
                    }
                    ContextMenuItem::CopyCommandWithPrompt => {
                        OverlayOutcome::ContextMenuCopyCommandWithPrompt
                    }
                    ContextMenuItem::SearchCommandOutput => {
                        OverlayOutcome::ContextMenuSearchCommandOutput
                    }
                    ContextMenuItem::JumpFailedCommandPrev => {
                        OverlayOutcome::ContextMenuJumpFailedCommandPrev
                    }
                    ContextMenuItem::JumpFailedCommandNext => {
                        OverlayOutcome::ContextMenuJumpFailedCommandNext
                    }
                    ContextMenuItem::ExportCommandOutput => {
                        OverlayOutcome::ContextMenuExportCommandOutput
                    }
                    ContextMenuItem::NewTab => OverlayOutcome::ContextMenuNewTab,
                    ContextMenuItem::NewTabWithProfile => {
                        OverlayOutcome::ContextMenuNewTabWithProfile
                    }
                    ContextMenuItem::NewLocalTab => OverlayOutcome::ContextMenuNewLocalTab,
                    ContextMenuItem::DuplicateTab => OverlayOutcome::ContextMenuDuplicateTab,
                    ContextMenuItem::DuplicateWorkspace => {
                        OverlayOutcome::ContextMenuDuplicateWorkspace
                    }
                    ContextMenuItem::NewWindow => OverlayOutcome::ContextMenuNewWindow,
                    ContextMenuItem::RenameTab => {
                        if let Some(target) = self.context_menu.rename_target() {
                            OverlayOutcome::ContextMenuRenameTab(target)
                        } else {
                            OverlayOutcome::Consumed
                        }
                    }
                    // §7.4 workspace surfaces: New is global; Rename/Close target
                    // the right-clicked slot's rail index.
                    ContextMenuItem::NewWorkspace => OverlayOutcome::ContextMenuNewWorkspace,
                    ContextMenuItem::NewWorkspaceWithProfile => {
                        OverlayOutcome::ContextMenuNewWorkspaceWithProfile
                    }
                    ContextMenuItem::RenameWorkspace => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(idx) => {
                            OverlayOutcome::ContextMenuRenameWorkspace(idx)
                        }
                        // The content-grid menu has no per-workspace target, so
                        // Rename/Close act on the active workspace.
                        crate::native::context_menu_ui::ContextMenuSurface::Content => {
                            OverlayOutcome::ContextMenuRenameActiveWorkspace
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::CloseWorkspace => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(idx) => {
                            OverlayOutcome::ContextMenuCloseWorkspace(idx)
                        }
                        crate::native::context_menu_ui::ContextMenuSurface::Content => {
                            OverlayOutcome::ContextMenuCloseActiveWorkspace
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    // RAIL-REORDER: the move rows are WorkspaceSlot-only; they
                    // carry the clicked slot's rail index.
                    ContextMenuItem::MoveWorkspaceUp => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(idx) => {
                            OverlayOutcome::ContextMenuMoveWorkspaceUp(idx)
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::MoveWorkspaceDown => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(idx) => {
                            OverlayOutcome::ContextMenuMoveWorkspaceDown(idx)
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    // ODP-6B: bind the active workspace to a host. The menu
                    // closed itself; the App opens the shared host picker seeded
                    // for the BindWorkspace purpose (ODP-1B). Unbind is direct —
                    // no host choice needed.
                    ContextMenuItem::BindWorkspaceToHost => match self.context_menu.surface() {
                        // RAIL-BIND: a rail slot binds the CLICKED workspace; the
                        // content-grid menu binds the active one.
                        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(idx) => {
                            OverlayOutcome::ContextMenuBindWorkspaceAt(idx)
                        }
                        _ => OverlayOutcome::ContextMenuBindWorkspace,
                    },
                    ContextMenuItem::UnbindWorkspace => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(idx) => {
                            OverlayOutcome::ContextMenuUnbindWorkspaceAt(idx)
                        }
                        _ => OverlayOutcome::ContextMenuUnbindWorkspace,
                    },
                    // SAVE-ALL-LAYOUT: the whole-app save is surface-independent —
                    // it captures every workspace regardless of where the menu was
                    // opened. The App opens the "Layout name:" prompt.
                    ContextMenuItem::SaveAllLayout => OverlayOutcome::ContextMenuSaveAllLayout,
                    // LAYOUT-SURFACE: a rail slot saves the CLICKED workspace;
                    // the content-grid section saves the active one. Either way
                    // the App opens the "Layout name:" prompt.
                    ContextMenuItem::SaveAsLayout => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(idx) => {
                            OverlayOutcome::ContextMenuSaveLayoutAt(idx)
                        }
                        _ => OverlayOutcome::ContextMenuSaveActiveLayout,
                    },
                    // LAYOUT-SURFACE: open the saved-layout picker (surface-
                    // independent — the App seeds it from the saved layout names).
                    ContextMenuItem::OpenLayout => OverlayOutcome::ContextMenuOpenLayoutPicker,
                    // NF-F7-1: a Close Tab chosen from a specific tab slot closes
                    // THAT tab, not the active one. The Content surface (no
                    // TabSlot token) keeps the active-tab close.
                    ContextMenuItem::CloseTab => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(token) => {
                            OverlayOutcome::ContextMenuCloseTabToken(token)
                        }
                        _ => OverlayOutcome::ContextMenuCloseTab,
                    },
                    // Tab-scoped: only ever visible on a TabSlot; close every
                    // other tab, keeping the right-clicked one.
                    ContextMenuItem::CloseOtherTabs => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(token) => {
                            OverlayOutcome::ContextMenuCloseOtherTabs(token)
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    // W4-v2: open the "Move to Workspace" picker for the
                    // right-clicked tab. Only ever visible on a TabSlot with >1
                    // workspace.
                    ContextMenuItem::MoveToWorkspace => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(token) => {
                            OverlayOutcome::ContextMenuMoveToWorkspace(token)
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    // ODP-5D: open the shared host picker for the clicked tab.
                    // The App seeds it for the ConnectTabAfter / ReplaceTab
                    // purpose so the pick routes back to the right tab. TabSlot-
                    // only, so a stray non-tab surface is inert.
                    ContextMenuItem::ConnectToHost => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(token) => {
                            OverlayOutcome::ContextMenuConnectToHost(token)
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::ReplaceTabWithHost => match self.context_menu.surface() {
                        crate::native::context_menu_ui::ContextMenuSurface::TabSlot(token) => {
                            OverlayOutcome::ContextMenuReplaceTabWithHost(token)
                        }
                        _ => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::SplitColumns => OverlayOutcome::ContextMenuSplitColumns,
                    ContextMenuItem::SplitRows => OverlayOutcome::ContextMenuSplitRows,
                    ContextMenuItem::ClosePane => OverlayOutcome::ContextMenuClosePane,
                    ContextMenuItem::Settings => {
                        let target = match surface {
                            crate::native::context_menu_ui::ContextMenuSurface::TabSlot(_)
                            | crate::native::context_menu_ui::ContextMenuSurface::TabStripEmpty
                            | crate::native::context_menu_ui::ContextMenuSurface::WorkspaceSlot(_)
                            | crate::native::context_menu_ui::ContextMenuSurface::WorkspaceRailEmpty => {
                                SettingsTarget::TabsAndPanes
                            }
                            _ => SettingsTarget::Root,
                        };
                        OverlayOutcome::ContextMenuSettings(target)
                    }
                    // F3: reuse the fully-wired OpenKeyBindings outcome (the
                    // settings panel's "keybinds" row emits the same one); the
                    // App handler flushes pending overlay settings and opens the
                    // key-remap editor. The context menu already closed itself
                    // above, matching the other launcher items.
                    ContextMenuItem::KeyboardShortcuts => OverlayOutcome::OpenKeyBindings,
                    ContextMenuItem::ConnectionManager => {
                        OverlayOutcome::ContextMenuConnectionManager
                    }
                    ContextMenuItem::CommandPalette => OverlayOutcome::ContextMenuCommandPalette,
                    ContextMenuItem::SessionReplay => OverlayOutcome::ContextMenuSessionReplay,
                    ContextMenuItem::SessionAttach => OverlayOutcome::ContextMenuSessionAttach,
                    // C3 file section: build each outcome from the resolved path
                    // snapshotted at open time (survives the `close()` above, as
                    // the rename-target read does). A missing target (defensive —
                    // these items are only visible with a target) is inert.
                    ContextMenuItem::OpenPath => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuOpenPath(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    // C4: only ever visible on a resolved image span; build the
                    // viewer-open outcome from the snapshotted target.
                    ContextMenuItem::OpenInOdytty => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuOpenInOdytty(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    // C3b: only ever visible on a resolved file span; the App
                    // enumerates the handler apps and opens the picker overlay.
                    ContextMenuItem::OpenWith => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuOpenWith(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::CopyPath => match self.context_menu.path_target() {
                        Some(resolved) => OverlayOutcome::ContextMenuCopyPath(resolved.abs.clone()),
                        None => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::CopyFile => match self.context_menu.path_target() {
                        Some(resolved) => OverlayOutcome::ContextMenuCopyFile(
                            crate::native::app::interactive_paths::file_uri(
                                &resolved.abs,
                                crate::native::app::platform_opener::OpenerOs::host(),
                            ),
                        ),
                        None => OverlayOutcome::Consumed,
                    },
                    ContextMenuItem::RevealPath => match self.context_menu.path_target() {
                        Some(resolved) => {
                            OverlayOutcome::ContextMenuRevealPath(Box::new(resolved.clone()))
                        }
                        None => OverlayOutcome::Consumed,
                    },
                    // The menu closed itself; the App reads the focused
                    // pane's cwd (it owns the terminal lock) and opens the 3-way
                    // Detach & switch choice dialog. The overlay cannot read the
                    // terminal, so the cwd is resolved App-side, not here.
                    ContextMenuItem::DetachSwitch => OverlayOutcome::ContextMenuDetachSwitch,
                    // ODP-2C connection-row items are handled by the early-return
                    // above whenever the surface is `ConnectionRow`; they are
                    // never visible on any other surface, so reaching here is
                    // defensive-only. The menu already closed itself.
                    ContextMenuItem::ConnRowOpenInTab
                    | ContextMenuItem::ConnRowOpenInWorkspace
                    | ContextMenuItem::ConnRowBindWorkspace
                    | ContextMenuItem::ConnRowEdit
                    | ContextMenuItem::ConnRowRemove => OverlayOutcome::Consumed,
                }
            }
        }
    }

    /// Route an activated ODP-2C connection-row menu item. Open in New Tab / New
    /// Workspace / Bind close the manager and hand the App the clicked host;
    /// Edit opens the P4 form in place (returning to the form, not the grid);
    /// Remove opens the confirm dialog over the still-loaded manager. A missing
    /// target (defensive — the items are only visible with one) returns to the
    /// manager rather than acting.
    pub(super) fn apply_connection_row_menu_item(
        &mut self,
        item: ContextMenuItem,
    ) -> OverlayOutcome {
        let host = self.context_menu.connection_target().cloned();
        match item {
            // Open in a new tab in the current workspace: reuse the manager's own
            // connect path (same as accepting a row).
            ContextMenuItem::ConnRowOpenInTab => {
                self.close();
                match host {
                    Some(host) => OverlayOutcome::Connect(Box::new(host)),
                    None => OverlayOutcome::Close,
                }
            }
            ContextMenuItem::ConnRowOpenInWorkspace => {
                self.close();
                match host {
                    Some(host) => OverlayOutcome::ConnectHostInNewWorkspace(Box::new(host)),
                    None => OverlayOutcome::Close,
                }
            }
            // Bind the current workspace to the clicked host (reuses the frozen
            // W5 setter + bind toast via the shared BindWorkspaceToHost outcome).
            ContextMenuItem::ConnRowBindWorkspace => {
                self.close();
                match host {
                    Some(host) => OverlayOutcome::BindWorkspaceToHost(host.alias),
                    None => OverlayOutcome::Close,
                }
            }
            // Open the Edit form in place (the manager stays torn down behind the
            // form, exactly like the keyboard → path). OdyTTY aliases minus this
            // one supply the collision guard.
            ContextMenuItem::ConnRowEdit => match host {
                Some(host) => {
                    let aliases = self
                        .connections
                        .odytty_aliases()
                        .into_iter()
                        .filter(|alias| *alias != host.alias)
                        .collect();
                    self.connection_form.open_edit(&host, aliases);
                    self.mode = OverlayMode::ConnectionForm;
                    self.open = true;
                    OverlayOutcome::Consumed
                }
                None => self.return_to_connection_manager(),
            },
            // Destructive: gate behind the remove-confirm dialog over the still-
            // loaded manager.
            ContextMenuItem::ConnRowRemove => match host {
                Some(host) => {
                    self.open_confirm_remove_host(Box::new(host));
                    OverlayOutcome::Consumed
                }
                None => self.return_to_connection_manager(),
            },
            // Not a connection-row item; defensively return to the manager.
            _ => self.return_to_connection_manager(),
        }
    }

    pub(super) fn handle_context_menu_input(&mut self, input: OverlayInput) -> OverlayOutcome {
        let outcome = self.context_menu.handle_input(input);
        self.apply_context_menu_outcome(outcome)
    }
}

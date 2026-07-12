// SPDX-License-Identifier: GPL-3.0-only
//! Pointer-driven interaction for the native app: mouse reporting, text
//! selection, hyperlink hover/open, and scrollback viewport movement.
//!
//! Mechanically split out of `app/mod.rs` (MS3) to keep that file under the
//! source-size cap; no behavior or API change. These are `App` methods that
//! live in a child module so they can reach `App`'s private fields and the
//! sibling methods that stayed in `app/mod.rs` directly. Methods the parent
//! `app` module calls back into are marked `pub(super)`.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InteractivePathOpenKind {
    InlineImage,
    External,
}

fn interactive_path_open_kind(
    settings: &crate::settings::Settings,
    resolved: &crate::paths::Resolved,
) -> InteractivePathOpenKind {
    if settings.interactive_paths_image_inline && crate::paths::is_image_path(&resolved.abs) {
        InteractivePathOpenKind::InlineImage
    } else {
        InteractivePathOpenKind::External
    }
}

/// Pure hit-test for the lightbox click-outside-to-dismiss (Phase 13d): is the
/// pointer pixel `(px, py)` OUTSIDE the image fit-rect `[x0, y0, x1, y1]`?
///
/// Boundary convention: edges are INCLUSIVE — a point exactly on the rect border
/// counts as ON the image (inside), so it is inert rather than dismissing; only
/// a point strictly beyond an edge returns `true`. Kept a free, pure fn so it is
/// unit-testable headless, with no GPU/window state. The pointer pixel and the
/// fit-rect share the same physical-pixel origin (full-viewport lightbox), so
/// the comparison is exact.
fn point_outside_rect(px: f64, py: f64, rect: [f32; 4]) -> bool {
    let [x0, y0, x1, y1] = rect;
    px < x0 as f64 || px > x1 as f64 || py < y0 as f64 || py > y1 as f64
}

impl App {
    fn mouse_protocol(&self) -> MouseProtocol {
        self.terminal
            .lock()
            .map(|terminal| terminal.mouse_protocol())
            .unwrap_or_default()
    }

    fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_protocol().is_enabled()
    }

    /// Shift is the local-selection escape hatch while a TUI has enabled mouse
    /// reporting, matching the common xterm-family terminal convention.
    pub(super) fn should_report_mouse_to_pty(&self) -> bool {
        self.mouse_reporting_enabled() && !self.modifiers.shift
    }

    /// Route an overlay [`OverlayOutcome`] (from either the keyboard or the
    /// pointer path) through the shared App-side handlers, so the two entry
    /// points stay in lockstep (UX4-P1).
    pub(super) fn apply_overlay_outcome(&mut self, outcome: OverlayOutcome) {
        self.apply_overlay_outcome_with_policy(outcome, false);
    }

    pub(super) fn apply_overlay_outcome_with_policy(
        &mut self,
        outcome: OverlayOutcome,
        coalesce_apply: bool,
    ) {
        match outcome {
            OverlayOutcome::Consumed => {}
            OverlayOutcome::Close => {
                self.flush_pending_overlay_settings();
                self.overlay.close();
            }
            // First-run onboarding dismissal: persist a marker so the welcome
            // card does not reshow next launch (best-effort; a write failure
            // must never block dismissal), then close like any other overlay.
            OverlayOutcome::CloseOnboarding => {
                self.flush_pending_overlay_settings();
                self.persist_first_run_config();
                self.overlay.close();
            }
            OverlayOutcome::OpenThemePicker => {
                self.flush_pending_overlay_settings();
                self.open_theme_picker_overlay();
            }
            OverlayOutcome::OpenThemeBuilder => {
                self.flush_pending_overlay_settings();
                self.open_theme_builder_overlay();
            }
            OverlayOutcome::OpenKeyBindings => {
                self.flush_pending_overlay_settings();
                self.open_key_bindings_overlay();
            }
            OverlayOutcome::OpenFontPicker => {
                self.flush_pending_overlay_settings();
                self.open_font_picker_overlay();
            }
            OverlayOutcome::ApplySettings(settings) => {
                if coalesce_apply {
                    self.queue_overlay_settings(*settings);
                } else {
                    self.pending_overlay_settings = None;
                    self.apply_overlay_settings(*settings);
                }
            }
            OverlayOutcome::SaveSettings(changes) => self.save_overlay_settings(&changes),
            OverlayOutcome::SaveTheme(request) => {
                self.flush_pending_overlay_settings();
                self.save_overlay_theme(request);
            }
            // IN2: the menu closed itself before emitting these; run the action.
            OverlayOutcome::ContextMenuCopy => {
                self.flush_pending_overlay_settings();
                self.handle_copy_shortcut();
            }
            OverlayOutcome::ContextMenuCut => {
                self.flush_pending_overlay_settings();
                self.handle_context_menu_cut();
            }
            OverlayOutcome::ContextMenuPaste => {
                self.flush_pending_overlay_settings();
                self.handle_paste_shortcut();
            }
            OverlayOutcome::ContextMenuDelete => {
                self.flush_pending_overlay_settings();
                self.handle_context_menu_delete();
            }
            OverlayOutcome::ContextMenuSelectAll => {
                self.flush_pending_overlay_settings();
                self.handle_select_all();
            }
            OverlayOutcome::ContextMenuNewTab => {
                self.flush_pending_overlay_settings();
                self.handle_new_tab();
            }
            // F6-W5: the bound-workspace escape hatch — always a local shell.
            OverlayOutcome::ContextMenuNewLocalTab => {
                self.flush_pending_overlay_settings();
                self.handle_new_local_tab();
            }
            // Duplicate Tab: same cwd-aware local-tab spawn as New Local Tab —
            // a fresh shell in the active pane's directory (not a process fork).
            OverlayOutcome::ContextMenuDuplicateTab => {
                self.flush_pending_overlay_settings();
                self.handle_new_local_tab();
            }
            // F1: the context menu closed itself; launch another OdyTTY window
            // through the same handler the Ctrl+Shift+N chord fires.
            OverlayOutcome::ContextMenuNewWindow => {
                self.flush_pending_overlay_settings();
                self.handle_new_window();
            }
            OverlayOutcome::ContextMenuRenameTab(target) => {
                self.flush_pending_overlay_settings();
                self.enter_rename_tab(target);
            }
            // §7.4 workspace context-menu / rail actions.
            OverlayOutcome::ContextMenuNewWorkspace => {
                self.flush_pending_overlay_settings();
                self.handle_new_workspace();
            }
            // Duplicate Workspace: a fresh workspace whose first shell opens in
            // the active pane's cwd (F1 cwd inheritance), mirroring Duplicate Tab
            // one level up — not a process fork.
            OverlayOutcome::ContextMenuDuplicateWorkspace => {
                self.flush_pending_overlay_settings();
                self.handle_duplicate_workspace();
            }
            OverlayOutcome::ContextMenuRenameWorkspace(idx) => {
                self.flush_pending_overlay_settings();
                self.enter_rename_workspace(idx);
            }
            OverlayOutcome::ContextMenuCloseWorkspace(idx) => {
                self.flush_pending_overlay_settings();
                self.close_workspace_at(idx);
            }
            // RAIL-REORDER: move the clicked workspace one slot in the rail.
            OverlayOutcome::ContextMenuMoveWorkspaceUp(idx) => {
                self.flush_pending_overlay_settings();
                self.move_workspace_at(idx, true);
            }
            OverlayOutcome::ContextMenuMoveWorkspaceDown(idx) => {
                self.flush_pending_overlay_settings();
                self.move_workspace_at(idx, false);
            }
            // Content-grid workspace section: Rename/Close target the active
            // workspace (no per-workspace click target on the grid).
            OverlayOutcome::ContextMenuRenameActiveWorkspace => {
                self.flush_pending_overlay_settings();
                self.enter_rename_workspace(self.sessions.active_workspace_index());
            }
            OverlayOutcome::ContextMenuCloseActiveWorkspace => {
                self.flush_pending_overlay_settings();
                self.close_workspace_at(self.sessions.active_workspace_index());
            }
            // ODP-6B: bind the active workspace to a host. The menu closed
            // itself; open the shared host picker (ODP-1B) seeded for binding.
            OverlayOutcome::ContextMenuBindWorkspace => {
                self.flush_pending_overlay_settings();
                self.open_bind_workspace_picker();
            }
            // ODP-6B: unbind the active workspace directly (no host choice).
            OverlayOutcome::ContextMenuUnbindWorkspace => {
                self.flush_pending_overlay_settings();
                self.unbind_active_workspace();
            }
            // ODP-1B/6B: the shared host picker closed itself before emitting
            // this; bind the active workspace to the chosen saved-host alias.
            OverlayOutcome::BindWorkspaceToHost(alias) => {
                self.flush_pending_overlay_settings();
                self.bind_active_workspace_to_host_alias(alias);
            }
            // RAIL-BIND: bind the CLICKED rail workspace to a host. The menu
            // closed itself; open the shared host picker seeded for the slot.
            OverlayOutcome::ContextMenuBindWorkspaceAt(idx) => {
                self.flush_pending_overlay_settings();
                self.open_bind_workspace_at_picker(idx);
            }
            // RAIL-BIND: unbind the CLICKED rail workspace directly.
            OverlayOutcome::ContextMenuUnbindWorkspaceAt(idx) => {
                self.flush_pending_overlay_settings();
                self.unbind_workspace_at(idx);
            }
            // RAIL-BIND: the shared host picker closed itself; bind the clicked
            // rail slot to the chosen saved-host alias.
            OverlayOutcome::BindWorkspaceAtToHost(idx, alias) => {
                self.flush_pending_overlay_settings();
                self.bind_workspace_at_to_host_alias(idx, alias);
            }
            // ODP-5D: a tab-menu host action closed the menu; open the shared
            // host picker seeded for the clicked tab so the pick routes back.
            OverlayOutcome::ContextMenuConnectToHost(token) => {
                self.flush_pending_overlay_settings();
                self.open_connect_tab_after_picker(token);
            }
            OverlayOutcome::ContextMenuReplaceTabWithHost(token) => {
                self.flush_pending_overlay_settings();
                self.open_replace_tab_picker(token);
            }
            // ODP-5D: the picker closed itself; open the host in a new tab right
            // after the clicked tab (leaves the clicked shell untouched).
            OverlayOutcome::ConnectHostInTabAfter(host, token) => {
                self.flush_pending_overlay_settings();
                self.connect_host_in_tab_after(&host, token);
            }
            // ODP-5D: replace the clicked tab with the picked host — gated behind
            // a confirm when that tab holds a running foreground child.
            OverlayOutcome::ReplaceTabWithHostPicked(host, token) => {
                self.flush_pending_overlay_settings();
                self.replace_tab_with_host(host, token);
            }
            // ODP-5D: the replace confirm was accepted; run the destructive
            // close+connect now.
            OverlayOutcome::ReplaceTabWithHostConfirmed(host, token) => {
                self.flush_pending_overlay_settings();
                self.do_replace_tab_with_host(&host, token);
            }
            // ODP-2C: a connection-row menu chose "Open in New Workspace"; create
            // a fresh workspace pre-bound to the host and connect its first tab.
            OverlayOutcome::ConnectHostInNewWorkspace(host) => {
                self.flush_pending_overlay_settings();
                self.open_host_in_new_workspace(&host);
            }
            // ODP-2C: the remove-host confirm was accepted; delete the host's
            // hosts.conf block and reopen the manager so the row disappears.
            OverlayOutcome::RemoveConnectionConfirmed(host) => {
                self.flush_pending_overlay_settings();
                self.remove_saved_host(&host);
            }
            OverlayOutcome::ContextMenuCloseTab => {
                self.flush_pending_overlay_settings();
                let _ = self.close_active_tab();
            }
            // NF-F7-1: close the right-clicked tab (by token), not the active one.
            OverlayOutcome::ContextMenuCloseTabToken(token) => {
                self.flush_pending_overlay_settings();
                self.close_tab_by_token(token);
            }
            // F7: close every tab except the right-clicked one.
            OverlayOutcome::ContextMenuCloseOtherTabs(token) => {
                self.flush_pending_overlay_settings();
                self.close_other_tabs(token);
            }
            // W4-v2: open the "Move to Workspace" destination picker for the
            // right-clicked tab.
            OverlayOutcome::ContextMenuMoveToWorkspace(token) => {
                self.flush_pending_overlay_settings();
                self.open_move_tab_workspace_picker(token);
            }
            // W4-v2: the picker chose a destination workspace; splice the tab.
            OverlayOutcome::MoveTabToWorkspacePicked(token, dest_ws) => {
                self.flush_pending_overlay_settings();
                self.move_tab_to_workspace(token, dest_ws);
            }
            // LAYOUT-SURFACE: a WorkspaceSlot menu chose Save as Layout; open the
            // name prompt seeded from the CLICKED workspace.
            OverlayOutcome::ContextMenuSaveLayoutAt(idx) => {
                self.flush_pending_overlay_settings();
                self.enter_save_layout_prompt(idx);
            }
            // LAYOUT-SURFACE: the content-grid workspace section chose Save as
            // Layout; open the name prompt seeded from the ACTIVE workspace.
            OverlayOutcome::ContextMenuSaveActiveLayout => {
                self.flush_pending_overlay_settings();
                self.enter_save_layout_prompt(self.sessions.active_workspace_index());
            }
            // SAVE-ALL-LAYOUT: the content-grid section or the empty rail chose the
            // whole-app Save as Layout; open the name prompt (captures every
            // workspace on Enter).
            OverlayOutcome::ContextMenuSaveAllLayout => {
                self.flush_pending_overlay_settings();
                self.enter_save_all_layout_prompt();
            }
            // OVERWRITE-WARN: the collision dialog's Replace arm — force-write the
            // layout, clobbering the existing file, routed by the carried kind.
            OverlayOutcome::OverwriteLayoutConfirmed { name, kind } => {
                self.flush_pending_overlay_settings();
                self.overwrite_layout_confirmed(&name, kind);
            }
            // OVERWRITE-WARN: the collision dialog's "different name" arm — reopen
            // the "Layout name:" prompt seeded with the colliding name.
            OverlayOutcome::RenameLayoutInstead { name, kind } => {
                self.flush_pending_overlay_settings();
                self.reopen_layout_name_prompt(kind, name);
            }
            // LAYOUT-SURFACE: open the saved-layout picker (seeded App-side).
            OverlayOutcome::ContextMenuOpenLayoutPicker => {
                self.flush_pending_overlay_settings();
                self.open_saved_layout_picker();
            }
            // LAYOUT-SURFACE: the picker chose a layout; open it — onto real
            // state this raises the Replace/Add/Cancel prompt (LAYOUT-OPEN-MODE),
            // onto a bare launch it opens directly (pristine-consume).
            OverlayOutcome::ContextMenuOpenLayout(name) => {
                self.flush_pending_overlay_settings();
                self.open_layout(&name);
            }
            // LAYOUT-OPEN-MODE: the open-layout dialog's Replace arm — tear down
            // the current workspaces and install the saved set as the whole app.
            OverlayOutcome::OpenLayoutReplace(name) => {
                self.flush_pending_overlay_settings();
                self.instantiate_layout(&name, LayoutPlacement::Replace);
            }
            // LAYOUT-OPEN-MODE: the open-layout dialog's Add arm — append the
            // saved set beside the current workspaces (the prior behavior).
            OverlayOutcome::OpenLayoutAdd(name) => {
                self.flush_pending_overlay_settings();
                self.instantiate_layout(&name, LayoutPlacement::Add);
            }
            // Part B: the context menu closed itself; split the focused pane
            // through the exact same action the keyboard split chords fire
            // (`apply_pane_action` → `split_active_pane`).
            OverlayOutcome::ContextMenuSplitColumns => {
                self.flush_pending_overlay_settings();
                self.apply_pane_action(crate::settings::BindableAction::SplitColumns);
            }
            OverlayOutcome::ContextMenuSplitRows => {
                self.flush_pending_overlay_settings();
                self.apply_pane_action(crate::settings::BindableAction::SplitRows);
            }
            // The context menu closed itself; close the focused pane through the
            // same action the tmux `Ctrl-b x` prefix / palette `close-pane` fire.
            // Only emitted in a multi-pane tab (the item is hidden single-pane).
            OverlayOutcome::ContextMenuClosePane => {
                self.flush_pending_overlay_settings();
                self.apply_pane_action(crate::settings::BindableAction::ClosePane);
            }
            // The context menu closed itself; open the settings panel at the
            // target selected from the clicked surface. Content remains at the
            // generic root while tab/workspace chrome enters Tabs & Panes.
            OverlayOutcome::ContextMenuSettings(target) => {
                self.open_settings_overlay_target(target);
            }
            // v0.3.1 launcher section: the context menu closed itself; open each
            // overlay through the same entry the discoverability chords fire.
            OverlayOutcome::ContextMenuConnectionManager => {
                self.flush_pending_overlay_settings();
                self.open_connection_overlay();
            }
            OverlayOutcome::ContextMenuCommandPalette => {
                self.flush_pending_overlay_settings();
                self.open_command_palette_overlay();
            }
            OverlayOutcome::ContextMenuSessionReplay => {
                self.flush_pending_overlay_settings();
                self.open_replay_overlay();
            }
            OverlayOutcome::ContextMenuSessionAttach => {
                self.flush_pending_overlay_settings();
                self.open_session_attach_overlay();
            }
            // C3 file section: the menu closed itself before emitting these.
            // Open dispatches through the same argv-only path the Ctrl+click
            // open uses; copy items write text to the clipboard; reveal opens
            // the parent directory. All best-effort — a spawn/clipboard failure
            // never panics the UI.
            OverlayOutcome::ContextMenuOpenPath(resolved) => {
                self.flush_pending_overlay_settings();
                let argv = self.path_open_argv_for(&resolved);
                self.spawn_open_or_notice(&argv);
            }
            // C4: open the resolved image span in the in-terminal viewer.
            OverlayOutcome::ContextMenuOpenInOdytty(resolved) => {
                self.flush_pending_overlay_settings();
                self.open_image_view(&resolved);
            }
            // C3b: enumerate the apps that can open the resolved file and open
            // the "Open With…" picker overlay. Enumeration is read-only; a file
            // with no handlers opens the overlay with its empty-state hint.
            OverlayOutcome::ContextMenuOpenWith(resolved) => {
                self.flush_pending_overlay_settings();
                self.open_open_with_overlay(&resolved);
            }
            // C3b: launch the app chosen in the picker. The overlay closed
            // itself before emitting this; the argv was built argv-only by
            // `exec_to_argv` (path already one inert element). A spawn failure
            // never panics the UI — it surfaces a transient notice (P0-2).
            OverlayOutcome::OpenWithApp(argv) => {
                self.flush_pending_overlay_settings();
                self.spawn_open_or_notice(&argv);
            }
            OverlayOutcome::ContextMenuCopyPath(abs) => {
                self.flush_pending_overlay_settings();
                let _ = self.clipboard.write_text(&abs);
            }
            // ABOUT: open a project link from the About view. The overlay stays
            // open. Routed through the SAME scheme allowlist + argv-only opener
            // the bare-URL / OSC 8 click paths use — never a shell string. The
            // URLs are hardcoded https project links, but the allowlist guard is
            // kept for defense in depth.
            OverlayOutcome::SettingsOpenUrl(url) => {
                if openable_hyperlink_uri(&url) {
                    let argv = super::platform_opener::open_default_argv(
                        super::platform_opener::OpenerOs::host(),
                        &url,
                    );
                    self.spawn_open_or_notice(&argv);
                }
            }
            // ABOUT: copy the diagnostics block to the clipboard. The overlay
            // stays open; the panel already showed its "copied" confirmation.
            OverlayOutcome::SettingsCopyDiagnostics(text) => {
                let _ = self.clipboard.write_text(&text);
            }
            OverlayOutcome::ContextMenuCopyFile(uri) => {
                self.flush_pending_overlay_settings();
                let _ = self.clipboard.write_text(&uri);
            }
            OverlayOutcome::ContextMenuRevealPath(resolved) => {
                self.flush_pending_overlay_settings();
                let argv = super::platform_opener::reveal_argv(
                    super::platform_opener::OpenerOs::host(),
                    &resolved,
                );
                self.spawn_open_or_notice(&argv);
            }
            OverlayOutcome::PaletteTypeText(text) => {
                self.flush_pending_overlay_settings();
                self.handle_palette_type_text(text);
            }
            OverlayOutcome::PaletteAction(id) => {
                self.flush_pending_overlay_settings();
                self.handle_palette_action(id);
            }
            // Phase 4: the connection-manager overlay closed itself before
            // emitting this; spawn the chosen host through the connect action
            // (system `ssh`, name-only argv). A spawn failure must never panic
            // the UI — surface nothing for now beyond the dropped result; the
            // overlay is already closed and the user can retry.
            OverlayOutcome::Connect(host) => {
                self.flush_pending_overlay_settings();
                let _ = self.connect_ssh_host_in_new_tab(&host);
            }
            // ADHOC-CONNECT: connect to a typed host AND append it to hosts.conf.
            // The connect and the save are independent — a save failure never
            // blocks the connection, and vice versa.
            OverlayOutcome::ConnectAndSave(host) => {
                self.flush_pending_overlay_settings();
                let _ = self.connect_ssh_host_in_new_tab(&host);
                self.save_adhoc_host(&host);
            }
            // REMOTE-UX P4: the Add / Edit connection form closed itself before
            // emitting this; persist the built host — append a new block for Add,
            // byte-splice over the named block for Edit. A write failure surfaces
            // a one-line notice and never panics.
            OverlayOutcome::SaveConnection { host, edit_target } => {
                self.flush_pending_overlay_settings();
                self.persist_connection_form(&host, edit_target);
            }
            // REMOTE-UX P4 / ODP-8: run the Test Connection probe on a
            // background thread; the overlay stays open and shows the tri-state
            // result when it lands.
            OverlayOutcome::TestConnection(host) => {
                self.run_connection_probe(&host);
            }
            // FORM-UX: the IdentityFile field asked to browse. Scan ~/.ssh for
            // candidate private keys (filename heuristics only — never key
            // contents) and seed the in-form browser. An empty scan still opens
            // the browser, which shows a "type a path manually" hint.
            OverlayOutcome::BrowseIdentityKeys => {
                let candidates = self.gather_identity_key_candidates();
                self.overlay.open_identity_key_browse(candidates);
            }
            // Phase 5 / B2: the session-attach overlay closed itself before
            // emitting this; attach the chosen session into a new tab. A stale
            // id (the session ended between list and accept) returns Err from
            // the attach path; swallow it like the connect arm — the overlay is
            // already closed and the user can retry. Never panics.
            OverlayOutcome::AttachSession(id) => {
                self.flush_pending_overlay_settings();
                self.route_attach_session(id);
            }
            // Phase 14: the attach-choice dialog closed itself before emitting
            // these; run the chosen attach. New tab = today's path. Replace =
            // attach + close the tab that was active when the manager opened.
            OverlayOutcome::AttachChoiceNewTab(id) => {
                self.flush_pending_overlay_settings();
                let _ = self.attach_session_in_new_tab(None, &id);
            }
            OverlayOutcome::AttachChoiceReplace(id) => {
                self.flush_pending_overlay_settings();
                self.attach_session_replacing_current(id);
            }
            // Manage Sessions: a right-click on a session row asks to kill it.
            // Open the confirm dialog over the carried id; the manager closes
            // (the dialog replaces it on screen). Confirming routes to
            // KillSessionConfirmed below.
            OverlayOutcome::KillSessionRequest(id) => {
                self.flush_pending_overlay_settings();
                self.reset_pointer_state_for_overlay();
                self.overlay.open_confirm_kill_session(id);
                self.request_selection_redraw();
            }
            // Manage Sessions: the kill was confirmed. Terminate the host and
            // reopen the manager so the now-dead row disappears. A stale/missing
            // socket is treated as already-gone by `kill_session` (Ok), so a
            // double-kill or a race with idle-timeout never panics. The dialog
            // already closed itself before emitting this.
            OverlayOutcome::KillSessionConfirmed(id) => {
                self.flush_pending_overlay_settings();
                // Killing a detached session goes through the Unix-only
                // session-host registry; on Windows there are no detached
                // sessions, so this is a no-op (the overlay still refreshes).
                #[cfg(unix)]
                let _ = crate::session_host::kill_session(None, &id);
                #[cfg(not(unix))]
                let _ = &id;
                self.open_session_attach_overlay();
            }
            // Packet 2: "Detach & switch" was chosen on the focused pane. The
            // menu closed itself; read the focused pane's cwd and open the 3-way
            // choice dialog.
            OverlayOutcome::ContextMenuDetachSwitch => {
                self.flush_pending_overlay_settings();
                self.open_detach_switch_choice();
            }
            // Packet 2: the Detach & switch dialog closed itself before emitting
            // these; run the chosen orchestration. Swap = spawn + attach + close
            // the original pane; Keep both = spawn + attach, original untouched.
            // A spawn/attach failure surfaces a transient notice and leaves the
            // original pane untouched (handled inside the orchestration).
            OverlayOutcome::DetachSwitchSwap(cwd) => {
                self.flush_pending_overlay_settings();
                self.detach_switch_swap(cwd);
            }
            OverlayOutcome::DetachSwitchKeepBoth(cwd) => {
                self.flush_pending_overlay_settings();
                self.detach_switch_keep_both(cwd);
            }
            // CLOSE-CONFIRM: the dialog closed itself before emitting this; flag
            // the exit so `window_event` exits the loop on this same turn (the
            // outcome cannot reach `ActiveEventLoop` from here — `&mut self`).
            OverlayOutcome::ForceClose => {
                self.flush_pending_overlay_settings();
                self.pending_exit = true;
            }
        }
    }

    /// Translate a winit mouse button edge over an open overlay into an
    /// [`OverlayPointer::Press`]/`Release` and apply the outcome (UX4-P1/P2).
    /// Press drives clicks and may arm an overlay drag in modes that still
    /// capture motion (theme builder). Release ends any such drag. Middle/other
    /// buttons are dropped so no PRIMARY paste fires while the overlay is up and
    /// so a stray middle release cannot disturb a drag.
    pub(in crate::native) fn handle_overlay_pointer_button(
        &mut self,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        let pointer_button = match button {
            WinitMouseButton::Left => PointerButton::Left,
            WinitMouseButton::Right => PointerButton::Right,
            _ => return,
        };
        // SLIDER-GUARD (D-SLIDER-GUARD): track whether the left button is held so
        // `handle_overlay_pointer_move` can gate drag updates in modes that
        // still capture motion. Clear BEFORE Release is processed so the
        // Release handler never sees a stale held flag, and set AFTER Press
        // lands so the flag reflects an active drag.
        if button == WinitMouseButton::Left {
            match state {
                ElementState::Released => {
                    self.overlay_left_held = false;
                    self.overlay.cancel_settings_drag();
                }
                ElementState::Pressed => {} // set below, after overlay confirms a drag
            }
        }
        // Phase 13d — lightbox click-outside-to-dismiss. A left PRESS while the
        // C4 image viewer is open intercepts before the generic cell/rect
        // dispatch: outside the drawn image fit-rect closes the viewer exactly
        // like Esc (overlay `Close` → the per-frame sync clears the GPU image →
        // byte-identical next frame); a press ON the image is inert (a
        // conventional lightbox has nothing to interact with, and we avoid a
        // surprise-close on a direct hit). Only when BOTH the fit-rect and the
        // pointer pixel are known — otherwise fall through so there is no
        // regression on the normal overlay path.
        if state == ElementState::Pressed
            && pointer_button == PointerButton::Left
            && self.overlay.image_view_open()
            && let Some(fit_rect) = self.gpu.as_ref().and_then(|g| g.overlay_image_fit_rect())
            && let Some((px, py)) = self.pointer_px
        {
            if point_outside_rect(px, py, fit_rect) {
                let o = self.overlay.handle_input(OverlayInput::Close);
                self.apply_overlay_outcome_with_policy(o, false);
                self.request_selection_redraw();
            }
            return;
        }
        // Window-level overlays use window-overlay cell space, not the focused
        // pane's sub-grid. In a single-pane tab these are exactly
        // `self.pointer_cell` / `self.grid`, so the single-pane path is
        // unchanged; in a multi-pane tab they map to the whole content grid so
        // clicks land on the panel that renders there.
        let Some(cell) = self.overlay_pointer_cell() else {
            if state == ElementState::Released {
                self.flush_pending_overlay_settings();
                self.request_selection_redraw();
            }
            return;
        };
        let (win_cols, win_rows) = self.overlay_grid_dims();
        let Some(rect) = overlay_rect(&self.overlay, win_cols, win_rows) else {
            if state == ElementState::Released {
                self.flush_pending_overlay_settings();
                self.request_selection_redraw();
            }
            return;
        };
        // MENU-DEBOUNCE: swallow a press that lands on a freshly-opened workspace
        // RAIL menu within `CONTEXT_MENU_INPUT_DEBOUNCE` of it opening. Such a
        // press can only be a stale queued click replaying into the just-opened
        // menu -- a human needs longer to see the menu, move to an item, and
        // click -- and letting one through is how a burst of queued presses could
        // activate a workspace-mutating item ("phantom New Workspace"). Scoped to
        // the workspace-rail surfaces (the only ones carrying create/close
        // workspace items, and the exact surface of the observed phantom); Content
        // and tab menus route normally so their open-then-click paths are
        // unchanged. Only presses are gated, so the opening right-click's own
        // release and hover-to-focus moves route normally.
        if state == ElementState::Pressed
            && self.overlay.is_workspace_rail_context_menu()
            && self
                .context_menu_opened_at
                .is_some_and(|opened| opened.elapsed() < super::CONTEXT_MENU_INPUT_DEBOUNCE)
        {
            return;
        }
        let x_in_body = self.pointer_x_in_body(&rect);
        let pointer = match state {
            ElementState::Pressed => OverlayPointer::Press {
                cell,
                button: pointer_button,
                x_in_body,
            },
            ElementState::Released => OverlayPointer::Release {
                cell,
                button: pointer_button,
            },
        };
        let outcome = self.overlay.handle_pointer(pointer, rect);
        // After a left press, arm the held flag only if the overlay confirms a
        // real drag. Settings sliders are click-to-set and leave this false.
        if button == WinitMouseButton::Left && state == ElementState::Pressed {
            self.overlay_left_held = self.overlay.is_settings_dragging();
        }
        let coalesce_apply = state == ElementState::Pressed && self.overlay.is_settings_dragging();
        self.apply_overlay_outcome_with_policy(outcome, coalesce_apply);
        if state == ElementState::Released {
            self.flush_pending_overlay_settings();
        }
        self.request_selection_redraw();
    }

    /// Drive an in-progress overlay drag from the cached pointer cell (UX4-P2).
    /// Gated on an active drag AND the left-button-held flag so cursor movements
    /// after the button is released can never advance an armed drag
    /// (D-SLIDER-GUARD). Ordinary hover over the open overlay stays a cheap
    /// no-op (no redraw, no PTY/selection work).
    pub(in crate::native) fn handle_overlay_pointer_move(&mut self) {
        // A bare hover is forwarded only to advance an active overlay drag
        // (UX4-P2, only when the left button IS held — D-SLIDER-GUARD) or to
        // drive context-menu hover-to-focus (IN2); otherwise it is a cheap no-op.
        let should_route = if self.overlay.is_settings_dragging() {
            // Slider move: require the left button to be held. If the drag state
            // is somehow stale (lost Release event), cancel it and return.
            if !self.overlay_left_held {
                self.overlay.cancel_settings_drag();
                return;
            }
            true
        } else {
            self.overlay.is_context_menu()
        };
        if !should_route {
            return;
        }
        // Window-space overlay geometry (see `handle_overlay_pointer_button`):
        // identical to `self.pointer_cell` / `self.grid` in a single-pane tab,
        // mapped to the content grid in a multi-pane tab.
        let Some(cell) = self.overlay_pointer_cell() else {
            return;
        };
        let (win_cols, win_rows) = self.overlay_grid_dims();
        let Some(rect) = overlay_rect(&self.overlay, win_cols, win_rows) else {
            return;
        };
        let x_in_body = self.pointer_x_in_body(&rect);
        let outcome = self
            .overlay
            .handle_pointer(OverlayPointer::Move { cell, x_in_body }, rect);
        let coalesce_apply = self.overlay.is_settings_dragging();
        self.apply_overlay_outcome_with_policy(outcome, coalesce_apply);
        self.request_selection_redraw();
    }

    /// Translate a winit wheel event over an open overlay into an
    /// [`OverlayPointer::Wheel`] free-scroll of the panel list (UX4-P1).
    pub(in crate::native) fn handle_overlay_pointer_wheel(&mut self, delta: MouseScrollDelta) {
        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        // P1-8: macOS trackpad / Magic-Mouse inertial scroll arrives as a
        // `PixelDelta` burst with a decaying momentum tail that winit does not
        // phase-tag, so the shared cell-height coalescer would fire many list
        // steps per flick (and the ×3 `WHEEL_STEP_LINES` multiplier multiplies
        // each). On macOS route through a SEPARATE overlay-only damper that emits
        // exactly one item per detent and absorbs the tail. Every OTHER target
        // keeps the historical `coalesce_scroll` → `wheel_lines` path verbatim,
        // so Linux (including hi-res `PixelDelta` mice) stays byte-identical by
        // construction — no feature flag, the `cfg!` gate is the only seam.
        let lines = if cfg!(target_os = "macos") {
            // One row/item per detent: the damper returns ±1, dropping the ×3
            // multiplier so the `scroll_lines` overlays move one row and the
            // `handle_input` overlays move one focus-step — unified across both
            // overlay routing groups.
            let Some(step) = self.overlay_wheel.step(delta, cell_height) else {
                return;
            };
            step
        } else {
            // WHEEL-SENS (T-overlay): coalesce the high-resolution burst so the
            // settings list advances one entry per physical notch instead of
            // flying. The overlay deliberately uses the fixed default step (the
            // user's `scroll_wheel_lines` multiplier is a terminal-scroll knob),
            // but it still benefits from notch-coalescing.
            let Some(notch) = self.wheel_accum.coalesce_scroll(delta, cell_height) else {
                return;
            };
            let lines = wheel_lines(notch, cell_height);
            if lines == 0 {
                return;
            }
            lines
        };
        // Window-space overlay dims (identical to `self.grid` single-pane).
        let (win_cols, win_rows) = self.overlay_grid_dims();
        let Some(rect) = overlay_rect(&self.overlay, win_cols, win_rows) else {
            return;
        };
        // Both the `wheel_lines` notch and the damper step are positive for
        // wheel-up (toward earlier content); the list scrolls toward earlier
        // entries (lower index), so negate.
        let outcome = self
            .overlay
            .handle_pointer(OverlayPointer::Wheel { lines: -lines }, rect);
        self.apply_overlay_outcome(outcome);
        self.request_selection_redraw();
    }

    /// Compute the fractional body-relative x coordinate from the cached
    /// physical pixel position. Returns `None` when pixel data or GPU cell info
    /// is unavailable (tests, headless mode).
    ///
    /// The value is body-relative: 0.0 = left edge of the first body cell,
    /// 1.0 = right edge of the first body cell, etc. Non-integer values give
    /// sub-cell precision for smooth slider tracking.
    fn pointer_x_in_body(&self, rect: &crate::native::overlay::OverlayRect) -> Option<f32> {
        let (x_px, _) = self.pointer_px?;
        let cell = self.gpu.as_ref().map(GpuState::cell)?;
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let body_left_px = rect.body_left as f32 * cell.width as f32 + padding.as_f32();
        Some((x_px as f32 - body_left_px) / cell.width.max(1) as f32)
    }

    pub(super) fn update_hover_hyperlink(&mut self) {
        let hovered = self
            .pointer_cell
            .and_then(|point| self.visible_cell_hyperlink(point));
        if self.hovered_hyperlink != hovered {
            self.hovered_hyperlink = hovered;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// INTERACTIVE-PATHS (Phase 7): recompute the resolved path span under the
    /// pointer and update the hover state that drives the pointer (hand) cursor.
    ///
    /// **The byte-identity gate.** The very first thing this does is check the
    /// `interactive_paths` setting; when it is off (the default) it returns
    /// before any terminal lock, row build, `detect_paths` scan, or stat probe —
    /// so the default hover path never scans and produces byte-identical frames.
    /// When on, it dedupes exactly like [`Self::update_hover_hyperlink`]: the
    /// rebuild flag/redraw fire only when the resolved span actually changes.
    pub(super) fn update_hover_path(&mut self) {
        if !self.settings.interactive_paths {
            // Clear a stale span if the setting was toggled off live while one
            // was hovered; otherwise nothing to do — the scanner never runs.
            if self.hovered_path.is_some() || self.hovered_path_cells.is_some() {
                self.hovered_path = None;
                self.hovered_path_cells = None;
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            return;
        }
        let (resolved, cells) = match self.resolved_hovered_path_with_cells() {
            Some((resolved, cells)) => (Some(resolved), Some(cells)),
            None => (None, None),
        };
        // Compare BOTH the resolved entry and the cell span: two occurrences of
        // the same filename on different rows resolve to the same `Resolved`, so
        // the span comparison is what moves the armed underline between them.
        if self.hovered_path != resolved || self.hovered_path_cells != cells {
            self.hovered_path = resolved;
            self.hovered_path_cells = cells;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Resolve the path span (if any) under the current pointer cell against the
    /// pane's OSC 7 working directory and `$HOME`, stat-gated through the active
    /// [`crate::paths::ResolveProbe`]. Pure aside from the single probe call;
    /// `None` when no live filesystem path sits under the pointer. Thin wrapper
    /// over [`Self::resolved_hovered_path_with_cells`] that drops the span — used
    /// by the context-menu path target which only needs the resolved entry.
    pub(super) fn resolved_hovered_path(&self) -> Option<crate::paths::Resolved> {
        self.resolved_hovered_path_with_cells()
            .map(|(resolved, _)| resolved)
    }

    /// INTERACTIVE-URLS: recompute the bare-URL span under the pointer.
    ///
    /// Mirrors [`Self::update_hover_path`]: when `interactive_urls` is off (and
    /// after clearing any stale span) it returns before any terminal lock or
    /// scan, so the default hover path is a single bool test and byte-identical.
    /// When on, it latches the openable URL under the pointer (if any) and fires
    /// a redraw only when the resolved URL or its span actually changes.
    pub(super) fn update_hover_url(&mut self) {
        if !self.settings.interactive_urls {
            if self.hovered_url.is_some() || self.hovered_url_cells.is_some() {
                self.hovered_url = None;
                self.hovered_url_cells = None;
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            return;
        }
        let (url, cells) = match self.resolved_hovered_url_with_cells() {
            Some((url, cells)) => (Some(url), Some(cells)),
            None => (None, None),
        };
        if self.hovered_url != url || self.hovered_url_cells != cells {
            self.hovered_url = url;
            self.hovered_url_cells = cells;
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Find the bare (non-OSC-8) URL under the pointer cell and its visible-cell
    /// span. Runs the shared, tested [`crate::hints`] URL scanner over the single
    /// hovered row, picks the match covering the pointer column, and keeps it
    /// only when its scheme is openable ([`openable_hyperlink_uri`]). Returns
    /// `None` when no URL sits under the pointer, when the scheme is not openable
    /// (e.g. `ftp`/`ssh` are detected but not opened), or when the hovered cell
    /// already carries an OSC 8 hyperlink — that explicit path wins, so a cell is
    /// never double-decorated. One terminal lock, no filesystem or network access.
    fn resolved_hovered_url_with_cells(
        &self,
    ) -> Option<(String, super::click_hint::HoverPathCells)> {
        let point = self.pointer_cell?;
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        let cols = snapshot.dimensions.columns;
        if cols == 0 || point.row >= snapshot.dimensions.rows {
            return None;
        }
        let start = point.row * cols;
        let row_cells = snapshot.cells.get(start..start + cols)?;
        // OSC 8 wins: an explicit hyperlink under the pointer is handled by the
        // OSC 8 path, so never light the bare-URL decoration on the same cell.
        if row_cells
            .get(point.column)
            .and_then(|cell| cell.attrs.hyperlink)
            .is_some()
        {
            return None;
        }
        let rows = [crate::core::SearchRow {
            cells: row_cells,
            wrapped: false,
        }];
        let matched = crate::hints::scan(&rows, crate::hints::HintKinds::URLS)
            .into_iter()
            .find(|m| {
                m.start.row == 0 && m.start.column <= point.column && point.column <= m.end.column
            })?;
        if !openable_hyperlink_uri(&matched.text) {
            return None;
        }
        let cells = super::click_hint::HoverPathCells {
            row: point.row,
            start: matched.start.column,
            end: matched.end.column + 1,
        };
        Some((matched.text, cells))
    }

    /// As [`Self::resolved_hovered_path`], but also returns the visible-cell span
    /// (UX-A): the row and column range the detected path occupies, so the
    /// Ctrl+hover armed underline can decorate exactly those cells. The span's
    /// byte offsets are mapped to column indices by counting chars (correct for
    /// any multi-byte content earlier in the row, though paths are ASCII/narrow).
    pub(super) fn resolved_hovered_path_with_cells(
        &self,
    ) -> Option<(crate::paths::Resolved, super::click_hint::HoverPathCells)> {
        let point = self.pointer_cell?;
        let (line, column, cwd) = self.hovered_row_text_and_cwd(point)?;
        // Map the pointer's cell column to a byte offset in the row string. Paths
        // are ASCII/narrow, so one char per cell column keeps the column and char
        // indices aligned.
        let target = line.char_indices().nth(column).map(|(byte, _)| byte)?;
        let options = crate::paths::DetectionOptions {
            barewords: self.settings.interactive_paths_barewords,
        };
        // Stat-guided span expansion: the scanner tokenizes on whitespace, so a
        // filename containing a space is split into separate tokens and never
        // resolves as one. Probe the contiguous token-run candidates that include
        // the hovered token, longest-first; the FIRST one the stat-gate confirms
        // exists wins. This picks the most-specific existing name (`my notes.txt`
        // over `notes.txt`) while prose runs that name no real file stay inert.
        // The single hovered token is always among the candidates, so a spaceless
        // filename resolves byte-identically to the previous single-span path.
        for span in crate::paths::detect_path_candidates_at(&line, target, options) {
            let Some(resolved) =
                self.classify_hovered_path(&span, cwd.as_deref(), self.home_dir.as_deref())
            else {
                continue;
            };
            // Panic-free byte→column mapping: count chars whose byte offset is
            // below the span boundary (never indexes a String slice at a raw
            // byte).
            let start = line
                .char_indices()
                .filter(|(byte, _)| *byte < span.start)
                .count();
            let end = line
                .char_indices()
                .filter(|(byte, _)| *byte < span.end)
                .count();
            let cells = super::click_hint::HoverPathCells {
                row: point.row,
                start,
                end,
            };
            return Some((resolved, cells));
        }
        None
    }

    /// UX-A (Phase 11): note a plain left-click that landed on a resolved path
    /// but did NOT open (the open-modifier gate failed — Ctrl on Linux, Cmd on
    /// macOS) — the "I clicked,
    /// nothing happened" mis-click. Raises the bottom-left teaching hint once two
    /// such mis-clicks land within the window. Gated INSIDE `interactive_paths`
    /// AND `interactive_paths_click_hint`; a no-op (no redraw) on every other
    /// path, so feature-off frames are byte-identical. Called from the left-press
    /// arm only when neither open helper fired.
    pub(super) fn note_possible_path_misclick(&mut self) {
        if !self.settings.interactive_paths || !self.settings.interactive_paths_click_hint {
            return;
        }
        // Only a click that actually landed on a resolved path counts.
        if self.hovered_path.is_none() {
            return;
        }
        // If the open gate WOULD have fired, this is not a mis-click (the open
        // path already handled it before we were called).
        if hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return;
        }
        if self.click_hint.note_misclick(std::time::Instant::now()) {
            self.request_selection_redraw();
        }
    }

    /// Single-lock fetch of the row text under `point` plus the pane's OSC 7
    /// working directory. Mirrors [`Self::visible_cell_hyperlink`]'s one-lock
    /// structure: the row string and the cwd both come from the same `terminal`
    /// lock. The row is built one char per cell column so a column index maps to
    /// a char index.
    fn hovered_row_text_and_cwd(
        &self,
        point: CellPoint,
    ) -> Option<(String, usize, Option<String>)> {
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        let cols = snapshot.dimensions.columns;
        if point.row >= snapshot.dimensions.rows {
            return None;
        }
        let start = point.row * cols;
        let row = snapshot.cells.get(start..start + cols)?;
        let line: String = row.iter().map(|cell| cell.ch).collect();
        let cwd = terminal.current_working_directory().map(str::to_owned);
        Some((line, point.column, cwd))
    }

    /// Stat-gate a candidate span through the production probe. Split on
    /// `cfg(test)` so headless hover tests resolve against an injected synthetic
    /// fs map (`test_path_probe`) and never touch the real filesystem, while
    /// production wires the real `std::fs::symlink_metadata` probe.
    #[cfg(not(test))]
    fn classify_hovered_path(
        &self,
        span: &crate::paths::PathSpan,
        cwd: Option<&str>,
        home: Option<&str>,
    ) -> Option<crate::paths::Resolved> {
        crate::paths::resolve(span, cwd, home, &super::interactive_paths::FsResolveProbe)
    }

    #[cfg(test)]
    fn classify_hovered_path(
        &self,
        span: &crate::paths::PathSpan,
        cwd: Option<&str>,
        home: Option<&str>,
    ) -> Option<crate::paths::Resolved> {
        crate::paths::resolve(span, cwd, home, &self.test_path_probe)
    }

    fn visible_cell_hyperlink(&self, point: CellPoint) -> Option<LinkId> {
        if point.row >= self.grid.rows || point.column >= self.grid.columns {
            return None;
        }
        let terminal = self.terminal.lock().ok()?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        snapshot
            .cells
            .get(point.row * snapshot.dimensions.columns + point.column)
            .and_then(|cell| cell.attrs.hyperlink)
    }

    fn hovered_hyperlink_uri(&self) -> Option<String> {
        let id = self.hovered_hyperlink?;
        self.terminal
            .lock()
            .ok()?
            .hyperlink(id)
            .map(|link| link.uri.clone())
    }

    pub(super) fn try_open_hovered_hyperlink(&mut self) -> bool {
        if !hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return false;
        }
        let Some(uri) = self.hovered_hyperlink_uri() else {
            return false;
        };
        if !openable_hyperlink_uri(&uri) {
            return false;
        }

        // Security: OdyTTY never auto-opens OSC 8 links. A URI is opened only
        // after an explicit modifier+click (Ctrl on Linux, Cmd on macOS),
        // scheme allowlist filtering, and direct
        // argv passing to the platform default opener. No shell interpolation is
        // involved. Routed through the single argv-only spawn point shared with
        // path opens; a failed/missing opener surfaces a transient notice (P0-2).
        let argv = super::platform_opener::open_default_argv(
            super::platform_opener::OpenerOs::host(),
            &uri,
        );
        self.spawn_open_or_notice(&argv);
        true
    }

    /// INTERACTIVE-PATHS (Phase 8 / C3): modifier+click open for a resolved
    /// path span under the pointer (Ctrl on Linux, Cmd on macOS). Chained in the
    /// pointer Pressed arm AFTER
    /// [`Self::try_open_hovered_hyperlink`] (OSC 8 wins ties) and BEFORE
    /// `begin_selection`, so when this returns `false` the selection path is
    /// byte-identical.
    ///
    /// Returns `false` immediately — opening nothing, starting no selection
    /// change — when the feature is off, the open-modifier gate is not
    /// satisfied, or no live path span sits under the pointer. The gate reused
    /// is exactly the hyperlink one ([`hyperlink_action_allowed`]): the platform
    /// open modifier required (Ctrl on Linux, Cmd on macOS), suppressed under
    /// mouse reporting unless Shift overrides. The open itself
    /// is an argv-only [`super::interactive_paths::spawn_detached`] of the
    /// dispatch vector ([`super::interactive_paths::path_open_argv`]) — never a
    /// shell string.
    pub(super) fn try_open_hovered_path(&mut self) -> bool {
        if !self.settings.interactive_paths {
            return false;
        }
        if !hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return false;
        }
        let Some(resolved) = self.hovered_path.clone() else {
            return false;
        };
        if interactive_path_open_kind(&self.settings, &resolved)
            == InteractivePathOpenKind::InlineImage
            && self.open_image_view(&resolved)
        {
            return true;
        }
        let argv = self.path_open_argv_for(&resolved);
        self.spawn_open_or_notice(&argv);
        true
    }

    /// INTERACTIVE-URLS: modifier+click open for a bare (non-OSC-8) URL span
    /// under the pointer (Ctrl on Linux, Cmd on macOS). Chained in the pointer
    /// Pressed arm AFTER [`Self::try_open_hovered_hyperlink`] and
    /// [`Self::try_open_hovered_path`] (OSC 8 and resolved paths win ties), before
    /// `begin_selection`, so a `false` return leaves the selection path
    /// byte-identical.
    ///
    /// Returns `false` immediately — opening nothing, starting no selection
    /// change — when the feature is off, the open-modifier gate is not satisfied,
    /// or no openable URL sits under the pointer. The gate and the open dispatch
    /// are exactly the OSC 8 ones: [`hyperlink_action_allowed`] (platform open
    /// modifier, suppressed under mouse reporting unless Shift overrides),
    /// [`openable_hyperlink_uri`] scheme allowlist, and the argv-only
    /// [`super::platform_opener::open_default_argv`] dispatch — never a shell
    /// string, never auto-opened.
    pub(super) fn try_open_hovered_url(&mut self) -> bool {
        if !self.settings.interactive_urls {
            return false;
        }
        if !hyperlink_action_allowed(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return false;
        }
        let Some(uri) = self.hovered_url.clone() else {
            return false;
        };
        if !openable_hyperlink_uri(&uri) {
            return false;
        }
        let argv = super::platform_opener::open_default_argv(
            super::platform_opener::OpenerOs::host(),
            &uri,
        );
        self.spawn_open_or_notice(&argv);
        true
    }

    /// Build the argv vector to open a resolved path, threading the configured
    /// editor override (`interactive_paths_editor`) and the `$EDITOR`/`$VISUAL`
    /// environment (read at open time). Pure aside from the env read; the spawn
    /// is the caller's separate step. Shared by the Ctrl+click path and the
    /// context-menu Open item so both dispatch identically.
    pub(super) fn path_open_argv_for(&self, resolved: &crate::paths::Resolved) -> Vec<String> {
        let editor_env = std::env::var("EDITOR")
            .ok()
            .or_else(|| std::env::var("VISUAL").ok());
        super::interactive_paths::path_open_argv(
            resolved,
            &self.settings.interactive_paths_editor,
            editor_env.as_deref(),
            super::platform_opener::OpenerOs::host(),
        )
    }

    /// Open a resolved image span in the in-terminal viewer (Phase 9 / C4).
    /// Decodes the file through the single bounded decode point
    /// ([`super::image_decode::decode_image_rgba`], FLAG B), uploads the pixels
    /// to the GPU image layer, and opens the `ImageView` overlay. Returns
    /// `false` when the resolved target is not a file, decode is refused/fails,
    /// or no GPU image layer exists; Ctrl+click uses that to fall back to the
    /// external opener. The context-menu action keeps its historical no-op on
    /// failure by ignoring this return value. Presentation-only.
    pub(super) fn open_image_view(&mut self, resolved: &crate::paths::Resolved) -> bool {
        // Only files are images; a directory span never reaches here, but guard
        // anyway so the decode is never attempted on a non-file.
        if resolved.kind != crate::paths::FsKind::File {
            return false;
        }
        let decode_started = Instant::now();
        let Some((rgba, width, height)) =
            crate::native::image_decode::decode_image_rgba(std::path::Path::new(&resolved.abs))
        else {
            return false;
        };
        let decode_elapsed = decode_started.elapsed();
        let Some(gpu) = self.gpu.as_mut() else {
            return false;
        };
        // Hand the pixels to the GPU overlay slot (centered fit computed there),
        // then open the presentation-only overlay with the filename caption.
        let upload_started = Instant::now();
        gpu.set_overlay_image(Some((rgba.as_slice(), width, height)));
        let upload_elapsed = upload_started.elapsed();
        tracing::debug!(
            width,
            height,
            decode_ms = decode_elapsed.as_millis(),
            upload_ms = upload_elapsed.as_millis(),
            "inline image viewer loaded"
        );
        let caption = resolved
            .abs
            .rsplit('/')
            .next()
            .unwrap_or(resolved.abs.as_str())
            .to_owned();
        self.overlay.open_image_view(caption);
        self.image_overlay = Some(super::interactive_paths::ImageOverlayState {
            rgba,
            width,
            height,
        });
        self.needs_rebuild = true;
        true
    }

    /// Keep the GPU image-viewer overlay in lockstep with the overlay state
    /// (C4). Called once per frame before drawing: when the `ImageView` overlay
    /// is no longer open (dismissed via Esc, click-outside, or any mode switch),
    /// clear the decoded buffer and the GPU overlay texture so the very next
    /// frame is byte-identical to the no-viewer path. Cheap no-op while the
    /// viewer stays open or was never opened.
    pub(super) fn sync_image_overlay(&mut self) {
        if self.image_overlay.is_some() && !self.overlay.image_view_open() {
            self.image_overlay = None;
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.set_overlay_image(None);
            }
        }
    }

    /// Re-push the current image-viewer overlay image after a surface resize so
    /// its centered fit-rect is recomputed for the new dimensions (C4). No-op
    /// when the viewer is closed.
    pub(super) fn refresh_image_overlay_on_resize(&mut self) {
        if let Some(state) = self.image_overlay.as_ref() {
            let rgba = state.rgba.clone();
            let (width, height) = (state.width, state.height);
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.set_overlay_image(Some((rgba.as_slice(), width, height)));
            }
        }
    }

    pub(super) fn write_pty_bytes(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn send_mouse_report(&mut self, button: CoreMouseButton, kind: MouseEventKind) -> bool {
        let protocol = self.mouse_protocol();
        // SGR-pixel (1016) reports true 1-based physical pixel coordinates; every
        // other encoding (legacy/UTF-8/SGR/urxvt) reports cells. Only 1016 takes
        // the pixel seam — the cell path is untouched for all other modes.
        let bytes = if protocol.encoding == MouseEncoding::SgrPixel {
            self.encode_pixel_mouse_report(protocol, button, kind)
        } else {
            self.pointer_cell.and_then(|point| {
                encode_native_mouse_report(protocol, point, button, kind, self.modifiers)
            })
        };
        let Some(bytes) = bytes else {
            return false;
        };

        self.return_to_live();
        self.write_pty_bytes(&bytes);
        true
    }

    /// Encode an SGR-pixel (1016) mouse report from the cached physical pointer
    /// position. Returns `None` until a cursor position and GPU cell metrics are
    /// available, or when the active tracking gate drops the event (the core
    /// encoder applies the same gating as the cell path). The grid is drawn at
    /// the window origin, so the cached physical position is already
    /// grid-relative; [`pixel_coords_for_report`] floors it to a 1-based pixel
    /// and clamps to the grid's pixel extent after removing any window padding.
    fn encode_pixel_mouse_report(
        &self,
        protocol: MouseProtocol,
        button: CoreMouseButton,
        kind: MouseEventKind,
    ) -> Option<Vec<u8>> {
        let (x_px, y_px) = self.pointer_px?;
        let gpu = self.gpu.as_ref()?;
        let cell = gpu.cell();
        let (px, py) = pixel_coords_for_report(x_px, y_px, cell, self.grid, gpu.window_padding());
        let mods = MouseModifiers {
            // Shift stays reserved for local selection while reporting is active,
            // matching the cell path's modifier policy.
            shift: false,
            alt: self.modifiers.alt,
            ctrl: self.modifiers.ctrl,
        };
        encode_mouse_event_pixel(protocol, button, kind, px, py, mods)
    }

    fn send_mouse_motion_report(&mut self) {
        let protocol = self.mouse_protocol();
        let Some(button) = motion_report_button(protocol, self.report_button) else {
            return;
        };
        let _ = self.send_mouse_report(button, MouseEventKind::Motion);
    }

    pub(super) fn send_focus_report(&mut self, focused: bool) {
        let Some(bytes) = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| encode_native_focus_report(&terminal, focused))
        else {
            return;
        };

        self.write_pty_bytes(&bytes);
    }

    /// BLACK-SCREEN-ON-RESTORE: clear the minimized flag and reset the
    /// skipped-frame retry budget when the window returns from minimized via a
    /// signal OTHER than a non-zero `Resized` (on Windows a restore can fire
    /// only `Focused(true)` / `Occluded(false)`). While `window_minimized`
    /// stays set, the first surface acquire after restore returns `Skipped` and
    /// [`should_schedule_skipped_retry`] vetoes the retry-wake, so no frame
    /// paints until an unrelated input event — the window is black until a
    /// click. Clearing the flag + resetting the budget lets the bounded retry
    /// schedule, and a repaint is requested so the recovered surface paints.
    ///
    /// Mirrors the recovery the non-zero `Resized` arm already performs, and is
    /// idempotent: a normal restore also fires `Resized(non-zero)` which already
    /// cleared the flag, so this is then a no-op; and on Linux/macOS, where
    /// un-minimize goes through `Resized`, the flag is already false by the time
    /// `Focused`/`Occluded` fire, so the callers below do nothing. Returns
    /// whether a minimized state was actually cleared.
    pub(super) fn restore_from_minimized(&mut self) -> bool {
        if !self.window_minimized {
            return false;
        }
        self.window_minimized = false;
        self.consecutive_skipped_frames = 0;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    /// Handle `WindowEvent::Focused`. Factored out of the event arm (so it needs
    /// no `ActiveEventLoop` and is unit-testable) with behavior unchanged except
    /// the added minimize-restore recovery: gaining focus while minimized
    /// (Windows restore-without-`Resized`) clears the flag so the vetoed repaint
    /// can schedule. The redraw the arm already requests then actually paints.
    pub(super) fn on_window_focus_changed(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            // Read the reset-immune episode before restore clears the bounded
            // retry counter. A fresh focus gain remains byte-identical.
            if self.skip_episode.is_active() {
                self.pending_surface_reconfigure = true;
            }
            // A restore may deliver `Focused(true)` before (or without) a
            // non-zero `Resized`; recover the paint here. No-op when not
            // minimized, so the ordinary focus-gain path is unchanged.
            self.restore_from_minimized();
        } else {
            self.window_pointer_px = None;
            self.cancel_overlay_drag_on_focus_loss();
            self.pointer_left_held = false;
            self.pointer_drag = PointerDrag::None;
            self.divider_drag = None;
            self.rail_seam_drag = false;
            self.tab_bar_seam_drag = false;
            self.rail_ws_drag = None;
            self.top_tab_drag = None;
            self.report_button = None;
            // NF21-8: an alt-tab can deliver the button release to another
            // window, stranding the grid selection's held flag. Drop it so a
            // `CursorMoved` on focus regain cannot resume a buttonless drag.
            self.grid_left_held = false;
            // WHEEL-SENS (T-reset): drop any partially-accumulated wheel
            // notch so a gesture interrupted by an alt-tab does not
            // resume against the next surface on focus regain.
            self.wheel_accum.reset();
            // SCROLL-FEEL Tier 2: drop any sub-row scroll remainder too, so a
            // partial continuous glide does not resume after an alt-tab.
            let token = self.sessions.active_id();
            self.clear_scroll_frac_of(token);
            // P1-8: drop the overlay damper's pixel carry too, for the
            // same reason (a half-detent flick must not resume later).
            self.overlay_wheel.reset();
            // §7: drop any pending multiplexer prefix on focus loss so a
            // half-entered prefix does not survive an alt-tab and capture
            // the first key on focus regain.
            self.prefix_engine.cancel();
        }
        // Force the cursor solid-on immediately on focus loss (and
        // resume blinking on focus gain) by rebuilding next frame.
        self.needs_rebuild = true;
        // ID2 focus dimming: a focus transition changes the effective
        // focus-dim amount applied to every cell, so the cell geometry
        // (not just the cursor) must be rebuilt. Bump the presentation
        // epoch — folded into the content render signature — so this
        // frame resolves to a Full geometry update rather than a
        // CursorOnly/Retained one. Harmless when focus_dim is off (the
        // rebuilt vertices are byte-identical).
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        self.send_focus_report(focused);
    }

    /// Handle `WindowEvent::Occluded`. Only the un-occlude (`false`) direction is
    /// acted on: on some platforms a Windows restore surfaces as
    /// `Occluded(false)` without a non-zero `Resized`, leaving the window black
    /// until a click. The occlude (`true`) direction is deliberately NOT treated
    /// as a minimize — occlusion (another window covering ours) is not minimize
    /// on every platform, and true minimize is already tracked via the 0x0
    /// `Resized` path — so setting the flag here could wrongly suppress repaints
    /// of a merely-covered window. `restore_from_minimized` is a no-op unless a
    /// minimized state is actually pending, so this is harmless on Linux/macOS
    /// where un-minimize goes through `Resized`.
    pub(super) fn on_window_occluded(&mut self, occluded: bool) -> bool {
        if !occluded {
            // Wayland workspace return is commonly not a minimize, so restore
            // cannot be relied on to request the redraw that consumes this flag.
            let recovering_skip_episode = self.skip_episode.is_active();
            if recovering_skip_episode {
                self.pending_surface_reconfigure = true;
            }
            self.restore_from_minimized();
            if recovering_skip_episode {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                return true;
            }
        }
        false
    }

    pub(super) fn handle_reported_mouse_input(
        &mut self,
        state: ElementState,
        button: CoreMouseButton,
    ) {
        match state {
            ElementState::Pressed => {
                self.report_button = Some(button);
                let _ = self.send_mouse_report(button, MouseEventKind::Press);
            }
            ElementState::Released => {
                let _ = self.send_mouse_report(button, MouseEventKind::Release);
                if self.report_button == Some(button) {
                    self.report_button = None;
                }
            }
        }
    }

    pub(super) fn handle_reported_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        // WHEEL-SENS (T-overlay decision, TUI arm): coalesce the burst so a
        // high-resolution scroll emits one wheel report per physical notch
        // rather than one per sub-notch event (which would fly a TUI pager). The
        // report protocol carries only a discrete up/down button — sign, not
        // magnitude — so we emit a single report per accumulated notch and
        // deliberately do NOT apply the user's `scroll_wheel_lines` multiplier
        // (the app owns its own line count). A clean `LineDelta(_, ±1.0)` still
        // yields exactly one report, byte-identical to before.
        let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
        let Some(notch) = self.wheel_accum.coalesce_scroll(delta, cell_height) else {
            return false;
        };
        let Some(button) = wheel_report_button(notch) else {
            return false;
        };
        self.send_mouse_report(button, MouseEventKind::Press)
    }

    /// Push a mouse-cursor shape to the window, but only when it actually
    /// changes — winit issues a platform request on every `set_cursor` call, so
    /// the dedupe keeps `CursorMoved` (which fires on every pixel of motion)
    /// from spamming the windowing system. The terminal grid shows an I-beam
    /// (`Text`), a hovered hyperlink shows a hand (`Pointer`), and window chrome
    /// (tab bar, open overlay) plus mouse-reporting TUIs show the arrow
    /// (`Default`). Before this, OdyTTY never called `set_cursor` at all, so the
    /// pointer stayed the OS default arrow everywhere.
    pub(super) fn apply_cursor_icon(&mut self, icon: CursorIcon) {
        if self.cursor_icon == icon {
            return;
        }
        self.cursor_icon = icon;
        if let Some(window) = self.window.as_ref() {
            window.set_cursor(icon);
        }
    }

    /// The resize cursor for a divider of the given split axis: a column split
    /// (panes side-by-side, vertical divider) drags horizontally → `ColResize`
    /// (`↔`); a row split (panes stacked, horizontal divider) drags vertically →
    /// `RowResize` (`↕`). Pure mapping, shared by the hover and active-drag
    /// cursor paths so both agree.
    pub(super) fn divider_resize_icon(axis: SplitAxis) -> CursorIcon {
        match axis {
            SplitAxis::Columns => CursorIcon::ColResize,
            SplitAxis::Rows => CursorIcon::RowResize,
        }
    }

    /// True when, in a multi-pane tab, the pointer is over a pane OTHER than the
    /// focused one — or in a divider gap with no pane content beneath it. This is
    /// the case where hover resolution must be suppressed: `self.grid` /
    /// `self.terminal` belong to the focused pane, so mapping an off-pane pointer
    /// into them resolves a false link/path/URL. Always `false` on a single-pane
    /// tab (`multipane_geometry` is `None`), keeping the single-pane and
    /// focused-pane hover paths byte-identical.
    fn pointer_over_nonfocused_pane(&self) -> bool {
        let Some((content, _)) = self.multipane_geometry() else {
            return false;
        };
        let Some((px, py)) = self.pointer_px else {
            return false;
        };
        match self
            .sessions
            .active_pane_at_point(content, PANE_DIVIDER_PX, px as f32, py as f32)
        {
            Some(token) => token != self.sessions.active_id(),
            None => true,
        }
    }

    /// Drop any hovered hyperlink / path / URL span (and the armed-underline
    /// cells that mirror them), requesting a rebuild/redraw only when something
    /// was actually cleared. Used when hover must be suppressed (pointer over a
    /// non-focused pane) so a stale span from a prior focused hover does not
    /// keep a hand cursor or decoration alive.
    fn clear_hovered_link_spans(&mut self) {
        let had_span = self.hovered_hyperlink.is_some()
            || self.hovered_path.is_some()
            || self.hovered_path_cells.is_some()
            || self.hovered_url.is_some()
            || self.hovered_url_cells.is_some();
        if !had_span {
            return;
        }
        self.hovered_hyperlink = None;
        self.hovered_path = None;
        self.hovered_path_cells = None;
        self.hovered_url = None;
        self.hovered_url_cells = None;
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    pub(super) fn update_pointer_cell(&mut self, x_px: f64, y_px: f64) {
        self.window_pointer_px = Some((x_px, y_px));
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        self.pointer_px = Some((x_px, y_px));
        // RAIL-DRAG: while a workspace-rail drag gesture is live, pointer motion
        // arms it past the threshold and tracks the drop target, and nothing else
        // — a drag owns the pointer for its lifetime. Placed before the auto-hide
        // reveal feed and every hover/selection path so the gesture is not
        // disturbed by them. Inert when no rail drag is in flight.
        if self.pointer_left_held && self.rail_ws_drag.is_some() {
            self.drag_workspace_to_pointer(x_px, y_px, cell);
            return;
        }
        if self.pointer_left_held && self.top_tab_drag.is_some() {
            self.drag_top_tab_to_pointer(x_px, y_px, cell);
            return;
        }
        // A grabbed floating-rail seam owns pointer motion before the auto-hide
        // band hover path. The pointer remains inside that band while shrinking
        // the rail, so handling hover first would swallow the resize motion.
        if self.pointer_left_held && self.rail_seam_drag {
            self.drag_rail_seam_to_pointer(x_px);
            self.apply_cursor_icon(CursorIcon::ColResize);
            return;
        }
        // F4-P3 rail auto-hide: feed the live pointer to the reveal machine
        // (arms/holds/hides the floating overlay). While the rail is revealed and
        // the pointer is over its band, the overlay owns the pointer — do rail
        // hover and nothing else, so a click there hits the rail, not the
        // terminal beneath it. Inert unless autohide is active.
        if self.rail_autohide_active() {
            self.update_rail_autohide_pointer(x_px, cell, Instant::now());
            if let Some(side) = self.rail_autohide_side()
                && self.rail_overlay_visible()
                && self.pointer_in_reveal_band(x_px, cell, side)
            {
                // The content-facing seam is part of the floating band, but it
                // owns that thin grab region so the resize cursor remains
                // discoverable. All other band motion stays rail-only.
                if !self.pointer_over_rail_seam(x_px, cell) {
                    self.update_rail_overlay_hover(x_px, y_px, cell, side);
                    return;
                }
            }
        }
        // While the tab-bar bottom seam is grabbed, pointer motion resizes the
        // bar height (sets the manual rows + reflows) and nothing else. The seam
        // is a horizontal edge -> a row-resize cursor for the gesture. Held only
        // while the top bar is shown, so the rail / single-pane path is
        // unaffected.
        if self.pointer_left_held && self.tab_bar_seam_drag {
            self.drag_tab_bar_seam_to_pointer(y_px);
            self.apply_cursor_icon(CursorIcon::RowResize);
            return;
        }
        // While a divider is grabbed, pointer motion reflows the split and
        // nothing else — no selection or hover work. `divider_drag` is only ever
        // `Some` in a multi-pane tab, so the single-pane motion path below is
        // byte-identical. Keep the matching resize cursor (`↔`/`↕`) for the
        // dragged divider's axis even as the pointer strays off the hairline, so
        // the affordance is stable through the whole gesture; fall back to the
        // arrow only if the divider can't be resolved.
        if let Some(idx) = self.divider_drag.filter(|_| self.pointer_left_held) {
            self.drag_divider_to_pointer();
            let icon = self
                .multipane_geometry()
                .and_then(|(content, _)| {
                    self.sessions
                        .active_divider_axis(content, PANE_DIVIDER_PX, idx)
                })
                .map(Self::divider_resize_icon)
                .unwrap_or(CursorIcon::Default);
            self.apply_cursor_icon(icon);
            return;
        }
        // F4-P4: rail seam hover — a column-resize cursor over the seam grab
        // band so drag-to-resize is discoverable (the press path grabs the same
        // band). Wins over tab-slot hover in its thin band and yields to the
        // scroll thumb (inside `pointer_over_rail_seam`); skipped while a
        // selection is in progress. Clears any stale slot highlight beneath the
        // resize cursor. Inert off a rail, so the plain hover path is unchanged.
        if !self.pointer_drag.is_selecting() && self.pointer_over_rail_seam(x_px, cell) {
            if self.tab_rail.hover.is_some() {
                self.tab_rail.set_hover(None);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            self.apply_cursor_icon(CursorIcon::ColResize);
            return;
        }
        // Tab-bar bottom-seam hover — a row-resize cursor over the seam grab band
        // so drag-to-resize the bar height is discoverable (the press path grabs
        // the same band). Wins over tab-slot hover in its thin band; skipped
        // while a selection is in progress. Clears any stale slot highlight
        // beneath the resize cursor. Inert when no top bar is shown.
        if !self.pointer_drag.is_selecting() && self.pointer_over_tab_bar_seam(x_px, y_px, cell) {
            if self.tab_bar.hover.is_some() {
                self.tab_bar.set_hover(None);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            self.apply_cursor_icon(CursorIcon::RowResize);
            return;
        }
        // Tab-chrome hover: a vertical rail hit-tests with its own row-major
        // X-band and tracks hover on `tab_rail`; the top bar keeps the
        // column-major test on `tab_bar` (F4-V2). Whichever is inactive has its
        // hover cleared so a stale highlight can't linger after a placement flip.
        // Tab-chrome hover (dual band). `current_chrome_hit` resolves the rail
        // first (full-height sidebar) then the top bar; whichever the pointer is
        // over gets its widget hover set and the other cleared. Under workspace-
        // rail auto-hide, only the pinned-rail lookup is skipped: the floating
        // rail is handled earlier, while the top strip remains hoverable.
        let chrome_hit = if self.rail_autohide_active() {
            self.current_top_bar_hit()
        } else {
            self.current_chrome_hit()
        };
        let (tab_bar_hit, hit_is_rail) = match chrome_hit {
            Some((ChromeBand::WorkspaceRail, hit)) => {
                let hover = Some(hit);
                if self.tab_rail.hover != hover {
                    self.tab_rail.set_hover(hover);
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
                self.tab_bar.set_hover(None);
                (hover, true)
            }
            Some((ChromeBand::TopBar, hit)) => {
                let hover = Some(hit);
                if self.tab_bar.hover != hover {
                    self.tab_bar.set_hover(hover);
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
                self.tab_rail.set_hover(None);
                (hover, false)
            }
            None => {
                self.tab_bar.set_hover(None);
                self.tab_rail.set_hover(None);
                (None, false)
            }
        };
        if tab_bar_hit.is_some() {
            // Record a benign pointer cell in the chrome region so grid selection
            // / link hover below are skipped; the press path resolves the actual
            // action via `current_chrome_hit` (rail or bar), not this cell.
            let point = if hit_is_rail {
                let y = (y_px as f32 - padding.as_f32()).max(0.0);
                let row = (y / cell.height as f32) as usize;
                CellPoint {
                    row: row.min(self.tab_rail_grid_rows().saturating_sub(1)),
                    column: 0,
                }
            } else {
                let x = (x_px as f32 - padding.as_f32()).max(0.0);
                let col = (x / cell.width as f32) as usize;
                CellPoint {
                    row: 0,
                    column: col.min(self.tab_bar_grid_cols().saturating_sub(1)),
                }
            };
            self.pointer_cell = Some(point);
            self.apply_cursor_icon(CursorIcon::Default);
            return;
        }
        // Map into content-relative space by subtracting the tab chrome: the top
        // bar shifts Y, the left rail shifts X (F4-V2). Byte-identical on the
        // plain path (both offsets 0). Multi-pane maps via the pane content rect
        // (already reserve-offset) inside `active_pane_pointer_cell`, so only the
        // single-pane fallback below consumes these adjusted coordinates.
        let (chrome_dx, chrome_dy) = self.tab_chrome_offset_px(cell);
        let x_px = x_px - chrome_dx;
        let y_px = y_px - chrome_dy;
        // In a multi-pane tab the focused pane's grid is offset from the window
        // origin, so the pointer must map relative to that pane's sub-rect — the
        // basis `self.grid` / `self.selection` use. On a single-pane tab
        // `active_pane_pointer_cell` is `None` and this falls back to the
        // byte-identical window-origin mapping.
        let point = self.active_pane_pointer_cell().unwrap_or_else(|| {
            selection::cell_at_physical_with_padding(x_px, y_px, cell, self.grid, padding)
        });
        self.pointer_cell = Some(point);
        // F4-RENAME-MOUSE: while a rename drag is live the field owns the
        // pointer — extend its selection to the new cell and stop, before any
        // grid hover/selection/PTY-report work. The rename modal renders only on
        // the single-pane path, so `self.grid` / `self.pointer_cell` are its
        // exact render basis here. `rename_dragging` is only ever set while the
        // modal is open, so this is inert on every other path.
        if self.rename_dragging {
            self.apply_cursor_icon(CursorIcon::Text);
            self.rename_drag_extend();
            return;
        }
        // UX4-P1/P2: while an overlay is open it owns the pointer. Keep caching
        // the coordinates above (a press needs them), but skip link hover, local
        // selection, and PTY motion reports — they belong to the terminal grid
        // beneath the panel. A move is forwarded to the overlay only to advance
        // an active slider drag (UX4-P2); non-drag hover is a no-op.
        if self.overlay.is_open() {
            self.apply_cursor_icon(CursorIcon::Default);
            self.handle_overlay_pointer_move();
            return;
        }
        // MOUSE-SCROLLBAR: a scroll-thumb drag owns the pointer move — scrub the
        // viewport to the offset the thumb-top maps to and stop. Placed before
        // hover/selection/PTY-report so a scrollbar drag does not update link
        // hover, extend a selection, or emit PTY motion. Mutually exclusive with
        // selection (one `pointer_drag` enum); the grab decision already ran at
        // press time.
        if let Some(grab_dy) = self
            .pointer_drag
            .scrollbar_grab()
            .filter(|_| self.pointer_left_held)
        {
            self.apply_cursor_icon(CursorIcon::Default);
            self.drag_scrollbar_to(y_px, grab_dy, cell, padding);
            return;
        }
        // Divider hover (multi-pane): show a resize cursor over a divider grab
        // zone so drag-to-resize is discoverable (the press path already grabs
        // the same band). Absolute pointer coords (`self.pointer_px`) match the
        // press-time hit-test basis — `content.y` already includes the tab-bar
        // offset, unlike the tab-bar-relative `y_px` shadowed above. Skipped
        // while a text selection is in progress (the gesture owns the pointer)
        // and never reached on a single-pane tab (`multipane_geometry` is
        // `None`), so the byte-identical path never shows a resize cursor.
        if !self.pointer_drag.is_selecting()
            && let Some((content, _)) = self.multipane_geometry()
            && let Some((px, py)) = self.pointer_px
            && let Some(axis) = self.sessions.active_divider_axis_at_point(
                content,
                PANE_DIVIDER_PX,
                px as f32,
                py as f32,
                DIVIDER_GRAB_PX,
            )
        {
            self.apply_cursor_icon(Self::divider_resize_icon(axis));
            return;
        }
        // Multi-pane hover analog of focus-follows-click: `self.grid` /
        // `self.terminal` are the FOCUSED pane's, so resolving hover while the
        // pointer is over a NON-focused pane (or a divider gap) would map the
        // pointer into the focused pane and light a false hyperlink / path / URL
        // hit (and hand cursor) there. Suppress hover in that case, clearing any
        // span left over from a prior focused-pane hover. Single-pane and
        // focused-pane hover are unaffected (`pointer_over_nonfocused_pane` is
        // always false on a single-pane tab), so the common path is byte-identical.
        if self.pointer_over_nonfocused_pane() {
            self.clear_hovered_link_spans();
        } else {
            self.update_hover_hyperlink();
            // INTERACTIVE-PATHS (Phase 7): recompute the hovered path span. Gated
            // on the `interactive_paths` setting inside `update_hover_path`, so
            // with the feature off (the default) it returns before scanning and
            // this call is a single bool test — the hover path stays byte-identical.
            self.update_hover_path();
            // INTERACTIVE-URLS: recompute the hovered bare-URL span. Gated on the
            // `interactive_urls` setting inside `update_hover_url`; off makes this
            // a single bool test so the hover path stays byte-identical.
            self.update_hover_url();
        }
        // Cursor shape over the terminal grid: a hand on a hovered hyperlink OR a
        // resolved interactive path OR a bare URL, the arrow while a TUI has mouse
        // reporting enabled (it owns clicks, so an I-beam would mislead), and the
        // I-beam over plain selectable text — the standard terminal affordance
        // OdyTTY previously never set. The hovered spans are permanently `None`
        // while their features are off, so the default decision is unchanged. OSC
        // 8 wins ties (cosmetically identical icon; precedence matters for click).
        let grid_icon = if self.hovered_hyperlink.is_some()
            || self.hovered_path.is_some()
            || self.hovered_url.is_some()
        {
            CursorIcon::Pointer
        } else if self.mouse_reporting_enabled() {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        };
        self.apply_cursor_icon(grid_icon);
        if self.pointer_drag.is_selecting() {
            // NF21-8 button-held guard (grid analogue of SLIDER-GUARD): only
            // extend the selection while the left button is physically down. A
            // `Selecting` latch whose release was lost — a mid-drag tab/workspace
            // switch, or an alt-tab that delivered the release elsewhere — would
            // otherwise resume a buttonless drag on the next bare `CursorMoved`,
            // and its eventual unmatched release could reach PTY mouse reporting.
            if self.grid_left_held {
                self.autoscroll_selection_if_needed(y_px, cell, padding);
                self.extend_drag_to(point);
                self.request_selection_redraw();
            }
        } else if self.should_report_mouse_to_pty() || self.report_button.is_some() {
            self.send_mouse_motion_report();
        }
    }

    /// Scrub the viewport to the scrollback offset the dragged scroll thumb maps
    /// to (MOUSE-SCROLLBAR). `grab_dy` anchors the cursor to the grab point on
    /// the thumb. Locks the terminal once for the scrollback length and reuses
    /// it for both the geometry and the clamped jump.
    fn drag_scrollbar_to(
        &mut self,
        y_px: f64,
        grab_dy: f32,
        cell: CellSize,
        padding: WindowPadding,
    ) {
        let y_px = if self.should_show_tab_bar() {
            y_px - f64::from(self.tab_bar_height_px(cell))
        } else {
            y_px
        };
        let scrollback_len = self.scrollback_len();
        let Some(target) = scrollbar_offset_for_drag_with_padding(
            y_px as f32,
            grab_dy,
            scrollback_len,
            self.grid,
            cell,
            padding,
        ) else {
            return;
        };
        if self.viewport.jump_to(target, scrollback_len) {
            self.on_viewport_changed();
        }
    }

    pub(super) fn begin_selection(&mut self) {
        let Some(point) = self.pointer_cell else {
            return;
        };
        // NF21-8: a selection gesture starts with the left button physically
        // down; record that so the motion path can refuse to extend once the
        // button is up (see the guard in the grid `CursorMoved` path).
        self.grid_left_held = true;
        // MOUSE-RECT: Alt makes the whole gesture a rectangular/column (block)
        // selection; every non-Alt gesture is wrapped. The mode is decided once
        // here at the single selection entry point, so the word/line/drag
        // sub-paths below all inherit the right mode and a prior block selection
        // can never leak into a new wrapped one. Alt is reached only on the
        // local path (the mouse-reporting gate already returned for a reporting
        // app, where Shift is the only selection-vs-passthrough seam), so
        // Alt+drag never steals Alt+motion from a TUI that wants it. Block
        // selection is inherently char-granularity, so Alt suppresses the
        // double/triple-click word/line semantics and starts a fresh block drag.
        self.selection_block = self.modifiers.alt;
        if self.modifiers.alt {
            self.begin_block_drag(point);
            return;
        }
        // MOUSE-EXTEND: Shift+click extends an existing selection (keep the
        // anchor, move the focus to the click) instead of starting a new one.
        // Reached only on the local path (the report decision already ran), so
        // Shift stays the selection-vs-passthrough seam untouched. Gated by the
        // feature flag and an existing selection; otherwise fall through to the
        // historical click-count dispatch.
        if self.settings.selection_drag_extend
            && self.modifiers.shift
            && self.selection.range().is_some()
        {
            let scrollback_len = self.scrollback_len();
            let viewport_offset = self.viewport.offset();
            self.selection.update(selection::visible_to_absolute(
                point,
                viewport_offset,
                scrollback_len,
            ));
            self.pointer_drag = PointerDrag::Select {
                granularity: SelectGranularity::Char,
                block: false,
            };
            self.drag_anchor_unit = None;
            self.last_selection_autoscroll = None;
            self.request_selection_redraw();
            return;
        }
        match self.clicks.register_click(point, Instant::now()) {
            1 => self.begin_drag_selection(point),
            2 => self.select_word(point),
            _ => self.select_line(point),
        }
    }

    fn begin_drag_selection(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let viewport_offset = self.viewport.offset();
        self.selection.begin(selection::visible_to_absolute(
            point,
            viewport_offset,
            scrollback_len,
        ));
        self.pointer_drag = PointerDrag::Select {
            granularity: SelectGranularity::Char,
            block: false,
        };
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    /// MOUSE-RECT: begin a rectangular/column (block) selection at `point`. The
    /// press cell is the anchor; the column band then grows as the pointer
    /// drags, reusing the existing Char-granularity `extend_drag_to` arm (a
    /// block drag follows the pointer exactly like a normal drag — only how the
    /// range renders and copies differs). `self.selection_block` is already set
    /// by the caller, so the render/copy paths treat the live selection as a
    /// block. Constructs the reserved `PointerDrag::Select { block: true }`.
    fn begin_block_drag(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let viewport_offset = self.viewport.offset();
        self.selection.begin(selection::visible_to_absolute(
            point,
            viewport_offset,
            scrollback_len,
        ));
        self.pointer_drag = PointerDrag::Select {
            granularity: SelectGranularity::Char,
            block: true,
        };
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    fn select_word(&mut self, point: CellPoint) {
        let (snapshot, scrollback_len) = self.selection_snapshot();
        let Some(range) = selection::word_range_at(&snapshot, point) else {
            // No word under the pointer: clear and finalize exactly as before
            // (nothing to anchor a word-drag to), regardless of the flag.
            self.selection.clear();
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
            self.request_selection_redraw();
            return;
        };

        let absolute =
            selection::absolute_range_from_visible(range, self.viewport.offset(), scrollback_len);
        self.selection.set_range(absolute);
        self.finalize_or_arm_unit_drag(SelectGranularity::Word, absolute);
        self.request_selection_redraw();
    }

    fn select_line(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let Some(range) = selection::line_range_at(point, self.grid) else {
            return;
        };

        let absolute =
            selection::absolute_range_from_visible(range, self.viewport.offset(), scrollback_len);
        self.selection.set_range(absolute);
        self.finalize_or_arm_unit_drag(SelectGranularity::Line, absolute);
        self.request_selection_redraw();
    }

    /// MOUSE-EXTEND: after a double/triple-click sets a word/line range, either
    /// keep the drag live so a follow-on drag extends by that unit (flag on) or
    /// finalize byte-identically to the historical click-to-finish behavior
    /// (flag off). The off branch is the mandated parity path.
    fn finalize_or_arm_unit_drag(
        &mut self,
        granularity: SelectGranularity,
        anchor: AbsoluteSelectionRange,
    ) {
        if self.settings.selection_drag_extend {
            self.pointer_drag = PointerDrag::Select {
                granularity,
                block: false,
            };
            self.drag_anchor_unit = Some(anchor);
        } else {
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
        }
    }

    /// Extend the in-progress drag-selection to a visible cell, honoring the
    /// active granularity (MOUSE-EXTEND). Char follows the pointer exactly (the
    /// historical drag); Word/Line snap to and union with whole words/lines.
    pub(super) fn extend_drag_to(&mut self, point: CellPoint) {
        match self.pointer_drag {
            PointerDrag::Select {
                granularity: SelectGranularity::Char,
                ..
            } => {
                let scrollback_len = self.scrollback_len();
                let viewport_offset = self.viewport.offset();
                self.selection.update(selection::visible_to_absolute(
                    point,
                    viewport_offset,
                    scrollback_len,
                ));
            }
            PointerDrag::Select {
                granularity: SelectGranularity::Word,
                ..
            } => self.extend_word_drag(point),
            PointerDrag::Select {
                granularity: SelectGranularity::Line,
                ..
            } => self.extend_line_drag(point),
            PointerDrag::None | PointerDrag::Scrollbar { .. } => {}
        }
    }

    fn extend_word_drag(&mut self, point: CellPoint) {
        let Some(anchor) = self.drag_anchor_unit else {
            return;
        };
        let (snapshot, scrollback_len) = self.selection_snapshot();
        let offset = self.viewport.offset();
        let focus_unit = selection::word_range_at(&snapshot, point)
            .map(|range| selection::absolute_range_from_visible(range, offset, scrollback_len))
            .unwrap_or_else(|| {
                // No word under the pointer (e.g. whitespace): extend to the
                // pointer cell as a degenerate unit so the drag still grows.
                let p = selection::visible_to_absolute(point, offset, scrollback_len);
                AbsoluteSelectionRange { start: p, end: p }
            });
        self.selection
            .set_range(selection::union_absolute_ranges(anchor, focus_unit));
    }

    fn extend_line_drag(&mut self, point: CellPoint) {
        let Some(anchor) = self.drag_anchor_unit else {
            return;
        };
        let scrollback_len = self.scrollback_len();
        let offset = self.viewport.offset();
        let Some(range) = selection::line_range_at(point, self.grid) else {
            return;
        };
        let focus_unit = selection::absolute_range_from_visible(range, offset, scrollback_len);
        self.selection
            .set_range(selection::union_absolute_ranges(anchor, focus_unit));
    }

    pub(super) fn request_selection_redraw(&mut self) {
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn selection_snapshot(&self) -> (Snapshot, usize) {
        // P0-3: drag-autoscroll selection path — poison-recover, never abort.
        let terminal = crate::native::lock_recover(&self.terminal);
        (
            terminal.snapshot_with_scrollback(self.viewport.offset()),
            terminal.screen().scrollback_len(),
        )
    }

    fn autoscroll_selection_if_needed(
        &mut self,
        y_px: f64,
        cell: CellSize,
        padding: WindowPadding,
    ) {
        // MOUSE-AUTOSCROLL-VEL: the step magnitude ramps with how far the pointer
        // is dragged past the edge band, up to the configured cap. `legacy` mode
        // returns a cap of 1, which makes the helper yield exactly ±1/0 —
        // byte-identical to the historical fixed one-row-per-tick autoscroll.
        let max_rows = self.settings.autoscroll_max_rows();
        let delta =
            selection::drag_autoscroll_step_with_padding(y_px, cell, self.grid, padding, max_rows);
        if delta == 0 {
            return;
        }

        let now = Instant::now();
        if self
            .last_selection_autoscroll
            .is_some_and(|last| now.saturating_duration_since(last) < SELECTION_AUTOSCROLL_INTERVAL)
        {
            return;
        }
        self.last_selection_autoscroll = Some(now);
        self.scroll_viewport(delta);
    }

    pub(super) fn finish_selection(&mut self) {
        // NF21-8: the left button is up once the gesture finalizes; drop the
        // held flag before any early return so a subsequent bare `CursorMoved`
        // cannot extend a stale latch.
        self.grid_left_held = false;
        if !self.pointer_drag.is_selecting() {
            return;
        }
        // SH-CLICK: a bare left click (no drag, so the selection stayed empty —
        // `range()` is `None` for a zero-width selection) on the live shell
        // prompt repositions the input cursor. Decided here at release, NOT at
        // press, so a real drag (a non-empty selection) always wins: drag-select
        // and click-to-position are mutually exclusive by construction (D-SHC-2).
        // `try_click_to_position` returns `false` whenever it does not fire
        // (feature off, shell not advertising, wrong row, same-cell, modified
        // click), so the off path falls straight through to the historical
        // finalize below — byte-identical to today (T1).
        if self.selection.range().is_none() && self.try_click_to_position() {
            // The click positioned the cursor; nothing was selected to copy.
        } else if self.drag_selection_should_write_primary() {
            // MOUSE-EXTEND parity: a plain double/triple-click that never dragged
            // must stay byte-identical to the historical finalize, which wrote
            // nothing to PRIMARY. Only write when a char drag ran (today's
            // behavior) or a word/line drag actually grew past its clicked unit.
            self.write_primary_selection();
            // MOUSE-COPYSELECT: when enabled, also write the CLIPBOARD via the
            // exact copy-shortcut path. Off by default, so the historical
            // PRIMARY-only finish is byte-identical.
            if self.settings.copy_on_select {
                self.handle_copy_shortcut();
            }
        }
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    /// Whether finishing the current drag should write PRIMARY (MOUSE-EXTEND).
    /// Char drags write as before (an empty selection no-ops in
    /// `current_selection_text`, so a plain single click stays a no-op too).
    /// Word/Line drags write only when the selection grew beyond the anchored
    /// click unit, so a plain double/triple-click without a drag stays no-write
    /// — byte-identical to the historical finalize.
    pub(super) fn drag_selection_should_write_primary(&self) -> bool {
        match self.pointer_drag {
            PointerDrag::Select {
                granularity: SelectGranularity::Char,
                ..
            } => true,
            PointerDrag::Select { .. } => match (self.selection.range(), self.drag_anchor_unit) {
                (Some(current), Some(anchor)) => current != anchor,
                _ => true,
            },
            PointerDrag::None | PointerDrag::Scrollbar { .. } => false,
        }
    }

    /// SH-CLICK: whether click-to-position is live right now — the `sh_click`
    /// setting is on AND the shell has advertised OSC 133 `click_events=1` on
    /// its prompt. Doubly off by default: the setting defaults off, and a
    /// non-integrated shell never sets the core flag. When the setting is off
    /// this short-circuits before locking the terminal, so the off path does no
    /// work at all (T1 off-path identity).
    fn sh_click_enabled(&self) -> bool {
        self.settings.sh_click
            && self
                .terminal
                .lock()
                .map(|terminal| terminal.click_events_enabled())
                .unwrap_or(false)
    }

    /// SH-CLICK (F2): emit the cursor-positioning key burst for a bare left
    /// click on the live prompt's input region, returning whether the click was
    /// consumed.
    ///
    /// Returns `false` (the caller falls through to the historical finalize,
    /// byte-identical to today) in every case but the narrow one the feature
    /// targets:
    /// - the feature is off or the shell has not advertised click-events (T1);
    /// - the click carries any modifier — Shift is the selection/passthrough
    ///   seam, Alt is block-select, Ctrl is hyperlink-open, so only a *plain*
    ///   click repositions (T2 — Shift seam preserved);
    /// - the viewport is scrolled off the live tail (a click in scrollback is
    ///   never a prompt edit);
    /// - the alternate screen is active — a full-screen app owns its layout, so
    ///   click-to-position never fires there (defense-in-depth; the live-prompt
    ///   gate below already excludes it in practice);
    /// - the live command block is not awaiting input — i.e. there is no live
    ///   prompt because the command already executed (an `OutputStart` exists)
    ///   or there are no marks at all. This is the real prompt-context gate
    ///   (T4): the click-events flag alone can linger across a running command,
    ///   so we require the last [`crate::core::CommandBlock`] to have no output
    ///   yet;
    /// - there is no core-derived [`crate::core::InputRegion`] (no OSC 133 `B`
    ///   input-start mark, or nothing typed): with no modeled input there is
    ///   nothing to click into (F2 G1);
    /// - the click resolves to no travel under the certainty ladder in
    ///   [`click_travel_delta`] — off the region's rows, on the prompt side of
    ///   the input start, on a hard-newline (multi-logical-line) buffer, or on
    ///   the cursor's own position (F2 G2/R-None/same-cell).
    ///
    /// When it does fire, the glyph delta from [`click_travel_delta`] is encoded
    /// as `|delta|` Left/Right cursor keys through the live key modes
    /// ([`click_position_bytes`]) — honoring DECCKM application-cursor mode, the
    /// load-bearing encoding trap — and written through the same PTY writer a
    /// real arrow keypress uses (T5), after returning to the live tail. Only
    /// Left/Right are ever synthesized, never Up/Down (which carry
    /// history-recall semantics in every shell — a synthesized Up could replace
    /// the user's buffer with a history entry).
    ///
    /// TUI mouse reporting (DECSET 1000/1002/1003/1006…) never reaches here: the
    /// reporting gate in [`App::handle_mouse_input`] returns earlier, so a
    /// reporting app's click is sent to the app, not to click-to-position (T3).
    fn try_click_to_position(&mut self) -> bool {
        if !self.sh_click_enabled() {
            return false;
        }
        // T2: only a plain left click repositions; any modifier defers to its
        // existing meaning (Shift=select/passthrough, Alt=block, Ctrl=open).
        if self.modifiers.shift || self.modifiers.alt || self.modifiers.ctrl || self.super_key {
            return false;
        }
        // A scrolled-back viewport is never a live-prompt edit.
        if self.viewport.offset() != 0 {
            return false;
        }
        let Some(point) = self.pointer_cell else {
            return false;
        };
        // HALF-CELL targeting (all platforms): resolve whether the live click
        // fell in the right half of its cell before locking the terminal, so
        // the caret target snaps to the nearest column boundary rather than
        // flooring to the cell's left edge. `false` (floor) when no live pointer
        // pixel is available, preserving the prior behaviour.
        let subcell_round_up = self.click_subcell_rounds_up(point);
        let delta = {
            let Ok(terminal) = self.terminal.lock() else {
                return false;
            };
            // F11: a full-screen app on the alternate screen owns its layout.
            if terminal.screen().on_alternate_screen() {
                return false;
            }
            // T4 prompt-context gate: the last command block must be awaiting
            // input (no OutputStart) for a live prompt to exist.
            let blocks = crate::core::command_blocks(&terminal.prompt_marks());
            let at_live_prompt = blocks
                .last()
                .is_some_and(|block| block.output_start.is_none());
            if !at_live_prompt {
                return false;
            }
            // F2 G1: the core-derived input region is the click target model.
            let Some(region) = terminal.input_region() else {
                return false;
            };
            let scrollback_len = terminal.screen().scrollback_len();
            let cursor = terminal.screen().cursor();
            let snapshot = terminal.snapshot_with_scrollback(0);
            click_travel_delta(
                &snapshot,
                &region,
                point,
                subcell_round_up,
                cursor,
                scrollback_len,
                self.grid.rows,
            )
        };
        let Some(delta) = delta else {
            return false;
        };
        let bytes = click_position_bytes(delta, self.key_modes());
        if bytes.is_empty() {
            return false;
        }
        // T5: the positioning burst goes to the host through the exact keystroke
        // writer, after snapping to the live tail like any typed input.
        self.return_to_live();
        self.write_pty_bytes(&bytes);
        true
    }

    /// HALF-CELL (nearest-boundary) click-to-position targeting: whether the
    /// live pointer fell in the RIGHT half of its resolved cell. Click-to-place
    /// snaps the caret target to the nearest column BOUNDARY — before a
    /// left-half click, after a right-half click — instead of flooring to the
    /// cell's left edge, matching universal text-editor caret hit-testing. (A
    /// floor target lands the caret one cell left of a click that fell a hair
    /// right of a cell boundary, the reported "clicking between two characters
    /// sometimes lands one cell left" symptom.)
    ///
    /// The sub-cell fraction is recovered from the cached `pointer_px` using the
    /// exact horizontal adjustments [`Self::update_pointer_cell`] applies before
    /// it resolves the cell, so the fraction lines up with the resolved column:
    /// single-pane subtracts the tab-chrome dx (a left rail shifts X; the top
    /// bar does not) and the window padding; multi-pane uses the focused pane's
    /// content-rect x origin (which already folds in the chrome/rail offset).
    /// Returns `false` — floor targeting, the shipped behaviour — when the
    /// pointer pixel or cell metrics are unavailable, so a synthesized press
    /// with no live coordinates stays byte-identical.
    ///
    /// Platform-agnostic: a pointer past the right edge (column clamped) yields
    /// a fraction >= 0.5 and rounds up, which is harmless because the travel
    /// flatten clamps the target to the input's end; a pointer left of the
    /// origin yields a negative fraction and rounds down. Platform-uniform:
    /// [`click_travel_delta`] carries no per-platform behaviour, so this rounding
    /// is the whole of the click-to-place boundary fix on every OS.
    fn click_subcell_rounds_up(&self, point: CellPoint) -> bool {
        let Some((x_px, _)) = self.pointer_px else {
            return false;
        };
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        let cell_w = f64::from(cell.width.max(1));
        // Origin of the column axis in the same physical-x basis the cell was
        // resolved against.
        let origin_x = if let Some((content, _)) = self.multipane_geometry() {
            // Multi-pane: the focused pane's content sub-rect x origin.
            let focused = self.sessions.active_id();
            let Some(rect_x) = self
                .sessions
                .active_pane_rects(content, PANE_DIVIDER_PX)
                .into_iter()
                .find(|(token, _)| *token == focused)
                .map(|(_, rect)| f64::from(rect.x))
            else {
                return false;
            };
            rect_x
        } else {
            // Single-pane: window padding after the tab-chrome dx. Both are 0 on
            // the plain top-bar path, so this is the bare padding there.
            let (chrome_dx, _) = self.tab_chrome_offset_px(cell);
            let pad = self
                .gpu
                .as_ref()
                .map(GpuState::window_padding)
                .unwrap_or(WindowPadding::ZERO);
            chrome_dx + f64::from(pad.physical_px())
        };
        // Fraction of the pointer within the resolved cell: >= 0.5 targets the
        // trailing boundary (caret after the glyph).
        let frac = (x_px - origin_x) / cell_w - point.column as f64;
        frac >= 0.5
    }

    /// Number of rows a Shift+PageUp/PageDown press scrolls: one screenful less
    /// one row of overlap for continuity (at least one row).
    pub(super) fn page_lines(&self) -> usize {
        self.grid.rows.saturating_sub(1).max(1)
    }

    /// Current scrollback length from the shared model (0 if the lock is
    /// poisoned), used to clamp upward scrolling.
    pub(super) fn scrollback_len(&self) -> usize {
        self.scrollback_len_of(self.sessions.active_id())
    }

    pub(super) fn scrollback_len_of(&self, token: SessionToken) -> usize {
        let Some(session) = self.sessions.get(token) else {
            return 0;
        };
        session
            .terminal
            .lock()
            .map(|t| t.screen().scrollback_len())
            .unwrap_or(0)
    }

    pub(super) fn scroll_viewport(&mut self, delta: isize) {
        self.scroll_viewport_of(self.sessions.active_id(), delta);
    }

    /// Adjust one pane's scrollback viewport. Wheel events in a split route to
    /// the pane under the pointer; keyboard/page actions keep using the focused
    /// pane through [`Self::scroll_viewport`].
    pub(super) fn scroll_viewport_of(&mut self, token: SessionToken, delta: isize) {
        let scrollback_len = self.scrollback_len_of(token);
        // SCROLL-GLIDE: capture where the follower currently renders BEFORE the
        // offset jumps, so a notch stream re-arms from its lagging position.
        let glide_start_visual = self.scroll_glide_start_visual(token);
        let changed = {
            let Some(session) = self.sessions.get_mut(token) else {
                return;
            };
            match delta.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    session.viewport.scroll_up(delta as usize, scrollback_len)
                }
                std::cmp::Ordering::Less => session.viewport.scroll_down((-delta) as usize),
                std::cmp::Ordering::Equal => false,
            }
        };
        if changed {
            // Discrete notch scrolling moves the integer `Viewport::offset`
            // immediately. `on_viewport_changed_of` snaps by default (clearing
            // any continuous-lane remainder AND the glide follower); re-arm the
            // SCROLL-GLIDE follower right after so the RENDERED viewport eases
            // toward the new offset (a no-op / instant jump when the knob is off
            // or the glide is ineligible).
            self.on_viewport_changed_of(token);
            self.arm_scroll_glide_of(token, glide_start_visual);
        }
    }

    /// Whether wheel events should be translated into cursor keys (alternate
    /// scroll mode, DECSET 1007). True only on the alternate screen with the
    /// mode enabled; the caller has already excluded the mouse-reporting case.
    pub(super) fn alternate_scroll_active(&self) -> bool {
        self.terminal
            .lock()
            .map(|t| t.on_alternate_screen() && t.alternate_scroll_enabled())
            .unwrap_or(false)
    }

    /// Translate a wheel movement of `lines` into that many Up/Down cursor-key
    /// presses sent to the PTY (alternate scroll mode). `lines > 0` is a
    /// scroll-up (toward earlier content) → Up; `lines < 0` → Down. Arrows are
    /// encoded through the shared key encoder so DECCKM application-cursor mode
    /// gets the SS3 form (`\x1bOA`/`\x1bOB`), byte-identical to a real arrow key.
    pub(super) fn send_wheel_as_arrows(&mut self, lines: isize) {
        let key = if lines > 0 { Key::Up } else { Key::Down };
        let count = lines.unsigned_abs();
        if count == 0 {
            return;
        }
        let modes = self.key_modes();
        let arrow = input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Press);
        if arrow.is_empty() {
            return;
        }
        let mut bytes = Vec::with_capacity(arrow.len() * count);
        for _ in 0..count {
            bytes.extend_from_slice(&arrow);
        }
        self.write_pty_bytes(&bytes);
    }

    /// Return the viewport to the live bottom (offset 0). Called whenever input
    /// is written to the PTY so typing always jumps back to the prompt.
    pub(super) fn return_to_live(&mut self) {
        if self.viewport.reset_to_live() {
            self.on_viewport_changed();
        }
    }

    /// Shared side effects of a viewport offset change: keep absolute
    /// selections intact and request one rebuild/redraw so their visible
    /// intersection is recomputed.
    pub(super) fn on_viewport_changed(&mut self) {
        self.on_viewport_changed_of(self.sessions.active_id());
    }

    pub(super) fn on_viewport_changed_of(&mut self, token: SessionToken) {
        // Snap by default — clear any sub-row scroll remainder the continuous
        // (pixel) lane left, so every viewport change (return-to-live, search
        // nav, scrollbar-thumb drag, resize) lands exactly on the integer
        // offset. The continuous lane re-writes the remainder after this call.
        // No-op at rest (byte-identical off path).
        self.clear_scroll_frac_of(token);
        // SCROLL-GLIDE: the same snap-by-default seam settles the forward-chase
        // follower to the exact offset; the scroll path re-arms it afterward.
        self.snap_scroll_glide_of(token);
        self.hovered_hyperlink = self
            .pointer_cell
            .and_then(|point| self.visible_cell_hyperlink(point));
        if let Some(session) = self.sessions.get_mut(token) {
            session.needs_rebuild = true;
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// Convert a physical cursor pixel position to 1-based terminal pixel
/// coordinates for SGR-pixel (1016) mouse reporting, clamped to the grid's
/// pixel extent.
///
/// `x_px`/`y_px` are the raw `winit` `CursorMoved` coordinates, which are
/// already physical pixels; `CellSize` and `padding` are likewise
/// physical-pixel sized. The result first subtracts the window padding to get
/// grid-relative coordinates, then floors to an integer pixel and shifts to the
/// 1-based convention the protocol uses. A cursor left of or above the grid
/// clamps to pixel 1; a cursor at or past the right/bottom edge (e.g. while
/// dragging outside the window) clamps to the last in-grid pixel, mirroring how
/// [`selection::cell_at_physical_with_padding`] saturates the cell path.
/// SH-CLICK (F2): resolve a plain click against the core-derived
/// [`InputRegion`](crate::core::InputRegion) into a signed glyph delta —
/// how many Left (negative) or Right (positive) presses move the shell's
/// line-editor caret from the cursor to the clicked position. `None` means
/// no travel: the click was off the region, on the prompt side of the input
/// start, on an untrustworthy geometry, or on the cursor's own position.
///
/// Certainty ladder (F2 §3, operator-approved):
/// - `Unknown` (stale mark, or a hard-newline multi-logical-line buffer per
///   the signal's `nl=` offsets) → `None`. Left/Right DO cross hard newlines
///   in every editor, but the continuation-prompt geometry (PS2 / `>>` /
///   fish indent) is unmodeled, so an exact count is not computable — v1
///   no-ops rather than landing the caret on the wrong logical line.
/// - `Exact` (fresh private edit-region signal) → rune-precise travel over
///   the reconciled `row_spans` (wrap fillers excluded), including soft-wrap
///   multi-row travel.
/// - `RightEdgeUnknown` (bash / PowerShell / fish mid-edit) → grapheme-cell
///   heuristic over the region bounds, also multi-row across soft wraps.
///   Off-by-one is tolerable here because motion is NON-destructive and
///   every editor clamps the caret at the buffer ends: a mis-land is a
///   click-again, never a wrong edit (contrast select+Delete's charter).
///
/// Click mapping within the region's rows: a click left of a row's input
/// span start (the prompt) is a no-op (F2 G2 — the shipped code walked the
/// caret to buffer position 0 there); a click at or right of the span end
/// clamps to the end of that row's input (a decoration/autosuggestion click
/// moves the caret to the true input end, extra motion absorbed by the
/// shell's own clamp). Glyph counting skips wide-glyph continuation cells,
/// so one wide glyph is one press — the shipped raw-cell delta over-sent
/// arrows on CJK/emoji lines (F2-NF1).
///
/// `subcell_round_up` is the half-cell (nearest-boundary) target: when the
/// click fell in the right half of its cell the caret targets the NEXT column
/// boundary (after the glyph), else the current one (before it). The
/// prompt-side no-op still tests the floored `click.column`, so rounding up
/// never crosses the input start; `flat_at` clamps the target, so a right-half
/// click on the last glyph resolves to the append origin.
///
/// Pure and GPU-free; `click` and `cursor` are in visible-viewport
/// coordinates, the region is absolute (offset by `scrollback_len`).
fn click_travel_delta(
    snapshot: &Snapshot,
    region: &crate::core::InputRegion,
    click: CellPoint,
    subcell_round_up: bool,
    cursor: Position,
    scrollback_len: usize,
    grid_rows: usize,
) -> Option<i32> {
    use super::pointer::snapshot_row_cell_count;
    if region.certainty == crate::core::InputCertainty::Unknown {
        return None;
    }
    let base_visible = region.start_row.checked_sub(scrollback_len)?;
    let row_count = region.end_row - region.start_row + 1;
    if base_visible + row_count > grid_rows {
        return None;
    }
    // F2 G2: the click must land on the region's rows.
    if click.row < base_visible || click.row >= base_visible + row_count {
        return None;
    }
    let columns = snapshot.dimensions.columns;
    if columns == 0 {
        return None;
    }
    // Per-row input spans `(start_col, end_col_exclusive)`: authoritative under
    // Exact (core's reconciled rune walk); reconstructed from the region bounds
    // under RightEdgeUnknown (row 0 starts at the `B` mark, wrapped
    // continuation rows span the full width, the last row ends at the
    // heuristic edge).
    let spans: Vec<(usize, usize)> = if region.certainty == crate::core::InputCertainty::Exact
        && region.row_spans.len() == row_count
    {
        region.row_spans.clone()
    } else {
        (0..row_count)
            .map(|rel| {
                let start = if rel == 0 { region.start_col } else { 0 };
                let end = if rel == row_count - 1 {
                    region.end_col.min(columns)
                } else {
                    columns
                };
                (start, end)
            })
            .collect()
    };
    // Flattened glyph offset at each row's span start (soft wraps carry no
    // newline, so the spans concatenate into one logical horizontal axis —
    // same flatten as the R5 delete rung).
    let mut prefix = Vec::with_capacity(row_count);
    let mut total = 0usize;
    for (rel, &(start, end)) in spans.iter().enumerate() {
        prefix.push(total);
        if start < end {
            total += snapshot_row_cell_count(snapshot, base_visible + rel, start, end - 1);
        }
    }
    // Flattened glyph offset of a caret position on a region row: glyphs
    // between the span start and `col`, clamped right to the span end.
    let flat_at = |row_rel: usize, col: usize| -> usize {
        let (start, end) = spans[row_rel];
        let col = col.clamp(start, end);
        prefix[row_rel]
            + if col > start {
                snapshot_row_cell_count(snapshot, base_visible + row_rel, start, col - 1)
            } else {
                0
            }
    };
    let click_rel = click.row - base_visible;
    // Prompt-side click (left of the input start on its row): a proper no-op,
    // never a caret walk to buffer position 0. The guard tests the LITERAL cell
    // the pixel is over (the floored `click.column`), NOT the nearest-boundary
    // target below, so a right-half click on the last prompt cell stays a no-op
    // and never rounds up across the input start into a bogus travel.
    if click.column < spans[click_rel].0 {
        return None;
    }
    // HALF-CELL (nearest-boundary) caret targeting: a click that fell in the
    // right half of its cell (`subcell_round_up`) targets the NEXT column
    // boundary — the caret AFTER that glyph — while a left-half click targets
    // the current column (caret BEFORE it), matching universal text-editor
    // hit-testing. Flooring to the cell's left edge (still used when no sub-cell
    // fraction is available) lands the caret one cell left of a click that fell
    // a hair past a cell boundary. `flat_at` clamps the target to the span end,
    // so a right-half click on the last input glyph resolves to the append
    // origin, never past it; a wide glyph's continuation cell flattens to the
    // same offset as its lead, so a right-half click on a 2-cell glyph lands the
    // caret after the whole glyph.
    let target_col = if subcell_round_up {
        click.column + 1
    } else {
        click.column
    };
    let target = flat_at(click_rel, target_col);
    // The cursor sits on the region's rows whenever certainty != Unknown; a
    // disagreement here means the region and grid raced — degrade to no-op.
    let cursor_rel = cursor.row.checked_sub(base_visible)?;
    if cursor_rel >= row_count {
        return None;
    }
    let cursor_flat = flat_at(cursor_rel, cursor.column);
    let delta = i32::try_from(target).ok()? - i32::try_from(cursor_flat).ok()?;
    if delta == 0 { None } else { Some(delta) }
}

/// SH-CLICK: encode the cursor-positioning key burst for a click-to-position
/// travel delta — `|delta|` repetitions of Left (negative delta) or Right
/// (positive delta), each encoded through the live [`KeyModes`] so a shell in
/// DECCKM application-cursor mode receives the SS3 form (`\x1bOC`/`\x1bOD`), not
/// the CSI form (`\x1b[C`/`\x1b[D`). This is the load-bearing encoding trap:
/// hardcoded CSI arrows would move the cursor wrong (or not at all) in zsh/zle,
/// fish, and readline shells that run in application-cursor mode, so the bytes
/// MUST be identical to a real arrow keypress in every mode.
///
/// Pure and total: returns the exact bytes the PTY writer receives. A
/// zero delta cannot reach here ([`click_travel_delta`] returns `None` for a
/// same-position click), so `unsigned_abs` never overflows.
fn click_position_bytes(delta: i32, modes: KeyModes) -> Vec<u8> {
    let (key, count) = if delta < 0 {
        (Key::Left, delta.unsigned_abs() as usize)
    } else {
        (Key::Right, delta as usize)
    };
    let arrow = input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Press);
    arrow.repeat(count)
}

fn pixel_coords_for_report(
    x_px: f64,
    y_px: f64,
    cell: CellSize,
    dims: Dimensions,
    padding: WindowPadding,
) -> (usize, usize) {
    let max_px = (dims.columns as u32)
        .saturating_mul(cell.width.max(1))
        .max(1);
    let max_py = (dims.rows as u32).saturating_mul(cell.height.max(1)).max(1);
    let pad = f64::from(padding.physical_px());
    let px = ((x_px - pad).max(0.0) as u32).min(max_px - 1) as usize + 1;
    let py = ((y_px - pad).max(0.0) as u32).min(max_py - 1) as usize + 1;
    (px, py)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::MouseTracking;

    fn resolved_path(abs: &str) -> crate::paths::Resolved {
        crate::paths::Resolved {
            abs: abs.to_owned(),
            kind: crate::paths::FsKind::File,
            line: None,
            col: None,
        }
    }

    #[test]
    fn point_inside_fit_rect_is_not_outside() {
        // Centered fit-rect within a synthetic 1000x800 viewport.
        let rect = [200.0_f32, 150.0, 800.0, 650.0];
        // Dead-center → inside.
        assert!(!point_outside_rect(500.0, 400.0, rect));
        // Just inside each edge.
        assert!(!point_outside_rect(201.0, 400.0, rect));
        assert!(!point_outside_rect(799.0, 400.0, rect));
        assert!(!point_outside_rect(500.0, 151.0, rect));
        assert!(!point_outside_rect(500.0, 649.0, rect));
    }

    #[test]
    fn point_past_each_edge_is_outside() {
        let rect = [200.0_f32, 150.0, 800.0, 650.0];
        // Left of x0.
        assert!(point_outside_rect(199.0, 400.0, rect));
        // Right of x1.
        assert!(point_outside_rect(801.0, 400.0, rect));
        // Above y0.
        assert!(point_outside_rect(500.0, 149.0, rect));
        // Below y1.
        assert!(point_outside_rect(500.0, 651.0, rect));
        // A corner well outside (both axes beyond).
        assert!(point_outside_rect(0.0, 0.0, rect));
    }

    #[test]
    fn point_on_fit_rect_border_is_inclusive_inside() {
        // Boundary convention: a point exactly on an edge counts as ON the image
        // (inside) → inert, never a dismiss.
        let rect = [200.0_f32, 150.0, 800.0, 650.0];
        assert!(!point_outside_rect(200.0, 400.0, rect)); // on x0
        assert!(!point_outside_rect(800.0, 400.0, rect)); // on x1
        assert!(!point_outside_rect(500.0, 150.0, rect)); // on y0
        assert!(!point_outside_rect(500.0, 650.0, rect)); // on y1
        assert!(!point_outside_rect(200.0, 150.0, rect)); // exact corner
    }

    #[test]
    fn image_open_kind_uses_inline_for_images_when_enabled() {
        let settings = crate::settings::Settings {
            interactive_paths_image_inline: true,
            ..crate::settings::Settings::default()
        };
        assert_eq!(
            interactive_path_open_kind(&settings, &resolved_path("/home/user/carpet1.jpg")),
            InteractivePathOpenKind::InlineImage
        );
    }

    #[test]
    fn image_open_kind_uses_external_for_images_when_disabled() {
        let settings = crate::settings::Settings {
            interactive_paths_image_inline: false,
            ..crate::settings::Settings::default()
        };
        assert_eq!(
            interactive_path_open_kind(&settings, &resolved_path("/home/user/carpet1.jpg")),
            InteractivePathOpenKind::External
        );
    }

    #[test]
    fn image_open_kind_uses_external_for_non_images() {
        let settings = crate::settings::Settings {
            interactive_paths_image_inline: true,
            ..crate::settings::Settings::default()
        };
        assert_eq!(
            interactive_path_open_kind(&settings, &resolved_path("/home/user/notes.txt")),
            InteractivePathOpenKind::External
        );
    }

    // --- SH-CLICK: click-to-position arrow encoding (Finding A) ---

    fn app_cursor_modes() -> KeyModes {
        KeyModes {
            application_cursor: true,
            ..KeyModes::default()
        }
    }

    #[test]
    fn click_position_emits_right_arrows_in_csi_mode() {
        // A positive delta (click right of the cursor) emits that many Right
        // cursor keys in the default CSI form.
        let bytes = click_position_bytes(5, KeyModes::default());
        assert_eq!(bytes, b"\x1b[C".repeat(5));
    }

    #[test]
    fn click_position_emits_left_arrows_in_csi_mode() {
        // A negative delta (click left of the cursor) emits Left cursor keys.
        let bytes = click_position_bytes(-3, KeyModes::default());
        assert_eq!(bytes, b"\x1b[D".repeat(3));
    }

    #[test]
    fn click_position_honors_decckm_application_cursor_mode() {
        // Finding A (the highest-risk encoding trap): a shell in DECCKM
        // application-cursor mode must receive the SS3 forms (\x1bOC / \x1bOD),
        // byte-identical to a real arrow keypress, NOT the CSI forms. This is
        // why the burst routes through `encode_key_event`, never hardcoded bytes.
        let right = click_position_bytes(5, app_cursor_modes());
        assert_eq!(right, b"\x1bOC".repeat(5));
        let left = click_position_bytes(-2, app_cursor_modes());
        assert_eq!(left, b"\x1bOD".repeat(2));
    }

    #[test]
    fn click_position_burst_length_matches_delta_magnitude() {
        // The number of arrows equals |delta|; a single-cell move emits one key.
        assert_eq!(click_position_bytes(1, KeyModes::default()), b"\x1b[C");
        assert_eq!(
            click_position_bytes(-1, KeyModes::default()).len(),
            b"\x1b[D".len()
        );
        // A wide delta maps to exactly that many arrows (no off-by-one), without
        // exercising an absurd allocation.
        let wide = click_position_bytes(200, KeyModes::default());
        assert_eq!(wide.len(), b"\x1b[C".len() * 200);
    }

    // --- MS2: SGR-pixel (1016) native pixel seam ---

    fn cell_8x16() -> CellSize {
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        }
    }

    #[test]
    fn pixel_coords_origin_maps_to_one_based() {
        // Cursor at the top-left physical pixel maps to (1, 1): the protocol is
        // 1-based and zero padding keeps the grid at the window origin.
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(0.0, 0.0, cell_8x16(), dims, WindowPadding::ZERO),
            (1, 1)
        );
    }

    #[test]
    fn pixel_coords_floor_then_one_base() {
        // Sub-pixel fractions floor; 10.9px -> pixel index 10 -> 1-based 11.
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(10.9, 33.2, cell_8x16(), dims, WindowPadding::ZERO),
            (11, 34)
        );
    }

    #[test]
    fn pixel_coords_are_independent_of_cell_size() {
        // The pixel path reports raw physical pixels, NOT cells: the same cursor
        // position yields the same pixel coords regardless of cell metrics
        // (a larger cell only changes the clamp extent, not the mapping).
        let dims = Dimensions::new(80, 24);
        let small = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let large = CellSize {
            width: 20,
            height: 40,
            baseline: 30,
        };
        assert_eq!(
            pixel_coords_for_report(100.0, 100.0, small, dims, WindowPadding::ZERO),
            pixel_coords_for_report(100.0, 100.0, large, dims, WindowPadding::ZERO)
        );
    }

    #[test]
    fn pixel_coords_clamp_negative_to_one() {
        // A cursor left of / above the grid (negative physical coords during a
        // drag) saturates to pixel 1, mirroring cell_at_physical's max(0.0).
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(-50.0, -5.0, cell_8x16(), dims, WindowPadding::ZERO),
            (1, 1)
        );
    }

    #[test]
    fn pixel_coords_clamp_to_grid_extent() {
        // Grid is 80x24 cells of 8x16 px = 640x384 px. A cursor at or beyond the
        // bottom-right edge clamps to the last in-grid pixel (640, 384).
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(640.0, 384.0, cell_8x16(), dims, WindowPadding::ZERO),
            (640, 384)
        );
        assert_eq!(
            pixel_coords_for_report(9999.0, 9999.0, cell_8x16(), dims, WindowPadding::ZERO),
            (640, 384)
        );
    }

    #[test]
    fn pixel_coords_last_in_grid_pixel_is_not_clamped() {
        // 639.0px -> index 639 -> 1-based 640, the max; still inside the grid so
        // it is reported as-is (the clamp only bites at/after the extent).
        let dims = Dimensions::new(80, 24);
        assert_eq!(
            pixel_coords_for_report(639.0, 383.0, cell_8x16(), dims, WindowPadding::ZERO),
            (640, 384)
        );
    }

    #[test]
    fn pixel_coords_subtract_window_padding_before_reporting() {
        let dims = Dimensions::new(80, 24);
        let padding = WindowPadding::from_logical(8.0, 1.0);

        assert_eq!(
            pixel_coords_for_report(8.0, 8.0, cell_8x16(), dims, padding),
            (1, 1)
        );
        assert_eq!(
            pixel_coords_for_report(18.9, 41.2, cell_8x16(), dims, padding),
            (11, 34)
        );
    }

    #[test]
    fn sgr_pixel_encoder_emits_pixel_wire_shape() {
        // The 1016 seam feeds computed pixel coords to the core encoder, which
        // emits the SGR wire shape with those pixel values (here 101;201).
        let protocol = MouseProtocol {
            tracking: MouseTracking::Normal,
            encoding: MouseEncoding::SgrPixel,
        };
        let dims = Dimensions::new(80, 24);
        let (px, py) =
            pixel_coords_for_report(100.0, 200.0, cell_8x16(), dims, WindowPadding::ZERO);
        let mods = MouseModifiers {
            shift: false,
            alt: false,
            ctrl: false,
        };
        let bytes = encode_mouse_event_pixel(
            protocol,
            CoreMouseButton::Left,
            MouseEventKind::Press,
            px,
            py,
            mods,
        )
        .expect("1016 press encodes");
        assert_eq!(bytes, b"\x1b[<0;101;201M");
    }

    #[test]
    fn pixel_encoder_guard_rejects_non_1016_encodings() {
        // The pixel encoder only fires for SgrPixel; for every other encoding it
        // returns None, so send_mouse_report's branch leaves the cell path
        // authoritative for legacy/UTF-8/SGR/urxvt.
        let dims = Dimensions::new(80, 24);
        let (px, py) =
            pixel_coords_for_report(100.0, 200.0, cell_8x16(), dims, WindowPadding::ZERO);
        let mods = MouseModifiers {
            shift: false,
            alt: false,
            ctrl: false,
        };
        for encoding in [
            MouseEncoding::Default,
            MouseEncoding::Utf8,
            MouseEncoding::Sgr,
            MouseEncoding::Urxvt,
        ] {
            let protocol = MouseProtocol {
                tracking: MouseTracking::Normal,
                encoding,
            };
            assert!(
                encode_mouse_event_pixel(
                    protocol,
                    CoreMouseButton::Left,
                    MouseEventKind::Press,
                    px,
                    py,
                    mods,
                )
                .is_none(),
                "encoding {encoding:?} must not take the pixel seam"
            );
        }
    }

    // --- HALF-CELL (nearest-boundary) click-to-position targeting ---
    //
    // Click-to-place snaps the caret target to the nearest column BOUNDARY: a
    // right-half click (`round_up`) targets a glyph's trailing edge, one column
    // further than a left-half click on the same cell. The prompt-side guard
    // tests the floored cell, so rounding up never crosses the input start. All
    // single-width and non-destructive, so these run on every platform;
    // click-to-place carries no per-platform behaviour, so these expectations
    // are platform-uniform.

    /// Signed travel for a click on the floored cell `col` (with `round_up` =
    /// the pixel fell in that cell's right half) against a single-row,
    /// single-width input of `len` glyphs starting at `start`, cursor at
    /// `cursor_col`. Uses the `Exact` certainty so these assert the pure
    /// half-cell rounding (target-column) logic; it is platform-uniform, as is
    /// the rest of the click-to-place travel.
    fn halfcell_delta(
        start: usize,
        len: usize,
        col: usize,
        round_up: bool,
        cursor_col: usize,
    ) -> Option<i32> {
        let columns = 80usize;
        let rows = 28usize;
        let mut cells = vec![crate::core::Cell::blank(); columns * rows];
        for i in 0..len {
            cells[start + i] = crate::core::Cell::new('x', crate::core::Attrs::default());
        }
        let snapshot = Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position {
                row: 0,
                column: cursor_col,
            },
            cursor_visible: true,
            colors: crate::core::DynamicColors::default(),
            cells,
        };
        let region = crate::core::InputRegion {
            start_row: 0,
            start_col: start,
            end_row: 0,
            end_col: start + len,
            joins: Vec::new(),
            certainty: InputCertainty::Exact,
            row_spans: vec![(start, start + len)],
        };
        click_travel_delta(
            &snapshot,
            &region,
            CellPoint {
                row: 0,
                column: col,
            },
            round_up,
            Position {
                row: 0,
                column: cursor_col,
            },
            0,
            rows,
        )
    }

    #[test]
    fn halfcell_right_half_targets_one_column_further_than_left_half() {
        // Input "xxxxx" at cols 2..7, cursor at the append origin (col 7 = 5
        // glyphs). A click on the 3rd glyph (col 4): the LEFT half targets
        // before it (2 glyphs in -> delta -3), the RIGHT half targets after it
        // (3 glyphs in -> delta -2). Exactly one column apart — the
        // nearest-boundary behaviour that fixes the one-cell-left mis-land.
        assert_eq!(halfcell_delta(2, 5, 4, false, 7), Some(-3));
        assert_eq!(halfcell_delta(2, 5, 4, true, 7), Some(-2));
    }

    #[test]
    fn halfcell_last_glyph_right_half_clamps_to_append_origin() {
        // Cursor pulled back to col 4 (2 glyphs in). A right-half click on the
        // LAST glyph (col 6) targets the append origin (5 glyphs), never past it
        // -> delta +3; a click well past the input clamps to the same origin.
        assert_eq!(halfcell_delta(2, 5, 6, true, 4), Some(3));
        assert_eq!(halfcell_delta(2, 5, 10, true, 4), Some(3));
    }

    #[test]
    fn halfcell_prompt_side_right_half_never_rounds_into_the_input() {
        // A right-half click on the last prompt cell (col 1; input starts at
        // col 2) stays a clean no-op: the guard tests the floored cell, so
        // rounding up to col 2 never fires a bogus travel toward position 0.
        assert_eq!(halfcell_delta(2, 5, 1, true, 7), None);
        assert_eq!(halfcell_delta(2, 5, 1, false, 7), None);
    }

    #[test]
    fn halfcell_left_half_of_first_glyph_is_the_input_start() {
        // A left-half click on the first input glyph (col 2) targets buffer
        // position 0; from the append origin (col 7 = 5 glyphs) that is -5.
        // Rounding up moves one glyph in (delta -4) — the boundary after the
        // first glyph.
        assert_eq!(halfcell_delta(2, 5, 2, false, 7), Some(-5));
        assert_eq!(halfcell_delta(2, 5, 2, true, 7), Some(-4));
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Overlay outcomes and pointer-side overlay routing for the native app.
//!
//! Owns context-menu construction, overlay outcome application, and the pointer
//! button/move/wheel arms an open overlay consumes before the terminal sees them.

use super::*;

/// Pure hit-test for the lightbox click-outside-to-dismiss (Phase 13d): is the
/// pointer pixel `(px, py)` OUTSIDE the image fit-rect `[x0, y0, x1, y1]`?
///
/// Boundary convention: edges are INCLUSIVE — a point exactly on the rect border
/// counts as ON the image (inside), so it is inert rather than dismissing; only
/// a point strictly beyond an edge returns `true`. Kept a free, pure fn so it is
/// unit-testable headless, with no GPU/window state. The pointer pixel and the
/// fit-rect share the same physical-pixel origin (full-viewport lightbox), so
/// the comparison is exact.
pub(super) fn point_outside_rect(px: f64, py: f64, rect: [f32; 4]) -> bool {
    let [x0, y0, x1, y1] = rect;
    px < x0 as f64 || px > x1 as f64 || py < y0 as f64 || py > y1 as f64
}

impl App {
    /// Open the right-click context menu (IN2) at the cached pointer cell, with
    /// Copy enabled iff a selection exists and Paste enabled iff the clipboard
    /// holds text — the per-item gating snapshot the menu renders. Deliberately
    /// does NOT call `reset_pointer_state_for_overlay`: that would clear the
    /// selection the Copy item needs. No pointer cell (e.g. before the first
    /// move) means no menu.
    pub(super) fn open_context_menu(&mut self, surface: ContextMenuSurface) {
        // Unlike full overlays the context menu preserves terminal selection,
        // so it does not use `reset_pointer_state_for_overlay`. It must still
        // settle a divider before capturing subsequent left-button releases.
        self.finish_divider_drag();
        // The rename/close target token rides on the surface: a `TabSlot`
        // right-click targets THAT tab (NF-F7-1); every other surface has no
        // tab target.
        let rename_target = match surface {
            ContextMenuSurface::TabSlot(token) => Some(token),
            _ => None,
        };
        // Window-overlay cell space: in a single-pane tab this is exactly
        // `self.pointer_cell`; in a multi-pane tab it maps the pointer into the
        // whole content grid so the menu spawns where it renders (and clicks
        // land), not in the focused pane's sub-grid.
        let Some(spawn) = self.overlay_pointer_cell() else {
            return;
        };
        let copy_enabled = self.selection.range().is_some();
        let editable_selection = self.editable_input_selection_for_context_menu();
        let prompt_editing_hint =
            editable_selection.is_none() && self.prompt_input_mark_missing_for_context_menu();
        // PASTE-GATE: do NOT probe the clipboard synchronously here. On Wayland
        // `get_text` reads a pipe served by the clipboard OWNER with no timeout,
        // so a slow or unresponsive owner blocks the winit event-loop thread --
        // and this ran on EVERY menu open, freezing the whole UI for seconds. The
        // Paste action itself (`handle_paste_shortcut`) already no-ops gracefully
        // on an empty clipboard, so the item is shown optimistically enabled;
        // activating it with nothing to paste simply does nothing. Windows: the
        // Win32 clipboard read does not block indefinitely, so this is a
        // no-behavior-change simplification there (the item is always enabled and
        // the action still no-ops on empty).
        let paste_enabled = true;
        // Part C: each item's *effective* keybind, derived from the live
        // `KeyBindings` (reverse action→chord lookup) so it reflects user
        // rebinds. Items with no bound chord get `None` (rendered blank). Reuses
        // `format_key_chord` for the chord decomposition; `humanize_chord` only
        // title-cases the tokens for display.
        let mut accelerators = crate::native::context_menu_ui::ContextMenuItem::ALL.map(|item| {
            item.bindable_action()
                .and_then(|action| self.key_bindings.chord_for_action(action))
                .map(|chord| {
                    crate::native::context_menu_ui::humanize_chord(
                        crate::settings::format_key_chord(chord),
                    )
                })
        });
        // Close Pane is shown only in a multi-pane tab, and its chord lives in
        // the multiplexer prefix table (`Ctrl-b x`), not the flat global table —
        // so its accelerator is composed here from the prefix engine rather than
        // the generic `bindable_action` → `chord_for_action` path above.
        let multi_pane = !self.sessions.active_is_single_pane();
        let multi_tab = self.sessions.tab_count() > 1;
        let multi_workspace = self.sessions.workspace_count() > 1;
        // F6-W5: the tab menu offers a "New Local Tab" escape only when the
        // active workspace routes New Tab through a bound host. RAIL-BIND: a
        // WorkspaceSlot menu targets the CLICKED slot, so its bind/unbind
        // conditional reads THAT workspace's binding, not the active one.
        let bound_workspace = match surface {
            ContextMenuSurface::WorkspaceSlot(idx) => {
                self.sessions.workspace_default_profile_at(idx).is_some()
            }
            _ => self.sessions.active_workspace_default_profile().is_some(),
        };
        if multi_pane
            && let Some(label) = self.close_pane_accelerator()
            && let Some(slot) = crate::native::context_menu_ui::ContextMenuItem::ALL
                .iter()
                .position(|item| {
                    *item == crate::native::context_menu_ui::ContextMenuItem::ClosePane
                })
        {
            accelerators[slot] = Some(label);
        }
        // C3: re-detect the interactive path at the click cell (do NOT reuse the
        // hover snapshot — a right-click may not pass through the hover path).
        // Gated on the setting so the default (feature-off) menu never scans and
        // is byte-identical. `None` hides the file section entirely.
        // PATH-GATE: `resolved_hovered_path` stat-probes candidate path spans,
        // which can block arbitrarily on a hung mount. A right-click on chrome (a
        // tab slot, the workspace rail, an empty strip) can never sit over a
        // content path, so restrict the scan to the terminal content surface --
        // that alone removes the stat site from every rail/tab right-click. Only
        // the content grid can host a hovered path. Windows: the scan is
        // filesystem path resolution (Unix path semantics; drive-letter cwds via
        // OSC 7 on Windows); this gate only narrows WHEN it runs and does not
        // change its cross-platform behavior.
        let scan_hovered_path =
            self.settings.interactive_paths && matches!(surface, ContextMenuSurface::Content);
        #[cfg(test)]
        {
            self.last_menu_path_scan_for_test = scan_hovered_path;
        }
        let path_target = if scan_hovered_path {
            self.resolved_hovered_path()
        } else {
            None
        };
        let command_handle = matches!(surface, ContextMenuSurface::Content)
            .then(|| self.command_handle_for_action())
            .flatten();
        self.context_command_handle =
            command_handle.map(|handle| (self.sessions.active_id(), handle));
        self.overlay.open_context_menu_with_prompt_editing_hint(
            spawn,
            copy_enabled,
            editable_selection.is_some(),
            paste_enabled,
            editable_selection.is_some(),
            prompt_editing_hint,
            rename_target,
            multi_pane,
            multi_tab,
            multi_workspace,
            bound_workspace,
            surface,
            path_target,
            accelerators,
        );
        if matches!(surface, ContextMenuSurface::Content) {
            self.overlay
                .set_context_menu_command_actions_enabled(command_handle.is_some());
        }
        // MENU-DEBOUNCE: stamp the open instant so a stale queued press flushed
        // into the just-opened menu is swallowed rather than activating an item
        // (the "phantom New Workspace" replay). Cleared implicitly -- the check
        // in `handle_overlay_pointer_button` also requires the menu to be open.
        self.context_menu_opened_at = Some(Instant::now());
        // RAIL-REORDER: a WorkspaceSlot menu needs the total workspace count to
        // gate its Move Up/Down rows (Move Down hides on the last slot). Set it
        // only for that surface; every other menu leaves the count at 0.
        if let ContextMenuSurface::WorkspaceSlot(_) = surface {
            self.overlay
                .set_context_menu_workspace_count(self.sessions.workspace_count());
        }
        // MENU-Z-ORDER: a rail-anchored menu keeps the auto-hide rail revealed
        // (RAIL-PIN), and the rail composites topmost — so without clearance the
        // menu box paints UNDER the floating rail band and its edge is occluded.
        // Reserve the rail band's columns (plus a one-column gap) on the rail's
        // side so the box lands beside the rail, fully visible and clickable.
        // Only the rail-anchored surfaces pin the rail; every other menu closes
        // the rail (overlay open ⇒ not revealed), so no clearance is applied and
        // the geometry is byte-identical.
        if self.rail_autohide_active()
            && matches!(
                surface,
                ContextMenuSurface::WorkspaceSlot(_) | ContextMenuSurface::WorkspaceRailEmpty
            )
            && let Some(side) = self.rail_autohide_side()
        {
            let band = self.rail_overlay_cols() + 1;
            let (left, right) = match side {
                RailSide::Left => (band, 0),
                RailSide::Right => (0, band),
            };
            self.overlay.set_context_menu_rail_clearance(left, right);
        }
        self.apply_cursor_icon(CursorIcon::Default);
        self.request_selection_redraw();
    }

    /// The human-readable accelerator label for the context menu's Close Pane
    /// item: the multiplexer prefix chord followed by the prefix-table key bound
    /// to `ClosePane` (e.g. `Ctrl+B X` for the tmux `Ctrl-b x` default). `None`
    /// when the prefix is disabled (`ODYTTY_PANE_PREFIX=off`) or `ClosePane` has
    /// no prefix binding — the menu then renders the item with a blank
    /// accelerator. Reuses the same `format_key_chord` + `humanize_chord` pair
    /// the flat-table accelerators use, so the styling matches.
    pub(super) fn close_pane_accelerator(&self) -> Option<String> {
        let prefix = self.prefix_engine.prefix()?;
        let second = self
            .prefix_engine
            .chord_for_action(crate::settings::BindableAction::ClosePane)?;
        let prefix_label = crate::native::context_menu_ui::humanize_chord(
            crate::settings::format_key_chord(prefix),
        );
        let second_label = crate::native::context_menu_ui::humanize_chord(
            crate::settings::format_key_chord(second),
        );
        Some(format!("{prefix_label} {second_label}"))
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
        if !matches!(
            &outcome,
            OverlayOutcome::Consumed
                | OverlayOutcome::ContextMenuSelectCommandOutput
                | OverlayOutcome::ContextMenuSelectCommandWithPrompt
                | OverlayOutcome::ContextMenuCopyCommandOutput
                | OverlayOutcome::ContextMenuCopyCommandWithPrompt
                | OverlayOutcome::ContextMenuSearchCommandOutput
                | OverlayOutcome::ContextMenuJumpFailedCommandPrev
                | OverlayOutcome::ContextMenuJumpFailedCommandNext
                | OverlayOutcome::ContextMenuExportCommandOutput
        ) {
            self.context_command_handle = None;
        }
        match outcome {
            OverlayOutcome::Consumed => {}
            OverlayOutcome::Close => {
                self.pending_text_paste = None;
                self.flush_pending_overlay_settings();
                self.overlay.close();
            }
            OverlayOutcome::RiskyPaste => self.commit_pending_text_paste(false),
            OverlayOutcome::RiskyPasteOneLine => self.commit_pending_text_paste(true),
            OverlayOutcome::RiskyPasteCancel => self.cancel_pending_text_paste(),
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
            OverlayOutcome::OpenProfileManager => {
                self.flush_pending_overlay_settings();
                self.open_profile_manager_overlay();
            }
            OverlayOutcome::CaptureThemeColors => {
                // THEME-CAPTURE, in-editor `C`: resolve the focused pane's
                // live colors and feed them into the open editor. The overlay
                // stays open and nothing is applied or written.
                let spec = self.capture_live_theme_spec();
                self.overlay.apply_theme_capture(spec);
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
            OverlayOutcome::SaveProfile { profile, replace } => {
                self.flush_pending_overlay_settings();
                self.save_overlay_profile(*profile, replace);
            }
            OverlayOutcome::DeleteProfile(name) => {
                self.flush_pending_overlay_settings();
                self.delete_overlay_profile(&name);
            }
            OverlayOutcome::ImportProfile => {
                self.flush_pending_overlay_settings();
                self.import_overlay_profile();
            }
            OverlayOutcome::ExportProfile(name) => {
                self.flush_pending_overlay_settings();
                self.export_overlay_profile(&name);
            }
            OverlayOutcome::SetDefaultLaunchProfile(name) => {
                self.flush_pending_overlay_settings();
                self.set_global_default_launch_profile(&name);
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
            OverlayOutcome::ContextMenuSelectCommandOutput => {
                self.flush_pending_overlay_settings();
                if let Some(handle) = self.take_context_command_handle() {
                    self.select_command_range_from_handle(
                        handle,
                        crate::core::CommandRangePart::Output,
                    );
                }
            }
            OverlayOutcome::ContextMenuSelectCommandWithPrompt => {
                self.flush_pending_overlay_settings();
                if let Some(handle) = self.take_context_command_handle() {
                    self.select_command_range_from_handle(
                        handle,
                        crate::core::CommandRangePart::PromptAndCommand,
                    );
                }
            }
            OverlayOutcome::ContextMenuCopyCommandOutput => {
                self.flush_pending_overlay_settings();
                if let Some(handle) = self.take_context_command_handle() {
                    self.copy_command_range_from_handle(
                        handle,
                        crate::core::CommandRangePart::Output,
                    );
                }
            }
            OverlayOutcome::ContextMenuCopyCommandWithPrompt => {
                self.flush_pending_overlay_settings();
                if let Some(handle) = self.take_context_command_handle() {
                    self.copy_command_range_from_handle(
                        handle,
                        crate::core::CommandRangePart::PromptAndCommand,
                    );
                }
            }
            OverlayOutcome::ContextMenuSearchCommandOutput => {
                self.flush_pending_overlay_settings();
                if let Some(handle) = self.take_context_command_handle() {
                    self.search_command_output_from_handle(handle);
                }
            }
            OverlayOutcome::ContextMenuJumpFailedCommandPrev => {
                self.flush_pending_overlay_settings();
                if self.take_context_command_handle().is_some() {
                    self.jump_failed_command(crate::core::CommandDirection::Prev);
                }
            }
            OverlayOutcome::ContextMenuJumpFailedCommandNext => {
                self.flush_pending_overlay_settings();
                if self.take_context_command_handle().is_some() {
                    self.jump_failed_command(crate::core::CommandDirection::Next);
                }
            }
            OverlayOutcome::ContextMenuExportCommandOutput => {
                self.flush_pending_overlay_settings();
                if let Some(handle) = self.take_context_command_handle() {
                    self.begin_command_output_export_from_handle(handle);
                }
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
            OverlayOutcome::ContextMenuNewTabWithProfile => {
                self.flush_pending_overlay_settings();
                self.open_profile_picker_for_new_tab();
            }
            OverlayOutcome::ContextMenuNewWorkspaceWithProfile => {
                self.flush_pending_overlay_settings();
                self.open_profile_picker_for_new_workspace();
            }
            OverlayOutcome::ProfilePickerNewTab(name) => {
                self.flush_pending_overlay_settings();
                self.handle_new_tab_with_profile(&name);
            }
            OverlayOutcome::ProfilePickerNewWorkspace(name) => {
                self.flush_pending_overlay_settings();
                self.handle_new_workspace_with_profile(&name);
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
            OverlayOutcome::FocusSession(token) => self.focus_session_from_navigator(token),
            OverlayOutcome::NavigatorAction(action) => self.run_navigator_action(action),
            OverlayOutcome::NavigatorCloseRequest(target) => {
                self.flush_pending_overlay_settings();
                self.reset_pointer_state_for_overlay();
                self.overlay.open_confirm_navigator_close(target);
                self.request_selection_redraw();
            }
            OverlayOutcome::NavigatorCloseConfirmed(target) => {
                self.flush_pending_overlay_settings();
                self.close_navigator_target(target);
            }
            OverlayOutcome::NavigatorCloseCanceled(target) => {
                self.flush_pending_overlay_settings();
                self.open_session_navigator_overlay_selected(Some(target));
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
            // the UI; it raises a one-line notice (a missing `ssh` or PTY
            // exhaustion otherwise reads as a dead click) and the user can retry.
            OverlayOutcome::Connect(host) => {
                self.flush_pending_overlay_settings();
                self.connect_or_notice(&host);
            }
            // ADHOC-CONNECT: connect to a typed host AND append it to hosts.conf.
            // The connect and the save are independent — a save failure never
            // blocks the connection, and vice versa.
            OverlayOutcome::ConnectAndSave(host) => {
                self.flush_pending_overlay_settings();
                self.connect_or_notice(&host);
                self.save_adhoc_host(&host);
            }
            OverlayOutcome::LaunchProfile(name) => {
                self.flush_pending_overlay_settings();
                self.handle_new_tab_with_profile(&name);
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
            // "Detach & switch" was chosen on the focused pane. The
            // menu closed itself; read the focused pane's cwd and open the 3-way
            // choice dialog.
            OverlayOutcome::ContextMenuDetachSwitch => {
                self.flush_pending_overlay_settings();
                self.open_detach_switch_choice();
            }
            // The Detach & switch dialog closed itself before emitting
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

    fn take_context_command_handle(&mut self) -> Option<crate::core::CommandRangeHandle> {
        let (session, handle) = self.context_command_handle.take()?;
        (session == self.sessions.active_id()).then_some(handle)
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
}

#[cfg(test)]
mod tests;

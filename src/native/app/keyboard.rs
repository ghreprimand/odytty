// SPDX-License-Identifier: GPL-3.0-only
//! Keyboard routing for the native app: key precedence, command dispatch, PTY
//! encoding, and held-exit behavior.
//!
//! `handle_key_event` remains the single ordered precedence chain -- held exit,
//! activity and drag settlement, OSC 52 prompt, prefix, global overlay toggles,
//! active overlay input, launchers and search, modal prompts, configured
//! actions, smart interrupt and selection deletion, image and reconnect
//! prompts, then Win32 input mode, keypad mode, and normal PTY encoding. Win32
//! input mode still owns every otherwise-unconsumed physical event.
//!
//! The `ApplicationHandler` match in the parent module remains the stable event
//! ingress; the `KeyboardInput` arm reaches this chain exactly as before.

use super::*;

impl App {
    /// Encode a key event and write its bytes to the PTY.
    ///
    /// Maps the `winit` logical key (plus the cached [`Modifiers`]) onto the
    /// neutral [`Key`] model and defers byte production to the shared
    /// [`input::encode_key`]. Keys the prototype does not encode are dropped. The
    /// PTY writer is flushed after each write so the keystroke reaches the shell
    /// without buffering latency.
    pub(super) fn handle_key_event(
        &mut self,
        logical: WinitKey,
        binding_key: WinitKey,
        physical: PhysicalKey,
        event_type: KeyEventType,
    ) {
        // Physical identity is the stable source for editing keys. KDE/KWin can
        // report Ctrl+Backspace as Character(BS), while Mutter and other stacks
        // report Named(Backspace). Canonicalize both logical views before any
        // prompt, chord, modal, or PTY routing so compositor choice cannot alter
        // behavior.
        let logical = normalize_winit_editing_key(logical, physical);
        let binding_key = normalize_winit_editing_key(binding_key, physical);
        key_event_diagnostics::log_backspace_stage(&logical, "normalized");
        // `--hold`: while the active pane contains the already-exited launch
        // command, all keyboard input is local UI input. Releases are swallowed
        // without dismissing; the first press or repeat closes through the
        // shell-already-exited cleanup path, so no byte reaches the dead PTY.
        if self.handle_held_exit_key(event_type) {
            return;
        }
        // A physical press or repeat is keyboard activity even when chrome,
        // an overlay, or a pane command consumes it below. Record it before
        // routing or PTY encoding so cursor presentation never changes input
        // bytes, ordering, or latency.
        if event_type != KeyEventType::Release {
            self.note_cursor_keyboard_activity(Instant::now());
            // A keyboard action can open a modal or mutate the active
            // tab/workspace/layout before the mouse release arrives. Settle the
            // pointer-owned divider against its original layout first; later
            // duplicate release/focus/leave/resize boundaries are inert.
            self.finish_divider_drag();
        }
        if self.handle_osc52_prompt_key(&binding_key, physical, event_type) {
            return;
        }
        let mods = self.modifiers;
        let key_modes = self.key_modes();
        if event_type != KeyEventType::Release {
            // RAIL-DRAG: Escape cancels an in-flight workspace-rail drag with the
            // order untouched, before any other key routing — the cancel-on-escape
            // ergonomic. Consumes the key only when a drag was actually cancelled;
            // otherwise Escape falls through to its normal meaning.
            if matches!(logical, WinitKey::Named(NamedKey::Escape))
                && (self.cancel_workspace_drag() || self.cancel_top_tab_drag())
            {
                return;
            }
            // Multiplexer prefix engine (§7, K2). Runs first so that, once a
            // prefix is pending, the next chord resolves against the prefix
            // table before any global chord (Settings/Search/etc.) — tmux
            // semantics: the key after the prefix is a pane command, and an
            // unknown one cancels. The engine is suppressed while an overlay /
            // search / modal is capturing, so those paths stay byte-identical;
            // and when not pending it returns `Inactive` for every non-prefix
            // chord, leaving the entire path below unchanged. Pane-management
            // chords (`%`, arrows, `x`, …) are excluded from the global table,
            // so they never reach the normal dispatch as bare keys.
            //
            // Single-pane gate (byte-identity): the prefix only intercepts once
            // the active tab is actually split (`panes > 1`). On a single-pane
            // tab — the default and overwhelmingly common case — the prefix key
            // (default `Ctrl-b` / `0x02`) and every other key flow straight
            // through to the focused pane's PTY, byte-identical to the pre-§7
            // path: readline `backward-char` still works in a lone shell. The
            // tmux prefix engages the moment the user splits. The disable knob
            // (`ODYTTY_PANE_PREFIX=off`) and the nested-multiplexer
            // `Ctrl-b Ctrl-b` passthrough are unchanged for multi-pane tabs.
            // `active_is_single_pane()` is a cheap read on the active tab and is
            // checked first so non-prefix keys on a single pane never touch the
            // engine.
            if !self.sessions.active_is_single_pane()
                && !self.overlay.is_open()
                && !self.search.is_open()
                && self.active_modal() == ActiveModal::None
                // Prefer the shifted logical character for the second key so
                // tmux punctuation chords (`%` = Shift+5, `"` = Shift+') match
                // their stored bindings; fall back to the unshifted base key for
                // `Ctrl+<letter>` second keys and the prefix itself. Passing
                // only `binding_key` (`key_without_modifiers()`) here is the bug
                // that made `%`/`"` silently no-op on hardware.
                && let Some(chord) =
                    prefix_chord_from_winit(&logical, &binding_key, mods, self.super_key)
            {
                match self.prefix_engine.on_chord(chord, Instant::now()) {
                    PrefixOutcome::Inactive => {}
                    PrefixOutcome::Entered => {
                        // Prefix captured; await the second key. Repaint so a
                        // future pending-state affordance can show (none yet).
                        if let Some(window) = self.window.as_ref() {
                            window.request_redraw();
                        }
                        return;
                    }
                    PrefixOutcome::Cancelled => {
                        // Unknown second key (or timed-out prefix that did not
                        // re-enter): swallow it, fire nothing, back to normal.
                        return;
                    }
                    PrefixOutcome::Passthrough => {
                        // Doubled prefix (`Ctrl-b Ctrl-b`) → send the literal
                        // prefix byte (e.g. `0x02`) to the focused pane's PTY so
                        // a multiplexer running *inside* OdyTTY still receives
                        // its own prefix (K3 nested-multiplexer story). Return to
                        // live first, like any keystroke that reaches the shell.
                        let bytes = self.prefix_engine.passthrough_bytes();
                        if !bytes.is_empty() {
                            self.return_to_live();
                            self.write_pty_bytes(&bytes);
                        }
                        return;
                    }
                    PrefixOutcome::Action(action) => {
                        self.apply_pane_action(action);
                        return;
                    }
                }
            }
            let action = self
                .key_bindings
                .action_for(&binding_key, mods, self.super_key);
            // C10 + C22: the Settings/ThemePicker shortcuts sit ABOVE the
            // overlay-open guard so they can open their overlay from the live
            // terminal. Two guards keep that from misbehaving:
            //  - `!is_capturing_chord()` (C10): while the key-remap UI is armed
            //    to capture a chord, let the chord fall through to
            //    `handle_overlay_key` so Ctrl+Shift+, / Ctrl+Shift+H can be
            //    *assigned* to an action instead of pre-empting capture. The
            //    normal open/close toggle is unaffected — capture is only armed
            //    on a remap row.
            //  - `Press`-only (C22): a held chord auto-repeats; firing the
            //    toggle on every Repeat open/close-flickered the overlay. Act on
            //    the initial Press only; Repeats fall through (to the overlay
            //    key path once it is open) and are harmless.
            if event_type == KeyEventType::Press && !self.overlay.is_capturing_chord() {
                if action == Some(BindableAction::SettingsPanel) {
                    self.toggle_settings_overlay();
                    return;
                }
                if action == Some(BindableAction::ThemePicker) {
                    self.open_theme_picker_overlay();
                    return;
                }
            }
            if self.overlay.is_open() {
                self.handle_overlay_key(&logical, event_type);
                return;
            }
            if action == Some(BindableAction::CommandPalette) {
                self.open_command_palette_overlay();
                return;
            }
            if action == Some(BindableAction::SessionReplay) {
                self.open_replay_overlay();
                return;
            }
            if action == Some(BindableAction::ConnectionManager) {
                self.open_connection_overlay();
                return;
            }
            if action == Some(BindableAction::SessionAttach) {
                self.open_session_attach_overlay();
                return;
            }
            if action == Some(BindableAction::ThemeBuilder) {
                self.open_theme_builder_overlay();
                return;
            }
            if action == Some(BindableAction::Search) {
                self.toggle_search();
                return;
            }
            if self.search.is_open() {
                self.handle_search_key(logical);
                return;
            }
            // Modal-input gate: a new keyboard modal captures keys beneath the
            // overlay/search guards, above the BindableAction match (precedence
            // D-INFRA-4). Always None today ⇒ falls through unchanged.
            match self.active_modal() {
                ActiveModal::None => {}
                modal => {
                    self.route_modal_key(modal, &logical);
                    return;
                }
            }
            match action {
                Some(BindableAction::Copy) => {
                    self.handle_copy_shortcut();
                    return;
                }
                Some(BindableAction::Paste) => {
                    self.handle_paste_shortcut();
                    return;
                }
                Some(BindableAction::ScrollPageUp) => {
                    self.scroll_viewport(self.page_lines() as isize);
                    return;
                }
                Some(BindableAction::ScrollPageDown) => {
                    self.scroll_viewport(-(self.page_lines() as isize));
                    return;
                }
                // The next four route to thin per-feature handlers that live in
                // sibling `app` modules so future feature work fills them in
                // disjoint files. Each returns whether it consumed the key; a
                // handler that does not act yet returns `false`, so the chord
                // falls through to the PTY encode path below exactly as an
                // unbound key would (the plain path stays byte-identical).
                Some(BindableAction::JumpPromptPrev) => {
                    if self.jump_prompt_prev() {
                        return;
                    }
                }
                Some(BindableAction::JumpPromptNext) => {
                    if self.jump_prompt_next() {
                        return;
                    }
                }
                Some(BindableAction::SelectCommandOutput) => {
                    self.select_command_range(crate::core::CommandRangePart::Output);
                    return;
                }
                Some(BindableAction::SelectCommandWithPrompt) => {
                    self.select_command_range(crate::core::CommandRangePart::PromptAndCommand);
                    return;
                }
                Some(BindableAction::CopyCommandOutput) => {
                    self.copy_command_range(crate::core::CommandRangePart::Output);
                    return;
                }
                Some(BindableAction::CopyCommandWithPrompt) => {
                    self.copy_command_range(crate::core::CommandRangePart::PromptAndCommand);
                    return;
                }
                Some(BindableAction::SearchCommandOutput) => {
                    self.search_current_command_output();
                    return;
                }
                Some(BindableAction::JumpFailedCommandPrev) => {
                    self.jump_failed_command(crate::core::CommandDirection::Prev);
                    return;
                }
                Some(BindableAction::JumpFailedCommandNext) => {
                    self.jump_failed_command(crate::core::CommandDirection::Next);
                    return;
                }
                Some(BindableAction::ExportCommandOutput) => {
                    self.begin_command_output_export();
                    return;
                }
                Some(BindableAction::CopyMode) => {
                    if self.enter_copy_mode() {
                        return;
                    }
                }
                Some(BindableAction::Hints) => {
                    if self.activate_hints() {
                        return;
                    }
                }
                Some(BindableAction::ClearInput) => {
                    // IN1: clear the current shell input line. Sends a
                    // readline-style "move to start, kill to end" sequence
                    // (Ctrl+A, Ctrl+K) so the whole line is cleared regardless
                    // of cursor position. Returns the viewport to live like any
                    // keystroke that reaches the shell, then consumes the chord.
                    self.return_to_live();
                    self.write_pty_bytes(&[0x01, 0x0b]);
                    return;
                }
                Some(BindableAction::NewTab) => {
                    self.handle_new_tab();
                    return;
                }
                Some(BindableAction::NewWindow) => {
                    self.handle_new_window();
                    return;
                }
                Some(BindableAction::NextTab) => {
                    self.switch_to_next_tab();
                    return;
                }
                Some(BindableAction::PrevTab) => {
                    self.switch_to_prev_tab();
                    return;
                }
                Some(BindableAction::CloseTab) => {
                    if self.close_active_tab() {
                        return;
                    }
                    return;
                }
                Some(BindableAction::DuplicateTab) => {
                    // Duplicate = a fresh local shell in the active pane's cwd (F1
                    // cwd inheritance), NOT a process fork: scrollback and the
                    // running program are not copied. Routes through the same
                    // cwd-aware local-tab spawn as New Local Tab.
                    self.handle_new_local_tab();
                    return;
                }
                Some(BindableAction::NewWorkspace) => {
                    self.handle_new_workspace();
                    return;
                }
                Some(BindableAction::DuplicateWorkspace) => {
                    // Duplicate = a fresh workspace whose first shell opens in the
                    // active pane's cwd (F1 cwd inheritance), NOT a process fork:
                    // scrollback and running programs are not copied. Mirrors
                    // Duplicate Tab one level up.
                    self.handle_duplicate_workspace();
                    return;
                }
                Some(BindableAction::CloseWorkspace) => {
                    self.close_active_workspace();
                    return;
                }
                Some(BindableAction::RenameWorkspace) => {
                    self.enter_rename_workspace(self.sessions.active_workspace_index());
                    return;
                }
                Some(BindableAction::NextWorkspace) => {
                    self.switch_to_next_workspace();
                    return;
                }
                Some(BindableAction::PrevWorkspace) => {
                    self.switch_to_prev_workspace();
                    return;
                }
                Some(BindableAction::WorkspacePicker) => {
                    self.open_command_palette_overlay();
                    return;
                }
                Some(BindableAction::Search)
                | Some(BindableAction::CommandPalette)
                | Some(BindableAction::SessionReplay)
                | Some(BindableAction::ConnectionManager)
                | Some(BindableAction::SessionAttach)
                | Some(BindableAction::ThemeBuilder)
                | Some(BindableAction::SettingsPanel)
                | Some(BindableAction::ThemePicker)
                | None => {}
                // Direct split chords (GUI, Ctrl+Shift+E / Ctrl+Shift+O). These
                // two *creation* splits have direct global bindings so the first
                // split on a single-pane tab is reachable without the prefix
                // (which is gated off at single-pane for byte-identity). They
                // dispatch the same action the prefix `%`/`"` path fires, and
                // work at single-pane (create the first split) and multi-pane.
                Some(action @ (BindableAction::SplitColumns | BindableAction::SplitRows)) => {
                    self.apply_pane_action(action);
                    return;
                }
                // The remaining pane-management actions (§7) resolve only on the
                // multiplexer prefix and are excluded from the flat global
                // binding table (`is_pane_action`), so `action_for` never
                // returns one here. These arms exist for match exhaustiveness;
                // the prefix engine (K2) dispatches them before this flat match.
                Some(BindableAction::FocusPaneLeft)
                | Some(BindableAction::FocusPaneRight)
                | Some(BindableAction::FocusPaneUp)
                | Some(BindableAction::FocusPaneDown)
                | Some(BindableAction::FocusPaneNext)
                | Some(BindableAction::ClosePane)
                | Some(BindableAction::ZoomPane)
                | Some(BindableAction::EqualizePanes) => {}
            }
            // SMART-CTRLC: a plain Ctrl+C that matched no binding copies + clears
            // a local selection when the copy-or-interrupt policy is on, then
            // swallows the chord. With the policy off, no selection, or any other
            // key it returns false and falls through to the interrupt-byte encode
            // below, so the ^C path stays byte-identical. Inside the press-only
            // guard, so a key release never triggers a copy.
            if self.smart_ctrl_c_intercept(&logical, mods) {
                return;
            }
            // SELDEL-KEY: a plain Delete/Backspace with a local selection on the
            // editable prompt line deletes that selection through the same gated,
            // shell-integration-aware path as the right-click Delete/Cut, then
            // swallows the key. If a selection exists but no OSC 133 input
            // boundary is known, consume the key, clear the stale visual
            // selection, and show the shell-integration hint instead of sending
            // blind edit bytes. With no selection, or with a selection that is
            // not on editable input despite a known boundary, Delete/Backspace
            // still falls through to the shell. Gated to no Ctrl/Alt/Super so
            // word-delete chords (Ctrl+W, Alt+Backspace) still reach the shell.
            // Press-only via the enclosing guard.
            if is_selection_delete_key(&logical)
                && !mods.ctrl
                && !mods.alt
                && !self.super_key
                && (self.try_delete_selected_editable_input()
                    || self.try_handle_unavailable_selection_delete())
            {
                return;
            }
        }
        if self.overlay.is_open() {
            return;
        }

        // F6-i4: when the active pane is a dropped remote session showing the
        // reconnect prompt, keys drive the prompt — not the dead shell. Enter
        // re-establishes the connection in the same tab; Esc / Ctrl+D dismiss it
        // (close the tab). Every other key is swallowed so nothing reaches the
        // now-defunct PTY. This sits after the global-chord dispatch above, so a
        // tab/workspace switch or the command palette still work while a pane
        // awaits reconnect.
        // F6-i7: while a clipboard image awaits the paste-through confirm prompt,
        // keys drive the prompt (Enter uploads, Esc/Ctrl+D cancel) — not the
        // shell. Sits with the reconnect gate after the global-chord dispatch so
        // a tab/workspace switch or the palette still work while it is up.
        if self.pending_image_paste.is_some() {
            if event_type == KeyEventType::Press {
                self.handle_image_paste_key(&logical, mods);
            }
            return;
        }

        if self.sessions.active_awaiting_reconnect() {
            if event_type == KeyEventType::Press {
                self.handle_reconnect_key(&logical, mods);
            }
            return;
        }

        key_event_diagnostics::log_backspace_modes(&logical, key_modes, event_type);
        let mut bytes = Vec::new();
        if key_modes.win32_input {
            // W32IM owns all otherwise-unconsumed physical events, including
            // modifier releases. It therefore precedes Kitty and
            // modifyOtherKeys; application chords and modal UI were already
            // consumed above, and paste uses its separate byte path.
            if let Some(event) =
                map_win32_key_event(physical, &logical, &binding_key, mods, event_type)
            {
                bytes = input::encode_win32_key_event(event, event_type);
            }
        } else if let Some(key) = map_keypad_physical_key(physical) {
            bytes = input::encode_key_event(key, mods, key_modes, event_type);
        } else {
            match &logical {
                // `Key::Character` may carry more than one char (composed input);
                // encode each so multi-char text still reaches the shell intact.
                WinitKey::Character(text) => {
                    for ch in text.chars() {
                        bytes.extend_from_slice(&input::encode_key_event(
                            Key::Char(ch),
                            mods,
                            key_modes,
                            event_type,
                        ));
                    }
                }
                WinitKey::Named(named) => {
                    if let Some(key) = map_named_key(*named, mods.shift) {
                        bytes = input::encode_key_event(key, mods, key_modes, event_type);
                    }
                }
                // Dead keys / unidentified: nothing to send.
                _ => {}
            }
        }

        key_event_diagnostics::log_backspace_encoding(&logical, &bytes);
        if bytes.is_empty() {
            return;
        }
        // Any keystroke that reaches the shell snaps the viewport back to live,
        // so typing always returns to the prompt at the bottom.
        self.return_to_live();
        if let Ok(mut writer) = self.writer.lock() {
            let write_ok = writer.write_all(&bytes).is_ok();
            let flush_ok = writer.flush().is_ok();
            key_event_diagnostics::log_backspace_write(&logical, write_ok, flush_ok);
        } else {
            key_event_diagnostics::log_backspace_writer_lock_failed(&logical);
        }
    }

    pub(super) fn handle_held_exit_key(&mut self, event_type: KeyEventType) -> bool {
        let Some(token) = self.held_exit else {
            return false;
        };
        if self.sessions.position_of_token(token).is_none() {
            self.held_exit = None;
            return false;
        }
        if self.sessions.active_id() != token {
            return false;
        }
        if event_type == KeyEventType::Release {
            return true;
        }

        self.held_exit = None;
        self.settle_divider_for_surface_change();
        let _ = self.finish_shell_exit(token);
        true
    }

    /// Drive the in-pane reconnect prompt (F6-i4) for the active dropped remote
    /// session. Enter re-establishes the connection in the same tab slot;
    /// Escape or Ctrl+D dismisses the prompt and closes the tab. Any other key
    /// is a no-op (the prompt stays up). Called only on a key press while the
    /// active session is awaiting reconnect.
    pub(super) fn handle_reconnect_key(&mut self, logical: &WinitKey, mods: Modifiers) {
        let token = self.sessions.active_id();
        match logical {
            WinitKey::Named(NamedKey::Enter) => {
                if self.sessions.reconnect(token) {
                    self.on_active_session_changed();
                }
            }
            WinitKey::Named(NamedKey::Escape) => {
                self.dismiss_reconnect_and_close(token);
            }
            // Ctrl+D — the shell's own end-of-input chord — dismisses too.
            WinitKey::Character(text)
                if mods.ctrl && !self.super_key && text.eq_ignore_ascii_case("d") =>
            {
                self.dismiss_reconnect_and_close(token);
            }
            _ => {}
        }
    }

    /// Dismiss the reconnect prompt and close the dropped tab. Closing the last
    /// tab of the last workspace signals app exit, mirroring a normal shell exit;
    /// the loop drains `pending_exit` after this window event returns.
    pub(super) fn dismiss_reconnect_and_close(&mut self, token: SessionToken) {
        if self.sessions.close_shell_exited(token) {
            self.pending_exit = true;
        } else {
            self.on_active_session_changed();
        }
    }

    pub(super) fn toggle_settings_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        // ABOUT: refresh the About data with the live GPU adapter before the
        // panel opens. Cheap to recompute; the adapter is present once the
        // renderer is up (`None` only on the headless/early path).
        let adapter = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.adapter_diagnostics().clone());
        self.overlay
            .set_about_info(crate::native::about::AboutInfo::collect(adapter));
        self.overlay.toggle_settings();
        self.request_selection_redraw();
    }

    pub(super) fn open_settings_overlay_target(
        &mut self,
        target: crate::native::overlay::SettingsTarget,
    ) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        let adapter = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.adapter_diagnostics().clone());
        self.overlay
            .set_about_info(crate::native::about::AboutInfo::collect(adapter));
        self.overlay.open_settings_target(target);
        self.request_selection_redraw();
    }

    pub(super) fn open_theme_picker_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_theme_picker(&self.settings);
        self.request_selection_redraw();
    }

    pub(super) fn open_theme_builder_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_theme_builder(&self.settings);
        self.request_selection_redraw();
    }

    /// Capture the focused pane's live colors into a theme draft and open the
    /// theme editor on it (THEME-CAPTURE). Entry point for both the command
    /// palette row and the settings/menu surface.
    pub(super) fn open_theme_capture_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        let spec = self.capture_live_theme_spec();
        self.overlay.open_theme_capture(&self.settings, spec);
        self.request_selection_redraw();
    }

    /// Snapshot the focused pane's **effective** dynamic-color state into a
    /// theme draft. Live `OSC 4`/`10`/`11`/`12` overrides win; where no
    /// override exists the theme-seeded value is used, because the core seeds
    /// its base colors and base palette from the active theme. The draft's
    /// derived roles come from the documented heuristics in
    /// [`crate::theme::capture_spec`].
    ///
    /// Reads state only — capturing changes nothing about the pane, the
    /// terminal, or the applied theme.
    pub(super) fn capture_live_theme_spec(&self) -> crate::theme::ThemeSpec {
        let (colors, palette) = {
            let terminal = crate::native::lock_recover(&self.terminal);
            (
                terminal.dynamic_colors().clone(),
                terminal.effective_ansi_palette(),
            )
        };
        let live = crate::theme::LiveColors {
            foreground: srgb_of(colors.foreground),
            background: srgb_of(colors.background),
            cursor: srgb_of(colors.cursor),
            palette: std::array::from_fn(|index| srgb_of(palette[index])),
        };
        crate::theme::capture_spec(&live, &captured_theme_name(self.settings.theme.name))
    }

    pub(super) fn open_key_bindings_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_key_bindings(&self.settings);
        self.request_selection_redraw();
    }

    pub(super) fn open_font_picker_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_font_picker(&self.settings);
        self.request_selection_redraw();
    }

    pub(super) fn handle_overlay_key(&mut self, logical: &WinitKey, event_type: KeyEventType) {
        // KB-REMAP chord capture (R2 KILL-SHOT): when the key-remap modal is
        // armed to capture a chord, this MUST be the first thing we do — route
        // the raw key through `chord_from_winit` BEFORE the lossy
        // `overlay_input_from_winit` mapper, which would otherwise collapse a
        // chord like Ctrl+Shift+K into an `OverlayInput` (or, for Enter/Esc, an
        // Activate/Close) and the modifiers would be lost. `is_capturing_chord`
        // is `false` whenever the modal is closed or merely browsing, so this
        // never disturbs normal overlay navigation (R1).
        if self.overlay.is_capturing_chord() {
            let chord =
                crate::native::bindings::chord_from_winit(logical, self.modifiers, self.super_key);
            let outcome = self.overlay.deliver_chord(chord);
            self.apply_overlay_outcome(outcome);
            self.request_selection_redraw();
            return;
        }

        let Some(input) = overlay_input_from_winit(logical, self.modifiers) else {
            self.request_selection_redraw();
            return;
        };

        let outcome = self.overlay.handle_input(input);
        self.apply_overlay_outcome_with_policy(outcome, event_type == KeyEventType::Repeat);
        self.request_selection_redraw();
    }

    pub(super) fn key_modes(&self) -> KeyModes {
        self.terminal
            .lock()
            .map(|terminal| key_modes_from_core(terminal.keyboard_modes()))
            .unwrap_or_default()
    }

    pub(super) fn toggle_search(&mut self) {
        if self.overlay.is_open() {
            self.overlay.close();
            self.request_selection_redraw();
        }
        if self.search.is_open() {
            self.close_search(true);
        } else {
            self.search_restore_viewport = Some(self.viewport.offset());
            self.search.open();
            self.selection.clear();
            self.selection_block = false;
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
            self.last_selection_autoscroll = None;
            self.refresh_search_matches();
        }
        self.request_selection_redraw();
    }

    pub(super) fn close_search(&mut self, restore_viewport: bool) {
        self.search.close();
        let restore_offset = restore_viewport
            .then(|| self.search_restore_viewport.take())
            .flatten();
        self.search_restore_viewport = None;

        if let Some(offset) = restore_offset {
            let scrollback_len = self.scrollback_len();
            if self.viewport.jump_to(offset, scrollback_len) {
                self.on_viewport_changed();
            }
        }
    }

    pub(super) fn handle_search_key(&mut self, logical: WinitKey) {
        match logical {
            WinitKey::Named(NamedKey::Escape) => {
                self.close_search(true);
                self.request_selection_redraw();
            }
            WinitKey::Named(NamedKey::Enter) => {
                self.refresh_search_matches();
                if self.modifiers.shift {
                    self.search.prev();
                } else {
                    self.search.next();
                }
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            WinitKey::Named(NamedKey::Backspace) => {
                self.search.backspace();
                self.refresh_search_matches();
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            WinitKey::Named(NamedKey::Space) if !self.modifiers.ctrl && !self.modifiers.alt => {
                self.search.push_char(' ');
                self.refresh_search_matches();
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            WinitKey::Character(text) if !self.modifiers.ctrl && !self.modifiers.alt => {
                for ch in text.chars() {
                    self.search.push_char(ch);
                }
                self.refresh_search_matches();
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            _ => {}
        }
    }

    pub(super) fn refresh_search_matches(&mut self) {
        if !self.search.is_open() {
            return;
        }
        let session = self.sessions.active_mut();
        if let Ok(terminal) = session.terminal.lock() {
            session.search.refresh(&terminal);
        }
    }

    pub(super) fn jump_to_current_search_match(&mut self) {
        let scrollback_len = self.scrollback_len();
        let Some(offset) = self
            .search
            .viewport_offset_for_current(scrollback_len, self.grid)
        else {
            return;
        };
        if self.viewport.jump_to(offset, scrollback_len) {
            self.on_viewport_changed();
        }
    }

    /// Begin the image paste-through confirm flow (F6-i7) when the clipboard
    /// holds an image, the active tab is a remote *integrated* ssh session, and
    /// the feature is enabled. Reads and PNG-encodes the clipboard image, refuses
    /// an over-cap image with a one-line notice, and otherwise arms the in-pane
    /// confirm prompt — nothing is uploaded until Enter confirms. A no-op (and no
    /// prompt) on a local/plain-ssh tab, with the setting `off`, or with no
    /// clipboard image, so the default paste path is untouched.
    pub(super) fn try_begin_image_paste(&mut self) {
        // Only a remote integrated tab is an upload target; this is also the gate
        // that keeps local/plain-ssh tabs completely unaffected.
        let Some(target) = self.sessions.active_remote_upload_target() else {
            return;
        };
        if !self.settings.remote_image_paste.is_enabled() {
            return;
        }
        let Some(image) = self.clipboard.read_image_png() else {
            return;
        };
        let png = match image {
            super::super::clipboard::ClipboardImagePng::Ready(png) => png,
            super::super::clipboard::ClipboardImagePng::TooLarge { limit } => {
                self.write_active_banner(&format!(
                    "\r\n\x1b[1;31m image too large \x1b[0m exceeds the {} processing cap — not uploaded\r\n",
                    format_byte_size(limit),
                ));
                return;
            }
        };
        let size = png.len();
        let cap = crate::settings::REMOTE_IMAGE_PASTE_MAX_BYTES;
        if size > cap {
            self.write_active_banner(&format!(
                "\r\n\x1b[1;31m image too large \x1b[0m {} exceeds the {} upload cap — not uploaded\r\n",
                format_byte_size(size),
                format_byte_size(cap),
            ));
            return;
        }
        let session = self.sessions.active_id();
        self.write_active_banner(&format!(
            "\r\n\x1b[1;36m upload image \x1b[0m {} to {}?  Enter: upload · Esc: cancel\r\n",
            format_byte_size(size),
            target,
        ));
        self.pending_image_paste = Some(PendingImagePaste { session, png });
    }

    /// Drive the image paste-through confirm prompt (F6-i7). Enter uploads the
    /// held image and, on success, pastes the remote path into the shell;
    /// Esc/Ctrl+D cancel with nothing sent. Any other key leaves the prompt up.
    /// Called only on a key press while a paste is pending.
    pub(super) fn handle_image_paste_key(&mut self, logical: &WinitKey, mods: Modifiers) {
        match logical {
            WinitKey::Named(NamedKey::Enter) => self.commit_image_paste(),
            WinitKey::Named(NamedKey::Escape) => self.cancel_image_paste(),
            WinitKey::Character(text)
                if mods.ctrl && !self.super_key && text.eq_ignore_ascii_case("d") =>
            {
                self.cancel_image_paste();
            }
            _ => {}
        }
    }

    /// Cancel a pending image paste: drop the held bytes, note it in the pane,
    /// and send nothing. The clipboard is untouched.
    pub(super) fn cancel_image_paste(&mut self) {
        if self.pending_image_paste.take().is_some() {
            self.write_active_banner("\r\n\x1b[2m image paste cancelled\x1b[0m\r\n");
        }
    }

    /// Confirm a pending image paste: hand the held PNG to a background upload
    /// worker for the originating remote session. The worker uploads over `ssh`
    /// (reusing the live master), then pastes the remote path into that shell on
    /// success or writes a one-line failure notice on error — so the UI never
    /// blocks on the transfer. Under `cfg(test)` the spawn is replaced by a
    /// record into `last_image_upload`, so the confirm flow is testable without a
    /// network.
    pub(super) fn commit_image_paste(&mut self) {
        let Some(pending) = self.pending_image_paste.take() else {
            return;
        };
        #[cfg(test)]
        {
            self.last_image_upload = Some((pending.session, pending.png.len()));
        }
        #[cfg(not(test))]
        {
            let Some(job) = self.sessions.remote_upload_job(pending.session) else {
                self.write_active_banner(
                    "\r\n\x1b[1;31m image upload failed \x1b[0m session is gone\r\n",
                );
                return;
            };
            if let Err(error) = image_paste::spawn_upload_worker(job, pending.png) {
                // Thread exhaustion: the worker could not start, so the confirmed
                // paste is dropped. Surface it in the pane rather than losing it
                // silently (LOW-02).
                tracing::warn!("image upload worker spawn failed: {error}");
                self.write_active_banner(
                    "\r\n\x1b[1;31m image upload failed \x1b[0m too many threads; try again\r\n",
                );
            }
        }
    }

    /// Write a one-line SGR banner into the active pane's terminal model and
    /// request a redraw. Shared by the reconnect-style in-pane prompts/notices
    /// (F6-i7): standard SGR so it renders in every theme, leading/trailing CRLF
    /// so it lands on its own line.
    pub(super) fn write_active_banner(&mut self, banner: &str) {
        crate::native::lock_recover(&self.terminal).advance(banner.as_bytes());
        self.needs_rebuild = true;
        self.last_render_signature = None;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// SMART-CTRLC: handle a plain `Ctrl+C` under the copy-or-interrupt policy.
    ///
    /// Returns `true` (the chord was consumed) only when the policy is active,
    /// the chord is *plain* `Ctrl+C` (Ctrl held; no Shift/Alt/Super — so the
    /// `Ctrl+Shift+C` copy binding, handled earlier, never reaches here), and a
    /// local OdyTTY selection exists: it copies the selection and clears it so an
    /// immediate second `Ctrl+C` interrupts. In every other case it returns
    /// `false` and the caller falls through to the normal interrupt-byte encode,
    /// so the `^C` path is byte-identical when the policy is off, when nothing is
    /// selected, or for any non-`Ctrl+C` key. Press-only by virtue of the
    /// enclosing `event_type != Release` guard at the call site.
    pub(super) fn smart_ctrl_c_intercept(&mut self, logical: &WinitKey, mods: Modifiers) -> bool {
        if !self.settings.smart_ctrl_c.is_active() {
            return false;
        }
        if !mods.ctrl || mods.shift || mods.alt || self.super_key {
            return false;
        }
        if !is_ctrl_c_key(logical) {
            return false;
        }
        if self.selection.range().is_none() {
            return false;
        }
        self.handle_copy_shortcut();
        self.selection.clear();
        self.selection_block = false;
        self.request_selection_redraw();
        true
    }

    /// Select the entire buffer — the full scrollback plus the visible grid
    /// (IN2 Select All). The range is stored in absolute row space, so it stays
    /// meaningful as the viewport scrolls; the copy path resolves whatever is
    /// visible at copy time (the app-wide selection→clipboard contract). Also
    /// mirrors the selection to PRIMARY like any other selection. No-op on an
    /// empty grid.
    pub(super) fn handle_select_all(&mut self) {
        let columns = self.grid.columns;
        let rows = self.grid.rows;
        if columns == 0 || rows == 0 {
            return;
        }
        let end_row = self.scrollback_len() + rows - 1;
        self.selection.set_range(AbsoluteSelectionRange {
            start: selection::AbsoluteCellPoint { row: 0, column: 0 },
            end: selection::AbsoluteCellPoint {
                row: end_row,
                column: columns - 1,
            },
        });
        self.selection_block = false;
        self.write_primary_selection();
        self.request_selection_redraw();
    }
}

// Keyboard event arms moved verbatim from the `ApplicationHandler` match; the
// match itself remains the stable ingress in `mod.rs`.
impl App {
    /// Handle one `ModifiersChanged` event.
    ///
    /// `winit` reports modifier state separately from key presses, so the
    /// cached state must be updated here for the next `KeyboardInput` to encode
    /// with the modifiers held at press time.
    pub(super) fn on_modifiers_changed(&mut self, state: winit::event::Modifiers) {
        let state = state.state();
        let was_ctrl = self.modifiers.ctrl;
        self.modifiers = Modifiers {
            ctrl: state.control_key(),
            alt: state.alt_key(),
            shift: state.shift_key(),
        };
        self.super_key = state.super_key();
        key_event_diagnostics::log_modifiers_changed(self.modifiers, self.super_key);
        // UX-A (Phase 11): the Ctrl+hover armed underline appears/clears
        // as Ctrl toggles while a path is hovered, so a Ctrl transition
        // there must trigger a rebuild + redraw to repaint the span.
        // Gated on `interactive_paths` + a hovered path, so the default /
        // feature-off path is untouched (byte-identical).
        if was_ctrl != self.modifiers.ctrl
            && self.settings.interactive_paths
            && self.hovered_path.is_some()
        {
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Handle one `KeyboardInput` event.
    ///
    /// Classifies press/repeat/release, resolves the binding key without
    /// modifiers, records diagnostics, and enters the ordered precedence chain.
    /// Same body, same order, reached from the same match arm.
    pub(super) fn on_keyboard_input(&mut self, event: winit::event::KeyEvent) {
        let event_type = match event.state {
            ElementState::Pressed if event.repeat => KeyEventType::Repeat,
            ElementState::Pressed => KeyEventType::Press,
            ElementState::Released => KeyEventType::Release,
        };
        let binding_key = event.key_without_modifiers();
        key_event_diagnostics::log_keyboard_event(
            &event,
            &binding_key,
            self.modifiers,
            self.super_key,
        );
        self.handle_key_event(
            event.logical_key,
            binding_key,
            event.physical_key,
            event_type,
        );
    }
}

/// Convert a core [`RgbColor`](crate::core::RgbColor) to the theme layer's
/// sRGB byte triple. The two layers deliberately do not share a color type —
/// the terminal core stays presentation-agnostic — so the conversion lives at
/// the boundary that needs it.
fn srgb_of(color: crate::core::RgbColor) -> crate::theme::Srgb {
    (color.red, color.green, color.blue)
}

/// Default draft name for a capture: the active theme's name with a `-capture`
/// suffix, so a captured draft is obviously derived and never silently
/// overwrites the theme it came from. The editor prompts for the final name
/// before saving.
fn captured_theme_name(active: &str) -> String {
    let base = active.trim();
    if base.is_empty() {
        "captured".to_owned()
    } else {
        format!("{base}-capture")
    }
}

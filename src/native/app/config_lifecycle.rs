// SPDX-License-Identifier: GPL-3.0-only
//! Configuration lifecycle for the native window: settings reload, settings
//! application through the reload seam, first-run and overlay-driven persistence,
//! and workspace-shape autosave and restore maintenance.
//!
//! Every entry point keeps its existing call site and ordering; `App` remains the
//! single state owner.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettingsApplySource {
    ConfigReload,
    OverlayEdit,
    /// A setting mutated by native chrome (a rail/tab-bar affordance) rather
    /// than by the settings panel, but applied through the SAME full reload
    /// seam so the rail reflow / grid recompute run. Unlike [`OverlayEdit`]
    /// (which assumes the panel itself committed the value and keeps the
    /// panel's edit overlay as the source of truth), this rebases the open
    /// panel onto the external value as a fresh clean baseline, so its row
    /// reflects the change instead of showing a stale copy. Any future
    /// out-of-panel toggle that needs the seam uses this variant.
    ExternalChrome,
}

pub(super) fn bloom_options(settings: &Settings) -> BloomOptions {
    BloomOptions {
        enabled: settings.effective_bloom_enabled(),
        threshold: settings.effective_bloom_threshold(),
        intensity: settings.effective_bloom_intensity(),
        radius: settings.effective_bloom_radius(),
    }
}

pub(super) fn crt_options(settings: &Settings) -> CrtOptions {
    CrtOptions {
        enabled: settings.effective_crt_enabled(),
        scanline_intensity: settings.effective_crt_scanline_intensity(),
        scanline_period: settings.crt_scanline_period,
        vignette_strength: settings.effective_crt_vignette_strength(),
        curvature: settings.effective_crt_curvature(),
    }
}

impl App {
    pub(super) fn options_for_settings(&self, settings: &Settings) -> NativeOptions {
        let parsed = NativeOptions::from_settings(settings);
        NativeOptions {
            title: self.options.title.clone(),
            working_directory: self.options.working_directory.clone(),
            command: self.options.command.clone(),
            initial_grid: self.options.initial_grid,
            font_family: parsed.font_family,
            font_weight: parsed.font_weight,
            font_path: parsed.font_path,
            font_size_px: parsed.font_size_px,
            text_gamma: parsed.text_gamma,
            subpixel: parsed.subpixel,
            window_padding_px: parsed.window_padding_px,
            line_height: parsed.line_height,
            box_thickness: parsed.box_thickness,
            attach_session: self.options.attach_session.clone(),
            bare_launch: self.options.bare_launch,
            app_id: self.options.app_id.clone(),
            hold: self.options.hold,
        }
    }

    pub(super) fn poll_config_reload(&mut self, now: Instant) {
        match self.settings_reloader.poll(now) {
            SettingsReloadOutcome::Unchanged | SettingsReloadOutcome::Deleted => {}
            SettingsReloadOutcome::Reloaded { settings, warnings } => {
                // Non-fatal parse notices (unknown/typo'd keys, clamped values)
                // are surfaced but never block the reload: apply the usable
                // settings, consistent with the startup path.
                for warning in warnings {
                    tracing::warn!(warning = %warning, "config reload notice");
                }
                self.apply_reloaded_settings(settings);
            }
            SettingsReloadOutcome::Unreadable { message } => {
                tracing::warn!(message = %message, "config reload ignored");
            }
        }
    }

    pub(super) fn apply_reloaded_settings(&mut self, reloaded: Settings) {
        self.apply_settings_through_reload_seam(reloaded, SettingsApplySource::ConfigReload);
    }

    pub(super) fn apply_overlay_settings(&mut self, reloaded: Settings) {
        self.apply_settings_through_reload_seam(reloaded, SettingsApplySource::OverlayEdit);
    }

    pub(super) fn queue_overlay_settings(&mut self, settings: Settings) {
        self.pending_overlay_settings = Some(settings);
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    pub(super) fn flush_pending_overlay_settings(&mut self) {
        if let Some(settings) = self.pending_overlay_settings.take() {
            self.apply_overlay_settings(settings);
        }
    }

    /// Persist a first-run marker when the onboarding card is dismissed, so it
    /// does not reshow on the next launch. Onboarding's gate is purely whether
    /// `odytty.conf` exists, and plain dismissal writes nothing; this ensures
    /// the file exists (without clobbering it if the user already has one).
    ///
    /// Best-effort: a write failure is logged but never blocks dismissal, and
    /// the onboarding overlay has no save UI to surface an error through.
    pub(super) fn persist_first_run_config(&mut self) {
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        if let Err(error) = ensure_config_file_exists_at(path) {
            tracing::warn!(error = %error, "could not record first-run marker");
        }
    }

    pub(super) fn save_overlay_settings(&mut self, changes: &[crate::settings::SettingEdit]) {
        self.flush_pending_overlay_settings();
        let Some(path) = self.settings_reloader.config_path() else {
            self.overlay
                .save_failed("could not resolve odytty.conf path".to_owned());
            return;
        };
        match write_settings_changes_to_path(path, changes) {
            Ok(result) => {
                // BUG 2 (FONT-SAVE-CORRECTNESS): a Save must also apply LIVE, not
                // only at restart. Re-read the just-written config as startup does
                // (`Settings::from_env`, same path + env) and route it through the
                // shared reload seam — no duplicated reload logic. Idempotent: a
                // live-previewed value and the later background poll both no-op.
                // Apply before notifying the overlay: pickers return to Settings
                // on success, and rebasing while their mode is still active keeps
                // the panel's displayed config token aligned with the new theme.
                if result.changed > 0 {
                    let reloaded = Settings::from_env();
                    self.apply_overlay_settings(reloaded);
                }
                self.overlay.save_succeeded(result.changed);
            }
            Err(error) => self.overlay.save_failed(error.to_string()),
        }
    }

    pub(super) fn save_overlay_theme(
        &mut self,
        request: crate::native::theme_builder::ThemeBuilderSaveRequest,
    ) {
        let Some(config_path) = self.settings_reloader.config_path() else {
            self.overlay
                .save_failed("could not resolve odytty.conf path".to_owned());
            return;
        };
        let Some(theme_dir) = user_theme_dir_for_config(config_path) else {
            self.overlay
                .save_failed("could not resolve theme directory".to_owned());
            return;
        };
        let saved_name = request.name.clone();
        let path = match save_theme_to_dir(&theme_dir, &request) {
            Ok(path) => path,
            Err(error) => {
                self.overlay
                    .save_failed(format!("could not write theme file: {error}"));
                return;
            }
        };
        let changes = [SettingEdit {
            key: "theme",
            env: THEME_ENV,
            value: saved_name.clone(),
        }];
        match write_settings_changes_to_path(config_path, &changes) {
            Ok(result) => {
                // The theme file itself may have changed even when the config
                // already names it, so always re-read before closing the builder.
                // This also replaces preview-only color state and stale config
                // metadata with the canonical saved theme in one transition.
                let reloaded = Settings::from_env();
                self.apply_overlay_settings(reloaded);
                self.overlay
                    .theme_builder_save_succeeded(&saved_name, &path, result.changed)
            }
            Err(error) => self.overlay.save_failed(error.to_string()),
        }
    }

    /// Whether a settings reload that touched `shell_integration` should raise
    /// the "applies to new shells" notice. True only on a genuine OFF->ON
    /// transition while a live session exists — silent on startup (no
    /// transition), an ON->ON reload, the ON->OFF reverse toggle, or an OFF->ON
    /// with no running shell to inform. Pure so the gating is exhaustively
    /// unit-tested without standing up an App.
    pub(super) fn should_announce_shell_integration_to_new_shells(
        was_enabled: bool,
        now_enabled: bool,
        has_live_session: bool,
    ) -> bool {
        !was_enabled && now_enabled && has_live_session
    }

    pub(super) fn apply_settings_through_reload_seam(
        &mut self,
        reloaded: Settings,
        source: SettingsApplySource,
    ) {
        // Test-only isolation for every process-global render value this seam
        // republishes. In a test binary the seam runs on a thread that shares
        // that state with every other test, so an unguarded republish is both a
        // race (a parallel reader observes the transient value) and a leak (the
        // value persists for the rest of the binary). The guard is taken at the
        // top of the seam rather than beside the color publish because the
        // stem-darkening gain is republished earlier, through the text-options
        // apply below; a guard placed after that point would snapshot the
        // already-published gain and restore the leak instead of removing it.
        // The reloadable-values helper on the next lines republishes the atlas
        // and shaping switches unconditionally -- before it compares old and
        // new settings -- so the top of the seam is also the only placement
        // that precedes those writes.
        // It restores the prior state when the seam returns, and it is
        // re-entrant, so a test body that already holds it keeps ownership of
        // restoration. Compiled out of the shipping binary: the publishes
        // themselves, and their ordering, are unchanged.
        #[cfg(test)]
        let _render_globals = crate::test_lock::render_globals_lock();
        // A permission prompt is tied to the policy snapshot that produced it.
        // Any live settings apply cancels it before equality checks or model
        // updates, so a stale prompt cannot authorize under changed policy.
        self.cancel_osc52_prompt();
        let mut next_settings = self.settings.clone();
        if !apply_reloadable_values(&mut next_settings, reloaded) {
            return;
        }
        // Capture the prior shell-integration state BEFORE `self.settings` is
        // replaced below, so a genuine OFF->ON toggle can be distinguished from
        // an unchanged reload (the new-shells notice fires only on the
        // transition, never on every reload).
        let shell_integration_was_enabled = self.settings.shell_integration;
        // F4 ODP-7: capture whether the tab bar is currently shown before the
        // settings swap, so a live `always_show_tab_bar` toggle can recompute the
        // content grid (the bar reserves a row; appearing/disappearing changes
        // the usable height). Nothing else in this reload path touches the tab
        // bar's visibility, so this is the only trigger for that recompute.
        let tab_bar_was_shown = self.should_show_tab_bar();
        // Capture the workspace-rail visibility and side too — a live
        // `workspace_rail` / `tab_bar_placement` change flips the reserved band
        // (columns off a side) without changing the top bar, so it needs the
        // same grid recompute.
        let rail_was_shown = self.should_show_workspace_rail();
        let rail_side_was = self.workspace_rail_side();

        let next_options = self.options_for_settings(&next_settings);
        let (text_rebuilt, padding_changed) = match self.gpu.as_mut() {
            Some(gpu) => {
                let text_rebuilt = match gpu
                    .apply_text_options(&next_options, next_settings.effective_stem_darken())
                {
                    Ok(changed) => changed,
                    Err(err) => {
                        tracing::warn!(error = %err, "config reload ignored: text options apply failed");
                        return;
                    }
                };
                let padding_changed = gpu.set_window_padding_px(next_options.window_padding_px);
                (text_rebuilt, padding_changed)
            }
            None => (false, false),
        };

        self.settings = next_settings;
        self.options = next_options;
        // Phase 2 output recording: fan the live `session_replay` state out to
        // every session's recorder so a config-reload / settings-panel toggle
        // takes effect immediately. Off (the default) is a cheap no-op that
        // also frees any buffered frames, so the plain path is unaffected.
        self.sessions
            .set_recording_enabled(self.settings.session_replay);
        self.sessions
            .set_shell_integration_enabled(self.settings.shell_integration);
        // Shell-integration hooks are injected only at spawn time, so enabling
        // the setting mid-session cannot retroactively integrate the shell that
        // is already running — only new tabs/panes pick it up. Surface an honest
        // transient notice on the genuine OFF->ON transition while a shell is
        // live, instead of silently appearing to do nothing.
        if Self::should_announce_shell_integration_to_new_shells(
            shell_integration_was_enabled,
            self.settings.shell_integration,
            !self.sessions.is_empty(),
        ) {
            self.raise_open_notice(
                "Shell integration applies to new shells — open a new tab or split to activate."
                    .to_owned(),
            );
        }
        // WIN-DECOR: apply a live decorations change immediately so the panel
        // toggle takes effect without a restart. `set_decorations` is
        // idempotent (calling it with the current value is a no-op), so this is
        // safe to call unconditionally on every reload. The window always
        // exists before a settings reload can fire.
        if let Some(window) = self.window.as_ref() {
            window.set_decorations(self.settings.window_decorations);
        }
        // OS-THEME: the active theme is the authored `settings.theme` unless an
        // OS dark/light override is active, in which case it wins — so a config
        // reload (which may change the authored theme or the dark/light pair)
        // re-derives the correct active theme rather than clobbering a live OS
        // override back to the authored theme. With `follow_os_theme` off this
        // returns exactly `self.settings.theme`, byte-identical to before.
        self.theme = self.resolve_active_theme();
        // U4: compute the effective (CVD-adapted) theme once at this change
        // chokepoint and publish IT to every renderer seam below. Off returns
        // the authored theme unchanged (byte-identical plain path); the cache
        // makes an unchanged theme/mode/strength a cheap clone. A later step can
        // route the theme builder's live preview around this compute (via
        // `cvd_theme::effective_theme`) so authoring stays WYSIWYG; that bypass
        // is not wired yet, so a preview is adapted like any other application
        // while a CVD mode is active (off by default).
        self.effective_theme = self.cvd_cache.resolve(
            &self.theme,
            self.settings.cvd_mode,
            self.settings.cvd_strength,
        );
        self.visual = self.settings.visual;
        self.themed_ui_roles = self.settings.themed_ui_roles;
        self.key_bindings = KeyBindings::from_overrides(&self.settings.key_bindings);
        self.prefix_engine = PrefixEngine::from_settings(&self.settings);
        match source {
            SettingsApplySource::ConfigReload => self.overlay.refresh_settings(&self.settings),
            SettingsApplySource::OverlayEdit => self.overlay.apply_settings(&self.settings),
            SettingsApplySource::ExternalChrome => self
                .overlay
                .rebase_settings_panel_onto_external(&self.settings),
        }
        // U4: all theme publishes read `effective_theme` (the authored theme
        // after CVD adaptation; identical to it when off), so the renderer sees
        // the adapted colors while `self.theme` keeps the authored one for
        // save/round-trip.
        text::set_default_colors(
            self.effective_theme.foreground,
            self.effective_theme.background,
        );
        text::set_ansi_palette(&self.effective_theme.palette);
        // RV1: republish the minimum-contrast floor so a live `min_contrast`
        // edit takes effect on the next frame (the grid resolve seam reads it
        // per cell). Mirrors the palette republish above; passthrough at 1.0.
        text::set_min_contrast(self.settings.effective_min_contrast());
        // NF21-4: fan the theme colors, palette, OSC 52 read gate, cursor
        // defaults and scrollback cap over EVERY session, not just the active
        // one through `Deref`. A background tab (or a background workspace's
        // tabs) otherwise answered OSC 4/10/11 with the pre-reload theme, kept a
        // stale cursor default, and carried a model `osc52_read` that could
        // disagree with the app-level answer-time gate. All values are app-global
        // for the reload, so one arena sweep applies the whole model state
        // consistently.
        self.apply_model_state_to_all_sessions();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_theme(self.effective_theme);
            gpu.set_visual(self.visual);
            gpu.set_text_gamma(self.settings.text_gamma);
            gpu.set_bloom(bloom_options(&self.settings));
            gpu.set_crt(crt_options(&self.settings));
            // ID3/U5: push the background-image settings. The scrim is computed
            // against `effective_theme` (the same CVD/OS-resolved background the
            // RV1 floor references), so the floor stays valid at any opacity.
            gpu.set_background_image(
                self.settings.effective_background_treatment()
                    == crate::settings::BackgroundTreatment::Image,
                self.settings.background_image.as_deref(),
                self.settings.background_blur_radius,
                self.settings.background_image_scrim,
                self.settings.cell_bg_opacity,
                self.effective_theme,
            );
            // SELECTION-OPACITY: push the independent selection strength so a
            // settings-panel or config change repaints an on-screen selection.
            gpu.set_selection_opacity(self.settings.selection_opacity);
            // COLORED-BG-FLOOR: push the colored-background opacity floor so a
            // settings-panel or config change repaints colored blocks live.
            gpu.set_colored_bg_opacity(self.settings.colored_bg_opacity);
            // TEXT-BRIGHTNESS: push the glyph-foreground lift live.
            gpu.set_text_brightness(self.settings.text_brightness);
        }

        if text_rebuilt || padding_changed {
            let resize = self.gpu.as_ref().and_then(|gpu| {
                let cell = gpu.cell();
                if let Ok(mut terminal) = self.terminal.lock() {
                    terminal.set_cell_metrics(cell.width, cell.height);
                }
                self.window.as_ref().map(|window| {
                    pending_resize_for_surface(cell, gpu.window_padding(), window.inner_size())
                })
            });
            if let Some(resize) = resize {
                self.apply_grid_resize(resize);
            }
        }

        // F4 ODP-7 / F4-V2: if a live toggle flipped the bar's visibility OR its
        // placement (top↔left changes the reserved axis), reserve/reclaim the tab
        // chrome now so the content grid matches. No-op when both are unchanged.
        if self.should_show_tab_bar() != tab_bar_was_shown
            || self.should_show_workspace_rail() != rail_was_shown
            || self.workspace_rail_side() != rail_side_was
        {
            self.recompute_grid_for_tab_bar();
        }

        self.last_render_signature = None;
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);

        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// WP2 sub-ODP 8d: record whether this instance holds the primary-instance
    /// lock. Only the primary autosaves and restores the workspace shape; a
    /// second concurrent window stays inert on both.
    pub(in crate::native) fn set_primary_instance(&mut self, primary: bool) {
        self.autosave_is_primary = primary;
    }

    /// SECONDARY-INSTANCE-NOTICE: a second concurrent window cannot own session
    /// restore or autosave — a live primary holds the instance lock — and that
    /// suppression is otherwise silent, which reads as "restore didn't work"
    /// after relaunching over a still-running (or wedged) first window. When
    /// this instance is secondary and the user expects restore
    /// (`restore_workspaces` on), raise the one-line banner once at startup so
    /// the behavior is legible. Purely a notice: ownership is unchanged. A stale
    /// lock cannot reach here — the advisory instance lock is released the
    /// instant its owner exits or crashes, so a non-primary election always
    /// means a live peer still holds it (the same std lock API on every
    /// platform, so this is platform-agnostic).
    pub(in crate::native) fn notice_secondary_instance_if_suppressed(&mut self) {
        if self.autosave_is_primary || !self.settings.restore_workspaces {
            return;
        }
        self.raise_open_notice(SECONDARY_INSTANCE_NOTICE.to_owned());
    }

    /// Apply the current app-global presentation/model state — theme base
    /// colors, palette, cursor defaults, OSC 52 read gate and scrollback cap — to
    /// EVERY session's terminal. Live-created sessions receive this through
    /// [`Self::initialize_session_with`] right after spawn; sessions built by
    /// snapshot restore-on-launch or layout append never pass through that path,
    /// so without this sweep they keep the `DynamicColors::default()` palette and
    /// render every `Color::Default` / `Color::Indexed` surface with the wrong
    /// colors. That is most visible on context menus and overlays, which paint in
    /// the terminal palette (`panel_attrs` etc.): a restored workspace's menu drew
    /// in the default grey while a live workspace's drew in the theme palette, so
    /// the two diverged in one window even though every setting is app-global.
    /// All values here are app-global, so one arena sweep is consistent, and it
    /// is idempotent — re-applying to an already-seeded session is a no-op.
    pub(super) fn apply_model_state_to_all_sessions(&mut self) {
        // ID1: with themed UI roles on, the cursor default comes from the theme
        // `cursor` role; otherwise it stays the foreground (today's behavior). A
        // live OSC 12 override is a separate core mechanism and still wins.
        let cursor_default = if self.themed_ui_roles {
            rgb(self.effective_theme.cursor)
        } else {
            rgb(self.effective_theme.foreground)
        };
        let base_fg = rgb(self.effective_theme.foreground);
        let base_bg = rgb(self.effective_theme.background);
        // C29: OSC 4 replies report the theme palette, not the xterm table.
        let base_palette = self.effective_theme.palette.map(rgb);
        let osc52_read = self.settings.osc52_read;
        let kitty_named_transports = self.settings.kitty_named_transports;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink.enabled();
        let scrollback_limit = self.settings.scrollback_limit();
        let button_gates = self.button_gates();
        for session in self.sessions.iter() {
            if let Ok(mut terminal) = session.terminal.lock() {
                terminal.set_base_colors(base_fg, base_bg, cursor_default);
                terminal.set_base_palette(base_palette);
                terminal.set_osc52_read_enabled(osc52_read);
                terminal.set_kitty_named_transports_enabled(kitty_named_transports);
                terminal.set_cursor_defaults(cursor_style, cursor_blink);
                terminal.set_scrollback_limit(scrollback_limit);
                button_gates.apply(&mut terminal);
            }
        }
    }

    /// WP2 restore-on-launch (sub-ODPs 8a/8b/8f). Called once at startup, and
    /// only when this is the primary instance, the launch was a bare `odytty`,
    /// and `restore_workspaces` is on. Rebuilds the saved shape; on a stale
    /// directory it lands that pane at home with ONE compact notice, and on an
    /// unreadable / version-skewed snapshot it starts fresh with a notice.
    /// Never produces a broken or empty window — worst case is the launch
    /// layout that was already on screen.
    pub(in crate::native) fn restore_workspaces_on_launch(&mut self) {
        use crate::native::persistence::{self, LoadOutcome};
        use crate::native::session::RestoreReport;
        match persistence::load_snapshot() {
            LoadOutcome::Loaded(snapshot) => {
                let home = persistence::restore_home_dir();
                // RESTORE-REMOTE: reconnect remote panes through the ssh connect
                // path. The context (settings + saved hosts) is gathered once,
                // owned, so the spawn closure never borrows `self` while the
                // workspace set is borrowed mutably.
                let ctx = self.remote_restore_context();
                let grid = self.grid;
                let report = self.sessions.restore_from_snapshot_remote(
                    &snapshot,
                    grid,
                    home.as_deref(),
                    |set, identity| ctx.spawn(set, grid, identity),
                );
                if let RestoreReport::Restored {
                    stale_cwd,
                    reattached,
                    reattach_attempted,
                    remote_fallback,
                    ..
                } = report
                {
                    let mut extras: Vec<String> = Vec::new();
                    if reattach_attempted > 0 {
                        // 8h: one compact "N of M sessions reattached" line.
                        extras.push(format!(
                            "{reattached} of {reattach_attempted} sessions reattached"
                        ));
                    }
                    if remote_fallback > 0 {
                        extras.push(format!("{remote_fallback} opened locally"));
                    }
                    if stale_cwd > 0 {
                        extras.push("some panes opened at home".to_owned());
                    }
                    if !extras.is_empty() {
                        self.raise_open_notice(format!(
                            "Restored your layout \u{2014} {}.",
                            extras.join("; ")
                        ));
                    }
                }
            }
            // First launch or nothing ever saved: start fresh, no notice.
            LoadOutcome::Absent => {}
            // Unreadable or from a newer build: start fresh, one quiet notice.
            LoadOutcome::Skew { .. } | LoadOutcome::Corrupt(_) => {
                self.raise_open_notice(
                    "Couldn't read the saved layout \u{2014} starting fresh.".to_owned(),
                );
            }
        }
        // Seed the restored sessions with the current theme palette / cursor
        // defaults / scrollback cap. Restore spawns terminals inside the session
        // arena without routing them through `initialize_session_with`, so
        // without this they would render menus, overlays and terminal content in
        // the `DynamicColors::default()` palette instead of the theme's — a
        // per-workspace presentation divergence in one window. Idempotent for the
        // launch session on the no-restore arms.
        self.apply_model_state_to_all_sessions();
        // Establish the post-restore fingerprint baseline so the restored shape
        // does not itself trigger an immediate redundant autosave.
        self.autosave_fingerprint = Some(self.sessions.structural_fingerprint());
    }

    /// WP2 sub-ODP 8c: arm / flush the debounced shape autosave. Runs every
    /// maintenance pass; inert on non-primary instances and when the structure
    /// is unchanged. A structural mutation re-arms the debounce so a burst (e.g.
    /// a split-ratio drag) coalesces into one write when it settles.
    pub(super) fn run_shape_autosave(&mut self, now: Instant) {
        if !self.autosave_is_primary {
            return;
        }
        let fingerprint = self.sessions.structural_fingerprint();
        match self.autosave_fingerprint {
            // Establish the baseline on the first pass without scheduling a write.
            None => self.autosave_fingerprint = Some(fingerprint),
            Some(previous) if previous != fingerprint => {
                self.autosave_fingerprint = Some(fingerprint);
                self.autosave_deadline = Some(now + SHAPE_AUTOSAVE_DEBOUNCE);
            }
            Some(_) => {}
        }
        if let Some(deadline) = self.autosave_deadline
            && now >= deadline
        {
            self.autosave_deadline = None;
            self.write_shape_snapshot();
        }
    }

    /// WP2 sub-ODP 8c: unconditional shape save on a clean exit (primary only).
    /// Skips an empty window so quitting after closing every tab cannot clobber a
    /// good snapshot with nothing.
    pub(in crate::native) fn save_shape_on_exit(&mut self) {
        if !self.autosave_is_primary || self.sessions.is_empty() {
            return;
        }
        self.write_shape_snapshot();
    }

    /// Capture the live workspace shape and persist it atomically (sub-ODP 8c).
    /// Best-effort: a write error is logged, never fatal. Under `cfg(test)` the
    /// disk write is replaced by a counter bump so the debounce-coalescing tests
    /// can assert exactly-once behavior without touching the filesystem.
    pub(super) fn write_shape_snapshot(&mut self) {
        #[cfg(test)]
        {
            self.autosave_saves += 1;
        }
        #[cfg(not(test))]
        {
            let snapshot = self.sessions.capture_shape();
            if let Err(err) = crate::native::persistence::save_snapshot(&snapshot) {
                tracing::warn!("workspace shape autosave failed: {err}");
            }
        }
    }
}

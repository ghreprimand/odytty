// SPDX-License-Identifier: GPL-3.0-only
//! Direct menu routes into the named-profile picker (v0.14 Phase A3).

use super::*;
use crate::native::profile_picker::ProfilePickerPurpose;

impl App {
    pub(super) fn open_profile_picker_for_new_tab(&mut self) {
        self.reset_pointer_state_for_overlay();
        let catalog = super::profile_launch::load_profile_catalog();
        let entries = super::profile_launch::profile_picker_entries(&catalog);
        self.overlay
            .open_profile_picker(entries, ProfilePickerPurpose::NewTab);
        self.request_selection_redraw();
    }

    pub(super) fn open_profile_picker_for_new_workspace(&mut self) {
        self.reset_pointer_state_for_overlay();
        let catalog = super::profile_launch::load_profile_catalog();
        let entries = super::profile_launch::profile_picker_entries(&catalog);
        self.overlay
            .open_profile_picker(entries, ProfilePickerPurpose::NewWorkspace);
        self.request_selection_redraw();
    }

    pub(super) fn handle_new_workspace_with_profile(&mut self, profile_name: &str) {
        self.finish_divider_drag();
        let cwd = self.validated_spawn_cwd();
        let effective = super::profile_launch::resolve_for_new_local_tab(
            &self.settings,
            Some(profile_name),
            cwd,
            None,
        );
        if let Some(alias) = effective.connection.clone() {
            self.open_profile_connection_in_new_workspace(profile_name, &alias);
            for warning in effective.warnings {
                tracing::warn!(warning = %warning, "profile launch notice");
            }
            return;
        }
        match self
            .sessions
            .new_workspace_with_effective(self.grid, profile_name, &effective)
        {
            Ok(token) => self.finish_new_workspace_with_effective(token, &effective),
            Err(error) => {
                if self.open_notice.is_none() {
                    self.raise_open_notice(format!(
                        "Could not create a workspace with profile \"{profile_name}\": {error}"
                    ));
                }
            }
        }
    }

    fn open_profile_connection_in_new_workspace(&mut self, profile_name: &str, alias: &str) {
        let host = self
            .load_connection_entries()
            .into_iter()
            .find(|entry| entry.alias == alias);
        let Some(host) = host else {
            self.raise_open_notice(format!(
                "Host \"{alias}\" is no longer configured; opened a plain workspace"
            ));
            self.handle_new_workspace_plain();
            return;
        };
        // Plain placeholder workspace: the explicit profile choice must not be
        // layered on top of the global default (which could itself be SSH).
        self.handle_new_workspace_plain();
        let placeholder = self.sessions.active_id();
        self.sessions
            .set_active_workspace_launch_profile(Some(profile_name.to_owned()));
        if self.connect_or_notice(&host).is_some() {
            self.close_tab_by_token(placeholder);
            self.on_active_session_changed();
        }
    }

    pub(super) fn finish_new_workspace_with_effective(
        &mut self,
        token: SessionToken,
        effective: &crate::profiles::EffectiveLaunch,
    ) {
        let session_theme = crate::native::cvd_theme::effective_theme(
            &effective.settings.theme,
            effective.settings.cvd_mode,
            effective.settings.cvd_strength,
        );
        let themed_ui_roles = effective.settings.themed_ui_roles;
        let osc52_read = effective.settings.osc52_read;
        let kitty_named_transports = effective.settings.kitty_named_transports;
        let cursor_style = effective.settings.cursor_style;
        let cursor_blink = effective.settings.cursor_blink;
        let scrollback_limit = effective.settings.scrollback_limit();
        let button_gates = self.button_gates();
        let cell = self.gpu.as_ref().map(GpuState::cell);
        if let Some(session) = self.sessions.get_mut(token) {
            Self::initialize_session_with(
                session,
                session_theme,
                themed_ui_roles,
                osc52_read,
                kitty_named_transports,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
                button_gates,
            );
        }
        self.flash_rail_autohide();
        self.recompute_grid_for_tab_bar();
        self.on_active_session_changed();
        for warning in &effective.warnings {
            tracing::warn!(warning = %warning, "profile launch notice");
        }
    }
}

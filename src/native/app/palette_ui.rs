// SPDX-License-Identifier: GPL-3.0-only
//! App-side command-palette integration.
//!
//! The overlay owns query/display state. This module owns the side effects that
//! happen only after the overlay accepts and closes: typing literal text into
//! the focused pane or dispatching an existing local action.

use super::*;
use crate::native::palette_overlay::{
    LAYOUT_SAVE_ALL_ID, LAYOUT_SAVE_ID, WORKSPACE_NEW_ID, WORKSPACE_NEW_LOCAL_TAB_ID,
    WORKSPACE_RENAME_ID, WORKSPACE_UNBIND_ID, WorkspacePaletteContext, parse_layout_delete_id,
    parse_layout_open_id, parse_workspace_bind_id, parse_workspace_switch_id,
};
use crate::palette_catalog::PaletteAction;
use crate::settings::BindableAction;

impl App {
    pub(super) fn open_command_palette_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        let cwd = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.current_working_directory().map(str::to_owned));
        self.reset_pointer_state_for_overlay();
        let workspaces = self.sessions.workspace_names();
        // F6-W5: offer the known-host aliases for binding, and the current
        // binding so the palette can show the unbind + New Local Tab escape rows.
        let host_aliases: Vec<String> = self
            .load_connection_entries()
            .into_iter()
            .map(|host| host.alias)
            .collect();
        let bound_profile = self
            .sessions
            .active_workspace_default_profile()
            .map(str::to_owned);
        let layout_names = crate::native::persistence::list_layout_names();
        let context = WorkspacePaletteContext {
            names: &workspaces,
            host_aliases: &host_aliases,
            bound_profile: bound_profile.as_deref(),
            layout_names: &layout_names,
        };
        self.overlay.open_command_palette(cwd.as_deref(), &context);
        self.request_selection_redraw();
    }

    pub(super) fn handle_palette_type_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.return_to_live();
        self.write_pty_bytes(text.as_bytes());
    }

    pub(super) fn handle_palette_action(&mut self, id: String) {
        // Workspace rows (ODP-5) carry dynamic ids the pure `PaletteAction`
        // catalog does not model: `workspace-switch-<idx>` plus the create /
        // rename rows. Route those first, then fall through to the static
        // catalog for everything else.
        if let Some(idx) = parse_workspace_switch_id(&id) {
            self.switch_to_workspace(idx);
            return;
        }
        if id == WORKSPACE_NEW_ID {
            self.new_workspace_from_palette();
            return;
        }
        if id == WORKSPACE_RENAME_ID {
            self.enter_rename_workspace(self.sessions.active_workspace_index());
            return;
        }
        if let Some(idx) = parse_workspace_bind_id(&id) {
            self.bind_active_workspace_to_host_index(idx);
            return;
        }
        if id == WORKSPACE_UNBIND_ID {
            self.unbind_active_workspace();
            return;
        }
        if id == WORKSPACE_NEW_LOCAL_TAB_ID {
            self.handle_new_local_tab();
            return;
        }
        if id == LAYOUT_SAVE_ALL_ID {
            self.save_all_workspaces_as_layout(None);
            return;
        }
        if id == LAYOUT_SAVE_ID {
            self.save_active_workspace_as_layout();
            return;
        }
        if let Some(idx) = parse_layout_open_id(&id) {
            if let Some(name) = crate::native::persistence::list_layout_names()
                .into_iter()
                .nth(idx)
            {
                self.open_layout(&name);
            }
            return;
        }
        if let Some(idx) = parse_layout_delete_id(&id) {
            if let Some(name) = crate::native::persistence::list_layout_names()
                .into_iter()
                .nth(idx)
            {
                self.delete_layout(&name);
            }
            return;
        }
        let Some(action) = PaletteAction::from_id(&id) else {
            return;
        };
        match action {
            PaletteAction::Search => self.toggle_search(),
            PaletteAction::OpenSettings => self.toggle_settings_overlay(),
            PaletteAction::OpenThemePicker => self.open_theme_picker_overlay(),
            PaletteAction::CreateThemeFromColors => self.open_theme_capture_overlay(),
            PaletteAction::CopySelection => self.handle_copy_shortcut(),
            PaletteAction::Paste => self.handle_paste_shortcut(),
            PaletteAction::ScrollPageUp => self.scroll_viewport(self.page_lines() as isize),
            PaletteAction::ScrollPageDown => self.scroll_viewport(-(self.page_lines() as isize)),
            PaletteAction::JumpPromptPrev => {
                let _ = self.jump_prompt_prev();
            }
            PaletteAction::JumpPromptNext => {
                let _ = self.jump_prompt_next();
            }
            PaletteAction::CopyMode => {
                let _ = self.enter_copy_mode();
            }
            PaletteAction::Hints => {
                let _ = self.activate_hints();
            }
            PaletteAction::ClearInput => {
                self.return_to_live();
                self.write_pty_bytes(&[0x01, 0x0b]);
            }
            PaletteAction::NewTab => self.handle_new_tab(),
            PaletteAction::CloseTab => {
                let _ = self.close_active_tab();
            }
            PaletteAction::NextTab => self.switch_to_next_tab(),
            PaletteAction::PrevTab => self.switch_to_prev_tab(),
            PaletteAction::RenameTab => self.enter_rename_tab(self.sessions.active_id()),
            PaletteAction::SplitPaneColumns => self.apply_pane_action(BindableAction::SplitColumns),
            PaletteAction::SplitPaneRows => self.apply_pane_action(BindableAction::SplitRows),
            PaletteAction::FocusPaneLeft => self.apply_pane_action(BindableAction::FocusPaneLeft),
            PaletteAction::FocusPaneRight => self.apply_pane_action(BindableAction::FocusPaneRight),
            PaletteAction::FocusPaneUp => self.apply_pane_action(BindableAction::FocusPaneUp),
            PaletteAction::FocusPaneDown => self.apply_pane_action(BindableAction::FocusPaneDown),
            PaletteAction::FocusPaneNext => self.apply_pane_action(BindableAction::FocusPaneNext),
            PaletteAction::ClosePane => self.apply_pane_action(BindableAction::ClosePane),
            PaletteAction::ZoomPane => self.apply_pane_action(BindableAction::ZoomPane),
            PaletteAction::EqualizePanes => self.apply_pane_action(BindableAction::EqualizePanes),
        }
    }
}

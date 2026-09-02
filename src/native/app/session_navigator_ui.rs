// SPDX-License-Identifier: GPL-3.0-only
//! App-side opening and focus dispatch for the unified session navigator.

use super::*;
use crate::native::session::SessionToken;
use crate::native::session_navigator::{
    ClosedNavigatorItem, ClosedNavigatorKind, MAX_RECENTLY_CLOSED, NavigatorAction,
    NavigatorTarget, append_detached, live_entries,
};

impl App {
    /// Snapshot the existing workspace arena and, on Unix, the existing
    /// detached-session registry.  Opening performs no attach, restore, or
    /// terminal read beyond bounded directory metadata.
    pub(super) fn open_session_navigator_overlay(&mut self) {
        self.open_session_navigator_overlay_selected(None);
    }

    pub(super) fn open_session_navigator_overlay_selected(
        &mut self,
        selected: Option<NavigatorTarget>,
    ) {
        if self.search.is_open() {
            self.close_search(true);
        }
        let mut entries = live_entries(&self.sessions, self.settings.navigator_preview);
        #[cfg(unix)]
        append_detached(
            &mut entries,
            crate::session_host::list_live_sessions(None).unwrap_or_default(),
        );
        #[cfg(not(unix))]
        let _ = &mut entries;
        self.reset_pointer_state_for_overlay();
        let selected = selected.map(|target| target.stable_id());
        self.overlay
            .open_session_navigator_selected(entries, selected.as_deref());
        self.request_selection_redraw();
    }

    /// Focus through the arena's existing stable-token switch seam, never a
    /// picker row index.  A stale snapshot is a harmless no-op.
    pub(super) fn focus_session_from_navigator(&mut self, token: SessionToken) {
        self.finish_divider_drag();
        if self.sessions.switch(token) {
            self.flash_rail_autohide();
            if self.sessions.active_is_single_pane() {
                self.prefix_engine.cancel();
            }
            self.on_active_session_changed();
        }
    }

    pub(super) fn run_navigator_action(&mut self, action: NavigatorAction) {
        match action {
            NavigatorAction::Rename(NavigatorTarget::Workspace(token)) => {
                if let Some((workspace, _)) = self.sessions.locate_token(token) {
                    self.enter_rename_workspace(workspace);
                }
            }
            NavigatorAction::Rename(NavigatorTarget::Tab(token))
            | NavigatorAction::Rename(NavigatorTarget::Live(token)) => {
                self.focus_session_from_navigator(token);
                self.enter_rename_tab(token);
            }
            NavigatorAction::Duplicate(NavigatorTarget::Workspace(token)) => {
                self.focus_session_from_navigator(token);
                self.handle_duplicate_workspace();
            }
            NavigatorAction::Duplicate(NavigatorTarget::Tab(token))
            | NavigatorAction::Duplicate(NavigatorTarget::Live(token)) => {
                self.focus_session_from_navigator(token);
                self.handle_new_local_tab();
            }
            NavigatorAction::Move(NavigatorTarget::Workspace(token)) => {
                if let Some((workspace, _)) = self.sessions.locate_token(token) {
                    self.move_workspace_at(workspace, false);
                }
            }
            NavigatorAction::Move(NavigatorTarget::Tab(token))
            | NavigatorAction::Move(NavigatorTarget::Live(token)) => {
                self.open_move_tab_workspace_picker(token);
            }
            NavigatorAction::Close(_) => {}
            NavigatorAction::Reopen => self.reopen_last_closed_navigator_item(),
            NavigatorAction::Rename(NavigatorTarget::Detached(_))
            | NavigatorAction::Duplicate(NavigatorTarget::Detached(_))
            | NavigatorAction::Move(NavigatorTarget::Detached(_)) => {}
        }
    }

    /// Confirmation has already made this an intentional close. Resolve the
    /// stable target through the workspace arena at execution time; a stale row
    /// cannot close a replacement tab or workspace.
    pub(super) fn close_navigator_target(&mut self, target: NavigatorTarget) {
        match target {
            NavigatorTarget::Workspace(token) => {
                if let Some((workspace, _)) = self.sessions.locate_token(token) {
                    self.close_workspace_at(workspace);
                }
            }
            NavigatorTarget::Tab(token) | NavigatorTarget::Live(token) => {
                self.focus_session_from_navigator(token);
                let _ = self.close_active_tab();
            }
            NavigatorTarget::Detached(_) => {}
        }
    }

    /// Capture one closed tab's restartable launch context. This never retains a
    /// PTY or terminal snapshot: reopen is explicitly a new shell, not process
    /// resurrection. `workspace_id` is descriptive only and never dereferenced.
    pub(super) fn record_navigator_closed_tab(&mut self, token: SessionToken) {
        let Some((workspace_id, tab_id)) = self.sessions.locate_token(token) else {
            return;
        };
        let workspace = &self.sessions.workspaces[workspace_id];
        let tab = &workspace.tabs[tab_id];
        let session = match self.sessions.get(tab.focused) {
            Some(session) => session,
            None => return,
        };
        let cwd = session
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.current_working_directory().map(str::to_owned));
        let title = tab
            .title_override
            .clone()
            .unwrap_or_else(|| session.tab_title.clone());
        self.push_navigator_recently_closed(ClosedNavigatorItem {
            kind: ClosedNavigatorKind::Tab,
            title,
            cwd,
            profile: session
                .launch_profile
                .clone()
                .or_else(|| workspace.launch_profile.clone()),
            workspace_id: Some(workspace_id),
        });
    }

    /// A workspace descriptor captures its active tab's launch context. Closing
    /// a workspace remains one reopenable item: reopen creates one fresh shell,
    /// never attempts to reconstruct all processes that were closed.
    pub(super) fn record_navigator_closed_workspace(&mut self, workspace_id: usize) {
        let Some(workspace) = self.sessions.workspaces.get(workspace_id) else {
            return;
        };
        let Some(tab) = workspace.tabs.get(workspace.active_tab) else {
            return;
        };
        let Some(session) = self.sessions.get(tab.focused) else {
            return;
        };
        let cwd = session
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.current_working_directory().map(str::to_owned));
        self.push_navigator_recently_closed(ClosedNavigatorItem {
            kind: ClosedNavigatorKind::Workspace,
            title: workspace.name.clone(),
            cwd,
            profile: session
                .launch_profile
                .clone()
                .or_else(|| workspace.launch_profile.clone()),
            workspace_id: None,
        });
    }

    fn push_navigator_recently_closed(&mut self, item: ClosedNavigatorItem) {
        if self.navigator_recently_closed.len() == MAX_RECENTLY_CLOSED {
            let _ = self.navigator_recently_closed.pop_front();
        }
        self.navigator_recently_closed.push_back(item);
    }

    /// Reopen is intentionally a relaunch: the process-lifetime ring supplies
    /// cwd/profile/title to existing profile-launch seams and no closed PTY is
    /// stored or revived.
    pub(super) fn reopen_last_closed_navigator_item(&mut self) {
        let Some(item) = self.navigator_recently_closed.pop_back() else {
            self.open_session_navigator_overlay();
            return;
        };
        let effective = super::profile_launch::resolve_for_new_local_tab(
            &self.settings,
            None,
            item.cwd.clone().map(std::path::PathBuf::from),
            item.profile.as_deref(),
        );
        match item.kind {
            ClosedNavigatorKind::Tab => {
                self.spawn_local_tab_from_effective(effective);
                self.restore_navigator_title(self.sessions.active_id(), item.title);
            }
            ClosedNavigatorKind::Workspace => match self
                .sessions
                .new_workspace_from_effective(self.grid, &effective)
            {
                Ok(token) => {
                    self.finish_new_workspace_with_effective(token, &effective);
                    if let Some(workspace) = self.sessions.workspaces.last_mut() {
                        workspace.name = item.title;
                    }
                }
                Err(error) => {
                    if self.open_notice.is_none() {
                        self.raise_open_notice(format!("Could not reopen the workspace: {error}"));
                    }
                }
            },
        }
        self.open_session_navigator_overlay();
    }

    fn restore_navigator_title(&mut self, token: SessionToken, title: String) {
        if let Some((workspace, tab)) = self.sessions.locate_token(token)
            && let Some(tab) = self
                .sessions
                .workspaces
                .get_mut(workspace)
                .and_then(|workspace| workspace.tabs.get_mut(tab))
        {
            tab.title_override = Some(title);
        }
    }
}

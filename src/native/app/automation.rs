// SPDX-License-Identifier: GPL-3.0-only
//! Structural-control projections and actions over the existing App model.
//!
//! Identities are the process window id plus the creation identities already
//! stored on workspaces, tabs, and panes. No automation registry owns or mirrors
//! live state.

use super::*;
use crate::automation::protocol::{ErrorCode, ObjectId, ObjectKind, ObjectStatus, SplitDirection};

impl App {
    pub(in crate::native) fn automation_endpoint_enabled(&self) -> bool {
        self.settings.automation_endpoint
    }

    pub(in crate::native) fn quick_terminal_enabled(&self) -> bool {
        self.settings.quick_terminal
    }

    pub(in crate::native) fn automation_notice(&mut self, message: String, failure: bool) {
        if failure {
            self.raise_open_notice(message);
        } else {
            self.raise_neutral_notice(message);
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn automation_objects(
        &self,
        instance: [u8; 16],
        hidden: bool,
    ) -> Vec<ObjectStatus> {
        self.automation_objects_bounded(instance, hidden, usize::MAX)
    }

    pub(in crate::native) fn automation_objects_bounded(
        &self,
        instance: [u8; 16],
        hidden: bool,
        max: usize,
    ) -> Vec<ObjectStatus> {
        let mut objects = Vec::new();
        if max == 0 {
            return objects;
        }
        let window_id = object_id(instance, ObjectKind::Window, self.process_window_id.0);
        let window_focused = self.focused && !hidden;
        objects.push(ObjectStatus {
            id: window_id,
            parent: None,
            focused: window_focused,
            hidden,
        });

        for (workspace_index, workspace) in self.sessions.workspaces.iter().enumerate() {
            if objects.len() == max {
                return objects;
            }
            let workspace_id = object_id(instance, ObjectKind::Workspace, workspace.identity.0);
            let workspace_focused =
                window_focused && workspace_index == self.sessions.active_workspace_index();
            objects.push(ObjectStatus {
                id: workspace_id,
                parent: Some(window_id),
                focused: workspace_focused,
                hidden,
            });
            for (tab_index, tab) in workspace.tabs.iter().enumerate() {
                if objects.len() == max {
                    return objects;
                }
                let tab_id = object_id(instance, ObjectKind::Tab, tab.identity.0);
                let tab_focused = workspace_focused && tab_index == workspace.active_tab;
                objects.push(ObjectStatus {
                    id: tab_id,
                    parent: Some(workspace_id),
                    focused: tab_focused,
                    hidden,
                });
                for pane in tab.layout.leaves() {
                    if objects.len() == max {
                        return objects;
                    }
                    objects.push(ObjectStatus {
                        id: object_id(instance, ObjectKind::Pane, pane.0),
                        parent: Some(tab_id),
                        focused: tab_focused && pane == tab.focused,
                        hidden,
                    });
                }
            }
        }
        objects
    }

    /// Resolve one object directly instead of projecting the whole tree.
    pub(in crate::native) fn automation_status(
        &self,
        instance: [u8; 16],
        hidden: bool,
        kind: ObjectKind,
        serial: u64,
    ) -> Option<ObjectStatus> {
        let window_id = object_id(instance, ObjectKind::Window, self.process_window_id.0);
        let window_focused = self.focused && !hidden;
        let status = |id, parent, focused| ObjectStatus {
            id,
            parent,
            focused,
            hidden,
        };
        match kind {
            ObjectKind::Window => (self.process_window_id.0 == serial)
                .then(|| status(window_id, None, window_focused)),
            ObjectKind::Workspace => {
                let index = self
                    .sessions
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.identity.0 == serial)?;
                let focused = window_focused && index == self.sessions.active_workspace_index();
                Some(status(
                    object_id(instance, kind, serial),
                    Some(window_id),
                    focused,
                ))
            }
            ObjectKind::Tab => {
                let (workspace_index, workspace) = self
                    .sessions
                    .workspaces
                    .iter()
                    .enumerate()
                    .find(|(_, workspace)| {
                        workspace.tabs.iter().any(|tab| tab.identity.0 == serial)
                    })?;
                let tab_index = workspace
                    .tabs
                    .iter()
                    .position(|tab| tab.identity.0 == serial)?;
                let workspace_focused =
                    window_focused && workspace_index == self.sessions.active_workspace_index();
                Some(status(
                    object_id(instance, kind, serial),
                    Some(object_id(
                        instance,
                        ObjectKind::Workspace,
                        workspace.identity.0,
                    )),
                    workspace_focused && tab_index == workspace.active_tab,
                ))
            }
            ObjectKind::Pane => {
                let token = SessionToken(serial);
                if !self.sessions.owns_session(token) {
                    return None;
                }
                let (workspace_index, workspace) = self
                    .sessions
                    .workspaces
                    .iter()
                    .enumerate()
                    .find(|(_, workspace)| {
                        workspace
                            .tabs
                            .iter()
                            .any(|tab| tab.layout.leaves().contains(&token))
                    })?;
                let (tab_index, tab) = workspace
                    .tabs
                    .iter()
                    .enumerate()
                    .find(|(_, tab)| tab.layout.leaves().contains(&token))?;
                let tab_focused = window_focused
                    && workspace_index == self.sessions.active_workspace_index()
                    && tab_index == workspace.active_tab;
                Some(status(
                    object_id(instance, kind, serial),
                    Some(object_id(instance, ObjectKind::Tab, tab.identity.0)),
                    tab_focused && tab.focused == token,
                ))
            }
        }
    }

    /// Whether an open overlay, search, keyboard modal (copy mode, hint
    /// selection, rename), or OSC 52 write confirmation owns this window's
    /// interaction. Structural automation and quick-terminal focus-loss hiding
    /// both honor this shared gate so neither switches state underneath a live
    /// interaction. The confirmation is checked here because it consumes every
    /// key before the overlay and modal ladder runs.
    pub(in crate::native) fn interaction_busy(&self) -> bool {
        self.overlay.is_open()
            || self.search.is_open()
            || self.active_modal() != ActiveModal::None
            || self.osc52_write.prompt_pending()
    }

    /// Pointer-owned divider state must settle against its original layout
    /// before any structural mutation, exactly as the keyboard ladder settles
    /// it before routing a key. Called at the automation mutation boundary.
    pub(in crate::native) fn settle_for_automation_mutation(&mut self) {
        self.finish_divider_drag();
    }

    pub(in crate::native) fn automation_focus(&mut self, kind: ObjectKind, serial: u64) -> bool {
        let token = match kind {
            ObjectKind::Window => {
                if self.process_window_id.0 != serial {
                    return false;
                }
                None
            }
            ObjectKind::Workspace => self
                .sessions
                .workspaces
                .iter()
                .find(|workspace| workspace.identity.0 == serial)
                .and_then(|workspace| workspace.tabs.get(workspace.active_tab))
                .map(|tab| tab.focused),
            ObjectKind::Tab => self
                .sessions
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .find(|tab| tab.identity.0 == serial)
                .map(|tab| tab.focused),
            ObjectKind::Pane => self
                .sessions
                .owns_session(SessionToken(serial))
                .then_some(SessionToken(serial)),
        };
        if kind != ObjectKind::Window && token.is_none() {
            return false;
        }
        if let Some(token) = token
            && self.sessions.switch(token)
        {
            self.on_active_session_changed();
        }
        if let Some(window) = self.window.as_ref() {
            window.focus_window();
            window.request_redraw();
        }
        true
    }

    pub(in crate::native) fn automation_owns(&self, kind: ObjectKind, serial: u64) -> bool {
        match kind {
            ObjectKind::Window => self.process_window_id.0 == serial,
            ObjectKind::Workspace => self
                .sessions
                .workspaces
                .iter()
                .any(|workspace| workspace.identity.0 == serial),
            ObjectKind::Tab => self
                .sessions
                .workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .any(|tab| tab.identity.0 == serial),
            ObjectKind::Pane => self.sessions.owns_session(SessionToken(serial)),
        }
    }

    pub(in crate::native) fn automation_create_tab(&mut self) -> Result<u64, ErrorCode> {
        let before = self.active_tab_serial();
        self.handle_new_tab();
        self.new_active_tab_serial(before)
    }

    pub(in crate::native) fn automation_open_profile(
        &mut self,
        name: &str,
    ) -> Result<u64, ErrorCode> {
        let name = trimmed_name(name)?;
        let catalog = super::profile_launch::load_profile_catalog();
        if !catalog
            .profiles
            .get(name)
            .is_some_and(|profile| profile.applies_on_current_platform())
        {
            return Err(ErrorCode::InvalidRequest);
        }
        let before = self.active_tab_serial();
        self.handle_new_tab_with_profile(name);
        self.new_active_tab_serial(before)
    }

    pub(in crate::native) fn automation_create_workspace(
        &mut self,
        name: &str,
    ) -> Result<u64, ErrorCode> {
        let name = trimmed_name(name)?.to_owned();
        let before = self.active_workspace_serial();
        self.handle_new_workspace();
        let index = self.sessions.active_workspace_index();
        let serial = self.sessions.workspaces[index].identity.0;
        if Some(serial) == before {
            return Err(ErrorCode::Unavailable);
        }
        self.sessions.rename_workspace(index, name);
        self.request_selection_redraw();
        Ok(serial)
    }

    pub(in crate::native) fn automation_split(
        &mut self,
        pane: u64,
        direction: SplitDirection,
    ) -> Result<u64, ErrorCode> {
        if !self.automation_focus(ObjectKind::Pane, pane) {
            return Err(ErrorCode::StaleIdentity);
        }
        let before = self.sessions.active_id().0;
        let axis = match direction {
            SplitDirection::Columns => SplitAxis::Columns,
            SplitDirection::Rows => SplitAxis::Rows,
        };
        self.split_active_pane(axis);
        let serial = self.sessions.active_id().0;
        (serial != before)
            .then_some(serial)
            .ok_or(ErrorCode::Unavailable)
    }

    pub(in crate::native) fn automation_rename(
        &mut self,
        kind: ObjectKind,
        serial: u64,
        name: &str,
    ) -> Result<(), ErrorCode> {
        let name = trimmed_name(name)?.to_owned();
        match kind {
            ObjectKind::Workspace => {
                let index = self
                    .sessions
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.identity.0 == serial)
                    .ok_or(ErrorCode::StaleIdentity)?;
                self.sessions.rename_workspace(index, name);
            }
            ObjectKind::Tab => {
                let token = self
                    .sessions
                    .workspaces
                    .iter()
                    .flat_map(|workspace| &workspace.tabs)
                    .find(|tab| tab.identity.0 == serial)
                    .map(|tab| tab.focused)
                    .ok_or(ErrorCode::StaleIdentity)?;
                self.sessions.set_title_override(token, Some(name));
            }
            ObjectKind::Window | ObjectKind::Pane => {
                return Err(ErrorCode::UnsupportedCapability);
            }
        }
        self.request_selection_redraw();
        Ok(())
    }

    // Creation routes activate what they create, so the prior active identity
    // is the only comparison needed; snapshotting every identity would let a
    // hostile client force repeated whole-tree allocations on the UI thread.
    fn active_tab_serial(&self) -> Option<u64> {
        let workspace = self
            .sessions
            .workspaces
            .get(self.sessions.active_workspace_index())?;
        workspace
            .tabs
            .get(workspace.active_tab)
            .map(|tab| tab.identity.0)
    }

    fn active_workspace_serial(&self) -> Option<u64> {
        self.sessions
            .workspaces
            .get(self.sessions.active_workspace_index())
            .map(|workspace| workspace.identity.0)
    }

    fn new_active_tab_serial(&self, before: Option<u64>) -> Result<u64, ErrorCode> {
        let serial = self.active_tab_serial().ok_or(ErrorCode::Unavailable)?;
        (Some(serial) != before)
            .then_some(serial)
            .ok_or(ErrorCode::Unavailable)
    }
}

fn object_id(instance: [u8; 16], kind: ObjectKind, serial: u64) -> ObjectId {
    ObjectId {
        instance,
        kind,
        serial,
    }
}

fn trimmed_name(name: &str) -> Result<&str, ErrorCode> {
    let name = name.trim();
    (!name.is_empty())
        .then_some(name)
        .ok_or(ErrorCode::InvalidRequest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::test_support::headless_app_for_test;

    #[test]
    fn projection_uses_existing_creation_identities_and_omits_text() {
        let (app, _) = headless_app_for_test();
        let instance = [7; 16];
        let objects = app.automation_objects(instance, false);
        assert_eq!(objects.len(), 4, "window + workspace + tab + pane");
        assert_eq!(objects[0].id.kind, ObjectKind::Window);
        assert_eq!(objects[1].id.kind, ObjectKind::Workspace);
        assert_eq!(objects[2].id.kind, ObjectKind::Tab);
        assert_eq!(objects[3].id.kind, ObjectKind::Pane);
        assert!(objects.iter().all(|object| object.id.instance == instance));
        assert_eq!(objects[1].parent, Some(objects[0].id));
        assert_eq!(objects[2].parent, Some(objects[1].id));
        assert_eq!(objects[3].parent, Some(objects[2].id));
    }

    #[test]
    fn hidden_window_marks_every_descendant_hidden_and_unfocused() {
        let (app, _) = headless_app_for_test();
        let objects = app.automation_objects([3; 16], true);
        assert!(objects.iter().all(|object| object.hidden));
        assert!(objects.iter().all(|object| !object.focused));
    }

    #[test]
    fn bounded_projection_stops_before_building_an_oversized_reply() {
        let (app, _) = headless_app_for_test();
        let objects = app.automation_objects_bounded([4; 16], false, 3);
        assert_eq!(objects.len(), 3);
        assert_eq!(objects[2].id.kind, ObjectKind::Tab);
    }

    #[test]
    fn rename_routes_only_to_existing_workspace_and_tab_names() {
        let (mut app, _) = headless_app_for_test();
        let workspace = app.sessions.workspaces[0].identity.0;
        let tab = app.sessions.workspaces[0].tabs[0].identity.0;
        let pane = app.sessions.active_id().0;
        assert_eq!(
            app.automation_rename(ObjectKind::Workspace, workspace, "ops"),
            Ok(())
        );
        assert_eq!(app.sessions.workspace_name(0), Some("ops"));
        assert_eq!(app.automation_rename(ObjectKind::Tab, tab, "build"), Ok(()));
        assert_eq!(
            app.sessions.workspaces[0].tabs[0].title_override.as_deref(),
            Some("build")
        );
        assert_eq!(
            app.automation_rename(ObjectKind::Pane, pane, "nope"),
            Err(ErrorCode::UnsupportedCapability)
        );
        assert_eq!(
            app.automation_rename(ObjectKind::Workspace, u64::MAX, "stale"),
            Err(ErrorCode::StaleIdentity)
        );
    }
}

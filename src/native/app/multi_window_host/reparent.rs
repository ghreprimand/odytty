// SPDX-License-Identifier: GPL-3.0-only
//! Moving a tab or a pane between windows of this process: the owner side.
//!
//! Two routes, both keyboard and palette driven, on Linux (Wayland and X11),
//! macOS, and Windows:
//!
//! - **To another window.** The merge picker opens with a move direction and
//!   the candidates paint "Press N to move here". The selected window takes
//!   the active tab (or pane, as a new tab) of the window that opened the
//!   picker. A source window that the move empties is retired like a merged
//!   window, and when it was the primary its shape-autosave role moves to the
//!   destination first.
//! - **To a new window.** A new window is built around the moved content with
//!   no shell spawn, from its own disjoint token range, and its surface is
//!   created before it joins the window list. If the surface cannot be
//!   created, the content goes back to its source exactly as it was and a
//!   notice says so. Moving a window's only tab to a new window is refused:
//!   it would only recreate the window.
//!
//! The quick terminal is never a source or a destination. Drag tear-out is
//! not part of this path; Wayland cannot position a new surface or report
//! drops, so the palette is the supported route there.

use super::*;
use crate::native::app::reparent::{MOVE_REFUSED_NOTICE, MoveRequest};
use crate::native::session::{MoveScope, WorkspaceSet};

/// Builds a window around an adopted session set, with no shell spawn.
pub(in crate::native) type AdoptFactory = Box<dyn FnMut(WorkspaceSet) -> App>;

/// Notice for a move the quick terminal cannot take part in.
pub(in crate::native) const QUICK_MOVE_NOTICE: &str =
    "The quick terminal cannot move tabs or panes between windows";

/// Notice for moving a window's only content to a new window.
pub(in crate::native) const ONLY_CONTENT_NOTICE: &str =
    "This is the window's only tab; it already has a window of its own";

impl MultiWindowHost {
    /// Tell every window which host sessions its siblings show, so attach
    /// dedup spans the whole process (a moved attached pane must not be
    /// attached a second time from the window it left).
    pub(super) fn sync_peer_attached_sessions(&mut self) {
        if self.windows.len() < 2 {
            if let Some(only) = self.windows.first_mut()
                && !only.peer_attached_sessions.is_empty()
            {
                only.set_peer_attached_sessions(Vec::new());
            }
            return;
        }
        let per_window: Vec<Vec<String>> =
            self.windows.iter().map(App::attached_session_ids).collect();
        for (idx, app) in self.windows.iter_mut().enumerate() {
            let peers = per_window
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != idx)
                .flat_map(|(_, ids)| ids.iter().cloned())
                .collect();
            app.set_peer_attached_sessions(peers);
        }
    }

    fn is_quick_window(&self, id: ProcessWindowId) -> bool {
        self.quick.identity().map(|identity| identity.window()) == Some(id)
    }

    /// Move the active tab or pane of `origin` into `target` (picker route).
    /// A refusal at any step leaves both windows as they were.
    pub(super) fn execute_move(
        &mut self,
        origin: ProcessWindowId,
        target: ProcessWindowId,
        scope: MoveScope,
    ) {
        if self.is_quick_window(origin) || self.is_quick_window(target) {
            if let Some(idx) = self.index_of(origin) {
                self.windows[idx].raise_open_notice(QUICK_MOVE_NOTICE.to_owned());
            }
            return;
        }
        let (Some(source_idx), Some(target_idx)) = (self.index_of(origin), self.index_of(target))
        else {
            return;
        };
        let Some((destination, source)) = two_mut(&mut self.windows, target_idx, source_idx) else {
            return;
        };
        if let Err(err) = destination
            .workspace_set()
            .preflight_accept(source.workspace_set(), scope)
        {
            tracing::warn!(?err, "move refused in preflight; both windows untouched");
            source.raise_open_notice(MOVE_REFUSED_NOTICE.to_owned());
            return;
        }
        let (content, hold) = match source.detach_for_move(scope) {
            Ok(moved) => moved,
            Err(err) => {
                tracing::warn!(?err, "move refused at detach; source untouched");
                source.raise_open_notice(MOVE_REFUSED_NOTICE.to_owned());
                return;
            }
        };
        match destination.attach_moved(content, hold) {
            Ok(()) => {
                let emptied = source.after_move_out(scope);
                if emptied {
                    // The destination now holds the source's last content: it
                    // inherits the primary's shape-autosave role, then the
                    // empty source is retired without a session shutdown.
                    destination.adopt_autosave_ownership_from(source, std::time::Instant::now());
                }
                destination.focus_quick_window();
                if emptied {
                    let mut retired = self.windows.remove(source_idx);
                    retired.release_surface();
                    self.detach_quick_if_owned(retired.process_window_id());
                }
                self.sync_sibling_counts();
            }
            Err(refused) => {
                let (err, content, hold) = *refused;
                tracing::warn!(?err, "move refused by the destination; content restored");
                source.restore_after_failed_move(content, hold);
            }
        }
    }

    /// Drain move-to-new-window requests and serve each one, creating the new
    /// window's surface through the event loop.
    pub(super) fn service_move_requests(&mut self, event_loop: &ActiveEventLoop) {
        let requests: Vec<(ProcessWindowId, MoveRequest)> = self
            .windows
            .iter_mut()
            .filter_map(|app| {
                let id = app.process_window_id();
                app.take_move_request().map(|request| (id, request))
            })
            .collect();
        for (origin, request) in requests {
            let MoveRequest::NewWindow(scope) = request;
            self.move_to_new_window(origin, scope, |app| {
                app.try_resume_presentation(event_loop)
                    .map_err(|err| err.to_string())
            });
        }
    }

    /// Build a new window around the active tab or pane of `origin`, open its
    /// surface with `open`, and add it to the window list. When `open` fails
    /// the new window is torn down and the content returns to `origin` as it
    /// was. Returns whether a window was added.
    pub(super) fn move_to_new_window(
        &mut self,
        origin: ProcessWindowId,
        scope: MoveScope,
        open: impl FnOnce(&mut App) -> Result<(), String>,
    ) -> bool {
        let Some(source_idx) = self.index_of(origin) else {
            return false;
        };
        if self.is_quick_window(origin) {
            self.windows[source_idx].raise_open_notice(QUICK_MOVE_NOTICE.to_owned());
            return false;
        }
        if self.windows[source_idx].move_empties_window(scope) {
            self.windows[source_idx].raise_open_notice(ONLY_CONTENT_NOTICE.to_owned());
            return false;
        }
        let Some(base) = crate::native::window_owner::next_window_token_base() else {
            self.windows[source_idx].raise_open_notice(MOVE_REFUSED_NOTICE.to_owned());
            return false;
        };
        let source = &mut self.windows[source_idx];
        let (content, hold) = match source.detach_for_move(scope) {
            Ok(moved) => moved,
            Err(err) => {
                tracing::warn!(?err, "move to a new window refused at detach");
                source.raise_open_notice(MOVE_REFUSED_NOTICE.to_owned());
                return false;
            }
        };
        let tokens = content.tokens();
        let template = content.restore_template();
        let set = WorkspaceSet::adopting(
            crate::native::session::SessionToken(base),
            content,
            source.workspace_set(),
            source.workspace_set().event_proxy(),
        );
        let mut window = (self.adopt)(set);
        if let Err(err) = open(&mut window) {
            tracing::warn!(%err, "new window for a moved tab could not open; content restored");
            let content = window.workspace_set_mut().release_adopted(template);
            window.release_surface();
            drop(window);
            self.windows[source_idx].restore_after_failed_move(content, hold);
            return false;
        }
        window.adopt_moved_hold(hold);
        window.arrive_moved_sessions(&tokens);
        window.focus_quick_window();
        self.windows[source_idx].after_move_out(scope);
        self.windows.push(window);
        self.sync_sibling_counts();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{adopt_for_test, headless, host_of};
    use super::*;
    use crate::core::{Dimensions, Terminal};
    use crate::native::quick_terminal::QuickTerminalIdentity;
    use crate::native::session::{Session, SessionToken};
    use std::sync::{Arc, Mutex};

    const SIBLING_BASE: u64 = 1 << 40;

    fn terminal() -> Arc<Mutex<Terminal>> {
        Arc::new(Mutex::new(Terminal::new(80, 24)))
    }

    /// A headless window whose single pane is `token` (a sibling window's
    /// disjoint range).
    fn window_at(token: u64) -> App {
        let session = Session::new_headless(
            SessionToken(token),
            terminal(),
            crate::native::test_support::headless_writer(),
            Arc::new(crate::native::session::HeadlessSession::new(
                Dimensions::new(80, 24),
            )),
        );
        adopt_for_test(WorkspaceSet::new(session, None))
    }

    /// A headless window with two tabs: token 0 and token 1 (active).
    fn two_tab_window() -> (App, Arc<Mutex<Terminal>>) {
        let mut app = headless();
        let model = terminal();
        let position = app.push_headless_session_for_test(
            model.clone(),
            crate::native::test_support::headless_writer(),
            Dimensions::new(80, 24),
        );
        assert!(app.switch_to_session_for_test(position));
        (app, model)
    }

    fn tab_leaves(app: &App) -> Vec<Vec<u64>> {
        app.workspace_set()
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .map(|tab| tab.layout.leaves().iter().map(|token| token.0).collect())
            .collect()
    }

    #[test]
    fn the_picker_moves_the_active_tab_and_the_source_keeps_the_rest() {
        let (origin, model) = two_tab_window();
        let moving = origin.active_session_token_for_test();
        let mut host = host_of(vec![origin, window_at(SIBLING_BASE)]);
        host.sync_sibling_counts();

        host.open_picker(0, MergeDirection::MoveTabInto);
        assert_eq!(host.windows[1].merge_numeral(), Some(1));
        assert!(
            host.windows[1].merge_picker_moves,
            "candidates say move here"
        );
        host.handle_picker_key(PickerKey::Select(1));

        assert_eq!(host.windows.len(), 2, "the source survives");
        assert!(!host.windows[0].owns_session(moving));
        assert_eq!(tab_leaves(&host.windows[0]), vec![vec![0]]);
        let destination = &host.windows[1];
        assert!(destination.owns_session(moving));
        assert_eq!(destination.active_session_token_for_test(), moving);
        let session = destination
            .workspace_set()
            .get(moving)
            .expect("moved session");
        assert!(
            Arc::ptr_eq(&session.terminal, &model),
            "same model, no respawn"
        );
        assert!(!host.windows[1].merge_picker_moves, "badges cleared");
    }

    #[test]
    fn moving_a_windows_last_tab_retires_it_and_hands_over_the_primary_role() {
        let mut origin = headless();
        origin.set_primary_instance_for_test(true);
        let moving = origin.active_session_token_for_test();
        let mut host = host_of(vec![origin, window_at(SIBLING_BASE)]);
        host.sync_sibling_counts();
        let destination_id = host.windows[1].process_window_id();

        host.open_picker(0, MergeDirection::MoveTabInto);
        host.handle_picker_key(PickerKey::Select(1));

        assert_eq!(host.windows.len(), 1, "the empty source window is retired");
        let survivor = &host.windows[0];
        assert_eq!(survivor.process_window_id(), destination_id);
        assert!(survivor.owns_session(moving));
        assert!(
            survivor.autosave_is_primary,
            "the primary role follows the content"
        );
    }

    #[test]
    fn a_pane_moves_to_a_new_window_with_its_hold_and_leaves_its_tab_split_no_more() {
        let mut origin = headless();
        let first = origin.active_session_token_for_test();
        origin.seed_headless_split_pane_for_test(
            true,
            terminal(),
            crate::native::test_support::headless_writer(),
            Dimensions::new(40, 24),
        );
        let moving = origin
            .active_tab_pane_tokens_for_test()
            .into_iter()
            .find(|token| *token != first)
            .expect("split pane");
        origin.focus_session_token_for_test(moving);
        origin.last_active_session = moving;
        origin.hold_session = Some(moving);
        origin.ime_session = Some(moving);
        // A program that asked for focus reports hears the pane leave.
        origin
            .workspace_set()
            .get(moving)
            .expect("pane")
            .terminal
            .lock()
            .expect("terminal")
            .advance(b"\x1b[?1004h");
        origin.set_window_focus_for_test(true);
        origin.focus_reports_for_test.clear();
        let origin_id = origin.process_window_id();
        let mut host = host_of(vec![origin]);

        assert!(host.move_to_new_window(origin_id, MoveScope::ActivePane, |_| Ok(())));

        assert_eq!(host.windows.len(), 2);
        let source = &host.windows[0];
        assert_eq!(tab_leaves(source), vec![vec![first.0]]);
        assert_eq!(source.hold_session, None, "the hold travels with the pane");
        assert_eq!(source.ime_session, None, "the composition is dropped");
        assert!(source.focus_reports_for_test.contains(&(moving, false)));
        let new_window = &host.windows[1];
        assert!(new_window.owns_session(moving));
        assert_eq!(new_window.hold_session, Some(moving));
        assert_eq!(tab_leaves(new_window), vec![vec![moving.0]]);
    }

    #[test]
    fn a_new_window_that_cannot_open_returns_the_content_unchanged() {
        let (origin, _model) = two_tab_window();
        let before = tab_leaves(&origin);
        let active_before = origin.active_session_token_for_test();
        let origin_id = origin.process_window_id();
        let mut host = host_of(vec![origin]);

        let opened = host.move_to_new_window(origin_id, MoveScope::ActiveTab, |_| {
            Err("no surface".to_owned())
        });

        assert!(!opened);
        assert_eq!(host.windows.len(), 1);
        assert_eq!(tab_leaves(&host.windows[0]), before);
        assert_eq!(
            host.windows[0].active_session_token_for_test(),
            active_before
        );
        assert_eq!(
            host.windows[0].open_notice_message_for_test().as_deref(),
            Some(MOVE_REFUSED_NOTICE)
        );
    }

    #[test]
    fn a_windows_only_tab_does_not_move_to_a_new_window() {
        let origin = headless();
        let origin_id = origin.process_window_id();
        let mut host = host_of(vec![origin]);

        assert!(!host.move_to_new_window(origin_id, MoveScope::ActiveTab, |_| Ok(())));
        assert_eq!(host.windows.len(), 1);
        assert_eq!(
            host.windows[0].open_notice_message_for_test().as_deref(),
            Some(ONLY_CONTENT_NOTICE)
        );
    }

    #[test]
    fn the_quick_terminal_neither_sends_nor_receives() {
        let (origin, _model) = two_tab_window();
        let origin_id = origin.process_window_id();
        let mut host = host_of(vec![origin, window_at(SIBLING_BASE)]);
        let quick_id = host.windows[1].process_window_id();
        host.quick
            .attach_window(QuickTerminalIdentity::new(quick_id));
        let before = tab_leaves(&host.windows[0]);

        host.execute_move(origin_id, quick_id, MoveScope::ActiveTab);
        assert_eq!(
            tab_leaves(&host.windows[0]),
            before,
            "not into the quick window"
        );

        let (quick, _model) = two_tab_window();
        let quick_origin = quick.process_window_id();
        let mut host = host_of(vec![quick]);
        host.quick
            .attach_window(QuickTerminalIdentity::new(quick_origin));
        assert!(!host.move_to_new_window(quick_origin, MoveScope::ActiveTab, |_| Ok(())));
        assert_eq!(host.windows.len(), 1);
        assert_eq!(
            host.windows[0].open_notice_message_for_test().as_deref(),
            Some(QUICK_MOVE_NOTICE)
        );
    }

    #[test]
    fn a_refused_destination_restores_the_source() {
        let (origin, _model) = two_tab_window();
        let before = tab_leaves(&origin);
        let origin_id = origin.process_window_id();
        // The destination already holds token 1: a collision.
        let mut host = host_of(vec![origin, window_at(1)]);
        let target = host.windows[1].process_window_id();

        host.execute_move(origin_id, target, MoveScope::ActiveTab);

        assert_eq!(tab_leaves(&host.windows[0]), before);
        assert_eq!(host.windows.len(), 2);
    }

    #[test]
    fn a_session_attached_in_another_window_is_not_attached_twice() {
        let mut sibling = window_at(SIBLING_BASE);
        let token = sibling.active_session_token_for_test();
        sibling
            .workspace_set_mut()
            .get_mut(token)
            .expect("pane")
            .attached_session_id = Some("s-0001-aaaa".to_owned());
        let mut host = host_of(vec![headless(), sibling]);
        host.sync_peer_attached_sessions();

        host.windows[0].route_attach_session("s-0001-aaaa".to_owned());

        assert_eq!(
            host.windows[0].open_notice_message_for_test().as_deref(),
            Some(crate::native::app::commands::ATTACHED_ELSEWHERE_NOTICE)
        );
        assert!(!host.windows[0].overlay.is_open(), "no attach choice opens");
        assert!(
            host.windows[1].peer_attached_sessions.is_empty(),
            "a window's own attachments are not its peers'"
        );
    }

    #[test]
    fn the_palette_offers_only_moves_that_can_happen() {
        let lone = headless();
        let rows = lone.move_palette_rows();
        assert!(!rows.tab_to_new_window, "a lone tab already has its window");
        assert!(
            !rows.tab_to_window && !rows.pane_to_window,
            "no sibling window"
        );
        assert!(!rows.pane_to_new_window, "no split");

        let (mut two_tabs, _model) = two_tab_window();
        two_tabs.seed_headless_split_pane_for_test(
            true,
            terminal(),
            crate::native::test_support::headless_writer(),
            Dimensions::new(40, 24),
        );
        two_tabs.set_sibling_window_count(1);
        let rows = two_tabs.move_palette_rows();
        assert!(rows.tab_to_new_window && rows.tab_to_window);
        assert!(rows.pane_to_new_window && rows.pane_to_window);
    }
}

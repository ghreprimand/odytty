// SPDX-License-Identifier: GPL-3.0-only
//! Moving a tab or a pane to another window of this process: the window side.
//!
//! The arena transaction lives in `session::reparent`; this module does what
//! a window must do around it.
//!
//! - **Source, before the move.** Window-scoped state that names a moving
//!   token is settled: a focused pane gets its focus-out report, pending paste,
//!   drop, and image-paste confirmations are cancelled, an IME composition on
//!   the pane is dropped, a context-menu command target and any open overlay
//!   (whose intents may name the pane) are closed, drags are settled, and the
//!   `--hold` state of the pane travels with it.
//! - **Source, after the move.** OSC 52 consents of departed panes are pruned,
//!   the remaining panes reflow, and the new active pane is reconciled.
//! - **Destination.** The moved panes' presentation caches (last presented
//!   frame, render signature, cursor comparison) are dropped because they hold
//!   the source window's geometry; their timers are parked; this window's
//!   presentation policy is applied; and the tab becomes active, which sends
//!   focus reports, resizes the panes to this window's grid, and repaints.
//!
//! A command-output export save dialog that was open when its pane moved
//! stays with the window that opened it; the export itself follows the pane.
//! Platform-neutral: Linux (Wayland and X11), macOS, and Windows run the same
//! path. The quick terminal never sends or receives (the window owner refuses).

use super::*;
use crate::native::session::{MoveError, MoveScope, MovedContent};

/// The `--hold` state that travels with a moved pane.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::native) struct MovedHold {
    hold_session: Option<SessionToken>,
    held_exit: Option<SessionToken>,
}

/// A request to move content out of this window, captured from the palette
/// and serviced by the window owner after the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum MoveRequest {
    /// Open a new window holding the content.
    NewWindow(MoveScope),
}

/// Notice shown when a move cannot happen.
pub(in crate::native) const MOVE_REFUSED_NOTICE: &str =
    "Could not move: the window was left unchanged";

impl App {
    /// Capture a request to move the active tab or pane into a new window.
    pub(in crate::native) fn request_move_to_new_window(&mut self, scope: MoveScope) {
        self.pending_move = Some(MoveRequest::NewWindow(scope));
    }

    /// Take the pending move request, if any (window owner only).
    pub(in crate::native) fn take_move_request(&mut self) -> Option<MoveRequest> {
        self.pending_move.take()
    }

    /// Whether moving the active tab out would leave this window with no
    /// other tab (so "to a new window" would only recreate this window).
    pub(in crate::native) fn move_empties_window(&self, scope: MoveScope) -> bool {
        self.sessions.move_empties_window(scope)
    }

    /// Whether the active tab holds more than one pane.
    pub(in crate::native) fn active_tab_is_split(&self) -> bool {
        !self.sessions.active_is_single_pane()
    }

    /// Settle every window-scoped piece of state that names a pane about to
    /// leave, then detach it. Returns the content and its `--hold` state, or
    /// the refusal with this window unchanged.
    pub(in crate::native) fn detach_for_move(
        &mut self,
        scope: MoveScope,
    ) -> Result<(MovedContent, MovedHold), MoveError> {
        let tokens: Vec<SessionToken> = match scope {
            MoveScope::ActiveTab => self.active_tab_tokens(),
            MoveScope::ActivePane => vec![self.sessions.active_id()],
        };
        if tokens.is_empty() {
            return Err(MoveError::SourceEmpty);
        }
        self.settle_before_move_out(&tokens);
        let content = self.sessions.detach_for_move(scope)?;
        let mut hold = MovedHold::default();
        for token in content.tokens() {
            if self.hold_session == Some(token) {
                hold.hold_session = self.hold_session.take();
            }
            if self.held_exit == Some(token) {
                hold.held_exit = self.held_exit.take();
            }
        }
        Ok((content, hold))
    }

    fn active_tab_tokens(&self) -> Vec<SessionToken> {
        self.sessions
            .active_layout()
            .map(|layout| layout.leaves())
            .unwrap_or_default()
    }

    /// Release this window's hold on panes that are about to leave.
    fn settle_before_move_out(&mut self, tokens: &[SessionToken]) {
        self.finish_divider_drag();
        let _ = self.cancel_workspace_drag();
        let _ = self.cancel_top_tab_drag();
        self.prefix_engine.cancel();
        // A focused pane that leaves is no longer focused here: tell a program
        // that asked for focus reports before the window loses its writer.
        if self.focused && tokens.contains(&self.last_active_session) {
            self.send_focus_report_to(self.last_active_session, false);
        }
        self.cancel_pending_input_for_merge();
        if self
            .ime_session
            .is_some_and(|token| tokens.contains(&token))
        {
            self.ime_preedit.clear();
            self.ime_session = None;
        }
        if self
            .context_command_handle
            .is_some_and(|(token, _)| tokens.contains(&token))
        {
            self.context_command_handle = None;
        }
        if self.overlay.is_open() {
            self.overlay.close();
        }
    }

    /// Reconcile this window after content left. Returns whether the window
    /// is now empty (the owner retires it).
    pub(in crate::native) fn after_move_out(&mut self, scope: MoveScope) -> bool {
        let live: Vec<SessionToken> = self.sessions.iter().map(|session| session.id).collect();
        self.prune_osc52_session_state(&live);
        if self.sessions.workspaces.is_empty() {
            return true;
        }
        if scope == MoveScope::ActivePane {
            self.reflow_active_panes_and_redraw();
        }
        self.on_active_session_changed();
        self.request_redraw_now();
        false
    }

    /// Put content back after a refused move, as if it never left.
    pub(in crate::native) fn restore_after_failed_move(
        &mut self,
        content: MovedContent,
        hold: MovedHold,
    ) {
        self.sessions.restore_moved(content);
        self.adopt_moved_hold(hold);
        if scope_was_pane_reflow(&self.sessions) {
            self.reflow_active_panes_and_redraw();
        }
        self.on_active_session_changed();
        self.raise_open_notice(MOVE_REFUSED_NOTICE.to_owned());
    }

    /// Take moved content as this window's new active tab. On refusal the
    /// content comes back for the source to restore.
    pub(in crate::native) fn attach_moved(
        &mut self,
        content: MovedContent,
        hold: MovedHold,
    ) -> Result<(), Box<(MoveError, MovedContent, MovedHold)>> {
        let tokens = content.tokens();
        self.finish_divider_drag();
        if let Err(refused) = self.sessions.attach_moved(content) {
            let (err, content) = *refused;
            return Err(Box::new((err, content, hold)));
        }
        self.adopt_moved_hold(hold);
        self.arrive_moved_sessions(&tokens);
        Ok(())
    }

    /// Reconcile sessions that just arrived, either through
    /// [`Self::attach_moved`] or because this window was built around them.
    pub(in crate::native) fn arrive_moved_sessions(&mut self, tokens: &[SessionToken]) {
        for token in tokens {
            if let Some(session) = self.sessions.get_mut(*token) {
                session.last_presented_snapshot = None;
                session.last_render_signature = None;
                session.last_cursor_comparison_snapshot = None;
                session.needs_rebuild = true;
                session.park_animation_timers();
            }
        }
        self.apply_model_state_to_all_sessions();
        self.on_active_session_changed();
        self.reflow_active_panes_and_redraw();
        self.sessions.park_background_timers();
        self.request_redraw_now();
    }

    /// Host session ids this window shows as attached panes.
    pub(in crate::native) fn attached_session_ids(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter_map(|session| session.attached_session_id.clone())
            .collect()
    }

    /// Record the host session ids attached in other windows (window owner).
    pub(in crate::native) fn set_peer_attached_sessions(&mut self, ids: Vec<String>) {
        self.peer_attached_sessions = ids;
    }

    /// Adopt the `--hold` state that travelled with moved panes.
    pub(in crate::native) fn adopt_moved_hold(&mut self, hold: MovedHold) {
        if hold.hold_session.is_some() {
            self.hold_session = hold.hold_session;
        }
        if hold.held_exit.is_some() {
            self.held_exit = hold.held_exit;
        }
    }
}

/// After a rollback the source tab is whole again; a pane rollback changed its
/// tree back, so the panes need their split geometry. Reflowing an unsplit tab
/// is harmless, so this reports whether the active tab is split.
fn scope_was_pane_reflow(sessions: &crate::native::session::WorkspaceSet) -> bool {
    !sessions.active_is_single_pane()
}

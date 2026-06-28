// SPDX-License-Identifier: GPL-3.0-only
//! App-side session-attach summon integration (Phase 5 / B2).
//!
//! The overlay owns list/filter/select presentation. This module owns only the
//! act of opening it: it enumerates the live, detached session-host sessions
//! through [`crate::session_host::list_live_sessions`] and hands a frozen clone
//! to the overlay. Opening is presentation-only — it never attaches anything
//! itself and never mutates the live terminal model. Accepting a row later
//! emits an [`super::super::overlay::OverlayOutcome::AttachSession`] the App
//! turns into a new-tab attach.

use super::*;

impl App {
    /// Open the in-window session-attach summon overlay over the live, detached
    /// sessions. The entry list is a frozen clone, so it stays stable while the
    /// overlay is open even if a session ends underneath it (a stale id is
    /// handled gracefully on accept). When no sessions are live the list is
    /// empty and the overlay shows a hint rather than failing to open.
    ///
    /// `runtime_base` is `None` in production (the registry derives the runtime
    /// dir from `XDG_RUNTIME_DIR`); a failed enumeration is treated as "no live
    /// sessions" so the overlay still opens with its hint rather than erroring.
    pub(super) fn open_session_attach_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        // The live detached-session list comes from the Unix-only session-host
        // registry; on Windows there are no detached sessions, so the overlay
        // opens with an empty list and shows its "no live sessions" hint.
        #[cfg(unix)]
        let entries = crate::session_host::list_live_sessions(None).unwrap_or_default();
        #[cfg(not(unix))]
        let entries: Vec<crate::session_host::ListedSession> = Vec::new();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_session_attach(entries);
        self.request_selection_redraw();
    }
}

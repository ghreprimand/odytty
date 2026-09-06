// SPDX-License-Identifier: GPL-3.0-only
//! App-side session-attach summon entry point.
//!
//! Since the Session Navigator unification this is a thin compatibility shim:
//! the "attach a session" summon now opens the unified navigator, which owns
//! enumeration and merging of live sessions (workspaces/tabs/panes from the live
//! `WorkspaceSet`) plus, on Unix, detached session-host sessions. That
//! enumeration lives in `session_navigator_ui.rs`, not here; this module only
//! forwards the open request.

use super::*;

impl App {
    /// Open the summon overlay. Delegates to the unified Session Navigator
    /// ([`Self::open_session_navigator_overlay`]), which enumerates and merges
    /// live and (on Unix) detached sessions. Opening is presentation-only: it
    /// never attaches anything itself and never mutates the live terminal model.
    pub(super) fn open_session_attach_overlay(&mut self) {
        self.open_session_navigator_overlay();
    }
}

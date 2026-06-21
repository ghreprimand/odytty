// SPDX-License-Identifier: GPL-3.0-only
//! App-side output-replay integration (Phase 2).
//!
//! The overlay owns the scrub state over a frozen clone of the focused
//! session's recorded frames. This module owns only the act of opening it: it
//! snapshots the recorder ring (a decoupled clone) and hands it to the overlay.
//! Replay is presentation-only — opening it never writes to the PTY and never
//! mutates the live terminal model.

use super::*;

impl App {
    /// Open the output-replay overlay over the focused session's recorded
    /// frames. The frame list is a decoupled clone of the recorder ring, so the
    /// live session keeps recording while the user scrubs. When recording is off
    /// (or nothing has been recorded yet) the clone is empty and the overlay
    /// shows a hint rather than failing to open.
    pub(super) fn open_replay_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        let frames = self.sessions.active_recorder_frames();
        self.reset_pointer_state_for_overlay();
        self.overlay.open_replay(frames);
        self.request_selection_redraw();
    }
}

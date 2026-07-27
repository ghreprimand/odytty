// SPDX-License-Identifier: GPL-3.0-only
//! Watchdog state probe (FREEZE-HARDEN item b): the one place the freeze
//! watchdog reads `App` state. Lives as an `App` child module (like
//! `pointer`/`ime`) so it reaches private fields without widening any field
//! visibility; the wrapper in `native::watchdog` calls this after every
//! delegated event.
//!
//! PRIVACY: this probe may only ever export booleans, counters, and C-like
//! enum discriminants (see [`WatchdogAppState`]) — never strings, titles, or
//! buffer contents. That is what makes the watchdog's stall record safe to
//! ship to a log file by construction.

use super::*;
use crate::native::watchdog::WatchdogAppState;

impl App {
    /// Snapshot the freeze-relevant state machine: the postmortem's requested
    /// fields (focused flag, occluded/minimized latch, active overlay/modal,
    /// `self.window` presence, frame counters), all as plain state.
    pub(in crate::native) fn watchdog_state(&self) -> WatchdogAppState {
        WatchdogAppState {
            focused: self.focused,
            window_minimized: self.window_minimized,
            window_present: self.window.is_some(),
            gpu_present: self.gpu.is_some(),
            overlay_open: self.overlay.is_open(),
            context_menu_open: self.overlay.is_context_menu(),
            modal: match self.active_modal() {
                ActiveModal::None => 0,
                ActiveModal::CopyMode => 1,
                ActiveModal::HintsSelect => 2,
                ActiveModal::RenameTab => 3,
            },
            needs_rebuild: self.needs_rebuild,
            frames_presented: self
                .gpu
                .as_ref()
                .map(GpuState::frames_presented)
                .unwrap_or(0),
            consecutive_skipped_frames: self.consecutive_skipped_frames,
            redraws_delivered: self.redraws_delivered,
            // Gating discriminator for the stall log: is a frame genuinely
            // owed right now? Use the multipane-aware `should_rebuild_frame()`
            // (NOT the bare single-pane `needs_rebuild`, which is still
            // exported above for the postmortem record) OR a pending
            // skipped-frame retry. When this is false the watchdog treats
            // latched-but-unpresented work as idle/background, not a freeze.
            render_owed: self.should_rebuild_frame() || self.skipped_frame_retry_deadline.is_some(),
        }
    }
}

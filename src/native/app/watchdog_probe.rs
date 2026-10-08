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
    /// The count of GPU frames this window has actually presented (v0.15.0 A
    /// readiness probe). Zero until the first usable terminal frame lands.
    /// Cheap: a single atomic load through the GPU state, no snapshot build.
    /// Used as the first-usable-frame predicate that gates deferred
    /// global-shortcut registration, so the OS grab never runs before a real
    /// terminal surface exists.
    pub(in crate::native) fn frames_presented(&self) -> u64 {
        self.gpu
            .as_ref()
            .map(GpuState::frames_presented)
            .unwrap_or(0)
    }

    /// Snapshot the freeze-relevant state machine: the postmortem's requested
    /// fields (focused flag, occluded/minimized latch, active overlay/modal,
    /// `self.window` presence, frame counters), all as plain state.
    pub(in crate::native) fn watchdog_state(&self) -> WatchdogAppState {
        WatchdogAppState {
            focused: self.focused,
            window_minimized: self.window_minimized,
            window_occluded: self.window_occluded,
            window_present: self.window.is_some(),
            gpu_present: self.gpu.is_some(),
            wayland_surface: self.is_wayland_client(),
            overlay_open: self.overlay.is_open(),
            context_menu_open: self.overlay.is_context_menu(),
            modal: match self.active_modal() {
                ActiveModal::None => 0,
                ActiveModal::CopyMode => 1,
                ActiveModal::HintsSelect => 2,
                ActiveModal::RenameTab => 3,
                ActiveModal::FloatArrange => 4,
            },
            needs_rebuild: self.needs_rebuild,
            frames_presented: self
                .gpu
                .as_ref()
                .map(GpuState::frames_presented)
                .unwrap_or(0),
            consecutive_skipped_frames: self.consecutive_skipped_frames,
            // The fast retry budget is spent: retries now run on the slow
            // keep-alive cadence (see `next_skipped_retry_delay`).
            skip_slow_retry: self.consecutive_skipped_frames >= frame::MAX_SKIPPED_RETRIES,
            redraws_delivered: self.redraws_delivered,
            // Gating discriminator for the stall log: is a frame genuinely
            // owed right now? Use the multipane-aware `should_rebuild_frame()`
            // (NOT the bare single-pane `needs_rebuild`, which is still
            // exported above for the postmortem record), a pending
            // skipped-frame retry, or the owed-frame latch that remains armed
            // until a present. When this is false the watchdog treats
            // latched-but-unpresented work as idle/background, not a freeze.
            // A lost device owes no frame: rendering is paused (and logged
            // once), so latched work is not a stall.
            render_owed: self.watchdog_render_owed(),
        }
    }

    /// Whether this window owes a frame it can be expected to present: it
    /// owes one (see [`Self::watchdog_render_owed`]) and it is shown, not
    /// minimized or occluded, since a hidden window is legitimately not asked
    /// to draw. The process host asks every window, so one window's presented
    /// frames cannot vouch for a visible sibling that owes one.
    pub(in crate::native) fn watchdog_owes_visible_frame(&self) -> bool {
        self.window.is_some()
            && !self.window_minimized
            && !self.window_occluded
            && self.watchdog_render_owed()
    }

    /// Whether this window genuinely owes a frame right now (see
    /// `render_owed` above).
    fn watchdog_render_owed(&self) -> bool {
        (!self.live_drag_destination || self.window.is_some())
            && !self.live_drag_source
            && !self.gpu_device_lost
            && (self.frame_owed_since.is_some()
                || self.should_rebuild_frame()
                || self.skipped_frame_retry_deadline.is_some())
    }
}

/// One window's presentation progress, as the process host's freeze
/// watchdog sees it: a stable window id, its presented-frame count, and
/// whether it owes a frame while shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct WindowProgress {
    pub(in crate::native) id: u64,
    pub(in crate::native) frames: u64,
    pub(in crate::native) owes_frame: bool,
}

/// Whether the host may report a presented frame to the shared watchdog.
/// That needs some window to have presented since the last report and every
/// window that owes a frame to have presented too, so a window that keeps
/// animating cannot clear the pending-work latch for a stalled sibling. On a
/// report, `baseline` (frames per window at the last report) advances to the
/// current counts; otherwise it is left alone. With one window this is the
/// historical rule: report whenever its frame count changed.
pub(in crate::native) fn report_present_for_windows(
    baseline: &mut Vec<(u64, u64)>,
    windows: &[WindowProgress],
) -> bool {
    let presented_since = |window: &WindowProgress| presented_since(baseline, window);
    let any = windows.iter().any(presented_since);
    let owing_all_presented = windows
        .iter()
        .filter(|window| window.owes_frame)
        .all(presented_since);
    if !(any && owing_all_presented) {
        return false;
    }
    baseline.clear();
    baseline.extend(windows.iter().map(|window| (window.id, window.frames)));
    true
}

/// The first window that owes a frame while shown and has presented nothing
/// since the last report, if any. The host mirrors that window's state into
/// the watchdog, so a stall record describes the window that is stuck.
pub(in crate::native) fn stalled_window(
    baseline: &[(u64, u64)],
    windows: &[WindowProgress],
) -> Option<usize> {
    windows
        .iter()
        .position(|window| window.owes_frame && !presented_since(baseline, window))
}

fn presented_since(baseline: &[(u64, u64)], window: &WindowProgress) -> bool {
    let before = baseline
        .iter()
        .find(|(id, _)| *id == window.id)
        .map_or(0, |(_, frames)| *frames);
    window.frames != before
}

#[cfg(test)]
mod progress_tests {
    use super::{WindowProgress, report_present_for_windows};

    fn window(id: u64, frames: u64, owes_frame: bool) -> WindowProgress {
        WindowProgress {
            id,
            frames,
            owes_frame,
        }
    }

    #[test]
    fn one_window_reports_whenever_its_frames_change() {
        let mut baseline = Vec::new();
        assert!(!report_present_for_windows(
            &mut baseline,
            &[window(1, 0, true)]
        ));
        assert!(report_present_for_windows(
            &mut baseline,
            &[window(1, 1, true)]
        ));
        assert!(!report_present_for_windows(
            &mut baseline,
            &[window(1, 1, true)]
        ));
        assert!(report_present_for_windows(
            &mut baseline,
            &[window(1, 2, false)]
        ));
    }

    /// An animating window cannot vouch for a sibling that owes a frame and
    /// presents nothing; once the sibling presents, the report goes through.
    #[test]
    fn an_animating_window_does_not_hide_a_stalled_sibling() {
        let mut baseline = vec![(1, 10), (2, 5)];
        for frames in 11..20 {
            assert!(
                !report_present_for_windows(
                    &mut baseline,
                    &[window(1, frames, true), window(2, 5, true)]
                ),
                "window 2 still owes its frame"
            );
        }
        assert!(report_present_for_windows(
            &mut baseline,
            &[window(1, 20, true), window(2, 6, false)]
        ));
        assert_eq!(baseline, vec![(1, 20), (2, 6)]);
        assert_eq!(
            super::stalled_window(
                &[(1, 20), (2, 6)],
                &[window(1, 21, true), window(2, 6, true)]
            ),
            Some(1),
            "the stuck window is the one whose state is mirrored"
        );
        // A sibling that owes nothing does not hold the report back.
        assert!(report_present_for_windows(
            &mut baseline,
            &[window(1, 21, true), window(2, 6, false)]
        ));
    }
}

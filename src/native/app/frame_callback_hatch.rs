// SPDX-License-Identifier: GPL-3.0-only
//! Policy for recovering a Wayland surface whose outstanding frame callback
//! no longer produces redraw events.

use std::time::{Duration, Instant};

use super::App;

/// An owed frame may wait this long before the callback escape hatch paints it.
pub(in crate::native) const FRAME_CALLBACK_STALE_AFTER: Duration = Duration::from_secs(2);
/// Minimum spacing between escape-hatch paint attempts for one window.
pub(in crate::native) const FRAME_CALLBACK_HATCH_INTERVAL: Duration = Duration::from_secs(1);

/// State gates that must all hold before a callback escape-hatch paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct FrameCallbackHatchGates {
    pub(in crate::native) presentation_active: bool,
    pub(in crate::native) render_owed: bool,
    pub(in crate::native) focused: bool,
    pub(in crate::native) minimized: bool,
    pub(in crate::native) occluded: bool,
    pub(in crate::native) wayland: bool,
}

impl FrameCallbackHatchGates {
    fn allow_hatch(self) -> bool {
        self.presentation_active
            && self.render_owed
            && self.focused
            && !self.minimized
            && !self.occluded
            && self.wayland
    }
}

/// Whether a direct paint may bypass a stale Wayland frame callback now.
pub(in crate::native) fn should_run_frame_callback_hatch(
    now: Instant,
    gates: FrameCallbackHatchGates,
    owed_since: Option<Instant>,
    delivered_at_start: u64,
    delivered_now: u64,
    last_hatch: Option<Instant>,
) -> bool {
    gates.allow_hatch()
        && owed_since
            .is_some_and(|since| now.saturating_duration_since(since) >= FRAME_CALLBACK_STALE_AFTER)
        && delivered_now == delivered_at_start
        && last_hatch
            .is_none_or(|last| now.saturating_duration_since(last) >= FRAME_CALLBACK_HATCH_INTERVAL)
}

/// Next instant at which the hatch could paint if no redraw arrives first.
pub(in crate::native) fn frame_callback_hatch_deadline(
    gates: FrameCallbackHatchGates,
    owed_since: Option<Instant>,
    delivered_at_start: u64,
    delivered_now: u64,
    last_hatch: Option<Instant>,
) -> Option<Instant> {
    if !gates.allow_hatch() || delivered_now != delivered_at_start {
        return None;
    }
    let stale_at = owed_since?.checked_add(FRAME_CALLBACK_STALE_AFTER)?;
    let rate_limited_at = last_hatch
        .and_then(|last| last.checked_add(FRAME_CALLBACK_HATCH_INTERVAL))
        .unwrap_or(stale_at);
    Some(stale_at.max(rate_limited_at))
}

impl App {
    fn frame_callback_hatch_gates(&self) -> FrameCallbackHatchGates {
        FrameCallbackHatchGates {
            presentation_active: self.frame_callback_hatch_presentation_active(),
            render_owed: self.frame_owed_since.is_some(),
            focused: self.focused,
            minimized: self.window_minimized,
            occluded: self.window_occluded,
            wayland: self.is_wayland_client(),
        }
    }

    fn frame_callback_hatch_presentation_active(&self) -> bool {
        #[cfg(test)]
        if let Some(active) = self.frame_callback_hatch_presentation_active_for_test {
            return active;
        }
        self.window.is_some() && self.gpu.is_some()
    }

    #[cfg(target_os = "linux")]
    pub(super) fn is_wayland_client(&self) -> bool {
        #[cfg(test)]
        if let Some(present) = self.wayland_surface_present_for_test {
            return present;
        }
        self.wayland_surface_ptr().is_some()
    }

    /// The Wayland frame-callback protocol is the only backend where a pending
    /// callback can suppress redraw delivery indefinitely. X11, macOS, and
    /// Windows therefore leave the timer hatch disabled.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn is_wayland_client(&self) -> bool {
        false
    }

    pub(super) fn refresh_frame_owed_interval(&mut self, now: Instant) {
        let render_owed =
            self.should_rebuild_frame() || self.skipped_frame_retry_deadline.is_some();
        if render_owed && self.frame_owed_since.is_none() {
            self.frame_owed_since = Some(now);
            self.redraws_delivered_at_owed_start = self.redraws_delivered;
        }
    }

    pub(super) fn clear_frame_callback_hatch_episode(&mut self) {
        self.frame_owed_since = None;
        self.redraws_delivered_at_owed_start = self.redraws_delivered;
        self.last_frame_callback_hatch_at = None;
    }

    pub(super) fn next_frame_callback_hatch_deadline(&self) -> Option<Instant> {
        frame_callback_hatch_deadline(
            self.frame_callback_hatch_gates(),
            self.frame_owed_since,
            self.redraws_delivered_at_owed_start,
            self.redraws_delivered,
            self.last_frame_callback_hatch_at,
        )
    }

    pub(super) fn run_frame_callback_hatch(&mut self, now: Instant) {
        self.refresh_frame_owed_interval(now);
        if !should_run_frame_callback_hatch(
            now,
            self.frame_callback_hatch_gates(),
            self.frame_owed_since,
            self.redraws_delivered_at_owed_start,
            self.redraws_delivered,
            self.last_frame_callback_hatch_at,
        ) {
            return;
        }

        self.last_frame_callback_hatch_at = Some(now);
        #[cfg(test)]
        if self
            .frame_callback_hatch_presentation_active_for_test
            .is_some()
        {
            self.frame_callback_hatch_paints_for_test =
                self.frame_callback_hatch_paints_for_test.saturating_add(1);
        } else {
            let _ = self.on_redraw_requested();
        }
        #[cfg(not(test))]
        let _ = self.on_redraw_requested();

        // A hatch attempt counts as a delivered redraw so the classic
        // watchdog can diagnose a render stall. If it did not present, accept
        // only that forced delivery as the new baseline so another bounded
        // hatch attempt remains possible; an actual compositor delivery still
        // disables the hatch for the rest of this owed interval.
        if self.frame_owed_since.is_some() {
            self.redraws_delivered_at_owed_start = self.redraws_delivered;
        }
    }
}

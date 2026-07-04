// SPDX-License-Identifier: GPL-3.0-only
use std::time::{Duration, Instant};

/// Half-period of the cursor blink, i.e. the interval between on/off toggles.
/// ~530ms matches the long-standing xterm/VT default cadence.
pub(super) const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Drives the cursor blink on/off phase from injected time.
///
/// Policy (documented): the cursor only blinks when the active style requests it
/// (DECSCUSR or the host default) **and** the window is focused. When either is
/// false the cursor is held solid-on and no wake is scheduled, so an idle or
/// unfocused window never spins the event loop. While blinking, [`Self::poll`]
/// toggles at [`CURSOR_BLINK_INTERVAL`] and [`Self::deadline`] reports the next
/// toggle instant for `ControlFlow::WaitUntil`, bounding the wake rate.
#[derive(Debug, Clone, Copy)]
pub(super) struct CursorBlinkState {
    interval: Duration,
    on: bool,
    next_toggle: Option<Instant>,
}

impl CursorBlinkState {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            on: true,
            next_toggle: None,
        }
    }

    /// Update the blink phase for `now` and return whether the cursor is
    /// currently visible (on-phase). Solid-on (and deadline cleared) whenever the
    /// cursor is not blinking or the window is unfocused.
    pub(super) fn poll(&mut self, now: Instant, blinking: bool, focused: bool) -> bool {
        if !blinking || !focused {
            self.on = true;
            self.next_toggle = None;
            return true;
        }
        match self.next_toggle {
            None => {
                self.on = true;
                self.next_toggle = Some(now + self.interval);
            }
            Some(deadline) if now >= deadline => {
                self.on = !self.on;
                self.next_toggle = Some(now + self.interval);
            }
            Some(_) => {}
        }
        self.on
    }

    /// The next scheduled toggle instant, if the cursor is currently blinking.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.next_toggle
    }

    /// Park the blink to solid-on with no scheduled wake — the same rest state as
    /// focus loss, but reached without a `now`/`blinking`/`focused` sample.
    /// Used to settle a session's blink when it is deactivated (a background tab
    /// is never rendered, so its cursor must not keep a live toggle deadline that
    /// nothing consumes — see NF20-B fan-out fix). Re-arms naturally on the next
    /// [`Self::poll`] once the session is active and focused again.
    pub(super) fn park(&mut self) {
        self.on = true;
        self.next_toggle = None;
    }

    /// Whether a scheduled toggle is due at `now` (the loop should rebuild and
    /// redraw so the phase flips).
    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.next_toggle.is_some_and(|deadline| now >= deadline)
    }
}

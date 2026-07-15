// SPDX-License-Identifier: GPL-3.0-only
use std::time::{Duration, Instant};

/// Half-period of the cursor blink, i.e. the interval between on/off toggles.
/// ~530ms matches the long-standing xterm/VT default cadence.
pub(super) const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Quiet interval after keyboard activity before a blinking cursor may first
/// turn off. Keeping the cursor visible while typing makes the insertion point
/// stable without changing the application's requested cursor shape.
pub(super) const CURSOR_ACTIVITY_HOLD: Duration = Duration::from_millis(650);

/// Keyboard-idle boundary at which an otherwise blinking cursor parks solid-on
/// and stops scheduling wakeups. A later key, IME update, focus gain, or pane
/// activation re-arms the normal activity hold.
pub(super) const CURSOR_BLINK_STOP_AFTER: Duration = Duration::from_secs(15);

/// Drives the cursor blink on/off phase from injected time.
///
/// Policy (documented): the cursor only blinks when the active style requests it
/// (DECSCUSR or the host default) **and** the window is focused. When either is
/// false the cursor is held solid-on and no wake is scheduled, so an unfocused
/// window never spins the event loop. Keyboard activity holds a blinking cursor
/// visible for [`CURSOR_ACTIVITY_HOLD`], after which [`Self::poll`] toggles at
/// [`CURSOR_BLINK_INTERVAL`]. After [`CURSOR_BLINK_STOP_AFTER`] of keyboard
/// inactivity it parks solid-on with no wake. [`Self::deadline`] reports only
/// the next state boundary for `ControlFlow::WaitUntil`.
#[derive(Debug, Clone, Copy)]
pub(super) struct CursorBlinkState {
    interval: Duration,
    on: bool,
    next_toggle: Option<Instant>,
    last_keyboard_activity: Option<Instant>,
    /// An idle-stop is distinct from focus loss and a steady application
    /// cursor. All three park solid, but only this state must remain parked
    /// when the next render samples the still-blinking cursor.
    idle_parked: bool,
}

impl CursorBlinkState {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            on: true,
            next_toggle: None,
            last_keyboard_activity: None,
            idle_parked: false,
        }
    }

    /// Record physical-key or meaningful IME activity. A blinking, focused
    /// cursor becomes solid immediately and waits for the quiet hold before its
    /// first off edge. Steady application cursor shapes and unfocused windows
    /// remain authoritative and therefore keep no blink deadline.
    pub(super) fn note_activity(&mut self, now: Instant, blinking: bool, focused: bool) {
        if !blinking || !focused {
            self.park();
            return;
        }
        self.on = true;
        self.last_keyboard_activity = Some(now);
        self.next_toggle = Some(now + CURSOR_ACTIVITY_HOLD);
        self.idle_parked = false;
    }

    /// Update the blink phase for `now` and return whether the cursor is
    /// currently visible (on-phase). Solid-on (and deadline cleared) whenever
    /// the cursor is not blinking, the window is unfocused, or keyboard idle
    /// has reached the bounded stop boundary.
    pub(super) fn poll(&mut self, now: Instant, blinking: bool, focused: bool) -> bool {
        if !blinking || !focused {
            self.park();
            return true;
        }
        if self.idle_parked {
            return true;
        }
        if self.last_keyboard_activity.is_none() {
            // The first visible sample starts the same quiet hold used after
            // input. This preserves a solid cursor on startup/activation rather
            // than immediately arming an arbitrary blink edge.
            self.note_activity(now, blinking, focused);
        }
        if self
            .last_keyboard_activity
            .is_some_and(|activity| now >= activity + CURSOR_BLINK_STOP_AFTER)
        {
            self.park_after_idle();
            return true;
        }
        match self.next_toggle {
            Some(deadline) if now >= deadline => {
                self.on = !self.on;
                self.next_toggle = Some(now + self.interval);
            }
            Some(_) | None => {}
        }
        self.on
    }

    /// The next state boundary, either the next blink edge or the long-idle
    /// solid-on stop. The latter prevents an off-phase cursor from remaining
    /// hidden after the wake scheduler otherwise becomes idle.
    pub(super) fn deadline(&self) -> Option<Instant> {
        [
            self.next_toggle,
            self.last_keyboard_activity
                .map(|activity| activity + CURSOR_BLINK_STOP_AFTER),
        ]
        .into_iter()
        .flatten()
        .min()
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
        self.last_keyboard_activity = None;
        self.idle_parked = false;
    }

    /// Park after the keyboard-idle budget elapses. Unlike [`Self::park`],
    /// retain that provenance so the next render sample does not mistake the
    /// settled state for a newly activated blinking cursor and re-arm it.
    fn park_after_idle(&mut self) {
        self.on = true;
        self.next_toggle = None;
        self.last_keyboard_activity = None;
        self.idle_parked = true;
    }

    /// Whether a scheduled toggle is due at `now` (the loop should rebuild and
    /// redraw so the phase flips).
    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! F4-P3 rail auto-hide state machine (ODP-4) — pure, GPU-free, clock-injected.
//!
//! When `tab_rail_autohide` is on and the tab chrome is a side rail, the rail
//! reserves **no** content columns (one reflow at toggle time) and draws only
//! as a **floating overlay** at its window edge when revealed — content never
//! reflows on reveal/hide (the survey's load-bearing rule: reveal is an overlay,
//! hide has a generous grace, show is near-instant).
//!
//! This module owns only the *timing state machine*; the App owns the geometry
//! (which pixels are the reveal edge / band) and the render (building the
//! overlay strip). The state machine is driven by three inputs:
//! - **pointer** — [`RailAutohide::on_pointer`] with two booleans the App
//!   derives from the live pointer: `in_edge` (the pointer is in — or its motion
//!   *segment* crossed — the trigger zone within `TAB_RAIL_REVEAL_PX` of the
//!   window edge; motion-aware so a fast approach that jumps over a static point
//!   zone still arms) and `in_band` (a point test: the pointer is *now* anywhere
//!   from the window edge through the revealed band to the seam — the
//!   *keep-alive* region, which by construction contains the edge zone).
//! - **keyboard flash** — [`RailAutohide::flash`] reveals the rail for
//!   [`FLASH`] after a tab-switch / new-tab / close action (ODP-4 SHOULD), so a
//!   chord's effect is confirmed even with the pointer away from the edge.
//! - **suspend** — [`RailAutohide::set_suspend`] holds the hide timer while a
//!   rail-anchored context menu / drag is active (the popup-tracking rule).
//!
//! Timers are advanced by [`RailAutohide::poll`] from the event loop's
//! about-to-wait maintenance; [`RailAutohide::wake_deadline`] feeds the shared
//! `WaitUntil` scheduler so a debounce/grace/flash boundary wakes the loop with
//! no busy-poll.

use std::time::{Duration, Instant};

/// Show debounce: a short confirm window between the first trigger sample and
/// the reveal, so a single incidental crossing (a window drag sweeping the edge)
/// does not summon the rail. Kept small — a live pointer trace showed the reveal
/// read as "won't open": a longer dwell, combined with the old abort-on-any-out-
/// of-band sample, killed deliberate fast approaches mid-debounce. The reveal now
/// arms on the motion-aware trigger (the pointer *segment* crossing the edge
/// zone, not just a point landing in it) and holds through the confirm as long as
/// the segment stays on the rail side, so 30 ms is enough to filter a lone stray
/// sample without making an approach feel sluggish (was 80 ms → 120 ms before).
pub(super) const SHOW_DEBOUNCE: Duration = Duration::from_millis(30);
/// Hide grace: after the pointer leaves the revealed band, the rail stays up
/// this long before hiding (middle of the 300–1000ms convention range, errs
/// toward snappy).
pub(super) const HIDE_GRACE: Duration = Duration::from_millis(600);
/// Keyboard-action flash duration (Zen's new-tab flash): a tab chord reveals the
/// rail this long regardless of pointer position.
pub(super) const FLASH: Duration = Duration::from_millis(1000);
/// Panel-wash alpha floor while revealed (ODP-4 / ODP-6): the overlay sits over
/// live terminal text and must be readable, so its wash is `max(p, 0.85)` —
/// near-opaque, unlike the pinned surface's translucent `p`.
pub(super) const REVEAL_WASH_ALPHA: f32 = 0.85;

/// Pointer-driven reveal phase. Orthogonal to the keyboard `flash` latch: the
/// rail is visible when EITHER the pointer phase is revealed-ish OR a flash is
/// in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not revealed, not drawn.
    Hidden,
    /// Pointer entered the edge zone at this instant; promotes to `Revealed`
    /// after [`SHOW_DEBOUNCE`], or falls back to `Hidden` if the pointer leaves
    /// first. Not drawn yet.
    Revealing(Instant),
    /// Fully revealed and drawn.
    Revealed,
    /// Pointer left the band at this instant; still drawn, hides after
    /// [`HIDE_GRACE`] unless the pointer returns or the hide is suspended.
    HideGrace(Instant),
}

/// The rail auto-hide reveal/hide state machine (ODP-4 §5). Pure and
/// clock-injected: every transition takes an explicit `now`, so the whole thing
/// is unit-testable without a window or a real clock.
#[derive(Debug, Clone)]
pub(super) struct RailAutohide {
    phase: Phase,
    /// Last pointer sample: the pointer is in — or its motion segment crossed —
    /// the edge trigger zone. The App derives this motion-aware so a fast
    /// approach that jumps over a static point zone still arms (see
    /// `reveal_edge_segment_crosses`).
    in_edge: bool,
    /// Last pointer sample: within the keep-alive band (⊇ the edge zone).
    in_band: bool,
    /// Keyboard-flash reveal expiry, or `None` when no flash is in flight.
    flash_until: Option<Instant>,
    /// While `true` the hide-grace timer never fires (rail-anchored popup/drag).
    suspend_hide: bool,
    /// The visibility the machine last committed to, so a transition a
    /// same-instant before/after comparison cannot see — most notably a flash
    /// expiring exactly at its deadline wake — is still reported as a change (the
    /// frame that dropped the flash must repaint). Updated by every mutator.
    last_visible: bool,
}

impl Default for RailAutohide {
    fn default() -> Self {
        Self {
            phase: Phase::Hidden,
            in_edge: false,
            in_band: false,
            flash_until: None,
            suspend_hide: false,
            last_visible: false,
        }
    }
}

impl RailAutohide {
    /// Whether the rail overlay should be drawn (and hit-tested) at `now`:
    /// pointer-revealed (including the hide-grace tail) OR flashing. `Revealing`
    /// is deliberately NOT visible: an armed-but-not-yet-elapsed debounce that
    /// aborts must never have painted a frame (no half-flash on a skim past the
    /// edge).
    pub(super) fn is_visible(&self, now: Instant) -> bool {
        let flashing = self.flash_until.is_some_and(|until| now < until);
        flashing || matches!(self.phase, Phase::Revealed | Phase::HideGrace(_))
    }

    /// The current phase as a short static label, for the `ODYTTY_RAIL_TRACE`
    /// operator-runnable reveal trace (coordinates + phases only, never
    /// content). Not part of the state machine's logic.
    pub(super) fn phase_name(&self) -> &'static str {
        match self.phase {
            Phase::Hidden => "hidden",
            Phase::Revealing(_) => "revealing",
            Phase::Revealed => "revealed",
            Phase::HideGrace(_) => "hidegrace",
        }
    }

    /// Feed a fresh pointer sample. `in_edge` = the pointer is in — or its motion
    /// segment crossed — the reveal trigger zone at the window edge; `in_band` =
    /// pointer anywhere in the keep-alive region (edge zone ∪ revealed band).
    /// Returns `true` when visibility changed (the caller should redraw).
    pub(super) fn on_pointer(&mut self, in_edge: bool, in_band: bool, now: Instant) -> bool {
        self.in_edge = in_edge;
        self.in_band = in_band;
        self.advance(now);
        self.commit_visibility(now)
    }

    /// Advance the timers with the last pointer sample (no new pointer data).
    /// Called from the event loop's about-to-wait maintenance. Returns `true`
    /// when visibility changed so the caller can redraw.
    pub(super) fn poll(&mut self, now: Instant) -> bool {
        self.advance(now);
        self.commit_visibility(now)
    }

    /// Reveal the rail for [`FLASH`] after a keyboard tab action (ODP-4 SHOULD).
    /// Extends an in-flight flash rather than shortening it. Commits the (now
    /// visible) state so the following expiry poll sees the flip.
    pub(super) fn flash(&mut self, now: Instant) {
        let until = now + FLASH;
        self.flash_until = Some(match self.flash_until {
            Some(existing) if existing > until => existing,
            _ => until,
        });
        self.last_visible = self.is_visible(now);
    }

    /// Recompute visibility after a mutation, clear an expired flash, and report
    /// whether the committed visibility changed since the last mutator call.
    fn commit_visibility(&mut self, now: Instant) -> bool {
        if let Some(until) = self.flash_until
            && now >= until
        {
            self.flash_until = None;
        }
        let now_visible = self.is_visible(now);
        let changed = now_visible != self.last_visible;
        self.last_visible = now_visible;
        changed
    }

    /// Suspend/resume the hide-grace timer (rail-anchored context menu or drag).
    /// While suspended a `HideGrace` phase never elapses to `Hidden`.
    pub(super) fn set_suspend(&mut self, suspend: bool) {
        self.suspend_hide = suspend;
    }

    /// The next instant a timer boundary would change state, for the shared
    /// `WaitUntil` scheduler. `None` when nothing is pending (steady Hidden /
    /// Revealed with the pointer parked, no flash).
    pub(super) fn wake_deadline(&self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut merge = |candidate: Instant| {
            next = Some(next.map_or(candidate, |current: Instant| current.min(candidate)));
        };
        match self.phase {
            Phase::Revealing(since) => merge(since + SHOW_DEBOUNCE),
            Phase::HideGrace(since) if !self.suspend_hide => merge(since + HIDE_GRACE),
            _ => {}
        }
        if let Some(until) = self.flash_until {
            merge(until);
        }
        // A boundary already in the past still needs an immediate wake so the
        // next maintenance pass advances it; clamp to `now` so the scheduler
        // fires promptly rather than scheduling into the past.
        next.map(|deadline| deadline.max(now))
    }

    /// Test seam: force the fully-revealed phase so app-level wiring tests
    /// (overlay geometry / hit-routing) can assert against a visible rail without
    /// simulating the debounce clock (which the pure tests above already cover).
    #[cfg(test)]
    pub(super) fn force_revealed(&mut self) {
        self.phase = Phase::Revealed;
        self.in_band = true;
        self.last_visible = true;
    }

    /// Single-pass phase transition using the stored pointer latches. Each call
    /// makes at most one timer/edge transition; the follow-up boundary is
    /// re-scheduled via [`Self::wake_deadline`], so this never loops.
    fn advance(&mut self, now: Instant) {
        self.phase = match self.phase {
            Phase::Hidden => {
                if self.in_edge {
                    Phase::Revealing(now)
                } else {
                    Phase::Hidden
                }
            }
            Phase::Revealing(since) => {
                if !self.in_edge && !self.in_band {
                    // Pointer decisively away before the debounce elapsed —
                    // neither still sweeping the trigger edge (motion-aware
                    // `in_edge`) nor over the keep-alive band — so the arm never
                    // flashed. A single fast follow-through that overshoots past
                    // the seam (the live trace's 7.9px → 214px in 74 ms) keeps
                    // `in_edge` set via segment-crossing, so a deliberate quick
                    // approach is no longer aborted mid-confirm.
                    Phase::Hidden
                } else if now.saturating_duration_since(since) >= SHOW_DEBOUNCE {
                    Phase::Revealed
                } else {
                    Phase::Revealing(since)
                }
            }
            Phase::Revealed => {
                if self.in_band {
                    Phase::Revealed
                } else {
                    Phase::HideGrace(now)
                }
            }
            Phase::HideGrace(since) => {
                if self.in_band {
                    Phase::Revealed
                } else if !self.suspend_hide && now.saturating_duration_since(since) >= HIDE_GRACE {
                    Phase::Hidden
                } else {
                    Phase::HideGrace(since)
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn default_is_hidden_and_invisible() {
        let m = RailAutohide::default();
        assert!(!m.is_visible(t0()));
        assert_eq!(m.wake_deadline(t0()), None);
    }

    #[test]
    fn pointer_at_edge_debounces_then_reveals() {
        let start = t0();
        let mut m = RailAutohide::default();
        // Enter the edge zone: armed, but not visible until the debounce.
        let changed = m.on_pointer(true, true, start);
        assert!(!changed, "still invisible during the debounce");
        assert!(!m.is_visible(start));
        // Before the debounce elapses: still hidden.
        let mid = start + SHOW_DEBOUNCE / 2;
        assert!(!m.poll(mid));
        assert!(!m.is_visible(mid));
        // After the debounce: revealed, and the transition flips visibility.
        let after = start + SHOW_DEBOUNCE;
        assert!(m.poll(after), "debounce boundary flips to visible");
        assert!(m.is_visible(after));
    }

    #[test]
    fn fling_past_edge_before_debounce_never_flashes() {
        let start = t0();
        let mut m = RailAutohide::default();
        m.on_pointer(true, true, start); // enter edge
        // Pointer leaves the whole keep-alive region before the debounce.
        let leave = start + SHOW_DEBOUNCE / 3;
        assert!(!m.on_pointer(false, false, leave));
        // Even long after, it never became visible.
        let later = start + FLASH;
        assert!(!m.poll(later));
        assert!(!m.is_visible(later));
    }

    #[test]
    fn fast_follow_through_past_the_seam_does_not_abort_the_arm() {
        // Live-trace regression (the 7.9px → 214px in 74 ms abort): an arm at the
        // edge followed within the debounce by one fast sample that overshoots
        // past the seam — `in_edge` still set by segment-crossing, but no longer
        // `in_band` — must NOT abort. It promotes at the debounce. The old
        // `!in_band` abort dropped it, so a deliberate quick approach "wouldn't
        // open".
        let start = t0();
        let mut m = RailAutohide::default();
        m.on_pointer(true, true, start); // arm at the edge
        // Fast follow-through: the motion segment still crosses the edge (in_edge
        // stays true), but the current point is past the band (in_band false).
        let follow = start + SHOW_DEBOUNCE / 3;
        assert!(
            !m.on_pointer(true, false, follow),
            "no flip yet — still confirming, not aborted"
        );
        // At the debounce boundary it promotes rather than aborting.
        let revealed = start + SHOW_DEBOUNCE;
        assert!(m.poll(revealed), "confirm elapses → revealed");
        assert!(m.is_visible(revealed));
    }

    #[test]
    fn leaving_the_band_hides_after_grace() {
        let start = t0();
        let mut m = RailAutohide::default();
        // Reveal.
        m.on_pointer(true, true, start);
        let revealed = start + SHOW_DEBOUNCE;
        m.poll(revealed);
        assert!(m.is_visible(revealed));
        // Pointer leaves the band → hide grace starts, still visible.
        assert!(!m.on_pointer(false, false, revealed));
        assert!(m.is_visible(revealed), "visible through the grace window");
        // Before the grace elapses: still visible.
        let mid = revealed + HIDE_GRACE / 2;
        assert!(!m.poll(mid));
        assert!(m.is_visible(mid));
        // After the grace: hidden.
        let gone = revealed + HIDE_GRACE;
        assert!(m.poll(gone), "grace boundary flips to hidden");
        assert!(!m.is_visible(gone));
    }

    #[test]
    fn returning_to_band_during_grace_cancels_hide() {
        let start = t0();
        let mut m = RailAutohide::default();
        m.on_pointer(true, true, start);
        let revealed = start + SHOW_DEBOUNCE;
        m.poll(revealed);
        m.on_pointer(false, false, revealed); // start grace
        // Pointer returns into the band before the grace elapses.
        let back = revealed + HIDE_GRACE / 2;
        assert!(!m.on_pointer(false, true, back), "still visible, no flip");
        assert!(m.is_visible(back));
        // The old grace deadline no longer applies; it stays revealed.
        let past_old_grace = revealed + HIDE_GRACE + Duration::from_millis(1);
        assert!(!m.poll(past_old_grace));
        assert!(m.is_visible(past_old_grace));
    }

    #[test]
    fn suspend_holds_the_rail_through_the_grace() {
        let start = t0();
        let mut m = RailAutohide::default();
        m.on_pointer(true, true, start);
        let revealed = start + SHOW_DEBOUNCE;
        m.poll(revealed);
        m.on_pointer(false, false, revealed); // grace
        m.set_suspend(true);
        // Well past the grace, still visible while suspended.
        let past = revealed + HIDE_GRACE * 3;
        assert!(!m.poll(past));
        assert!(m.is_visible(past));
        assert_eq!(m.wake_deadline(past), None, "no hide wake while suspended");
        // Resume → the next poll hides it (grace already elapsed).
        m.set_suspend(false);
        assert!(m.poll(past));
        assert!(!m.is_visible(past));
    }

    #[test]
    fn flash_reveals_without_the_pointer() {
        let start = t0();
        let mut m = RailAutohide::default();
        m.flash(start);
        assert!(m.is_visible(start), "flash reveals immediately");
        // Still visible just before expiry, gone at expiry.
        let almost = start + FLASH - Duration::from_millis(1);
        assert!(m.is_visible(almost));
        let expired = start + FLASH;
        assert!(m.poll(expired));
        assert!(!m.is_visible(expired));
    }

    #[test]
    fn flash_extends_but_never_shortens() {
        let start = t0();
        let mut m = RailAutohide::default();
        m.flash(start);
        // A later flash pushes the expiry out.
        m.flash(start + Duration::from_millis(500));
        let old_expiry = start + FLASH;
        assert!(m.is_visible(old_expiry), "extended past the first expiry");
        // An earlier-expiring flash does not pull the expiry back in.
        m.flash(start); // would expire at start+FLASH, earlier than current
        assert!(m.is_visible(old_expiry));
    }

    #[test]
    fn wake_deadline_tracks_the_pending_boundary() {
        let start = t0();
        let mut m = RailAutohide::default();
        // Revealing → wake at the show boundary.
        m.on_pointer(true, true, start);
        assert_eq!(m.wake_deadline(start), Some(start + SHOW_DEBOUNCE));
        // Revealed with the pointer parked → no wake.
        m.poll(start + SHOW_DEBOUNCE);
        assert_eq!(m.wake_deadline(start + SHOW_DEBOUNCE), None);
        // Grace → wake at the hide boundary.
        let r = start + SHOW_DEBOUNCE;
        m.on_pointer(false, false, r);
        assert_eq!(m.wake_deadline(r), Some(r + HIDE_GRACE));
    }
}

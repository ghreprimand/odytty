// SPDX-License-Identifier: GPL-3.0-only
//! UX-A (Phase 11): click-to-open discoverability for interactive paths.
//!
//! The hand cursor appears on plain hover over a resolved path, but OPENING
//! requires a modifier+click — Ctrl on Linux, Cmd on macOS
//! (`hyperlink_action_allowed`). A user sees the hand,
//! left-clicks, gets a text selection, and thinks nothing happened — the cursor
//! lies and there is no other signal. This module adds two teaching affordances,
//! both strictly INSIDE the `interactive_paths` master gate so the default
//! (feature-off) frame is byte-identical:
//!
//! 1. **Armed underline** — when the open modifier is held (Ctrl on Linux, Cmd
//!    on macOS) while hovering a resolved path, the span is underlined (the "now
//!    it will open" signal). Presentation-only; painted onto the snapshot cells
//!    like the selection/search highlights.
//! 2. **Click hint** — a transient bottom-left "Ctrl+click to open" (macOS:
//!    "Cmd+click to open") message that
//!    fires only after ≥2 plain mis-clicks on a path land within a short window
//!    (the "I clicked, nothing happened, let me try again" signal). It reuses the
//!    [`super::open_notice::OpenNotice`] *pattern* (transient `raised_at` clock,
//!    non-blocking, byte-identical-when-absent painter + cache signature) with a
//!    SEPARATE field and its own bottom-left paint position — it does NOT overload
//!    the full-width top failure banner.
//!
//! All feel-constants are named so they are easy to tune during the dev-build
//! hands-on exercise; they have no portable-correct value.

use std::time::{Duration, Instant};

use crate::core::{Attrs, Cell, Color, Snapshot};

use super::super::context_menu_ui::SHELL_INTEGRATION_DISABLED_HINT;
use super::super::render_helpers::open_modifier_held;
use super::{App, OverlayFragment};

/// How close together the two plain mis-clicks must land to count as the
/// "I clicked, nothing happened, let me try again" confusion signal. A single
/// isolated mis-click never raises the hint.
pub(in crate::native) const CLICK_HINT_MISCLICK_WINDOW: Duration = Duration::from_millis(1500);

/// How long the bottom-left hint stays on screen before it auto-expires. Long
/// enough to read a short line, short enough to not linger.
pub(in crate::native) const CLICK_HINT_DURATION: Duration = Duration::from_millis(3000);

/// Extra suppression AFTER the hint clears before another hint can raise, so a
/// burst of held / rapid mis-clicks does not restack or flicker the hint. The
/// re-arm point is `raised_at + CLICK_HINT_DURATION + CLICK_HINT_COOLDOWN`.
pub(in crate::native) const CLICK_HINT_COOLDOWN: Duration = Duration::from_millis(3000);

/// The teaching text (Linux). Kept short so it fits a few bottom-left cells.
pub(in crate::native) const CLICK_HINT_TEXT: &str = " Ctrl+click to open ";

/// NF17: the honest text for a select+Delete no-op whose cause is unavailable
/// geometry — NOT missing shell integration. Raised when the input region
/// exists (its mark is present) but its certainty can't back a real buffer edit:
/// a stale/hard-newline `Unknown` region, a multi-row `RightEdgeUnknown` region
/// (fish/bash/PowerShell mid-edit without an exact edge report), or a
/// decoration-only span. Telling the user to "enable shell integration" there
/// is wrong — integration is already active — so this variant says plainly that
/// the selection can't be edited, without shell-specific jargon. Kept short to
/// fit the bottom-left chip.
pub(in crate::native) const SELECTION_GEOMETRY_HINT: &str = "Selection can't be edited here";

/// The teaching text on macOS, where the open modifier is Cmd, not Ctrl (Ctrl
/// is consumed by the OS as a secondary-click). ASCII "Cmd" is used rather than
/// the ⌘ glyph to stay within the safe glyph-atlas character set.
pub(in crate::native) const CLICK_HINT_TEXT_MACOS: &str = " Cmd+click to open ";

/// Resolve the teaching text for `os`: macOS → [`CLICK_HINT_TEXT_MACOS`]
/// ("Cmd+click"), everything else → [`CLICK_HINT_TEXT`] ("Ctrl+click",
/// byte-for-byte unchanged on Linux). Windows uses Ctrl+click, the same modifier
/// as Linux, so it shares [`CLICK_HINT_TEXT`]. Production passes
/// [`OpenerOs::host`].
pub(in crate::native) fn click_hint_text(os: super::platform_opener::OpenerOs) -> &'static str {
    match os {
        super::platform_opener::OpenerOs::Macos => CLICK_HINT_TEXT_MACOS,
        super::platform_opener::OpenerOs::Linux | super::platform_opener::OpenerOs::Windows => {
            CLICK_HINT_TEXT
        }
    }
}

/// How many times the hint may appear in a single launch before it gives up for
/// the rest of the session. A teaching affordance should teach a few times and
/// then stop nagging — once the user has plausibly seen "Ctrl+click to open"
/// this many times, further plain mis-clicks no longer raise it (until the next
/// launch). Reset per launch because the state is in-memory only.
pub(in crate::native) const CLICK_HINT_MAX_SHOWS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum ClickHintMessage {
    OpenModifier,
    ShellIntegration,
    /// NF17: select+Delete no-op caused by unavailable geometry (not missing
    /// integration). Paints [`SELECTION_GEOMETRY_HINT`].
    SelectionGeometry,
}

impl ClickHintMessage {
    fn text(self, os: super::platform_opener::OpenerOs) -> &'static str {
        match self {
            Self::OpenModifier => click_hint_text(os),
            Self::ShellIntegration => SHELL_INTEGRATION_DISABLED_HINT,
            Self::SelectionGeometry => SELECTION_GEOMETRY_HINT,
        }
    }
}

/// In-memory, per-launch click-hint state (NOT persisted). Tracks the active
/// transient hint plus the mis-click bookkeeping that decides when to raise it.
/// Pure and platform-independent so the trigger logic is unit-tested directly.
#[derive(Debug, Default, Clone)]
pub(in crate::native) struct ClickHintState {
    /// When the visible hint was raised, or `None` when not shown. Drives both
    /// the painter early-out and the auto-expiry clock.
    shown_at: Option<Instant>,
    /// The previous unpaired plain mis-click, awaiting a partner within
    /// [`CLICK_HINT_MISCLICK_WINDOW`]. Dropped once it goes stale.
    last_misclick: Option<Instant>,
    /// Suppress raising a new hint until this instant (re-arm point). `None`
    /// before the first hint ever fires.
    cooldown_until: Option<Instant>,
    /// How many times the hint has been raised this launch. Once it reaches
    /// [`CLICK_HINT_MAX_SHOWS`] the hint retires for the session so it stops
    /// nagging a user who keeps plain-clicking. In-memory only (resets per
    /// launch).
    times_shown: u32,
    /// Which short teaching message the visible chip paints.
    message: Option<ClickHintMessage>,
}

impl ClickHintState {
    /// Register a plain mis-click (a left-click on a resolved path that did NOT
    /// open because the Ctrl gate failed) at `now`. Returns `true` iff this
    /// raised the hint, so the caller can request a redraw.
    ///
    /// Rules: a hint already shown, an active cooldown, or a reached per-launch
    /// show cap swallows the click (no restack); otherwise the first click is
    /// recorded and a SECOND click within [`CLICK_HINT_MISCLICK_WINDOW`] raises
    /// the hint, arms the cooldown, and counts toward the cap.
    pub(in crate::native) fn note_misclick(&mut self, now: Instant) -> bool {
        // Already visible — additional clicks never restack it.
        if self.shown_at.is_some() {
            return false;
        }
        // Taught enough this launch — retire the hint so it stops nagging.
        if self.times_shown >= CLICK_HINT_MAX_SHOWS {
            return false;
        }
        // Cooling down after a recent hint — suppress to avoid flicker.
        if let Some(until) = self.cooldown_until
            && now < until
        {
            return false;
        }
        let paired = self
            .last_misclick
            .is_some_and(|prev| now.saturating_duration_since(prev) <= CLICK_HINT_MISCLICK_WINDOW);
        if paired {
            self.shown_at = Some(now);
            self.message = Some(ClickHintMessage::OpenModifier);
            self.last_misclick = None;
            self.cooldown_until = Some(now + CLICK_HINT_DURATION + CLICK_HINT_COOLDOWN);
            self.times_shown = self.times_shown.saturating_add(1);
            true
        } else {
            // First (or stale-partner) click: record and wait for a partner.
            self.last_misclick = Some(now);
            false
        }
    }

    /// Frame-clock update: clear an expired hint and drop a stale unpaired
    /// mis-click whose partner never arrived. No-op on the idle path.
    pub(in crate::native) fn tick(&mut self, now: Instant) {
        if let Some(shown) = self.shown_at
            && now.saturating_duration_since(shown) >= CLICK_HINT_DURATION
        {
            self.shown_at = None;
            self.message = None;
        }
        if let Some(prev) = self.last_misclick
            && now.saturating_duration_since(prev) > CLICK_HINT_MISCLICK_WINDOW
        {
            self.last_misclick = None;
        }
    }

    /// Whether the bottom-left hint is currently visible.
    pub(in crate::native) fn is_shown(&self) -> bool {
        self.shown_at.is_some()
    }

    pub(in crate::native) fn shown_text(
        &self,
        os: super::platform_opener::OpenerOs,
    ) -> Option<&'static str> {
        self.shown_at?;
        self.message.map(|message| message.text(os))
    }

    pub(in crate::native) fn show_shell_integration_hint(&mut self, now: Instant) -> bool {
        if self.shown_at.is_some() {
            return false;
        }
        self.shown_at = Some(now);
        self.message = Some(ClickHintMessage::ShellIntegration);
        self.last_misclick = None;
        true
    }

    /// NF17: raise the geometry-unavailable hint (integration is on, but the
    /// selection's geometry can't back an edit). Sibling of
    /// [`Self::show_shell_integration_hint`]; same not-restack guard.
    pub(in crate::native) fn show_selection_geometry_hint(&mut self, now: Instant) -> bool {
        if self.shown_at.is_some() {
            return false;
        }
        self.shown_at = Some(now);
        self.message = Some(ClickHintMessage::SelectionGeometry);
        self.last_misclick = None;
        true
    }

    /// The auto-expiry wake instant while the hint is visible, else `None` so an
    /// at-rest terminal schedules no extra wakes.
    pub(in crate::native) fn deadline(&self) -> Option<Instant> {
        self.shown_at.map(|shown| shown + CLICK_HINT_DURATION)
    }
}

/// The cell span (visible coordinates) of a hovered, resolved interactive path,
/// captured alongside `hovered_path` so the armed underline can decorate exactly
/// those cells without re-scanning the row at paint time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::native) struct HoverPathCells {
    /// Visible row index of the hovered span.
    pub(in crate::native) row: usize,
    /// First column of the span (inclusive).
    pub(in crate::native) start: usize,
    /// One past the last column of the span (exclusive).
    pub(in crate::native) end: usize,
}

impl App {
    /// The armed-underline span, or `None` unless the platform open modifier is
    /// held (Ctrl on Linux, Cmd on macOS — same per-OS resolution as the open
    /// gesture, [`open_modifier_held`]) and a hovered openable target exists: a
    /// resolved interactive path (gated on `interactive_paths`) or a bare URL
    /// (gated on `interactive_urls`). Both the painter and the cache signature
    /// read this, so the underline appears/disappears coherently when the open
    /// modifier toggles or the hovered span moves. A path and a URL can never be
    /// hovered at the same cell, so the path span is preferred when both somehow
    /// resolve.
    pub(in crate::native) fn armed_path_underline_cells(&self) -> Option<HoverPathCells> {
        if !open_modifier_held(
            self.modifiers,
            self.super_key,
            super::platform_opener::OpenerOs::host(),
        ) {
            return None;
        }
        if self.settings.interactive_paths && self.hovered_path_cells.is_some() {
            return self.hovered_path_cells;
        }
        if self.settings.interactive_urls && self.hovered_url_cells.is_some() {
            return self.hovered_url_cells;
        }
        None
    }

    /// Cache fragment for the armed underline: the span coords while armed, else
    /// `Inert`. `Inert` on the default / plain-hover / feature-off path keeps the
    /// composite constant; the coords change the key so a moving armed hover
    /// reclassifies to a Full rebuild and the underline tracks the pointer.
    pub(in crate::native) fn armed_path_overlay_signature(&self) -> OverlayFragment {
        match self.armed_path_underline_cells() {
            Some(cells) => OverlayFragment::ArmedPath {
                row: cells.row,
                start: cells.start,
                end: cells.end,
            },
            None => OverlayFragment::Inert,
        }
    }

    /// Underline the Ctrl+hovered path span (presentation-only). No-op unless
    /// armed, so plain hover and feature-off frames are byte-identical. Sets only
    /// the underline attribute on the span's existing cells; the glyphs, colors,
    /// and the rest of the row are untouched.
    pub(in crate::native) fn paint_armed_path_underline_cells(&self, snapshot: &mut Snapshot) {
        let Some(cells) = self.armed_path_underline_cells() else {
            return;
        };
        let columns = snapshot.dimensions.columns;
        if columns == 0 || cells.row >= snapshot.dimensions.rows {
            return;
        }
        let base = cells.row * columns;
        for column in cells.start..cells.end.min(columns) {
            if let Some(cell) = snapshot.cells.get_mut(base + column) {
                cell.attrs.set_underline(true);
            }
        }
    }

    /// Cache fragment for the bottom-left hint: `ClickHint { shown: true }` while
    /// visible, else `Inert` (the default / not-shown path), so a no-hint frame
    /// is byte-identical.
    pub(in crate::native) fn click_hint_overlay_signature(&self) -> OverlayFragment {
        if self.click_hint.is_shown() {
            OverlayFragment::ClickHint { shown: true }
        } else {
            OverlayFragment::Inert
        }
    }

    /// The hint's auto-expiry wake, folded into the animation-deadline
    /// aggregator. `None` when no hint is in flight.
    pub(in crate::native) fn click_hint_deadline(&self) -> Option<Instant> {
        self.click_hint.deadline()
    }

    /// Clear the hint once it has outlived [`CLICK_HINT_DURATION`] and drop a
    /// stale unpaired mis-click. Called from the frame clock alongside
    /// [`App::update_open_notice`]. No-op on the idle path.
    pub(in crate::native) fn update_click_hint(&mut self, now: Instant) {
        self.click_hint.tick(now);
    }

    pub(in crate::native) fn show_shell_integration_hint(&mut self, now: Instant) {
        if self.click_hint.show_shell_integration_hint(now) {
            self.request_selection_redraw();
        }
    }

    /// NF17: raise the geometry-unavailable select+Delete hint. Used by the
    /// `NoOpWithHint` arm (mark present, geometry can't back the edit); the
    /// mark-missing path keeps [`Self::show_shell_integration_hint`].
    pub(in crate::native) fn show_selection_geometry_hint(&mut self, now: Instant) {
        if self.click_hint.show_selection_geometry_hint(now) {
            self.request_selection_redraw();
        }
    }

    /// Paint the hint as a short inverse-video chip in the bottom-LEFT of the
    /// grid (distinct from the failure banner's full-width TOP bar). No-op when
    /// the hint is not shown, so the frame is byte-identical on the default path.
    /// Overwrites only the few bottom-left cells the text occupies for the hint's
    /// lifetime; the rest of the row and the content underneath are untouched in
    /// the model and reappear when the hint clears.
    pub(in crate::native) fn paint_click_hint_cells(&self, snapshot: &mut Snapshot) {
        if !self.click_hint.is_shown() {
            return;
        }
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        if columns == 0 || rows == 0 {
            return;
        }
        let attrs = hint_attrs();
        let base = (rows - 1) * columns;
        let mut x = 0usize;
        let text = self
            .click_hint
            .shown_text(super::platform_opener::OpenerOs::host())
            .unwrap_or_else(|| click_hint_text(super::platform_opener::OpenerOs::host()));
        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }
            if x >= columns {
                break;
            }
            snapshot.cells[base + x] = Cell::new(ch, attrs);
            x += 1;
        }
    }
}

/// Hint chip attributes: an informational inverse chip, distinct from the
/// failure banner's red. Indexed colors keep it theme-portable (the active
/// palette supplies the RGB).
fn hint_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    // Blue background, bright-white foreground via indexed colors so it reads as
    // an informational teaching chip rather than the red failure banner.
    attrs.foreground = Color::Indexed(15);
    attrs.background = Color::Indexed(4);
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn two_misclicks_within_window_raise_the_hint() {
        let mut state = ClickHintState::default();
        let start = t0();
        assert!(
            !state.note_misclick(start),
            "a single isolated mis-click does not raise"
        );
        assert!(!state.is_shown());
        // Second within the window → raises.
        assert!(state.note_misclick(start + Duration::from_millis(400)));
        assert!(state.is_shown());
    }

    #[test]
    fn single_misclick_never_raises() {
        let mut state = ClickHintState::default();
        let start = t0();
        assert!(!state.note_misclick(start));
        // A partner that arrives too late (outside the window) is itself just a
        // fresh first click — still no hint.
        let late = start + CLICK_HINT_MISCLICK_WINDOW + Duration::from_millis(1);
        assert!(!state.note_misclick(late));
        assert!(!state.is_shown());
    }

    #[test]
    fn burst_after_raise_does_not_restack() {
        let mut state = ClickHintState::default();
        let start = t0();
        assert!(!state.note_misclick(start));
        assert!(state.note_misclick(start + Duration::from_millis(100)));
        let raised_at = state.shown_at.expect("shown");
        // Rapid held clicks while shown are swallowed — no restack, clock unmoved.
        assert!(!state.note_misclick(start + Duration::from_millis(150)));
        assert!(!state.note_misclick(start + Duration::from_millis(200)));
        assert_eq!(state.shown_at, Some(raised_at), "raise clock did not move");
    }

    #[test]
    fn cooldown_blocks_re_raise_until_after_the_hint_clears() {
        let mut state = ClickHintState::default();
        // Raise at exactly `raise` (a 0ms-apart pair is within the window).
        let raise = t0();
        assert!(!state.note_misclick(raise));
        assert!(state.note_misclick(raise));
        assert!(state.is_shown());
        // The hint clears `CLICK_HINT_DURATION` after it was raised.
        let after_clear = raise + CLICK_HINT_DURATION + Duration::from_millis(1);
        state.tick(after_clear);
        assert!(!state.is_shown(), "hint cleared after its duration");
        // But the cooldown still suppresses a fresh pair right after clearing
        // (re-arm is at raise + DURATION + COOLDOWN).
        assert!(!state.note_misclick(after_clear));
        assert!(
            !state.note_misclick(after_clear + Duration::from_millis(50)),
            "cooldown blocks the re-raise"
        );
        assert!(!state.is_shown());
        // Past the re-arm point a fresh pair raises again.
        let rearmed = raise + CLICK_HINT_DURATION + CLICK_HINT_COOLDOWN + Duration::from_millis(1);
        assert!(!state.note_misclick(rearmed));
        assert!(state.note_misclick(rearmed + Duration::from_millis(50)));
        assert!(state.is_shown());
    }

    #[test]
    fn hint_retires_after_the_per_launch_show_cap() {
        let mut state = ClickHintState::default();
        let mut now = t0();
        // Raise the hint exactly CLICK_HINT_MAX_SHOWS times, each a fresh pair
        // past the prior re-arm point.
        for _ in 0..CLICK_HINT_MAX_SHOWS {
            assert!(!state.note_misclick(now), "first of the pair records");
            assert!(state.note_misclick(now), "second of the pair raises");
            assert!(state.is_shown());
            // Clear it and advance past the cooldown re-arm point.
            now += CLICK_HINT_DURATION + CLICK_HINT_COOLDOWN + Duration::from_millis(1);
            state.tick(now);
            assert!(!state.is_shown());
        }
        // The cap is now reached: no further pair ever raises it this launch,
        // even well past every cooldown.
        assert!(!state.note_misclick(now));
        assert!(
            !state.note_misclick(now),
            "past the per-launch cap, the hint retires and stops nagging"
        );
        assert!(!state.is_shown());
    }

    #[test]
    fn stale_unpaired_misclick_is_dropped_by_tick() {
        let mut state = ClickHintState::default();
        let start = t0();
        assert!(!state.note_misclick(start));
        // Tick past the window → the lone click is forgotten, so a later click is
        // again a fresh first (not a partner).
        state.tick(start + CLICK_HINT_MISCLICK_WINDOW + Duration::from_millis(1));
        let later = start + CLICK_HINT_MISCLICK_WINDOW + Duration::from_millis(2);
        assert!(
            !state.note_misclick(later),
            "the stale partner was dropped, so this is a fresh first click"
        );
        assert!(!state.is_shown());
    }

    #[test]
    fn hint_clears_after_duration() {
        let mut state = ClickHintState::default();
        let start = t0();
        assert!(!state.note_misclick(start));
        assert!(state.note_misclick(start + Duration::from_millis(100)));
        let deadline = state.deadline().expect("a deadline while shown");
        // Just before the deadline it is still shown.
        state.tick(deadline - Duration::from_millis(1));
        assert!(state.is_shown());
        // At/after the deadline it clears.
        state.tick(deadline);
        assert!(!state.is_shown());
        assert!(state.deadline().is_none());
    }

    #[test]
    fn feel_constants_are_conservative() {
        // Sanity bounds (tuned in the dev build).
        assert!(CLICK_HINT_MISCLICK_WINDOW >= Duration::from_millis(500));
        assert!(CLICK_HINT_MISCLICK_WINDOW <= Duration::from_secs(3));
        assert!(CLICK_HINT_DURATION >= Duration::from_secs(2));
        assert!(CLICK_HINT_DURATION <= Duration::from_secs(8));
    }

    #[test]
    fn hint_attrs_are_informational_not_the_failure_red() {
        let a = hint_attrs();
        // Distinct from the open-notice banner (bg Indexed(9) red).
        assert_eq!(a.background, Color::Indexed(4));
        assert_eq!(a.foreground, Color::Indexed(15));
    }
}

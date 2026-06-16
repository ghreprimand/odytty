// SPDX-License-Identifier: GPL-3.0-only
//! ID1 cursor blink-fade easing for the native app.
//!
//! Home for the cursor blink-fade easing feature (ID1). The Wave-15b foundation
//! landed only the two contributor stubs the [`App::cursor_render_params`] and
//! [`App::animation_deadline`] aggregators fold in; this module fills the live
//! easing body. The aggregators read the precomputed [`App::cursor_anim_alpha`]
//! / [`App::cursor_ease_deadline`] fields, which [`App::update_cursor_easing`]
//! refreshes once per rebuild from the injected `now` and blink phase.
//!
//! Off-path contract (`cursor_easing` defaults to `false`): the alpha is held at
//! `1.0` and no wake is armed, so the cursor renders byte-identically and an
//! idle terminal arms no extra wakeups. The blink off-phase still hard-hides the
//! cursor (the caller's existing `cursor_visible = false`), so easing never
//! double-hides.

use super::*;

/// Duration of the opacity ramp at each blink edge. Shorter than the blink
/// half-period ([`CURSOR_BLINK_INTERVAL`], ~530ms) so the cursor still reads as
/// a blink: it fades in over this window, holds, fades out, holds.
const CURSOR_EASE_FADE: Duration = Duration::from_millis(180);

/// Animation frame cadence while a fade is in flight. ~60fps; bounded because
/// the ramp settles after [`CURSOR_EASE_FADE`] and then arms no further easing
/// wake (the blink toggle deadline carries to the next edge).
const CURSOR_ANIM_FRAME: Duration = Duration::from_millis(16);

impl App {
    /// Alpha multiplier for the cursor quad color (ID1 easing).
    ///
    /// Polarity is the kill-shot: `1.0` = fully opaque (today's render), `0.0` =
    /// invisible. Returns the value [`App::update_cursor_easing`] precomputed for
    /// the current frame; `1.0` whenever easing is off, the cursor is steady, or
    /// the window is unfocused.
    pub(super) fn cursor_blink_alpha(&self) -> f32 {
        self.cursor_anim_alpha
    }

    /// Next wake instant while a blink-fade ramp is in flight, or `None` once it
    /// settles (the bounded-wake contract: the ramp ends and the blink toggle
    /// deadline — scheduled independently — carries to the next edge).
    pub(super) fn cursor_blink_fade_deadline(&self) -> Option<Instant> {
        self.cursor_ease_deadline
    }

    /// Recompute the eased cursor alpha + the next fade wake for `now`.
    ///
    /// Called once per rebuild after the blink poll, so the `&self` accessors
    /// above return a value consistent with this frame. `cursor_on` is the blink
    /// driver's current phase and `blinking` whether the active style requests a
    /// blink at all.
    ///
    /// Identity when off: with `cursor_easing == false` (the default), the
    /// cursor is steady, or the window unfocused, this pins `alpha = 1.0` and
    /// clears the deadline, so [`App::cursor_render_params`] stays at the
    /// identity and [`App::animation_deadline`] contributes nothing.
    pub(in crate::native) fn update_cursor_easing(
        &mut self,
        now: Instant,
        cursor_on: bool,
        blinking: bool,
    ) {
        if !self.settings.cursor_easing || !blinking || !self.focused {
            self.cursor_anim_alpha = 1.0;
            self.cursor_ease_deadline = None;
            // Resync the edge tracker so the first ramp after (re-)enabling
            // starts cleanly rather than from a stale phase.
            self.cursor_ease_phase_on = cursor_on;
            self.cursor_ease_toggle_at = Some(now);
            return;
        }
        // Record the instant of each on/off transition so the ramp is measured
        // from the edge, independent of the blink driver's internal clock.
        if self.cursor_ease_phase_on != cursor_on || self.cursor_ease_toggle_at.is_none() {
            self.cursor_ease_phase_on = cursor_on;
            self.cursor_ease_toggle_at = Some(now);
        }
        let toggled_at = self.cursor_ease_toggle_at.unwrap_or(now);
        let elapsed = now.saturating_duration_since(toggled_at).as_secs_f32();
        let progress = (elapsed / CURSOR_EASE_FADE.as_secs_f32()).clamp(0.0, 1.0);
        // Fade in (0 -> 1) when turning on, out (1 -> 0) when turning off. The
        // off-phase ramp keeps the cursor visible while alpha decays; the caller
        // skips the hard-hide while easing is on, so this never double-hides.
        self.cursor_anim_alpha = if cursor_on { progress } else { 1.0 - progress };
        self.cursor_ease_deadline = if progress < 1.0 {
            Some(now + CURSOR_ANIM_FRAME)
        } else {
            None
        };
    }
}

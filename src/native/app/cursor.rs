// SPDX-License-Identifier: GPL-3.0-only
//! ID1-easing cursor contributor stubs (Wave-15b foundation).
//!
//! Home for the cursor blink-fade easing feature (ID1). The foundation lands
//! only the two contributor stubs the [`App::cursor_render_params`] and
//! [`App::animation_deadline`] aggregators fold in; the live easing body fills
//! these same method bodies in Wave 16 without touching any other file.
//!
//! Off-path contract: both stubs return the identity (full opacity, no wake),
//! so the cursor renders byte-identically and an idle terminal arms no extra
//! wakeups.

use super::*;

impl App {
    /// Alpha multiplier for the cursor quad color (ID1-easing).
    ///
    /// Polarity is the kill-shot: `1.0` = fully opaque (today's render), `0.0` =
    /// invisible. The stub returns `1.0` so the foundation is byte-identical;
    /// the easing body will return a continuous blink-fade float in Wave 16.
    pub(super) fn cursor_blink_alpha(&self) -> f32 {
        1.0
    }

    /// Next wake instant while a blink-fade is in flight, or `None` at rest.
    ///
    /// The stub returns `None` so [`App::animation_deadline`] contributes no
    /// extra wakeups; the easing body will return `Some(next_tick)` while a
    /// fade is animating in Wave 16.
    pub(super) fn cursor_blink_fade_deadline(&self) -> Option<Instant> {
        None
    }
}

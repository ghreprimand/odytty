// SPDX-License-Identifier: GPL-3.0-only
//! Transient, non-blocking native status notices.
//!
//! v0.4.0 swallowed every `Command::spawn()` error at the single
//! [`super::interactive_paths::spawn_detached`] point, so a missing or broken
//! opener (`xdg-open`/`open`) was indistinguishable from "feature off" — the
//! user clicked and nothing happened, with no explanation. This surface fixes
//! that: when an open/reveal spawn fails, the App raises a short message that
//! paints as a one-row banner over the top of the grid and auto-expires. Routine
//! security status can reuse the same bounded surface with a neutral tone.
//!
//! Design constraints (from the Phase-2 ruling):
//! * NON-BLOCKING — it never captures focus, pauses the loop, or eats input;
//!   it is purely a painted row plus a wake deadline, modelled on the existing
//!   bell-flash overlay plumbing.
//! * OPEN FAILURES REMAIN FAILURE-ONLY - [`App::spawn_open_or_notice`] sets its
//!   notice from the `Err` arm only; the success path never touches it.
//! * BYTE-IDENTICAL WHEN ABSENT — both the painter ([`App::paint_open_notice_cells`])
//!   and the cache signature ([`App::open_notice_overlay_signature`]) early-out
//!   when `open_notice` is `None`, so a no-error / feature-off frame is
//!   unchanged from before this surface existed.

use std::time::{Duration, Instant};

use crate::core::{Attrs, Cell, Color, Snapshot};

use super::interactive_paths::spawn_detached;
use super::{App, OverlayFragment};

/// How long a notice stays on screen before it auto-expires. Long enough to
/// read a one-line failure message, short enough to not linger over the shell.
pub(in crate::native) const NOTICE_DURATION: Duration = Duration::from_millis(4000);

/// A raised notice: message, tone, and the instant used by the auto-expiry
/// clock. Presentation-only.
#[derive(Debug, Clone)]
pub(in crate::native) struct OpenNotice {
    message: String,
    raised_at: Instant,
    tone: NoticeTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeTone {
    Failure,
    Neutral,
}

impl OpenNotice {
    #[cfg(test)]
    pub(in crate::native) fn message_for_test(&self) -> &str {
        &self.message
    }
}

impl App {
    /// Spawn an open/reveal `argv` through the single argv-only spawn point and,
    /// on failure, raise a transient [`OpenNotice`] (P0-2). The success path is
    /// a pure spawn with NO notice — byte-identical to the old behaviour. A
    /// missing opener (`xdg-open`/`open` not found) is the common failure and is
    /// reported with the program name so the user knows what to install/repair.
    pub(in crate::native) fn spawn_open_or_notice(&mut self, argv: &[String]) {
        if let Err(error) = spawn_detached(argv) {
            let program = argv.first().map(String::as_str).unwrap_or("opener");
            let message = if error.kind() == std::io::ErrorKind::NotFound {
                format!("Couldn't open — '{program}' not found (is it installed?)")
            } else {
                format!("Couldn't open — {program} failed: {error}")
            };
            self.raise_open_notice(message);
        }
    }

    /// Raise a transient notice now. Replaces any in-flight notice (the newest
    /// failure is the relevant one) and requests a redraw so the banner appears
    /// without waiting for another event.
    pub(in crate::native) fn raise_open_notice(&mut self, message: String) {
        self.open_notice = Some(OpenNotice {
            message,
            raised_at: Instant::now(),
            tone: NoticeTone::Failure,
        });
        self.request_selection_redraw();
    }

    /// Raise a neutral transient status notice. The caller owns rate limiting;
    /// this keeps the established one-notice, one-expiry-wake bound.
    pub(in crate::native) fn raise_neutral_notice(&mut self, message: String) {
        self.open_notice = Some(OpenNotice {
            message,
            raised_at: Instant::now(),
            tone: NoticeTone::Neutral,
        });
        self.request_selection_redraw();
    }

    /// Clear the notice once it has outlived [`NOTICE_DURATION`]. Called from the
    /// frame clock alongside the other transient-overlay updates. No-op when no
    /// notice is in flight (the default path).
    pub(in crate::native) fn update_open_notice(&mut self, now: Instant) {
        if let Some(notice) = self.open_notice.as_ref()
            && now.saturating_duration_since(notice.raised_at) >= NOTICE_DURATION
        {
            self.open_notice = None;
        }
    }

    /// The next wake instant while a notice is visible (its expiry), or `None`
    /// when none is in flight — so an at-rest terminal schedules no extra wakes.
    pub(in crate::native) fn open_notice_deadline(&self) -> Option<Instant> {
        self.open_notice
            .as_ref()
            .map(|notice| notice.raised_at + NOTICE_DURATION)
    }

    /// Render-cache fragment: the live message while a notice is visible (so the
    /// banner repaints when the text changes or it clears), `Inert` otherwise.
    /// `Inert` on the default path keeps the geometry-update decision unchanged.
    pub(in crate::native) fn open_notice_overlay_signature(&self) -> OverlayFragment {
        if let Some(prompt) = self.osc52_prompt_overlay_signature() {
            return prompt;
        }
        match self.open_notice.as_ref() {
            Some(notice) => OverlayFragment::OpenNotice {
                text: notice.message.clone(),
            },
            None => OverlayFragment::Inert,
        }
    }

    /// Paint the notice as a single inverse-video banner row across the top of
    /// the grid. No-op when no notice is in flight (the default path emits
    /// nothing, so the frame is byte-identical). Overwrites only row 0 for the
    /// notice's lifetime; the shell content underneath is untouched in the model
    /// and reappears when the banner clears.
    pub(in crate::native) fn paint_open_notice_cells(&self, snapshot: &mut Snapshot) {
        if self.paint_osc52_prompt_cells(snapshot) {
            return;
        }
        let Some(notice) = self.open_notice.as_ref() else {
            return;
        };
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        if columns == 0 || rows == 0 {
            return;
        }
        let attrs = notice_attrs(notice.tone);
        // Fill the whole top row so the banner reads as a solid bar.
        for column in 0..columns {
            snapshot.cells[column] = Cell::new(' ', attrs);
        }
        // Write the (control-stripped, width-clamped) message from column 1.
        let mut x = 1usize;
        for ch in notice.message.chars() {
            if ch.is_control() {
                continue;
            }
            if x >= columns {
                break;
            }
            snapshot.cells[x] = Cell::new(ch, attrs);
            x += 1;
        }
    }
}

/// Banner attributes: an inverse, attention-colored bar. Indexed colors keep it
/// theme-portable (the active palette supplies the actual RGB).
fn notice_attrs(tone: NoticeTone) -> Attrs {
    let mut attrs = Attrs::default();
    match tone {
        NoticeTone::Failure => {
            // Bright-red background and near-black foreground keep failures
            // visually distinct from routine status updates.
            attrs.foreground = Color::Indexed(0);
            attrs.background = Color::Indexed(9);
        }
        NoticeTone::Neutral => {
            attrs.foreground = Color::Indexed(15);
            attrs.background = Color::Indexed(4);
        }
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_duration_is_a_few_seconds() {
        // Sanity bound: long enough to read, short enough to not linger.
        assert!(NOTICE_DURATION >= Duration::from_secs(2));
        assert!(NOTICE_DURATION <= Duration::from_secs(8));
    }

    #[test]
    fn notice_attrs_are_inverse_attention_colors() {
        let a = notice_attrs(NoticeTone::Failure);
        assert_eq!(a.background, Color::Indexed(9));
        assert_eq!(a.foreground, Color::Indexed(0));
    }

    #[test]
    fn neutral_notice_uses_status_colors() {
        let a = notice_attrs(NoticeTone::Neutral);
        assert_eq!(a.background, Color::Indexed(4));
        assert_eq!(a.foreground, Color::Indexed(15));
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Reusable, bounded text HUD for short window-level feedback.
//!
//! The surface owns one message and one expiry deadline. It is intentionally
//! static: reduced-motion mode needs no special branch, and an idle terminal
//! gains no frame-paced wakeups. Font zoom uses it now; resize feedback can use
//! the same `show`/`paint` boundary without introducing another timer or visual
//! treatment.

use std::time::{Duration, Instant};

use crate::core::{Attrs, Cell, Color, Snapshot};

use super::{App, OverlayFragment};

/// Long enough to confirm a gesture without lingering over terminal content.
pub(super) const TRANSIENT_HUD_DURATION: Duration = Duration::from_millis(1500);

#[derive(Debug, Default, Clone)]
pub(super) struct TransientHud {
    text: Option<String>,
    shown_at: Option<Instant>,
}

impl TransientHud {
    /// Show or replace the current message. Repeated gesture steps refresh the
    /// single deadline rather than stacking multiple surfaces.
    pub(super) fn show(&mut self, text: String, now: Instant) {
        self.text = Some(text);
        self.shown_at = Some(now);
    }

    /// Clear the message once its one-shot deadline passes. Returns whether the
    /// visible state changed so the event-loop owner can request one repaint.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let Some(shown_at) = self.shown_at else {
            return false;
        };
        if now.saturating_duration_since(shown_at) < TRANSIENT_HUD_DURATION {
            return false;
        }
        self.text = None;
        self.shown_at = None;
        true
    }

    /// The sole wake while visible; `None` at rest preserves zero-wake idle.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.shown_at
            .map(|shown_at| shown_at + TRANSIENT_HUD_DURATION)
    }

    pub(super) fn signature(&self) -> OverlayFragment {
        self.text.as_ref().map_or(OverlayFragment::Inert, |text| {
            OverlayFragment::TransientHud { text: text.clone() }
        })
    }

    /// Paint a compact, centered one-row chip. Indexed black/bright-white uses
    /// the active terminal palette and remains readable under the plain theme;
    /// no alpha or animation is involved.
    pub(super) fn paint(&self, snapshot: &mut Snapshot) {
        let Some(text) = self.text.as_deref() else {
            return;
        };
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        if columns < 3 || rows == 0 {
            return;
        }

        let message: Vec<char> = text
            .chars()
            .filter(|ch| !ch.is_control())
            .take(columns.saturating_sub(2))
            .collect();
        if message.is_empty() {
            return;
        }
        let width = message.len() + 2;
        let start_col = columns.saturating_sub(width) / 2;
        let row = rows / 2;
        let attrs = hud_attrs();
        let row_start = row * columns;

        for col in start_col..start_col + width {
            snapshot.cells[row_start + col] = Cell::new(' ', attrs);
        }
        for (offset, ch) in message.into_iter().enumerate() {
            snapshot.cells[row_start + start_col + 1 + offset] = Cell::new(ch, attrs);
        }
    }

    #[cfg(test)]
    pub(super) fn text_for_test(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

fn hud_attrs() -> Attrs {
    let mut attrs = Attrs::default();
    attrs.foreground = Color::Indexed(15);
    attrs.background = Color::Indexed(0);
    attrs.set_bold(true);
    attrs
}

impl App {
    /// Replace the current HUD message and schedule its one expiry wake.
    pub(super) fn show_transient_hud(&mut self, text: String) {
        self.transient_hud.show(text, Instant::now());
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    pub(super) fn show_font_size_hud(&mut self, font_size_px: f32) {
        let value = if font_size_px.fract().abs() < f32::EPSILON {
            format!("{font_size_px:.0}")
        } else {
            format!("{font_size_px:.1}")
        };
        self.show_transient_hud(format!("Font {value} px"));
    }

    /// Paint window-level feedback only when no modal surface owns the frame.
    /// This keeps prompts and settings authoritative instead of allowing a
    /// late presentation chip to overwrite their cells.
    pub(super) fn paint_transient_hud_cells(&self, snapshot: &mut Snapshot) {
        if self.overlay.is_open() || self.rename_state.is_some() {
            return;
        }
        self.transient_hud.paint(snapshot);
    }

    pub(super) fn transient_hud_deadline(&self) -> Option<Instant> {
        self.transient_hud.deadline()
    }

    /// Consume the due one-shot boundary. Unlike frame-paced animation timers,
    /// this is valid in both single- and multi-pane layouts.
    pub(super) fn expire_transient_hud(&mut self, now: Instant) -> bool {
        self.transient_hud.expire(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Dimensions, Snapshot};

    fn blank_snapshot(columns: usize, rows: usize) -> Snapshot {
        Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cells: vec![Cell::default(); columns * rows],
            cursor: Default::default(),
            cursor_visible: true,
            colors: Default::default(),
        }
    }

    #[test]
    fn show_replaces_and_refreshes_one_bounded_deadline() {
        let mut hud = TransientHud::default();
        let t0 = Instant::now();
        hud.show("Font 20 px".to_owned(), t0);
        assert_eq!(hud.deadline(), Some(t0 + TRANSIENT_HUD_DURATION));

        let t1 = t0 + Duration::from_millis(200);
        hud.show("80 x 24".to_owned(), t1);
        assert_eq!(hud.text_for_test(), Some("80 x 24"));
        assert_eq!(hud.deadline(), Some(t1 + TRANSIENT_HUD_DURATION));
        assert!(!hud.expire(t1 + TRANSIENT_HUD_DURATION - Duration::from_millis(1)));
        assert!(hud.expire(t1 + TRANSIENT_HUD_DURATION));
        assert_eq!(hud.deadline(), None);
    }

    #[test]
    fn centered_hud_is_static_and_plain_theme_safe() {
        let mut hud = TransientHud::default();
        hud.show("Font 21 px".to_owned(), Instant::now());
        let mut first = blank_snapshot(20, 5);
        let mut second = first.clone();
        hud.paint(&mut first);
        hud.paint(&mut second);
        assert_eq!(first, second, "no motion phase changes the HUD");

        let row = 2;
        let message_start = row * 20 + 5;
        let painted = &first.cells[message_start..message_start + "Font 21 px".len()];
        assert_eq!(
            painted.iter().map(|cell| cell.ch).collect::<String>(),
            "Font 21 px"
        );
        assert!(painted.iter().all(|cell| {
            cell.attrs.foreground == Color::Indexed(15)
                && cell.attrs.background == Color::Indexed(0)
                && cell.attrs.bold()
        }));
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Reusable, bounded text HUD for short window-level feedback.
//!
//! The surface owns one message and one expiry deadline. It is intentionally
//! static: reduced-motion mode needs no special branch, and an idle terminal
//! gains no frame-paced wakeups. Font zoom and debounced resize feedback share
//! the same state and painter without introducing independent timers or visual
//! treatments.

use std::time::{Duration, Instant};

use crate::core::{Attrs, Cell, Color, Dimensions, DynamicColors, Position, Snapshot};
use crate::native::layout::PaneRect;
use crate::text::CellSize;

use super::{App, OverlayFragment};

/// Long enough to confirm a gesture without lingering over terminal content.
pub(super) const TRANSIENT_HUD_DURATION: Duration = Duration::from_millis(1500);

#[derive(Debug, Default, Clone)]
pub(super) struct TransientHud {
    text: Option<String>,
    deadline: Option<Instant>,
}

impl TransientHud {
    /// Show or replace the current message. Repeated gesture steps refresh the
    /// single deadline rather than stacking multiple surfaces.
    pub(super) fn show(&mut self, text: String, now: Instant) {
        self.show_for(text, now, TRANSIENT_HUD_DURATION);
    }

    /// Show or replace a message with a producer-specific bounded lifetime.
    /// Resize feedback uses Ghostty's shorter 750 ms convention while font
    /// zoom retains the more relaxed gesture-confirmation interval.
    pub(super) fn show_for(&mut self, text: String, now: Instant, duration: Duration) {
        self.text = Some(text);
        self.deadline = Some(now + duration);
    }

    /// Clear the message once its one-shot deadline passes. Returns whether the
    /// visible state changed so the event-loop owner can request one repaint.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.deadline else {
            return false;
        };
        if now < deadline {
            return false;
        }
        self.text = None;
        self.deadline = None;
        true
    }

    /// The sole wake while visible; `None` at rest preserves zero-wake idle.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
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

    pub(super) fn text(&self) -> Option<&str> {
        self.text.as_deref()
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
        self.invalidate_transient_hud();
    }

    pub(super) fn show_transient_hud_for(
        &mut self,
        text: String,
        now: Instant,
        duration: Duration,
    ) {
        self.transient_hud.show_for(text, now, duration);
        self.invalidate_transient_hud();
    }

    fn invalidate_transient_hud(&mut self) {
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

    /// Build the same compact HUD as an independent topmost snapshot for the
    /// multi-pane renderer, centered over the whole terminal content area.
    pub(super) fn build_transient_hud_top(
        &self,
        content: PaneRect,
        cell: CellSize,
    ) -> Option<(Snapshot, [f32; 2])> {
        if self.overlay.is_open() || self.rename_state.is_some() {
            return None;
        }
        let text = self.transient_hud.text()?;
        let (columns, rows) =
            crate::native::layout::grid_dims_for_rect(content, cell.width, cell.height);
        if columns < 3 || rows == 0 {
            return None;
        }
        let message_width = text.chars().filter(|ch| !ch.is_control()).count();
        if message_width == 0 {
            return None;
        }
        let width = message_width.saturating_add(2).min(columns);
        let left = columns.saturating_sub(width) / 2;
        let top = rows / 2;
        let colors: DynamicColors = crate::native::lock_recover(&self.terminal)
            .dynamic_colors()
            .clone();
        let mut snapshot = Snapshot {
            dimensions: Dimensions::new(width, 1),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors,
            cells: vec![Cell::default(); width],
        };
        self.transient_hud.paint(&mut snapshot);
        Some((
            snapshot,
            [
                content.x + left as f32 * cell.width as f32,
                content.y + top as f32 * cell.height as f32,
            ],
        ))
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
    use crate::native::NativeOptions;
    use crate::native::layout::PaneRect;
    use crate::native::test_support::headless_app_with;
    use crate::settings::Settings;
    use crate::text::CellSize;

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

    #[test]
    fn split_hud_is_centered_over_the_window_and_modal_surfaces_suppress_it() {
        let dims = Dimensions::new(80, 24);
        let (mut app, _terminal) =
            headless_app_with(NativeOptions::default(), dims, Settings::default());
        app.transient_hud.show("80 × 24".to_owned(), Instant::now());
        let content = PaneRect {
            x: 10.0,
            y: 20.0,
            w: 800.0,
            h: 480.0,
        };
        let cell = CellSize {
            width: 10,
            height: 20,
            baseline: 15,
        };
        let (panel, origin) = app
            .build_transient_hud_top(content, cell)
            .expect("visible HUD");
        assert_eq!(panel.dimensions, Dimensions::new(9, 1));
        assert_eq!(origin, [360.0, 260.0]);
        assert_eq!(
            panel.cells[1..8]
                .iter()
                .map(|cell| cell.ch)
                .collect::<String>(),
            "80 × 24"
        );

        app.open_settings_overlay_for_test();
        assert!(app.build_transient_hud_top(content, cell).is_none());
        let mut single = blank_snapshot(20, 5);
        app.paint_transient_hud_cells(&mut single);
        assert!(single.cells.iter().all(|cell| *cell == Cell::default()));
    }
}

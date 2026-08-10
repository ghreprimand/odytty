// SPDX-License-Identifier: GPL-3.0-only
//! Window-resize producer for the shared transient HUD.
//!
//! This state observes nonzero surface resize events and publishes only after
//! the existing debounce path applies its final terminal geometry. It never
//! owns terminal, PTY, or input state.

use std::time::{Duration, Instant};

use crate::core::Dimensions;

use super::App;

pub(super) const RESIZE_HUD_DURATION: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Default)]
pub(super) struct ResizeHud {
    saw_first_nonzero_resize: bool,
    pending_after_first: bool,
}

impl ResizeHud {
    /// Ghostty-style `after-first` behavior: the first nonzero configure arms
    /// feedback, later events publish after the debounce applies. Minimize
    /// notifications do not consume the first-configure suppression.
    pub(super) fn note_window_resize(&mut self, width_px: u32, height_px: u32) {
        if width_px == 0 || height_px == 0 {
            return;
        }
        if !self.saw_first_nonzero_resize {
            self.saw_first_nonzero_resize = true;
            return;
        }
        self.pending_after_first = true;
    }

    pub(super) fn applied_text(&mut self, dimensions: Dimensions) -> Option<String> {
        if !std::mem::take(&mut self.pending_after_first) {
            return None;
        }
        Some(format!("{} × {}", dimensions.columns, dimensions.rows))
    }
}

impl App {
    pub(super) fn note_window_resize_for_hud(&mut self, width_px: u32, height_px: u32) {
        self.resize_hud.note_window_resize(width_px, height_px);
    }

    pub(super) fn finish_resize_for_hud(&mut self, now: Instant) {
        if let Some(text) = self.resize_hud.applied_text(self.grid) {
            self.show_transient_hud_for(text, now, RESIZE_HUD_DURATION);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::NativeOptions;
    use crate::native::test_support::headless_app_with;
    use crate::settings::Settings;
    use crate::text::CellSize;

    #[test]
    fn after_first_uses_applied_grid_and_zero_size_does_not_arm() {
        let mut hud = ResizeHud::default();
        hud.note_window_resize(0, 0);
        hud.note_window_resize(800, 480);
        assert_eq!(hud.applied_text(Dimensions::new(80, 24)), None);

        hud.note_window_resize(810, 480);
        assert_eq!(
            hud.applied_text(Dimensions::new(81, 24)).as_deref(),
            Some("81 × 24")
        );
        assert_eq!(hud.applied_text(Dimensions::new(81, 24)), None);
    }

    #[test]
    fn debounce_publishes_only_the_last_geometry_and_uses_750_ms() {
        let start = Instant::now();
        let dims = Dimensions::new(80, 24);
        let (mut app, _terminal) =
            headless_app_with(NativeOptions::default(), dims, Settings::default());
        let cell = CellSize {
            width: 10,
            height: 20,
            baseline: 15,
        };
        let pending = |width_px| super::super::PendingResize {
            cell,
            padding: crate::native::viewport::WindowPadding::ZERO,
            width_px,
            height_px: 480,
        };

        app.note_window_resize_for_hud(800, 480);
        app.record_pending_resize(pending(800), start);
        assert_eq!(app.transient_hud.text(), None);

        app.note_window_resize_for_hud(810, 480);
        app.record_pending_resize(pending(810), start + Duration::from_millis(10));
        app.note_window_resize_for_hud(820, 480);
        app.record_pending_resize(pending(820), start + Duration::from_millis(20));
        assert_eq!(app.transient_hud.text(), None);

        let due = app
            .resize_debounce
            .take_due(start + super::super::RESIZE_DEBOUNCE_INTERVAL)
            .expect("last resize becomes due");
        app.apply_grid_resize(due);
        app.finish_resize_for_hud(start + super::super::RESIZE_DEBOUNCE_INTERVAL);
        assert_eq!(app.grid, Dimensions::new(82, 24));
        assert_eq!(app.transient_hud.text(), Some("82 × 24"));
        assert_eq!(
            app.transient_hud.deadline(),
            Some(start + super::super::RESIZE_DEBOUNCE_INTERVAL + RESIZE_HUD_DURATION)
        );
    }

    #[test]
    fn pixel_only_resize_refreshes_the_shared_hud_lifetime() {
        let start = Instant::now();
        let dims = Dimensions::new(80, 24);
        let (mut app, _terminal) =
            headless_app_with(NativeOptions::default(), dims, Settings::default());

        app.note_window_resize_for_hud(800, 480);
        app.finish_resize_for_hud(start);
        app.note_window_resize_for_hud(801, 480);
        app.finish_resize_for_hud(start);
        assert_eq!(app.transient_hud.text(), Some("80 × 24"));

        let refreshed = start + Duration::from_millis(40);
        app.note_window_resize_for_hud(802, 480);
        app.finish_resize_for_hud(refreshed);
        assert_eq!(app.transient_hud.text(), Some("80 × 24"));
        assert_eq!(
            app.transient_hud.deadline(),
            Some(refreshed + RESIZE_HUD_DURATION)
        );
    }
}

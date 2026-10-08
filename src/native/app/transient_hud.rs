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
    /// no animation is involved. Both render paths draw the chip's background
    /// opaque: the split path as its own top layer, the single-pane path
    /// through [`chip_span`] in the window's opaque cell span.
    pub(super) fn paint(&self, snapshot: &mut Snapshot) {
        let Some(text) = self.text.as_deref() else {
            return;
        };
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        let Some((start_col, row, width)) = chip_span(text, columns, rows) else {
            return;
        };
        let message = text.chars().filter(|ch| !ch.is_control()).take(width - 2);
        let attrs = hud_attrs();
        let row_start = row * columns;

        for col in start_col..start_col + width {
            snapshot.cells[row_start + col] = Cell::new(' ', attrs);
        }
        for (offset, ch) in message.enumerate() {
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

/// The chip's `(left column, row, width)` in a `columns` x `rows` grid, or
/// `None` when nothing would paint. The painter and the single-pane opaque
/// span share it, so the opaque cells are exactly the painted chip.
fn chip_span(text: &str, columns: usize, rows: usize) -> Option<(usize, usize, usize)> {
    if columns < 3 || rows == 0 {
        return None;
    }
    let shown = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(columns - 2)
        .count();
    if shown == 0 {
        return None;
    }
    let width = shown + 2;
    Some(((columns - width) / 2, rows / 2, width))
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

    /// A window overlay or rename prompt owns the frame, so the chip stays
    /// hidden in both render paths.
    fn transient_hud_suppressed(&self) -> bool {
        self.overlay.is_open() || self.rename_state.is_some()
    }

    /// Paint window-level feedback only when no modal surface owns the frame.
    /// This keeps prompts and settings authoritative instead of allowing a
    /// late presentation chip to overwrite their cells.
    pub(super) fn paint_transient_hud_cells(&self, snapshot: &mut Snapshot) {
        if self.transient_hud_suppressed() {
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
        if self.transient_hud_suppressed() {
            return None;
        }
        let text = self.transient_hud.text()?;
        let (columns, rows) =
            crate::native::layout::grid_dims_for_rect(content, cell.width, cell.height);
        let (left, top, width) = chip_span(text, columns, rows)?;
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

    /// The single-pane chip's `(left, top, width, height)` in content cells,
    /// or `None` while no chip paints. Joins the single-pane opaque span so
    /// the chip background matches the split path's opaque top layer.
    pub(super) fn transient_hud_content_rect(&self) -> Option<(usize, usize, usize, usize)> {
        if self.transient_hud_suppressed() {
            return None;
        }
        let text = self.transient_hud.text()?;
        let (left, top, width) = chip_span(text, self.grid.columns, self.grid.rows)?;
        Some((left, top, width, 1))
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

    /// Paint the single-pane HUD into a blank content snapshot and return the
    /// painted cells' `(left, top, width)`.
    fn painted_chip(app: &App) -> (usize, usize, usize) {
        let mut content = blank_snapshot(app.grid.columns, app.grid.rows);
        app.paint_transient_hud_cells(&mut content);
        let painted: Vec<usize> = (0..content.cells.len())
            .filter(|&i| content.cells[i] != Cell::default())
            .collect();
        let first = *painted.first().expect("chip painted");
        let last = *painted.last().expect("chip painted");
        let columns = app.grid.columns;
        assert_eq!(first / columns, last / columns, "one-row chip");
        (first % columns, first / columns, last - first + 1)
    }

    #[test]
    fn single_pane_hud_chip_is_held_opaque_like_the_split_layer() {
        let dims = Dimensions::new(80, 24);
        let (mut app, _terminal) =
            headless_app_with(NativeOptions::default(), dims, Settings::default());
        assert!(
            app.settings.cell_bg_opacity < 1.0,
            "default content is translucent"
        );
        assert_eq!(
            app.single_pane_opaque_region_for_frame(1.0),
            None,
            "no HUD, no span"
        );

        app.transient_hud
            .show("Font 21 px".to_owned(), Instant::now());
        let (left, top, width) = painted_chip(&app);
        let reserve = app.tab_reserve();
        let expected = crate::grid::CellRegion {
            left: left + reserve.left_reserved_cols(),
            top: top + reserve.top_rows,
            width,
            height: 1,
        };
        // Opaque window at the default cell opacity, and a translucent window:
        // the chip's cells are exactly the opaque span in both.
        assert_eq!(app.single_pane_opaque_region_for_frame(1.0), Some(expected));
        assert_eq!(app.single_pane_opaque_region_for_frame(0.5), Some(expected));

        // Fully opaque content needs no span, keeping that path byte-identical.
        app.settings.cell_bg_opacity = 1.0;
        assert_eq!(app.single_pane_opaque_region_for_frame(1.0), None);
        assert_eq!(app.single_pane_opaque_region_for_frame(0.5), Some(expected));

        // A modal surface owns the span and hides the chip.
        app.open_settings_overlay_for_test();
        let overlay = app.single_pane_opaque_region_for_frame(0.5);
        assert!(overlay.is_some() && overlay != Some(expected));
    }

    #[test]
    fn hud_span_tracks_truncation_on_a_narrow_grid() {
        let dims = Dimensions::new(6, 3);
        let (mut app, _terminal) =
            headless_app_with(NativeOptions::default(), dims, Settings::default());
        app.grid = dims;
        app.transient_hud
            .show("Font 21 px".to_owned(), Instant::now());
        let (left, top, width) = painted_chip(&app);
        assert_eq!((left, top, width), (0, 1, 6));
        assert_eq!(app.transient_hud_content_rect(), Some((0, 1, 6, 1)));
        let narrow = PaneRect {
            x: 0.0,
            y: 0.0,
            w: 60.0,
            h: 60.0,
        };
        let cell = CellSize {
            width: 10,
            height: 20,
            baseline: 15,
        };
        let (panel, _) = app.build_transient_hud_top(narrow, cell).expect("chip");
        assert_eq!(panel.dimensions, Dimensions::new(6, 1));
    }

    /// The emitted background alpha of the HUD chip: fully opaque inside the
    /// frame's span at the default cell opacity, as the split path's top
    /// layer builds it, while the surrounding content keeps its opacity.
    #[test]
    fn single_pane_hud_background_draws_opaque() {
        let _guard = crate::test_lock::render_globals_lock();
        let font = crate::text::FontHandle::try_from_vec(
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf")
                .to_vec(),
        )
        .expect("required OFL-licensed embedded HUD fixture parses");
        let atlas = crate::text::GlyphAtlas::build(&font, 24.0);
        let dims = Dimensions::new(20, 5);
        let (mut app, _terminal) =
            headless_app_with(NativeOptions::default(), dims, Settings::default());
        app.grid = dims;
        app.transient_hud
            .show("Font 21 px".to_owned(), Instant::now());
        let mut content = blank_snapshot(20, 5);
        app.paint_transient_hud_cells(&mut content);
        let opacity = app.settings.cell_bg_opacity;
        let build = |region| {
            let mut out = Vec::new();
            crate::grid::build_cell_vertices_with_focus_dim_and_origin_into(
                &mut out,
                &content,
                &atlas,
                &[],
                0.0,
                [0.0, 0.0],
                crate::grid::BackgroundTreatmentParams::default(),
                opacity,
                1.0,
                region,
                crate::grid::ChromePin::NONE,
            );
            out
        };
        // The first quad starting at the cell's corner is its background:
        // the builder emits every background before any glyph.
        let cell = atlas.cell;
        let bg_alpha = |verts: &[crate::grid::Vertex], row: usize, col: usize| {
            let corner = [
                (col as u32 * cell.width) as f32,
                (row as u32 * cell.height) as f32,
            ];
            verts
                .iter()
                .step_by(crate::grid::INSTANCES_PER_QUAD)
                .find(|vertex| vertex.pos == corner)
                .map(|vertex| vertex.color[3])
                .expect("chip cell has a background quad")
        };
        let (left, top, width) = painted_chip(&app);
        let span = app.single_pane_opaque_region_for_frame(1.0);
        assert!(span.is_some_and(|r| r.left == left && r.top == top && r.width == width));
        let with_span = build(span);
        let without = build(None);
        for col in left..left + width {
            assert_eq!(
                bg_alpha(&with_span, top, col),
                1.0,
                "chip cell {col} is opaque"
            );
            assert!(
                bg_alpha(&without, top, col) < 1.0,
                "unforced chip cell {col} is translucent"
            );
        }
    }
}

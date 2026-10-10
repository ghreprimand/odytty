// SPDX-License-Identifier: GPL-3.0-only
//! ID4 themed window border: an optional thin frame in the theme `border` role
//! color drawn around the terminal content, in single-pane and split tabs alike.
//!
//! The frame is painted as four [`SolidQuad`]s forming a ring whose inner edge
//! is flush with the content rect and which extends OUTWARD into the existing
//! window-padding band (D-ID4-1): it never overlaps cell area, so the grid text
//! is untouched and the plain/fast path is unaffected. The border thickness is
//! authored in logical pixels and multiplied by the surface scale factor
//! (`ctx.scale`) so it is a consistent visual weight across displays (T-ID4-2,
//! DPI), and it is clamped to the available padding so a thick border on a
//! narrow padding band can never bleed past the surface edge. Because the ring
//! is derived entirely from the live content rect (padding + columns·cell_w /
//! rows·cell_h), it tracks the viewport on every resize with no stored rect
//! (T-ID4-3).
//!
//! Cache invalidation needs no dedicated signature fragment: the border is a
//! pure function of the `window_border` knob, the theme `border` color, the grid
//! geometry, and the scale factor — and every one of those forces a Full rebuild
//! through an existing signature input (a settings/theme change bumps
//! `presentation_epoch`; a resize or DPI change moves the grid/cell signature),
//! so the border quad is always recomputed when any input that affects it
//! changes, and a Retained frame correctly keeps the previously drawn border.
//!
//! Off-path contract (`window_border` defaults to `false`, T-ID4-1):
//! [`App::paint_window_border_quads`] returns before emitting any quad, so the
//! default render path is byte-identical to before this feature existed.

use super::overlay_registry::OverlayCtx;
use super::*;

/// Border thickness in LOGICAL pixels, scaled by the surface DPI factor at draw
/// time. A hairline frame that reads as a deliberate edge without crowding the
/// padding.
const BORDER_THICKNESS_LOGICAL_PX: f32 = 1.5;

impl App {
    /// Emit the themed window-border quads for this frame.
    ///
    /// No-op unless `window_border` is on. The ring sits in the padding band
    /// just outside the content rect, so it never covers a cell; its thickness
    /// is `BORDER_THICKNESS_LOGICAL_PX · scale`, clamped to the padding width so
    /// it stays inside the surface. Emits four quads (top / bottom / left /
    /// right) in the theme `border` role color at full opacity.
    pub(in crate::native) fn paint_window_border_quads(
        &self,
        ctx: &OverlayCtx,
        out: &mut Vec<SolidQuad>,
    ) {
        let cols = ctx.grid.columns;
        let rows = ctx.grid.rows;
        if cols == 0 || rows == 0 {
            return;
        }
        let pad = ctx.window_padding.as_f32();
        let content = [
            pad,
            pad,
            pad + cols as f32 * ctx.cell.width as f32,
            pad + rows as f32 * ctx.cell.height as f32,
        ];
        self.window_border_ring(content, pad, ctx.scale, out);
    }

    /// Emit the border ring around `content` (`[x0, y0, x1, y1]` in surface
    /// pixels): the single-pane grid, or the whole pane area of a split,
    /// zoomed, stacked or floating tab, so splitting never removes the frame.
    /// `room` is the padding band available outside `content`; the ring is
    /// clamped to it so it never reaches the surface edge or a chrome band.
    /// No-op unless `window_border` is on or when `content` is empty.
    pub(in crate::native) fn window_border_ring(
        &self,
        content: [f32; 4],
        room: f32,
        scale: f32,
        out: &mut Vec<SolidQuad>,
    ) {
        if !self.settings.window_border {
            return;
        }
        let [cx0, cy0, cx1, cy1] = content;
        if cx1 <= cx0 || cy1 <= cy0 {
            return;
        }
        // Thickness in physical px, scaled by DPI and clamped to the padding so
        // the outward ring never extends past the surface edge. A zero padding
        // band leaves no room for an outward border.
        let thickness = (BORDER_THICKNESS_LOGICAL_PX * scale.max(1.0)).min(room);
        if thickness <= 0.0 {
            return;
        }
        // Outer edge of the ring, `thickness` into the padding band.
        let ox0 = cx0 - thickness;
        let oy0 = cy0 - thickness;
        let ox1 = cx1 + thickness;
        let oy1 = cy1 + thickness;
        // Present the active pane's theme (a profile tab paints its authored
        // border; a plain tab uses the global effective theme, which
        // `chrome_theme` equals).
        let (r, g, b) = self.chrome_theme.border;
        let mut color = text::foreground_linear(Color::Rgb(r, g, b));
        color[3] = 1.0;
        // Top and bottom span the full outer width; left and right fill only the
        // content-height gap between them, so the four quads tile the ring
        // without overlapping at the corners.
        out.push(SolidQuad {
            rect: [ox0, oy0, ox1, cy0],
            color,
        });
        out.push(SolidQuad {
            rect: [ox0, cy1, ox1, oy1],
            color,
        });
        out.push(SolidQuad {
            rect: [ox0, cy0, cx0, cy1],
            color,
        });
        out.push(SolidQuad {
            rect: [cx1, cy0, ox1, cy1],
            color,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL_W: u32 = 8;
    const CELL_H: u32 = 16;
    const ROWS: usize = 6;
    const COLS: usize = 40;

    fn build_app() -> Option<App> {
        let d = Dimensions::new(COLS, ROWS);
        let (mut app, _terminal) = crate::native::test_support::headless_app_with(
            crate::native::options::NativeOptions::default(),
            d,
            Settings::default(),
        );
        app.grid = d;
        app.set_test_cell_for_test(CellSize {
            width: CELL_W,
            height: CELL_H,
            baseline: 0,
        });
        Some(app)
    }

    /// Padding a headless App has no GPU to report: an explicit logical band
    /// and scale, so the bordered paths run instead of finding no room.
    const PAD_LOGICAL: f32 = 8.0;

    fn ctx(app: &App) -> OverlayCtx {
        ctx_at(app, PAD_LOGICAL, 1.0)
    }

    fn ctx_at(app: &App, pad_logical: f32, scale: f32) -> OverlayCtx {
        let mut ctx = app.overlay_ctx(
            app.scrollback_len(),
            CellSize {
                width: CELL_W,
                height: CELL_H,
                baseline: 0,
            },
            Position { row: 0, column: 0 },
            true,
            Instant::now(),
        );
        ctx.window_padding = crate::native::WindowPadding::from_logical(pad_logical, scale);
        ctx.scale = scale;
        ctx
    }

    // --- T-ID4-1: off-path identity -----------------------------------------

    #[test]
    fn off_path_emits_nothing() {
        let Some(app) = build_app() else {
            return;
        };
        assert!(!app.settings.window_border, "off by default");
        let mut quads = Vec::new();
        app.paint_window_border_quads(&ctx(&app), &mut quads);
        assert!(quads.is_empty(), "off path emits zero border quads");
    }

    // --- on path: four quads in the theme border color ----------------------

    #[test]
    fn on_path_emits_four_border_quads_in_theme_color() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.window_border = true;
        let c = ctx(&app);
        assert!(
            c.window_padding.as_f32() > 0.0,
            "the fixture has a padding band"
        );
        let mut quads = Vec::new();
        app.paint_window_border_quads(&c, &mut quads);
        assert_eq!(quads.len(), 4, "top/bottom/left/right");
        let (r, g, b) = app.chrome_theme.border;
        let expect = crate::text::foreground_linear(crate::core::Color::Rgb(r, g, b));
        for q in &quads {
            assert_eq!(
                [q.color[0], q.color[1], q.color[2]],
                [expect[0], expect[1], expect[2]],
                "border uses the theme border role color"
            );
            assert!((q.color[3] - 1.0).abs() < 1e-6, "opaque chrome");
        }
    }

    // --- never covers cell area (ring stays in the padding band) ------------

    #[test]
    fn border_stays_outside_the_content_rect() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.window_border = true;
        let c = ctx(&app);
        let pad = c.window_padding.as_f32();
        assert!(pad > 0.0, "the fixture has a padding band");
        let content_x1 = pad + COLS as f32 * CELL_W as f32;
        let content_y1 = pad + ROWS as f32 * CELL_H as f32;
        let mut quads = Vec::new();
        app.paint_window_border_quads(&c, &mut quads);
        assert_eq!(quads.len(), 4, "the ring is drawn");
        // No quad intrudes into the interior of the content rect: each quad is
        // either entirely in a padding band (outside [pad, content_x1] ×
        // [pad, content_y1]) along at least one axis.
        for q in &quads {
            let outside_x = q.rect[2] <= pad + 1e-3 || q.rect[0] >= content_x1 - 1e-3;
            let outside_y = q.rect[3] <= pad + 1e-3 || q.rect[1] >= content_y1 - 1e-3;
            assert!(
                outside_x || outside_y,
                "border quad must not cover cell area: {:?}",
                q.rect
            );
            // And it never extends above/left of the surface origin.
            assert!(q.rect[0] >= -1e-3 && q.rect[1] >= -1e-3, "stays on-surface");
        }
    }

    // --- T-ID4-3: tracks geometry on resize (no stored rect) ----------------

    #[test]
    fn border_tracks_content_rect_after_resize() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.window_border = true;
        let pad = ctx(&app).window_padding.as_f32();
        assert!(pad > 0.0, "the fixture has a padding band");
        // Grow the grid; the right/bottom edges of the ring must follow.
        app.grid = Dimensions::new(COLS + 10, ROWS + 4);
        let mut quads = Vec::new();
        app.paint_window_border_quads(&ctx(&app), &mut quads);
        assert_eq!(quads.len(), 4, "the ring is drawn");
        let content_x1 = pad + (COLS + 10) as f32 * CELL_W as f32;
        let content_y1 = pad + (ROWS + 4) as f32 * CELL_H as f32;
        let bottom = quads
            .iter()
            .max_by(|a, b| a.rect[1].partial_cmp(&b.rect[1]).unwrap())
            .expect("a bottom border quad");
        assert!(
            (bottom.rect[1] - content_y1).abs() < 1.0,
            "bottom border tracks the resized content rect"
        );
        // The right-edge quad's inner edge sits at the new content_x1.
        let right = quads
            .iter()
            .max_by(|a, b| a.rect[0].partial_cmp(&b.rect[0]).unwrap())
            .expect("a right border quad");
        assert!(
            (right.rect[0] - content_x1).abs() < 1.0,
            "right border tracks the resized content rect"
        );
    }

    /// The ring is `1.5 * scale` physical pixels thick on a HiDPI surface,
    /// and never thicker than the padding band leaves room for.
    #[test]
    fn border_thickness_scales_with_dpi_and_is_capped_by_the_padding() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.window_border = true;
        let thickness = |quads: &[SolidQuad]| quads[0].rect[3] - quads[0].rect[1];
        let mut hidpi = Vec::new();
        app.paint_window_border_quads(&ctx_at(&app, PAD_LOGICAL, 2.0), &mut hidpi);
        assert_eq!(hidpi.len(), 4);
        assert!(
            (thickness(&hidpi) - 3.0).abs() < 1e-3,
            "{}",
            thickness(&hidpi)
        );
        let mut narrow = Vec::new();
        app.paint_window_border_quads(&ctx_at(&app, 1.0, 2.0), &mut narrow);
        assert_eq!(narrow.len(), 4);
        let room = ctx_at(&app, 1.0, 2.0).window_padding.as_f32();
        assert!(
            thickness(&narrow) <= room + 1e-3,
            "{} > {room}",
            thickness(&narrow)
        );
        let mut none = Vec::new();
        app.paint_window_border_quads(&ctx_at(&app, 0.0, 1.0), &mut none);
        assert!(none.is_empty(), "no padding band leaves no room for a ring");
    }

    // --- split, zoomed and floating tabs keep the outer ring ---------------

    fn split_app(border: bool) -> App {
        let d = Dimensions::new(COLS, ROWS);
        let (mut app, _terminal) = crate::native::test_support::headless_app_with(
            crate::native::options::NativeOptions::default(),
            d,
            Settings {
                window_border: border,
                ..Settings::default()
            },
        );
        let second = std::sync::Arc::new(std::sync::Mutex::new(crate::core::Terminal::new(
            COLS, ROWS,
        )));
        let writer = crate::native::test_support::headless_writer();
        app.seed_headless_split_pane_for_test(true, second, writer, d);
        app.set_test_cell_for_test(CellSize {
            width: CELL_W,
            height: CELL_H,
            baseline: 0,
        });
        app.set_test_surface_for_test(
            (COLS as u32) * CELL_W * 2,
            (ROWS as u32) * CELL_H * 2,
            crate::native::WindowPadding::from_logical(8.0, 1.0),
        );
        app
    }

    /// The four ring quads around the split's content rect.
    fn expected_ring(app: &App) -> Vec<[f32; 4]> {
        let (content, _) = app.pane_geometry_for_test().expect("split geometry");
        let t = BORDER_THICKNESS_LOGICAL_PX;
        let (x0, y0) = (content.x, content.y);
        let (x1, y1) = (content.x + content.w, content.y + content.h);
        vec![
            [x0 - t, y0 - t, x1 + t, y0],
            [x0 - t, y1, x1 + t, y1 + t],
            [x0 - t, y0, x0, y1],
            [x1, y0, x1 + t, y1],
        ]
    }

    fn assert_ring(quads: &[SolidQuad], app: &App, label: &str) {
        let (r, g, b) = app.chrome_theme.border;
        let color = crate::text::foreground_linear(crate::core::Color::Rgb(r, g, b));
        for rect in expected_ring(app) {
            assert!(
                quads.iter().any(|q| q.rect == rect
                    && q.color[..3] == color[..3]
                    && (q.color[3] - 1.0).abs() < 1e-6),
                "{label}: no border quad at {rect:?} in {:?}",
                quads.iter().map(|q| q.rect).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_split_tab_keeps_the_outer_window_border() {
        let mut app = split_app(true);
        let quads = app.rebuild_multipane_frame_quads_for_test();
        assert_ring(&quads, &app, "tiled split");
    }

    #[test]
    fn a_zoomed_split_keeps_the_outer_window_border() {
        let mut app = split_app(true);
        app.drive_char_with_mods_for_test('b', true, false);
        app.drive_char_with_mods_for_test('z', false, false);
        assert!(app.active_is_zoomed_for_test(), "the pane zoomed");
        let quads = app.rebuild_multipane_frame_quads_for_test();
        assert_ring(&quads, &app, "zoomed split");
    }

    #[test]
    fn the_split_border_adds_exactly_its_ring_and_is_absent_when_off() {
        let mut off = split_app(false);
        let off_quads = off.rebuild_multipane_frame_quads_for_test();
        let ring = expected_ring(&off);
        assert!(
            !off_quads.iter().any(|q| ring.contains(&q.rect)),
            "the off path draws no ring"
        );
        let mut on = split_app(true);
        let on_quads = on.rebuild_multipane_frame_quads_for_test();
        assert_eq!(on_quads.len(), off_quads.len() + 4, "four ring quads added");
        let without_ring: Vec<[f32; 4]> = on_quads
            .iter()
            .map(|q| q.rect)
            .filter(|rect| !ring.contains(rect))
            .collect();
        assert_eq!(
            without_ring,
            off_quads.iter().map(|q| q.rect).collect::<Vec<_>>(),
            "dividers and strip quads are unchanged"
        );
    }
}

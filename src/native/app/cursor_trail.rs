// SPDX-License-Identifier: GPL-3.0-only
//! VE4 cursor motion trail — a short fading after-image that follows the cursor
//! along its slide path while it glides between cells.
//!
//! The trail RIDES the existing cursor-slide animation (`cursor_motion`,
//! `cursor_frame.rs`) rather than tracking the cursor itself (D-VE4T-1): the
//! slide already stores the full displacement (`cursor_slide_from_px`) and the
//! current decaying sub-cell offset (`cursor_anim_offset`), so the trail derives
//! its ghost positions purely from those two vectors and adds no state of its
//! own. Because it piggybacks on the slide, it is visible only while
//! `cursor_motion` is also on and a glide is in flight, and it adds **zero**
//! animation wakes — the slide's own bounded wake schedule (`cursor_motion_deadline`)
//! drives every trail frame, and the trail vanishes the instant the slide
//! settles (`cursor_slide_start` returns to `None`). This is the bounded-repaint
//! contract: no perpetual wake is ever armed by the trail (T-TRAIL-2).
//!
//! Echo geometry (D-VE4T-3): the echo sits between the cursor's current
//! animated position (offset `o = cursor_anim_offset`) and the slide origin
//! (offset `f = cursor_slide_from_px`), at a fixed lag fraction back toward the
//! origin — the cells the cursor just passed through. One-cell moves omit the
//! echo so ordinary typing stays crisp; only short moves of two through six
//! cells produce it. The echo intensity is a
//! single half-sine bump over the *remaining* displacement fraction
//! `remain = |o| / |f|`: `sin(remain · π)`, which is `0` at the slide start
//! (`remain == 1`, the echo coincides with the cursor) and `0` at the slide end
//! (`remain == 0`), peaking mid-glide. So the echo never piles opaque on the
//! cursor cell at either endpoint, and the cursor block — drawn AFTER the
//! overlay quads (ID1 reorder) — composites over them without any double-blend
//! of the cursor cell (T-TRAIL-3).
//!
//! The linked strength profile sets echo alpha and lag. Balanced preserves the
//! original `0.09` / `0.70` treatment; every profile decays fully at rest.
//!
//! Off-path contract (`cursor_trail = false`):
//! [`App::paint_cursor_trail_quads`] returns before emitting any quad and
//! [`App::cursor_trail_overlay_signature`] is constant `Inert`, so disabling the
//! feature emits no geometry and does not source animation work.

use super::overlay_registry::OverlayCtx;
use super::*;
use crate::settings::CursorTrailStrength;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CursorTrailProfile {
    pub(super) echo_alpha: f32,
    pub(super) echo_lag: f32,
    /// Continuous first-order response of the direction-facing cursor edges.
    pub(super) follower_leading_rate: f32,
    /// Slower response of the opposite edges, which creates the elastic body.
    pub(super) follower_trailing_rate: f32,
    /// Maximum presented width/height as a multiple of the settled shape.
    pub(super) follower_max_stretch: f32,
    /// Hard upper bound for a follower flight after the latest retarget.
    pub(super) follower_max_settle: Duration,
}

pub(super) fn cursor_trail_profile(strength: CursorTrailStrength) -> CursorTrailProfile {
    match strength {
        CursorTrailStrength::Subtle => CursorTrailProfile {
            echo_alpha: 0.05,
            echo_lag: 0.62,
            follower_leading_rate: 42.0,
            follower_trailing_rate: 34.0,
            follower_max_stretch: 1.5,
            follower_max_settle: Duration::from_millis(180),
        },
        CursorTrailStrength::Balanced => CursorTrailProfile {
            echo_alpha: 0.09,
            echo_lag: 0.70,
            follower_leading_rate: 36.0,
            follower_trailing_rate: 24.0,
            follower_max_stretch: 2.25,
            follower_max_settle: Duration::from_millis(240),
        },
        CursorTrailStrength::Expressive => CursorTrailProfile {
            echo_alpha: 0.13,
            echo_lag: 0.78,
            follower_leading_rate: 32.0,
            follower_trailing_rate: 16.0,
            follower_max_stretch: 3.0,
            follower_max_settle: Duration::from_millis(340),
        },
    }
}

/// Ordinary one-cell cursor steps stay clean; the trail begins at two cells.
const MIN_TRAIL_CELLS: f32 = 2.0;

/// Below this displacement magnitude (in pixels) the slide is treated as
/// degenerate (no meaningful path) and the trail emits nothing — guards the
/// `|o| / |f|` ratio against division by a near-zero origin displacement.
const MIN_TRAIL_PX: f32 = 0.5;

impl App {
    /// Emit the cursor-trail ghost quads for this frame.
    ///
    /// No-op unless `cursor_trail` is on, the cursor is drawn this frame, and a
    /// cursor slide is actually in flight (`cursor_slide_start.is_some()`), so
    /// the trail only ever appears layered on the existing slide animation. Each
    /// qualifying move emits one cursor-cell-sized [`SolidQuad`] in the theme
    /// cursor-role color at a position lagging behind the gliding cursor, with a
    /// half-sine intensity that is zero at both slide endpoints. Drawn before
    /// the cursor block (cursor-layer overlay), so the cursor composites on top.
    pub(in crate::native) fn paint_cursor_trail_quads(
        &self,
        ctx: &OverlayCtx,
        out: &mut Vec<SolidQuad>,
    ) {
        if self.settings.reduced_motion
            || !self.settings.cursor_trail
            || !ctx.cursor_visible
            || self.cursor_slide_start.is_none()
        {
            return;
        }
        let cols = ctx.grid.columns;
        let rows = ctx.grid.rows;
        if cols == 0 || rows == 0 {
            return;
        }
        // Full slide displacement (origin offset) and the current decaying
        // offset. A degenerate (zero-length) path emits nothing.
        let f = self.cursor_slide_from_px;
        let o = self.cursor_anim_offset;
        let fmag = (f[0] * f[0] + f[1] * f[1]).sqrt();
        if fmag < MIN_TRAIL_PX {
            return;
        }
        let cell_w = ctx.cell.width as f32;
        let cell_h = ctx.cell.height as f32;
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }
        let move_cells = f[0].abs() / cell_w + f[1].abs() / cell_h;
        if !(MIN_TRAIL_CELLS..=super::cursor_frame::MAX_SLIDE_CELLS).contains(&move_cells) {
            return;
        }
        let omag = (o[0] * o[0] + o[1] * o[1]).sqrt();
        // Remaining-displacement fraction: 1.0 at the slide start (cursor at the
        // origin), 0.0 at the end (cursor settled). The half-sine bump peaks
        // mid-glide and is zero at both endpoints.
        let remain = (omag / fmag).clamp(0.0, 1.0);
        let intensity = (remain * std::f32::consts::PI).sin();
        if intensity <= f32::EPSILON {
            return;
        }
        // Destination cell origin (the logical cursor cell). Defensive clamp
        // mirroring `push_cursor` / the glow.
        let col = ctx.cursor.column.min(cols - 1) as f32;
        let row = ctx.cursor.row.min(rows - 1) as f32;
        let pad = ctx.window_padding.as_f32();
        let x0 = pad + col * cell_w;
        let y0 = pad + row * cell_h;
        // Trail color = theme cursor-role color in linear RGB (matches the
        // cursor block's default color basis); echo alpha set below.
        let (r, g, b) = self.effective_theme.cursor;
        let base = text::foreground_linear(Color::Rgb(r, g, b));
        // Echo offset = current offset lerped toward the origin offset by the
        // fixed lag (the path the cursor just traversed).
        let profile = cursor_trail_profile(self.settings.cursor_trail_strength);
        let gx = o[0] + (f[0] - o[0]) * profile.echo_lag;
        let gy = o[1] + (f[1] - o[1]) * profile.echo_lag;
        let mut color = base;
        color[3] = profile.echo_alpha * intensity;
        out.push(SolidQuad {
            rect: [x0 + gx, y0 + gy, x0 + gx + cell_w, y0 + gy + cell_h],
            color,
        });
    }

    /// Render-cache fragment. Constant `CursorTrail { phase: 0 }` while the trail
    /// is enabled, `Inert` while off — mirroring the cursor-glow contributor: the
    /// off→on toggle flips `Inert` ↔ `CursorTrail`, forcing one rebuild so the
    /// trail appears/disappears without a stale cache, while the trail's
    /// per-frame motion already reclassifies through the cursor `anim` key (the
    /// slide offset changes every frame), which resolves to the cheap CursorOnly
    /// path that re-passes the freshly recomputed overlay quads. Keeping this a
    /// frame-to-frame constant therefore avoids forcing a Full rebuild on every
    /// slide frame.
    pub(super) fn cursor_trail_overlay_signature(&self) -> OverlayFragment {
        if self.settings.reduced_motion || !self.settings.cursor_trail {
            return OverlayFragment::Inert;
        }
        OverlayFragment::CursorTrail { phase: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Terminal;

    const CELL_W: u32 = 8;
    const CELL_H: u32 = 16;
    const ROWS: usize = 6;
    const COLS: usize = 40;

    fn build_app() -> Option<App> {
        let d = Dimensions::new(COLS, ROWS);
        let session = crate::native::test_support::spawn_test_pause_shell(d).ok()?;
        let writer: crate::native::pty::PtyWriter =
            Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(d.columns, d.rows)));
        let pty = Arc::new(Mutex::new(session));
        let mut app = App::new(
            crate::native::options::NativeOptions::default(),
            terminal,
            writer,
            pty,
            Settings::default(),
            crate::settings::SettingsReloader::for_current_process(Instant::now()),
        );
        app.grid = d;
        app.set_test_cell_for_test(CellSize {
            width: CELL_W,
            height: CELL_H,
            baseline: 0,
        });
        Some(app)
    }

    fn ctx_at(app: &App, cursor: Position, cursor_visible: bool, now: Instant) -> OverlayCtx {
        app.overlay_ctx(
            app.scrollback_len(),
            CellSize {
                width: CELL_W,
                height: CELL_H,
                baseline: 0,
            },
            cursor,
            cursor_visible,
            now,
        )
    }

    /// Arm a horizontal slide directly at a chosen remaining fraction.
    fn arm_slide(app: &mut App, cells: f32, remain: f32) {
        let full = [cells * CELL_W as f32, 0.0];
        app.cursor_slide_from_px = full;
        app.cursor_anim_offset = [full[0] * remain, 0.0];
        app.cursor_slide_start = Some(Instant::now());
    }

    /// Arm a mid-glide 3-column move at the echo's peak intensity.
    fn arm_midslide(app: &mut App) {
        arm_slide(app, 3.0, 0.5);
    }

    fn pos(row: usize, column: usize) -> Position {
        Position { row, column }
    }

    // --- T-TRAIL-1: off-path identity ---------------------------------------

    #[test]
    fn off_path_emits_nothing_and_is_inert() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = false; // exercise the explicit off path
        arm_midslide(&mut app);
        // Even with a slide in flight, the off path emits zero quads.
        let mut quads = Vec::new();
        let ctx = ctx_at(&app, pos(2, 10), true, Instant::now());
        app.paint_cursor_trail_quads(&ctx, &mut quads);
        assert!(quads.is_empty(), "off path emits no trail quads");
        assert_eq!(
            app.cursor_trail_overlay_signature(),
            OverlayFragment::Inert,
            "signature constant-Inert while off"
        );
    }

    // --- only emits while a slide is in flight ------------------------------

    #[test]
    fn no_trail_without_an_active_slide() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        // On, but no slide armed (cursor_slide_start is None) ⇒ nothing.
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), true, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "no slide ⇒ no trail");
        // Signature is non-Inert when on (so a toggle forces one rebuild).
        assert_eq!(
            app.cursor_trail_overlay_signature(),
            OverlayFragment::CursorTrail { phase: 0 }
        );
    }

    #[test]
    fn hidden_cursor_suppresses_trail() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        arm_midslide(&mut app);
        let mut quads = Vec::new();
        // cursor_visible == false (blink off-phase / hidden) ⇒ no trail.
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), false, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "hidden cursor emits no trail");
    }

    // --- T-TRAIL-3: distance gate, one restrained echo ----------------------

    #[test]
    fn one_cell_move_emits_no_echo() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        arm_slide(&mut app, 1.0, 0.5);
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), true, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "ordinary one-cell typing emits no echo");
    }

    #[test]
    fn short_midslide_emits_one_restrained_cursor_color_echo() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        arm_midslide(&mut app);
        let cursor = pos(2, 10);
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, cursor, true, Instant::now()), &mut quads);
        assert_eq!(quads.len(), 1, "a short move emits one echo");
        let (r, g, b) = app.effective_theme.cursor;
        let expect = crate::text::foreground_linear(crate::core::Color::Rgb(r, g, b));
        for q in &quads {
            assert_eq!(
                [q.color[0], q.color[1], q.color[2]],
                [expect[0], expect[1], expect[2]],
                "the echo uses the theme cursor color"
            );
            assert!(q.color[3] > 0.0, "visible mid-glide");
            assert!(
                (q.color[3]
                    - cursor_trail_profile(crate::settings::CursorTrailStrength::Balanced)
                        .echo_alpha)
                    .abs()
                    <= 1e-6,
                "mid-glide reaches the restrained peak alpha: {}",
                q.color[3]
            );
            // Cell-sized quad.
            assert!((q.rect[2] - q.rect[0] - CELL_W as f32).abs() < 1e-3);
            assert!((q.rect[3] - q.rect[1] - CELL_H as f32).abs() < 1e-3);
        }
        // Ghosts lag behind the destination cell (to its left, toward origin).
        let pad = 0.0_f32;
        let dest_x = pad + cursor.column as f32 * CELL_W as f32;
        assert!(
            quads.iter().all(|q| q.rect[0] > dest_x - 1e-3),
            "the echo sits between current offset and origin, never past the destination"
        );
    }

    #[test]
    fn two_and_six_cell_moves_each_emit_one_echo() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        for cells in [MIN_TRAIL_CELLS, super::super::cursor_frame::MAX_SLIDE_CELLS] {
            arm_slide(&mut app, cells, 0.5);
            let mut quads = Vec::new();
            app.paint_cursor_trail_quads(
                &ctx_at(&app, pos(2, 10), true, Instant::now()),
                &mut quads,
            );
            assert_eq!(quads.len(), 1, "{cells}-cell move emits one echo");
        }
    }

    #[test]
    fn snapped_large_move_emits_no_echo() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        arm_slide(
            &mut app,
            super::super::cursor_frame::MAX_SLIDE_CELLS + 1.0,
            0.5,
        );
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), true, Instant::now()), &mut quads);
        assert!(
            quads.is_empty(),
            "a move beyond the snap threshold emits no echo"
        );
    }

    #[test]
    fn reduced_motion_emits_no_echo() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        app.settings.reduced_motion = true;
        arm_midslide(&mut app);
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), true, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "reduced motion emits no echo");
    }

    // --- T-TRAIL-2: intensity is zero at both slide endpoints ---------------

    #[test]
    fn endpoints_emit_no_visible_trail() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        let full = [3.0 * CELL_W as f32, 0.0];
        app.cursor_slide_from_px = full;
        app.cursor_slide_start = Some(Instant::now());

        // Slide start: offset == full (remain == 1) ⇒ sin(π) == 0 ⇒ no quads.
        app.cursor_anim_offset = full;
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), true, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "no trail at the slide start endpoint");

        // Slide end: offset == 0 (remain == 0) ⇒ sin(0) == 0 ⇒ no quads.
        app.cursor_anim_offset = [0.0, 0.0];
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), true, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "no trail at the slide end endpoint");
    }

    #[test]
    fn degenerate_zero_length_slide_emits_nothing() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = true;
        app.cursor_slide_from_px = [0.0, 0.0];
        app.cursor_anim_offset = [0.0, 0.0];
        app.cursor_slide_start = Some(Instant::now());
        let mut quads = Vec::new();
        app.paint_cursor_trail_quads(&ctx_at(&app, pos(2, 10), true, Instant::now()), &mut quads);
        assert!(quads.is_empty(), "degenerate slide path emits nothing");
    }

    // --- signature toggles with the knob ------------------------------------

    #[test]
    fn signature_flips_with_the_knob() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.cursor_trail = false;
        assert_eq!(app.cursor_trail_overlay_signature(), OverlayFragment::Inert);
        app.settings.cursor_trail = true;
        assert_eq!(
            app.cursor_trail_overlay_signature(),
            OverlayFragment::CursorTrail { phase: 0 }
        );
    }
}

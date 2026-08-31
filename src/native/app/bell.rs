// SPDX-License-Identifier: GPL-3.0-only
//! BELL (`0x07`) presentation. The core latches a bell (see
//! [`crate::core::Terminal::take_bell`]); this module decides what the window
//! does with it, gated by [`crate::settings::BellMode`]:
//!
//! - **Urgent** (default): request window user-attention when unfocused. No
//!   pixels change while focused, so a foreground shell never flashes on
//!   tab-completion bells; a finished background job pings the taskbar.
//! - **Visual**: a brief, readability-safe full-viewport flash that decays to
//!   transparent over [`FLASH_DURATION`]. The underlying cells render at full
//!   opacity beneath the fading tint (obscure-then-reveal), so no frame drops
//!   text below the readability floor.
//! - **All**: both. **Off**: drain and ignore.
//!
//! Off-path contract: with no flash in flight, `bell_flash_start` is `None`,
//! [`App::bell_flash_deadline`] is `None` (no extra wakes),
//! [`App::paint_bell_flash_quad`] emits nothing, and
//! [`App::bell_flash_overlay_signature`] is constant `Inert` — the default
//! render path is byte-identical to before this feature existed.

use winit::window::{UserAttentionType, Window};

use super::overlay_registry::OverlayCtx;
use super::*;

/// Visual-flash ramp: the tint is fully revealed (transparent) this long after
/// the bell. Short enough to read as a blink, long enough to register.
pub(super) const FLASH_DURATION: Duration = Duration::from_millis(150);
/// Animation cadence (~60 fps) while a flash is decaying.
const FLASH_FRAME: Duration = Duration::from_millis(16);
/// Peak opacity of the flash tint at the instant of the bell.
const FLASH_PEAK_ALPHA: f32 = 0.30;

/// One platform attention request is enough for an unfocused window episode.
/// A focus gain clears the latch and permits a later episode to request again.
#[derive(Debug, Default)]
pub(super) struct BellAttentionLatch {
    requested: bool,
}

impl BellAttentionLatch {
    /// Returns whether this bell should issue the platform attention request.
    /// A missing window never consumes an episode because no request can issue.
    pub(super) fn request_due(
        &mut self,
        wants_urgent: bool,
        focused: bool,
        has_window: bool,
    ) -> bool {
        if !wants_urgent || focused || !has_window || self.requested {
            return false;
        }
        self.requested = true;
        true
    }

    /// Clears the request state on focus gain and reports whether the platform
    /// request needs clearing too.
    pub(super) fn rearm_on_focus_gain(&mut self) -> bool {
        let requested = self.requested;
        self.requested = false;
        requested
    }
}

/// Ease-out cubic: fast departure, gentle arrival. Maps `0.0..=1.0` to itself.
fn ease_out_cubic(p: f32) -> f32 {
    let inv = 1.0 - p;
    1.0 - inv * inv * inv
}

impl App {
    /// React to a bell drained from the **active-visible focused pane**: route
    /// urgency (unfocused) and start the visual flash per the active
    /// [`BellMode`]. No-op on the off path.
    pub(in crate::native) fn note_bell(&mut self, now: Instant, window: Option<&Window>) {
        self.request_bell_attention(window);
        if self.settings.bell.wants_visual() {
            self.bell_flash_start = Some(now);
        }
    }

    /// Window urgency for a bell that rang ANYWHERE (any pane, tab, or
    /// workspace), with NO viewport flash — a background bell must never tint
    /// the pane the user is currently looking at (NF21-6). No-op while focused,
    /// on the off / visual-only path, or without a window. The urgency call is
    /// cross-platform (taskbar flash on Windows, dock bounce on macOS, WM
    /// attention hint on Linux).
    pub(in crate::native) fn request_bell_attention(&mut self, window: Option<&Window>) {
        if self.bell_attention.request_due(
            self.settings.bell.wants_urgent(),
            self.focused,
            window.is_some(),
        ) && let Some(window) = window
        {
            window.request_user_attention(Some(UserAttentionType::Informational));
        }
    }

    /// Clear any outstanding portable platform attention request and re-arm a
    /// later unfocused episode. `None` is winit's portable clear operation.
    pub(super) fn rearm_bell_attention_on_focus_gain(&mut self) {
        if self.bell_attention.rearm_on_focus_gain()
            && let Some(window) = self.window.as_ref()
        {
            window.request_user_attention(None);
        }
        self.notification_attention.rearm_on_focus_gain();
    }

    /// Advance the flash clock: clear it once settled, otherwise bump the epoch
    /// so each animation frame reclassifies the render cache. No-op when no
    /// flash is in flight.
    pub(in crate::native) fn update_bell_flash(&mut self, now: Instant) {
        let Some(start) = self.bell_flash_start else {
            return;
        };
        if now.saturating_duration_since(start) >= FLASH_DURATION {
            self.bell_flash_start = None;
        } else {
            self.bell_flash_epoch = self.bell_flash_epoch.wrapping_add(1);
        }
    }

    /// Next flash wake, or `None` once the flash settles (and always `None` on
    /// the off path). Folds into [`App::animation_deadline`].
    pub(super) fn bell_flash_deadline(&self) -> Option<Instant> {
        let start = self.bell_flash_start?;
        Some((start + FLASH_DURATION).min(Instant::now() + FLASH_FRAME))
    }

    /// Render-cache fragment: `BellFlash { epoch }` while a flash decays (the
    /// per-rebuild epoch bump makes every animation frame reclassify), `Inert`
    /// otherwise.
    pub(super) fn bell_flash_overlay_signature(&self) -> OverlayFragment {
        if self.bell_flash_start.is_some() {
            OverlayFragment::BellFlash {
                epoch: self.bell_flash_epoch,
            }
        } else {
            OverlayFragment::Inert
        }
    }

    /// Emit a single full-viewport [`SolidQuad`] whose alpha decays from
    /// [`FLASH_PEAK_ALPHA`] to `0` over [`FLASH_DURATION`] on an ease-out curve.
    /// The tint contrasts the background (light flash on a dark theme, dark on a
    /// light theme) so the blink is visible without a foreground-color input.
    /// No-op when no flash is in flight (the off / urgent-only path).
    pub(in crate::native) fn paint_bell_flash_quad(
        &self,
        ctx: &OverlayCtx,
        out: &mut Vec<SolidQuad>,
    ) {
        let Some(start) = self.bell_flash_start else {
            return;
        };
        let elapsed = ctx.now.saturating_duration_since(start);
        if elapsed >= FLASH_DURATION {
            return;
        }
        let cols = ctx.grid.columns;
        let rows = ctx.grid.rows;
        if cols == 0 || rows == 0 {
            return;
        }
        let p = (elapsed.as_secs_f32() / FLASH_DURATION.as_secs_f32()).min(1.0);
        let alpha = (1.0 - ease_out_cubic(p)) * FLASH_PEAK_ALPHA;
        // Contrast tint from background luminance (linear RGB). Dark bg → white
        // flash; light bg → near-black flash.
        let bg = ctx.clear_color;
        let luma = 0.2126 * bg[0] + 0.7152 * bg[1] + 0.0722 * bg[2];
        let tint = if luma < 0.5 { 1.0 } else { 0.0 };
        let pad = ctx.window_padding.as_f32();
        let content_w = cols as f32 * ctx.cell.width as f32;
        let content_h = rows as f32 * ctx.cell.height as f32;
        out.push(SolidQuad {
            rect: [pad, pad, pad + content_w, pad + content_h],
            color: [tint, tint, tint, alpha],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::BellMode;

    const CELL_W: u32 = 8;
    const CELL_H: u32 = 16;
    const ROWS: usize = 6;
    const COLS: usize = 40;

    #[test]
    fn ease_out_cubic_endpoints() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
    }

    #[test]
    fn bell_attention_latches_once_per_unfocused_episode() {
        let mut latch = BellAttentionLatch::default();

        assert!(
            !latch.request_due(true, true, true),
            "a focused bell cannot consume the next unfocused episode"
        );
        assert!(
            latch.request_due(true, false, true),
            "first bell requests attention"
        );
        assert!(
            !latch.request_due(true, false, true),
            "repeated bells remain suppressed while unfocused"
        );
        assert!(latch.rearm_on_focus_gain(), "focus gain clears the request");
        assert!(
            latch.request_due(true, false, true),
            "the next unfocused episode requests attention"
        );
    }

    #[test]
    fn bell_attention_without_a_window_does_not_latch() {
        let mut latch = BellAttentionLatch::default();

        assert!(!latch.request_due(true, false, false));
        assert!(
            latch.request_due(true, false, true),
            "a later window-backed bell still requests attention"
        );
    }

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

    fn ctx_at(app: &App, now: Instant) -> OverlayCtx {
        app.overlay_ctx(
            app.scrollback_len(),
            CellSize {
                width: CELL_W,
                height: CELL_H,
                baseline: 0,
            },
            crate::core::Position { row: 0, column: 0 },
            true,
            now,
        )
    }

    #[test]
    fn default_mode_is_urgent_and_focused_bell_paints_nothing() {
        let Some(mut app) = build_app() else {
            return;
        };
        assert_eq!(app.settings.bell, BellMode::Urgent);
        app.focused = true;
        let now = Instant::now();
        // Urgent + focused: no flash, no signature change, no quad.
        app.note_bell(now, None);
        assert!(app.bell_flash_start.is_none());
        assert_eq!(app.bell_flash_overlay_signature(), OverlayFragment::Inert);
        let mut out = Vec::new();
        app.paint_bell_flash_quad(&ctx_at(&app, now), &mut out);
        assert!(out.is_empty(), "off/urgent path emits no flash quad");
    }

    #[test]
    fn visual_mode_flashes_then_settles() {
        let Some(mut app) = build_app() else {
            return;
        };
        app.settings.bell = BellMode::Visual;
        let start = Instant::now();
        app.note_bell(start, None);
        assert_eq!(app.bell_flash_start, Some(start));
        // Active flash: signature is BellFlash and a single quad is emitted.
        assert!(matches!(
            app.bell_flash_overlay_signature(),
            OverlayFragment::BellFlash { .. }
        ));
        let mut out = Vec::new();
        app.paint_bell_flash_quad(&ctx_at(&app, start), &mut out);
        assert_eq!(out.len(), 1, "active flash emits one full-viewport quad");
        assert!(out[0].color[3] > 0.0, "peak alpha is positive");
        assert!(app.bell_flash_deadline().is_some());

        // After the ramp, update clears the flash and the path goes inert.
        let after = start + FLASH_DURATION + Duration::from_millis(1);
        app.update_bell_flash(after);
        assert!(app.bell_flash_start.is_none());
        assert_eq!(app.bell_flash_overlay_signature(), OverlayFragment::Inert);
        let mut out2 = Vec::new();
        app.paint_bell_flash_quad(&ctx_at(&app, after), &mut out2);
        assert!(out2.is_empty(), "settled flash emits nothing");
    }
}

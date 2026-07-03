// SPDX-License-Identifier: GPL-3.0-only
//! Shared tab-chrome treatment — the **"Phosphor Flat"** visual language
//! (F4-RESKIN, operator-ruled A+C 2026-07-03) used by BOTH the horizontal
//! [`super::tab_bar`] and the vertical [`super::tab_rail`] widgets.
//!
//! This module owns **color**; the widgets own **layout**. Every function here
//! is pure over theme roles ([`TabBarColors`]) with no GPU/quad/geometry types,
//! so both axes share one treatment and it unit-tests without a device (the
//! F4V2-NF2 "promote shared treatment fns to a shared chrome location" follow-up
//! that the F4-V2 spec called for).
//!
//! ## Phosphor Flat, precisely
//! The redesign dropped the whole outlined-box language (rings, underlines,
//! separators — deleted, not bypassed) after the operator rejected it as
//! "hacked together / cheap". The survey's unanimous law: **the container is
//! invisible; only the active tab is a drawn object.** Hierarchy comes from
//! luminance, not geometry.
//!
//! - **ACTIVE** — a warm `selection` fill (the *bloom-off fallback*, always
//!   emitted so the active tab is identifiable even with CRT/bloom disabled)
//!   plus a bright, bold `foreground` label brightened **above the bloom
//!   threshold** ([`ACTIVE_LABEL_TARGET_LUMA`]) so the existing `bloom.wgsl`
//!   post-process auto-halos it with zero new geometry. The brighten only
//!   applies when the fill is dark (the phosphor/dark-theme case); on a light
//!   theme the plain (dark) foreground is kept for contrast and bloom correctly
//!   no-ops.
//! - **INACTIVE** — bare `inactive`-role labels on the wallpaper-through
//!   background (no fill), dimmed along a **phosphor-persistence luminance
//!   ramp**: tabs nearer the active one glow slightly brighter, distant ones
//!   fade toward [`RAMP_FLOOR`] (a per-slot foreground multiplier).
//! - **HOVER** — the label warms one tier toward the active label
//!   ([`HOVER_LABEL_LIFT`]) and gains a whisper of the selection fill
//!   ([`HOVER_FILL_BLEND`]), always subordinate to the full active treatment.
//!
//! The wallpaper-through background is painted as an explicit `Color::Rgb` of the
//! theme `background` (alpha 1.0, so `cell_bg_opacity` composites it exactly like
//! an empty terminal cell) rather than `Color::Default`, so the multi-pane strip
//! path — which builds its snapshot with `DynamicColors::default()` — resolves
//! it identically to the single-pane path.

use super::tab_bar::TabBarColors;
use crate::theme::{Srgb, relative_luminance};

// ---------------------------------------------------------------------------
// Treatment constants
// ---------------------------------------------------------------------------

/// Target WCAG relative luminance the ACTIVE label is brightened to (on dark
/// fills) so it clears the default bloom threshold (`DEFAULT_BLOOM_THRESHOLD =
/// 0.7`) with real headroom — the bright-pass factor `(y - 0.7)/y` at `0.85` is
/// `≈0.18`, a visible halo — and auto-blooms through `bloom.wgsl` with no new
/// geometry. Above the threshold, not merely at it, so the glow reads.
pub(super) const ACTIVE_LABEL_TARGET_LUMA: f64 = 0.85;

/// Per-tier dimming of the inactive phosphor-persistence ramp: each step of
/// distance from the active tab multiplies the `inactive` role's brightness down
/// by this much (the nearest inactive tab, distance 1, is at full brightness).
pub(super) const RAMP_STEP: f32 = 0.14;

/// Floor for the inactive ramp multiplier so distant tabs fade but never vanish
/// — a phosphor ghost persists.
pub(super) const RAMP_FLOOR: f32 = 0.55;

/// How far a hovered tab's fill is blended from the wallpaper-through background
/// toward the `selection` role — a whisper of the active fill, never as strong.
pub(super) const HOVER_FILL_BLEND: f32 = 0.35;

/// How far a hovered inactive label is warmed from the `inactive` role toward
/// the active label color — "one tier" of lift, subordinate to the active label.
pub(super) const HOVER_LABEL_LIFT: f32 = 0.40;

// ---------------------------------------------------------------------------
// Treatment functions (theme roles → sRGB)
// ---------------------------------------------------------------------------

/// The wallpaper-through background for inactive slots, inter-slot gaps, and the
/// bar band: the theme `background`, painted explicitly so both render paths
/// agree. Composites through `cell_bg_opacity` exactly like an empty terminal
/// cell, so inactive tabs recede into the wallpaper.
pub(super) fn wallpaper_background(colors: TabBarColors) -> Srgb {
    colors.background
}

/// The ACTIVE-slot fill — the `selection` role. This is the **bloom-off
/// fallback**: it is always emitted, so the active tab reads as a warm anchor
/// even when CRT/bloom is disabled (the auto-glow is a bonus on top, not the
/// only signal).
pub(super) fn active_fill(colors: TabBarColors) -> Srgb {
    colors.active_bg
}

/// The ACTIVE-slot label color. On a dark fill (the phosphor/dark-theme case)
/// the `foreground` role is brightened toward white until it clears the bloom
/// threshold ([`ACTIVE_LABEL_TARGET_LUMA`]), so it auto-halos through
/// `bloom.wgsl`. On a light fill the plain (dark) `foreground` is kept for
/// contrast — bloom is a bright-on-dark effect and correctly no-ops there.
pub(super) fn active_label(colors: TabBarColors) -> Srgb {
    if relative_luminance(colors.active_bg) < 0.5 {
        brighten_to_luma(colors.foreground, ACTIVE_LABEL_TARGET_LUMA)
    } else {
        colors.foreground
    }
}

/// The multiplier applied to the `inactive` role for a tab `distance` slots from
/// the active one (phosphor-persistence ramp). Distance 1 (adjacent) → `1.0`
/// (full `inactive` brightness); each further step dims by [`RAMP_STEP`], floored
/// at [`RAMP_FLOOR`]. Monotonically non-increasing in `distance`.
pub(super) fn inactive_ramp_multiplier(distance: usize) -> f32 {
    let steps = distance.saturating_sub(1) as f32;
    (1.0 - steps * RAMP_STEP).max(RAMP_FLOOR)
}

/// The INACTIVE-slot label color: the `inactive` role scaled by the
/// phosphor-persistence ramp for its `distance` from the active tab.
pub(super) fn inactive_label(colors: TabBarColors, distance: usize) -> Srgb {
    scale_srgb(colors.inactive, inactive_ramp_multiplier(distance))
}

/// The HOVER label color: the `inactive` role warmed one tier toward the active
/// label. Brighter/more prominent than any resting inactive label, always
/// subordinate to the full active label.
pub(super) fn hover_label(colors: TabBarColors) -> Srgb {
    blend_srgb(colors.inactive, active_label(colors), HOVER_LABEL_LIFT)
}

/// The HOVER fill color: the wallpaper-through background blended a whisper
/// toward the `selection` role — subordinate to the active fill.
pub(super) fn hover_fill(colors: TabBarColors) -> Srgb {
    blend_srgb(
        wallpaper_background(colors),
        colors.active_bg,
        HOVER_FILL_BLEND,
    )
}

// ---------------------------------------------------------------------------
// Pure color helpers
// ---------------------------------------------------------------------------

/// Blend two sRGB colors: `a*(1-t) + b*t` per channel, `t` clamped to `[0,1]`.
/// A gamma-naive sRGB mix — fine for subtle chrome tints (not a color-managed
/// image op).
pub(super) fn blend_srgb(a: Srgb, b: Srgb, t: f32) -> Srgb {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        (f32::from(x) * (1.0 - t) + f32::from(y) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Scale an sRGB color's brightness by `f` (channel-wise multiply, clamped).
/// `f < 1.0` dims toward black; the phosphor ramp uses this as its per-slot
/// foreground multiplier. Monotonic in `f`, so a larger `f` is never dimmer.
pub(super) fn scale_srgb(c: Srgb, f: f32) -> Srgb {
    let s = |x: u8| (f32::from(x) * f).round().clamp(0.0, 255.0) as u8;
    (s(c.0), s(c.1), s(c.2))
}

/// Brighten `base` toward white until its relative luminance reaches `target`
/// (already there → returned unchanged). Monotonic bisection on the blend
/// fraction; deterministic and cheap (only the active label uses it, once per
/// render). Preserves as much of the theme hue as the target allows.
pub(super) fn brighten_to_luma(base: Srgb, target: f64) -> Srgb {
    if relative_luminance(base) >= target {
        return base;
    }
    const WHITE: Srgb = (255, 255, 255);
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if relative_luminance(blend_srgb(base, WHITE, mid)) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    blend_srgb(base, WHITE, hi)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const COLORS: TabBarColors = TabBarColors {
        foreground: (0xCC, 0xCC, 0xCC),
        background: (0x10, 0x10, 0x10),
        inactive: (0x66, 0x66, 0x66),
        active_bg: (0x40, 0x60, 0x90),
    };

    #[test]
    fn brighten_to_luma_reaches_the_target_and_is_monotone() {
        // A dim base is lifted to at least the target luminance...
        let base = (0x40, 0x30, 0x00); // dark amber
        let out = brighten_to_luma(base, ACTIVE_LABEL_TARGET_LUMA);
        assert!(
            relative_luminance(out) >= ACTIVE_LABEL_TARGET_LUMA - 1e-3,
            "brightened luma {} < target {}",
            relative_luminance(out),
            ACTIVE_LABEL_TARGET_LUMA
        );
        // ...and an already-bright base is returned unchanged.
        let bright = (0xF0, 0xF0, 0xF0);
        assert_eq!(
            brighten_to_luma(bright, 0.5),
            bright,
            "already above target → unchanged"
        );
    }

    #[test]
    fn active_label_clears_the_bloom_threshold_on_dark_fills() {
        // On the dark test palette the active label is brightened above the
        // default bloom threshold (0.7), so it auto-halos through bloom.wgsl.
        let luma = relative_luminance(active_label(COLORS));
        assert!(
            luma >= 0.7,
            "active label luma {luma} must clear the bloom threshold 0.7"
        );
    }

    #[test]
    fn active_label_keeps_dark_foreground_on_light_fills() {
        // Light theme: a light selection fill → keep the (dark) foreground for
        // contrast, do NOT brighten it into the fill (bloom no-ops correctly).
        let light = TabBarColors {
            foreground: (0x20, 0x20, 0x20),
            background: (0xF0, 0xF0, 0xF0),
            inactive: (0x80, 0x80, 0x80),
            active_bg: (0xC0, 0xD0, 0xF0),
        };
        assert_eq!(
            active_label(light),
            light.foreground,
            "light fill keeps the dark foreground label"
        );
    }

    #[test]
    fn inactive_ramp_is_monotonically_non_increasing_and_floored() {
        let mut prev = f32::INFINITY;
        for d in 1..12 {
            let m = inactive_ramp_multiplier(d);
            assert!(m <= prev + 1e-6, "ramp must not brighten with distance");
            assert!(m >= RAMP_FLOOR - 1e-6, "ramp floored at {RAMP_FLOOR}");
            prev = m;
        }
        assert!(
            (inactive_ramp_multiplier(1) - 1.0).abs() < 1e-6,
            "adjacent inactive tab is at full inactive brightness"
        );
    }

    #[test]
    fn inactive_label_luminance_ramps_down_with_distance() {
        // Nearer-to-active labels are at least as bright as more distant ones.
        let mut prev = f64::INFINITY;
        for d in 1..8 {
            let luma = relative_luminance(inactive_label(COLORS, d));
            assert!(
                luma <= prev + 1e-9,
                "label at distance {d} brighter than nearer"
            );
            prev = luma;
        }
    }

    #[test]
    fn hover_label_is_between_inactive_and_active() {
        // On the dark palette the hover label is brighter than the resting
        // inactive label but never as bright as the active label.
        let rest = relative_luminance(inactive_label(COLORS, 1));
        let hover = relative_luminance(hover_label(COLORS));
        let active = relative_luminance(active_label(COLORS));
        assert!(hover > rest, "hover lifts above the resting inactive label");
        assert!(
            hover < active,
            "hover stays subordinate to the active label"
        );
    }

    #[test]
    fn active_fill_is_the_selection_role() {
        assert_eq!(active_fill(COLORS), COLORS.active_bg);
    }

    /// Minimum luminance-delta floors the retargeted 100-theme guard enforces —
    /// the Phosphor Flat identifiability invariants that replace the deleted
    /// outline-ring contrast test. Well below the smallest real delta measured
    /// across the built-in themes, so a future theme/role edit that made the
    /// active fill or the labels indistinguishable trips this guard.
    const MIN_ACTIVE_FILL_BG_DELTA: f64 = 0.004;
    const MIN_ACTIVE_LABEL_FILL_DELTA: f64 = 0.02;
    const MIN_INACTIVE_LABEL_BG_DELTA: f64 = 0.01;

    #[test]
    fn every_builtin_theme_keeps_phosphor_flat_identifiable() {
        // Retargeted from the outline-ring era's `every_builtin_theme_keeps_rings
        // _visible_against_the_band`: instead of ring-vs-band contrast, guard the
        // three Phosphor Flat identity signals for every built-in theme —
        //   (1) the active FILL is distinguishable from the wallpaper-through
        //       background (the active tab is locatable even with bloom off),
        //   (2) the active LABEL pops off its fill (readable),
        //   (3) the nearest inactive LABEL is visible on the background.
        for theme in crate::theme::all() {
            let colors = TabBarColors {
                foreground: theme.foreground,
                background: theme.background,
                inactive: theme.inactive,
                active_bg: theme.selection,
            };
            let bg_luma = relative_luminance(wallpaper_background(colors));
            let fill_luma = relative_luminance(active_fill(colors));
            let active_lbl_luma = relative_luminance(active_label(colors));
            let inactive_lbl_luma = relative_luminance(inactive_label(colors, 1));

            assert!(
                (fill_luma - bg_luma).abs() >= MIN_ACTIVE_FILL_BG_DELTA,
                "{}: active fill vs background delta {:.4} < {MIN_ACTIVE_FILL_BG_DELTA} \
                 — active tab not locatable",
                theme.name,
                (fill_luma - bg_luma).abs()
            );
            assert!(
                (active_lbl_luma - fill_luma).abs() >= MIN_ACTIVE_LABEL_FILL_DELTA,
                "{}: active label vs fill delta {:.4} < {MIN_ACTIVE_LABEL_FILL_DELTA} \
                 — active label unreadable on its fill",
                theme.name,
                (active_lbl_luma - fill_luma).abs()
            );
            assert!(
                (inactive_lbl_luma - bg_luma).abs() >= MIN_INACTIVE_LABEL_BG_DELTA,
                "{}: inactive label vs background delta {:.4} < {MIN_INACTIVE_LABEL_BG_DELTA} \
                 — inactive label invisible",
                theme.name,
                (inactive_lbl_luma - bg_luma).abs()
            );
        }
    }
}

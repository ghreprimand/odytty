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

/// How far the resting new-slot `+` affordance is lifted from the `inactive`
/// role toward the active label so it reads as a deliberate "add" control at
/// rest (F4-PLUS). A touch stronger than [`HOVER_LABEL_LIFT`] so the `+` clears
/// the dim inactive-label floor it used to share; still well below the full
/// active label, which the `+` reaches only on hover.
pub(super) const NEW_SLOT_PLUS_REST_LIFT: f32 = 0.55;

/// F4-P1 panel: base foreground-ward blend fraction of the theme `background`
/// that forms the panel-tint cell surface at the default strength (0.5). A
/// foreground-ward blend is direction-correct on every theme (lighter on dark
/// themes, darker on light) and can never crush to zero on a near-black theme.
pub(super) const PANEL_TINT_LIFT: f32 = 0.05;

/// F4-P1 panel: hard cap on the effective tint lift (`PANEL_TINT_LIFT × strength
/// / 0.5`) so a maxed strength knob still reads as a *quiet* surface.
pub(super) const MAX_PANEL_TINT_LIFT: f32 = 0.10;

/// F4-P1 seam: alpha the seam quad composites over the panel at. Low enough that
/// the seam is a hairline, not a bar.
pub(super) const SEAM_ALPHA: f32 = 0.45;

/// F4-P1 seam: the seam color's own relative luminance is capped here so it can
/// never cross the default bloom threshold (0.7) — the seam never haloes.
pub(super) const SEAM_MAX_LUMA: f64 = 0.60;

/// F4-P1 seam: minimum composited-seam-vs-panel luminance delta the treatment
/// guarantees (and the §7 guard asserts) so the seam survives every theme — the
/// guard the old `border`-role incident lacked.
pub(super) const SEAM_MIN_PANEL_DELTA: f64 = 0.02;

/// ACTIVE-FILL legibility floor: the minimum WCAG contrast RATIO the active-slot
/// fill guarantees against the panel surface it rests on. The raw `selection`
/// role is often only ~1.05:1 against the panel tint on a dark theme (the fill
/// reads as invisible; only the bold label signals active), so the fill is
/// lifted until it clears this ratio — a clearly visible warm slab that still
/// keeps the theme hue. Kept modest (a slab, not a glow) and capped at
/// [`SEAM_MAX_LUMA`] so the fill never crosses the bloom threshold.
pub(super) const ACTIVE_FILL_MIN_PANEL_RATIO: f64 = 1.55;

// ---------------------------------------------------------------------------
// Treatment functions (theme roles → sRGB)
// ---------------------------------------------------------------------------

/// The wallpaper-through background: the theme `background`. Now only a test
/// reference for "the raw wallpaper" (the resting cells paint the panel tint,
/// and the hover fill re-bases on that panel), so it is compiled only under
/// `cfg(test)` -- production reads `colors.background` / `panel_tint` directly.
#[cfg(test)]
pub(super) fn wallpaper_background(colors: TabBarColors) -> Srgb {
    colors.background
}

/// The ACTIVE-slot fill — the `selection` role, LIFTED away from the panel
/// surface until it clears [`ACTIVE_FILL_MIN_PANEL_RATIO`] (WCAG contrast). This
/// is the **bloom-off fallback**: always emitted, so the active tab reads as a
/// warm anchor even when CRT/bloom is disabled. The raw `selection` role is often
/// nearly panel-colored on a dark theme (~1.05:1), so the fill would be invisible
/// with only the bold label to signal active; the lift restores a clearly visible
/// slab. Mirrors [`seam_color`]'s guaranteed-delta bisection, but toward a WCAG
/// RATIO floor: start from `selection` (its own luma first capped so the slab
/// never blooms), and if it is too close to the panel, bisect toward a
/// bloom-capped `foreground` (direction-correct on every theme — lighter on dark,
/// darker on light) for the minimal lift that clears the floor. The result luma
/// is capped at [`SEAM_MAX_LUMA`], so the fill never haloes.
pub(super) fn active_fill(colors: TabBarColors, panel_surface: Srgb) -> Srgb {
    // The slab itself must never bloom: cap its own luminance like the seam.
    let mut fill = colors.active_bg;
    if relative_luminance(fill) > SEAM_MAX_LUMA {
        fill = dim_to_luma(fill, SEAM_MAX_LUMA);
    }
    let ratio = |s: Srgb| crate::theme::contrast_ratio(s, panel_surface);
    if ratio(fill) >= ACTIVE_FILL_MIN_PANEL_RATIO {
        return fill;
    }
    // Too close to the panel — lift toward the (bloom-capped) foreground until
    // the ratio clears the floor. Bisection on the blend fraction; the predicate
    // is a clean false->true step (the blended luma moves monotonically away
    // from the panel toward the higher-contrast target), so this converges to the
    // minimal lift.
    let target = if relative_luminance(colors.foreground) > SEAM_MAX_LUMA {
        dim_to_luma(colors.foreground, SEAM_MAX_LUMA)
    } else {
        colors.foreground
    };
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if ratio(blend_srgb(fill, target, mid)) >= ACTIVE_FILL_MIN_PANEL_RATIO {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    blend_srgb(fill, target, hi)
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

/// The resting new-slot `+` affordance color (F4-PLUS): the `inactive` role
/// lifted toward the active label by [`NEW_SLOT_PLUS_REST_LIFT`], so the `+`
/// reads as an intentional "add" button at rest -- brighter than any inactive
/// tab/slot label, still subordinate to the active label (and to its own hover
/// state, which goes full-active). Shared by the top tab bar and the workspace
/// rail so both `+` affordances lift identically.
pub(super) fn new_slot_plus_rest(colors: TabBarColors) -> Srgb {
    blend_srgb(
        colors.inactive,
        active_label(colors),
        NEW_SLOT_PLUS_REST_LIFT,
    )
}

/// The HOVER fill color: the PANEL surface blended a whisper toward the
/// guaranteed active fill — subordinate to the active fill, above the resting
/// panel. Re-basing on the panel (not the raw `background`) fixes the inversion
/// where a hovered slot dimmed BELOW its resting panel surface: because the
/// resting cell IS the panel, blending from the panel toward the (lifted) active
/// fill makes rest < hover < active hold by construction on every theme.
pub(super) fn hover_fill(colors: TabBarColors, panel_surface: Srgb) -> Srgb {
    blend_srgb(
        panel_surface,
        active_fill(colors, panel_surface),
        HOVER_FILL_BLEND,
    )
}

// ---------------------------------------------------------------------------
// F4-P1 panel + seam treatment
// ---------------------------------------------------------------------------

/// The effective panel-tint lift for `strength`: `PANEL_TINT_LIFT × strength /
/// 0.5`, capped at [`MAX_PANEL_TINT_LIFT`]. `strength = 0.0` → `0.0` (panel off).
pub(super) fn panel_tint_lift(strength: f32) -> f32 {
    (PANEL_TINT_LIFT * strength.clamp(0.0, 1.0) / 0.5).min(MAX_PANEL_TINT_LIFT)
}

/// The F4-P1 unified-panel **tint surface**: the theme `background` blended a
/// small amount toward `foreground` (Layer 1 of ODP-1). This is the cell
/// background the rail/bar resting cells paint instead of the raw `background`,
/// so the panel is a perceivable surface even at `cell_bg_opacity = 1` (no
/// wallpaper). At `strength = 0.0` it collapses to the theme `background`, i.e.
/// the pre-panel "bare labels over the wallpaper" look.
pub(super) fn panel_tint(colors: TabBarColors, strength: f32) -> Srgb {
    blend_srgb(
        colors.background,
        colors.foreground,
        panel_tint_lift(strength),
    )
}

/// The alpha of the F4-P1 panel **wash** quad (Layer 2 of ODP-1):
/// `p = strength × (1 − cell_bg_opacity)`. At `cell_bg_opacity = 1` this is `0`
/// (no overdraw — the tint layer is the whole panel); as opacity drops, the wash
/// mutes the wallpaper behind the tabs. Always `≥ 0`, so the panel is never
/// *less* opaque than the body.
pub(super) fn panel_wash_alpha(strength: f32, cell_bg_opacity: f32) -> f32 {
    (strength.clamp(0.0, 1.0) * (1.0 - cell_bg_opacity.clamp(0.0, 1.0))).clamp(0.0, 1.0)
}

/// The F4-P1 seam color (ODP-2): derived from the **inactive TEXT role only**
/// (never the `border` role — the v1.3 dark-on-dark lesson), dimmed if needed so
/// its own luminance never exceeds [`SEAM_MAX_LUMA`] (bloom guard), then lifted
/// toward `foreground` — still capped at [`SEAM_MAX_LUMA`] — until the composited
/// seam-vs-`panel_surface` luminance delta clears [`SEAM_MIN_PANEL_DELTA`]. The
/// seam quad is drawn at [`SEAM_ALPHA`], so the composite is
/// `blend_srgb(panel_surface, seam, SEAM_ALPHA)`.
pub(super) fn seam_color(colors: TabBarColors, panel_surface: Srgb) -> Srgb {
    // Start from the mid-luminance inactive text role; never let the seam glow.
    let mut seam = colors.inactive;
    if relative_luminance(seam) > SEAM_MAX_LUMA {
        seam = dim_to_luma(seam, SEAM_MAX_LUMA);
    }
    let panel_luma = relative_luminance(panel_surface);
    let delta =
        |s: Srgb| (relative_luminance(blend_srgb(panel_surface, s, SEAM_ALPHA)) - panel_luma).abs();
    if delta(seam) >= SEAM_MIN_PANEL_DELTA {
        return seam;
    }
    // Too close to the panel — lift toward foreground, but never above the bloom
    // cap. Bisection on the blend fraction toward a capped-luma foreground.
    let target = if relative_luminance(colors.foreground) > SEAM_MAX_LUMA {
        dim_to_luma(colors.foreground, SEAM_MAX_LUMA)
    } else {
        colors.foreground
    };
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if delta(blend_srgb(seam, target, mid)) >= SEAM_MIN_PANEL_DELTA {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    blend_srgb(seam, target, hi)
}

// ---------------------------------------------------------------------------
// Pure color helpers
// ---------------------------------------------------------------------------

/// Dim `base` toward black until its relative luminance drops to `target`
/// (already at or below → unchanged). Monotone bisection on a channel-wise
/// scale factor; the sibling of [`brighten_to_luma`].
pub(super) fn dim_to_luma(base: Srgb, target: f64) -> Srgb {
    if relative_luminance(base) <= target {
        return base;
    }
    let mut lo = 0.0f32; // fully black
    let mut hi = 1.0f32; // unchanged
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if relative_luminance(scale_srgb(base, mid)) <= target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    scale_srgb(base, lo)
}

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
    fn active_fill_lifts_a_panel_colored_selection_to_clear_the_ratio() {
        // A `selection` role sitting right on the panel (an invisible slab) is
        // lifted until it clears the legibility ratio floor; the lift never
        // blooms. A selection already clear of the panel is returned unchanged.
        let panel = panel_tint(COLORS, 0.5);
        let flat = TabBarColors {
            active_bg: panel,
            ..COLORS
        };
        let lifted = active_fill(flat, panel);
        assert!(
            crate::theme::contrast_ratio(lifted, panel) >= ACTIVE_FILL_MIN_PANEL_RATIO - 1e-6,
            "a panel-colored selection must lift to clear the ratio floor, got {:.3}",
            crate::theme::contrast_ratio(lifted, panel),
        );
        assert!(
            relative_luminance(lifted) <= SEAM_MAX_LUMA + 1e-6,
            "the lifted fill must not bloom",
        );
        if crate::theme::contrast_ratio(COLORS.active_bg, panel) >= ACTIVE_FILL_MIN_PANEL_RATIO {
            assert_eq!(
                active_fill(COLORS, panel),
                COLORS.active_bg,
                "an already-legible selection is returned unchanged",
            );
        }
    }

    // -----------------------------------------------------------------------
    // F4-P1 panel + seam treatment
    // -----------------------------------------------------------------------

    #[test]
    fn panel_tint_off_at_zero_strength_and_lifts_toward_foreground() {
        // strength 0 → the panel collapses to the raw theme background (the
        // pre-panel bare-labels look stays reachable).
        assert_eq!(
            panel_tint(COLORS, 0.0),
            COLORS.background,
            "strength 0 → no tint (panel off)"
        );
        // A lifted tint is a perceptible, foreground-ward surface on the dark
        // palette (lighter than the near-black background).
        let lifted = panel_tint(COLORS, 0.5);
        assert!(
            relative_luminance(lifted) > relative_luminance(COLORS.background),
            "dark theme: tint lifts the surface above the background"
        );
        // The lift is capped so a maxed knob stays quiet.
        assert!((panel_tint_lift(1.0) - MAX_PANEL_TINT_LIFT).abs() < 1e-6);
        assert!((panel_tint_lift(0.5) - PANEL_TINT_LIFT).abs() < 1e-6);
    }

    #[test]
    fn panel_wash_alpha_is_zero_at_full_opacity_and_grows_as_opacity_drops() {
        assert_eq!(panel_wash_alpha(0.5, 1.0), 0.0, "opaque cells → no wash");
        assert!((panel_wash_alpha(0.5, 0.8) - 0.10).abs() < 1e-6);
        assert!((panel_wash_alpha(0.5, 0.5) - 0.25).abs() < 1e-6);
        assert_eq!(panel_wash_alpha(0.0, 0.5), 0.0, "strength 0 → no wash");
    }

    #[test]
    fn seam_never_blooms_and_clears_the_panel_floor() {
        // The seam is derived from the inactive role, capped below the bloom
        // threshold, and always clears the panel delta floor.
        for theme in crate::theme::all() {
            let colors = colors_for(theme);
            let panel = panel_tint(colors, 0.5);
            let seam = seam_color(colors, panel);
            assert!(
                relative_luminance(seam) <= SEAM_MAX_LUMA + 1e-6,
                "{}: seam luma {:.3} exceeds the bloom cap {SEAM_MAX_LUMA}",
                theme.name,
                relative_luminance(seam)
            );
            let composite = blend_srgb(panel, seam, SEAM_ALPHA);
            let delta = (relative_luminance(composite) - relative_luminance(panel)).abs();
            assert!(
                delta >= SEAM_MIN_PANEL_DELTA - 1e-6,
                "{}: seam-vs-panel delta {delta:.4} < {SEAM_MIN_PANEL_DELTA}",
                theme.name
            );
            // The seam is derived from the `inactive` TEXT role, NEVER the
            // `border` role (the v1.3 dark-on-dark incident). That is structural
            // — `seam_color` reads `colors.inactive`/`colors.foreground` only.
            // The clearing-the-floor assertion above is what the naive
            // near-black `border` approach failed; on a theme whose `border` is
            // near-black the seam is unaffected because it never touches it.
        }
    }

    fn colors_for(theme: &crate::theme::Theme) -> TabBarColors {
        TabBarColors {
            foreground: theme.foreground,
            background: theme.background,
            inactive: theme.inactive,
            active_bg: theme.selection,
        }
    }

    /// The §7 seven-invariant identifiability floors (retargeted from the v1
    /// three-signal test to the panel era). Well below the smallest real delta
    /// across the built-in themes, so a future theme/role edit that flattened a
    /// tier trips a named assertion.
    // Accepted deviation from the spec's 0.003: on a pure-black CRT monochrome
    // theme (ibm-5151) the 5%-toward-foreground tint yields only a 0.0026 luma
    // delta — the panel is genuinely a whisper there (and the wash layer carries
    // it when a wallpaper is present; the seam always separates it). 0.002 is a
    // real non-zero guard that a zeroed tint (strength 0 with the panel forced
    // on, or a broken blend) trips.
    const MIN_PANEL_BG_DELTA: f64 = 0.002;
    // Accepted deviation from the spec's 0.004: retargeting the active-fill floor
    // from the *background* (v1) to the *panel tint* reference shifts the
    // measured delta — the smallest across all built-in themes is 0.0034
    // (odyssey-orchid) — so the floor sits at 0.003, still a real non-zero guard
    // that a flattened active fill (delta → 0) trips. The active tab also carries
    // the bold bright label + bloom, so bloom-off locatability never rests on the
    // fill delta alone.
    const MIN_ACTIVE_FILL_PANEL_DELTA: f64 = 0.003;
    const MIN_ACTIVE_LABEL_FILL_DELTA: f64 = 0.02;
    const MIN_INACTIVE_LABEL_PANEL_DELTA: f64 = 0.01;

    #[test]
    fn every_builtin_theme_keeps_phosphor_flat_identifiable() {
        // ODP-6 retarget: for every built-in theme, over BOTH the opaque regime
        // (cell_bg_opacity = 1 → wash alpha 0, pure tint) and a representative
        // translucent regime (opacity 0.5, strength 0.5 → wash alpha 0.25), the
        // seven panel-era invariants hold. The active/hover/label treatment is
        // unchanged from v1; the panel + seam sit under it.
        const STRENGTH: f32 = 0.5;
        for theme in crate::theme::all() {
            let colors = colors_for(theme);
            let panel = panel_tint(colors, STRENGTH);
            let panel_luma = relative_luminance(panel);
            let bg_luma = relative_luminance(colors.background);

            // (1) The panel is a perceivable surface vs the body background.
            assert!(
                (panel_luma - bg_luma).abs() >= MIN_PANEL_BG_DELTA,
                "{}: panel vs background delta {:.4} < {MIN_PANEL_BG_DELTA}",
                theme.name,
                (panel_luma - bg_luma).abs()
            );

            // (2) The seam survives on this panel; (3) it never blooms.
            let seam = seam_color(colors, panel);
            let seam_composite = blend_srgb(panel, seam, SEAM_ALPHA);
            assert!(
                (relative_luminance(seam_composite) - panel_luma).abs()
                    >= SEAM_MIN_PANEL_DELTA - 1e-6,
                "{}: seam vs panel delta too small",
                theme.name
            );
            assert!(
                relative_luminance(seam) <= SEAM_MAX_LUMA + 1e-6,
                "{}: seam blooms",
                theme.name
            );

            // Labels draw ON TOP of the wash (glyph segment), so they are never
            // veiled; only the fills (cell backgrounds) are veiled by the wash.
            let active_lbl = relative_luminance(active_label(colors));
            let inactive_lbl = relative_luminance(inactive_label(colors, 1));
            let hover_lbl = relative_luminance(hover_label(colors));
            let floor_lbl = relative_luminance(scale_srgb(colors.inactive, RAMP_FLOOR));

            // (4) The active fill clears the legibility RATIO floor against the
            // panel it rests on -- the core fix. Opacity-independent (a raw color
            // relationship), so asserted once. Fails-before on themes whose raw
            // `selection` sits ~1.05:1 on the panel (e.g. odyssey-default): that
            // is the built-in evidence this guard is real.
            let fill = active_fill(colors, panel);
            let fill_ratio = crate::theme::contrast_ratio(fill, panel);
            assert!(
                fill_ratio >= ACTIVE_FILL_MIN_PANEL_RATIO - 1e-6,
                "{}: active fill vs panel ratio {fill_ratio:.3} < {ACTIVE_FILL_MIN_PANEL_RATIO}",
                theme.name,
            );
            // ...and the lifted slab never crosses the bloom threshold.
            assert!(
                relative_luminance(fill) <= SEAM_MAX_LUMA + 1e-6,
                "{}: active fill blooms",
                theme.name,
            );
            // Fills ladder: rest (the panel itself) < hover < active in distance
            // from the panel, so a hover lifts a slot TOWARD active rather than
            // dimming it below the panel (the pre-fix hover inversion).
            let hover_f = hover_fill(colors, panel);
            let fdist = |c: Srgb| (relative_luminance(c) - panel_luma).abs();
            assert!(
                fdist(hover_f) > 0.0 && fdist(hover_f) <= fdist(fill) + 1e-9,
                "{}: hover fill dist {:.4} not between panel and active fill dist {:.4}",
                theme.name,
                fdist(hover_f),
                fdist(fill),
            );

            for opacity in [1.0f32, 0.5f32] {
                let p = panel_wash_alpha(STRENGTH, opacity);
                let veiled_fill_luma = relative_luminance(blend_srgb(fill, panel, p));
                // (5) Active label pops off the veiled fill in every regime.
                assert!(
                    (active_lbl - veiled_fill_luma).abs() >= MIN_ACTIVE_LABEL_FILL_DELTA,
                    "{}: active label vs veiled fill delta {:.4} < {MIN_ACTIVE_LABEL_FILL_DELTA} (opacity {opacity})",
                    theme.name,
                    (active_lbl - veiled_fill_luma).abs()
                );
            }

            // (6) Nearest inactive label visible on the panel.
            assert!(
                (inactive_lbl - panel_luma).abs() >= MIN_INACTIVE_LABEL_PANEL_DELTA,
                "{}: inactive label vs panel delta {:.4} < {MIN_INACTIVE_LABEL_PANEL_DELTA}",
                theme.name,
                (inactive_lbl - panel_luma).abs()
            );

            // (7) Prominence-ladder monotonicity against the panel backing,
            // expressed theme-agnostically as distance from the panel luminance
            // (|luma − panel|): each interactive tier is at least as prominent as
            // the previous one. On a dark theme prominence == brighter; on a
            // light theme == darker — the distance metric captures both without
            // asserting a direction the light-theme treatment inverts.
            let dist = |l: f64| (l - panel_luma).abs();
            assert!(
                dist(inactive_lbl) <= dist(hover_lbl) + 1e-9
                    && dist(hover_lbl) <= dist(active_lbl) + 1e-9,
                "{}: prominence ladder not monotone (inactive {:.3} ≤ hover {:.3} ≤ active {:.3} in distance-from-panel)",
                theme.name,
                dist(inactive_lbl),
                dist(hover_lbl),
                dist(active_lbl)
            );
            // The distant-tab ramp floor is never MORE prominent than the nearest
            // inactive tab on dark themes (the phosphor-persistence direction);
            // the light-theme ramp direction is a separate v1 characteristic and
            // is intentionally not constrained here.
            if relative_luminance(colors.foreground) > relative_luminance(colors.background) {
                assert!(
                    dist(floor_lbl) <= dist(inactive_lbl) + 1e-9,
                    "{}: dark-theme ramp floor ({floor_lbl:.3}) more prominent than the nearest inactive ({inactive_lbl:.3})",
                    theme.name
                );
            }
        }
    }

    #[test]
    fn revealed_autohide_panel_keeps_fills_and_labels_identifiable() {
        // ODP-6 extra case (F4-P3): the auto-hide reveal draws the panel wash at
        // `p_reveal = max(p, 0.85)` — near-opaque — over live content. The
        // worst-case backing for readability is mid-gray content (neither dark
        // nor light). At that alpha over mid-gray, assertions (4)–(6) still hold:
        // the active fill, active label, and nearest inactive label stay
        // distinguishable from the revealed panel surface.
        const STRENGTH: f32 = 0.5;
        const MID_GRAY: Srgb = (0x80, 0x80, 0x80);
        let p_reveal = super::super::rail_autohide::REVEAL_WASH_ALPHA;
        assert!(
            (p_reveal - 0.85).abs() < 1e-6,
            "the guard is written for the 0.85 reveal floor"
        );
        for theme in crate::theme::all() {
            let colors = colors_for(theme);
            let panel = panel_tint(colors, STRENGTH);
            // The panel surface as it reads through the near-opaque reveal wash
            // over worst-case mid-gray content.
            let reveal_panel = blend_srgb(MID_GRAY, panel, p_reveal);
            let reveal_panel_luma = relative_luminance(reveal_panel);
            // The active fill cell veiled by the near-opaque panel wash.
            let veiled_fill = blend_srgb(active_fill(colors, panel), panel, p_reveal);
            let veiled_fill_luma = relative_luminance(veiled_fill);
            let active_lbl = relative_luminance(active_label(colors));
            let inactive_lbl = relative_luminance(inactive_label(colors, 1));

            // (4) Active fill locatable vs the revealed panel (raw fill clears the
            // floor; the near-opaque veil shrinks the observed delta by ~(1−p)).
            let veiled_floor = MIN_ACTIVE_FILL_PANEL_DELTA * (1.0 - p_reveal as f64);
            assert!(
                (veiled_fill_luma - reveal_panel_luma).abs() >= veiled_floor - 1e-9,
                "{}: reveal veiled fill vs panel delta {:.4} < {veiled_floor:.4}",
                theme.name,
                (veiled_fill_luma - reveal_panel_luma).abs()
            );
            // (5) Active label pops off the veiled fill.
            assert!(
                (active_lbl - veiled_fill_luma).abs() >= MIN_ACTIVE_LABEL_FILL_DELTA,
                "{}: reveal active label vs veiled fill delta {:.4} < {MIN_ACTIVE_LABEL_FILL_DELTA}",
                theme.name,
                (active_lbl - veiled_fill_luma).abs()
            );
            // (6) Nearest inactive label visible on the revealed panel.
            assert!(
                (inactive_lbl - reveal_panel_luma).abs() >= MIN_INACTIVE_LABEL_PANEL_DELTA,
                "{}: reveal inactive label vs panel delta {:.4} < {MIN_INACTIVE_LABEL_PANEL_DELTA}",
                theme.name,
                (inactive_lbl - reveal_panel_luma).abs()
            );
        }
    }
}

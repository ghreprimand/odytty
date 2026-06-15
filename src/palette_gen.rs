// SPDX-License-Identifier: GPL-3.0-only
//! Contrast-aware palette generation (U3): one seed color in, a complete,
//! legible-by-construction [`ThemeSpec`] out.
//!
//! You hand [`generate`] a single accent color (a hex value or a sampled
//! wallpaper color), an appearance polarity, and a contrast floor; it hands back
//! a full 24-role theme that is guaranteed readable. The output IS a
//! [`ThemeSpec`], so it drops straight into the existing serialize / live-apply /
//! theme-builder paths — generation adds nothing to the on-disk format.
//!
//! ## How it stays legible
//!
//! Generation is an explicit authoring action: it changes no pixels unless
//! invoked, so it needs no visual gate. Its one non-negotiable is that it
//! *cannot emit an unreadable theme*. Every floored role clears the authoring
//! floor against its mapped surface by construction — the final step is a loop
//! of [`color::enforce_min_contrast`](crate::color::enforce_min_contrast) over
//! those roles, the same primitive the render-time floor (U1) and the
//! interactive snap-to-floor (U2) use, so all three layers agree.
//!
//! ## Determinism
//!
//! [`generate`] is a pure function with no RNG: the same `(seed, appearance,
//! floor)` always yields a byte-identical spec. That makes it golden-testable and
//! reproducible — "the theme I generated from blue" is the same every time.
//!
//! ## Pipeline (`generate`)
//!
//! 1. Seed → OKLCH; extract its hue and chroma. A greyscale seed (chroma ≈ 0)
//!    has no meaningful hue, so it falls back to a neutral cool-blue anchor
//!    rather than reading a garbage `atan2(0, 0)`.
//! 2. Background / foreground come from the **appearance polarity**, not the
//!    seed's own lightness — a bright accent still yields a dark theme in Dark
//!    appearance. They carry a faint seed tint so the field reads as related to
//!    the accent without being colored.
//! 3. The 16 ANSI colors sit on one shared lightness/chroma ramp at the
//!    canonical hue anchors (derived from the reference xterm palette at run
//!    time, not hardcoded), each biased toward the seed hue by a **bounded**
//!    rotation so red still reads as red.
//! 4. The seed lands strongest in the cursor and the selection/search fills.
//! 5. The RV1 validation pass floors every readable role against its surface.

use crate::color::{
    self, LinearRgb, Oklch, enforce_min_contrast, linear_to_oklab, linear_to_srgb_u8,
    oklab_to_linear, oklab_to_oklch, oklch_to_oklab, srgb_to_linear,
};
use crate::text::DEFAULT_ANSI_SRGB;
use crate::theme::{Appearance, Srgb, ThemeSpec, VisualEffect};

/// The maximum a canonical ANSI hue anchor may rotate toward the seed hue, in
/// radians (18°). Past this, "red stops looking red"; capping the bias keeps the
/// generated palette recognizable while still pulling it toward the accent for
/// cohesion (Director Q-2).
const HUE_ROTATION_CAP: f32 = 18.0 * std::f32::consts::PI / 180.0;

/// Chroma below which a seed is treated as achromatic (grey): its `atan2` hue is
/// meaningless, so [`seed_hue`] substitutes the neutral fallback anchor.
const ACHROMATIC_CHROMA: f32 = 0.012;

/// ANSI palette slots that carry a chromatic family, paired with the canonical
/// reference index whose hue anchors that family. Index 0/7/8/15 are achromatic
/// (black / white / bright-black / bright-white) and are handled separately.
///
/// Each entry is `(normal_slot, bright_slot, reference_index)`; the reference
/// index points into [`DEFAULT_ANSI_SRGB`] so the anchor hue is *derived* from
/// the reference palette at generation time rather than pinned to a magic angle.
const CHROMATIC_FAMILIES: [(usize, usize, usize); 6] = [
    (1, 9, 1),  // red
    (2, 10, 2), // green
    (3, 11, 3), // yellow
    (4, 12, 4), // blue
    (5, 13, 5), // magenta
    (6, 14, 6), // cyan
];

/// Per-appearance lightness/chroma targets for the polarity-derived roles. The
/// chromatic ANSI ramp shares one normal and one bright lightness across both
/// appearances (the RV1 pass adapts them to the background); the neutral and
/// semantic roles flip with polarity.
struct Polarity {
    /// Background lightness (the field).
    bg_l: f32,
    /// Window-clear lightness (slightly past the background so the grid reads as
    /// a panel: darker than bg in Dark, lighter in Light).
    clear_l: f32,
    /// Foreground lightness (the body text).
    fg_l: f32,
    /// Cursor lightness — a vivid, seed-hued block.
    cursor_l: f32,
    /// Selection-fill lightness.
    selection_l: f32,
    /// Search-fill lightness.
    search_l: f32,
    /// Border lightness (window chrome).
    border_l: f32,
    /// Inactive/dim lightness (window chrome).
    inactive_l: f32,
    /// Neutral ANSI black (`color0`) lightness, near the background.
    ansi_black_l: f32,
    /// Neutral ANSI bright-black (`color8`) lightness, between bg and fg.
    ansi_bright_black_l: f32,
    /// Neutral ANSI white (`color7`) lightness.
    ansi_white_l: f32,
    /// Neutral ANSI bright-white (`color15`) lightness.
    ansi_bright_white_l: f32,
}

impl Polarity {
    fn for_appearance(appearance: Appearance) -> Polarity {
        match appearance {
            Appearance::Dark => Polarity {
                bg_l: 0.15,
                clear_l: 0.11,
                fg_l: 0.90,
                cursor_l: 0.72,
                selection_l: 0.32,
                search_l: 0.40,
                border_l: 0.26,
                inactive_l: 0.50,
                ansi_black_l: 0.22,
                ansi_bright_black_l: 0.45,
                ansi_white_l: 0.80,
                ansi_bright_white_l: 0.93,
            },
            Appearance::Light => Polarity {
                bg_l: 0.95,
                clear_l: 0.99,
                fg_l: 0.22,
                cursor_l: 0.45,
                selection_l: 0.80,
                search_l: 0.74,
                border_l: 0.78,
                inactive_l: 0.55,
                ansi_black_l: 0.30,
                ansi_bright_black_l: 0.50,
                ansi_white_l: 0.82,
                ansi_bright_white_l: 0.96,
            },
        }
    }
}

/// Shared chroma of the **normal** ANSI chromatic family colors.
const ANSI_NORMAL_C: f32 = 0.125;
/// Shared chroma of the **bright** ANSI chromatic family colors (a touch more
/// saturated than the normal ramp).
const ANSI_BRIGHT_C: f32 = 0.135;
/// Shared lightness of the **normal** ANSI chromatic family colors.
const ANSI_NORMAL_L: f32 = 0.62;
/// Shared lightness of the **bright** ANSI chromatic family colors.
const ANSI_BRIGHT_L: f32 = 0.76;
/// Faint chroma applied to the neutral roles (bg / fg / neutral greys) so the
/// whole theme reads as gently related to the accent without being colored.
const NEUTRAL_TINT_C: f32 = 0.012;
/// Chroma of the selection / search fills — a low-saturation seed tint.
const FILL_C: f32 = 0.045;
/// Minimum / maximum chroma the cursor may take (clamped from the seed chroma)
/// so the cursor is always a visible accent block, never washed out and never
/// out of gamut.
const CURSOR_C_MIN: f32 = 0.10;
const CURSOR_C_MAX: f32 = 0.16;
/// Chroma of the chrome roles (border / inactive) — barely tinted neutrals.
const CHROME_C: f32 = 0.02;

/// Generate a complete, RV1-validated [`ThemeSpec`] from a single seed color.
///
/// `seed` drives the theme's hue identity; `appearance` sets the light/dark
/// polarity (the seed's own lightness is deliberately *not* trusted for the
/// background, so a bright accent still yields a dark theme in Dark appearance);
/// `floor` is the authoring contrast floor every readable role is guaranteed to
/// clear (typically WCAG 4.5).
///
/// Pure and deterministic: identical inputs yield a byte-identical spec.
pub fn generate(seed: Srgb, appearance: Appearance, floor: f32) -> ThemeSpec {
    let pol = Polarity::for_appearance(appearance);
    let hue = seed_hue(seed);
    let seed_c = srgb_to_oklch(seed).c;

    // --- Polarity-derived neutrals (background / foreground / clear) --------
    let background = oklch_to_srgb(Oklch {
        l: pol.bg_l,
        c: NEUTRAL_TINT_C,
        h: hue,
    });
    let foreground = oklch_to_srgb(Oklch {
        l: pol.fg_l,
        c: NEUTRAL_TINT_C,
        h: hue,
    });
    let clear = oklch_to_srgb(Oklch {
        l: pol.clear_l,
        c: NEUTRAL_TINT_C,
        h: hue,
    });

    // --- The 16 ANSI colors on one shared ramp at the canonical anchors -----
    let mut palette: [Srgb; 16] = [(0, 0, 0); 16];
    palette[0] = oklch_to_srgb(Oklch {
        l: pol.ansi_black_l,
        c: NEUTRAL_TINT_C,
        h: hue,
    });
    palette[7] = oklch_to_srgb(Oklch {
        l: pol.ansi_white_l,
        c: NEUTRAL_TINT_C,
        h: hue,
    });
    palette[8] = oklch_to_srgb(Oklch {
        l: pol.ansi_bright_black_l,
        c: NEUTRAL_TINT_C,
        h: hue,
    });
    palette[15] = oklch_to_srgb(Oklch {
        l: pol.ansi_bright_white_l,
        c: NEUTRAL_TINT_C,
        h: hue,
    });
    for (normal_slot, bright_slot, reference_index) in CHROMATIC_FAMILIES {
        let anchor = anchor_hue(reference_index);
        let biased = bias_hue(anchor, hue);
        palette[normal_slot] = oklch_to_srgb(Oklch {
            l: ANSI_NORMAL_L,
            c: ANSI_NORMAL_C,
            h: biased,
        });
        palette[bright_slot] = oklch_to_srgb(Oklch {
            l: ANSI_BRIGHT_L,
            c: ANSI_BRIGHT_C,
            h: biased,
        });
    }

    // --- Seat the accent: cursor + selection / search fills -----------------
    let cursor = oklch_to_srgb(Oklch {
        l: pol.cursor_l,
        c: seed_c.clamp(CURSOR_C_MIN, CURSOR_C_MAX),
        h: hue,
    });
    let selection = oklch_to_srgb(Oklch {
        l: pol.selection_l,
        c: FILL_C,
        h: hue,
    });
    let search = oklch_to_srgb(Oklch {
        l: pol.search_l,
        c: FILL_C,
        h: hue,
    });

    // --- Window chrome (unfloored) ------------------------------------------
    let border = oklch_to_srgb(Oklch {
        l: pol.border_l,
        c: CHROME_C,
        h: hue,
    });
    let inactive = oklch_to_srgb(Oklch {
        l: pol.inactive_l,
        c: CHROME_C,
        h: hue,
    });

    let mut spec = ThemeSpec {
        name: "custom".to_string(),
        appearance,
        foreground,
        background,
        clear,
        palette,
        cursor,
        selection,
        search,
        border,
        inactive,
        font_family: None,
        font_size: None,
        visual: VisualEffect::Off,
    };

    validate(&mut spec, floor);
    spec
}

/// The RV1 validation pass — the legibility guarantee.
///
/// Floors every readable role against its mapped surface, reusing the U2 Q-4
/// surface mapping verbatim so the author-time snap and this generator agree on
/// what is floored against what:
///
/// * `foreground`, the 14 chromatic ANSI slots, `cursor` → against the
///   **background**.
/// * `selection` / `search` fills → against the **foreground** text that is drawn
///   over them (so selected/highlighted text stays legible). `wcag_contrast` is
///   symmetric, so flooring the fill against the foreground guarantees the
///   foreground-over-fill pair clears the floor.
/// * `border`, `inactive`, `clear` → window chrome, left untouched.
/// * **The achromatic neutral pair on the background's own side is exempt** from
///   the vs-background floor (see [`bg_side_neutral_slots`]). These are
///   structural ramp neutrals, not primary text: in a dark theme `color0`
///   (darkest) and `color8` (a step up) stay near the background so the ramp
///   keeps its low end and the two don't collapse to the same legible grey; in a
///   light theme the near-bg light neutrals `color7`/`color15` are exempt by
///   symmetry. The render-time legibility floor (U1) still lifts any app text
///   that actually lands on the background at draw time, so pre-flooring these
///   here would be both redundant and aesthetically harmful. The far neutral
///   pair (nearest the foreground) clears the floor naturally and stays floored.
///
/// [`enforce_min_contrast`] only nudges OKLab lightness (preserving hue and
/// chroma) and is idempotent, so this preserves every hue identity built above
/// while making the output RV1-valid by construction for all primary text.
fn validate(spec: &mut ThemeSpec, floor: f32) {
    let bg = to_linear(spec.background);
    spec.foreground = floor_role(spec.foreground, bg, floor);
    let exempt = bg_side_neutral_slots(spec.appearance);
    for (index, slot) in spec.palette.iter_mut().enumerate() {
        if exempt.contains(&index) {
            continue;
        }
        *slot = floor_role(*slot, bg, floor);
    }
    spec.cursor = floor_role(spec.cursor, bg, floor);

    // Selection / search carry the foreground text; floor the fills against it.
    let fg = to_linear(spec.foreground);
    spec.selection = floor_role(spec.selection, fg, floor);
    spec.search = floor_role(spec.search, fg, floor);
}

/// The two achromatic ANSI neutrals sitting on the same side as the background,
/// exempt from the vs-background floor so they stay as near-bg structural ramp
/// neutrals. Dark themes keep `color0`/`color8` (the dark neutrals near a dark
/// bg); light themes keep `color7`/`color15` (the light neutrals near a light
/// bg). The opposite pair sits near the foreground and stays floored.
fn bg_side_neutral_slots(appearance: Appearance) -> [usize; 2] {
    match appearance {
        Appearance::Dark => [0, 8],
        Appearance::Light => [7, 15],
    }
}

/// Floor one role (as `Srgb`) against a linear surface, returning the adjusted
/// `Srgb` whose **quantized bytes** clear `floor`.
///
/// [`enforce_min_contrast`] guarantees the floor in *linear* space, but two
/// downstream steps can nibble contrast back below it: rounding the result to
/// 8-bit bytes, and gamut-mapping a saturated lift back into the sRGB cube. So
/// after enforcing, we gamut-map (hue-preserving) and quantize, then *re-check*
/// the byte result; if it fell short, we bump the internal target and try again.
/// This makes the guarantee hold on the bytes that are actually emitted.
///
/// `floor <= 1.0` is the passthrough no-op (the validation pass leaves the role
/// untouched, bit-for-bit), matching [`enforce_min_contrast`]'s contract.
fn floor_role(role: Srgb, surface: LinearRgb, floor: f32) -> Srgb {
    if floor <= 1.0 {
        return role;
    }
    let role_lin = to_linear(role);
    let mut target = floor;
    let mut best = role;
    for _ in 0..8 {
        let adjusted = enforce_min_contrast(role_lin, surface, target);
        let mapped = oklch_to_linear_gamut(oklab_to_oklch(linear_to_oklab(adjusted)));
        best = from_linear(mapped);
        if color::wcag_contrast(to_linear(best), surface) >= floor {
            return best;
        }
        // Quantization / gamut-map shaved it just under; lift the target a hair.
        target += 0.1;
    }
    best
}

/// Extract the seed's hue (radians), substituting the neutral fallback anchor
/// for an achromatic (grey) seed whose hue is undefined.
fn seed_hue(seed: Srgb) -> f32 {
    let lch = srgb_to_oklch(seed);
    if lch.c < ACHROMATIC_CHROMA {
        // Grey seed: no meaningful hue. Fall back to the cool-blue family anchor
        // (the Odyssey default direction) rather than a garbage `atan2(0, 0)`.
        anchor_hue(4)
    } else {
        lch.h
    }
}

/// The canonical hue (radians) of a reference ANSI family, derived by round-trip
/// of [`DEFAULT_ANSI_SRGB`] through OKLCH. Keeps the anchors honest and
/// self-documenting — the reference palette is the single source of the hues.
fn anchor_hue(reference_index: usize) -> f32 {
    srgb_to_oklch(DEFAULT_ANSI_SRGB[reference_index]).h
}

/// Rotate `anchor` toward `target` by at most [`HUE_ROTATION_CAP`], taking the
/// shortest path around the hue circle. Caps the bias so a family stays
/// recognizable (red stays red) while leaning toward the accent.
fn bias_hue(anchor: f32, target: f32) -> f32 {
    let delta = wrap_angle(target - anchor);
    anchor + delta.clamp(-HUE_ROTATION_CAP, HUE_ROTATION_CAP)
}

/// Wrap an angle (radians) into `(-π, π]` so hue deltas take the short way
/// around the circle.
fn wrap_angle(mut a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    while a > PI {
        a -= TAU;
    }
    while a <= -PI {
        a += TAU;
    }
    a
}

// ---------------------------------------------------------------------------
// Srgb ↔ linear ↔ OKLCH bridges (thin wrappers over `color`)
// ---------------------------------------------------------------------------

/// An sRGB byte triple to linear RGB.
fn to_linear((r, g, b): Srgb) -> LinearRgb {
    [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)]
}

/// A linear RGB triple to the nearest in-gamut sRGB byte triple.
fn from_linear(lin: LinearRgb) -> Srgb {
    (
        linear_to_srgb_u8(lin[0]),
        linear_to_srgb_u8(lin[1]),
        linear_to_srgb_u8(lin[2]),
    )
}

/// An sRGB byte triple to OKLCH.
fn srgb_to_oklch(srgb: Srgb) -> Oklch {
    oklab_to_oklch(linear_to_oklab(to_linear(srgb)))
}

/// An OKLCH color to the nearest in-gamut sRGB byte triple, gamut-mapped by
/// **chroma reduction** (preserving lightness and hue) rather than per-channel
/// clamping (which skews hue). Keeps generated colors true to their intended
/// hue even when the requested chroma exceeds the sRGB gamut at that lightness.
fn oklch_to_srgb(lch: Oklch) -> Srgb {
    from_linear(oklch_to_linear_gamut(lch))
}

/// True if a linear color lies within the sRGB cube (small epsilon for float
/// slop at the boundary).
fn in_gamut(lin: LinearRgb) -> bool {
    lin.iter().all(|&c| (-1e-4..=1.0 + 1e-4).contains(&c))
}

/// Convert an OKLCH color to linear RGB, reducing chroma toward the neutral axis
/// (constant lightness and hue) until the result fits the sRGB gamut. A straight
/// OKLab→linear of a saturated color can fall outside the cube; clamping it per
/// channel would shift the hue, so instead we bisect chroma down to the gamut
/// boundary, which keeps the hue exact and only desaturates as much as needed.
fn oklch_to_linear_gamut(lch: Oklch) -> LinearRgb {
    let direct = oklab_to_linear(oklch_to_oklab(lch));
    if in_gamut(direct) {
        return direct;
    }
    let (mut lo, mut hi) = (0.0_f32, lch.c);
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        let candidate = oklab_to_linear(oklch_to_oklab(Oklch { c: mid, ..lch }));
        if in_gamut(candidate) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    oklab_to_linear(oklch_to_oklab(Oklch { c: lo, ..lch }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::wcag_contrast;

    const AUTHORING_FLOOR: f32 = 4.5;

    /// A coarse sweep of seed hues at full saturation, both appearances, used by
    /// the by-construction guarantees.
    fn seed_sweep() -> Vec<Srgb> {
        let mut seeds = Vec::new();
        // Saturated hues stepped around the wheel.
        for step in 0..12 {
            let h = step as f32 / 12.0 * std::f32::consts::TAU - std::f32::consts::PI;
            seeds.push(oklch_to_srgb(Oklch {
                l: 0.65,
                c: 0.15,
                h,
            }));
        }
        // A couple of real accent bytes plus achromatic extremes.
        seeds.push((0x86, 0xc1, 0xff)); // cool blue
        seeds.push((0xff, 0x6a, 0x3d)); // warm orange
        seeds.push((0x10, 0x10, 0x10)); // near-black grey
        seeds.push((0x80, 0x80, 0x80)); // mid grey
        seeds.push((0xf0, 0xf0, 0xf0)); // near-white grey
        seeds
    }

    #[test]
    fn generate_is_deterministic() {
        let seed = (0x86, 0xc1, 0xff);
        let a = generate(seed, Appearance::Dark, AUTHORING_FLOOR);
        let b = generate(seed, Appearance::Dark, AUTHORING_FLOOR);
        assert_eq!(a, b, "generate must be byte-identical for identical inputs");
    }

    #[test]
    fn every_floored_role_clears_the_floor_by_construction() {
        for &seed in &seed_sweep() {
            for appearance in [Appearance::Dark, Appearance::Light] {
                let spec = generate(seed, appearance, AUTHORING_FLOOR);
                let bg = to_linear(spec.background);
                // Foreground, full palette, and cursor vs background.
                assert!(
                    wcag_contrast(to_linear(spec.foreground), bg) >= AUTHORING_FLOOR - 1e-3,
                    "fg floor failed seed={seed:?} {appearance:?}"
                );
                // The achromatic neutral pair on the bg's own side is exempt
                // (structural ramp neutrals); every other slot clears the floor.
                let exempt = bg_side_neutral_slots(appearance);
                for (i, &color) in spec.palette.iter().enumerate() {
                    if exempt.contains(&i) {
                        continue;
                    }
                    let c = wcag_contrast(to_linear(color), bg);
                    assert!(
                        c >= AUTHORING_FLOOR - 1e-3,
                        "palette[{i}] floor failed: {c} seed={seed:?} {appearance:?}"
                    );
                }
                assert!(
                    wcag_contrast(to_linear(spec.cursor), bg) >= AUTHORING_FLOOR - 1e-3,
                    "cursor floor failed seed={seed:?} {appearance:?}"
                );
                // Selection / search fills vs the foreground text drawn over them.
                let fg = to_linear(spec.foreground);
                assert!(
                    wcag_contrast(to_linear(spec.selection), fg) >= AUTHORING_FLOOR - 1e-3,
                    "selection floor failed seed={seed:?} {appearance:?}"
                );
                assert!(
                    wcag_contrast(to_linear(spec.search), fg) >= AUTHORING_FLOOR - 1e-3,
                    "search floor failed seed={seed:?} {appearance:?}"
                );
            }
        }
    }

    #[test]
    fn bright_accent_in_dark_appearance_still_yields_a_dark_theme() {
        // A blindingly bright seed must not drag the background light: polarity,
        // not the seed's lightness, decides bg/fg.
        let bright = (0xff, 0xf2, 0x40); // near-white yellow
        let spec = generate(bright, Appearance::Dark, AUTHORING_FLOOR);
        assert!(
            color::relative_luminance(to_linear(spec.background)) < 0.10,
            "dark theme background should stay dark, got {:?}",
            spec.background
        );
        assert!(
            color::relative_luminance(to_linear(spec.foreground))
                > color::relative_luminance(to_linear(spec.background)),
            "dark theme foreground should be lighter than its background"
        );
    }

    #[test]
    fn appearance_polarity_flips_background_and_foreground() {
        let seed = (0x4f, 0x9c, 0xff);
        let dark = generate(seed, Appearance::Dark, AUTHORING_FLOOR);
        let light = generate(seed, Appearance::Light, AUTHORING_FLOOR);
        let dark_bg = color::relative_luminance(to_linear(dark.background));
        let light_bg = color::relative_luminance(to_linear(light.background));
        assert!(dark_bg < 0.10, "dark bg luminance {dark_bg}");
        assert!(light_bg > 0.80, "light bg luminance {light_bg}");
        // Foreground polarity is the inverse of the background in each.
        assert!(
            color::relative_luminance(to_linear(dark.foreground)) > dark_bg,
            "dark fg lighter than dark bg"
        );
        assert!(
            color::relative_luminance(to_linear(light.foreground)) < light_bg,
            "light fg darker than light bg"
        );
    }

    #[test]
    fn bg_side_neutrals_keep_their_near_bg_lightness() {
        // The exempt neutral pair stays close to the background lightness rather
        // than being lifted to the legible floor. Dark exempts color0/color8
        // (near a dark bg); Light exempts color7/color15 (near a light bg) — the
        // symmetric mirror.
        let seed = (0x86, 0xc1, 0xff);

        let dark = generate(seed, Appearance::Dark, AUTHORING_FLOOR);
        let dark_bg_l = srgb_to_oklch(dark.background).l;
        for slot in bg_side_neutral_slots(Appearance::Dark) {
            let l = srgb_to_oklch(dark.palette[slot]).l;
            assert!(
                (l - dark_bg_l).abs() < 0.35 && l < 0.55,
                "dark color{slot} should hug the dark bg, L={l} (bg L={dark_bg_l})"
            );
        }

        let light = generate(seed, Appearance::Light, AUTHORING_FLOOR);
        let light_bg_l = srgb_to_oklch(light.background).l;
        for slot in bg_side_neutral_slots(Appearance::Light) {
            let l = srgb_to_oklch(light.palette[slot]).l;
            assert!(
                (l - light_bg_l).abs() < 0.35 && l > 0.55,
                "light color{slot} should hug the light bg, L={l} (bg L={light_bg_l})"
            );
        }

        // And the far (fg-side) neutrals are NOT exempt — they clear the floor.
        let dark_bg = to_linear(dark.background);
        for slot in [7usize, 15] {
            let c = wcag_contrast(to_linear(dark.palette[slot]), dark_bg);
            assert!(
                c >= AUTHORING_FLOOR - 1e-3,
                "dark color{slot} should be floored: {c}"
            );
        }
        let light_bg = to_linear(light.background);
        for slot in [0usize, 8] {
            let c = wcag_contrast(to_linear(light.palette[slot]), light_bg);
            assert!(
                c >= AUTHORING_FLOOR - 1e-3,
                "light color{slot} should be floored: {c}"
            );
        }
    }

    #[test]
    fn chromatic_families_stay_within_the_rotation_cap_of_their_anchor() {
        // Red stays red: each generated chromatic family's hue stays within the
        // bias cap of its canonical anchor across the seed sweep. A tiny epsilon
        // absorbs byte-quantization / gamut-clamp drift.
        let tol = HUE_ROTATION_CAP + 0.06;
        for &seed in &seed_sweep() {
            for appearance in [Appearance::Dark, Appearance::Light] {
                let spec = generate(seed, appearance, AUTHORING_FLOOR);
                for (normal_slot, bright_slot, reference_index) in CHROMATIC_FAMILIES {
                    let anchor = anchor_hue(reference_index);
                    for slot in [normal_slot, bright_slot] {
                        let lch = srgb_to_oklch(spec.palette[slot]);
                        // Skip degenerate near-grey results (hue undefined); the
                        // floor pass can desaturate a clamped color.
                        if lch.c < 0.02 {
                            continue;
                        }
                        let drift = wrap_angle(lch.h - anchor).abs();
                        assert!(
                            drift <= tol,
                            "slot {slot} drifted {drift} rad from anchor (seed={seed:?} {appearance:?})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn achromatic_seed_produces_a_valid_legible_palette_without_panicking() {
        // A pure grey seed has no hue; generation must not NaN or panic and must
        // still satisfy the floor.
        for grey in [(0x00, 0x00, 0x00), (0x7f, 0x7f, 0x7f), (0xff, 0xff, 0xff)] {
            let spec = generate(grey, Appearance::Dark, AUTHORING_FLOOR);
            let bg = to_linear(spec.background);
            let exempt = bg_side_neutral_slots(spec.appearance);
            for (i, &color) in spec.palette.iter().enumerate() {
                let c = wcag_contrast(to_linear(color), bg);
                assert!(c.is_finite(), "grey {grey:?} produced non-finite contrast");
                if exempt.contains(&i) {
                    continue;
                }
                assert!(c >= AUTHORING_FLOOR - 1e-3, "grey {grey:?} slot {i}: {c}");
            }
        }
    }

    #[test]
    fn seed_lands_in_the_cursor() {
        // The cursor should carry the seed hue (within the achromatic guard),
        // making "the theme I generated from blue" visibly blue where it counts.
        let seed = (0x3d, 0x7d, 0xff); // distinctly blue
        let seed_h = srgb_to_oklch(seed).h;
        let spec = generate(seed, Appearance::Dark, AUTHORING_FLOOR);
        let cursor_h = srgb_to_oklch(spec.cursor).h;
        let drift = wrap_angle(cursor_h - seed_h).abs();
        assert!(drift < 0.12, "cursor hue drifted {drift} rad from the seed");
    }

    #[test]
    fn floor_of_one_is_a_no_op_validation_pass() {
        // With the floor at unity (passthrough), the validation pass must not
        // move any role — the pre-validation colors survive intact.
        //
        // A Light appearance is the clean demonstrator: the chromatic ramp sits
        // at L≈0.62, which is too light against the L≈0.95 light background, so
        // the 4.5 floor darkens those slots. With floor=1.0 they must stay put.
        let seed = (0x86, 0xc1, 0xff);
        let floored = generate(seed, Appearance::Light, 4.5);
        let unfloored = generate(seed, Appearance::Light, 1.0);

        // color1 (red) is a non-exempt chromatic slot: unfloored it keeps its
        // built lightness; floored it is pulled darker to clear the floor.
        let unfloored_l = srgb_to_oklch(unfloored.palette[1]).l;
        let floored_l = srgb_to_oklch(floored.palette[1]).l;
        assert!(
            unfloored_l > floored_l + 0.05,
            "floor=1.0 should leave color1 lighter than the floored variant: \
             {unfloored_l} vs {floored_l}"
        );

        // floor=1.0 is an exact passthrough: every role equals the pre-floor
        // build. Re-running the same generation is byte-identical (determinism),
        // and the floored variant genuinely differs — so the pass did real work.
        assert_eq!(
            unfloored,
            generate(seed, Appearance::Light, 1.0),
            "floor=1.0 must be deterministic / a stable passthrough"
        );
        assert_ne!(
            unfloored, floored,
            "the 4.5 floor must move at least one role vs the unfloored build"
        );
    }

    #[test]
    fn generated_spec_round_trips_through_the_theme_format() {
        // The generated spec serializes and re-parses identically — it rides the
        // existing on-disk path with no additions.
        let spec = generate((0x86, 0xc1, 0xff), Appearance::Dark, AUTHORING_FLOOR);
        let text = spec.serialize();
        let reparsed = ThemeSpec::parse(&text, |m| panic!("warn: {m}"));
        assert_eq!(reparsed, spec);
        // No real home dir leaks into the serialized fixture text.
        assert!(!text.contains("/home/"));
    }

    #[test]
    fn golden_spec_for_a_representative_seed() {
        // Pin the exact byte output for a representative cool-blue seed in Dark
        // appearance at the 4.5 authoring floor. This locks the whole pipeline:
        // a change to any constant, anchor, the bias cap, or the floor pass that
        // alters the result trips this test on purpose. (In Dark, color0/color8
        // are the exempt near-bg structural neutrals — distinct, not floored.)
        let spec = generate((0x86, 0xc1, 0xff), Appearance::Dark, AUTHORING_FLOOR);
        assert_eq!(spec.name, "custom");
        assert_eq!(spec.appearance, Appearance::Dark);
        assert_eq!(spec.foreground, (216, 223, 230));
        assert_eq!(spec.background, (8, 12, 16));
        assert_eq!(spec.clear, (3, 5, 8));
        assert_eq!(spec.cursor, (111, 169, 230));
        assert_eq!(spec.selection, (33, 52, 73));
        assert_eq!(spec.search, (53, 74, 95));
        assert_eq!(spec.border, (29, 37, 46));
        assert_eq!(spec.inactive, (91, 100, 111));
        assert_eq!(
            spec.palette,
            [
                (22, 27, 32),    // 0  black  (exempt near-bg neutral)
                (197, 99, 115),  // 1  red
                (42, 157, 108),  // 2  green
                (112, 148, 58),  // 3  yellow
                (70, 138, 206),  // 4  blue
                (158, 111, 190), // 5  magenta
                (0, 151, 173),   // 6  cyan
                (184, 190, 197), // 7  white
                (80, 86, 92),    // 8  bright black (exempt near-bg neutral)
                (250, 139, 155), // 9  bright red
                (84, 203, 148),  // 10 bright green
                (153, 193, 95),  // 11 bright yellow
                (111, 182, 255), // 12 bright blue
                (204, 152, 241), // 13 bright magenta
                (0, 198, 227),   // 14 bright cyan
                (226, 233, 240), // 15 bright white
            ]
        );
        assert_eq!(spec.font_family, None);
        assert_eq!(spec.font_size, None);
        assert_eq!(spec.visual, VisualEffect::Off);
    }
}

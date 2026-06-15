// SPDX-License-Identifier: GPL-3.0-only
//! Perceptual colour-vision-deficiency (CVD) palette adaptation (U4-core): take
//! a theme's colours in, hand back a CVD-adapted, still-readable theme out.
//!
//! A viewer with a colour-vision deficiency loses one of the two perceptual
//! opponent axes: protan/deutan viewers cannot resolve the red–green (`a`) axis,
//! tritan viewers cannot resolve the blue–yellow (`b`) axis. Two theme colours
//! that differ *only* on the lost axis collapse onto the same perceived colour —
//! red and green become indistinguishable, or blue and yellow do. This module
//! *daltonises* a palette: it moves the information off the lost axis onto the
//! retained axis (plus a small lightness nudge) so the colours separate again
//! for that viewer, then re-floors the result so it stays legible.
//!
//! ## Why OKLCH
//!
//! OKLab's `a`/`b` opponent coordinates line up directly with the two CVD
//! confusion axes, so the deficient axis and the retained axis the correction
//! redistributes onto are explicit coordinates — no LMS round-trip, no new
//! dependency. The whole transform lives in the existing OKLCH machinery.
//!
//! ## Two opposite operations
//!
//! * [`cvd_adapt`] — *daltonise*: remap a colour so a CVD viewer can tell it
//!   apart from its confusion partner. The accessibility win; shown to the user.
//! * [`cvd_simulate`] — *simulate*: collapse the lost axis to approximate what a
//!   CVD viewer perceives. Never shown to a CVD user; it exists so a non-CVD
//!   author (and these tests) can verify that semantic pairs survive.
//!
//! ## Readable AND distinguishable
//!
//! Daltonising makes colours *distinguishable*; the RV1 contrast floor keeps
//! them *readable*. They compose because the floor moves **only OKLab L** while
//! the correction does its primary separating work on the retained **opponent
//! axis** (the `a`/`b` plane) — orthogonal coordinates, so the floor's L-lift
//! cannot undo the separation. [`adapt_palette`] runs the correction across the
//! chromatic roles, holds background/foreground structural, then re-floors
//! against the same per-role surface mapping the theme generator/builder use, so
//! the output is RV1-valid **by construction**.
//!
//! ## Determinism
//!
//! Every entry point is a pure function with no globals and no RNG: identical
//! inputs yield byte-identical output. The "off" state is not represented here —
//! that lives at the future render-wiring layer; the core is only ever called
//! with a real deficiency type. `strength = 0` is an exact bit-for-bit
//! passthrough.

use crate::color::{
    self, LinearRgb, Oklab, Oklch, enforce_min_contrast, linear_to_oklab, linear_to_srgb_u8,
    oklab_to_linear, oklab_to_oklch, oklch_to_oklab, srgb_to_linear,
};
use crate::theme::{Appearance, Srgb, ThemeSpec};

/// A colour-vision-deficiency type. Each names the impaired cone class and, with
/// it, the OKLab opponent axis the viewer cannot resolve:
///
/// * [`Protan`](CvdType::Protan) / [`Deutan`](CvdType::Deutan) — red/green cones;
///   the red–green **`a`** axis is lost (red↔green confusion).
/// * [`Tritan`](CvdType::Tritan) — blue cones; the blue–yellow **`b`** axis is
///   lost (blue↔yellow confusion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CvdType {
    /// Long-wave (red) cones impaired; the red–green `a` axis is lost.
    Protan,
    /// Medium-wave (green) cones impaired (the most common deficiency); the
    /// red–green `a` axis is lost.
    Deutan,
    /// Short-wave (blue) cones impaired (rare); the blue–yellow `b` axis is lost.
    Tritan,
}

/// The OKLab opponent axis a [`CvdType`] cannot resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LostAxis {
    /// The red–green `a` axis (protan / deutan).
    A,
    /// The blue–yellow `b` axis (tritan).
    B,
}

impl CvdType {
    /// The opponent axis this deficiency collapses.
    fn lost_axis(self) -> LostAxis {
        match self {
            CvdType::Protan | CvdType::Deutan => LostAxis::A,
            CvdType::Tritan => LostAxis::B,
        }
    }
}

/// How much of the deficient-axis component is removed from the adapted colour
/// at full strength. The viewer cannot perceive that axis anyway, so removing it
/// frees gamut budget for the retained axis. `1.0` = fully neutralise it.
const ATTEN: f32 = 1.0;
/// The retained-axis gain — the **primary** separating cue. The lost-axis
/// component is re-expressed as a proportional push along the retained opponent
/// axis, where the CVD viewer *can* see it.
const RETAINED_GAIN: f32 = 0.8;
/// The lightness gain — the **secondary** cue. A small L offset proportional to
/// the lost component helps colours that are confusable *and* share a luminance
/// (where the retained-axis push alone, after gamut clamping, may not separate
/// them). Kept small so it costs the floor little and rarely fights an existing
/// luminance difference.
const L_GAIN: f32 = 0.15;

/// The authoring contrast floor [`adapt_palette`] re-floors against — the same
/// WCAG-AA target the theme generator and builder use, so all the colour
/// modules agree on what "readable" means. The accepted `adapt_palette`
/// signature carries no floor parameter, so the standard floor is applied
/// internally.
const AUTHORING_FLOOR: f32 = 4.5;

/// Daltonise one colour for a CVD viewer: move the information the viewer loses
/// off the deficient opponent axis onto the retained one (plus a small lightness
/// nudge) so the colour separates from its confusion partner.
///
/// `strength` is clamped to `0..=1`. `strength <= 0` is an exact bit-for-bit
/// passthrough (the identity the wiring layer's "off" state relies on); `1` is
/// the full correction. The transform is **self-limiting**: a colour whose
/// lost-axis component is already small (one a CVD viewer can already resolve)
/// barely moves, so adapting a whole palette never heavy-handedly recolours an
/// already-accessible scene.
///
/// Pure and deterministic. Apply it **exactly once** per colour: it is not
/// idempotent (re-feeding its own output would over-shift), which the
/// palette-in → palette-out shape of [`adapt_palette`] enforces.
pub fn cvd_adapt(color: Srgb, ty: CvdType, strength: f32) -> Srgb {
    if strength <= 0.0 {
        return color;
    }
    let k = strength.min(1.0);
    let lab = linear_to_oklab(to_linear(color));
    let adapted = match ty.lost_axis() {
        LostAxis::A => {
            // Red–green is lost: attenuate `a`, redistribute it onto `b` (the
            // retained blue–yellow axis) plus a small L offset.
            let lost = lab.a;
            Oklab {
                l: lab.l + L_GAIN * lost * k,
                a: lab.a * (1.0 - ATTEN * k),
                b: lab.b + RETAINED_GAIN * lost * k,
            }
        }
        LostAxis::B => {
            // Blue–yellow is lost: attenuate `b`, redistribute it onto `a` (the
            // retained red–green axis) plus a small L offset.
            let lost = lab.b;
            Oklab {
                l: lab.l + L_GAIN * lost * k,
                a: lab.a + RETAINED_GAIN * lost * k,
                b: lab.b * (1.0 - ATTEN * k),
            }
        }
    };
    from_linear(oklch_to_linear_gamut(oklab_to_oklch(adapted)))
}

/// Approximate what a CVD viewer perceives by collapsing the deficient opponent
/// axis to zero (projecting the colour onto the retained axis + lightness).
///
/// This is the *opposite* of [`cvd_adapt`]: it does not help anyone see better,
/// it models the loss. Its purpose is verification — a non-CVD author (or a
/// test) runs it over a palette to check whether two semantic colours collapse
/// onto the same perceived point. Never present its output to a CVD user.
///
/// Pure and deterministic. A full (dichromatic) collapse is used as the test and
/// preview basis; it is the clean worst case for "do these still separate?".
pub fn cvd_simulate(color: Srgb, ty: CvdType) -> Srgb {
    let lab = linear_to_oklab(to_linear(color));
    let collapsed = match ty.lost_axis() {
        LostAxis::A => Oklab { a: 0.0, ..lab },
        LostAxis::B => Oklab { b: 0.0, ..lab },
    };
    from_linear(oklch_to_linear_gamut(oklab_to_oklch(collapsed)))
}

/// Daltonise a whole theme for a CVD viewer and re-floor it so it stays
/// readable. The deterministic core of U4.
///
/// Remaps the colours that carry semantic meaning — the 16 ANSI colours and the
/// chromatic roles (cursor, selection, search) — and **holds the background and
/// foreground structural** (the canvas and the floor's stable reference; pure
/// near-neutrals barely move under the remap anyway, but holding them is cleaner
/// and testable). Window chrome (border / inactive / clear) is likewise left
/// untouched. After the remap it re-floors against [`AUTHORING_FLOOR`] using the
/// same per-role surface mapping the theme generator/builder use, so the output
/// is RV1-valid by construction.
///
/// Single-pass (each colour is adapted exactly once) and pure: identical
/// `(spec, ty, strength)` yields a byte-identical spec.
pub fn adapt_palette(spec: &ThemeSpec, ty: CvdType, strength: f32) -> ThemeSpec {
    let mut out = spec.clone();
    for slot in out.palette.iter_mut() {
        *slot = cvd_adapt(*slot, ty, strength);
    }
    out.cursor = cvd_adapt(out.cursor, ty, strength);
    out.selection = cvd_adapt(out.selection, ty, strength);
    out.search = cvd_adapt(out.search, ty, strength);
    // background / foreground / chrome held structural.
    validate(&mut out, AUTHORING_FLOOR);
    out
}

/// Re-floor the readable roles against their mapped surfaces — the legibility
/// guarantee, mirroring the theme generator's pass verbatim so every colour
/// module floors the same role against the same surface:
///
/// * `foreground`, the 14 chromatic ANSI slots, `cursor` → against the
///   **background**.
/// * `selection` / `search` fills → against the **foreground** drawn over them.
/// * `border`, `inactive`, `clear` → window chrome, left untouched.
/// * The achromatic neutral pair on the background's own side is **exempt** from
///   the vs-background floor (see [`bg_side_neutral_slots`]) — structural ramp
///   neutrals that stay near the background rather than being lifted to the
///   legible floor.
///
/// [`enforce_min_contrast`] moves **only** OKLab lightness, so this preserves
/// the `a`/`b` separation the remap created while making the output readable.
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

    let fg = to_linear(spec.foreground);
    spec.selection = floor_role(spec.selection, fg, floor);
    spec.search = floor_role(spec.search, fg, floor);
}

/// The two achromatic ANSI neutrals on the same side as the background, exempt
/// from the vs-background floor so they stay near-bg structural ramp neutrals.
/// Dark themes keep `color0`/`color8`; light themes keep `color7`/`color15`.
/// Mirrors the theme generator's exemption verbatim.
fn bg_side_neutral_slots(appearance: Appearance) -> [usize; 2] {
    match appearance {
        Appearance::Dark => [0, 8],
        Appearance::Light => [7, 15],
    }
}

/// Floor one role against a linear surface, returning the `Srgb` whose
/// **quantized bytes** clear `floor`. Enforces in linear space, then gamut-maps
/// (hue-preserving) and quantizes, then re-checks the byte result and bumps the
/// internal target if rounding/gamut shaved it under. `floor <= 1.0` is the
/// passthrough no-op. Mirrors the theme generator's `floor_role` verbatim.
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
        target += 0.1;
    }
    best
}

// ---------------------------------------------------------------------------
// Srgb ↔ linear ↔ OKLCH bridges (thin wrappers over `color`; mirror the theme
// generator so the gamut handling is identical across the colour modules).
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

/// True if a linear colour lies within the sRGB cube (small epsilon for float
/// slop at the boundary).
fn in_gamut(lin: LinearRgb) -> bool {
    lin.iter().all(|&c| (-1e-4..=1.0 + 1e-4).contains(&c))
}

/// Convert an OKLCH colour to linear RGB, reducing chroma toward the neutral
/// axis (constant lightness and hue) until the result fits the sRGB gamut. A
/// saturated OKLab→linear can fall outside the cube; per-channel clamping would
/// skew the hue, so this bisects chroma down to the gamut boundary, keeping the
/// hue exact and desaturating only as much as needed.
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
    use crate::theme::VisualEffect;

    /// The OKLab ΔE bar above which two colours count as "distinguishable" under
    /// simulation. ≈5× the ~0.02 OKLab JND, so it is clearly suprathreshold.
    /// Test-only and tunable (Director Q-5).
    const DELTA_E_BAR: f32 = 0.10;

    const ALL_TYPES: [CvdType; 3] = [CvdType::Protan, CvdType::Deutan, CvdType::Tritan];

    /// OKLab ΔE (euclidean distance in OKLab) between two sRGB colours.
    fn delta_e(a: Srgb, b: Srgb) -> f32 {
        let la = linear_to_oklab(to_linear(a));
        let lb = linear_to_oklab(to_linear(b));
        ((la.l - lb.l).powi(2) + (la.a - lb.a).powi(2) + (la.b - lb.b).powi(2)).sqrt()
    }

    /// A floor-clean dark fixture spec built from representative chromatic roles,
    /// pre-floored so the re-floor inside [`adapt_palette`] is a genuine no-op at
    /// `strength = 0` (lets the structural-hold / identity tests be exact).
    fn fixture() -> ThemeSpec {
        let mut spec = ThemeSpec {
            name: "fixture".to_string(),
            appearance: Appearance::Dark,
            foreground: (0xD8, 0xDF, 0xE6),
            background: (0x08, 0x0C, 0x10),
            clear: (0x03, 0x05, 0x08),
            palette: crate::text::DEFAULT_ANSI_SRGB,
            cursor: (0x6F, 0xA9, 0xE6),
            selection: (0x21, 0x34, 0x49),
            search: (0x35, 0x4A, 0x5F),
            border: (0x1D, 0x25, 0x2E),
            inactive: (0x5B, 0x64, 0x6F),
            font_family: None,
            font_size: None,
            visual: VisualEffect::Off,
        };
        // Pre-floor against the same surface mapping so `adapt_palette(.,.,0)` is
        // an exact fixed point.
        validate(&mut spec, AUTHORING_FLOOR);
        spec
    }

    #[test]
    fn adapt_palette_is_deterministic() {
        let spec = fixture();
        for ty in ALL_TYPES {
            let a = adapt_palette(&spec, ty, 1.0);
            let b = adapt_palette(&spec, ty, 1.0);
            assert_eq!(a, b, "adapt_palette must be byte-identical for {ty:?}");
        }
    }

    #[test]
    fn strength_zero_is_a_bitwise_passthrough_per_color() {
        // The wiring layer's "off" relies on this exact identity.
        for ty in ALL_TYPES {
            for &c in &crate::text::DEFAULT_ANSI_SRGB {
                assert_eq!(cvd_adapt(c, ty, 0.0), c, "{ty:?} strength 0 moved {c:?}");
            }
        }
    }

    #[test]
    fn adapt_palette_at_zero_strength_only_refloors() {
        // On a pre-floored fixture the re-floor is a no-op, so strength 0 returns
        // the spec unchanged.
        let spec = fixture();
        for ty in ALL_TYPES {
            assert_eq!(adapt_palette(&spec, ty, 0.0), spec, "{ty:?}");
        }
    }

    #[test]
    fn pinned_semantic_pairs_clear_the_delta_e_bar_after_adapt() {
        // Director Q-5: red(1/9) vs green(2/10) for protan+deutan; blue(4/12) vs
        // yellow(3/11) for tritan must separate by >= the bar, measured through
        // the matching simulation after the full palette adapt.
        let spec = fixture();
        let check = |ty: CvdType, x: usize, y: usize| {
            let adapted = adapt_palette(&spec, ty, 1.0);
            let sx = cvd_simulate(adapted.palette[x], ty);
            let sy = cvd_simulate(adapted.palette[y], ty);
            let de = delta_e(sx, sy);
            assert!(
                de >= DELTA_E_BAR,
                "{ty:?}: color{x} vs color{y} only ΔE {de} under simulation"
            );
        };
        for ty in [CvdType::Protan, CvdType::Deutan] {
            check(ty, 1, 2); // red vs green
            check(ty, 9, 10); // bright red vs bright green
        }
        check(CvdType::Tritan, 4, 3); // blue vs yellow
        check(CvdType::Tritan, 12, 11); // bright blue vs bright yellow
    }

    #[test]
    fn adapt_separates_a_genuinely_confusable_pair() {
        // Two colours that collapse onto the SAME perceived point pre-adapt (same
        // L and b, opposite a) must separate past the bar post-adapt — this is
        // the mechanism doing real work, not riding a pre-existing L gap.
        let red_ish = from_linear(oklch_to_linear_gamut(oklab_to_oklch(Oklab {
            l: 0.60,
            a: 0.12,
            b: 0.0,
        })));
        let green_ish = from_linear(oklch_to_linear_gamut(oklab_to_oklch(Oklab {
            l: 0.60,
            a: -0.12,
            b: 0.0,
        })));
        for ty in [CvdType::Protan, CvdType::Deutan] {
            let before = delta_e(cvd_simulate(red_ish, ty), cvd_simulate(green_ish, ty));
            let after = delta_e(
                cvd_simulate(cvd_adapt(red_ish, ty, 1.0), ty),
                cvd_simulate(cvd_adapt(green_ish, ty, 1.0), ty),
            );
            assert!(
                before < 0.02,
                "{ty:?}: the pair should be confusable pre-adapt, ΔE {before}"
            );
            assert!(
                after >= DELTA_E_BAR,
                "{ty:?}: adapt failed to separate the pair, ΔE {after}"
            );
        }
    }

    #[test]
    fn every_remapped_role_clears_the_floor_after_adapt() {
        // Readability survives the remap: each floored role over its mapped
        // surface clears the floor (respecting the bg-side neutral exemption),
        // at the default and a raised floor target.
        let spec = fixture();
        for ty in ALL_TYPES {
            let adapted = adapt_palette(&spec, ty, 1.0);
            let bg = to_linear(adapted.background);
            assert!(wcag_contrast(to_linear(adapted.foreground), bg) >= AUTHORING_FLOOR - 1e-3);
            let exempt = bg_side_neutral_slots(adapted.appearance);
            for (i, &c) in adapted.palette.iter().enumerate() {
                if exempt.contains(&i) {
                    continue;
                }
                let ratio = wcag_contrast(to_linear(c), bg);
                assert!(
                    ratio >= AUTHORING_FLOOR - 1e-3,
                    "{ty:?} palette[{i}] only {ratio}"
                );
            }
            assert!(wcag_contrast(to_linear(adapted.cursor), bg) >= AUTHORING_FLOOR - 1e-3);
            let fg = to_linear(adapted.foreground);
            assert!(wcag_contrast(to_linear(adapted.selection), fg) >= AUTHORING_FLOOR - 1e-3);
            assert!(wcag_contrast(to_linear(adapted.search), fg) >= AUTHORING_FLOOR - 1e-3);
        }
    }

    #[test]
    fn refloor_preserves_the_remap_separation_orthogonally() {
        // The floor moves only L, so the a/b separation it leaves behind matches
        // the separation the remap created (the floor cannot undo it). Compare
        // the post-remap a/b gap of a semantic pair to the post-floor gap.
        let spec = fixture();
        for ty in [CvdType::Protan, CvdType::Deutan] {
            let pre_red = cvd_adapt(spec.palette[1], ty, 1.0);
            let pre_green = cvd_adapt(spec.palette[2], ty, 1.0);
            let ab_gap = |x: Srgb, y: Srgb| {
                let lx = linear_to_oklab(to_linear(x));
                let ly = linear_to_oklab(to_linear(y));
                ((lx.a - ly.a).powi(2) + (lx.b - ly.b).powi(2)).sqrt()
            };
            let pre = ab_gap(pre_red, pre_green);
            let adapted = adapt_palette(&spec, ty, 1.0);
            let post = ab_gap(adapted.palette[1], adapted.palette[2]);
            // The floor only lifts L; any a/b change is incidental gamut slop.
            assert!(
                (post - pre).abs() < 0.03,
                "{ty:?}: floor disturbed a/b separation, pre {pre} post {post}"
            );
        }
    }

    #[test]
    fn background_and_foreground_are_held_structural() {
        // bg is never touched; fg is held (and, on a pre-floored fixture, exact).
        let spec = fixture();
        for ty in ALL_TYPES {
            for s in [0.0, 0.5, 1.0] {
                let adapted = adapt_palette(&spec, ty, s);
                assert_eq!(adapted.background, spec.background, "{ty:?} s={s} bg moved");
                assert_eq!(adapted.foreground, spec.foreground, "{ty:?} s={s} fg moved");
                assert_eq!(adapted.border, spec.border, "{ty:?} s={s} border moved");
                assert_eq!(
                    adapted.inactive, spec.inactive,
                    "{ty:?} s={s} inactive moved"
                );
                assert_eq!(adapted.clear, spec.clear, "{ty:?} s={s} clear moved");
            }
        }
    }

    #[test]
    fn applying_the_remap_twice_over_shifts_versus_once() {
        // cvd_adapt is not idempotent; the wrapper's single-pass discipline
        // matters. A confusable colour shifts further on a second application.
        let c = spec_red();
        for ty in [CvdType::Protan, CvdType::Deutan] {
            let once = cvd_adapt(c, ty, 1.0);
            let twice = cvd_adapt(once, ty, 1.0);
            assert_ne!(once, twice, "{ty:?}: second application should over-shift");
        }
    }

    fn spec_red() -> Srgb {
        crate::text::DEFAULT_ANSI_SRGB[1]
    }

    #[test]
    fn strength_sweep_is_monotonic_between_identity_and_full() {
        // strength 0 is identity, 1 is full; the shift magnitude grows with
        // strength in between.
        let c = spec_red();
        for ty in ALL_TYPES {
            let mut prev = 0.0_f32;
            for step in 0..=10 {
                let s = step as f32 / 10.0;
                let shift = delta_e(c, cvd_adapt(c, ty, s));
                if step == 0 {
                    assert_eq!(shift, 0.0, "{ty:?} strength 0 must be identity");
                }
                assert!(
                    shift >= prev - 1e-4,
                    "{ty:?} shift not monotonic at s={s}: {shift} < {prev}"
                );
                prev = shift;
            }
        }
    }

    #[test]
    fn simulate_collapses_the_deficient_axis() {
        // The simulation basis is real: simulating a red and a green under deutan
        // drives their red–green (`a`) coordinate together. Proves the test/
        // preview basis genuinely models the loss.
        for ty in [CvdType::Protan, CvdType::Deutan] {
            let r = linear_to_oklab(to_linear(cvd_simulate(spec_red(), ty)));
            let g = linear_to_oklab(to_linear(cvd_simulate(
                crate::text::DEFAULT_ANSI_SRGB[2],
                ty,
            )));
            assert!(r.a.abs() < 0.02 && g.a.abs() < 0.02, "a not collapsed");
        }
        // Tritan collapses the blue–yellow (`b`) axis.
        let b = linear_to_oklab(to_linear(cvd_simulate(
            crate::text::DEFAULT_ANSI_SRGB[4],
            CvdType::Tritan,
        )));
        assert!(b.b.abs() < 0.02, "b not collapsed under tritan");
    }

    #[test]
    fn achromatic_colors_survive_adapt_without_panicking() {
        // A grey has ~zero chroma → near-zero lost component → near-identity. No
        // NaN, no panic, and it stays close to grey.
        for grey in [(0x00, 0x00, 0x00), (0x7F, 0x7F, 0x7F), (0xFF, 0xFF, 0xFF)] {
            for ty in ALL_TYPES {
                let out = cvd_adapt(grey, ty, 1.0);
                let lab = linear_to_oklab(to_linear(out));
                assert!(lab.l.is_finite() && lab.a.is_finite() && lab.b.is_finite());
                assert!(
                    lab.a.abs() < 0.03 && lab.b.abs() < 0.03,
                    "grey {grey:?} drifted chromatic under {ty:?}: {lab:?}"
                );
            }
        }
    }

    #[test]
    fn adapted_spec_round_trips_through_the_theme_format() {
        // The adapted spec rides the existing on-disk path with no additions.
        let adapted = adapt_palette(&fixture(), CvdType::Deutan, 1.0);
        let text = adapted.serialize();
        let reparsed = ThemeSpec::parse(&text, |m| panic!("warn: {m}"));
        assert_eq!(reparsed, adapted);
        // No real home dir leaks into the serialized fixture text.
        assert!(!text.contains("/home/"));
    }

    #[test]
    fn golden_adapted_palette_for_deutan() {
        // Pin the exact byte output for the fixture under deutan at full
        // strength. A change to any constant, the algorithm, or the floor pass
        // that alters the result trips this on purpose.
        let adapted = adapt_palette(&fixture(), CvdType::Deutan, 1.0);
        assert_eq!(
            adapted.palette,
            [
                (0, 0, 0),
                (169, 134, 0),
                (158, 160, 164),
                (224, 190, 96),
                (79, 107, 255),
                (152, 142, 113),
                (142, 169, 255),
                (229, 229, 229),
                (127, 127, 127),
                (179, 142, 0),
                (198, 200, 205),
                (255, 240, 200),
                (84, 113, 246),
                (184, 171, 136),
                (202, 215, 255),
                (255, 255, 255),
            ]
        );
        assert_eq!(adapted.cursor, (131, 158, 245));
        assert_eq!(adapted.selection, (39, 48, 78));
        assert_eq!(adapted.search, (60, 70, 101));
    }
}

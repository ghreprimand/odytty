// SPDX-License-Identifier: GPL-3.0-only
//! Perceptual-safe theme authoring math (U2 core): the pure, deterministic
//! primitives the interactive theme builder wires its OKLCH sliders, live
//! contrast readout, and snap-to-floor onto.
//!
//! The native builder (sliders, the readout widget, the hex-entry fallback) is a
//! separate, later packet; this module is only the math, with no I/O, no RNG, and
//! no UI. Every function is pure and deterministic — identical inputs yield
//! byte-identical output — so the whole authoring surface is golden-testable.
//!
//! ## The guarantee: "you cannot author an unreadable theme"
//!
//! The builder edits a role in OKLCH (perceptually uniform lightness / chroma /
//! hue) via [`nudge`], shows the live WCAG contrast of the role against the
//! surface it is read on via [`authoring_contrast`], and — when the operator
//! asks, or as a hard backstop on write — pulls the role up to the authoring
//! floor via [`snap_to_floor`]. Because the snap is validated on the **final
//! quantized 8-bit bytes**, the value the builder writes to the `.theme` is
//! exactly what the renderer draws: *authored == rendered*. No render-time
//! re-nudge is needed for an authored theme.
//!
//! ## Two distinct floors
//!
//! - **Authoring floor** ([`AUTHORING_CONTRAST_FLOOR`], WCAG 4.5): what the
//!   builder authors against — the WCAG AA body-text threshold, so a theme built
//!   here is comfortably legible, not merely above the bare render minimum.
//! - **Render-time floor** (`min_contrast`, default 1.0): the universal safety
//!   net the renderer applies to *all* text (U1). It stays at unity by default so
//!   it never silently rewrites a deliberately-authored low-contrast accent.
//!
//! The builder authors against 4.5; the renderer floor stays 1.0. They are
//! deliberately separate constants so neither moves the other.
//!
//! ## Relationship to the palette generator
//!
//! [`crate::palette_gen`] solves the one-shot "seed → whole legible theme"
//! problem; this module solves the interactive "operator is dragging a slider on
//! one role" problem. They share the same hue-preserving chroma-reduction gamut
//! map and the same byte-rechecked floor technique, reimplemented locally here on
//! purpose: consolidating both into one shared authoring core is a deliberately
//! deferred task, not part of this packet.

use crate::color::{
    self, LinearRgb, Oklch, linear_to_oklab, linear_to_srgb_u8, oklab_to_linear, oklab_to_oklch,
    oklch_to_oklab, srgb_to_linear,
};
use crate::theme::{Appearance, Srgb, ThemeSpec};

/// The WCAG contrast ratio the interactive builder authors against — the AA
/// body-text threshold. Distinct from the render-time `min_contrast` floor
/// (default 1.0): the builder snaps roles up to 4.5 so an authored theme is
/// comfortably legible, while the renderer's universal floor stays at unity and
/// never rewrites a deliberately low-contrast accent.
pub const AUTHORING_CONTRAST_FLOOR: f32 = 4.5;

/// Nudge a color in OKLCH by additive lightness / chroma / hue deltas, then map
/// the result back into the sRGB gamut **preserving hue** and quantize to bytes.
///
/// This backs the builder's three sliders: `dl` moves perceptual lightness, `dc`
/// moves chroma (saturation), `dh` rotates hue (radians). Lightness is clamped to
/// `[0, 1]` and chroma to `>= 0`; hue is free (the `cos`/`sin` round-trip handles
/// wrap). If the nudged chroma exceeds the sRGB gamut at that lightness, the
/// gamut map reduces chroma toward the neutral axis rather than clamping
/// per-channel (which would skew the hue), so the hue the operator dialed in is
/// the hue they get.
///
/// Pure and deterministic.
pub fn nudge(color: Srgb, dl: f32, dc: f32, dh: f32) -> Srgb {
    let lch = srgb_to_oklch(color);
    oklch_to_srgb(Oklch {
        l: (lch.l + dl).clamp(0.0, 1.0),
        c: (lch.c + dc).max(0.0),
        h: lch.h + dh,
    })
}

/// Return the nearest sRGB color to `candidate` that provably clears `floor`
/// WCAG contrast against `partner`, preserving `candidate`'s hue and moving its
/// lightness away from the partner.
///
/// This is the "cannot author unreadable" backstop. The guarantee is checked on
/// the **final quantized 8-bit bytes**, not on the intermediate float color, so
/// the value returned here is exactly what the renderer will draw against the
/// same partner — *authored == rendered*. The underlying
/// [`enforce_min_contrast`] only moves OKLab lightness (hue and chroma
/// preserved) and picks the lightness direction that increases contrast, so the
/// snap lifts a too-dark role lighter on a dark surface and pushes a too-light
/// role darker on a light surface.
///
/// `floor <= 1.0` is an exact passthrough no-op: there is no floor to enforce, so
/// `candidate` is returned bit-for-bit (matching [`enforce_min_contrast`]'s
/// contract).
///
/// Best-effort cap: against a near-mid-grey partner, even pure black or white may
/// not reach a high `floor`; in that pathological case the most-contrasting
/// in-gamut result is returned. For real backgrounds / foregrounds (the authoring
/// case) the floor is always reachable and provably met.
///
/// Pure and deterministic; never panics or yields a non-finite channel.
pub fn snap_to_floor(candidate: Srgb, partner: Srgb, floor: f32) -> Srgb {
    // Delegates to the single shared floor/re-check implementation
    // (`palette_gen::floor_role`), which enforces the floor in linear space,
    // then gamut-maps (hue-preserving), quantizes, and re-checks the byte
    // result, bumping the internal target if rounding/gamut shaved it under.
    // `floor <= 1.0` is the passthrough no-op. `snap_to_floor` differs only in
    // taking its partner as `Srgb`; converting to the linear surface here is
    // exactly what the old inline body did (`partner_lin = to_linear(partner)`),
    // so this is behavior-preserving and keeps the three floor passes on one
    // implementation (see the cross-check test below).
    crate::palette_gen::floor_role(candidate, to_linear(partner), floor)
}

/// The live WCAG contrast ratio between two sRGB colors, for the builder's
/// readout. A thin wrapper over [`color::wcag_contrast`] that decodes both bytes
/// through the same linearization the render floor uses, so the number shown is
/// the number enforced. Range `[1.0, 21.0]`.
pub fn authoring_contrast(a: Srgb, b: Srgb) -> f32 {
    color::wcag_contrast(to_linear(a), to_linear(b))
}

/// A theme role the builder can edit, used to decide which surface its contrast
/// is floored against (see [`floor_partner`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorRole {
    /// Body text (`foreground`).
    Foreground,
    /// One of the 16 ANSI palette slots.
    Palette(usize),
    /// The cursor block.
    Cursor,
    /// The selection fill.
    Selection,
    /// The search-highlight fill.
    Search,
    /// The window background (the surface itself — not floored).
    Background,
    /// Window-clear color (chrome — not floored).
    Clear,
    /// Window border (chrome — not floored).
    Border,
    /// Inactive / dim chrome (not floored).
    Inactive,
}

/// Which surface a role's contrast is floored against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorAgainst {
    /// Text roles (foreground, the ANSI palette, the cursor) are read on the
    /// window background.
    Background,
    /// Fill roles (selection, search) carry the foreground text drawn over them,
    /// so they are floored against the foreground. `wcag_contrast` is symmetric,
    /// so flooring the fill against the foreground guarantees the
    /// foreground-over-fill pair clears the floor.
    Foreground,
}

/// The two achromatic ANSI neutrals sitting on the **same side as the
/// background**, exempt from the vs-background floor so they stay as near-bg
/// structural ramp neutrals. Dark themes keep `color0`/`color8` (the dark
/// neutrals near a dark bg); light themes keep `color7`/`color15` (the light
/// neutrals near a light bg). The opposite pair sits near the foreground and
/// stays floored.
///
/// The exemption table is the shared `palette_gen::bg_side_neutral_slots`
/// (imported below), so the interactive author-time snap and the one-shot
/// generator exempt exactly the same slots by construction -- pre-flooring these
/// would collapse the very ramp the generator protects, and the render-time
/// floor (U1) still lifts any text that actually lands on the background at draw
/// time.
use crate::palette_gen::bg_side_neutral_slots;

/// Map a role to the surface its contrast must clear the floor against, or `None`
/// for roles that are not floored (the background itself, the window chrome, and
/// the background-side neutral ramp pair).
///
/// This is the single source of the author-time surface mapping; it matches the
/// generator's RV1 validation pass ([`palette_gen::validate`](crate::palette_gen))
/// verbatim — including the [`bg_side_neutral_slots`] exemption — so the
/// interactive snap and the one-shot generator agree on what is floored against
/// what. The palette mapping is therefore **appearance-dependent**: which two
/// neutral slots are exempt flips with the light/dark polarity.
pub fn floor_partner(role: AuthorRole, appearance: Appearance) -> Option<FloorAgainst> {
    match role {
        AuthorRole::Foreground | AuthorRole::Cursor => Some(FloorAgainst::Background),
        AuthorRole::Palette(index) => {
            // The background-side neutral pair is a structural ramp neutral, not
            // primary text: it stays near the background, unfloored. Every other
            // slot (chromatic families + the foreground-side neutral pair) floors
            // against the background.
            if bg_side_neutral_slots(appearance).contains(&index) {
                None
            } else {
                Some(FloorAgainst::Background)
            }
        }
        AuthorRole::Selection | AuthorRole::Search => Some(FloorAgainst::Foreground),
        AuthorRole::Background | AuthorRole::Clear | AuthorRole::Border | AuthorRole::Inactive => {
            None
        }
    }
}

/// Resolve the concrete partner color a role is floored against within a given
/// spec, or `None` when the role is not floored. A convenience over
/// [`floor_partner`] that reads the appearance + surface color out of `spec` so a
/// caller can `snap_to_floor(role_color, partner, AUTHORING_CONTRAST_FLOOR)`
/// directly. Because the background-side neutral exemption is appearance-keyed,
/// this resolves it from `spec.appearance` — the same polarity the generator
/// used to author the theme.
pub fn partner_color(spec: &ThemeSpec, role: AuthorRole) -> Option<Srgb> {
    floor_partner(role, spec.appearance).map(|against| match against {
        FloorAgainst::Background => spec.background,
        FloorAgainst::Foreground => spec.foreground,
    })
}

// ---------------------------------------------------------------------------
// Srgb ↔ linear ↔ OKLCH bridges (thin, local wrappers over `color`)
//
// Reimplemented here rather than shared with `palette_gen` on purpose: folding
// both modules' bridges into one authoring core is a deliberately deferred task.
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
/// hue-preserving chroma reduction.
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
/// boundary, keeping the hue exact and desaturating only as much as needed.
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
    use std::f32::consts::{PI, TAU};

    /// Wrap an angle (radians) into `(-π, π]` so hue deltas take the short way
    /// around the circle (test-local helper).
    fn wrap_angle(mut a: f32) -> f32 {
        while a > PI {
            a -= TAU;
        }
        while a <= -PI {
            a += TAU;
        }
        a
    }

    /// A coarse sweep of mid-lightness candidate colors stepped around the hue
    /// wheel, used by the by-construction guarantees.
    fn candidate_sweep() -> Vec<Srgb> {
        let mut out = Vec::new();
        for step in 0..12 {
            let h = step as f32 / 12.0 * TAU - PI;
            // Two lightnesses per hue so the snap is exercised in both
            // directions on each polarity.
            for l in [0.40_f32, 0.62] {
                out.push(oklch_to_srgb(Oklch { l, c: 0.12, h }));
            }
        }
        out
    }

    // --- nudge ---------------------------------------------------------------

    #[test]
    fn nudge_is_deterministic() {
        let c = (0x4f, 0x9c, 0xff);
        assert_eq!(nudge(c, 0.05, -0.02, 0.1), nudge(c, 0.05, -0.02, 0.1));
    }

    #[test]
    fn nudge_zero_delta_round_trips_within_quantization() {
        // A zero nudge is identity up to the sRGB→OKLCH→sRGB byte round-trip,
        // which is exact or off by at most one quantization step per channel.
        for &c in &candidate_sweep() {
            let r = nudge(c, 0.0, 0.0, 0.0);
            for (a, b) in [(c.0, r.0), (c.1, r.1), (c.2, r.2)] {
                assert!(
                    (a as i16 - b as i16).abs() <= 1,
                    "zero-nudge drifted {c:?} -> {r:?}"
                );
            }
        }
    }

    #[test]
    fn nudge_hue_rotation_preserves_target_hue() {
        // Rotating hue by dh lands (after gamut-map + quantization) within a
        // small tolerance of the intended hue — the operator gets the hue dialed.
        let base = (0x4f, 0x9c, 0xff);
        let base_h = srgb_to_oklch(base).h;
        for &dh in &[0.0_f32, 0.3, -0.5, 1.2] {
            let out = nudge(base, 0.0, 0.0, dh);
            let got = srgb_to_oklch(out);
            if got.c < 0.02 {
                continue; // near-grey: hue undefined, skip
            }
            let drift = wrap_angle(got.h - (base_h + dh)).abs();
            assert!(drift < 0.08, "hue drift {drift} for dh={dh}");
        }
    }

    #[test]
    fn nudge_lightness_clamps_and_stays_in_gamut() {
        // Pushing lightness past the ends clamps without producing a non-finite
        // or out-of-range byte.
        let c = (0x80, 0x40, 0xc0);
        let up = nudge(c, 5.0, 0.0, 0.0);
        let down = nudge(c, -5.0, 0.0, 0.0);
        // L clamps to the ends; residual chroma at the extreme can leave a
        // one-or-two-step tint, so assert near-white / near-black rather than the
        // exact corner.
        for ch in [up.0, up.1, up.2] {
            assert!(ch >= 252, "up nudge not near-white: {up:?}");
        }
        for ch in [down.0, down.1, down.2] {
            assert!(ch <= 3, "down nudge not near-black: {down:?}");
        }
    }

    #[test]
    fn nudge_chroma_does_not_go_negative_or_panic() {
        // A large negative chroma delta drives toward the neutral axis (grey),
        // never a panic or garbage value.
        let c = (0x4f, 0x9c, 0xff);
        let out = nudge(c, 0.0, -10.0, 0.0);
        let lch = srgb_to_oklch(out);
        assert!(lch.c < 0.03, "expected near-grey, got chroma {}", lch.c);
    }

    // --- snap_to_floor -------------------------------------------------------

    #[test]
    fn snap_is_deterministic() {
        let cand = (0x33, 0x55, 0x44);
        let partner = (0x10, 0x12, 0x16);
        assert_eq!(
            snap_to_floor(cand, partner, AUTHORING_CONTRAST_FLOOR),
            snap_to_floor(cand, partner, AUTHORING_CONTRAST_FLOOR)
        );
    }

    #[test]
    fn snap_floor_of_one_is_exact_passthrough() {
        // floor <= 1.0 returns the candidate bit-for-bit.
        let cand = (0x33, 0x55, 0x44);
        let partner = (0x10, 0x12, 0x16);
        assert_eq!(snap_to_floor(cand, partner, 1.0), cand);
        assert_eq!(snap_to_floor(cand, partner, 0.5), cand);
    }

    #[test]
    fn floor_math_is_shared_across_author_generator_and_cvd() {
        // The three floor passes (author snap, generator validate, CVD adapt)
        // now share one implementation. This pins the two remaining wrappers to
        // it so a future edit that reintroduces a private copy is caught:
        //   - `snap_to_floor(c, p, f)` must equal `palette_gen::floor_role(c,
        //     to_linear(p), f)` byte-for-byte across a hue/lightness sweep, both
        //     surface polarities, and several floors.
        //   - the vs-background exemption table must be one shared function.
        let dark_partner = (0x0a, 0x0c, 0x10);
        let light_partner = (0xf2, 0xf3, 0xf5);
        for partner in [dark_partner, light_partner] {
            for &cand in &candidate_sweep() {
                for &floor in &[1.0_f32, 3.0, AUTHORING_CONTRAST_FLOOR, 7.0] {
                    assert_eq!(
                        snap_to_floor(cand, partner, floor),
                        crate::palette_gen::floor_role(cand, to_linear(partner), floor),
                        "snap_to_floor must delegate to the shared floor_role \
                         (cand={cand:?}, partner={partner:?}, floor={floor})"
                    );
                }
            }
        }
        for appearance in [Appearance::Dark, Appearance::Light] {
            assert_eq!(
                bg_side_neutral_slots(appearance),
                crate::palette_gen::bg_side_neutral_slots(appearance),
                "the exemption table must be the single shared implementation"
            );
        }
    }

    #[test]
    fn snap_already_legible_is_a_no_op() {
        // White on near-black already clears 4.5; snap must not move it.
        let cand = (0xff, 0xff, 0xff);
        let partner = (0x08, 0x0a, 0x0e);
        assert!(wcag_contrast(to_linear(cand), to_linear(partner)) >= AUTHORING_CONTRAST_FLOOR);
        assert_eq!(snap_to_floor(cand, partner, AUTHORING_CONTRAST_FLOOR), cand);
    }

    #[test]
    fn snap_clears_floor_on_quantized_bytes_both_polarities() {
        // The headline guarantee + authored==rendered: across a hue/lightness
        // sweep, on both a dark surface (text floored lighter) and a light
        // surface (text floored darker), the SNAPPED 8-bit bytes provably clear
        // the floor when re-checked exactly as the renderer would.
        let dark_partner = (0x0a, 0x0c, 0x10);
        let light_partner = (0xf2, 0xf3, 0xf5);
        for partner in [dark_partner, light_partner] {
            for &cand in &candidate_sweep() {
                let snapped = snap_to_floor(cand, partner, AUTHORING_CONTRAST_FLOOR);
                let rendered = wcag_contrast(to_linear(snapped), to_linear(partner));
                assert!(
                    rendered >= AUTHORING_CONTRAST_FLOOR - 1e-3,
                    "snapped {snapped:?} vs {partner:?} only {rendered} (cand {cand:?})"
                );
            }
        }
    }

    #[test]
    fn snap_preserves_hue_while_moving_lightness() {
        // The snap moves only lightness (and minimal gamut chroma), so a
        // saturated candidate keeps its hue after being lifted to the floor.
        let cand = oklch_to_srgb(Oklch {
            l: 0.30,
            c: 0.10,
            h: 0.8,
        });
        let partner = (0x0a, 0x0c, 0x10);
        let before = srgb_to_oklch(cand);
        let snapped = snap_to_floor(cand, partner, AUTHORING_CONTRAST_FLOOR);
        let after = srgb_to_oklch(snapped);
        if after.c >= 0.02 {
            let drift = wrap_angle(after.h - before.h).abs();
            assert!(drift < 0.08, "hue drifted {drift} during snap");
        }
        // And contrast actually improved to meet the floor.
        assert!(
            wcag_contrast(to_linear(snapped), to_linear(partner))
                >= AUTHORING_CONTRAST_FLOOR - 1e-3
        );
    }

    #[test]
    fn snap_is_idempotent_on_its_own_output() {
        // Snapping an already-snapped value changes nothing material — a second
        // pass returns a byte-identical result.
        let cand = oklch_to_srgb(Oklch {
            l: 0.35,
            c: 0.11,
            h: -1.2,
        });
        let partner = (0x0a, 0x0c, 0x10);
        let once = snap_to_floor(cand, partner, AUTHORING_CONTRAST_FLOOR);
        let twice = snap_to_floor(once, partner, AUTHORING_CONTRAST_FLOOR);
        assert_eq!(once, twice);
    }

    #[test]
    fn snap_achromatic_candidate_and_partner_is_finite_and_legible() {
        // Grey-on-grey has no hue; the snap must not NaN/panic and must still
        // reach the floor (the partner here is dark enough for it to be feasible).
        let cand = (0x20, 0x20, 0x20);
        let partner = (0x05, 0x05, 0x05);
        let snapped = snap_to_floor(cand, partner, AUTHORING_CONTRAST_FLOOR);
        let c = wcag_contrast(to_linear(snapped), to_linear(partner));
        assert!(
            c.is_finite() && c >= AUTHORING_CONTRAST_FLOOR - 1e-3,
            "grey snap gave {c}"
        );
    }

    // --- authoring_contrast --------------------------------------------------

    #[test]
    fn authoring_contrast_matches_color_primitive() {
        let a = (0xd0, 0xd4, 0xda);
        let b = (0x10, 0x12, 0x16);
        assert_eq!(
            authoring_contrast(a, b),
            wcag_contrast(to_linear(a), to_linear(b))
        );
    }

    #[test]
    fn authoring_contrast_is_symmetric_and_bounded() {
        let a = (0xff, 0xff, 0xff);
        let b = (0x00, 0x00, 0x00);
        let ab = authoring_contrast(a, b);
        assert_eq!(ab, authoring_contrast(b, a));
        assert!((1.0..=21.0).contains(&ab), "contrast {ab} out of range");
        // Identical colors contrast at 1.0.
        assert!((authoring_contrast(a, a) - 1.0).abs() < 1e-3);
    }

    // --- role → partner mapping ----------------------------------------------

    #[test]
    fn text_roles_floor_against_background() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            assert_eq!(
                floor_partner(AuthorRole::Foreground, appearance),
                Some(FloorAgainst::Background)
            );
            assert_eq!(
                floor_partner(AuthorRole::Cursor, appearance),
                Some(FloorAgainst::Background)
            );
        }
    }

    #[test]
    fn chromatic_and_far_neutral_palette_slots_floor_against_background() {
        // Every palette slot EXCEPT the background-side neutral pair floors
        // against the background, in both appearances.
        for appearance in [Appearance::Dark, Appearance::Light] {
            let exempt = bg_side_neutral_slots(appearance);
            for i in 0..16 {
                let expected = if exempt.contains(&i) {
                    None
                } else {
                    Some(FloorAgainst::Background)
                };
                assert_eq!(
                    floor_partner(AuthorRole::Palette(i), appearance),
                    expected,
                    "palette[{i}] in {appearance:?}"
                );
            }
        }
    }

    #[test]
    fn background_side_neutral_pair_is_exempt_per_appearance() {
        // Mirrors palette_gen::validate: dark exempts color0/color8, light exempts
        // color7/color15 — these structural ramp neutrals are NOT pre-floored.
        for slot in [0, 8] {
            assert_eq!(
                floor_partner(AuthorRole::Palette(slot), Appearance::Dark),
                None,
                "dark color{slot} should be exempt"
            );
        }
        for slot in [7, 15] {
            assert_eq!(
                floor_partner(AuthorRole::Palette(slot), Appearance::Light),
                None,
                "light color{slot} should be exempt"
            );
        }
    }

    #[test]
    fn opposite_side_neutral_pair_stays_floored() {
        // The neutral pair on the FOREGROUND side (opposite the background) clears
        // the floor naturally and stays floored against the background. In a dark
        // theme that is color7/color15; in a light theme, color0/color8.
        for slot in [7, 15] {
            assert_eq!(
                floor_partner(AuthorRole::Palette(slot), Appearance::Dark),
                Some(FloorAgainst::Background),
                "dark color{slot} (fg-side neutral) should stay floored"
            );
        }
        for slot in [0, 8] {
            assert_eq!(
                floor_partner(AuthorRole::Palette(slot), Appearance::Light),
                Some(FloorAgainst::Background),
                "light color{slot} (fg-side neutral) should stay floored"
            );
        }
    }

    #[test]
    fn fill_roles_floor_against_foreground() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            assert_eq!(
                floor_partner(AuthorRole::Selection, appearance),
                Some(FloorAgainst::Foreground)
            );
            assert_eq!(
                floor_partner(AuthorRole::Search, appearance),
                Some(FloorAgainst::Foreground)
            );
        }
    }

    #[test]
    fn chrome_and_background_roles_are_not_floored() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            for role in [
                AuthorRole::Background,
                AuthorRole::Clear,
                AuthorRole::Border,
                AuthorRole::Inactive,
            ] {
                assert_eq!(
                    floor_partner(role, appearance),
                    None,
                    "{role:?} should be unfloored"
                );
            }
        }
    }

    #[test]
    fn partner_color_resolves_the_surface_from_the_spec() {
        // ThemeSpec::default() is a Dark-appearance spec, so color0/color8 are
        // the background-side exempt neutrals.
        let spec = ThemeSpec::default();
        assert_eq!(spec.appearance, Appearance::Dark);
        // Text role → background; chromatic palette slot → background.
        assert_eq!(
            partner_color(&spec, AuthorRole::Foreground),
            Some(spec.background)
        );
        assert_eq!(
            partner_color(&spec, AuthorRole::Palette(3)),
            Some(spec.background)
        );
        // Background-side neutral (dark color0) resolves to None — not floored.
        assert_eq!(partner_color(&spec, AuthorRole::Palette(0)), None);
        // Fill role → foreground; chrome → None.
        assert_eq!(
            partner_color(&spec, AuthorRole::Selection),
            Some(spec.foreground)
        );
        assert_eq!(partner_color(&spec, AuthorRole::Border), None);
    }

    #[test]
    fn snap_a_role_against_its_resolved_partner_clears_the_floor() {
        // End-to-end: resolve a role's partner from a spec, snap the role color
        // against it, and confirm the snapped bytes clear the authoring floor —
        // the exact call shape the native builder will use.
        let spec = ThemeSpec::default();
        // Deliberately darken the foreground toward the background to break the
        // floor, then snap it back.
        let broken = nudge(spec.foreground, -0.55, 0.0, 0.0);
        let partner = partner_color(&spec, AuthorRole::Foreground).unwrap();
        let snapped = snap_to_floor(broken, partner, AUTHORING_CONTRAST_FLOOR);
        assert!(
            wcag_contrast(to_linear(snapped), to_linear(partner))
                >= AUTHORING_CONTRAST_FLOOR - 1e-3,
            "snapped role failed to clear the floor"
        );
    }
}

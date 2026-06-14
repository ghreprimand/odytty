// SPDX-License-Identifier: GPL-3.0-only
//! Perceptual color primitives (RV3).
//!
//! Linear/sRGB transfer plus OKLab / OKLCH conversions, used for
//! perceptually-uniform dimming, fading, and blending. The point of working in
//! OKLab is that equal numeric steps look like equal perceived steps — so SGR
//! dim, selection/search blends, and theme interpolation stay legible and even
//! instead of collapsing into mud the way a naive linear-RGB scale does.
//!
//! Everything here is a pure function with no external dependencies. The OKLab
//! matrices are Björn Ottosson's published constants (the same ones the CSS
//! Color 4 `oklab()`/`oklch()` definitions use); they are reproduced inline and
//! pinned by round-trip and reference-value tests.
//!
//! This module is the single source of truth for the sRGB transfer:
//! [`text::srgb_to_linear`](crate::text::srgb_to_linear) delegates to
//! [`srgb_to_linear`] here, so the byte path stays identical for every existing
//! caller (`native::gpu`, `grid`).

/// Scene-linear RGB, each channel nominally in `[0, 1]`.
pub type LinearRgb = [f32; 3];

// ---------------------------------------------------------------------------
// sRGB transfer (IEC 61966-2-1)
// ---------------------------------------------------------------------------

/// sRGB electro-optical transfer: one gamma-encoded channel in `[0, 1]` to
/// linear `[0, 1]`.
pub fn srgb_to_linear_f32(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer: linear `[0, 1]` to gamma-encoded `[0, 1]`.
pub fn linear_to_srgb_f32(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// One sRGB byte channel to a linear float in `[0, 1]`.
///
/// The historical [`text::srgb_to_linear`](crate::text::srgb_to_linear) is a
/// thin wrapper around this; keeping one implementation guarantees the surface
/// (which expects linear shader inputs) sees byte-identical values.
pub fn srgb_to_linear(byte: u8) -> f32 {
    srgb_to_linear_f32(byte as f32 / 255.0)
}

/// Linear float in `[0, 1]` to the nearest sRGB byte channel (rounded, clamped).
pub fn linear_to_srgb_u8(c: f32) -> u8 {
    let s = linear_to_srgb_f32(c.clamp(0.0, 1.0));
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// OKLab
// ---------------------------------------------------------------------------

/// A color in the OKLab perceptual space: lightness `l` plus the `a`/`b`
/// green–red and blue–yellow opponent axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

/// Convert scene-linear RGB to OKLab.
///
/// Constants are Ottosson's `linear_srgb_to_oklab` matrices.
pub fn linear_to_oklab(rgb: LinearRgb) -> Oklab {
    let [r, g, b] = rgb;

    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    Oklab {
        l: 0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        a: 1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        b: 0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    }
}

/// Convert OKLab back to scene-linear RGB.
///
/// Constants are Ottosson's `oklab_to_linear_srgb` matrices. The result is not
/// clamped to gamut; callers that need bytes go through [`linear_to_srgb_u8`].
pub fn oklab_to_linear(lab: Oklab) -> LinearRgb {
    let l_ = lab.l + 0.396_337_78 * lab.a + 0.215_803_76 * lab.b;
    let m_ = lab.l - 0.105_561_346 * lab.a - 0.063_854_17 * lab.b;
    let s_ = lab.l - 0.089_484_18 * lab.a - 1.291_485_5 * lab.b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

// ---------------------------------------------------------------------------
// OKLCH (cylindrical OKLab)
// ---------------------------------------------------------------------------

/// OKLab in cylindrical form: lightness `l`, chroma `c`, hue `h` in radians.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

/// Convert OKLab to OKLCH. Hue is `atan2(b, a)` in radians (`[-π, π]`).
pub fn oklab_to_oklch(lab: Oklab) -> Oklch {
    Oklch {
        l: lab.l,
        c: (lab.a * lab.a + lab.b * lab.b).sqrt(),
        h: lab.b.atan2(lab.a),
    }
}

/// Convert OKLCH back to OKLab.
pub fn oklch_to_oklab(lch: Oklch) -> Oklab {
    Oklab {
        l: lch.l,
        a: lch.c * lch.h.cos(),
        b: lch.c * lch.h.sin(),
    }
}

// ---------------------------------------------------------------------------
// Perceptual operations
// ---------------------------------------------------------------------------

/// The default perceptual-dim amount, the OKLab-L analog of the historical
/// naive linear `×0.5` SGR-dim scale. Tuned so dimmed body text reads as
/// "clearly fainter but still legible" rather than the harsher linear halving.
pub const DEFAULT_DIM_AMOUNT: f32 = 0.40;

/// Dim a linear color by scaling it toward black in OKLab.
///
/// `amount` is in `[0, 1]`: `0.0` returns the input unchanged (exact identity,
/// short-circuited before any round-trip so there is zero float drift), `1.0`
/// drives the color to black. All three OKLab coordinates (`L`, `a`, `b`) are
/// scaled by the same `1 - amount` factor.
///
/// HONESTY NOTE — this *uniform* OKLab scale is algebraically identical to a
/// *uniform* linear-RGB scale, and not a perceptual improvement over one for
/// this code path. OKLab's only nonlinearity is the per-component cube root
/// applied to an `LMS` mix that is linear in RGB; uniformly scaling `(L, a, b)`
/// by `k` therefore commutes back through the cube root to scaling linear RGB
/// by `k³`. Concretely `dim_perceptual(rgb, amount) == (1 - amount)³ · rgb`
/// exactly (to float epsilon). So for the uniform-dim case this is
/// OUTPUT-IDENTICAL to a naive per-channel linear scale — both preserve hue,
/// because a uniform scale of all channels cannot skew it. The pinning test
/// `grid::tests::closure_sgr_dim_equals_naive_half_brightness` locks this
/// equivalence so the claim cannot drift.
///
/// The perceptual framing is real for the *non-uniform* helpers
/// ([`mix_oklab`] / [`fade`]), which interpolate along an OKLab segment and so
/// genuinely differ from a linear-RGB blend — not for this uniform scale.
pub fn dim_perceptual(rgb: LinearRgb, amount: f32) -> LinearRgb {
    if amount <= 0.0 {
        return rgb;
    }
    let amount = amount.min(1.0);
    let k = 1.0 - amount;
    let lab = linear_to_oklab(rgb);
    oklab_to_linear(Oklab {
        l: lab.l * k,
        a: lab.a * k,
        b: lab.b * k,
    })
}

/// Linear-space interpolation between two colors.
///
/// `t` is clamped to `[0, 1]`; `t = 0` returns `a` exactly and `t = 1` returns
/// `b` exactly. Mixing in linear space is energy-correct (the right model for
/// alpha compositing / coverage blends), but can pass through a desaturated
/// midpoint — use [`mix_oklab`] when perceptual evenness matters more.
pub fn mix_linear(a: LinearRgb, b: LinearRgb, t: f32) -> LinearRgb {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Perceptually-uniform interpolation between two colors through OKLab.
///
/// `t` is clamped to `[0, 1]` with exact endpoints. Equal steps in `t` look
/// like equal perceived steps, so gradients and theme transitions keep an even
/// lightness ramp and don't dip through a muddy grey midpoint. The OKLab
/// lightness of the result is exactly the linear interpolation of the
/// endpoints' lightnesses, so the perceived-lightness ramp is monotonic in `t`.
///
/// **Gamut:** the result is *not* clamped. A straight segment in OKLab maps to a
/// curve in linear RGB, so an intermediate between two in-gamut endpoints can
/// bulge slightly outside the `[0, 1]` cube (empirically within roughly
/// `[-0.09, 1.12]` for saturated primary/secondary pairs). This is intentional:
/// clamping mid-interpolation would flatten the perceptual ramp, and every
/// display path already clamps on output via [`linear_to_srgb_u8`]. Callers that
/// need an in-gamut linear value should clamp the channels themselves.
pub fn mix_oklab(a: LinearRgb, b: LinearRgb, t: f32) -> LinearRgb {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    let la = linear_to_oklab(a);
    let lb = linear_to_oklab(b);
    oklab_to_linear(Oklab {
        l: la.l + (lb.l - la.l) * t,
        a: la.a + (lb.a - la.a) * t,
        b: la.b + (lb.b - la.b) * t,
    })
}

/// Fade `from` toward `to` by `t` through OKLab. Alias of [`mix_oklab`] named
/// for the fade-in / fade-out use site (new output, focus dimming).
pub fn fade(from: LinearRgb, to: LinearRgb, t: f32) -> LinearRgb {
    mix_oklab(from, to, t)
}

// ---------------------------------------------------------------------------
// Minimum-contrast guarantee (RV1)
// ---------------------------------------------------------------------------

/// WCAG relative luminance of a *linear* RGB color (0.0 = black, 1.0 = white).
///
/// This is the standard `0.2126 R + 0.7152 G + 0.0722 B` luminance, evaluated
/// directly on linear channels. Because the render path already holds colors in
/// linear space, computing luminance here is exact and sidesteps the sRGB
/// decode entirely — it matches [`crate::theme::relative_luminance`] (which
/// decodes from bytes first) to within float precision.
pub fn relative_luminance(rgb: LinearRgb) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// WCAG contrast ratio between two linear colors, in `1.0..=21.0`.
///
/// `1.0` means equal luminance; `21.0` is black against white. Symmetric. The
/// luminances are clamped to `[0, 1]` so out-of-gamut intermediates (which the
/// adjustment search can produce) score the same contrast they would once
/// clamped for display.
pub fn wcag_contrast(a: LinearRgb, b: LinearRgb) -> f32 {
    let la = relative_luminance(a).clamp(0.0, 1.0);
    let lb = relative_luminance(b).clamp(0.0, 1.0);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Number of bisection steps used to home in on the minimal lightness move that
/// satisfies the contrast floor. 24 steps resolves OKLab L to < 1e-7 — far finer
/// than the 8-bit output quantum.
const CONTRAST_BISECT_STEPS: u32 = 24;

/// Adjust `fg` so its WCAG contrast against `bg` meets at least `ratio`, moving
/// only OKLab lightness and preserving hue and chroma direction.
///
/// Metric: the WCAG 2.x relative-luminance contrast ratio (the established,
/// user-expected legibility measure) computed via [`wcag_contrast`]. The
/// *adjustment* is perceptual — it walks fg's OKLab L (from RV3) toward black or
/// white, keeping the `a`/`b` opponent values fixed, so the corrected color
/// keeps its hue and only changes how light/dark it is.
///
/// Guarantees:
/// - `ratio <= 1.0` returns `fg` **unchanged, bit-for-bit** (passthrough — the
///   default-setting no-op that keeps the plain path byte-identical).
/// - If `fg`/`bg` already meet the floor, `fg` is returned unchanged.
/// - The search keeps the existing fg-vs-bg polarity (lighter text stays the
///   lighter color) when that direction can satisfy the floor; otherwise it
///   flips to the only feasible direction.
/// - Best-effort cap: if even pure black or pure white cannot reach `ratio`
///   against this `bg` (a near-mid-grey background), the most-contrasting
///   in-gamut endpoint is returned.
/// - Idempotent: a second application is a no-op, because the first result
///   already meets the floor.
pub fn enforce_min_contrast(fg: LinearRgb, bg: LinearRgb, ratio: f32) -> LinearRgb {
    // Passthrough: at or below unity there is no floor to enforce.
    if ratio <= 1.0 {
        return fg;
    }
    if wcag_contrast(fg, bg) >= ratio {
        return fg;
    }

    let lab = linear_to_oklab(fg);
    let lum_fg = relative_luminance(fg).clamp(0.0, 1.0);
    let lum_bg = relative_luminance(bg).clamp(0.0, 1.0);

    // Pick the lightness direction. Default to preserving polarity: if fg is the
    // lighter color, push it lighter (toward L = 1); otherwise push it darker
    // (toward L = 0). If the preferred direction can't reach the floor but the
    // opposite can, use the opposite.
    let lighten_first = lum_fg >= lum_bg;
    let try_dir = |toward_white: bool| -> Option<LinearRgb> {
        let bound_l = if toward_white { 1.0 } else { 0.0 };
        // If even the extreme doesn't meet the floor, this direction fails.
        let extreme = oklab_to_linear(Oklab {
            l: bound_l,
            a: lab.a,
            b: lab.b,
        });
        if wcag_contrast(extreme, bg) < ratio {
            return None;
        }
        // Bisect between the current L and the bound for the smallest move that
        // meets the floor. Contrast is monotonic in L along this direction.
        let mut near = lab.l; // does not (yet) meet the floor
        let mut far = bound_l; // meets the floor
        for _ in 0..CONTRAST_BISECT_STEPS {
            let mid = 0.5 * (near + far);
            let candidate = oklab_to_linear(Oklab {
                l: mid,
                a: lab.a,
                b: lab.b,
            });
            if wcag_contrast(candidate, bg) >= ratio {
                far = mid;
            } else {
                near = mid;
            }
        }
        Some(oklab_to_linear(Oklab {
            l: far,
            a: lab.a,
            b: lab.b,
        }))
    };

    if let Some(adjusted) = try_dir(lighten_first) {
        return adjusted;
    }
    if let Some(adjusted) = try_dir(!lighten_first) {
        return adjusted;
    }

    // Best effort: neither pure black nor pure white meets the floor against this
    // background. Return whichever extreme contrasts most.
    let white = oklab_to_linear(Oklab {
        l: 1.0,
        a: lab.a,
        b: lab.b,
    });
    let black = oklab_to_linear(Oklab {
        l: 0.0,
        a: lab.a,
        b: lab.b,
    });
    if wcag_contrast(white, bg) >= wcag_contrast(black, bg) {
        white
    } else {
        black
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    fn rgb_close(a: LinearRgb, b: LinearRgb, eps: f32) -> bool {
        close(a[0], b[0], eps) && close(a[1], b[1], eps) && close(a[2], b[2], eps)
    }

    #[test]
    fn srgb_transfer_endpoints_and_inverse() {
        assert_eq!(srgb_to_linear_f32(0.0), 0.0);
        assert!(close(srgb_to_linear_f32(1.0), 1.0, 1e-6));
        assert_eq!(linear_to_srgb_f32(0.0), 0.0);
        assert!(close(linear_to_srgb_f32(1.0), 1.0, 1e-6));
        // Round-trip a spread of values through both transfers.
        for i in 0..=20 {
            let c = i as f32 / 20.0;
            assert!(close(linear_to_srgb_f32(srgb_to_linear_f32(c)), c, 1e-5));
        }
    }

    #[test]
    fn byte_path_matches_text_module_formula() {
        // The byte wrapper must equal the historical text::srgb_to_linear math
        // for every byte, since native/grid rely on byte identity.
        for byte in 0u16..=255 {
            let byte = byte as u8;
            let expected = {
                let c = byte as f32 / 255.0;
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            assert_eq!(srgb_to_linear(byte), expected);
        }
    }

    #[test]
    fn linear_to_srgb_u8_round_trips_bytes() {
        for byte in 0u16..=255 {
            let byte = byte as u8;
            assert_eq!(linear_to_srgb_u8(srgb_to_linear(byte)), byte);
        }
    }

    #[test]
    fn oklab_reference_white() {
        // Linear white -> OKLab L=1, a=b=0 (Ottosson reference).
        let lab = linear_to_oklab([1.0, 1.0, 1.0]);
        assert!(close(lab.l, 1.0, 1e-4));
        assert!(close(lab.a, 0.0, 1e-4));
        assert!(close(lab.b, 0.0, 1e-4));
    }

    #[test]
    fn oklab_reference_primaries() {
        // Published OKLab values for sRGB primaries (linear 1,0,0 etc.).
        let red = linear_to_oklab([1.0, 0.0, 0.0]);
        assert!(close(red.l, 0.627_955, 1e-3), "red L {}", red.l);
        assert!(close(red.a, 0.224_863, 1e-3), "red a {}", red.a);
        assert!(close(red.b, 0.125_846, 1e-3), "red b {}", red.b);

        let green = linear_to_oklab([0.0, 1.0, 0.0]);
        assert!(close(green.l, 0.866_440, 1e-3), "green L {}", green.l);

        let blue = linear_to_oklab([0.0, 0.0, 1.0]);
        assert!(close(blue.l, 0.452_014, 1e-3), "blue L {}", blue.l);
    }

    #[test]
    fn oklab_round_trip_is_accurate() {
        let samples = [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.5, 0.25, 0.75],
            [0.1, 0.8, 0.3],
            [0.9, 0.1, 0.05],
        ];
        for s in samples {
            let back = oklab_to_linear(linear_to_oklab(s));
            assert!(rgb_close(s, back, 1e-4), "round-trip {s:?} -> {back:?}");
        }
    }

    #[test]
    fn oklch_round_trip_is_accurate() {
        let lab = linear_to_oklab([0.3, 0.6, 0.2]);
        let back = oklch_to_oklab(oklab_to_oklch(lab));
        assert!(close(lab.l, back.l, 1e-5));
        assert!(close(lab.a, back.a, 1e-5));
        assert!(close(lab.b, back.b, 1e-5));
    }

    #[test]
    fn dim_zero_is_exact_identity() {
        let c = [0.4, 0.55, 0.2];
        assert_eq!(dim_perceptual(c, 0.0), c);
        assert_eq!(dim_perceptual(c, -1.0), c);
    }

    #[test]
    fn dim_one_is_black() {
        let c = dim_perceptual([0.4, 0.55, 0.2], 1.0);
        assert!(rgb_close(c, [0.0, 0.0, 0.0], 1e-4));
    }

    #[test]
    fn dim_reduces_lightness_monotonically() {
        let base = [0.6, 0.4, 0.2];
        let l0 = linear_to_oklab(base).l;
        let l_half = linear_to_oklab(dim_perceptual(base, 0.5)).l;
        let l_more = linear_to_oklab(dim_perceptual(base, 0.8)).l;
        assert!(l_half < l0);
        assert!(l_more < l_half);
        // Lightness scales by (1 - amount).
        assert!(close(l_half, l0 * 0.5, 1e-4));
    }

    #[test]
    fn dim_preserves_hue() {
        let base = [0.7, 0.2, 0.1];
        let h0 = oklab_to_oklch(linear_to_oklab(base)).h;
        let h1 = oklab_to_oklch(linear_to_oklab(dim_perceptual(base, 0.5))).h;
        assert!(close(h0, h1, 1e-3), "hue drift {h0} -> {h1}");
    }

    #[test]
    fn mix_endpoints_are_exact() {
        let a = [0.2, 0.4, 0.6];
        let b = [0.9, 0.1, 0.3];
        assert_eq!(mix_linear(a, b, 0.0), a);
        assert_eq!(mix_linear(a, b, 1.0), b);
        assert_eq!(mix_oklab(a, b, 0.0), a);
        assert_eq!(mix_oklab(a, b, 1.0), b);
        assert_eq!(mix_linear(a, b, -0.5), a);
        assert_eq!(mix_oklab(a, b, 2.0), b);
        assert_eq!(fade(a, b, 0.0), a);
    }

    #[test]
    fn mix_linear_midpoint_is_arithmetic_mean() {
        let a = [0.2, 0.4, 0.6];
        let b = [0.8, 0.6, 0.2];
        let m = mix_linear(a, b, 0.5);
        assert!(rgb_close(m, [0.5, 0.5, 0.4], 1e-6));
    }

    #[test]
    fn mix_oklab_midpoint_has_even_lightness() {
        // OKLab midpoint lightness equals the mean of endpoint lightnesses,
        // unlike a linear-RGB midpoint which skews dark.
        let a = [1.0, 1.0, 1.0];
        let b = [0.0, 0.0, 0.0];
        let m = mix_oklab(a, b, 0.5);
        let lm = linear_to_oklab(m).l;
        assert!(close(lm, 0.5, 1e-4), "oklab mid L {lm}");
        // Linear midpoint of black/white is 0.5 linear, which is L ~ 0.738 —
        // demonstrably lighter, i.e. the two paths genuinely differ.
        let lin_mid_l = linear_to_oklab(mix_linear(a, b, 0.5)).l;
        assert!(lin_mid_l > 0.7, "linear mid L {lin_mid_l}");
    }

    // --- RV3 round-trip accuracy across the gamut -----------------------

    /// A regular grid over the linear-RGB cube plus a few explicit near-black /
    /// near-white extremes, where the cube-root in the OKLab transform has its
    /// steepest slope and is most likely to lose precision. Every sample must
    /// survive `linear -> OKLab -> linear` with tightly bounded error.
    #[test]
    fn oklab_round_trip_bounded_across_gamut_and_extremes() {
        let n = 12;
        let mut max_err = 0f32;
        for i in 0..=n {
            for j in 0..=n {
                for k in 0..=n {
                    let s = [
                        i as f32 / n as f32,
                        j as f32 / n as f32,
                        k as f32 / n as f32,
                    ];
                    let back = oklab_to_linear(linear_to_oklab(s));
                    for c in 0..3 {
                        max_err = max_err.max((s[c] - back[c]).abs());
                    }
                }
            }
        }
        // Cube-root-touchy extremes near both ends of the range.
        for s in [
            [0.001, 0.001, 0.001],
            [0.0005, 0.0, 0.0],
            [0.999, 0.999, 0.999],
            [1.0, 0.5, 0.0001],
            [0.0001, 1.0, 0.0001],
        ] {
            let back = oklab_to_linear(linear_to_oklab(s));
            for c in 0..3 {
                max_err = max_err.max((s[c] - back[c]).abs());
            }
        }
        // Measured worst case ~2e-6; 1e-4 leaves generous margin while still
        // catching any real regression in the matrices or the cube-root path.
        assert!(
            max_err < 1e-4,
            "oklab round-trip error too large: {max_err:e}"
        );
    }

    /// OKLCH is a pure polar relabelling of OKLab's `a`/`b`, so the round-trip
    /// should be near-exact everywhere, including across the `atan2` branch cut
    /// at hue `±π` (negative-`a`, small-`b` colors).
    #[test]
    fn oklch_round_trip_bounded_across_gamut() {
        let n = 12;
        let mut max_err = 0f32;
        for i in 0..=n {
            for j in 0..=n {
                for k in 0..=n {
                    let s = [
                        i as f32 / n as f32,
                        j as f32 / n as f32,
                        k as f32 / n as f32,
                    ];
                    let lab = linear_to_oklab(s);
                    let back = oklch_to_oklab(oklab_to_oklch(lab));
                    max_err = max_err
                        .max((lab.l - back.l).abs())
                        .max((lab.a - back.a).abs())
                        .max((lab.b - back.b).abs());
                }
            }
        }
        // Explicit near-branch-cut colors (cyan-ish: a < 0, b ~ 0) so the hue
        // wrap at ±π is exercised directly.
        for s in [[0.0, 0.4, 0.5], [0.0, 0.5, 0.45], [0.02, 0.3, 0.31]] {
            let lab = linear_to_oklab(s);
            let back = oklch_to_oklab(oklab_to_oklch(lab));
            max_err = max_err
                .max((lab.l - back.l).abs())
                .max((lab.a - back.a).abs())
                .max((lab.b - back.b).abs());
        }
        assert!(
            max_err < 1e-5,
            "oklch round-trip error too large: {max_err:e}"
        );
    }

    // --- RV3 dim_perceptual depth ---------------------------------------

    /// Dimming is monotonic in `amount`: increasing the amount strictly lowers
    /// both the OKLab lightness and the WCAG relative luminance, with no
    /// reversals along the way.
    #[test]
    fn dim_perceptual_is_monotonic_in_amount() {
        let base = [0.6, 0.35, 0.2];
        let mut prev_l = linear_to_oklab(base).l;
        let mut prev_lum = relative_luminance(base);
        for step in 1..=20 {
            let amount = step as f32 / 20.0;
            let dimmed = dim_perceptual(base, amount);
            let l = linear_to_oklab(dimmed).l;
            let lum = relative_luminance(dimmed);
            assert!(
                l < prev_l + 1e-6,
                "L not decreasing at amount={amount}: {l} vs {prev_l}"
            );
            assert!(
                lum < prev_lum + 1e-6,
                "luminance not decreasing at amount={amount}: {lum} vs {prev_lum}"
            );
            prev_l = l;
            prev_lum = lum;
        }
        // Endpoint: amount = 1 lands at black.
        assert!(rgb_close(dim_perceptual(base, 1.0), [0.0, 0.0, 0.0], 1e-4));
    }

    /// Dimming two colors by the same amount preserves their lightness order:
    /// a lighter color stays lighter (the OKLab L scale is a shared positive
    /// factor), so dimming never inverts relative brightness within a frame.
    #[test]
    fn dim_perceptual_preserves_lightness_order() {
        let lighter = [0.8, 0.8, 0.8];
        let darker = [0.3, 0.25, 0.2];
        assert!(linear_to_oklab(lighter).l > linear_to_oklab(darker).l);
        for step in 0..=20 {
            let amount = step as f32 / 20.0;
            let dl = linear_to_oklab(dim_perceptual(lighter, amount)).l;
            let dd = linear_to_oklab(dim_perceptual(darker, amount)).l;
            // Order is preserved (and only ties exactly at amount = 1, both 0).
            assert!(
                dl + 1e-6 >= dd,
                "order inverted at amount={amount}: {dl} < {dd}"
            );
        }
    }

    /// Dimming preserves hue across the useful range of amounts for several
    /// distinct hues. `a` and `b` are scaled by the same positive factor, so
    /// `atan2(b, a)` is invariant; chroma collapses only as `amount -> 1`, so the
    /// sweep stops short of the degenerate black endpoint where hue is undefined.
    #[test]
    fn dim_perceptual_preserves_hue_across_amounts_and_hues() {
        let hues = [
            [0.7, 0.2, 0.1],  // red-ish
            [0.2, 0.6, 0.15], // green-ish
            [0.15, 0.3, 0.7], // blue-ish
            [0.6, 0.55, 0.1], // yellow-ish
        ];
        for base in hues {
            let h0 = oklab_to_oklch(linear_to_oklab(base)).h;
            for step in 1..=18 {
                let amount = step as f32 / 20.0; // up to 0.9, well clear of black
                let h = oklab_to_oklch(linear_to_oklab(dim_perceptual(base, amount))).h;
                let mut dh = (h - h0).abs();
                if dh > std::f32::consts::PI {
                    dh = std::f32::consts::TAU - dh;
                }
                assert!(dh < 1e-3, "hue drift {dh} for {base:?} at amount={amount}");
            }
        }
    }

    // --- RV3 blend monotonicity + gamut behavior ------------------------

    /// `mix_oklab` ramps OKLab lightness monotonically in `t` between the
    /// endpoints — the core perceptual-evenness guarantee documented on the fn.
    #[test]
    fn mix_oklab_lightness_is_monotonic_in_t() {
        let pairs = [
            ([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
            ([0.9, 0.1, 0.05], [0.05, 0.1, 0.9]),
            ([0.2, 0.6, 0.15], [0.6, 0.55, 0.1]),
        ];
        for (a, b) in pairs {
            let la = linear_to_oklab(a).l;
            let lb = linear_to_oklab(b).l;
            let ascending = lb >= la;
            let mut prev = linear_to_oklab(mix_oklab(a, b, 0.0)).l;
            for step in 1..=20 {
                let t = step as f32 / 20.0;
                let l = linear_to_oklab(mix_oklab(a, b, t)).l;
                if ascending {
                    assert!(l + 1e-5 >= prev, "L dipped at t={t}: {l} < {prev}");
                } else {
                    assert!(l <= prev + 1e-5, "L rose at t={t}: {l} > {prev}");
                }
                prev = l;
            }
        }
    }

    /// Regression guard for the documented gamut behavior: `mix_oklab` may bulge
    /// out of the `[0, 1]` cube between in-gamut endpoints, but the excursion is
    /// bounded. If a change blew the perceptual path wide open (or, conversely,
    /// silently introduced a clamp), this catches it. The display path clamps on
    /// output, so the bound here is about *intermediate* sanity, not display.
    #[test]
    fn mix_oklab_gamut_excursion_is_bounded() {
        let corners = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
        ];
        let (mut lo, mut hi) = (0f32, 1f32);
        for a in corners {
            for b in corners {
                for step in 0..=20 {
                    let m = mix_oklab(a, b, step as f32 / 20.0);
                    for c in 0..3 {
                        lo = lo.min(m[c]);
                        hi = hi.max(m[c]);
                    }
                }
            }
        }
        // Measured worst case ~[-0.083, 1.110]; pad the asserted envelope a touch.
        assert!(lo > -0.15, "negative excursion too large: {lo:e}");
        assert!(hi < 1.15, "positive excursion too large: {hi:e}");
    }

    /// `mix_linear` is a straight per-channel lerp, so every channel is
    /// monotonic in `t` and never leaves the `[0, 1]` cube for in-gamut
    /// endpoints (the convex-combination property the OKLab path lacks).
    #[test]
    fn mix_linear_is_per_channel_monotonic_and_in_gamut() {
        let a = [0.1, 0.8, 0.3];
        let b = [0.9, 0.2, 0.6];
        let mut prev = mix_linear(a, b, 0.0);
        for step in 1..=20 {
            let t = step as f32 / 20.0;
            let m = mix_linear(a, b, t);
            for c in 0..3 {
                // a[c] < b[c] for c=0,2 (ascending) and a[1] > b[1] (descending).
                if b[c] >= a[c] {
                    assert!(m[c] + 1e-6 >= prev[c], "ch{c} dipped at t={t}");
                } else {
                    assert!(m[c] <= prev[c] + 1e-6, "ch{c} rose at t={t}");
                }
                assert!((0.0..=1.0).contains(&m[c]), "ch{c} left gamut: {}", m[c]);
            }
            prev = m;
        }
    }

    /// `fade` is documented as an alias of `mix_oklab`; pin that contract so a
    /// future refactor can't silently diverge the fade-in/out path from blends.
    #[test]
    fn fade_is_mix_oklab_alias() {
        let a = [0.3, 0.6, 0.2];
        let b = [0.8, 0.2, 0.5];
        for step in 0..=10 {
            let t = step as f32 / 10.0;
            assert_eq!(fade(a, b, t), mix_oklab(a, b, t));
        }
    }

    // --- RV1 minimum-contrast guarantee ---------------------------------

    fn srgb_byte_to_linear(c: u8) -> f32 {
        srgb_to_linear(c)
    }

    fn linear_of(r: u8, g: u8, b: u8) -> LinearRgb {
        [
            srgb_byte_to_linear(r),
            srgb_byte_to_linear(g),
            srgb_byte_to_linear(b),
        ]
    }

    #[test]
    fn contrast_matches_wcag_reference() {
        // Black on white is the 21:1 maximum; identical colors are 1:1.
        let black = [0.0, 0.0, 0.0];
        let white = [1.0, 1.0, 1.0];
        assert!(close(wcag_contrast(black, white), 21.0, 0.01));
        assert!(close(wcag_contrast(white, black), 21.0, 0.01));
        let grey = linear_of(0x33, 0x66, 0x99);
        assert!(close(wcag_contrast(grey, grey), 1.0, 1e-6));
    }

    #[test]
    fn contrast_agrees_with_theme_helper() {
        // The linear-domain metric must match the byte-domain theme helper
        // (which TH3 uses to validate themes) to within float precision.
        let pairs = [
            ((0x00, 0x00, 0x00), (0xff, 0xff, 0xff)),
            ((0x80, 0x80, 0x80), (0x00, 0x00, 0x00)),
            ((0x1d, 0x20, 0x21), (0xd4, 0xbe, 0x98)),
        ];
        for (a, b) in pairs {
            let mine = wcag_contrast(linear_of(a.0, a.1, a.2), linear_of(b.0, b.1, b.2));
            let theirs = crate::theme::contrast_ratio(a, b) as f32;
            assert!(close(mine, theirs, 1e-3), "{mine} vs {theirs}");
        }
    }

    #[test]
    fn min_contrast_ratio_at_or_below_one_is_exact_identity() {
        let fg = [0.30, 0.31, 0.30];
        let bg = [0.25, 0.26, 0.25];
        assert_eq!(enforce_min_contrast(fg, bg, 1.0), fg);
        assert_eq!(enforce_min_contrast(fg, bg, 0.5), fg);
        assert_eq!(enforce_min_contrast(fg, bg, -3.0), fg);
    }

    #[test]
    fn min_contrast_leaves_already_legible_pair_untouched() {
        // Black on white already far exceeds any sane floor.
        let fg = [0.0, 0.0, 0.0];
        let bg = [1.0, 1.0, 1.0];
        assert_eq!(enforce_min_contrast(fg, bg, 4.5), fg);
    }

    #[test]
    fn min_contrast_lifts_low_contrast_pair_to_floor() {
        // A dim grey on a slightly darker grey — illegibly low contrast.
        let fg = linear_of(0x55, 0x55, 0x55);
        let bg = linear_of(0x44, 0x44, 0x44);
        let ratio = 4.5;
        assert!(wcag_contrast(fg, bg) < ratio, "precondition: starts low");
        let adj = enforce_min_contrast(fg, bg, ratio);
        let after = wcag_contrast(adj, bg);
        // Meets the floor (small epsilon for the bisection residual).
        assert!(after >= ratio - 1e-3, "after={after} < {ratio}");
        // And does not massively overshoot — the minimal move lands near the
        // floor, not pinned to an extreme.
        assert!(after <= ratio + 0.5, "overshoot after={after}");
    }

    #[test]
    fn min_contrast_preserves_hue() {
        // A muted red on dark grey; after lifting it should still read as red
        // (hue in OKLCH roughly unchanged), only lighter.
        let fg = linear_of(0x6a, 0x30, 0x30);
        let bg = linear_of(0x20, 0x20, 0x20);
        let h0 = oklab_to_oklch(linear_to_oklab(fg)).h;
        let adj = enforce_min_contrast(fg, bg, 7.0);
        let h1 = oklab_to_oklch(linear_to_oklab(adj)).h;
        assert!(close(h0, h1, 0.02), "hue drift {h0} -> {h1}");
    }

    #[test]
    fn min_contrast_is_idempotent() {
        let fg = linear_of(0x55, 0x55, 0x55);
        let bg = linear_of(0x44, 0x44, 0x44);
        let ratio = 4.5;
        let once = enforce_min_contrast(fg, bg, ratio);
        let twice = enforce_min_contrast(once, bg, ratio);
        assert!(rgb_close(once, twice, 1e-6), "{once:?} vs {twice:?}");
    }

    #[test]
    fn min_contrast_keeps_polarity_dark_text_on_light_bg() {
        // Dark-ish text on a light bg, low contrast: it should get darker (stay
        // the darker color), not flip to white.
        let fg = linear_of(0x99, 0x99, 0x99);
        let bg = linear_of(0xcc, 0xcc, 0xcc);
        let adj = enforce_min_contrast(fg, bg, 4.5);
        assert!(
            relative_luminance(adj) < relative_luminance(bg),
            "fg should stay darker than bg"
        );
        assert!(wcag_contrast(adj, bg) >= 4.5 - 1e-3);
    }

    #[test]
    fn min_contrast_best_effort_against_mid_grey() {
        // No color can reach 21:1 against mid-grey; the function returns the
        // most-contrasting extreme rather than looping forever or panicking.
        let fg = linear_of(0x70, 0x70, 0x70);
        let bg = linear_of(0x7f, 0x7f, 0x7f);
        let adj = enforce_min_contrast(fg, bg, 21.0);
        let c = wcag_contrast(adj, bg);
        // It at least improved over the near-zero starting contrast.
        assert!(c > wcag_contrast(fg, bg));
        assert!(c.is_finite());
    }
}

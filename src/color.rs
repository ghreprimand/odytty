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

/// Perceptually dim a linear color by scaling it toward black in OKLab.
///
/// `amount` is in `[0, 1]`: `0.0` returns the input unchanged (exact identity,
/// short-circuited before any round-trip so there is zero float drift), `1.0`
/// drives the color to black. Lightness and chroma are scaled by the same
/// `1 - amount` factor, so dimming reduces both perceived brightness and
/// saturation together (as dimmer light naturally does) while preserving hue —
/// unlike an independent per-channel linear scale, which can skew hue.
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
/// lightness ramp and don't dip through a muddy grey midpoint.
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
}

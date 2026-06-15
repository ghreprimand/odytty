// SPDX-License-Identifier: GPL-3.0-only
//! Byte-domain contrast readouts, unified with the render-time readability floor.
//!
//! OdyTTY has **one** contrast metric: the WCAG 2.x relative-luminance contrast
//! ratio implemented in [`crate::color`], which is exactly what the RV1/U1
//! readability floor ([`crate::color::enforce_min_contrast`]) enforces at render
//! time. This module is a thin **byte-domain adapter** over that metric: it
//! decodes 8-bit sRGB triples ([`Srgb`]) to scene-linear RGB with the canonical
//! transfer ([`crate::color::srgb_to_linear`]) and then delegates to the same
//! [`crate::color::relative_luminance`] / [`crate::color::wcag_contrast`] the
//! renderer uses.
//!
//! Keeping a single metric is the load-bearing invariant behind the theme
//! builder: a contrast number **shown** to the user (theme validation in TH3,
//! the theme-builder readout) equals the contrast the renderer **enforces**, so
//! an authored `.theme` and its rendered pixels agree. There is deliberately no
//! second "display" formula here — that divergence (an 8-bit luminance path that
//! used the older `0.03928` sRGB knee and `f64` math) has been retired in favor
//! of the canonical linear primitives.
//!
//! The byte form ([`Srgb`]) and `f64` return type are retained so existing
//! callers (theme validation, the builder readout, the bloom-threshold seed) are
//! unchanged; the values they receive now match the render floor to float
//! precision.

use super::Srgb;
use crate::color;

/// Decode an 8-bit sRGB triple to scene-linear RGB via the canonical transfer
/// ([`crate::color::srgb_to_linear`]) — the same decode the render path uses, so
/// downstream luminance/contrast match the floor exactly.
fn to_linear((r, g, b): Srgb) -> color::LinearRgb {
    [
        color::srgb_to_linear(r),
        color::srgb_to_linear(g),
        color::srgb_to_linear(b),
    ]
}

/// WCAG relative luminance of an sRGB color (0.0 = black, 1.0 = white).
///
/// Byte-domain wrapper over [`crate::color::relative_luminance`]: decodes to
/// linear with the canonical sRGB transfer, then evaluates the identical
/// luminance the render floor uses. Returned as `f64` for caller compatibility;
/// the underlying computation is the canonical `f32` metric.
pub fn relative_luminance(rgb: Srgb) -> f64 {
    f64::from(color::relative_luminance(to_linear(rgb)))
}

/// WCAG contrast ratio between two sRGB colors, in the range `1.0..=21.0`.
///
/// `1.0` means the two colors are identical; `21.0` is pure black against pure
/// white. The ratio is symmetric in its arguments. Byte-domain wrapper over
/// [`crate::color::wcag_contrast`] — the single contrast metric the render floor
/// enforces — so a shown contrast equals the enforced contrast.
pub fn contrast_ratio(a: Srgb, b: Srgb) -> f64 {
    f64::from(color::wcag_contrast(to_linear(a), to_linear(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_on_white_is_maximal() {
        let ratio = contrast_ratio((0, 0, 0), (255, 255, 255));
        assert!((ratio - 21.0).abs() < 0.01, "ratio={ratio}");
    }

    #[test]
    fn identical_colors_are_unity() {
        assert!((contrast_ratio((0x33, 0x66, 0x99), (0x33, 0x66, 0x99)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ratio_is_symmetric() {
        let a = (0x12, 0x34, 0x56);
        let b = (0xab, 0xcd, 0xef);
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-9);
    }

    #[test]
    fn shown_contrast_equals_enforced_metric() {
        // The load-bearing invariant for U2: the byte-domain readout this module
        // produces is the *same number* the render floor's metric
        // (color::wcag_contrast on linear inputs) computes for the same colors.
        // A divergent display metric would let the builder show a contrast the
        // renderer does not actually deliver.
        let pairs = [
            ((0x00u8, 0x00u8, 0x00u8), (0xffu8, 0xffu8, 0xffu8)),
            ((0x1d, 0x20, 0x21), (0xd4, 0xbe, 0x98)), // gruvbox-ish dark
            ((0x80, 0x80, 0x80), (0x00, 0x00, 0x00)),
            ((0x55, 0x55, 0x55), (0x44, 0x44, 0x44)), // low contrast
            ((0xfa, 0xfa, 0xfa), (0x2e, 0x34, 0x40)),
        ];
        for (a, b) in pairs {
            let shown = contrast_ratio(a, b);
            let enforced = f64::from(color::wcag_contrast(
                [
                    color::srgb_to_linear(a.0),
                    color::srgb_to_linear(a.1),
                    color::srgb_to_linear(a.2),
                ],
                [
                    color::srgb_to_linear(b.0),
                    color::srgb_to_linear(b.1),
                    color::srgb_to_linear(b.2),
                ],
            ));
            assert_eq!(shown, enforced, "shown {shown} != enforced {enforced}");
        }
    }

    #[test]
    fn shown_luminance_equals_enforced_metric() {
        for rgb in [
            (0u8, 0u8, 0u8),
            (255, 255, 255),
            (0x1d, 0x20, 0x21),
            (0xd4, 0xbe, 0x98),
        ] {
            let shown = relative_luminance(rgb);
            let enforced = f64::from(color::relative_luminance([
                color::srgb_to_linear(rgb.0),
                color::srgb_to_linear(rgb.1),
                color::srgb_to_linear(rgb.2),
            ]));
            assert_eq!(shown, enforced);
        }
    }
}

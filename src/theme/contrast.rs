// SPDX-License-Identifier: GPL-3.0-only
//! Perceptual contrast helpers (WCAG 2.x relative luminance + contrast ratio).
//!
//! This is the first slice of the readability floor that [RV1] (minimum-contrast
//! guarantee) will build on: TH3 uses [`contrast_ratio`] to validate that every
//! built-in theme clears a documented minimum default fg/bg contrast, and RV1
//! will reuse the same math to enforce a configurable floor at render time.
//!
//! The formula is the standard sRGB → linear relative-luminance contrast ratio
//! defined by WCAG. It is luminance-based (not a full OKLab perceptual model),
//! which is the established, widely-understood baseline for legibility checks;
//! the OKLab pipeline (RV3) refines blending, not this legibility gate.
//!
//! [RV1]: minimum-contrast guarantee (see the build plan, Epic C Tier 1)

use super::Srgb;

/// Linearize a single 0–255 sRGB channel to 0.0–1.0 linear light, per the WCAG
/// transfer function.
fn linearize(channel: u8) -> f64 {
    let c = channel as f64 / 255.0;
    if c <= 0.03928 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance of an sRGB color (0.0 = black, 1.0 = white).
pub fn relative_luminance((r, g, b): Srgb) -> f64 {
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

/// WCAG contrast ratio between two sRGB colors, in the range `1.0..=21.0`.
///
/// `1.0` means the two colors are identical; `21.0` is pure black against pure
/// white. The ratio is symmetric in its arguments.
pub fn contrast_ratio(a: Srgb, b: Srgb) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
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
        assert!((contrast_ratio((0x33, 0x66, 0x99), (0x33, 0x66, 0x99)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ratio_is_symmetric() {
        let a = (0x12, 0x34, 0x56);
        let b = (0xab, 0xcd, 0xef);
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < 1e-12);
    }
}

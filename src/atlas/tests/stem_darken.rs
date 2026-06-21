// SPDX-License-Identifier: GPL-3.0-only
//! Stem-darkening (RV5) coverage-boost tests.
//!
//! The boost is applied at raster time in [`apply_stem_darken`]; these tests
//! pin the pixel-identity guarantee (strength `0.0` is a true no-op, endpoints
//! always exact) and the monotonic darkening behavior, plus the global
//! [`set_stem_darken`] seam's clamping. The pure-function tests touch no
//! process-global state, so they cannot perturb sibling atlas builds.

use super::*;

#[test]
fn strength_zero_is_identity_for_every_coverage_value() {
    // The escape hatch: a disabled (or absent) setting reproduces the
    // historical atlas byte-for-byte.
    for v in 0u8..=255 {
        assert_eq!(apply_stem_darken(v, 0.0), v, "value {v} must pass through");
    }
    // Negative / non-positive strengths are also identity.
    for v in [0u8, 1, 64, 128, 200, 254, 255] {
        assert_eq!(apply_stem_darken(v, -0.5), v);
    }
}

#[test]
fn endpoints_are_always_exact() {
    // Fully-uncovered and fully-covered samples never move, at any strength, so
    // glyph ink bounds and solid interiors are unchanged — only anti-aliased
    // edges and thin stems are boosted.
    for s in [0.0, 0.25, 0.5, 0.75, 1.0] {
        assert_eq!(apply_stem_darken(0, s), 0, "uncovered at strength {s}");
        assert_eq!(apply_stem_darken(255, s), 255, "covered at strength {s}");
    }
}

#[test]
fn partial_coverage_is_boosted_and_never_reduced() {
    // For any positive strength, intermediate coverage rises toward full
    // (irradiation compensation), and never below the input.
    for s in [0.25, 0.5, 1.0] {
        for v in 1u8..=254 {
            let out = apply_stem_darken(v, s);
            assert!(
                out >= v,
                "strength {s}: coverage {v} -> {out} must not decrease"
            );
        }
    }
    // A mid-tone edge sample is visibly thickened at full strength.
    let boosted = apply_stem_darken(128, 1.0);
    assert!(boosted > 140, "midtone boost too weak: 128 -> {boosted}");
}

#[test]
fn boost_is_monotonic_in_strength() {
    // Stronger settings darken at least as much as weaker ones.
    let v = 96u8;
    let a = apply_stem_darken(v, 0.25);
    let b = apply_stem_darken(v, 0.5);
    let c = apply_stem_darken(v, 1.0);
    assert!(a <= b && b <= c, "monotonic: {a} <= {b} <= {c}");
    assert!(a >= v);
}

#[test]
fn set_stem_darken_round_trips_and_clamps() {
    // Mutates the process-global stem-darken gain; serialize against any test
    // that builds a real atlas (which reads the global) via the shared lock.
    let _guard = crate::test_lock::render_globals_lock();
    // Minimal global-seam check: set, read back, restore — no atlas build
    // between set and restore, so the non-zero window cannot affect other
    // tests' presence/relative coverage assertions.
    let restore = stem_darken_strength();

    set_stem_darken(0.5);
    assert_eq!(stem_darken_strength(), 0.5);

    // Clamp above 1.0 and below 0.0.
    set_stem_darken(4.0);
    assert_eq!(stem_darken_strength(), 1.0);
    set_stem_darken(-2.0);
    assert_eq!(stem_darken_strength(), 0.0);

    // Non-finite values fall back to the disabled state.
    set_stem_darken(f32::NAN);
    assert_eq!(stem_darken_strength(), 0.0);
    set_stem_darken(f32::INFINITY);
    assert_eq!(stem_darken_strength(), 0.0);

    // Restore whatever was active before (the default is 0.0).
    set_stem_darken(restore);
    assert_eq!(stem_darken_strength(), restore);
}

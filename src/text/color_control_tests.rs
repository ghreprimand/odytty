// SPDX-License-Identifier: GPL-3.0-only
//! Invalid presentation controls preserve finite input colors.

use super::*;

#[test]
fn non_finite_contrast_controls_use_passthrough() {
    let _guard = crate::test_lock::render_globals_lock();
    let fg = [0.1, 0.2, 0.3, 0.5];
    let bg = [0.04, 0.05, 0.06, 1.0];
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        set_min_contrast(value);
        assert_eq!(min_contrast(), 1.0);
        assert_eq!(enforce_contrast_rgba(fg, bg), fg);
    }
}

#[test]
fn non_finite_brightness_controls_use_passthrough() {
    let color = [0.1, 0.2, 0.3, 0.5];
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(lift_brightness_rgba(color, value), color);
    }
}

#[test]
fn non_finite_direct_contrast_targets_use_passthrough() {
    let fg = [0.1, 0.2, 0.3];
    let bg = [0.04, 0.05, 0.06];
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(crate::color::enforce_min_contrast(fg, bg, value), fg);
    }
}

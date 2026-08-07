// SPDX-License-Identifier: GPL-3.0-only
//! Hit-test tests for the overlay pointer paths.

use super::*;

#[test]
fn point_inside_fit_rect_is_not_outside() {
    // Centered fit-rect within a synthetic 1000x800 viewport.
    let rect = [200.0_f32, 150.0, 800.0, 650.0];
    // Dead-center → inside.
    assert!(!point_outside_rect(500.0, 400.0, rect));
    // Just inside each edge.
    assert!(!point_outside_rect(201.0, 400.0, rect));
    assert!(!point_outside_rect(799.0, 400.0, rect));
    assert!(!point_outside_rect(500.0, 151.0, rect));
    assert!(!point_outside_rect(500.0, 649.0, rect));
}

#[test]
fn point_past_each_edge_is_outside() {
    let rect = [200.0_f32, 150.0, 800.0, 650.0];
    // Left of x0.
    assert!(point_outside_rect(199.0, 400.0, rect));
    // Right of x1.
    assert!(point_outside_rect(801.0, 400.0, rect));
    // Above y0.
    assert!(point_outside_rect(500.0, 149.0, rect));
    // Below y1.
    assert!(point_outside_rect(500.0, 651.0, rect));
    // A corner well outside (both axes beyond).
    assert!(point_outside_rect(0.0, 0.0, rect));
}

#[test]
fn point_on_fit_rect_border_is_inclusive_inside() {
    // Boundary convention: a point exactly on an edge counts as ON the image
    // (inside) → inert, never a dismiss.
    let rect = [200.0_f32, 150.0, 800.0, 650.0];
    assert!(!point_outside_rect(200.0, 400.0, rect)); // on x0
    assert!(!point_outside_rect(800.0, 400.0, rect)); // on x1
    assert!(!point_outside_rect(500.0, 150.0, rect)); // on y0
    assert!(!point_outside_rect(500.0, 650.0, rect)); // on y1
    assert!(!point_outside_rect(200.0, 150.0, rect)); // exact corner
}

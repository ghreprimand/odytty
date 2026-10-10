// SPDX-License-Identifier: GPL-3.0-only
//! Occlusion cuts: whatever a hole covers must not survive in the kept quad.

use super::clipping::{OcclusionCut, occlusion_cut};

const RECT: [f32; 4] = [0.0, 0.0, 10.0, 10.0];

fn area(rect: [f32; 4]) -> f32 {
    (rect[2] - rect[0]).max(0.0) * (rect[3] - rect[1]).max(0.0)
}

fn intersects(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0].max(b[0]) < a[2].min(b[2]) && a[1].max(b[1]) < a[3].min(b[3])
}

#[test]
fn a_hole_inside_the_quad_leaves_the_largest_strip_around_it() {
    // Strips around [3,3,5,5]: left 30, right 50, top 30, bottom 50. The right
    // strip comes first among equals, so it wins the tie with the bottom one.
    assert_eq!(
        occlusion_cut(RECT, [3.0, 3.0, 5.0, 5.0]),
        OcclusionCut::Crop([5.0, 0.0, 10.0, 10.0])
    );
    // Off-centre hole: the bottom strip (60) beats left (20), right (50).
    assert_eq!(
        occlusion_cut(RECT, [2.0, 2.0, 5.0, 4.0]),
        OcclusionCut::Crop([0.0, 4.0, 10.0, 10.0])
    );
}

#[test]
fn a_hole_touching_one_edge_in_its_middle_is_cut_out() {
    // Left edge, vertically interior: the right strip keeps 60.
    assert_eq!(
        occlusion_cut(RECT, [-2.0, 3.0, 4.0, 5.0]),
        OcclusionCut::Crop([4.0, 0.0, 10.0, 10.0])
    );
    // Top edge, horizontally interior: the bottom strip keeps 60.
    assert_eq!(
        occlusion_cut(RECT, [3.0, -5.0, 5.0, 4.0]),
        OcclusionCut::Crop([0.0, 4.0, 10.0, 10.0])
    );
    // Right and bottom edges mirror the same rule.
    assert_eq!(
        occlusion_cut(RECT, [6.0, 3.0, 14.0, 5.0]),
        OcclusionCut::Crop([0.0, 0.0, 6.0, 10.0])
    );
    assert_eq!(
        occlusion_cut(RECT, [3.0, 6.0, 5.0, 15.0]),
        OcclusionCut::Crop([0.0, 0.0, 10.0, 6.0])
    );
}

#[test]
fn every_overlapping_hole_leaves_a_rectangle_that_avoids_it() {
    // Deterministic lattice sweep over all hole placements, including corner,
    // edge, interior and full-span cases.
    let steps = [-2.0f32, 0.0, 1.0, 3.0, 5.0, 7.0, 10.0, 12.0];
    for &x0 in &steps {
        for &x1 in &steps {
            for &y0 in &steps {
                for &y1 in &steps {
                    let hole = [x0, y0, x1, y1];
                    let overlaps = intersects(RECT, hole);
                    match occlusion_cut(RECT, hole) {
                        OcclusionCut::Keep => assert!(!overlaps, "{hole:?} was kept"),
                        OcclusionCut::Collapse => {
                            assert!(
                                x0 <= 0.0 && y0 <= 0.0 && x1 >= 10.0 && y1 >= 10.0,
                                "{hole:?} collapsed a quad it does not cover"
                            );
                        }
                        OcclusionCut::Crop(kept) => {
                            assert!(overlaps, "{hole:?} cropped an untouched quad");
                            assert!(area(kept) > 0.0, "{hole:?} kept nothing");
                            assert!(!intersects(kept, hole), "{kept:?} still meets {hole:?}");
                            assert!(
                                kept[0] >= RECT[0]
                                    && kept[1] >= RECT[1]
                                    && kept[2] <= RECT[2]
                                    && kept[3] <= RECT[3],
                                "{kept:?} leaves the quad"
                            );
                        }
                    }
                }
            }
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! General antialiased polygon filling for geometric glyphs.
//!
//! Coverage is the exact area of the polygon inside each pixel square, found
//! by clipping the polygon to that square and taking the shoelace area. Any
//! simple polygon works, convex or concave, in either winding. Several
//! polygons accumulate into one [`PolygonCoverage`] and their union is clamped
//! to full coverage, so overlapping strokes never exceed one fully inked pixel.
//!
//! Inputs are cell-geometry coordinates derived from the cell's pixel size,
//! never terminal input, and work is limited to each polygon's bounding box.

use super::Canvas;

/// A point in cell pixel space, origin top-left, y down.
pub(super) type Point = (f32, f32);

/// Per-pixel fractional area accumulated from one or more polygons.
pub(super) struct PolygonCoverage {
    w: u32,
    h: u32,
    area: Vec<f32>,
    scratch: Vec<Point>,
    clipped: Vec<Point>,
}

impl PolygonCoverage {
    pub(super) fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            area: vec![0.0; (w as usize).saturating_mul(h as usize)],
            scratch: Vec::new(),
            clipped: Vec::new(),
        }
    }

    /// Add the exact per-pixel area of `poly`. Fewer than three points add
    /// nothing; non-finite points make the polygon a no-op.
    pub(super) fn add(&mut self, poly: &[Point]) {
        if poly.len() < 3 || poly.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in poly {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let xs = (x0.floor().max(0.0) as u32).min(self.w);
        let xe = (x1.ceil().max(0.0) as u32).min(self.w);
        let ys = (y0.floor().max(0.0) as u32).min(self.h);
        let ye = (y1.ceil().max(0.0) as u32).min(self.h);
        for y in ys..ye {
            for x in xs..xe {
                let a = self.pixel_area(poly, x as f32, y as f32);
                if a > 0.0 {
                    self.area[(y * self.w + x) as usize] += a;
                }
            }
        }
    }

    /// Area of `poly` inside the unit square at `(px, py)`.
    fn pixel_area(&mut self, poly: &[Point], px: f32, py: f32) -> f32 {
        self.clipped.clear();
        self.clipped.extend_from_slice(poly);
        // Keep the side of each square edge where the square's interior lies.
        let edges = [
            ((px, py), (px + 1.0, py)),
            ((px + 1.0, py), (px + 1.0, py + 1.0)),
            ((px + 1.0, py + 1.0), (px, py + 1.0)),
            ((px, py + 1.0), (px, py)),
        ];
        let inside = (px + 0.5, py + 0.5);
        for (a, b) in edges {
            clip_half_plane(&self.clipped, a, b, inside, &mut self.scratch);
            std::mem::swap(&mut self.clipped, &mut self.scratch);
            if self.clipped.len() < 3 {
                return 0.0;
            }
        }
        shoelace(&self.clipped).abs().min(1.0)
    }

    /// Write the clamped union into `canvas` (max-combine). With `invert`, the
    /// polygons are holes cut from a fully inked cell.
    pub(super) fn write(&self, canvas: &mut Canvas, invert: bool) {
        for y in 0..self.h {
            for x in 0..self.w {
                let a = self.area[(y * self.w + x) as usize].clamp(0.0, 1.0);
                let a = if invert { 1.0 - a } else { a };
                let v = (a * 255.0).round() as u8;
                if v > 0 {
                    canvas.put(x as i32, y as i32, v);
                }
            }
        }
    }
}

/// Clip `poly` to the closed half-plane bounded by the line `a`-`b` that
/// contains `keep`, writing the result into `out` (Sutherland-Hodgman).
pub(super) fn clip_half_plane(
    poly: &[Point],
    a: Point,
    b: Point,
    keep: Point,
    out: &mut Vec<Point>,
) {
    out.clear();
    let side = |p: Point| (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1);
    let sign = if side(keep) < 0.0 { -1.0 } else { 1.0 };
    let dist = |p: Point| side(p) * sign;
    let Some(&last) = poly.last() else {
        return;
    };
    let mut prev = last;
    let mut prev_d = dist(prev);
    for &cur in poly {
        let cur_d = dist(cur);
        if cur_d >= 0.0 {
            if prev_d < 0.0 {
                out.push(intersect(prev, cur, prev_d, cur_d));
            }
            out.push(cur);
        } else if prev_d >= 0.0 {
            out.push(intersect(prev, cur, prev_d, cur_d));
        }
        prev = cur;
        prev_d = cur_d;
    }
}

fn intersect(p: Point, q: Point, dp: f32, dq: f32) -> Point {
    let t = dp / (dp - dq);
    (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t)
}

/// Signed shoelace area.
fn shoelace(poly: &[Point]) -> f32 {
    let mut twice = 0.0f32;
    for (i, &(x0, y0)) in poly.iter().enumerate() {
        let (x1, y1) = poly[(i + 1) % poly.len()];
        twice += x0 * y1 - x1 * y0;
    }
    twice / 2.0
}

/// A stroke of width `thickness` along `a`-`b`, extended by `extend` past both
/// ends so strokes that end on a cell edge reach it without a notch.
pub(super) fn stroke(a: Point, b: Point, thickness: f32, extend: f32) -> [Point; 4] {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return [a; 4];
    }
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy * thickness / 2.0, ux * thickness / 2.0);
    let a = (a.0 - ux * extend, a.1 - uy * extend);
    let b = (b.0 + ux * extend, b.1 + uy * extend);
    [
        (a.0 + nx, a.1 + ny),
        (b.0 + nx, b.1 + ny),
        (b.0 - nx, b.1 - ny),
        (a.0 - nx, a.1 - ny),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total(cov: &PolygonCoverage) -> f32 {
        cov.area.iter().sum()
    }

    #[test]
    fn exact_area_for_convex_concave_and_reversed_polygons() {
        let mut cov = PolygonCoverage::new(8, 8);
        cov.add(&[(1.25, 1.5), (6.5, 1.5), (6.5, 5.75), (1.25, 5.75)]);
        assert!((total(&cov) - 5.25 * 4.25).abs() < 1e-3);

        // Concave L shape: area 4 + 3 = 7.
        let mut cov = PolygonCoverage::new(8, 8);
        cov.add(&[
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 1.0),
            (1.0, 1.0),
            (1.0, 4.0),
            (0.0, 4.0),
        ]);
        assert!((total(&cov) - 7.0).abs() < 1e-3);
        let mut rev = PolygonCoverage::new(8, 8);
        rev.add(&[
            (0.0, 4.0),
            (1.0, 4.0),
            (1.0, 1.0),
            (4.0, 1.0),
            (4.0, 0.0),
            (0.0, 0.0),
        ]);
        assert_eq!(cov.area, rev.area);

        // Triangle across pixel boundaries.
        let mut cov = PolygonCoverage::new(8, 8);
        cov.add(&[(0.0, 0.0), (7.0, 0.0), (0.0, 5.0)]);
        assert!((total(&cov) - 17.5).abs() < 1e-3);
    }

    #[test]
    fn polygons_are_clipped_to_the_canvas_and_degenerate_input_is_ignored() {
        let mut cov = PolygonCoverage::new(4, 4);
        cov.add(&[(-10.0, -10.0), (20.0, -10.0), (20.0, 20.0), (-10.0, 20.0)]);
        assert!(cov.area.iter().all(|&a| (a - 1.0).abs() < 1e-6));
        let mut cov = PolygonCoverage::new(4, 4);
        cov.add(&[(0.0, 0.0), (1.0, 1.0)]);
        cov.add(&[(0.0, 0.0), (f32::NAN, 1.0), (2.0, 2.0)]);
        cov.add(&stroke((1.0, 1.0), (1.0, 1.0), 2.0, 1.0));
        assert!(cov.area.iter().all(|&a| a == 0.0));
    }

    #[test]
    fn overlapping_polygons_clamp_to_full_coverage() {
        let square = [(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)];
        let mut cov = PolygonCoverage::new(2, 2);
        cov.add(&square);
        cov.add(&square);
        let mut canvas = Canvas::new(2, 2);
        cov.write(&mut canvas, false);
        assert_eq!(canvas.data, vec![255; 4]);
        let mut canvas = Canvas::new(2, 2);
        cov.write(&mut canvas, true);
        assert_eq!(canvas.data, vec![0; 4]);
    }
}

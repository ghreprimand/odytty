// SPDX-License-Identifier: GPL-3.0-only
// Project-authored callback coordinates and existing licensed font bytes.
use super::*;

fn stops() -> Vec<GradientColorStop> {
    vec![
        GradientColorStop {
            offset: 0.0,
            color: [1.0, 0.0, 0.0, 1.0],
        },
        GradientColorStop {
            offset: 1.0,
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ]
}
fn radial(c0: Point, r0: f32, c1: Point, r1: f32) -> PreparedBrush {
    PreparedBrush::Radial {
        c0,
        r0,
        c1,
        r1,
        stops: stops(),
        extend: Extend::Pad,
    }
}

#[test]
fn radial_no_circle_coverage_is_transparent() {
    let brush = radial(Point::from_xy(0.0, 0.0), 1.0, Point::from_xy(2.0, 0.0), 1.0);
    assert_eq!(brush.sample(Point::from_xy(0.0, 2.0)), [0.0; 4]);
}
#[test]
fn radial_identical_circles_are_transparent() {
    let brush = radial(Point::from_xy(0.0, 0.0), 1.0, Point::from_xy(0.0, 0.0), 1.0);
    assert_eq!(brush.sample(Point::from_xy(0.0, 0.0)), [0.0; 4]);
}
#[test]
fn radial_zero_radius_roots_do_not_paint() {
    let brush = radial(Point::from_xy(0.0, 0.0), 0.0, Point::from_xy(2.0, 0.0), 0.0);
    assert_eq!(brush.sample(Point::from_xy(0.0, 0.0)), [0.0; 4]);
}
#[test]
fn radial_positive_coverage_keeps_the_valid_midpoint() {
    let brush = radial(
        Point::from_xy(0.0, 0.0),
        0.0,
        Point::from_xy(0.0, 0.0),
        100.0,
    );
    assert_eq!(
        brush.sample(Point::from_xy(50.0, 0.0)),
        [0.5, 0.0, 0.5, 1.0]
    );
}
fn sweep(start_angle: f32, end_angle: f32, extend: Extend) -> PreparedBrush {
    PreparedBrush::Sweep {
        center: Point::from_xy(0.0, 0.0),
        start_angle,
        end_angle,
        stops: stops(),
        extend,
    }
}
#[test]
fn sweep_pad_before_start_uses_the_first_stop() {
    assert_eq!(
        sweep(90.0, 180.0, Extend::Pad).sample(Point::from_xy(1.0, -1.0)),
        [1.0, 0.0, 0.0, 1.0]
    );
}
#[test]
fn sweep_pad_after_end_uses_the_last_stop() {
    assert_eq!(
        sweep(90.0, 180.0, Extend::Pad).sample(Point::from_xy(-1.0, 1.0)),
        [0.0, 0.0, 1.0, 1.0]
    );
}
#[test]
fn sweep_normalized_interval_outside_the_domain_keeps_padding_side() {
    assert_eq!(
        sweep(450.0, 540.0, Extend::Pad).sample(Point::from_xy(1.0, -1.0)),
        [1.0, 0.0, 0.0, 1.0]
    );
}
fn alpha_sweep(extend: Extend) -> PreparedBrush {
    PreparedBrush::Sweep {
        center: Point::from_xy(0.0, 0.0),
        start_angle: 90.0,
        end_angle: 170.0,
        stops: vec![
            GradientColorStop {
                offset: 0.0,
                color: [0.0; 4],
            },
            GradientColorStop {
                offset: 1.0,
                color: [0.0, 0.0, 0.0, 1.0],
            },
        ],
        extend,
    }
}
#[test]
fn sweep_repeat_preserves_the_signed_interval_parameter() {
    let color = alpha_sweep(Extend::Repeat).sample(Point::from_xy(1.0, -1.0));
    assert_eq!(color, [0.0, 0.0, 0.0, 0.4375]);
}
#[test]
fn sweep_reflect_preserves_the_signed_interval_parameter() {
    let color = alpha_sweep(Extend::Reflect).sample(Point::from_xy(1.0, -1.0));
    assert_eq!(color, [0.0, 0.0, 0.0, 0.5625]);
}
fn midpoint(alpha: f32) -> [u8; 4] {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fonts/color-emoji-colr-v1.ttf"
    ));
    let font = FontRef::from_index(bytes, 0).unwrap();
    let mut painter = RasterPainter::new(
        font,
        vec![[0, 0, 0, 255], [255; 4]],
        3,
        1,
        CanvasTransform::identity(),
    )
    .unwrap();
    let color_stops = [
        ColorStop {
            offset: 0.0,
            palette_index: 0,
            alpha,
        },
        ColorStop {
            offset: 1.0,
            palette_index: 1,
            alpha,
        },
    ];
    painter.paint_brush(Brush::LinearGradient {
        p0: skrifa::raw::types::Point { x: 0.5, y: 0.5 },
        p1: skrifa::raw::types::Point { x: 2.5, y: 0.5 },
        color_stops: &color_stops,
        extend: Extend::Pad,
    });
    assert!(!painter.failed);
    painter.layers[0].pixmap.data()[4..8].try_into().unwrap()
}
#[test]
fn colr_opaque_gradient_interpolates_in_linear_light() {
    assert_eq!(midpoint(1.0), [188, 188, 188, 255]);
}
#[test]
fn colr_translucent_gradient_outputs_premultiplied_srgb() {
    assert_eq!(midpoint(0.5), [94, 94, 94, 128]);
}

#[test]
fn radial_tangent_and_negative_radius_coverage_are_distinct() {
    let strip = radial(Point::from_xy(0.0, 0.0), 1.0, Point::from_xy(2.0, 0.0), 1.0);
    assert_eq!(strip.sample(Point::from_xy(1.0, 1.0)), [0.5, 0.0, 0.5, 1.0]);
    assert_eq!(
        radial_parameter(
            Point::from_xy(50.0, 0.0),
            Point::from_xy(0.0, 0.0),
            -100.0,
            Point::from_xy(0.0, 0.0),
            0.0
        ),
        Some(1.5)
    );
}

#[test]
fn degenerate_sweep_does_not_repeat_and_pad_keeps_both_sides() {
    for extend in [Extend::Repeat, Extend::Reflect] {
        assert_eq!(
            sweep(90.0, 90.0, extend).sample(Point::from_xy(1.0, -1.0)),
            [0.0; 4]
        );
    }
    assert_eq!(
        sweep(90.0, 90.0, Extend::Pad).sample(Point::from_xy(1.0, -1.0)),
        [1.0, 0.0, 0.0, 1.0]
    );
    assert_eq!(
        sweep(90.0, 90.0, Extend::Pad).sample(Point::from_xy(-1.0, 1.0)),
        [0.0, 0.0, 1.0, 1.0]
    );
}

#[test]
fn gradient_transfer_handles_transparent_stops_and_preserves_solid_bytes() {
    assert_eq!(linearize_premultiplied([0.0; 4]), [0.0; 4]);
    assert_eq!(encode_premultiplied([0.0; 4]), [0.0; 4]);
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/fonts/color-emoji-colr-v1.ttf"
    ));
    let mut painter = RasterPainter::new(
        FontRef::from_index(bytes, 0).unwrap(),
        vec![[128, 64, 32, 128]],
        1,
        1,
        CanvasTransform::identity(),
    )
    .unwrap();
    painter.paint_brush(Brush::Solid {
        palette_index: 0,
        alpha: 0.5,
    });
    assert_eq!(painter.layers[0].pixmap.data(), &[32, 16, 8, 64]);
    assert!(!painter.failed);
}

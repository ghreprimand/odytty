// SPDX-License-Identifier: GPL-3.0-only
//! Boundary checks for the SVG raster budgets with small, hand-counted
//! documents. Coordinates are canvas pixels (identity transform).

use resvg::usvg;

use super::*;

fn tree(body: &str) -> usvg::Tree {
    let text =
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">{body}</svg>"#);
    usvg::Tree::from_str(&text, &usvg::Options::default()).expect("test document converts")
}

fn glyph_cost(body: &str, side: u32, limits: Limits) -> Option<RasterCost> {
    let tree = tree(body);
    let node = tree.node_by_id("glyph1").expect("glyph1");
    cost(node, Transform::identity(), side, side, limits)
}

const OPEN: Limits = Limits {
    visits: u64::MAX,
    pixel_work: u64::MAX,
    live_bytes: u64::MAX,
};

/// A 16 x 16 canvas: 256 pixels, 1024 bytes.
const CANVAS_PIXELS: u64 = 256;
const CANVAS_BYTES: u64 = 1024;
/// A 10 x 10 rect: covered box 12 x 12 with padding, five verbs over 12 rows.
const RECT_WORK: u64 = 144 + 5 * 12;
const RECT: &str = r#"<rect width="10" height="10" fill="red"/>"#;

#[test]
fn a_plain_path_costs_its_covered_box_and_edge_rows() {
    let body = r#"<rect id="glyph1" width="10" height="10" fill="red"/>"#;
    let cost = glyph_cost(body, 16, OPEN).expect("within open limits");
    assert_eq!(cost.pixel_work, CANVAS_PIXELS + RECT_WORK);
    assert_eq!(cost.peak_bytes, CANVAS_BYTES);
    assert_eq!(cost.visits, 1);

    let exact = Limits {
        pixel_work: CANVAS_PIXELS + RECT_WORK,
        live_bytes: CANVAS_BYTES,
        visits: 1,
    };
    assert!(glyph_cost(body, 16, exact).is_some(), "exact limits admit");
    for over in [
        Limits {
            pixel_work: exact.pixel_work - 1,
            ..exact
        },
        Limits {
            live_bytes: exact.live_bytes - 1,
            ..exact
        },
        Limits { visits: 0, ..exact },
    ] {
        assert_eq!(glyph_cost(body, 16, over), None, "{over:?} refuses");
    }
}

#[test]
fn an_isolated_group_holds_a_padded_layer_while_its_children_render() {
    // Layer box 10 x 10 padded to 14 x 14: allocated, filled, composited.
    let body = format!(r#"<g id="glyph1" opacity="0.5">{RECT}</g>"#);
    let cost = glyph_cost(&body, 16, OPEN).expect("within open limits");
    assert_eq!(cost.peak_bytes, CANVAS_BYTES + 14 * 14 * 4);
    assert_eq!(cost.pixel_work, CANVAS_PIXELS + 2 * 14 * 14 + RECT_WORK);
}

#[test]
fn nested_layers_stack_and_sibling_layers_do_not() {
    let nested = format!(r#"<g id="glyph1" opacity="0.5"><g opacity="0.5">{RECT}</g></g>"#);
    let siblings =
        format!(r#"<g id="glyph1"><g opacity="0.5">{RECT}</g><g opacity="0.5">{RECT}</g></g>"#);
    let layer = 14 * 14 * 4;
    assert_eq!(
        glyph_cost(&nested, 16, OPEN).expect("nested").peak_bytes,
        CANVAS_BYTES + 2 * layer
    );
    assert_eq!(
        glyph_cost(&siblings, 16, OPEN)
            .expect("siblings")
            .peak_bytes,
        CANVAS_BYTES + layer
    );
    let one_layer = Limits {
        live_bytes: CANVAS_BYTES + layer,
        ..OPEN
    };
    assert!(glyph_cost(&siblings, 16, one_layer).is_some());
    assert_eq!(glyph_cost(&nested, 16, one_layer), None);
}

#[test]
fn a_layer_is_clamped_to_five_canvases_per_side() {
    let body = r#"<g id="glyph1" opacity="0.5"><rect width="1000" height="1000" fill="red"/></g>"#;
    let cost = glyph_cost(body, 16, OPEN).expect("within open limits");
    assert_eq!(cost.peak_bytes, CANVAS_BYTES + 80 * 80 * 4);
}

#[test]
fn a_clip_path_holds_a_target_buffer_and_a_coverage_mask() {
    let body = format!(
        r#"<clipPath id="c"><rect width="5" height="5"/></clipPath>
           <g id="glyph1" clip-path="url(#c)">{RECT}</g>"#
    );
    let cost = glyph_cost(&body, 16, OPEN).expect("within open limits");
    // Canvas, the group's layer, then the clip buffer and its one-byte mask
    // sized to that layer.
    let layer = 14 * 14;
    assert_eq!(
        cost.peak_bytes,
        CANVAS_BYTES + layer * 4 + layer * 4 + layer
    );
}

#[test]
fn dashes_are_charged_per_dash() {
    // A 100-pixel line with a two-pixel period: 50 dashes.
    let plain = r#"<path id="glyph1" d="M0 1 L100 1" stroke="red" stroke-width="1"/>"#;
    let dashed = r#"<path id="glyph1" d="M0 1 L100 1" stroke="red" stroke-width="1" stroke-dasharray="1 1"/>"#;
    let plain = glyph_cost(plain, 16, OPEN).expect("plain").pixel_work;
    let dashed = glyph_cost(dashed, 16, OPEN).expect("dashed").pixel_work;
    assert_eq!(dashed - plain, 50 * DASH_WORK);
}

#[test]
fn a_dash_count_past_the_rasterizer_cap_is_charged_at_the_cap() {
    let body =
        r#"<path id="glyph1" d="M0 1 L100000 1" stroke="red" stroke-dasharray="0.01 0.01"/>"#;
    let tree = tree(body);
    let Some(usvg::Node::Path(path)) = tree.node_by_id("glyph1") else {
        panic!("glyph1 is a path");
    };
    let dashes = path
        .stroke()
        .and_then(|stroke| stroke.dasharray())
        .expect("dashes");
    assert_eq!(dash_count(path, dashes), MAX_DASHES as u64);
}

#[test]
fn content_outside_the_model_is_refused_even_after_conversion() {
    let filtered = format!(
        r#"<filter id="f"><feGaussianBlur stdDeviation="1"/></filter>
           <g id="glyph1" filter="url(#f)">{RECT}</g>"#
    );
    let masked = format!(
        r#"<mask id="m"><rect width="5" height="5" fill="white"/></mask>
           <g id="glyph1" mask="url(#m)">{RECT}</g>"#
    );
    let function = format!(r#"<g id="glyph1" filter="blur(1)">{RECT}</g>"#);
    for body in [filtered, masked, function] {
        assert_eq!(glyph_cost(&body, 16, OPEN), None, "{body}");
    }
}

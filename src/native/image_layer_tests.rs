// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeSet;

use crate::atlas::CellSize;
use crate::graphics::{GraphicsProtocol, PlacementId, SourceRect, StoredImageId, VisiblePlacement};

use super::app::TAB_BAR_ROWS;
use super::image_layer::{
    ImageUpload, cache_sync_plan, overlay_fit_quad, placement_quad, placement_quad_with_padding,
    placement_quad_with_padding_and_row_offset, visible_image_ids,
};
use super::viewport::WindowPadding;

fn placement(row: usize, column: usize, image_id: StoredImageId) -> VisiblePlacement {
    VisiblePlacement {
        id: PlacementId(1),
        image_id,
        protocol: GraphicsProtocol::Sixel,
        row,
        column,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        },
        display_columns: 4,
        display_rows: 2,
        pixel_offset_x: 0,
        pixel_offset_y: 0,
        z_index: 0,
        generation: 1,
    }
}

#[test]
fn placement_geometry_maps_cells_to_pixel_rect() {
    let mut placement = placement(2, 3, StoredImageId(7));
    placement.pixel_offset_x = 1;
    placement.pixel_offset_y = -2;

    let quad = placement_quad(
        &placement,
        20,
        12,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
    )
    .expect("quad");

    assert_eq!(quad.rect, [25.0, 30.0, 45.0, 42.0]);
    assert_eq!(quad.uv, [0.0, 0.0, 1.0, 1.0]);
}

#[test]
fn placement_geometry_is_offset_by_window_padding() {
    let mut placement = placement(2, 3, StoredImageId(7));
    placement.pixel_offset_x = 1;
    placement.pixel_offset_y = -2;
    let padding = WindowPadding::from_logical(8.0, 1.0);

    let quad = placement_quad_with_padding(
        &placement,
        20,
        12,
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        },
        padding,
    )
    .expect("quad");

    assert_eq!(quad.rect, [33.0, 38.0, 53.0, 50.0]);
}

#[test]
fn placement_geometry_is_offset_by_reserved_top_rows() {
    let mut placement = placement(2, 3, StoredImageId(7));
    placement.pixel_offset_x = 1;
    placement.pixel_offset_y = -2;
    let cell = CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    };
    let reserved_rows = TAB_BAR_ROWS as usize;

    let quad = placement_quad_with_padding_and_row_offset(
        &placement,
        20,
        12,
        cell,
        WindowPadding::ZERO,
        reserved_rows,
        0,
    )
    .expect("quad");

    let cell_grid_y = (placement.row + reserved_rows) as f32 * cell.height as f32
        + placement.pixel_offset_y as f32;
    assert_eq!(quad.rect, [25.0, cell_grid_y, 45.0, cell_grid_y + 12.0]);
}

#[test]
fn placement_geometry_is_offset_by_reserved_left_cols() {
    // F4-V2 (F4V2-NF3): the vertical rail reserves COLUMNS off the left, so a
    // left rail shifts every image placement RIGHT by `reserved_cols` cells —
    // the column-axis sibling of the top-bar row offset above. `reserved_rows`
    // stays 0 for a rail (it reserves columns, not rows), so Y is unshifted.
    let mut placement = placement(2, 3, StoredImageId(7));
    placement.pixel_offset_x = 1;
    placement.pixel_offset_y = -2;
    let cell = CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    };
    let reserved_cols = 16usize;

    let quad = placement_quad_with_padding_and_row_offset(
        &placement,
        20,
        12,
        cell,
        WindowPadding::ZERO,
        0,
        reserved_cols,
    )
    .expect("quad");

    let cell_grid_x = (placement.column + reserved_cols) as f32 * cell.width as f32
        + placement.pixel_offset_x as f32;
    let cell_grid_y = placement.row as f32 * cell.height as f32 + placement.pixel_offset_y as f32;
    assert_eq!(
        quad.rect,
        [
            cell_grid_x,
            cell_grid_y,
            cell_grid_x + 20.0,
            cell_grid_y + 12.0
        ]
    );
}

#[test]
fn placement_geometry_crops_to_cell_extent_without_scaling() {
    let mut placement = placement(1, 1, StoredImageId(2));
    placement.display_columns = 2;
    placement.display_rows = 1;
    placement.source = SourceRect {
        x: 4,
        y: 3,
        width: 50,
        height: 40,
    };

    let quad = placement_quad(
        &placement,
        64,
        64,
        CellSize {
            width: 10,
            height: 12,
            baseline: 9,
        },
    )
    .expect("quad");

    assert_eq!(quad.rect, [10.0, 12.0, 30.0, 24.0]);
    assert_eq!(quad.uv, [4.0 / 64.0, 3.0 / 64.0, 24.0 / 64.0, 15.0 / 64.0]);
}

#[test]
fn visible_ids_are_deduplicated_for_cache_bookkeeping() {
    let placements = vec![
        placement(0, 0, StoredImageId(2)),
        placement(1, 0, StoredImageId(1)),
        placement(2, 0, StoredImageId(2)),
    ];

    assert_eq!(
        visible_image_ids(&placements),
        BTreeSet::from([StoredImageId(1), StoredImageId(2)])
    );
}

#[test]
fn cache_plan_evicts_hidden_ids_and_uploads_missing_visible_ids() {
    let cached = BTreeSet::from([StoredImageId(1), StoredImageId(3)]);
    let placements = vec![
        placement(0, 0, StoredImageId(1)),
        placement(0, 1, StoredImageId(2)),
        placement(0, 2, StoredImageId(4)),
    ];
    let uploads = vec![ImageUpload {
        id: StoredImageId(2),
        width: 1,
        height: 1,
        generation: 1,
        rgba: vec![255, 0, 0, 255],
    }];

    let plan = cache_sync_plan(&cached, &placements, &uploads);

    assert_eq!(plan.evict, vec![StoredImageId(3)]);
    assert_eq!(plan.upload, vec![StoredImageId(2)]);
}

#[test]
fn zero_sized_or_out_of_bounds_sources_emit_no_quad() {
    let mut placement = placement(0, 0, StoredImageId(1));
    placement.source = SourceRect {
        x: 99,
        y: 0,
        width: 1,
        height: 1,
    };

    assert!(
        placement_quad(
            &placement,
            20,
            20,
            CellSize {
                width: 8,
                height: 16,
                baseline: 12,
            },
        )
        .is_none()
    );
}

#[test]
fn visible_placement_row_already_includes_scrollback_projection() {
    let projected = placement(5, 2, StoredImageId(9));
    let quad = placement_quad(
        &projected,
        5,
        5,
        CellSize {
            width: 7,
            height: 11,
            baseline: 8,
        },
    )
    .expect("quad");

    assert_eq!(quad.rect[0], 14.0);
    assert_eq!(quad.rect[1], 55.0);
}

// ---------------------------------------------------------------------------
// C4 viewer overlay fit-quad (pure geometry; no GPU).
// ---------------------------------------------------------------------------

#[test]
fn overlay_fit_centers_a_small_image_without_upscaling() {
    // A 100×50 image inside a 1000×800 viewport: well under 90% on both axes,
    // so it renders at native size, centered.
    let quad = overlay_fit_quad(100, 50, 1000.0, 800.0);
    let w = quad.rect[2] - quad.rect[0];
    let h = quad.rect[3] - quad.rect[1];
    assert_eq!(w, 100.0, "never upscaled past source width");
    assert_eq!(h, 50.0, "never upscaled past source height");
    // Centered: equal margins on each axis.
    assert_eq!(quad.rect[0], (1000.0 - 100.0) / 2.0);
    assert_eq!(quad.rect[1], (800.0 - 50.0) / 2.0);
    // Full texture UV.
    assert_eq!(quad.uv, [0.0, 0.0, 1.0, 1.0]);
}

#[test]
fn overlay_fit_scales_a_large_image_down_preserving_aspect() {
    // A 2000×1000 (2:1) image inside a 1000×1000 viewport. Max box is 900×900;
    // width is the binding axis → scale 900/2000 = 0.45 → 900×450, aspect kept.
    let quad = overlay_fit_quad(2000, 1000, 1000.0, 1000.0);
    let w = quad.rect[2] - quad.rect[0];
    let h = quad.rect[3] - quad.rect[1];
    assert!((w - 900.0).abs() < 1e-3, "fit to 90% width, got {w}");
    assert!((h - 450.0).abs() < 1e-3, "aspect preserved, got {h}");
    // Aspect ratio preserved.
    assert!((w / h - 2.0).abs() < 1e-3);
    // Centered vertically (height < viewport), flush-ish horizontally (90%).
    assert!((quad.rect[1] - (1000.0 - 450.0) / 2.0).abs() < 1e-3);
}

#[test]
fn overlay_fit_height_bound_image() {
    // A tall 500×4000 image inside a 1000×1000 viewport: height is the binding
    // axis. Max box 900×900 → scale 900/4000 = 0.225 → 112.5×900.
    let quad = overlay_fit_quad(500, 4000, 1000.0, 1000.0);
    let w = quad.rect[2] - quad.rect[0];
    let h = quad.rect[3] - quad.rect[1];
    assert!((h - 900.0).abs() < 1e-3, "fit to 90% height, got {h}");
    assert!((w - 112.5).abs() < 1e-3, "aspect preserved, got {w}");
}

#[test]
fn overlay_fit_is_robust_to_degenerate_dims() {
    // Zero dims must not divide-by-zero / NaN; the quad stays finite and inside
    // the viewport (defensive — the decode path never yields a zero dimension).
    let quad = overlay_fit_quad(0, 0, 800.0, 600.0);
    for v in quad.rect {
        assert!(
            v.is_finite(),
            "fit rect must stay finite for degenerate dims"
        );
    }
}

#[test]
fn overlay_fit_constraining_axis_is_ninety_percent_and_centered() {
    // Phase 13a framing contract: a large image is fit to exactly 0.9 of the
    // viewport on the CONSTRAINING axis, and is centered with equal L/R and T/B
    // margins. Use a square image in a wide viewport: width is the tighter axis
    // relative to 0.9 on each side, so the fit is height-bound here.
    let (vp_w, vp_h) = (1600.0f32, 1000.0f32);
    let quad = overlay_fit_quad(2000, 2000, vp_w, vp_h);
    let (x0, y0, x1, y1) = (quad.rect[0], quad.rect[1], quad.rect[2], quad.rect[3]);
    let w = x1 - x0;
    let h = y1 - y0;

    // Square image stays square.
    assert!((w - h).abs() < 1e-3, "aspect preserved: {w} vs {h}");
    // Height is the constraining axis (1000*0.9 = 900 < 1600*0.9 = 1440).
    assert!(
        (h - vp_h * 0.9).abs() < 1e-3,
        "constraining axis at 90%, got {h}"
    );
    assert!(w <= vp_w * 0.9 + 1e-3, "non-constraining axis within 90%");

    // Equal margins on each axis (centered).
    let left = x0;
    let right = vp_w - x1;
    let top = y0;
    let bottom = vp_h - y1;
    assert!(
        (left - right).abs() < 1e-3,
        "L/R margins equal: {left} vs {right}"
    );
    assert!(
        (top - bottom).abs() < 1e-3,
        "T/B margins equal: {top} vs {bottom}"
    );
}

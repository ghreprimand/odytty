// SPDX-License-Identifier: GPL-3.0-only
use std::collections::BTreeSet;

use crate::atlas::CellSize;
use crate::graphics::{GraphicsProtocol, PlacementId, SourceRect, StoredImageId, VisiblePlacement};

use super::image_layer::{ImageUpload, cache_sync_plan, placement_quad, visible_image_ids};

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

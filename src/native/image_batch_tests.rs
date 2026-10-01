// SPDX-License-Identifier: GPL-3.0-only
//! Draw batching for image tiles such as Kitty Unicode-placeholder runs.
//!
//! Each run of placeholder cells is its own image tile, so one image spread
//! over a screen of short runs cost one draw per run. Consecutive tiles of one
//! texture at one z-index (and, in a split, under one pane scissor) now share
//! a draw. These tests pin the draw counts and read every frame back, so a
//! batch can never change which pixels an image paints or let a run escape
//! its pane.
//!
//! GPU-gated: each test skips when no adapter is available.

use super::gpu::ViewportUniform;
use super::gpu_tests::{TEST_SURFACE_FORMAT, test_device_with_hdr};
use super::image_layer::{ImageLayer, ImageUpload, PaneImageInput, PaneImageUpload};
use crate::atlas::CellSize;
use crate::graphics::{GraphicsProtocol, PlacementId, SourceRect, StoredImageId, VisiblePlacement};
use wgpu::util::DeviceExt;

/// Cell edge in pixels.
const CELL: u32 = 8;

fn cell_size() -> CellSize {
    CellSize {
        width: CELL,
        height: CELL,
        baseline: 6,
    }
}

fn viewport_buffer(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("test-image-batch-viewport"),
        contents: bytemuck::bytes_of(&ViewportUniform {
            size: [width as f32, height as f32],
            effect: [0.0, 1.0],
            text: [1.0, 0.0, 0.0, 0.0],
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

/// One one-row run of image 1, `columns` cells wide.
fn run(row: usize, column: usize, columns: usize, z_index: i32) -> VisiblePlacement {
    VisiblePlacement {
        id: PlacementId(((row * 4096 + column) as u64) | (1 << 63)),
        image_id: StoredImageId(1),
        protocol: GraphicsProtocol::Kitty,
        row,
        column,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        },
        display_columns: columns,
        display_rows: 1,
        pixel_offset_x: 0,
        pixel_offset_y: 0,
        z_index,
        generation: 1,
    }
}

/// An opaque blue 8x8 image 1.
fn blue_upload() -> ImageUpload {
    ImageUpload {
        id: StoredImageId(1),
        width: 8,
        height: 8,
        generation: 1,
        rgba: [0, 0, 255, 255].repeat(64),
    }
}

/// Draw the layer's below-text and above-text halves over a green clear and
/// return the target as tightly packed RGBA rows.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layer: &ImageLayer,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test-image-batch-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TEST_SURFACE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("test-image-batch-encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("test-image-batch-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::GREEN),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        layer.draw_below(&mut pass);
        layer.draw_above(&mut pass);
    }
    let bpr = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-image-batch-readback"),
        size: u64::from(bpr) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).ok();
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll");
    rx.recv().expect("map callback").expect("map readback");
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let start = (y * bpr) as usize;
        pixels.extend_from_slice(&mapped[start..start + (width * 4) as usize]);
    }
    drop(mapped);
    readback.unmap();
    pixels
}

/// The pixel at the centre of cell (`row`, `column`).
fn cell_centre(pixels: &[u8], width: u32, row: usize, column: usize) -> [u8; 4] {
    let x = column as u32 * CELL + CELL / 2;
    let y = row as u32 * CELL + CELL / 2;
    let i = ((y * width + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn is_image(px: [u8; 4]) -> bool {
    px[2] >= 180 && px[1] <= 40
}

fn is_clear(px: [u8; 4]) -> bool {
    px[1] >= 150 && px[2] <= 40
}

/// Every even column of the grid holds a one-cell run and every odd column is
/// a gap.
fn assert_alternating_runs(pixels: &[u8], width: u32, rows: usize, columns: usize, frame: &str) {
    for row in 0..rows {
        for column in 0..columns {
            let px = cell_centre(pixels, width, row, column);
            if column % 2 == 0 {
                assert!(
                    is_image(px),
                    "{frame}: run at row {row} column {column} must paint the image, got {px:?}"
                );
            } else {
                assert!(
                    is_clear(px),
                    "{frame}: gap at row {row} column {column} must stay clear, got {px:?}"
                );
            }
        }
    }
}

#[test]
fn one_image_placeholder_grid_batches_into_one_draw_and_paints_every_run() {
    let Some((device, queue)) = test_device_with_hdr() else {
        return;
    };
    const COLS: usize = 80;
    const ROWS: usize = 25;
    const W: u32 = COLS as u32 * CELL;
    const H: u32 = ROWS as u32 * CELL;
    let mut layer = ImageLayer::new(&device, TEST_SURFACE_FORMAT, TEST_SURFACE_FORMAT);
    let viewport_buf = viewport_buffer(&device, W, H);
    let upload = blue_upload();
    let update = |layer: &mut ImageLayer, placements: &[VisiblePlacement]| {
        let uploads = if layer.cached_generations(3).is_empty() {
            vec![upload.clone()]
        } else {
            Vec::new()
        };
        layer.update_with_padding(
            &device,
            &queue,
            &viewport_buf,
            3,
            placements,
            &uploads,
            cell_size(),
            crate::native::WindowPadding::ZERO,
            0,
            0,
            [0.0, 0.0],
        );
    };

    let grid: Vec<VisiblePlacement> = (0..ROWS)
        .flat_map(|row| {
            (0..COLS)
                .step_by(2)
                .map(move |column| run(row, column, 1, 0))
        })
        .collect();
    assert_eq!(grid.len(), 1_000);
    update(&mut layer, &grid);
    assert_eq!(
        layer.draw_call_count(),
        1,
        "one image across 1,000 disjoint runs is one draw"
    );
    let pixels = render(&device, &queue, &layer, W, H);
    assert_alternating_runs(&pixels, W, ROWS, COLS, "one batch");

    // A run at another z-index in the middle splits the batch into the runs
    // before it, the run itself (drawn in the below-text half), and the runs
    // after it. The split frame still paints every run and leaves every gap.
    let mut layered = grid.clone();
    layered[500].z_index = -1;
    update(&mut layer, &layered);
    assert_eq!(layer.draw_call_count(), 3);
    let pixels = render(&device, &queue, &layer, W, H);
    assert_alternating_runs(&pixels, W, ROWS, COLS, "z-split batch");
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
}

#[test]
fn pane_runs_batch_only_under_one_scissor_and_stay_inside_their_pane() {
    let Some((device, queue)) = test_device_with_hdr() else {
        return;
    };
    // Two 8-column panes side by side, 4 rows tall. Both use the same
    // namespace and image, so the cache key matches across them and only the
    // scissor can keep their draws apart.
    const PANE_COLS: usize = 8;
    const ROWS: usize = 4;
    const W: u32 = 2 * PANE_COLS as u32 * CELL;
    const H: u32 = ROWS as u32 * CELL;
    let pane_px = PANE_COLS as u32 * CELL;
    let mut layer = ImageLayer::new(&device, TEST_SURFACE_FORMAT, TEST_SURFACE_FORMAT);
    let viewport_buf = viewport_buffer(&device, W, H);

    // Left pane: runs at even columns, plus a two-cell run at its last column
    // whose second cell would cross into the right pane.
    let left: Vec<VisiblePlacement> = (0..ROWS)
        .flat_map(|row| {
            [0, 2, 4]
                .into_iter()
                .map(move |column| run(row, column, 1, 0))
                .chain(std::iter::once(run(row, PANE_COLS - 1, 2, 0)))
        })
        .collect();
    // Right pane: runs at odd columns, so its column 0 is a gap that the left
    // pane's overhang must not paint.
    let right: Vec<VisiblePlacement> = (0..ROWS)
        .flat_map(|row| {
            (1..PANE_COLS)
                .step_by(2)
                .map(move |column| run(row, column, 1, 0))
        })
        .collect();
    let panes = [
        PaneImageInput {
            namespace: 7,
            placements: &left,
            origin: [0.0, 0.0],
            scissor: [0, 0, pane_px, H],
        },
        PaneImageInput {
            namespace: 7,
            placements: &right,
            origin: [pane_px as f32, 0.0],
            scissor: [pane_px, 0, pane_px, H],
        },
    ];
    let upload = PaneImageUpload {
        namespace: 7,
        upload: blue_upload(),
    };
    layer.update_panes(
        &device,
        &queue,
        &viewport_buf,
        &panes,
        std::slice::from_ref(&upload),
        cell_size(),
        [W, H],
    );
    assert_eq!(
        layer.draw_call_count(),
        2,
        "each pane's runs batch into one draw, and the two scissors never merge"
    );

    let pixels = render(&device, &queue, &layer, W, H);
    for row in 0..ROWS {
        for column in 0..PANE_COLS {
            let px = cell_centre(&pixels, W, row, column);
            let painted = matches!(column, 0 | 2 | 4 | 7);
            assert!(
                if painted { is_image(px) } else { is_clear(px) },
                "left pane row {row} column {column}: painted={painted}, got {px:?}"
            );
            let px = cell_centre(&pixels, W, row, PANE_COLS + column);
            let painted = column % 2 == 1;
            assert!(
                if painted { is_image(px) } else { is_clear(px) },
                "right pane row {row} column {column}: painted={painted}, got {px:?} \
                 (column 0 also proves the left overhang was clipped)"
            );
        }
    }
    device.poll(wgpu::PollType::wait_indefinitely()).ok();
}

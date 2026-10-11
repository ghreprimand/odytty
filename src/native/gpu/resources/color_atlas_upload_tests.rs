// SPDX-License-Identifier: GPL-3.0-only
//! A colour atlas wider than the device texture limit is a supported, declining
//! state; building its texture must neither panic nor raise a validation error.
use super::*;
use crate::atlas::CellSize;
use crate::emoji::{ColorGlyphId, ColorGlyphKey};

/// Headless device whose 2D texture limit is as small as the adapter allows,
/// under the shared GPU test availability policy.
fn small_limit_device(test: &str) -> Option<crate::native::gpu::HeadlessGpuFixture> {
    crate::native::test_support::gpu_device::headless_device_with_limits(
        test,
        "odytty-color-atlas-upload-test-device",
        false,
        wgpu::Limits {
            max_texture_dimension_2d: 256,
            ..wgpu::Limits::default()
        },
    )
}

#[test]
fn atlas_wider_than_the_device_limit_builds_a_clamped_texture() {
    let Some(gpu) =
        small_limit_device("atlas_wider_than_the_device_limit_builds_a_clamped_texture")
    else {
        return;
    };
    let limit = gpu.device.limits().max_texture_dimension_2d;
    // The atlas is 32 cells wide, so this cell makes it wider than the limit.
    let cell = CellSize {
        width: limit / 32 + 1,
        height: 2,
        baseline: 1,
    };
    let atlas = ColorGlyphAtlas::new(cell);
    assert!(atlas.width > limit, "fixture must exceed the device limit");
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let texture = create_color_atlas_texture(&gpu.device, &gpu.queue, &atlas);
    gpu.queue.submit([]);
    let error = pollster::block_on(scope.pop());
    assert!(error.is_none(), "validation error: {error:?}");
    assert_eq!(texture.width(), limit);
    assert_eq!(texture.height(), atlas.height.min(limit));
    // The oversized atlas still declines every lookup and insert.
    let key = ColorGlyphKey::new(1, ColorGlyphId::Glyph(1), 1.0, 1.0, 1);
    assert!(atlas.lookup(key).is_none());
}

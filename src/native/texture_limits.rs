// SPDX-License-Identifier: GPL-3.0-only
//! Shared guards for two-dimensional GPU texture allocations.

use std::borrow::Cow;

/// Bytes a single-mip 2D texture occupies at its own dimensions and format, for
/// memory attribution.
///
/// This is the size OdyTTY asked the driver for, computed from the texture's
/// declared extent and its format's block size. It is not a claim about where
/// the driver placed those bytes — that varies by adapter, backend, and
/// allocator, which is exactly why the attribution reports GPU totals alongside
/// the resident set rather than folded into it. A format with no host-copy block
/// size contributes zero rather than a guessed figure.
pub(super) fn texture_bytes(texture: &wgpu::Texture) -> u64 {
    let Some(block) = texture.format().block_copy_size(None) else {
        return 0;
    };
    u64::from(texture.width())
        .saturating_mul(u64::from(texture.height()))
        .saturating_mul(u64::from(texture.depth_or_array_layers()))
        .saturating_mul(u64::from(block))
}

/// Clamp a texture extent axis-by-axis to a device limit. Zero-sized inputs
/// become one pixel because WebGPU textures cannot have an empty dimension.
pub(super) fn clamp_dimensions(width: u32, height: u32, limit: u32) -> (u32, u32) {
    let limit = limit.max(1);
    (width.clamp(1, limit), height.clamp(1, limit))
}

/// Fit an image inside a square device limit while preserving its aspect ratio.
pub(super) fn fit_dimensions(width: u32, height: u32, limit: u32) -> (u32, u32) {
    let width = width.max(1);
    let height = height.max(1);
    let limit = limit.max(1);
    if width <= limit && height <= limit {
        return (width, height);
    }

    if width >= height {
        let fitted_height = (u64::from(height) * u64::from(limit) / u64::from(width)).max(1) as u32;
        (limit, fitted_height)
    } else {
        let fitted_width = (u64::from(width) * u64::from(limit) / u64::from(height)).max(1) as u32;
        (fitted_width, limit)
    }
}

/// Return tightly-packed RGBA8 pixels that fit the device texture limit.
/// In-limit input stays borrowed; oversized input is resampled once.
pub(super) fn fit_rgba8<'a>(
    rgba: &'a [u8],
    width: u32,
    height: u32,
    limit: u32,
) -> Option<(Cow<'a, [u8]>, u32, u32)> {
    let needed = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if width == 0 || height == 0 || rgba.len() < needed {
        return None;
    }
    let (fitted_width, fitted_height) = fit_dimensions(width, height, limit);
    if (fitted_width, fitted_height) == (width, height) {
        return Some((Cow::Borrowed(&rgba[..needed]), width, height));
    }

    let source = image::RgbaImage::from_raw(width, height, rgba[..needed].to_vec())?;
    let fitted = image::imageops::resize(
        &source,
        fitted_width,
        fitted_height,
        image::imageops::FilterType::Triangle,
    );
    Some((Cow::Owned(fitted.into_raw()), fitted_width, fitted_height))
}

pub(super) fn extent_2d(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Extent3d {
    let (width, height) = clamp_dimensions(width, height, device.limits().max_texture_dimension_2d);
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_clamp_at_device_boundary() {
        assert_eq!(clamp_dimensions(8193, 4096, 8192), (8192, 4096));
        assert_eq!(clamp_dimensions(0, 0, 8192), (1, 1));
    }

    #[test]
    fn image_fit_preserves_aspect_at_device_boundary() {
        assert_eq!(fit_dimensions(8193, 4096, 8192), (8192, 4095));
        assert_eq!(fit_dimensions(4096, 8193, 8192), (4095, 8192));
        assert_eq!(fit_dimensions(8192, 4096, 8192), (8192, 4096));
    }

    #[test]
    fn oversized_rgba_is_resampled_to_the_limit() {
        let rgba = vec![0x80; 9 * 3 * 4];
        let (pixels, width, height) = fit_rgba8(&rgba, 9, 3, 8).expect("valid RGBA");
        assert_eq!((width, height), (8, 2));
        assert_eq!(pixels.len(), 8 * 2 * 4);
        assert!(matches!(pixels, Cow::Owned(_)));
    }

    #[test]
    fn largest_viewer_image_stays_borrowed_at_desktop_device_limit() {
        const WIDTH: u32 = 12_000;
        const HEIGHT: u32 = 9_000;
        let rgba = vec![0_u8; WIDTH as usize * HEIGHT as usize * 4];

        let (pixels, width, height) = fit_rgba8(&rgba, WIDTH, HEIGHT, 16_384).expect("valid RGBA");

        assert_eq!((width, height), (WIDTH, HEIGHT));
        assert!(matches!(pixels, Cow::Borrowed(_)));
        assert_eq!(pixels.as_ptr(), rgba.as_ptr());
    }
}

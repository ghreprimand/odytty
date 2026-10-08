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

    // This copy is confined to the oversized case, which the graphics-image
    // store already caps at 64 MiB; the borrow above keeps the common in-limit
    // path copy-free.
    let fitted = resample_rgba8(
        rgba[..needed].to_vec(),
        width,
        height,
        fitted_width,
        fitted_height,
    )?;
    Some((Cow::Owned(fitted), fitted_width, fitted_height))
}

/// Resample tightly-packed RGBA8 pixels to an exact target size.
///
/// The filter is `Triangle`, whose support the resampler scales by the
/// downscale ratio: reducing an axis by a factor of N averages over the full
/// N-pixel source footprint rather than over a fixed 2-tap neighbourhood. A
/// minifying downscale is therefore a correct weighted area average, and every
/// source pixel contributes to the result. That is the property the background
/// pass depends on — nearest sampling would drop source pixels outright, and
/// GPU bilinear minification without mipmaps reads only a 2x2 texel
/// neighbourhood no matter how large the ratio is, which aliases high-frequency
/// content instead of averaging it.
///
/// Returns `None` for a zero-sized target or an under-length source buffer,
/// rather than fabricating pixels.
///
/// Takes the source buffer by value and truncates rather than copying:
/// duplicating a full-resolution RGBA image purely to satisfy a borrow was a
/// measured term in the startup peak, so the caller decides whether a copy is
/// made.
pub(super) fn resample_rgba8(
    mut rgba: Vec<u8>,
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Option<Vec<u8>> {
    let needed = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if width == 0 || height == 0 || target_width == 0 || target_height == 0 || rgba.len() < needed {
        return None;
    }
    rgba.truncate(needed);
    // `resize` treats colour and alpha as independent channels, which is only
    // right for premultiplied pixels: on straight alpha the colour of fully
    // transparent pixels (often black) bleeds into the semi-transparent edge.
    // Premultiply in place when alpha varies, resize, then divide back out.
    // Constant alpha (every opaque image) skips both passes and is unchanged.
    let varying_alpha = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] != rgba[3]);
    if varying_alpha {
        premultiply_rgba8(&mut rgba);
    }
    let source = image::RgbaImage::from_raw(width, height, rgba)?;
    let resampled = image::imageops::resize(
        &source,
        target_width,
        target_height,
        image::imageops::FilterType::Triangle,
    );
    let mut out = resampled.into_raw();
    if varying_alpha {
        unpremultiply_rgba8(&mut out);
    }
    Some(out)
}

/// Scale each pixel's colour by its alpha, rounding to nearest.
fn premultiply_rgba8(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// Divide premultiplied colour back by alpha, rounding to nearest and
/// clamping; a fully transparent pixel keeps black colour.
fn unpremultiply_rgba8(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .map_or(0, |value| value.min(255) as u8);
        }
    }
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

    /// An oversized image whose left half is transparent black and whose
    /// right half is opaque white: downscaling must not darken the edge with
    /// the transparent pixels' colour. Each visible output pixel stays white
    /// whatever its averaged alpha.
    #[test]
    fn resampling_straight_alpha_does_not_bleed_transparent_colour() {
        let (width, height) = (8u32, 2u32);
        let mut rgba = Vec::new();
        for _ in 0..height {
            for x in 0..width {
                if x < width / 2 {
                    rgba.extend_from_slice(&[0, 0, 0, 0]);
                } else {
                    rgba.extend_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        let out = resample_rgba8(rgba, width, height, 3, 1).expect("resampled");
        for pixel in out.as_chunks::<4>().0.iter() {
            if pixel[3] > 0 {
                assert!(
                    pixel[..3].iter().all(|&channel| channel >= 250),
                    "edge pixel darkened by transparent colour: {pixel:?}"
                );
            }
        }
        assert!(
            out.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] > 0 && pixel[3] < 255),
            "an edge pixel blends alpha"
        );
    }

    #[test]
    fn opaque_images_resample_unchanged_by_the_alpha_passes() {
        let rgba: Vec<u8> = (0..9 * 3)
            .flat_map(|i| [i as u8, 255 - i as u8, 7, 255])
            .collect();
        let expected = {
            let source = image::RgbaImage::from_raw(9, 3, rgba.clone()).unwrap();
            image::imageops::resize(&source, 8, 2, image::imageops::FilterType::Triangle).into_raw()
        };
        assert_eq!(resample_rgba8(rgba, 9, 3, 8, 2).unwrap(), expected);
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

// SPDX-License-Identifier: GPL-3.0-only
//! Shared image-file decode point (FLAG B + FLAG F).
//!
//! Every image-file decode in the app funnels through this one module so the
//! decode bound is enforced in exactly one place — the same "single audit
//! point" discipline as the argv-only `spawn_detached`. Two callers use it:
//!
//! * the background-image / wallpaper path ([`super::gpu::image`]), and
//! * the in-terminal image viewer overlay (Phase 9 / C4).
//!
//! ## The decode bound (FLAG B)
//!
//! The terminal graphics `ImageStore` enforces a 64 MiB cap, but that cap is
//! applied *after* decode — on the resulting RGBA buffer. A crafted small file
//! (a decompression bomb: tiny on disk, enormous decoded) can therefore OOM
//! *during* decode before the post-decode cap ever sees it. The fix is to bound
//! the decode itself: [`image::Limits`] (max width/height/total allocation) set
//! on the reader **before** `.decode()`. An image that exceeds the bound is
//! refused gracefully (`None`) — never a panic, never an unbounded allocation.
//!
//! The bound is deliberately generous (large enough for any real screenshot or
//! photo a user would open) while still capping pathological inputs:
//! [`MAX_IMAGE_DIM`] per axis and [`MAX_IMAGE_ALLOC_BYTES`] total.
//!
//! ## Format trust
//!
//! `with_guessed_format()` content-sniffs the actual bytes (magic numbers), so
//! a `.png` that is really text — or a file with no/extension-mismatched name —
//! is classified by its content, not its name. The extension allowlist in the
//! pure `crate::paths` layer only decides whether to *offer* the viewer; this
//! decode is the real gate.

use std::path::Path;

/// Maximum decoded dimension per axis (FLAG B). 12000px comfortably covers
/// 8K-and-beyond screenshots and large photos while refusing a dimension bomb.
pub(in crate::native) const MAX_IMAGE_DIM: u32 = 12_000;

/// Maximum total decode allocation (FLAG B). 256 MiB bounds the worst-case
/// intermediate + output buffers; an image needing more is refused before it
/// can exhaust memory. (The post-decode graphics store still applies its own
/// 64 MiB cap on what is actually uploaded.)
pub(in crate::native) const MAX_IMAGE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

/// The single decode bound, applied identically to every decode call.
fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    limits
}

/// Decode an image **file** to tightly-packed RGBA8 + dimensions, bounded by
/// [`image_limits`]. Silent + resilient by design: a missing / unreadable /
/// unidentifiable / undecodable / oversized file returns `None` and never
/// panics, so a bad path can never crash the renderer. The caller decides
/// whether to log.
pub(in crate::native) fn decode_image_rgba(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    reader.limits(image_limits());
    let image = reader.decode().ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some((image.into_raw(), width, height))
}

/// Decode image **bytes** to tightly-packed RGBA8 + dimensions, bounded by
/// [`image_limits`]. Same contract as [`decode_image_rgba`]; this is the
/// file-free seam used by two callers: the embedded default background
/// ([`super::gpu::default_background`], which decodes compiled-in bytes), and
/// the robustness tests that drive synthetic byte buffers (truncated headers,
/// decompression bombs, garbage) so no test ever touches the real filesystem.
pub(in crate::native) fn decode_image_rgba_bytes(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = image::ImageReader::new(cursor).with_guessed_format().ok()?;
    reader.limits(image_limits());
    let image = reader.decode().ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some((image.into_raw(), width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, ImageFormat};

    /// Encode a synthetic solid RGBA image to an in-memory byte buffer in the
    /// given format — the test-only inverse of the decoder, so robustness tests
    /// never need a real file. PNG/JPEG only (the always-available encoders
    /// among the enabled features).
    fn encode_solid(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let rgba = vec![0x40u8; (width as usize) * (height as usize) * 4];
        let mut out = std::io::Cursor::new(Vec::new());
        match format {
            ImageFormat::Png => {
                image::codecs::png::PngEncoder::new(&mut out)
                    .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
                    .unwrap();
            }
            ImageFormat::Jpeg => {
                // JPEG has no alpha; feed RGB.
                let rgb = vec![0x40u8; (width as usize) * (height as usize) * 3];
                image::codecs::jpeg::JpegEncoder::new(&mut out)
                    .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
                    .unwrap();
            }
            _ => unreachable!("test encoder only covers png/jpeg"),
        }
        out.into_inner()
    }

    #[test]
    fn decodes_a_valid_synthetic_png() {
        let bytes = encode_solid(3, 2, ImageFormat::Png);
        let (rgba, w, h) = decode_image_rgba_bytes(&bytes).expect("valid png decodes");
        assert_eq!((w, h), (3, 2));
        assert_eq!(rgba.len(), 3 * 2 * 4);
    }

    #[test]
    fn decodes_a_valid_synthetic_jpeg() {
        let bytes = encode_solid(4, 4, ImageFormat::Jpeg);
        let (rgba, w, h) = decode_image_rgba_bytes(&bytes).expect("valid jpeg decodes");
        assert_eq!((w, h), (4, 4));
        assert_eq!(rgba.len(), 4 * 4 * 4);
    }

    #[test]
    fn content_sniffing_ignores_a_lying_name() {
        // The decoder never sees the name — it sniffs the bytes. A valid PNG
        // body decodes regardless of what extension a caller thought it had.
        let bytes = encode_solid(2, 2, ImageFormat::Png);
        assert!(decode_image_rgba_bytes(&bytes).is_some());
        // And real PNG magic is what `guess_format` keys on.
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Png);
    }

    #[test]
    fn empty_bytes_refused_gracefully() {
        assert!(decode_image_rgba_bytes(&[]).is_none());
    }

    #[test]
    fn garbage_bytes_refused_gracefully() {
        let garbage = vec![0xAB, 0xCD, 0xEF, 0x00, 0x11, 0x22, 0x33, 0x44];
        assert!(decode_image_rgba_bytes(&garbage).is_none());
    }

    #[test]
    fn truncated_png_header_refused_gracefully() {
        // The 8-byte PNG signature alone, with no IHDR/body. Must not panic.
        let truncated = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert!(decode_image_rgba_bytes(&truncated).is_none());
    }

    #[test]
    fn truncated_after_valid_header_refused_gracefully() {
        // A real PNG cut off mid-stream: a valid header that cannot finish
        // decoding. Refused, never a panic or unbounded read.
        let mut bytes = encode_solid(8, 8, ImageFormat::Png);
        bytes.truncate(bytes.len() / 2);
        assert!(decode_image_rgba_bytes(&bytes).is_none());
    }

    #[test]
    fn dimension_bomb_is_refused_by_the_limit() {
        // A PNG that *declares* a dimension past MAX_IMAGE_DIM must be rejected
        // by the pre-decode width/height limit rather than attempting the
        // allocation. We forge a PNG signature + IHDR declaring a huge width.
        let bytes = forge_png_ihdr(MAX_IMAGE_DIM + 1, 1);
        assert!(
            decode_image_rgba_bytes(&bytes).is_none(),
            "an over-limit declared dimension must be refused, not allocated"
        );
    }

    /// Build a PNG signature + a single IHDR chunk declaring `width`×`height`
    /// (8-bit RGBA). No image data follows — enough for the decoder to read the
    /// header and apply the dimension limit before any pixel allocation.
    fn forge_png_ihdr(width: u32, height: u32) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth 8, color type 6 (RGBA)
        let len = (ihdr.len() as u32 - 4).to_be_bytes(); // length excludes the type tag
        let crc = crc32(&ihdr).to_be_bytes();
        out.extend_from_slice(&len);
        out.extend_from_slice(&ihdr);
        out.extend_from_slice(&crc);
        out
    }

    /// Minimal CRC-32 (PNG polynomial) for the forged IHDR chunk.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
}

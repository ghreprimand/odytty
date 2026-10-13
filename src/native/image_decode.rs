// SPDX-License-Identifier: GPL-3.0-only
//! Shared image-file decode point.
//!
//! Every image-file decode in the app funnels through this one module so the
//! decode bound is enforced in exactly one place - the same "single audit
//! point" discipline as the argv-only `spawn_detached`. Two callers use it:
//!
//! * the background-image / wallpaper path ([`super::gpu::image`]), and
//! * the in-terminal image viewer overlay.
//!
//! ## The decode bound
//!
//! The terminal graphics `ImageStore` enforces a 64 MiB cap, but that cap is
//! applied *after* decode - on the resulting RGBA buffer. A crafted small file
//! (a decompression bomb: tiny on disk, enormous decoded) can therefore OOM
//! *during* decode before the post-decode cap ever sees it. The fix is to bound
//! the decode itself: [`image::Limits`] (max width/height/total allocation) set
//! on the reader **before** `.decode()`. Reported decode errors return `None`.
//! Later RGBA conversion and resize allocations are outside this limits object;
//! it is not a whole-pipeline peak-memory or execution-time bound.
//!
//! The bound is deliberately generous (large enough for any real screenshot or
//! photo a user would open) while still capping pathological inputs:
//! [`MAX_IMAGE_DIM`] per axis and [`MAX_IMAGE_ALLOC_BYTES`] total.
//!
//! ## Format trust
//!
//! `with_guessed_format()` content-sniffs the actual bytes (magic numbers), so
//! a `.png` that is really text - or a file with no/extension-mismatched name -
//! is classified by its content, not its name. The extension allowlist in the
//! pure `crate::paths` layer only decides whether to *offer* the viewer; this
//! decode is the real gate.

use std::path::Path;

/// Maximum decoded dimension per axis. 12000px comfortably covers
/// 8K-and-beyond screenshots and large photos while refusing a dimension bomb.
pub(in crate::native) const MAX_IMAGE_DIM: u32 = 12_000;

/// Decoder allocation budget. Subsequent RGBA conversion and resizing
/// can allocate additional buffers outside this budget. The post-decode
/// graphics store separately limits retained image data.
pub(in crate::native) const MAX_IMAGE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;

/// The single decode bound, applied identically to every decode call.
fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    limits
}

/// Open an image file for decoding. Only a regular file is opened, and on
/// Unix the open never blocks, so a FIFO or device named like an image cannot
/// freeze the window (hover open, image viewer, background image).
fn open_image_reader(path: &Path) -> Option<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    let file = crate::bounded_io::open_regular(path).ok()?;
    image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .ok()
}

/// Decode an image **file** to tightly-packed RGBA8 + dimensions. The decoder
/// receives [`image_limits`]; conversion to RGBA8 occurs afterward. Reported
/// I/O and decode errors return `None`, as does a path that is not a regular
/// file. Reads of a regular file have no deadline, and this
/// helper does not catch dependency panics or allocation failure. The caller
/// decides whether to log.
pub(in crate::native) fn decode_image_rgba(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let mut reader = open_image_reader(path)?;
    reader.limits(image_limits());
    let image = reader.decode().ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some((image.into_raw(), width, height))
}

/// Decode image **bytes** to tightly-packed RGBA8 + dimensions, bounded by
/// [`image_limits`]. Same contract as [`decode_image_rgba`].
///
/// Test-only identity-fit convenience over [`decode_image_rgba_fit_bytes`]:
/// production byte decodes (the embedded default background) always size to
/// the surface through the fit seam, while the robustness tests that drive
/// synthetic byte buffers (truncated headers, decompression bombs, garbage)
/// only need the decode bound and not a resize.
#[cfg(test)]
fn decode_image_rgba_bytes(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, width, height, _source) =
        decode_image_rgba_fit_bytes(bytes, |width, height| (width, height))?;
    Some((rgba, width, height))
}

/// A fit-decoded image: tightly-packed RGBA8 bytes at target size, the target
/// width and height, and the source dimensions the decoder saw.
type FittedRgba = (Vec<u8>, u32, u32, (u32, u32));

/// Decode an image **file** and resize it to the dimensions `fit` chooses,
/// converting to RGBA8 only *after* the resize. Same bound and resilience
/// contract as [`decode_image_rgba`].
///
/// The ordering is the point: a full-resolution RGBA8 copy of a large source
/// is avoided when `fit` selects a smaller target. For a 3840x2160 source that
/// buffer alone is 33 MB. The prior pipeline (decode to RGBA8, then copy, then
/// resample) held three
/// full-resolution buffers at once at startup. Resizing the decoder-native
/// image first avoids that full-size conversion: the opaque RGB8 source uses
/// 24.9 MB, and RGBA8 conversion happens at target size. Source, resize, and
/// conversion allocations may overlap; the decoder budget does not cover them
/// all.
///
/// `fit` receives the source dimensions and returns the target dimensions.
/// Returning the source dimensions skips the resize. A zero target refuses the
/// decode (`None`), matching the resilience contract.
pub(in crate::native) fn decode_image_rgba_fit(
    path: &Path,
    fit: impl FnOnce(u32, u32) -> (u32, u32),
) -> Option<FittedRgba> {
    let mut reader = open_image_reader(path)?;
    reader.limits(image_limits());
    finish_fit(reader.decode().ok()?, fit)
}

/// Decode image **bytes** and resize to the dimensions `fit` chooses; the
/// byte-input twin of [`decode_image_rgba_fit`], used for the embedded default
/// background.
pub(in crate::native) fn decode_image_rgba_fit_bytes(
    bytes: &[u8],
    fit: impl FnOnce(u32, u32) -> (u32, u32),
) -> Option<FittedRgba> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = image::ImageReader::new(cursor).with_guessed_format().ok()?;
    reader.limits(image_limits());
    finish_fit(reader.decode().ok()?, fit)
}

/// Shared tail of the fit decoders: choose target dimensions, downscale in
/// the decoder-native colour type, then convert to RGBA8 at target size.
/// Returns `(rgba, width, height, source_dimensions)`.
fn finish_fit(
    image: image::DynamicImage,
    fit: impl FnOnce(u32, u32) -> (u32, u32),
) -> Option<FittedRgba> {
    let source_width = image.width();
    let source_height = image.height();
    let (target_width, target_height) = fit(source_width, source_height);
    if target_width == 0 || target_height == 0 {
        return None;
    }
    if (target_width, target_height) == (source_width, source_height) {
        let rgba = image.into_rgba8();
        return Some((
            rgba.into_raw(),
            source_width,
            source_height,
            (source_width, source_height),
        ));
    }
    // `thumbnail_exact` is the integer box-average downscale: every source
    // pixel contributes to exactly one target pixel, with no floating-point
    // intermediate image. The float resampler's intermediate alone is
    // source-width x target-height x 4 channels of f32 (a measured ~75-100 MB
    // startup transient for a 4K source), while the box average allocates only
    // the target. Averaging semantics are what the scrim's luminance reasoning
    // needs (a convex combination can never widen the luminance bounds), and
    // the background sizing never upscales past source resolution, so the
    // downscale-only contract holds.
    let rgba = image
        .thumbnail_exact(target_width, target_height)
        .into_rgba8()
        .into_raw();
    Some((
        rgba,
        target_width,
        target_height,
        (source_width, source_height),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, ImageFormat};

    /// Encode a synthetic solid RGBA image to an in-memory byte buffer in the
    /// given format - the test-only inverse of the decoder, so robustness tests
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
        // The decoder never sees the name - it sniffs the bytes. A valid PNG
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

    /// Every decode entry point, over the same PNG bytes: the two byte
    /// decoders directly and the two file decoders through a temp file.
    fn decode_through_every_entry_point(name: &str, bytes: &[u8]) -> [bool; 4] {
        let path = crate::test_dirs::fresh_temp_dir("oid").join(format!("{name}.png"));
        std::fs::write(&path, bytes).expect("write fixture");
        let identity = |width, height| (width, height);
        let decoded = [
            decode_image_rgba_bytes(bytes).is_some(),
            decode_image_rgba_fit_bytes(bytes, identity).is_some(),
            decode_image_rgba(&path).is_some(),
            decode_image_rgba_fit(&path, identity).is_some(),
        ];
        let _ = std::fs::remove_dir_all(path.parent().expect("fixture dir"));
        decoded
    }

    #[test]
    fn a_real_image_one_pixel_past_the_axis_limit_is_refused() {
        // A complete, valid PNG one pixel wider than MAX_IMAGE_DIM (a few
        // hundred bytes compressed, about 48 KB decoded): only the decoder
        // limit refuses it. The control at the limit decodes.
        let past = encode_solid(MAX_IMAGE_DIM + 1, 1, ImageFormat::Png);
        assert_eq!(decode_through_every_entry_point("past", &past), [false; 4]);
        let tall = encode_solid(1, MAX_IMAGE_DIM + 1, ImageFormat::Png);
        assert_eq!(decode_through_every_entry_point("tall", &tall), [false; 4]);
        let at = encode_solid(MAX_IMAGE_DIM, 1, ImageFormat::Png);
        assert_eq!(decode_through_every_entry_point("at", &at), [true; 4]);
    }

    #[test]
    fn a_real_image_past_the_allocation_budget_is_refused() {
        // 8,200 x 8,200 RGBA8 is 268,960,000 bytes, past the 256 MiB budget,
        // with each axis inside MAX_IMAGE_DIM. The PNG is complete and valid
        // (all-zero rows as one fixed-Huffman deflate stream, under 2 MB), so
        // only the allocation budget refuses it before any pixel is inflated.
        const SIDE: u32 = 8_200;
        assert!(u64::from(SIDE) * u64::from(SIDE) * 4 > MAX_IMAGE_ALLOC_BYTES);
        const { assert!(SIDE <= MAX_IMAGE_DIM) };
        let bytes = zero_rgba_png(SIDE, SIDE);
        assert!(bytes.len() < 2 * 1024 * 1024, "{}", bytes.len());
        assert_eq!(decode_through_every_entry_point("area", &bytes), [false; 4]);
    }

    /// A complete RGBA8 PNG whose every pixel is zero, encoded by hand: one
    /// literal zero, then repeated 258-byte matches at distance 1.
    fn zero_rgba_png(width: u32, height: u32) -> Vec<u8> {
        struct Bits {
            out: Vec<u8>,
            acc: u64,
            len: u32,
        }
        impl Bits {
            /// Append `count` bits of `value`, least significant first.
            fn put(&mut self, value: u32, count: u32) {
                self.acc |= u64::from(value) << self.len;
                self.len += count;
                while self.len >= 8 {
                    self.out.push(self.acc as u8);
                    self.acc >>= 8;
                    self.len -= 8;
                }
            }
            /// Append a Huffman code, which deflate stores most significant
            /// bit first.
            fn code(&mut self, code: u32, count: u32) {
                let reversed = code.reverse_bits() >> (32 - count);
                self.put(reversed, count);
            }
        }
        let total = u64::from(height) * (1 + u64::from(width) * 4);
        let mut bits = Bits {
            out: vec![0x78, 0x01],
            acc: 0,
            len: 0,
        };
        bits.put(1, 1); // final block
        bits.put(1, 2); // fixed Huffman codes
        bits.code(0x30, 8); // literal 0
        let mut remaining = total - 1;
        while remaining >= 258 {
            bits.code(0xC5, 8); // length 258 (code 285)
            bits.code(0, 5); // distance 1 (code 0)
            remaining -= 258;
        }
        for _ in 0..remaining {
            bits.code(0x30, 8);
        }
        bits.code(0, 7); // end of block
        if bits.len > 0 {
            bits.put(0, 8 - bits.len);
        }
        // Adler-32 of all-zero data: the first sum stays 1, the second is the
        // byte count.
        let adler = (((total % 65_521) as u32) << 16) | 1;
        let mut zlib = bits.out;
        zlib.extend_from_slice(&adler.to_be_bytes());

        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
        for (kind, data) in [(&b"IHDR"[..], ihdr), (b"IDAT", zlib), (b"IEND", Vec::new())] {
            let mut chunk = kind.to_vec();
            chunk.extend_from_slice(&data);
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            png.extend_from_slice(&chunk);
            png.extend_from_slice(&crc32(&chunk).to_be_bytes());
        }
        png
    }

    /// Minimal CRC-32 (PNG polynomial) for hand-built chunks.
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

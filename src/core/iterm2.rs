// SPDX-License-Identifier: GPL-3.0-only
//! iTerm2 inline images: `OSC 1337 ; File=<args> : <base64 payload>`.
//!
//! The iTerm2 inline-image extension rides in the OSC 1337 namespace rather
//! than in APC (Kitty) or DCS (Sixel). Its payload is one base64 blob of an
//! ordinary image *file* — the terminal decodes the container itself instead
//! of receiving raw pixels — followed by a small set of `key=value` arguments
//! that describe the requested on-screen size.
//!
//! # Wire form
//!
//! ```text
//! OSC 1337 ; File = inline=1 ; size=1234 ; width=40 ; height=20 : <base64> ST
//! ```
//!
//! Argument separators are semicolons, which the OSC parser already splits on,
//! so the dispatch arrives as several parameter slices. They are rejoined
//! verbatim here ([`joined_payload`]) before parsing, which also keeps the
//! `MAX_OSC_PARAMS` overflow tail (the final slot absorbs surplus separators)
//! working unchanged.
//!
//! # Size ceiling (parser-limit discipline)
//!
//! The payload lives inside the OSC accumulator, so a single OSC — arguments
//! and base64 together — is bounded by the parser's `MAX_OSC_RAW` (128 KiB).
//! Over-cap payload bytes are *dropped* by the accumulator and the OSC still
//! dispatches with its in-cap prefix, which for an image would mean handing a
//! silently truncated file to the decoder. That is refused: when the
//! accumulator is at its cap the whole command is rejected, no image is
//! stored, and nothing is drawn ([`payload_truncated`]). This mirrors the APC
//! rule that a corrupt partial image is worse than none.
//!
//! The practical ceiling is therefore ~96 KiB of *encoded file bytes* per
//! image (128 KiB of base64 minus the argument text). Larger images must use
//! the Kitty protocol, whose APC buffer is 1 MiB and which supports chunked
//! transmission. `docs/graphics.md` states this ceiling.
//!
//! ## Sibling payload paths
//!
//! Three other paths carry a bounded binary payload, and all of them already
//! refuse a truncated one:
//!
//! * APC (Kitty) marks overflow and drops the whole string.
//! * DCS (Sixel) marks `overflowed` on its capture and decodes nothing.
//! * OSC 52 (clipboard) rides the same accumulator as this path, but its
//!   64 KiB decode budget is smaller than what a cap-length OSC decodes to
//!   (~96 KiB), so an OSC 52 long enough to have been truncated is already
//!   rejected by that budget and needs no separate truncation check.
//!
//! # What is deliberately not supported
//!
//! `inline=0` in iTerm2 means "download this file to disk". OdyTTY never
//! writes files on behalf of a terminal escape sequence, so a non-inline
//! `File=` command is parsed and dropped — no file is created, no placement is
//! made. `MultipartFile=`/`FilePart=`/`FileEnd=` (the chunked download form)
//! are likewise unhandled and fall through to the namespace's
//! consume-without-state path.
//!
//! # Platform surface
//!
//! Platform-neutral: pure parser plus image-store work with no filesystem,
//! process, or environment access on any target. Windows behaves identically
//! to Unix.

use crate::graphics::{GraphicsProtocol, ImageScene, PlacementRequest};

use super::types::CellMetrics;

/// Longest `name=` value accepted, in encoded bytes. The name is advisory
/// metadata (iTerm2 shows it in its downloads UI); OdyTTY decodes it only far
/// enough to validate the argument, so a small bound is sufficient and keeps
/// an adversarial name from reserving memory.
const MAX_NAME_BYTES: usize = 1024;

/// Tolerated difference between a declared `size=` and the decoded payload
/// length. Base64 decoding is exact, so a correct emitter matches exactly;
/// three bytes absorbs the padding quantum for emitters that compute the
/// declared size from the encoded form. Anything beyond this is treated as a
/// corrupt or truncated transfer and the command is rejected whole.
const SIZE_SLACK_BYTES: usize = 3;

/// Maximum decoded dimension per axis for the container decode. Matches the
/// bound the rest of the app applies to untrusted image files: generous enough
/// for any real screenshot, small enough to refuse a dimension bomb. The
/// post-decode store cap still applies to the resulting RGBA buffer.
const MAX_IMAGE_DIM: u32 = 12_000;

/// Maximum total allocation the container decode may make. Bounds the
/// intermediate buffers a decompression bomb would otherwise demand *before*
/// the store's post-decode cap could see them.
const MAX_IMAGE_ALLOC_BYTES: u64 = 64 * 1024 * 1024;

/// A requested on-screen dimension. iTerm2 spells it four ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Dimension {
    /// `auto` or absent: derive from the image's natural pixel size.
    #[default]
    Auto,
    /// Bare integer: that many terminal cells.
    Cells(usize),
    /// `Npx`: that many pixels, converted to cells with the live cell metrics.
    Pixels(u32),
    /// `N%`: that percentage of the screen's width/height.
    Percent(u32),
}

/// Parsed `File=` arguments. Unknown keys are ignored (forward compatibility
/// with iTerm2 additions), but a malformed *value* on a known key rejects the
/// command rather than guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileArgs {
    /// `inline=1` is required for display. Anything else is a download
    /// request, which is never honored.
    pub(super) inline: bool,
    /// Declared byte length of the encoded file, cross-checked after decode.
    pub(super) size: Option<usize>,
    pub(super) width: Dimension,
    pub(super) height: Dimension,
    /// `preserveAspectRatio=0` disables aspect-preserving fit. Default on.
    pub(super) preserve_aspect_ratio: bool,
    /// Whether a syntactically valid `name=` was present. The value itself is
    /// advisory and not retained: OdyTTY has no downloads UI to show it in.
    pub(super) has_name: bool,
}

impl Default for FileArgs {
    fn default() -> Self {
        Self {
            inline: false,
            size: None,
            width: Dimension::Auto,
            height: Dimension::Auto,
            preserve_aspect_ratio: true,
            has_name: false,
        }
    }
}

/// Whether this OSC 1337 dispatch is a `File=` command. Checked before the
/// button parser so the two payload families never contend.
pub(super) fn is_file_command(parts: &[&[u8]]) -> bool {
    parts
        .first()
        .is_some_and(|first| first.starts_with(b"File="))
}

/// Rejoin the parameter slices the OSC parser split on `;` into the original
/// payload bytes. Lossless: the parser splits on a byte it does not otherwise
/// consume, and the final-slot overflow tail keeps its separators verbatim.
fn joined_payload(parts: &[&[u8]]) -> Vec<u8> {
    let total = parts.iter().map(|part| part.len()).sum::<usize>() + parts.len().saturating_sub(1);
    let mut out = Vec::with_capacity(total);
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            out.push(b';');
        }
        out.extend_from_slice(part);
    }
    out
}

/// Whether the OSC accumulator hit its cap on this dispatch, meaning the tail
/// of the payload was dropped and what arrived is a truncated prefix.
///
/// The accumulator holds the whole OSC body including the `1337` selector and
/// the separator after it, so the reconstructed length adds those five bytes
/// to the rejoined argument/payload bytes. At exactly the cap the payload is
/// assumed truncated: a legitimate command that lands precisely on the
/// boundary is rejected too, which is the safe direction — the alternative is
/// decoding a half-transferred file.
fn payload_truncated(joined_len: usize) -> bool {
    joined_len.saturating_add(b"1337;".len()) >= crate::parser::MAX_OSC_RAW
}

/// Split `File=<args>:<payload>` into its argument text and base64 payload.
/// The separator is the first colon, which cannot appear in the base64
/// alphabet or in any argument value (values are numbers, keywords, or base64
/// names), so the split is unambiguous.
fn split_args_and_payload(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = body.strip_prefix(b"File=")?;
    let colon = rest.iter().position(|&byte| byte == b':')?;
    Some((&rest[..colon], &rest[colon + 1..]))
}

/// Parse the semicolon-separated `key=value` argument text.
///
/// Returns `None` when a known key carries a malformed value, when a key is
/// repeated, or when an argument is not in `key=value` form. Unknown keys are
/// skipped so future iTerm2 arguments degrade to "ignored" rather than
/// "rejects the image".
fn parse_args(args: &[u8]) -> Option<FileArgs> {
    let mut out = FileArgs::default();
    let mut seen_inline = false;
    let mut seen_size = false;
    let mut seen_width = false;
    let mut seen_height = false;
    let mut seen_par = false;

    for field in args.split(|&byte| byte == b';') {
        if field.is_empty() {
            continue;
        }
        let equals = field.iter().position(|&byte| byte == b'=')?;
        let (key, value) = (&field[..equals], &field[equals + 1..]);
        match key {
            b"inline" => {
                if std::mem::replace(&mut seen_inline, true) {
                    return None;
                }
                out.inline = parse_flag(value)?;
            }
            b"size" => {
                if std::mem::replace(&mut seen_size, true) {
                    return None;
                }
                out.size = Some(parse_usize(value)?);
            }
            b"width" => {
                if std::mem::replace(&mut seen_width, true) {
                    return None;
                }
                out.width = parse_dimension(value)?;
            }
            b"height" => {
                if std::mem::replace(&mut seen_height, true) {
                    return None;
                }
                out.height = parse_dimension(value)?;
            }
            b"preserveAspectRatio" => {
                if std::mem::replace(&mut seen_par, true) {
                    return None;
                }
                out.preserve_aspect_ratio = parse_flag(value)?;
            }
            b"name" => {
                if value.len() > MAX_NAME_BYTES {
                    return None;
                }
                out.has_name = true;
            }
            // Unknown key: ignored on purpose (forward compatibility).
            _ => {}
        }
    }
    Some(out)
}

fn parse_flag(value: &[u8]) -> Option<bool> {
    match value {
        b"0" => Some(false),
        b"1" => Some(true),
        _ => None,
    }
}

fn parse_usize(value: &[u8]) -> Option<usize> {
    if value.is_empty() || value.len() > 20 || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(value).ok()?.parse::<usize>().ok()
}

/// Parse one dimension value: `auto`, `N`, `Npx`, or `N%`.
fn parse_dimension(value: &[u8]) -> Option<Dimension> {
    if value == b"auto" {
        return Some(Dimension::Auto);
    }
    if let Some(number) = value.strip_suffix(b"px") {
        return Some(Dimension::Pixels(u32::try_from(parse_usize(number)?).ok()?));
    }
    if let Some(number) = value.strip_suffix(b"%") {
        return Some(Dimension::Percent(
            u32::try_from(parse_usize(number)?).ok()?,
        ));
    }
    Some(Dimension::Cells(parse_usize(value)?))
}

/// Resolve one axis of the requested extent to a cell count, or `None` for
/// `auto` (the caller then derives it from the image's natural size or from
/// the other axis).
fn resolve_dimension(dimension: Dimension, cell_px: u32, screen_extent: usize) -> Option<usize> {
    let cell_px = cell_px.max(1) as u64;
    match dimension {
        Dimension::Auto => None,
        Dimension::Cells(cells) => Some(cells),
        Dimension::Pixels(px) => Some((px as u64).div_ceil(cell_px) as usize),
        Dimension::Percent(percent) => {
            // A percentage past 100 is not rejected (iTerm2 does not require
            // one in range); it is bounded here and clamped to the screen by
            // the caller.
            let cells = (screen_extent as u64)
                .saturating_mul(percent.min(1000) as u64)
                .div_ceil(100);
            Some(cells as usize)
        }
    }
}

/// Convert the image's natural pixel size to whole cells on one axis.
fn natural_cells(px: u32, cell_px: u32) -> usize {
    (px.max(1) as u64).div_ceil(cell_px.max(1) as u64) as usize
}

/// Compute the placement extent in cells from the parsed arguments and the
/// decoded image size.
///
/// Aspect handling follows iTerm2: with `preserveAspectRatio=1` (the default)
/// a single specified axis derives the other, and two specified axes define a
/// box the image is fitted *inside*. With it disabled, the specified values
/// are used as given and the image stretches. All arithmetic is 64-bit and
/// saturating — every input here is attacker-chosen.
fn extent_in_cells(
    args: &FileArgs,
    image_px: (u32, u32),
    screen: (usize, usize),
    cell_metrics: CellMetrics,
) -> (usize, usize) {
    let (px_w, px_h) = (image_px.0.max(1), image_px.1.max(1));
    let (screen_rows, screen_cols) = screen;
    let cell_w = cell_metrics.width_px.max(1);
    let cell_h = cell_metrics.height_px.max(1);

    let want_cols = resolve_dimension(args.width, cell_w, screen_cols);
    let want_rows = resolve_dimension(args.height, cell_h, screen_rows);

    let (cols, rows) = match (want_cols, want_rows, args.preserve_aspect_ratio) {
        (None, None, _) => (natural_cells(px_w, cell_w), natural_cells(px_h, cell_h)),
        (Some(cols), None, true) => (cols, derive_rows(cols, px_w, px_h, cell_w, cell_h)),
        (Some(cols), None, false) => (cols, natural_cells(px_h, cell_h)),
        (None, Some(rows), true) => (derive_cols(rows, px_w, px_h, cell_w, cell_h), rows),
        (None, Some(rows), false) => (natural_cells(px_w, cell_w), rows),
        (Some(cols), Some(rows), false) => (cols, rows),
        (Some(cols), Some(rows), true) => fit_inside(cols, rows, px_w, px_h, cell_w, cell_h),
    };

    (cols.max(1), rows.max(1))
}

/// Rows that keep the image's aspect ratio at a given column count.
fn derive_rows(cols: usize, px_w: u32, px_h: u32, cell_w: u32, cell_h: u32) -> usize {
    let target_px_w = (cols as u64).saturating_mul(cell_w as u64);
    let target_px_h = target_px_w
        .saturating_mul(px_h as u64)
        .div_ceil(px_w.max(1) as u64);
    target_px_h.div_ceil(cell_h.max(1) as u64).max(1) as usize
}

/// Columns that keep the image's aspect ratio at a given row count.
fn derive_cols(rows: usize, px_w: u32, px_h: u32, cell_w: u32, cell_h: u32) -> usize {
    let target_px_h = (rows as u64).saturating_mul(cell_h as u64);
    let target_px_w = target_px_h
        .saturating_mul(px_w as u64)
        .div_ceil(px_h.max(1) as u64);
    target_px_w.div_ceil(cell_w.max(1) as u64).max(1) as usize
}

/// Fit the image inside a cols x rows box without changing its aspect ratio:
/// take whichever axis binds first and derive the other from it.
fn fit_inside(
    cols: usize,
    rows: usize,
    px_w: u32,
    px_h: u32,
    cell_w: u32,
    cell_h: u32,
) -> (usize, usize) {
    let by_width = derive_rows(cols, px_w, px_h, cell_w, cell_h);
    if by_width <= rows {
        (cols, by_width)
    } else {
        (
            derive_cols(rows, px_w, px_h, cell_w, cell_h).min(cols),
            rows,
        )
    }
}

/// Decode the image container to tightly-packed RGBA8 plus dimensions.
///
/// The container is content-sniffed, never trusted from a filename, and the
/// decode itself is bounded by [`MAX_IMAGE_DIM`] / [`MAX_IMAGE_ALLOC_BYTES`]
/// so a decompression bomb is refused before it can allocate. Supported
/// containers are exactly the ones the crate's `image` features enable: PNG,
/// JPEG, and WebP.
fn decode_container(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = image::ImageReader::new(cursor).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIM);
    limits.max_image_height = Some(MAX_IMAGE_DIM);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    Some((decoded.into_raw(), width, height))
}

/// Handle one `File=` command. Returns the new cursor position when an image
/// was placed, or `None` when the command was rejected or drew nothing.
///
/// # Cursor semantics
///
/// The image is anchored at the cursor and the cursor then moves to column 0
/// of the row below the image, matching iTerm2's own behavior and OdyTTY's
/// existing Sixel default (DECSDM reset). Kitty's `C=1`-style "stay put"
/// variant has no iTerm2 spelling, so there is nothing to opt into.
///
/// # Bounds
///
/// The extent is clamped to the screen at this parse boundary: columns to what
/// remains to the right of the cursor, rows to the screen height. Every
/// downstream computation is saturating.
pub(super) fn handle_file_osc(
    graphics: &mut ImageScene,
    parts: &[&[u8]],
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
) -> Option<(usize, usize)> {
    let body = joined_payload(parts);
    if payload_truncated(body.len()) {
        return None;
    }
    let (args_bytes, payload) = split_args_and_payload(&body)?;
    let args = parse_args(args_bytes)?;
    if !args.inline {
        // A download request. OdyTTY never writes files for an escape
        // sequence, so this is consumed with no effect.
        return None;
    }

    let max_decoded = graphics.store().limits().max_decoded_bytes;
    // The encoded file is bounded by the OSC cap already; bound the decode
    // buffer by the store's own budget so an oversized transfer is refused
    // before it is materialized.
    let file_bytes = super::kitty::decode_base64_bytes(payload, max_decoded.max(1))?;
    if file_bytes.is_empty() {
        return None;
    }
    if let Some(declared) = args.size
        && declared.abs_diff(file_bytes.len()) > SIZE_SLACK_BYTES
    {
        return None;
    }

    let (rgba, width, height) = decode_container(&file_bytes)?;
    let insert = graphics.insert_rgba(None, width, height, rgba).ok()?;

    let (cols, rows) = extent_in_cells(
        &args,
        (width, height),
        (screen_rows, screen_cols),
        cell_metrics,
    );
    let display_columns = cols.min(screen_cols.saturating_sub(cursor_col)).max(1);
    let display_rows = rows.min(screen_rows.max(1)).max(1);

    graphics.place(PlacementRequest::new(
        insert.id,
        GraphicsProtocol::Iterm2,
        cursor_row,
        cursor_col,
        display_columns,
        display_rows,
    ))?;

    let new_row = cursor_row
        .saturating_add(display_rows)
        .min(screen_rows.saturating_sub(1));
    Some((new_row, 0))
}

#[cfg(test)]
pub(super) fn test_parse_args(args: &[u8]) -> Option<FileArgs> {
    parse_args(args)
}

#[cfg(test)]
pub(super) fn test_extent_in_cells(
    args: &FileArgs,
    image_px: (u32, u32),
    screen: (usize, usize),
    cell_metrics: CellMetrics,
) -> (usize, usize) {
    extent_in_cells(args, image_px, screen, cell_metrics)
}

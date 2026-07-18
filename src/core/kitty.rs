// SPDX-License-Identifier: GPL-3.0-only
//! Kitty graphics protocol: APC image transmit/display/delete/query.
//!
//! Supports direct raw RGB/RGBA (`f=24`/`f=32`), PNG (`f=100`), and
//! file-based transports (`t=f`, `t=t`, `t=s`) with security hardening.
//! PNG decode uses the `png` crate's still-image path: grayscale,
//! grayscale+alpha, RGB, RGBA, and indexed images are normalized to 8-bit RGBA;
//! 16-bit samples are stripped to 8-bit.
//!
//! File transports are security-restricted — see `kitty_transport` module docs.

use crate::graphics::{GraphicsProtocol, ImageScene, PlacementRequest};

use super::kitty_transport;
use super::types::CellMetrics;
use std::io::Cursor;

const MAX_PENDING_ENCODED_BYTES: usize = 96 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub(super) struct KittyState {
    pending: Option<PendingTransmission>,
}

#[derive(Debug, Clone)]
struct PendingTransmission {
    control: ControlData,
    encoded_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KittyOutcome {
    pub dirty: bool,
    pub cursor: Option<(usize, usize)>,
    pub response: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ControlData {
    pub(super) action: Option<char>,
    pub(super) format: Option<u32>,
    pub(super) transmission: Option<char>,
    pub(super) more_chunks: bool,
    pub(super) image_id: Option<u32>,
    pub(super) placement_id: Option<u32>,
    pub(super) width: Option<u32>,
    pub(super) height: Option<u32>,
    pub(super) display_columns: Option<usize>,
    pub(super) display_rows: Option<usize>,
    pub(super) cursor_movement: Option<u32>,
    pub(super) quiet: Option<u32>,
    /// Delete specifier (`d=`): a/A/i/I/c/C/p/P
    pub(super) delete_specifier: Option<char>,
    /// `x=` value. Kitty overloads this key by action: on a delete command
    /// (`a=d`, `d=p/P`) it is the target cell **column**; on a placement
    /// command (`a=p/T`) it is the source-rectangle left edge in **pixels**.
    /// Stored raw and interpreted at the use site.
    pub(super) x: Option<u32>,
    /// `y=` value. Like `x=`: cell **row** for deletes, source-rect top edge in
    /// **pixels** for placements.
    pub(super) y: Option<u32>,
    /// Source-rectangle width in pixels (`w=`), placement crop.
    pub(super) source_w: Option<u32>,
    /// Source-rectangle height in pixels (`h=`), placement crop.
    pub(super) source_h: Option<u32>,
    /// Pixel offset of the image within the anchor cell, x axis (`X=`).
    pub(super) offset_x: Option<i32>,
    /// Pixel offset of the image within the anchor cell, y axis (`Y=`).
    pub(super) offset_y: Option<i32>,
    /// Placement z-index (`z=`), signed; negative renders under text.
    pub(super) z_index: Option<i32>,
}

impl ControlData {
    fn response_prefix(&self) -> String {
        let mut parts = Vec::new();
        if let Some(id) = self.image_id {
            parts.push(format!("i={id}"));
        }
        if let Some(id) = self.placement_id {
            parts.push(format!("p={id}"));
        }
        parts.join(",")
    }

    /// NF9: per the kitty graphics spec, `q=1` suppresses SUCCESS (OK)
    /// responses and `q=2` suppresses both success and error responses. This
    /// method gates only OK responses, so any explicit quiet level (>= 1)
    /// suppresses; the error path checks `quiet >= 2` separately (C19).
    fn suppress_response(&self) -> bool {
        self.quiet.is_some_and(|quiet| quiet >= 1)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum KittyError {
    NotKitty,
    MalformedControl,
    UnsupportedAction,
    UnsupportedFormat,
    UnsupportedPngColor,
    UnsupportedTransmission,
    MissingDimensions,
    DimensionMismatch,
    InvalidPayload,
    PayloadTooLarge,
    StoreRejected,
    TransportFailed(&'static str),
}

impl KittyError {
    fn message(&self) -> &'static str {
        match self {
            KittyError::NotKitty => "not-kitty",
            KittyError::MalformedControl => "malformed-control",
            KittyError::UnsupportedAction => "unsupported-action",
            KittyError::UnsupportedFormat => "unsupported-format",
            KittyError::UnsupportedPngColor => "unsupported-png-color",
            KittyError::UnsupportedTransmission => "unsupported-transmission",
            KittyError::MissingDimensions => "missing-dimensions",
            KittyError::DimensionMismatch => "dimension-mismatch",
            KittyError::InvalidPayload => "invalid-payload",
            KittyError::PayloadTooLarge => "payload-too-large",
            KittyError::StoreRejected => "store-rejected",
            KittyError::TransportFailed(msg) => msg,
        }
    }
}

/// Handle one APC payload as delivered by OdyParser. Returns [`KittyError::NotKitty`]
/// when the APC is not a Kitty graphics command, so callers can preserve other
/// APC behavior.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_apc(
    state: &mut KittyState,
    graphics: &mut ImageScene,
    data: &[u8],
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
    named_transports_enabled: bool,
) -> Result<KittyOutcome, KittyError> {
    let command = parse_apc(data)?;
    if !graphics.record_kitty_apc(data) {
        return Err(KittyError::NotKitty);
    }

    // C19: q=2 suppresses ERROR responses as well as OK ones (kitty spec).
    // Capture the quiet level before `command` is consumed; for chunked
    // transmissions the level may live on the FIRST chunk's control data
    // (`state.pending`), since intermediate/final chunks usually carry only
    // `m=`. Must be read before the error arm clears `state.pending`.
    let suppress_error_response = command
        .control
        .quiet
        .or_else(|| {
            state
                .pending
                .as_ref()
                .and_then(|pending| pending.control.quiet)
        })
        .is_some_and(|quiet| quiet >= 2);

    let result = handle_command(
        state,
        graphics,
        command,
        cursor_row,
        cursor_col,
        screen_rows,
        screen_cols,
        cell_metrics,
        named_transports_enabled,
    );
    match result {
        Ok(outcome) => Ok(outcome),
        Err(err) => {
            state.pending = None;
            let response = if suppress_error_response {
                Vec::new()
            } else {
                kitty_response(None, err.message())
            };
            Ok(KittyOutcome {
                dirty: false,
                cursor: None,
                response,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_command(
    state: &mut KittyState,
    graphics: &mut ImageScene,
    command: Command,
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
    named_transports_enabled: bool,
) -> Result<KittyOutcome, KittyError> {
    if command.control.more_chunks {
        // C3: a chunked transmission is FIRST chunk (full control data, m=1),
        // then ANY NUMBER of intermediate chunks (m=1, payload only), then the
        // final chunk (m=0). The old code treated an existing pending
        // accumulation as malformed, capping every transmission at exactly two
        // chunks and rejecting real emitters (kitty icat, timg, term-image)
        // that split large images into many chunks. Intermediate chunks append
        // to the pending payload under the same MAX_PENDING_ENCODED_BYTES
        // budget the final-chunk merge enforces; their control keys (beyond
        // `m`) are ignored, matching kitty's protocol where only the first
        // chunk carries the transmission metadata.
        // NF9: chunk acks are SUCCESS responses, so `q>=1` suppresses them
        // too. For intermediate chunks (which usually carry only `m=`) the
        // quiet level lives on the FIRST chunk's control data in
        // `state.pending`, same as the C19 error-path lookup above.
        let (prefix, suppress_ok) = match state.pending.as_mut() {
            Some(pending) => {
                if pending
                    .encoded_payload
                    .len()
                    .saturating_add(command.payload.len())
                    > MAX_PENDING_ENCODED_BYTES
                {
                    return Err(KittyError::PayloadTooLarge);
                }
                pending.encoded_payload.extend_from_slice(&command.payload);
                (
                    pending.control.response_prefix(),
                    pending.control.suppress_response(),
                )
            }
            None => {
                if command.payload.len() > MAX_PENDING_ENCODED_BYTES {
                    return Err(KittyError::PayloadTooLarge);
                }
                let prefix = command.control.response_prefix();
                let suppress_ok = command.control.suppress_response();
                state.pending = Some(PendingTransmission {
                    control: command.control,
                    encoded_payload: command.payload,
                });
                (prefix, suppress_ok)
            }
        };
        return Ok(KittyOutcome {
            dirty: false,
            cursor: None,
            response: if suppress_ok {
                Vec::new()
            } else {
                kitty_response(Some(prefix), "OK")
            },
        });
    }

    let command = if let Some(mut pending) = state.pending.take() {
        if pending
            .encoded_payload
            .len()
            .saturating_add(command.payload.len())
            > MAX_PENDING_ENCODED_BYTES
        {
            return Err(KittyError::PayloadTooLarge);
        }
        pending.encoded_payload.extend_from_slice(&command.payload);
        let mut control = pending.control;
        control.more_chunks = false;
        merge_final_chunk_control(&mut control, command.control);
        Command {
            control,
            payload: pending.encoded_payload,
        }
    } else {
        command
    };

    // Dispatch by action type.
    match command.control.action {
        Some('d') => process_delete_command(graphics, &command.control, cursor_row, cursor_col),
        Some('q') => process_query_command(graphics, command, cell_metrics),
        Some('p') => process_display_command(
            graphics,
            command,
            cursor_row,
            cursor_col,
            screen_rows,
            screen_cols,
            cell_metrics,
        ),
        Some('f') | Some('a') => Err(KittyError::UnsupportedAction),
        _ => process_complete_command(
            graphics,
            command,
            cursor_row,
            cursor_col,
            (screen_rows, screen_cols),
            cell_metrics,
            named_transports_enabled,
        ),
    }
}

fn merge_final_chunk_control(base: &mut ControlData, final_chunk: ControlData) {
    base.quiet = final_chunk.quiet.or(base.quiet);
    base.placement_id = final_chunk.placement_id.or(base.placement_id);
    base.cursor_movement = final_chunk.cursor_movement.or(base.cursor_movement);
}

fn process_complete_command(
    graphics: &mut ImageScene,
    command: Command,
    cursor_row: usize,
    cursor_col: usize,
    screen_dims: (usize, usize),
    cell_metrics: CellMetrics,
    named_transports_enabled: bool,
) -> Result<KittyOutcome, KittyError> {
    validate_supported_control(&command.control)?;
    let max_decoded = graphics.store().limits().max_decoded_bytes;

    // Resolve the image payload depending on the transmission medium.
    let image_bytes = match command.control.transmission.unwrap_or('d') {
        'd' => {
            // Direct: payload IS the base64-encoded image data.
            decode_base64(&command.payload, max_decoded)?
        }
        'f' => {
            if !named_transports_enabled {
                return Err(KittyError::TransportFailed(
                    "EPERM:named-transport-disabled",
                ));
            }
            // File: payload is base64-encoded file path; read from fs.
            let path_bytes = decode_base64(&command.payload, 4096)?;
            kitty_transport::read_file_transport(&path_bytes, max_decoded)
                .map_err(|e| KittyError::TransportFailed(e.kitty_message()))?
        }
        't' => {
            if !named_transports_enabled {
                return Err(KittyError::TransportFailed(
                    "EPERM:named-transport-disabled",
                ));
            }
            // Temp file: like 'f' but delete after read.
            let path_bytes = decode_base64(&command.payload, 4096)?;
            kitty_transport::read_temp_transport(&path_bytes, max_decoded)
                .map_err(|e| KittyError::TransportFailed(e.kitty_message()))?
        }
        's' => {
            if !named_transports_enabled {
                return Err(KittyError::TransportFailed(
                    "EPERM:named-transport-disabled",
                ));
            }
            // Shared memory: payload is base64-encoded shm name.
            let name_bytes = decode_base64(&command.payload, 4096)?;
            let mut bytes = kitty_transport::read_shm_transport(&name_bytes, max_decoded)
                .map_err(|e| KittyError::TransportFailed(e.kitty_message()))?;
            // POSIX shm objects are rounded up to a page boundary on macOS, so
            // the mapped segment can be larger than the logical payload. For
            // fixed-size raw formats the exact length is known from the
            // dimensions — trim trailing padding to it so the strict length
            // check in `rgba_from_payload` still holds. PNG (self-delimiting)
            // and segments already at the exact size (every Linux segment) are
            // unaffected.
            if let Some(expected) = expected_raw_payload_len(&command.control) {
                if bytes.len() < expected {
                    return Err(KittyError::InvalidPayload);
                }
                bytes.truncate(expected);
            }
            bytes
        }
        _ => return Err(KittyError::UnsupportedTransmission),
    };

    let (rgba, width, height) = rgba_from_payload(&command.control, image_bytes, max_decoded)?;
    let insert = graphics
        .insert_rgba(command.control.image_id, width, height, rgba)
        .map_err(|_| KittyError::StoreRejected)?;

    let mut dirty = true;
    let mut cursor = None;
    if command.control.action == Some('T') {
        let (screen_rows, screen_cols) = screen_dims;
        let (placed, new_cursor) = place_image(
            graphics,
            &command.control,
            insert.id,
            width,
            height,
            cursor_row,
            cursor_col,
            screen_rows,
            screen_cols,
            cell_metrics,
        );
        dirty = placed;
        cursor = new_cursor;
    }

    let response = if command.control.suppress_response() {
        Vec::new()
    } else {
        kitty_response(Some(command.control.response_prefix()), "OK")
    };
    Ok(KittyOutcome {
        dirty,
        cursor,
        response,
    })
}

// ---------------------------------------------------------------------------
// Display previously transmitted image (a=p)
// ---------------------------------------------------------------------------

/// `a=p` — display an image already in the store, addressed by its protocol
/// image id (`i=`), without re-transmitting pixel data. Required for icat-style
/// reuse and for placing multiple named placements (`p=`) of one image.
fn process_display_command(
    graphics: &mut ImageScene,
    command: Command,
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
) -> Result<KittyOutcome, KittyError> {
    let protocol_id = command
        .control
        .image_id
        .ok_or(KittyError::MalformedControl)?;
    let stored_id = graphics
        .find_by_protocol_id(protocol_id)
        .ok_or(KittyError::StoreRejected)?;
    let (width, height) = graphics
        .store()
        .get(stored_id)
        .map(|image| (image.width, image.height))
        .ok_or(KittyError::StoreRejected)?;

    let (placed, cursor) = place_image(
        graphics,
        &command.control,
        stored_id,
        width,
        height,
        cursor_row,
        cursor_col,
        screen_rows,
        screen_cols,
        cell_metrics,
    );

    let response = if command.control.suppress_response() {
        Vec::new()
    } else {
        kitty_response(Some(command.control.response_prefix()), "OK")
    };
    Ok(KittyOutcome {
        dirty: placed,
        cursor,
        response,
    })
}

/// Build and apply a placement from the control data for a resolved stored
/// image. Returns whether the scene changed and the optional new cursor
/// position (when `C=1` is absent and the default cursor-advance applies).
#[allow(clippy::too_many_arguments)]
fn place_image(
    graphics: &mut ImageScene,
    control: &ControlData,
    stored_id: crate::graphics::StoredImageId,
    width: u32,
    height: u32,
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
) -> (bool, Option<(usize, usize)>) {
    // Placement path: x/y/w/h are the source crop rectangle in pixels. A zero
    // width/height means "use the rest of the image" (handled downstream).
    let source = crate::graphics::SourceRect {
        x: control.x.unwrap_or(0),
        y: control.y.unwrap_or(0),
        width: control.source_w.unwrap_or(0),
        height: control.source_h.unwrap_or(0),
    };
    // Default display extent derives from the visible source region when a crop
    // is set, otherwise the full image.
    let effective_w = control.source_w.unwrap_or(width).min(width).max(1);
    let effective_h = control.source_h.unwrap_or(height).min(height).max(1);
    let display_columns =
        display_columns(control, effective_w, cursor_col, screen_cols, cell_metrics);
    let display_rows = display_rows(control, effective_h, cursor_row, screen_rows, cell_metrics);

    let request = PlacementRequest::new(
        stored_id,
        GraphicsProtocol::Kitty,
        cursor_row,
        cursor_col,
        display_columns,
        display_rows,
    )
    .with_source(source)
    .with_pixel_offset(control.offset_x.unwrap_or(0), control.offset_y.unwrap_or(0))
    .with_z_index(control.z_index.unwrap_or(0))
    .with_protocol_ids(control.image_id, control.placement_id);

    let placed = graphics.place(request).is_some();
    let cursor = if control.cursor_movement == Some(1) {
        let row = cursor_row
            .saturating_add(display_rows)
            .min(screen_rows.saturating_sub(1));
        Some((row, 0))
    } else {
        None
    };
    (placed, cursor)
}

// ---------------------------------------------------------------------------
// Delete (a=d)
// ---------------------------------------------------------------------------

fn process_delete_command(
    graphics: &mut ImageScene,
    control: &ControlData,
    cursor_row: usize,
    cursor_col: usize,
) -> Result<KittyOutcome, KittyError> {
    let spec = control.delete_specifier.unwrap_or('a');
    match spec {
        'a' => graphics.delete_all_placements(),
        'A' => graphics.delete_all_placements_and_free(),
        'i' => {
            let id = control.image_id.ok_or(KittyError::MalformedControl)?;
            graphics.delete_by_image_id(id, control.placement_id);
        }
        'I' => {
            let id = control.image_id.ok_or(KittyError::MalformedControl)?;
            graphics.delete_by_image_id_and_free(id, control.placement_id);
        }
        'c' => graphics.delete_at_cursor(cursor_row, cursor_col, false),
        'C' => graphics.delete_at_cursor(cursor_row, cursor_col, true),
        'p' => {
            // Delete path: x/y are cell coordinates (column/row), not pixels.
            let col = control.x.map(|v| v as usize).unwrap_or(cursor_col);
            let row = control.y.map(|v| v as usize).unwrap_or(cursor_row);
            graphics.delete_at_position(row, col, false);
        }
        'P' => {
            // Delete path: x/y are cell coordinates (column/row), not pixels.
            let col = control.x.map(|v| v as usize).unwrap_or(cursor_col);
            let row = control.y.map(|v| v as usize).unwrap_or(cursor_row);
            graphics.delete_at_position(row, col, true);
        }
        _ => return Err(KittyError::UnsupportedAction),
    }
    let response = if control.suppress_response() {
        Vec::new()
    } else {
        kitty_response(Some(control.response_prefix()), "OK")
    };
    Ok(KittyOutcome {
        dirty: true,
        cursor: None,
        response,
    })
}

// ---------------------------------------------------------------------------
// Query (a=q)
// ---------------------------------------------------------------------------

fn process_query_command(
    graphics: &mut ImageScene,
    command: Command,
    _cell_metrics: CellMetrics,
) -> Result<KittyOutcome, KittyError> {
    // Validate the image would be accepted without storing it.
    validate_supported_control(&command.control)?;
    let max_decoded = graphics.store().limits().max_decoded_bytes;
    let decoded = decode_base64(&command.payload, max_decoded)?;
    // Validate pixel dimensions / format match.
    let _validated = rgba_from_payload(&command.control, decoded, max_decoded)?;

    let response = if command.control.suppress_response() {
        Vec::new()
    } else {
        kitty_response(Some(command.control.response_prefix()), "OK")
    };
    Ok(KittyOutcome {
        dirty: false,
        cursor: None,
        response,
    })
}

fn validate_supported_control(control: &ControlData) -> Result<(), KittyError> {
    match control.action.unwrap_or('t') {
        't' | 'T' | 'q' => {}
        _ => return Err(KittyError::UnsupportedAction),
    }
    match control.format {
        Some(24 | 32 | 100) => {}
        _ => return Err(KittyError::UnsupportedFormat),
    }
    match control.transmission.unwrap_or('d') {
        'd' | 'f' | 't' | 's' => {}
        _ => return Err(KittyError::UnsupportedTransmission),
    }
    // Raw pixel formats require explicit dimensions; PNG and file transports
    // derive dimensions from the payload/file content.
    if matches!(control.format, Some(24 | 32))
        && matches!(control.transmission.unwrap_or('d'), 'd')
        && (control.width.is_none() || control.height.is_none())
    {
        return Err(KittyError::MissingDimensions);
    }
    Ok(())
}

fn display_columns(
    control: &ControlData,
    width: u32,
    cursor_col: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
) -> usize {
    let requested = control
        .display_columns
        .unwrap_or_else(|| width.div_ceil(cell_metrics.width_px) as usize);
    requested.min(screen_cols.saturating_sub(cursor_col)).max(1)
}

fn display_rows(
    control: &ControlData,
    height: u32,
    cursor_row: usize,
    screen_rows: usize,
    cell_metrics: CellMetrics,
) -> usize {
    // Clamped to the rows below the cursor, mirroring `display_columns`: an
    // attacker-chosen `r=` parameter (or an extreme pixel height) must not
    // produce a placement extent that overflows downstream signed row
    // arithmetic or dwarfs the screen.
    let requested = control
        .display_rows
        .unwrap_or_else(|| height.div_ceil(cell_metrics.height_px) as usize);
    requested.min(screen_rows.saturating_sub(cursor_row)).max(1)
}

/// Exact byte length of a fixed-size raw payload (`f=24` / `f=32`) given the
/// declared dimensions, or `None` for variable-size formats (e.g. PNG) or when
/// dimensions are absent. Used to trim page-padding off shared-memory segments
/// (see the `t=s` arm of [`process_complete_command`]).
fn expected_raw_payload_len(control: &ControlData) -> Option<usize> {
    let bpp = match control.format {
        Some(32) => 4,
        Some(24) => 3,
        _ => return None,
    };
    let pixels = (control.width? as usize).checked_mul(control.height? as usize)?;
    pixels.checked_mul(bpp)
}

fn rgba_from_payload(
    control: &ControlData,
    decoded: Vec<u8>,
    max_decoded: usize,
) -> Result<(Vec<u8>, u32, u32), KittyError> {
    match control.format {
        Some(32) => {
            let width = control.width.ok_or(KittyError::MissingDimensions)?;
            let height = control.height.ok_or(KittyError::MissingDimensions)?;
            // `pixel_count` alone is not enough: u32::MAX² still fits u64, so
            // the byte-size multiply must be checked too (an unchecked
            // `pixels * 4` panics in debug builds and wraps in release,
            // spoofing the length check for attacker-chosen dimensions).
            let pixels = pixel_count(width, height)?;
            let expected = pixels.checked_mul(4).ok_or(KittyError::PayloadTooLarge)?;
            if decoded.len() != expected {
                return Err(KittyError::InvalidPayload);
            }
            Ok((decoded, width, height))
        }
        Some(24) => {
            let width = control.width.ok_or(KittyError::MissingDimensions)?;
            let height = control.height.ok_or(KittyError::MissingDimensions)?;
            let pixels = pixel_count(width, height)?;
            let expected = pixels.checked_mul(3).ok_or(KittyError::PayloadTooLarge)?;
            if decoded.len() != expected {
                return Err(KittyError::InvalidPayload);
            }
            let capacity = pixels.checked_mul(4).ok_or(KittyError::PayloadTooLarge)?;
            let mut rgba = Vec::with_capacity(capacity);
            for rgb in decoded.chunks_exact(3) {
                rgba.extend_from_slice(rgb);
                rgba.push(255);
            }
            Ok((rgba, width, height))
        }
        Some(100) => rgba_from_png(control, &decoded, max_decoded),
        _ => Err(KittyError::UnsupportedFormat),
    }
}

fn rgba_from_png(
    control: &ControlData,
    decoded: &[u8],
    max_decoded: usize,
) -> Result<(Vec<u8>, u32, u32), KittyError> {
    let mut decoder = png::Decoder::new(Cursor::new(decoded));
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );

    let header = decoder
        .read_header_info()
        .map_err(|_| KittyError::InvalidPayload)?;
    let width = header.width;
    let height = header.height;
    validate_png_header(
        control,
        header.color_type,
        header.bit_depth,
        width,
        height,
        max_decoded,
    )?;

    let mut reader = decoder
        .read_info()
        .map_err(|_| KittyError::InvalidPayload)?;
    let output_size = reader
        .output_buffer_size()
        .ok_or(KittyError::PayloadTooLarge)?;
    if output_size > max_decoded {
        return Err(KittyError::PayloadTooLarge);
    }

    let mut buf = vec![0; output_size];
    let output = reader
        .next_frame(&mut buf)
        .map_err(|_| KittyError::InvalidPayload)?;
    let frame_bytes = &buf[..output.buffer_size()];
    let rgba = png_frame_to_rgba(output.color_type, frame_bytes)?;
    if rgba.len() > max_decoded {
        return Err(KittyError::PayloadTooLarge);
    }
    Ok((rgba, output.width, output.height))
}

fn validate_png_header(
    control: &ControlData,
    color_type: png::ColorType,
    bit_depth: png::BitDepth,
    width: u32,
    height: u32,
    max_decoded: usize,
) -> Result<(), KittyError> {
    match color_type {
        png::ColorType::Grayscale
        | png::ColorType::Rgb
        | png::ColorType::Indexed
        | png::ColorType::GrayscaleAlpha
        | png::ColorType::Rgba => {}
    }
    match bit_depth {
        png::BitDepth::One
        | png::BitDepth::Two
        | png::BitDepth::Four
        | png::BitDepth::Eight
        | png::BitDepth::Sixteen => {}
    }
    if width == 0 || height == 0 {
        return Err(KittyError::InvalidPayload);
    }
    if control.width.is_some_and(|expected| expected != width)
        || control.height.is_some_and(|expected| expected != height)
    {
        return Err(KittyError::DimensionMismatch);
    }
    let pixels = pixel_count(width, height)?;
    if pixels
        .checked_mul(4)
        .is_none_or(|bytes| bytes > max_decoded)
    {
        return Err(KittyError::PayloadTooLarge);
    }
    Ok(())
}

fn png_frame_to_rgba(color_type: png::ColorType, bytes: &[u8]) -> Result<Vec<u8>, KittyError> {
    match color_type {
        png::ColorType::Rgba => Ok(bytes.to_vec()),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(bytes.len() / 3 * 4);
            for rgb in bytes.chunks_exact(3) {
                rgba.extend_from_slice(rgb);
                rgba.push(255);
            }
            if bytes.len().is_multiple_of(3) {
                Ok(rgba)
            } else {
                Err(KittyError::InvalidPayload)
            }
        }
        png::ColorType::Grayscale => Ok(bytes
            .iter()
            .flat_map(|gray| [*gray, *gray, *gray, 255])
            .collect()),
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(bytes.len() / 2 * 4);
            for gray_alpha in bytes.chunks_exact(2) {
                let gray = gray_alpha[0];
                rgba.extend_from_slice(&[gray, gray, gray, gray_alpha[1]]);
            }
            if bytes.len().is_multiple_of(2) {
                Ok(rgba)
            } else {
                Err(KittyError::InvalidPayload)
            }
        }
        png::ColorType::Indexed => Err(KittyError::UnsupportedPngColor),
    }
}

fn pixel_count(width: u32, height: u32) -> Result<usize, KittyError> {
    (width as usize)
        .checked_mul(height as usize)
        .ok_or(KittyError::PayloadTooLarge)
}

fn kitty_response(control: Option<String>, status: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b_G");
    if let Some(control) = control
        && !control.is_empty()
    {
        out.extend_from_slice(control.as_bytes());
    }
    out.push(b';');
    out.extend_from_slice(status.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Command {
    control: ControlData,
    payload: Vec<u8>,
}

fn parse_apc(data: &[u8]) -> Result<Command, KittyError> {
    let rest = data.strip_prefix(b"G").ok_or(KittyError::NotKitty)?;
    let (control, payload) = match rest.iter().position(|byte| *byte == b';') {
        Some(index) => (&rest[..index], &rest[index + 1..]),
        None => (rest, &[][..]),
    };
    Ok(Command {
        control: parse_control(control)?,
        payload: payload.to_vec(),
    })
}

fn parse_control(control: &[u8]) -> Result<ControlData, KittyError> {
    let mut parsed = ControlData::default();
    if control.is_empty() {
        return Ok(parsed);
    }
    for part in control.split(|byte| *byte == b',') {
        if part.is_empty() {
            continue;
        }
        let Some(eq) = part.iter().position(|byte| *byte == b'=') else {
            return Err(KittyError::MalformedControl);
        };
        let key = std::str::from_utf8(&part[..eq]).map_err(|_| KittyError::MalformedControl)?;
        let value =
            std::str::from_utf8(&part[eq + 1..]).map_err(|_| KittyError::MalformedControl)?;
        match key {
            "a" => parsed.action = parse_char(value),
            "f" => parsed.format = parse_u32(value),
            "t" => parsed.transmission = parse_char(value),
            "m" => parsed.more_chunks = parse_u32(value).unwrap_or(0) != 0,
            "i" => parsed.image_id = parse_u32(value),
            "p" => parsed.placement_id = parse_u32(value),
            "s" => parsed.width = parse_u32(value),
            "v" => parsed.height = parse_u32(value),
            "c" => parsed.display_columns = parse_usize(value),
            "r" => parsed.display_rows = parse_usize(value),
            "C" => parsed.cursor_movement = parse_u32(value),
            "q" => parsed.quiet = parse_u32(value),
            "d" => parsed.delete_specifier = parse_char(value),
            "x" => parsed.x = parse_u32(value),
            "y" => parsed.y = parse_u32(value),
            "w" => parsed.source_w = parse_u32(value),
            "h" => parsed.source_h = parse_u32(value),
            "X" => parsed.offset_x = parse_i32(value),
            "Y" => parsed.offset_y = parse_i32(value),
            "z" => parsed.z_index = parse_i32(value),
            _ => {}
        }
    }
    Ok(parsed)
}

fn parse_u32(value: &str) -> Option<u32> {
    value.parse().ok()
}

fn parse_i32(value: &str) -> Option<i32> {
    value.parse().ok()
}

fn parse_usize(value: &str) -> Option<usize> {
    value.parse().ok()
}

fn parse_char(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn decode_base64(input: &[u8], max_decoded: usize) -> Result<Vec<u8>, KittyError> {
    let mut out = Vec::with_capacity(input.len().saturating_mul(3) / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u8;
    let mut padding = 0u8;

    for &byte in input {
        if byte == b'=' {
            padding = padding.saturating_add(1);
            if padding > 2 {
                return Err(KittyError::InvalidPayload);
            }
            continue;
        }
        if padding > 0 {
            return Err(KittyError::InvalidPayload);
        }
        let value = base64_value(byte).ok_or(KittyError::InvalidPayload)? as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        while bits >= 8 {
            bits -= 8;
            out.push(((accumulator >> bits) & 0xff) as u8);
            if out.len() > max_decoded {
                return Err(KittyError::PayloadTooLarge);
            }
            accumulator &= (1u32 << bits) - 1;
        }
    }

    Ok(out)
}

pub(crate) fn decode_base64_bytes(input: &[u8], max_decoded: usize) -> Option<Vec<u8>> {
    decode_base64(input, max_decoded).ok()
}

pub(crate) fn encode_base64_bytes(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;

        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() >= 2 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() == 3 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn test_parse_apc(data: &[u8]) -> Result<(ControlData, Vec<u8>), &'static str> {
    parse_apc(data)
        .map(|command| (command.control, command.payload))
        .map_err(|err| err.message())
}

#[cfg(test)]
pub(super) fn test_decode_base64(
    input: &[u8],
    max_decoded: usize,
) -> Result<Vec<u8>, &'static str> {
    decode_base64(input, max_decoded).map_err(|err| err.message())
}

/// C3 test seam: drive `handle_command` with an intermediate (`m=1`) chunk of
/// `chunk_len` bytes appended onto a pending accumulation of `pending_len`
/// bytes, without round-tripping ~100 MB of base64 through the parser. Returns
/// the error message, or `None` when the append is accepted.
#[cfg(test)]
pub(super) fn test_intermediate_chunk_append(
    pending_len: usize,
    chunk_len: usize,
) -> Option<&'static str> {
    let mut state = KittyState {
        pending: Some(PendingTransmission {
            control: ControlData::default(),
            encoded_payload: vec![b'A'; pending_len],
        }),
    };
    let mut graphics = ImageScene::default();
    let command = Command {
        control: ControlData {
            more_chunks: true,
            ..ControlData::default()
        },
        payload: vec![b'A'; chunk_len],
    };
    handle_command(
        &mut state,
        &mut graphics,
        command,
        0,
        0,
        4,
        20,
        CellMetrics::default(),
        false,
    )
    .err()
    .map(|err| err.message())
}

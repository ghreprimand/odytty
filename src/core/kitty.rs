//! Kitty graphics protocol MVP: APC direct still-image transmit/display.
//!
//! This module deliberately implements only the direct raw RGB/RGBA path used by
//! the Stage 6 graphics ladder. Deferred surfaces are explicit: PNG (`f=100`)
//! needs a decoder dependency decision, and file/shared-memory transmission
//! needs a security packet before it can be accepted.

use crate::graphics::{GraphicsProtocol, ImageScene, PlacementRequest};

use super::types::CellMetrics;

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

    fn suppress_response(&self) -> bool {
        self.quiet == Some(2)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum KittyError {
    NotKitty,
    MalformedControl,
    UnsupportedAction,
    UnsupportedFormat,
    UnsupportedTransmission,
    MissingDimensions,
    InvalidPayload,
    PayloadTooLarge,
    StoreRejected,
}

impl KittyError {
    fn message(&self) -> &'static str {
        match self {
            KittyError::NotKitty => "not-kitty",
            KittyError::MalformedControl => "malformed-control",
            KittyError::UnsupportedAction => "unsupported-action",
            KittyError::UnsupportedFormat => "unsupported-format",
            KittyError::UnsupportedTransmission => "unsupported-transmission",
            KittyError::MissingDimensions => "missing-dimensions",
            KittyError::InvalidPayload => "invalid-payload",
            KittyError::PayloadTooLarge => "payload-too-large",
            KittyError::StoreRejected => "store-rejected",
        }
    }
}

/// Handle one APC payload as delivered by OdyParser. Returns [`KittyError::NotKitty`]
/// when the APC is not a Kitty graphics command, so callers can preserve other
/// APC behavior.
pub(super) fn handle_apc(
    state: &mut KittyState,
    graphics: &mut ImageScene,
    data: &[u8],
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
) -> Result<KittyOutcome, KittyError> {
    let command = parse_apc(data)?;
    if !graphics.record_kitty_apc(data) {
        return Err(KittyError::NotKitty);
    }

    let result = handle_command(
        state,
        graphics,
        command,
        cursor_row,
        cursor_col,
        screen_rows,
        screen_cols,
        cell_metrics,
    );
    match result {
        Ok(outcome) => Ok(outcome),
        Err(err) => {
            state.pending = None;
            let response = kitty_response(None, err.message());
            Ok(KittyOutcome {
                dirty: false,
                cursor: None,
                response,
            })
        }
    }
}

fn handle_command(
    state: &mut KittyState,
    graphics: &mut ImageScene,
    command: Command,
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
) -> Result<KittyOutcome, KittyError> {
    if command.control.more_chunks {
        if state.pending.is_some() {
            return Err(KittyError::MalformedControl);
        }
        if command.payload.len() > MAX_PENDING_ENCODED_BYTES {
            return Err(KittyError::PayloadTooLarge);
        }
        let prefix = command.control.response_prefix();
        state.pending = Some(PendingTransmission {
            control: command.control,
            encoded_payload: command.payload,
        });
        return Ok(KittyOutcome {
            dirty: false,
            cursor: None,
            response: kitty_response(Some(prefix), "OK"),
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

    process_complete_command(
        graphics,
        command,
        cursor_row,
        cursor_col,
        screen_rows,
        screen_cols,
        cell_metrics,
    )
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
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
) -> Result<KittyOutcome, KittyError> {
    validate_supported_control(&command.control)?;
    let max_decoded = graphics.store().limits().max_decoded_bytes;
    let decoded = decode_base64(&command.payload, max_decoded)?;
    let (rgba, width, height) = rgba_from_payload(&command.control, decoded)?;
    let insert = graphics
        .insert_rgba(command.control.image_id, width, height, rgba)
        .map_err(|_| KittyError::StoreRejected)?;

    let mut dirty = true;
    let mut cursor = None;
    if command.control.action == Some('T') {
        let display_columns = display_columns(
            &command.control,
            width,
            cursor_col,
            screen_cols,
            cell_metrics,
        );
        let display_rows = display_rows(&command.control, height, cell_metrics);
        let placed = graphics.place(PlacementRequest::new(
            insert.id,
            GraphicsProtocol::Kitty,
            cursor_row,
            cursor_col,
            display_columns,
            display_rows,
        ));
        dirty = placed.is_some();
        if command.control.cursor_movement == Some(1) {
            let row = cursor_row
                .saturating_add(display_rows)
                .min(screen_rows.saturating_sub(1));
            cursor = Some((row, 0));
        }
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

fn validate_supported_control(control: &ControlData) -> Result<(), KittyError> {
    match control.action.unwrap_or('t') {
        't' | 'T' => {}
        _ => return Err(KittyError::UnsupportedAction),
    }
    match control.format {
        Some(24 | 32) => {}
        Some(100) => return Err(KittyError::UnsupportedFormat),
        _ => return Err(KittyError::UnsupportedFormat),
    }
    match control.transmission.unwrap_or('d') {
        'd' => {}
        _ => return Err(KittyError::UnsupportedTransmission),
    }
    if control.width.is_none() || control.height.is_none() {
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
        .unwrap_or_else(|| ((width + cell_metrics.width_px - 1) / cell_metrics.width_px) as usize);
    requested.min(screen_cols.saturating_sub(cursor_col)).max(1)
}

fn display_rows(control: &ControlData, height: u32, cell_metrics: CellMetrics) -> usize {
    control
        .display_rows
        .unwrap_or_else(|| {
            ((height + cell_metrics.height_px - 1) / cell_metrics.height_px) as usize
        })
        .max(1)
}

fn rgba_from_payload(
    control: &ControlData,
    decoded: Vec<u8>,
) -> Result<(Vec<u8>, u32, u32), KittyError> {
    let width = control.width.ok_or(KittyError::MissingDimensions)?;
    let height = control.height.ok_or(KittyError::MissingDimensions)?;
    let pixels = width as usize * height as usize;
    match control.format {
        Some(32) => {
            if decoded.len() != pixels * 4 {
                return Err(KittyError::InvalidPayload);
            }
            Ok((decoded, width, height))
        }
        Some(24) => {
            if decoded.len() != pixels * 3 {
                return Err(KittyError::InvalidPayload);
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for rgb in decoded.chunks_exact(3) {
                rgba.extend_from_slice(rgb);
                rgba.push(255);
            }
            Ok((rgba, width, height))
        }
        _ => Err(KittyError::UnsupportedFormat),
    }
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
            _ => {}
        }
    }
    Ok(parsed)
}

fn parse_u32(value: &str) -> Option<u32> {
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

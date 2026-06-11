//! DCS/APC graphics routing: captures DCS sixel payloads, decodes them via
//! the SX1 Sixel decoder, and wires decoded images into the graphics scene
//! as cell-anchored placements.
//!
//! Extracted from [`super::screen`] per the modularity directive (screen.rs was
//! approaching the 2000-line threshold). The DCS state machine (hook/put/unhook)
//! and sixel decode→place pipeline live here; screen.rs retains thin forwarding
//! methods.
//!
//! # Cursor-below-image policy
//!
//! After a successful sixel decode, the cursor moves to the row immediately
//! below the image's last sixel band, column 0 — matching xterm's default
//! behavior (DECSDM off). DECSDM (scrolling mode) is noted as a finding but
//! not implemented; virtually all modern sixel usage assumes DECSDM-off.
//!
//! # Cell-extent provisioning
//!
//! The G2.1 placement API requires cell-extent (`display_columns`,
//! `display_rows`). The core has no access to actual pixel cell metrics (those
//! live in the render layer), so we use a **provisional cell-size assumption**:
//! 8×16 pixels. The render layer may override placement extent when uploading
//! to GPU with actual glyph metrics; this is documented as the expected path.
//! The constant is [`PROVISIONAL_CELL_WIDTH`] / [`PROVISIONAL_CELL_HEIGHT`].

use crate::graphics::placement::MAX_RAW_GRAPHICS_BYTES;
use crate::graphics::sixel::{SixelBackground, decode_sixel};
use crate::graphics::{GraphicsProtocol, ImageScene, PlacementRequest};
use crate::parser::Params;

/// Provisional cell width in pixels for cell-extent calculation.
/// The render layer should override with actual glyph metrics.
pub(super) const PROVISIONAL_CELL_WIDTH: u32 = 8;

/// Provisional cell height in pixels for cell-extent calculation.
pub(super) const PROVISIONAL_CELL_HEIGHT: u32 = 16;

/// In-progress DCS capture accumulator.
#[derive(Debug, Clone)]
pub(super) struct DcsCapture {
    pub body: Vec<u8>,
    pub payload_start: usize,
    pub p2: Option<u16>,
    pub overflowed: bool,
}

/// Tracks decode failures for debug/diagnostic access.
#[derive(Debug, Clone, Default)]
pub(super) struct GraphicsStats {
    pub sixel_decode_errors: u64,
}

// ---------------------------------------------------------------------------
// DCS state machine helpers (extracted from screen.rs)
// ---------------------------------------------------------------------------

/// Begin a DCS capture if this is a sixel sequence (`q` final byte,
/// no intermediates, not ignored).
pub(super) fn dcs_hook(
    params: &Params,
    intermediates: &[u8],
    ignore: bool,
    action: char,
) -> Option<DcsCapture> {
    if ignore || !intermediates.is_empty() || action != 'q' {
        return None;
    }
    let mut body = serialize_dcs_params(params);
    let p2 = dcs_param(params, 1);
    body.push(action as u8);
    let payload_start = body.len();
    Some(DcsCapture {
        body,
        payload_start,
        p2,
        overflowed: false,
    })
}

/// Append a byte to the in-progress DCS capture, respecting the size cap.
pub(super) fn dcs_put(capture: &mut DcsCapture, byte: u8) {
    if capture.body.len() < MAX_RAW_GRAPHICS_BYTES {
        capture.body.push(byte);
    } else {
        capture.overflowed = true;
    }
}

/// Finalize a DCS capture: record the raw command in the graphics scene,
/// then attempt sixel decode + placement. Returns `true` if the scene was
/// mutated (caller should mark dirty). On success, returns the new cursor
/// position `(row, col)` for the caller to apply.
///
/// Cursor-below-image policy (xterm DECSDM-off default): cursor moves to
/// the row immediately below the image, column 0.
///
/// Decode errors never disturb terminal state — the payload is dropped and
/// the error is counted in `stats`.
pub(super) fn dcs_unhook(
    capture: DcsCapture,
    graphics: &mut ImageScene,
    stats: &mut GraphicsStats,
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
) -> Option<(usize, usize)> {
    if capture.overflowed {
        return None;
    }

    // Record the raw command (existing G2.1 behavior).
    if !graphics.record_sixel_dcs(&capture.body, capture.payload_start, capture.p2) {
        return None;
    }

    // Decode the sixel payload.
    let payload = &capture.body[capture.payload_start..];
    let background = match capture.p2 {
        Some(p2) => SixelBackground::from_p2(p2),
        None => SixelBackground::Opaque,
    };
    let image = match decode_sixel(payload, background) {
        Ok(img) => img,
        Err(_e) => {
            stats.sixel_decode_errors += 1;
            return Some((cursor_row, cursor_col));
        }
    };

    // Insert decoded RGBA into the image store.
    let insert = match graphics.insert_rgba(None, image.width, image.height, image.rgba) {
        Ok(ins) => ins,
        Err(_e) => {
            stats.sixel_decode_errors += 1;
            return Some((cursor_row, cursor_col));
        }
    };

    // Compute cell extent from pixel dimensions using provisional cell metrics.
    let display_columns =
        ((image.width + PROVISIONAL_CELL_WIDTH - 1) / PROVISIONAL_CELL_WIDTH) as usize;
    let display_rows =
        ((image.height + PROVISIONAL_CELL_HEIGHT - 1) / PROVISIONAL_CELL_HEIGHT) as usize;

    // Clamp extent to screen bounds from anchor.
    let display_columns = display_columns
        .min(screen_cols.saturating_sub(cursor_col))
        .max(1);
    let display_rows = display_rows.max(1);

    // Place at cursor position.
    graphics.place(PlacementRequest::new(
        insert.id,
        GraphicsProtocol::Sixel,
        cursor_row,
        cursor_col,
        display_columns,
        display_rows,
    ));

    // Cursor-below-image: row immediately below the image, column 0.
    let image_bottom_row = cursor_row + display_rows;
    let new_row = image_bottom_row.min(screen_rows.saturating_sub(1));
    Some((new_row, 0))
}

/// Handle an APC payload. Returns `true` if the graphics scene was mutated.
pub(super) fn apc_dispatch(graphics: &mut ImageScene, data: &[u8]) -> bool {
    graphics.record_kitty_apc(data)
}

// ---------------------------------------------------------------------------
// DCS parameter serialization (extracted from screen.rs)
// ---------------------------------------------------------------------------

pub(super) fn serialize_dcs_params(params: &Params) -> Vec<u8> {
    let groups: Vec<&[u16]> = params.iter().collect();
    if groups.is_empty() || (groups.len() == 1 && groups[0] == [0]) {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        if group_index > 0 {
            out.push(b';');
        }
        for (sub_index, value) in group.iter().enumerate() {
            if sub_index > 0 {
                out.push(b':');
            }
            out.extend_from_slice(value.to_string().as_bytes());
        }
    }
    out
}

pub(super) fn dcs_param(params: &Params, index: usize) -> Option<u16> {
    let groups: Vec<&[u16]> = params.iter().collect();
    if groups.len() == 1 && groups[0] == [0] {
        return None;
    }
    groups.get(index).and_then(|group| group.first()).copied()
}

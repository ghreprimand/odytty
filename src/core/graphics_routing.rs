// SPDX-License-Identifier: GPL-3.0-only
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
//! After a successful sixel decode, DECSDM reset (the default, matching
//! xterm) moves the cursor to the row immediately below the image's last
//! sixel band, column 0. DECSDM set keeps the cursor at the placement
//! anchor instead. Private mode 80 is queryable and resets on RIS/DECSTR.
//!
//! # Cell-extent calculation
//!
//! The G2.1 placement API requires cell-extent (`display_columns`,
//! `display_rows`). The core receives live cell pixel metrics from the native
//! layer via [`super::screen::Screen::set_cell_metrics`]; headless tests use
//! the [`super::types::CellMetrics::DEFAULT`] (8×16 px). Extent is computed
//! as `ceil(image_px / cell_px)` per axis, clamped to screen bounds.

use crate::graphics::placement::MAX_RAW_GRAPHICS_BYTES;
use crate::graphics::sixel::{SixelBackground, decode_sixel};
use crate::graphics::{GraphicsProtocol, ImageScene, PlacementRequest};
use crate::parser::Params;

use super::kitty::{self, KittyState};
use super::types::CellMetrics;

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
    pub kitty: KittyState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApcOutcome {
    pub dirty: bool,
    pub cursor: Option<(usize, usize)>,
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
/// Cursor-below-image policy:
/// - DECSDM off (default): cursor moves to the row below the image, column 0.
/// - DECSDM on: cursor stays at its current position (image anchors at cursor).
///
/// Decode errors never disturb terminal state — the payload is dropped and
/// the error is counted in `stats`.
#[allow(clippy::too_many_arguments)]
pub(super) fn dcs_unhook(
    capture: DcsCapture,
    graphics: &mut ImageScene,
    stats: &mut GraphicsStats,
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
    sixel_display_mode: bool,
) -> Option<(usize, usize)> {
    if capture.overflowed {
        return None;
    }

    // Gate on well-formed DCS framing before decoding.
    if !graphics.accepts_sixel_dcs(&capture.body, capture.payload_start) {
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

    // Compute cell extent from pixel dimensions using live cell metrics.
    let display_columns = image.width.div_ceil(cell_metrics.width_px) as usize;
    let display_rows = image.height.div_ceil(cell_metrics.height_px) as usize;

    // Clamp extent to screen bounds from the anchor on BOTH axes, like the
    // Kitty and iTerm2 placement paths: a tall Sixel must not store a row
    // extent that runs past the bottom of the screen from the cursor row.
    let display_columns = display_columns
        .min(screen_cols.saturating_sub(cursor_col))
        .max(1);
    let display_rows = display_rows
        .min(screen_rows.saturating_sub(cursor_row))
        .max(1);

    // Place at cursor position.
    graphics.place(PlacementRequest::new(
        insert.id,
        GraphicsProtocol::Sixel,
        cursor_row,
        cursor_col,
        display_columns,
        display_rows,
    ));

    // DECSDM on: cursor stays at its current position.
    if sixel_display_mode {
        return Some((cursor_row, cursor_col));
    }

    // DECSDM off (default): cursor moves to the row below the image, column 0.
    let image_bottom_row = cursor_row + display_rows;
    let new_row = image_bottom_row.min(screen_rows.saturating_sub(1));
    Some((new_row, 0))
}

/// Handle an APC payload. Kitty graphics commands are decoded into the image
/// scene; unknown APC payloads retain the historical raw-recording behavior.
#[allow(clippy::too_many_arguments)]
pub(super) fn apc_dispatch(
    graphics: &mut ImageScene,
    stats: &mut GraphicsStats,
    host_output: &mut Vec<u8>,
    data: &[u8],
    cursor_row: usize,
    cursor_col: usize,
    screen_rows: usize,
    screen_cols: usize,
    cell_metrics: CellMetrics,
    named_transports_enabled: bool,
) -> ApcOutcome {
    match kitty::handle_apc(
        &mut stats.kitty,
        graphics,
        data,
        cursor_row,
        cursor_col,
        screen_rows,
        screen_cols,
        cell_metrics,
        named_transports_enabled,
    ) {
        Ok(outcome) => {
            host_output.extend_from_slice(&outcome.response);
            ApcOutcome {
                dirty: outcome.dirty,
                cursor: outcome.cursor,
            }
        }
        Err(_) => ApcOutcome {
            dirty: graphics.accepts_kitty_apc(data),
            cursor: None,
        },
    }
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

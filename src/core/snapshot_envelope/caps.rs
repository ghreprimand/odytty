// SPDX-License-Identifier: GPL-3.0-only
//! Capture-side limits and decode-side resource caps.
//!
//! The two are deliberately separate: capture limits bound how much history
//! OdyTTY copies out of its own terminal, while the decode caps bound what an
//! attaching client will allocate for a file it did not produce.

use super::format::{
    MAX_CELL_WIRE_BYTES, ROW_WIRE_OVERHEAD_BYTES, TERMINAL_STATE_PRELUDE_WIRE_BYTES,
};

/// Capture-side bounds for copying terminal state into an owned DTO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotCaptureLimits {
    pub max_scrollback_rows: usize,
}

impl Default for SnapshotCaptureLimits {
    fn default() -> Self {
        Self {
            max_scrollback_rows: 10_000,
        }
    }
}

/// Default per-string byte cap shared by the decoder and the capture path. The
/// decoder rejects any string longer than [`SnapshotEnvelopeCaps::max_string_bytes`];
/// capture bounds `title`/`working_directory` to this same value so the encoder
/// never emits a string its own default decoder would reject (a reattach that
/// dropped the whole grid over an oversized title would be far worse than a
/// truncated title). It also keeps each length within the `u16` on-wire prefix.
pub const DEFAULT_MAX_STRING_BYTES: usize = 4096;

/// Decode-side resource caps. These are separate from capture limits so an
/// attaching client can reject untrusted or future-expanded files before
/// allocating large buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEnvelopeCaps {
    pub max_total_len: usize,
    pub max_sections: usize,
    pub max_section_len: usize,
    pub max_columns: usize,
    pub max_rows: usize,
    pub max_scrollback_rows: usize,
    pub max_cells: usize,
    pub max_string_bytes: usize,
}

impl Default for SnapshotEnvelopeCaps {
    fn default() -> Self {
        Self {
            max_total_len: 64 * 1024 * 1024,
            max_sections: 32,
            max_section_len: 32 * 1024 * 1024,
            max_columns: 4096,
            max_rows: 4096,
            max_scrollback_rows: 100_000,
            max_cells: 4_000_000,
            max_string_bytes: DEFAULT_MAX_STRING_BYTES,
        }
    }
}

impl SnapshotEnvelopeCaps {
    /// Largest visible-grid cell count whose terminal-state section is
    /// guaranteed to decode under these caps even when every cell encodes at
    /// its worst-case wire size and no scrollback can be shed. Capture can
    /// truncate scrollback to fit the section budget, but the visible grid is
    /// structural (the decoder requires exactly `rows` visible rows), so any
    /// producer accepting untrusted dimensions must bound the visible grid by
    /// this figure or risk emitting a snapshot its own consumers reject.
    pub fn max_self_decodable_visible_cells(&self) -> usize {
        let fixed = TERMINAL_STATE_PRELUDE_WIRE_BYTES
            .saturating_add(2 * 4)
            .saturating_add(self.max_rows.saturating_mul(ROW_WIRE_OVERHEAD_BYTES));
        (self.max_section_len.saturating_sub(fixed) / MAX_CELL_WIRE_BYTES).min(self.max_cells)
    }
}

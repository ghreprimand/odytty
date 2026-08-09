// SPDX-License-Identifier: GPL-3.0-only
//! Wire identity of the snapshot container: magic, versions, section ids, and
//! the pinned per-element wire sizes the capture budgets are derived from.
//!
//! Nothing here decides policy. The values are the format itself, so every
//! encoder, decoder, and budget calculation reads them from one place and a
//! format change cannot drift between producer and consumer.

pub const SNAPSHOT_MAGIC: &[u8; 15] = b"ODYTTY-SNAPSHOT";
pub const SNAPSHOT_FORMAT_VERSION: u16 = 3;
pub const SNAPSHOT_PROTOCOL_VERSION: u16 = 1;

pub(super) const SECTION_TERMINAL_STATE: u16 = 1;
pub(super) const SECTION_DYNAMIC_COLORS: u16 = 2;
pub(super) const SECTION_METADATA: u16 = 3;
pub(super) const SECTION_PROMPT_MARKS: u16 = 4;
pub(super) const SECTION_LAYOUT_STATE: u16 = 5;
pub(super) const SECTION_FLAG_REQUIRED: u8 = 0x01;

/// Worst-case wire bytes for one encoded cell: char scalar (4), attribute
/// flags (2), underline style (1), optional RGB underline color (1 + 4),
/// RGB foreground (4), RGB background (4), hyperlink id (4), protected (1),
/// wide-continuation (1), combining count (1) and `MAX_COMBINING` (4)
/// combining scalars (4 each). Pinned by `maximal_cell_wire_len_is_pinned` so
/// a wire format change cannot silently drift the budgets derived from it.
pub const MAX_CELL_WIRE_BYTES: usize = 43;

/// Wire bytes per row beyond its cells: wrapped flag (1) + width prefix (4).
pub const ROW_WIRE_OVERHEAD_BYTES: usize = 5;

/// Wire bytes of the terminal-state section ahead of the two row lists:
/// dimensions (8), cursor (8), cursor visibility/style/blink (3), and basic
/// modes (12). Pinned by `terminal_state_prelude_wire_len_is_pinned`.
pub const TERMINAL_STATE_PRELUDE_WIRE_BYTES: usize = 31;

/// One section-table entry as read back from the wire.
#[derive(Debug, Clone, Copy)]
pub(super) struct SectionHeader {
    pub(super) id: u16,
    pub(super) flags: u8,
    pub(super) len: usize,
}

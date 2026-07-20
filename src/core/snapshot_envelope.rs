// SPDX-License-Identifier: GPL-3.0-only
//! Versioned, OdyTTY-owned terminal snapshot envelope for resumable sessions.
//!
//! This is the Phase 2 persistence boundary: it serializes owned DTOs copied
//! out of the terminal model, not private `Screen` / `Scrollback` internals and
//! not third-party derives over those internals. The first format version keeps
//! the state subset intentionally constrained: dimensions, visible grid,
//! bounded physical scrollback, cursor, and basic terminal modes.

use std::fmt;
use std::num::NonZeroU32;

use super::prompt_marks::PromptKind;
use super::screen::Terminal;
use super::types::{
    Attrs, Cell, CharsetModes, Color, CursorStyle, Dimensions, DynamicColors, KeyboardModes,
    LinkId, MouseEncoding, MouseProtocol, MouseTracking, Position, RgbColor, UnderlineStyle,
};

pub const SNAPSHOT_MAGIC: &[u8; 15] = b"ODYTTY-SNAPSHOT";
pub const SNAPSHOT_FORMAT_VERSION: u16 = 3;
pub const SNAPSHOT_PROTOCOL_VERSION: u16 = 1;

const SECTION_TERMINAL_STATE: u16 = 1;
const SECTION_DYNAMIC_COLORS: u16 = 2;
const SECTION_METADATA: u16 = 3;
const SECTION_PROMPT_MARKS: u16 = 4;
const SECTION_LAYOUT_STATE: u16 = 5;
const SECTION_FLAG_REQUIRED: u8 = 0x01;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEnvelope {
    pub producer_version: String,
    pub protocol_version: u16,
    pub terminal: SnapshotTerminalState,
    pub dynamic_colors: DynamicColors,
    pub metadata: SnapshotMetadata,
    pub prompt_marks: Vec<SnapshotPromptMark>,
    pub layout: SnapshotLayoutState,
}

impl SnapshotEnvelope {
    pub fn from_terminal(terminal: &Terminal, limits: SnapshotCaptureLimits) -> Self {
        let mut terminal_state = terminal.snapshot_state(limits.max_scrollback_rows);
        terminal_state.bound_to_decode_budget();
        // Prompt-mark rows index the terminal's FULL history (row 0 = oldest
        // physical scrollback row), while the captured state holds only the
        // newest rows that survived both the capture limit and the decode
        // budget above. Rebase each mark onto the captured window and drop
        // marks whose rows were truncated away; an unrebased mark would point
        // at the wrong row, or fail the whole restore as out of range.
        let dropped = terminal
            .screen()
            .scrollback_len()
            .saturating_sub(terminal_state.scrollback_rows.len());
        let total_rows = terminal_state.scrollback_rows.len() + terminal_state.visible_rows.len();
        Self {
            producer_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            terminal: terminal_state,
            dynamic_colors: terminal.dynamic_colors().clone(),
            metadata: SnapshotMetadata::from_terminal(terminal),
            prompt_marks: terminal
                .prompt_marks()
                .into_iter()
                .filter_map(|(row, kind)| {
                    let row = row.checked_sub(dropped)?;
                    (row < total_rows).then_some(SnapshotPromptMark { row, kind })
                })
                .collect(),
            layout: terminal.snapshot_layout_state(),
        }
    }

    /// Validate every field the wire format narrows before it is truncated
    /// into its on-wire width: `u32` dimensions/cursor/row-and-mark counts and
    /// scroll-region bounds, `u16` string lengths, and the `u8` per-cell
    /// combining count. [`Self::from_terminal`] output always passes (capture
    /// bounds every value structurally), so this exists for externally
    /// constructed envelopes, whose oversized `usize` fields would otherwise
    /// truncate silently into bytes the envelope's own decoder cannot read.
    pub fn validate_wire_bounds(&self) -> Result<(), SnapshotEnvelopeError> {
        // The producer version rides the header with a u16 length prefix.
        // `from_terminal` fills it from the compile-time package version, so
        // only externally constructed envelopes can exceed the width.
        check_u16(self.producer_version.len(), "producer version length")?;
        self.terminal.validate_wire_bounds()?;
        self.metadata.validate_wire_bounds()?;
        check_u32(self.prompt_marks.len(), "prompt mark count")?;
        for mark in &self.prompt_marks {
            check_u32(mark.row, "prompt mark row")?;
        }
        self.layout.validate_wire_bounds()?;
        Ok(())
    }

    /// Encode the envelope into one contiguous buffer.
    ///
    /// Fallible: encoding validates wire bounds first
    /// ([`Self::validate_wire_bounds`]) and refuses an envelope whose fields
    /// cannot be represented losslessly, instead of truncating them into a
    /// buffer that fails to decode. The writers themselves are infallible —
    /// after validation every narrowing cast is provably lossless.
    ///
    /// The terminal-state section (by far the largest payload; tens of MB for
    /// a wide, deep session) is encoded DIRECTLY into the output buffer, with
    /// its section-table length backpatched afterwards, instead of being
    /// encoded into its own vector and then copied into the envelope. That
    /// removes one full copy and one transient full-size allocation from
    /// every snapshot broadcast/save. The wire bytes are identical to the
    /// table-driven encoder (`encode_sections`), pinned by
    /// `direct_encode_matches_the_table_driven_encoder`.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotEnvelopeError> {
        self.validate_wire_bounds()?;
        const TABLE: [(u16, u8); 5] = [
            (SECTION_TERMINAL_STATE, SECTION_FLAG_REQUIRED),
            (SECTION_DYNAMIC_COLORS, 0),
            (SECTION_METADATA, 0),
            (SECTION_PROMPT_MARKS, 0),
            (SECTION_LAYOUT_STATE, 0),
        ];
        // Section table entry wire size: id u16 + flags u8 + reserved u8 +
        // len u64.
        const TABLE_ENTRY_BYTES: usize = 12;
        const LEN_OFFSET_IN_ENTRY: usize = 4;

        let mut out = Vec::new();
        out.extend_from_slice(SNAPSHOT_MAGIC);
        write_u16(&mut out, SNAPSHOT_FORMAT_VERSION);
        write_u16(&mut out, self.protocol_version);
        write_string(&mut out, &self.producer_version);
        write_u16(&mut out, TABLE.len() as u16);
        let table_start = out.len();
        for (id, flags) in TABLE {
            write_u16(&mut out, id);
            write_u8(&mut out, flags);
            write_u8(&mut out, 0);
            write_u64(&mut out, 0); // Backpatched once the payload is written.
        }
        for (index, (id, _)) in TABLE.iter().enumerate() {
            let payload_start = out.len();
            match *id {
                SECTION_TERMINAL_STATE => self.terminal.encode_into(&mut out),
                SECTION_DYNAMIC_COLORS => {
                    out.extend_from_slice(&encode_dynamic_colors(&self.dynamic_colors));
                }
                SECTION_METADATA => out.extend_from_slice(&self.metadata.encode()),
                SECTION_PROMPT_MARKS => {
                    out.extend_from_slice(&encode_prompt_marks(&self.prompt_marks));
                }
                SECTION_LAYOUT_STATE => out.extend_from_slice(&self.layout.encode()),
                _ => unreachable!("fixed section table"),
            }
            let len = (out.len() - payload_start) as u64;
            let entry = table_start + index * TABLE_ENTRY_BYTES + LEN_OFFSET_IN_ENTRY;
            out[entry..entry + 8].copy_from_slice(&len.to_le_bytes());
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> Result<Self, SnapshotEnvelopeError> {
        if bytes.len() > caps.max_total_len {
            return Err(SnapshotEnvelopeError::TotalTooLarge {
                len: bytes.len(),
                max: caps.max_total_len,
            });
        }

        let mut reader = Reader::new(bytes);
        reader.read_magic()?;
        let format_version = reader.read_u16()?;
        let protocol_version = reader.read_u16()?;
        if !(1..=SNAPSHOT_FORMAT_VERSION).contains(&format_version)
            || protocol_version != SNAPSHOT_PROTOCOL_VERSION
        {
            return Err(SnapshotEnvelopeError::UnsupportedVersion {
                format_version,
                protocol_version,
            });
        }
        let producer_version = reader.read_string(caps.max_string_bytes)?;
        let section_count = reader.read_u16()? as usize;
        if section_count > caps.max_sections {
            return Err(SnapshotEnvelopeError::TooManySections {
                count: section_count,
                max: caps.max_sections,
            });
        }

        let mut table = Vec::with_capacity(section_count);
        for _ in 0..section_count {
            let id = reader.read_u16()?;
            let flags = reader.read_u8()?;
            let _reserved = reader.read_u8()?;
            let len = reader.read_u64()? as usize;
            if len > caps.max_section_len {
                return Err(SnapshotEnvelopeError::SectionTooLarge {
                    id,
                    len,
                    max: caps.max_section_len,
                });
            }
            table.push(SectionHeader { id, flags, len });
        }

        let mut terminal = None;
        let mut dynamic_colors = None;
        let mut metadata = None;
        let mut prompt_marks = None;
        let mut layout = None;
        for section in table {
            let payload = reader.read_bytes(section.len)?;
            match section.id {
                SECTION_TERMINAL_STATE => {
                    let state = SnapshotTerminalState::decode(payload, caps, format_version)?;
                    terminal = Some(state);
                }
                SECTION_DYNAMIC_COLORS => {
                    dynamic_colors = Some(decode_dynamic_colors(payload)?);
                }
                SECTION_METADATA => {
                    metadata = Some(SnapshotMetadata::decode(payload, caps)?);
                }
                SECTION_PROMPT_MARKS => {
                    prompt_marks = Some(decode_prompt_marks(payload, caps)?);
                }
                SECTION_LAYOUT_STATE => {
                    layout = Some(SnapshotLayoutState::decode(payload, caps)?);
                }
                _ if section.flags & SECTION_FLAG_REQUIRED != 0 => {
                    return Err(SnapshotEnvelopeError::UnknownRequiredSection(section.id));
                }
                _ => {}
            }
        }
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }

        let Some(terminal) = terminal else {
            return Err(SnapshotEnvelopeError::MissingRequiredSection(
                SECTION_TERMINAL_STATE,
            ));
        };
        let layout =
            layout.unwrap_or_else(|| SnapshotLayoutState::defaults_for(terminal.dimensions));
        layout.validate(terminal.dimensions)?;

        Ok(Self {
            producer_version,
            protocol_version,
            terminal,
            dynamic_colors: dynamic_colors.unwrap_or_default(),
            metadata: metadata.unwrap_or_default(),
            prompt_marks: prompt_marks.unwrap_or_default(),
            layout,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotMetadata {
    pub title: Option<String>,
    pub working_directory: Option<String>,
}

impl SnapshotMetadata {
    pub fn from_terminal(terminal: &Terminal) -> Self {
        // Bound each string to the decoder's default cap at capture time. An
        // unbounded OSC 2 title or OSC 7 cwd would otherwise encode a string the
        // default decoder rejects, aborting the whole envelope (grid, scrollback
        // and modes) on reattach rather than just shortening the title.
        Self {
            title: terminal
                .title()
                .map(|title| truncate_to_char_boundary(title, DEFAULT_MAX_STRING_BYTES)),
            working_directory: terminal
                .current_working_directory()
                .map(|cwd| truncate_to_char_boundary(cwd, DEFAULT_MAX_STRING_BYTES)),
        }
    }

    /// The metadata half of [`SnapshotEnvelope::validate_wire_bounds`]: both
    /// strings carry a `u16` length prefix on the wire.
    fn validate_wire_bounds(&self) -> Result<(), SnapshotEnvelopeError> {
        if let Some(title) = &self.title {
            check_u16(title.len(), "title length")?;
        }
        if let Some(cwd) = &self.working_directory {
            check_u16(cwd.len(), "working directory length")?;
        }
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_optional_string(&mut out, self.title.as_deref());
        write_optional_string(&mut out, self.working_directory.as_deref());
        out
    }

    fn decode(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> Result<Self, SnapshotEnvelopeError> {
        let mut reader = Reader::new(bytes);
        let title = reader.read_optional_string(caps.max_string_bytes)?;
        let working_directory = reader.read_optional_string(caps.max_string_bytes)?;
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            title,
            working_directory,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotPromptMark {
    pub row: usize,
    pub kind: PromptKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLayoutState {
    pub scroll_region: Option<SnapshotScrollRegion>,
    pub tab_stops: Vec<bool>,
}

impl SnapshotLayoutState {
    pub fn defaults_for(dimensions: Dimensions) -> Self {
        Self {
            scroll_region: None,
            tab_stops: default_tab_stops(dimensions.columns),
        }
    }

    /// The layout half of [`SnapshotEnvelope::validate_wire_bounds`]: the
    /// scroll-region bounds and the tab-stop count travel as `u32`.
    fn validate_wire_bounds(&self) -> Result<(), SnapshotEnvelopeError> {
        if let Some(region) = self.scroll_region {
            check_u32(region.top, "scroll region top")?;
            check_u32(region.bottom, "scroll region bottom")?;
        }
        check_u32(self.tab_stops.len(), "tab stop count")?;
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self.scroll_region {
            Some(region) => {
                write_u8(&mut out, 1);
                write_u32(&mut out, region.top as u32);
                write_u32(&mut out, region.bottom as u32);
            }
            None => write_u8(&mut out, 0),
        }
        write_u32(&mut out, self.tab_stops.len() as u32);
        for &stop in &self.tab_stops {
            write_u8(&mut out, u8::from(stop));
        }
        out
    }

    fn decode(bytes: &[u8], caps: SnapshotEnvelopeCaps) -> Result<Self, SnapshotEnvelopeError> {
        let mut reader = Reader::new(bytes);
        let scroll_region = match reader.read_u8()? {
            0 => None,
            1 => Some(SnapshotScrollRegion {
                top: reader.read_u32()? as usize,
                bottom: reader.read_u32()? as usize,
            }),
            value => return Err(SnapshotEnvelopeError::InvalidBool(value)),
        };
        let count = reader.read_u32()? as usize;
        if count > caps.max_columns {
            return Err(SnapshotEnvelopeError::InvalidTabStopCount {
                count,
                expected: caps.max_columns,
            });
        }
        // `count` is already bounded by `max_columns` (~4 KB worst case), but
        // cap the reserve by what the remaining payload could actually encode
        // (one byte per stop), matching the other length-prefixed decoders.
        let mut tab_stops = Vec::with_capacity(count.min(reader.remaining()));
        for _ in 0..count {
            tab_stops.push(reader.read_bool()?);
        }
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            scroll_region,
            tab_stops,
        })
    }

    pub(in crate::core) fn validate(
        &self,
        dimensions: Dimensions,
    ) -> Result<(), SnapshotEnvelopeError> {
        if self.tab_stops.len() != dimensions.columns {
            return Err(SnapshotEnvelopeError::InvalidTabStopCount {
                count: self.tab_stops.len(),
                expected: dimensions.columns,
            });
        }
        if let Some(region) = self.scroll_region
            && !(region.top < region.bottom && region.bottom < dimensions.rows)
        {
            return Err(SnapshotEnvelopeError::InvalidScrollRegion {
                top: region.top,
                bottom: region.bottom,
                rows: dimensions.rows,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotScrollRegion {
    pub top: usize,
    pub bottom: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTerminalState {
    pub dimensions: Dimensions,
    pub cursor: Position,
    pub cursor_visible: bool,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    pub basic_modes: SnapshotBasicModes,
    pub scrollback_rows: Vec<SnapshotRow>,
    pub visible_rows: Vec<SnapshotRow>,
}

impl SnapshotTerminalState {
    /// Standalone section encode; production writes through [`Self::encode_into`],
    /// tests use this for section-level fixtures and size measurements.
    #[cfg(test)]
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Append the encoded terminal-state section to `out`. The envelope
    /// encoder writes the (large) section straight into the frame buffer
    /// through this seam instead of copying an intermediate vector.
    fn encode_into(&self, out: &mut Vec<u8>) {
        self.encode_prelude(out);
        write_rows(out, &self.scrollback_rows);
        write_rows(out, &self.visible_rows);
    }

    /// Everything the terminal-state section carries ahead of the two row
    /// lists. Split out so the capture-side budget check measures the fixed
    /// cost with the same encoder that produces the wire bytes.
    fn encode_prelude(&self, out: &mut Vec<u8>) {
        write_u32(out, self.dimensions.columns as u32);
        write_u32(out, self.dimensions.rows as u32);
        write_u32(out, self.cursor.row as u32);
        write_u32(out, self.cursor.column as u32);
        write_u8(out, u8::from(self.cursor_visible));
        write_u8(out, encode_cursor_style(self.cursor_style));
        write_u8(out, u8::from(self.cursor_blink));
        self.basic_modes.encode(out);
    }

    /// Truncate the oldest scrollback rows until this state decodes under the
    /// DEFAULT decoder caps: the row-count cap, the total-cell cap, and the
    /// terminal-section byte cap. Capture limits bound only how much history
    /// is copied out of the terminal; without this coupling a wide session
    /// with deep scrollback encodes a terminal section larger than the
    /// decoder's section budget, and the host serves a snapshot every default
    /// consumer rejects — leaving the session permanently un-attachable.
    /// Returns the number of scrollback rows dropped so the caller can rebase
    /// row-indexed metadata (prompt marks) onto the truncated history.
    ///
    /// Byte costs are measured with the same row encoder that produces the
    /// wire bytes, so the bound cannot drift from the format.
    fn bound_to_decode_budget(&mut self) -> usize {
        self.bound_to_decode_budget_with(&SnapshotEnvelopeCaps::default())
    }

    fn bound_to_decode_budget_with(&mut self, caps: &SnapshotEnvelopeCaps) -> usize {
        let before = self.scrollback_rows.len();
        let columns = self.dimensions.columns.max(1);

        // Row-count and total-cell budgets (cheap, count arithmetic only).
        let cell_budget_rows = (caps.max_cells / columns).saturating_sub(self.visible_rows.len());
        let keep = before.min(caps.max_scrollback_rows).min(cell_budget_rows);
        if keep < self.scrollback_rows.len() {
            let drop = self.scrollback_rows.len() - keep;
            self.scrollback_rows.drain(..drop);
        }

        // Section byte budget: fixed prelude + both row-count prefixes + the
        // visible rows are mandatory; the newest scrollback rows that still
        // fit are kept, oldest first to go.
        let mut scratch = Vec::new();
        self.encode_prelude(&mut scratch);
        let mut used = scratch.len() + 2 * 4;
        for row in &self.visible_rows {
            scratch.clear();
            write_row(&mut scratch, row);
            used = used.saturating_add(scratch.len());
        }
        let mut keep_from = self.scrollback_rows.len();
        for (index, row) in self.scrollback_rows.iter().enumerate().rev() {
            scratch.clear();
            write_row(&mut scratch, row);
            match used.checked_add(scratch.len()) {
                Some(total) if total <= caps.max_section_len => {
                    used = total;
                    keep_from = index;
                }
                _ => break,
            }
        }
        if keep_from > 0 {
            self.scrollback_rows.drain(..keep_from);
        }
        before - self.scrollback_rows.len()
    }

    /// The terminal-state half of [`SnapshotEnvelope::validate_wire_bounds`]:
    /// dimensions, cursor, row counts, per-row cell counts, and per-cell
    /// combining counts must all fit their on-wire widths.
    fn validate_wire_bounds(&self) -> Result<(), SnapshotEnvelopeError> {
        check_u32(self.dimensions.columns, "columns")?;
        check_u32(self.dimensions.rows, "rows")?;
        check_u32(self.cursor.row, "cursor row")?;
        check_u32(self.cursor.column, "cursor column")?;
        check_u32(self.scrollback_rows.len(), "scrollback row count")?;
        check_u32(self.visible_rows.len(), "visible row count")?;
        for row in self.scrollback_rows.iter().chain(&self.visible_rows) {
            check_u32(row.cells.len(), "row cell count")?;
            for cell in &row.cells {
                check_u8(cell.combining.len(), "combining mark count")?;
            }
        }
        Ok(())
    }

    fn decode(
        bytes: &[u8],
        caps: SnapshotEnvelopeCaps,
        format_version: u16,
    ) -> Result<Self, SnapshotEnvelopeError> {
        let mut reader = Reader::new(bytes);
        let columns = reader.read_u32()? as usize;
        let rows = reader.read_u32()? as usize;
        if columns == 0 || rows == 0 || columns > caps.max_columns || rows > caps.max_rows {
            return Err(SnapshotEnvelopeError::InvalidDimensions { columns, rows });
        }
        let dimensions = Dimensions::new(columns, rows);
        let cursor = Position {
            row: reader.read_u32()? as usize,
            column: reader.read_u32()? as usize,
        };
        if cursor.row >= rows || cursor.column >= columns {
            return Err(SnapshotEnvelopeError::InvalidCursor { cursor });
        }
        let cursor_visible = reader.read_bool()?;
        let cursor_style = decode_cursor_style(reader.read_u8()?)?;
        let cursor_blink = reader.read_bool()?;
        let basic_modes = SnapshotBasicModes::decode(&mut reader, format_version)?;
        let scrollback_rows = read_rows(&mut reader, dimensions, caps.max_scrollback_rows)?;
        let visible_rows = read_rows(&mut reader, dimensions, caps.max_rows)?;
        if visible_rows.len() != rows {
            return Err(SnapshotEnvelopeError::InvalidVisibleRowCount {
                count: visible_rows.len(),
                expected: rows,
            });
        }
        let total_rows = scrollback_rows
            .len()
            .checked_add(visible_rows.len())
            .ok_or(SnapshotEnvelopeError::CellCapExceeded)?;
        let total_cells = total_rows
            .checked_mul(columns)
            .ok_or(SnapshotEnvelopeError::CellCapExceeded)?;
        if total_cells > caps.max_cells {
            return Err(SnapshotEnvelopeError::CellCapExceeded);
        }
        if reader.remaining() != 0 {
            return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
        }
        Ok(Self {
            dimensions,
            cursor,
            cursor_visible,
            cursor_style,
            cursor_blink,
            basic_modes,
            scrollback_rows,
            visible_rows,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotBasicModes {
    pub bracketed_paste: bool,
    pub alternate_scroll: bool,
    pub alternate_screen: bool,
    pub synchronized_output: bool,
    pub focus_reporting: bool,
    pub mouse: MouseProtocol,
    pub keyboard: KeyboardModes,
    pub charsets: CharsetModes,
}

impl SnapshotBasicModes {
    pub fn from_terminal(terminal: &Terminal) -> Self {
        Self {
            bracketed_paste: terminal.bracketed_paste_enabled(),
            alternate_scroll: terminal.alternate_scroll_enabled(),
            alternate_screen: terminal.on_alternate_screen(),
            synchronized_output: terminal.synchronized_output_enabled(),
            focus_reporting: terminal.focus_reporting(),
            mouse: terminal.mouse_protocol(),
            keyboard: terminal.keyboard_modes(),
            charsets: terminal.charset_modes(),
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        write_u8(out, u8::from(self.bracketed_paste));
        write_u8(out, u8::from(self.alternate_scroll));
        write_u8(out, u8::from(self.alternate_screen));
        write_u8(out, u8::from(self.synchronized_output));
        write_u8(out, u8::from(self.focus_reporting));
        write_u8(out, encode_mouse_tracking(self.mouse.tracking));
        write_u8(out, encode_mouse_encoding(self.mouse.encoding));
        write_u8(out, u8::from(self.keyboard.application_cursor));
        write_u8(out, u8::from(self.keyboard.application_keypad));
        write_u16(out, self.keyboard.kitty_keyboard_flags);
        // Format v3 appended field: G0/G1 charset designations + GL selection,
        // packed into one byte so a mid-ACS-run session round-trips its
        // line-drawing state across snapshot/attach.
        write_u8(out, encode_charset_modes(self.charsets));
    }

    fn decode(reader: &mut Reader<'_>, format_version: u16) -> Result<Self, SnapshotEnvelopeError> {
        Ok(Self {
            bracketed_paste: reader.read_bool()?,
            alternate_scroll: reader.read_bool()?,
            alternate_screen: reader.read_bool()?,
            synchronized_output: reader.read_bool()?,
            focus_reporting: reader.read_bool()?,
            mouse: MouseProtocol {
                tracking: decode_mouse_tracking(reader.read_u8()?)?,
                encoding: decode_mouse_encoding(reader.read_u8()?)?,
            },
            keyboard: KeyboardModes {
                application_cursor: reader.read_bool()?,
                application_keypad: reader.read_bool()?,
                kitty_keyboard_flags: reader.read_u16()?,
                // W32IM is a live ConPTY negotiation, not resumable terminal
                // state. A newly attached ConPTY re-emits CSI ? 9001 h.
                win32_input: false,
                // modifyOtherKeys is not persisted in the snapshot wire format
                // (format v2 predates it); restored sessions start at level 0
                // and an attached app re-enables it with its next XTMODKEYS.
                modify_other_keys: 0,
            },
            // Appended in format v3; older snapshots restore at the charset
            // power-on state (ASCII G0/G1, GL=G0).
            charsets: if format_version >= 3 {
                decode_charset_modes(reader.read_u8()?)?
            } else {
                CharsetModes::default()
            },
        })
    }
}

/// Pack [`CharsetModes`] into the format v3 wire byte: bit 0 = GL selects G1
/// (SO latched), bit 1 = G0 designated DEC Special Graphics, bit 2 = G1
/// designated DEC Special Graphics. Bits 3..=7 are reserved and must be zero.
fn encode_charset_modes(charsets: CharsetModes) -> u8 {
    u8::from(charsets.gl_g1)
        | (u8::from(charsets.g0_graphics) << 1)
        | (u8::from(charsets.g1_graphics) << 2)
}

fn decode_charset_modes(byte: u8) -> Result<CharsetModes, SnapshotEnvelopeError> {
    if byte & !0b111 != 0 {
        // Defensive decode: reserved bits from a future/corrupt producer fail
        // cleanly rather than silently dropping unknown charset state.
        return Err(SnapshotEnvelopeError::InvalidEnum("charset modes", byte));
    }
    Ok(CharsetModes {
        gl_g1: byte & 0b001 != 0,
        g0_graphics: byte & 0b010 != 0,
        g1_graphics: byte & 0b100 != 0,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRow {
    pub wrapped: bool,
    pub cells: Vec<SnapshotCell>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCell {
    pub ch: char,
    pub attrs: SnapshotAttrs,
    pub protected: bool,
    pub wide_continuation: bool,
    pub combining: Vec<char>,
}

impl From<Cell> for SnapshotCell {
    fn from(cell: Cell) -> Self {
        Self {
            ch: cell.ch,
            attrs: SnapshotAttrs::from(cell.attrs),
            protected: cell.protected,
            wide_continuation: cell.wide_continuation,
            combining: cell.combining().to_vec(),
        }
    }
}

impl SnapshotCell {
    pub fn to_cell(&self) -> Cell {
        let mut cell = Cell::new(self.ch, self.attrs.to_attrs());
        cell.protected = self.protected;
        cell.wide_continuation = self.wide_continuation;
        for &mark in &self.combining {
            let _ = cell.push_combining(mark);
        }
        cell
    }

    fn encode(&self, out: &mut Vec<u8>) {
        write_u32(out, self.ch as u32);
        self.attrs.encode(out);
        write_u8(out, u8::from(self.protected));
        write_u8(out, u8::from(self.wide_continuation));
        write_u8(out, self.combining.len() as u8);
        for &mark in &self.combining {
            write_u32(out, mark as u32);
        }
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, SnapshotEnvelopeError> {
        let ch = read_char(reader)?;
        let attrs = SnapshotAttrs::decode(reader)?;
        let protected = reader.read_bool()?;
        let wide_continuation = reader.read_bool()?;
        let combining_len = reader.read_u8()? as usize;
        let mut combining = Vec::with_capacity(combining_len);
        for _ in 0..combining_len {
            combining.push(read_char(reader)?);
        }
        Ok(Self {
            ch,
            attrs,
            protected,
            wide_continuation,
            combining,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAttrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub strikethrough: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub underline_style: UnderlineStyle,
    pub underline_color: Option<Color>,
    pub foreground: Color,
    pub background: Color,
    pub hyperlink: Option<u32>,
}

impl From<Attrs> for SnapshotAttrs {
    fn from(attrs: Attrs) -> Self {
        Self {
            bold: attrs.bold(),
            dim: attrs.dim(),
            italic: attrs.italic(),
            underline: attrs.underline(),
            blink: attrs.blink(),
            strikethrough: attrs.strikethrough(),
            inverse: attrs.inverse(),
            hidden: attrs.hidden(),
            underline_style: attrs.underline_style,
            underline_color: attrs.underline_color,
            foreground: attrs.foreground,
            background: attrs.background,
            hyperlink: attrs.hyperlink.map(LinkId::get),
        }
    }
}

impl SnapshotAttrs {
    pub fn to_attrs(&self) -> Attrs {
        let mut attrs = Attrs::default();
        attrs.underline_style = self.underline_style;
        attrs.underline_color = self.underline_color;
        attrs.foreground = self.foreground;
        attrs.background = self.background;
        attrs.hyperlink = self.hyperlink.and_then(NonZeroU32::new).map(LinkId::new);
        attrs.set_bold(self.bold);
        attrs.set_dim(self.dim);
        attrs.set_italic(self.italic);
        attrs.set_underline(self.underline);
        attrs.set_blink(self.blink);
        attrs.set_strikethrough(self.strikethrough);
        attrs.set_inverse(self.inverse);
        attrs.set_hidden(self.hidden);
        attrs
    }

    fn encode(&self, out: &mut Vec<u8>) {
        let mut flags = 0u16;
        flags |= u16::from(self.bold);
        flags |= u16::from(self.dim) << 1;
        flags |= u16::from(self.italic) << 2;
        flags |= u16::from(self.underline) << 3;
        flags |= u16::from(self.blink) << 4;
        flags |= u16::from(self.strikethrough) << 5;
        flags |= u16::from(self.inverse) << 6;
        flags |= u16::from(self.hidden) << 7;
        write_u16(out, flags);
        write_u8(out, encode_underline_style(self.underline_style));
        write_optional_color(out, self.underline_color);
        write_color(out, self.foreground);
        write_color(out, self.background);
        write_u32(out, self.hyperlink.unwrap_or(0));
    }

    fn decode(reader: &mut Reader<'_>) -> Result<Self, SnapshotEnvelopeError> {
        let flags = reader.read_u16()?;
        Ok(Self {
            bold: flags & (1 << 0) != 0,
            dim: flags & (1 << 1) != 0,
            italic: flags & (1 << 2) != 0,
            underline: flags & (1 << 3) != 0,
            blink: flags & (1 << 4) != 0,
            strikethrough: flags & (1 << 5) != 0,
            inverse: flags & (1 << 6) != 0,
            hidden: flags & (1 << 7) != 0,
            underline_style: decode_underline_style(reader.read_u8()?)?,
            underline_color: read_optional_color(reader)?,
            foreground: read_color(reader)?,
            background: read_color(reader)?,
            hyperlink: match reader.read_u32()? {
                0 => None,
                id => Some(id),
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotEnvelopeError {
    BadMagic,
    UnexpectedEof,
    InvalidUtf8,
    InvalidChar(u32),
    InvalidBool(u8),
    InvalidEnum(&'static str, u8),
    /// An externally constructed field exceeds its on-wire integer width;
    /// encoding refuses rather than truncating into undecodable bytes.
    ValueTooLarge {
        what: &'static str,
        value: usize,
        max: usize,
    },
    TotalTooLarge {
        len: usize,
        max: usize,
    },
    TooManySections {
        count: usize,
        max: usize,
    },
    SectionTooLarge {
        id: u16,
        len: usize,
        max: usize,
    },
    UnknownRequiredSection(u16),
    MissingRequiredSection(u16),
    UnsupportedVersion {
        format_version: u16,
        protocol_version: u16,
    },
    InvalidDimensions {
        columns: usize,
        rows: usize,
    },
    InvalidCursor {
        cursor: Position,
    },
    InvalidRowWidth {
        width: usize,
        columns: usize,
    },
    InvalidVisibleRowCount {
        count: usize,
        expected: usize,
    },
    InvalidTabStopCount {
        count: usize,
        expected: usize,
    },
    InvalidScrollRegion {
        top: usize,
        bottom: usize,
        rows: usize,
    },
    InvalidPromptMark {
        row: usize,
        rows: usize,
    },
    TooManyRows {
        count: usize,
        max: usize,
    },
    CellCapExceeded,
    StringTooLarge {
        len: usize,
        max: usize,
    },
    TrailingBytes(usize),
}

impl fmt::Display for SnapshotEnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "invalid OdyTTY snapshot magic"),
            Self::UnexpectedEof => write!(f, "truncated OdyTTY snapshot"),
            Self::InvalidUtf8 => write!(f, "invalid UTF-8 string in OdyTTY snapshot"),
            Self::InvalidChar(value) => write!(f, "invalid char scalar value {value}"),
            Self::InvalidBool(value) => write!(f, "invalid bool value {value}"),
            Self::InvalidEnum(name, value) => write!(f, "invalid {name} value {value}"),
            Self::ValueTooLarge { what, value, max } => {
                write!(f, "snapshot {what} {value} exceeds the wire maximum {max}")
            }
            Self::TotalTooLarge { len, max } => {
                write!(f, "snapshot is too large: {len} bytes exceeds cap {max}")
            }
            Self::TooManySections { count, max } => {
                write!(
                    f,
                    "snapshot has too many sections: {count} exceeds cap {max}"
                )
            }
            Self::SectionTooLarge { id, len, max } => {
                write!(
                    f,
                    "snapshot section {id} is too large: {len} exceeds cap {max}"
                )
            }
            Self::UnknownRequiredSection(id) => {
                write!(f, "unknown required snapshot section {id}")
            }
            Self::MissingRequiredSection(id) => {
                write!(f, "missing required snapshot section {id}")
            }
            Self::UnsupportedVersion {
                format_version,
                protocol_version,
            } => write!(
                f,
                "unsupported snapshot version format={format_version} protocol={protocol_version}"
            ),
            Self::InvalidDimensions { columns, rows } => {
                write!(f, "invalid snapshot dimensions {columns}x{rows}")
            }
            Self::InvalidCursor { cursor } => write!(
                f,
                "invalid snapshot cursor row={} column={}",
                cursor.row, cursor.column
            ),
            Self::InvalidRowWidth { width, columns } => {
                write!(f, "invalid snapshot row width {width}; expected {columns}")
            }
            Self::InvalidVisibleRowCount { count, expected } => {
                write!(f, "invalid visible row count {count}; expected {expected}")
            }
            Self::InvalidTabStopCount { count, expected } => {
                write!(f, "invalid tab-stop count {count}; expected {expected}")
            }
            Self::InvalidScrollRegion { top, bottom, rows } => write!(
                f,
                "invalid scroll region top={top} bottom={bottom} for {rows} rows"
            ),
            Self::InvalidPromptMark { row, rows } => {
                write!(f, "invalid prompt mark row={row} for {rows} rows")
            }
            Self::TooManyRows { count, max } => {
                write!(f, "snapshot has too many rows: {count} exceeds cap {max}")
            }
            Self::CellCapExceeded => write!(f, "snapshot cell cap exceeded"),
            Self::StringTooLarge { len, max } => {
                write!(f, "snapshot string too large: {len} exceeds cap {max}")
            }
            Self::TrailingBytes(count) => write!(f, "snapshot has {count} trailing bytes"),
        }
    }
}

impl std::error::Error for SnapshotEnvelopeError {}

#[cfg(test)]
struct SectionPayload {
    id: u16,
    flags: u8,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct SectionHeader {
    id: u16,
    flags: u8,
    len: usize,
}

/// Table-driven envelope encoder retained as the byte-format oracle: the
/// shipping `SnapshotEnvelope::encode` writes the large terminal section
/// directly into the output buffer, and the equivalence test pins its bytes
/// against this straightforward copy-based construction.
#[cfg(test)]
fn encode_sections(
    producer_version: &str,
    protocol_version: u16,
    sections: &[SectionPayload],
) -> Vec<u8> {
    encode_sections_for_version(
        SNAPSHOT_FORMAT_VERSION,
        producer_version,
        protocol_version,
        sections,
    )
}

#[cfg(test)]
fn encode_sections_for_version(
    format_version: u16,
    producer_version: &str,
    protocol_version: u16,
    sections: &[SectionPayload],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(SNAPSHOT_MAGIC);
    write_u16(&mut out, format_version);
    write_u16(&mut out, protocol_version);
    write_string(&mut out, producer_version);
    write_u16(&mut out, sections.len() as u16);
    for section in sections {
        write_u16(&mut out, section.id);
        write_u8(&mut out, section.flags);
        write_u8(&mut out, 0);
        write_u64(&mut out, section.payload.len() as u64);
    }
    for section in sections {
        out.extend_from_slice(&section.payload);
    }
    out
}

fn write_rows(out: &mut Vec<u8>, rows: &[SnapshotRow]) {
    write_u32(out, rows.len() as u32);
    for row in rows {
        write_row(out, row);
    }
}

fn write_row(out: &mut Vec<u8>, row: &SnapshotRow) {
    write_u8(out, u8::from(row.wrapped));
    write_u32(out, row.cells.len() as u32);
    for cell in &row.cells {
        cell.encode(out);
    }
}

fn read_rows(
    reader: &mut Reader<'_>,
    dimensions: Dimensions,
    max_rows: usize,
) -> Result<Vec<SnapshotRow>, SnapshotEnvelopeError> {
    let count = reader.read_u32()? as usize;
    if count > max_rows {
        return Err(SnapshotEnvelopeError::TooManyRows {
            count,
            max: max_rows,
        });
    }
    // Reserve no more than the remaining payload could possibly encode: a row
    // costs at least 5 wire bytes (wrapped flag + width u32), so a declared
    // count far beyond the actual payload cannot force a huge up-front
    // allocation before the first short read fails. The Vec grows normally if
    // the honest count exceeds the estimate.
    let mut rows = Vec::with_capacity(count.min(reader.remaining() / 5));
    for _ in 0..count {
        let wrapped = reader.read_bool()?;
        let width = reader.read_u32()? as usize;
        if width != dimensions.columns {
            return Err(SnapshotEnvelopeError::InvalidRowWidth {
                width,
                columns: dimensions.columns,
            });
        }
        let mut cells = Vec::with_capacity(width);
        for _ in 0..width {
            cells.push(SnapshotCell::decode(reader)?);
        }
        rows.push(SnapshotRow { wrapped, cells });
    }
    Ok(rows)
}

fn encode_dynamic_colors(colors: &DynamicColors) -> Vec<u8> {
    let mut out = Vec::new();
    write_rgb(&mut out, colors.foreground);
    write_rgb(&mut out, colors.background);
    write_rgb(&mut out, colors.cursor);
    for color in colors.palette {
        write_optional_rgb(&mut out, color);
    }
    out
}

fn decode_dynamic_colors(bytes: &[u8]) -> Result<DynamicColors, SnapshotEnvelopeError> {
    let mut reader = Reader::new(bytes);
    let foreground = read_rgb(&mut reader)?;
    let background = read_rgb(&mut reader)?;
    let cursor = read_rgb(&mut reader)?;
    let mut palette = [None; 256];
    for color in &mut palette {
        *color = read_optional_rgb(&mut reader)?;
    }
    if reader.remaining() != 0 {
        return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
    }
    Ok(DynamicColors {
        foreground,
        background,
        cursor,
        palette,
    })
}

fn encode_prompt_marks(marks: &[SnapshotPromptMark]) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32(&mut out, marks.len() as u32);
    for mark in marks {
        write_u32(&mut out, mark.row as u32);
        encode_prompt_kind(&mut out, mark.kind);
    }
    out
}

fn decode_prompt_marks(
    bytes: &[u8],
    caps: SnapshotEnvelopeCaps,
) -> Result<Vec<SnapshotPromptMark>, SnapshotEnvelopeError> {
    let mut reader = Reader::new(bytes);
    let count = reader.read_u32()? as usize;
    let max = caps.max_scrollback_rows.saturating_add(caps.max_rows);
    if count > max {
        return Err(SnapshotEnvelopeError::TooManyRows { count, max });
    }
    // Reserve no more than the remaining payload could possibly encode: a
    // mark costs at least 5 wire bytes (row u32 + kind byte), so a declared
    // count far beyond the actual payload cannot force a huge up-front
    // allocation before the first short read fails — the same cap the row
    // decoder applies. The Vec grows normally for an honest count.
    let mut marks = Vec::with_capacity(count.min(reader.remaining() / 5));
    for _ in 0..count {
        marks.push(SnapshotPromptMark {
            row: reader.read_u32()? as usize,
            kind: decode_prompt_kind(&mut reader)?,
        });
    }
    if reader.remaining() != 0 {
        return Err(SnapshotEnvelopeError::TrailingBytes(reader.remaining()));
    }
    Ok(marks)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn read_magic(&mut self) -> Result<(), SnapshotEnvelopeError> {
        let magic = self.read_bytes(SNAPSHOT_MAGIC.len())?;
        if magic == SNAPSHOT_MAGIC {
            Ok(())
        } else {
            Err(SnapshotEnvelopeError::BadMagic)
        }
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], SnapshotEnvelopeError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(SnapshotEnvelopeError::UnexpectedEof)?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(SnapshotEnvelopeError::UnexpectedEof)?;
        self.pos = end;
        Ok(bytes)
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotEnvelopeError> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotEnvelopeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(SnapshotEnvelopeError::InvalidBool(value)),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotEnvelopeError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotEnvelopeError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotEnvelopeError> {
        let bytes = self.read_bytes(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_string(&mut self, max: usize) -> Result<String, SnapshotEnvelopeError> {
        let len = self.read_u16()? as usize;
        if len > max {
            return Err(SnapshotEnvelopeError::StringTooLarge { len, max });
        }
        let bytes = self.read_bytes(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| SnapshotEnvelopeError::InvalidUtf8)
    }

    fn read_optional_string(
        &mut self,
        max: usize,
    ) -> Result<Option<String>, SnapshotEnvelopeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => self.read_string(max).map(Some),
            value => Err(SnapshotEnvelopeError::InvalidBool(value)),
        }
    }
}

/// Wire-bound checks backing [`SnapshotEnvelope::validate_wire_bounds`]: a
/// `usize` field must fit its narrowed on-wire integer. Portable across
/// pointer widths via `try_from` (on 32-bit targets the u32 check is
/// vacuously true and compiles without a lint suppression).
fn check_u32(value: usize, what: &'static str) -> Result<(), SnapshotEnvelopeError> {
    u32::try_from(value)
        .map(|_| ())
        .map_err(|_| SnapshotEnvelopeError::ValueTooLarge {
            what,
            value,
            max: u32::MAX as usize,
        })
}

fn check_u16(value: usize, what: &'static str) -> Result<(), SnapshotEnvelopeError> {
    u16::try_from(value)
        .map(|_| ())
        .map_err(|_| SnapshotEnvelopeError::ValueTooLarge {
            what,
            value,
            max: u16::MAX as usize,
        })
}

fn check_u8(value: usize, what: &'static str) -> Result<(), SnapshotEnvelopeError> {
    u8::try_from(value)
        .map(|_| ())
        .map_err(|_| SnapshotEnvelopeError::ValueTooLarge {
            what,
            value,
            max: u8::MAX as usize,
        })
}

fn write_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

fn write_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Return `value` shortened to at most `max` bytes, cut on a UTF-8 char
/// boundary (never mid-codepoint). Used to keep captured strings within the
/// decoder's per-string cap.
fn truncate_to_char_boundary(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_owned();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_u16(out, value.len() as u16);
    out.extend_from_slice(value.as_bytes());
}

fn write_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            write_u8(out, 1);
            write_string(out, value);
        }
        None => write_u8(out, 0),
    }
}

fn read_char(reader: &mut Reader<'_>) -> Result<char, SnapshotEnvelopeError> {
    let value = reader.read_u32()?;
    char::from_u32(value).ok_or(SnapshotEnvelopeError::InvalidChar(value))
}

fn write_rgb(out: &mut Vec<u8>, color: RgbColor) {
    write_u8(out, color.red);
    write_u8(out, color.green);
    write_u8(out, color.blue);
}

fn read_rgb(reader: &mut Reader<'_>) -> Result<RgbColor, SnapshotEnvelopeError> {
    Ok(RgbColor::new(
        reader.read_u8()?,
        reader.read_u8()?,
        reader.read_u8()?,
    ))
}

fn write_optional_rgb(out: &mut Vec<u8>, color: Option<RgbColor>) {
    match color {
        Some(color) => {
            write_u8(out, 1);
            write_rgb(out, color);
        }
        None => write_u8(out, 0),
    }
}

fn read_optional_rgb(reader: &mut Reader<'_>) -> Result<Option<RgbColor>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => read_rgb(reader).map(Some),
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
    }
}

fn write_color(out: &mut Vec<u8>, color: Color) {
    match color {
        Color::Default => write_u8(out, 0),
        Color::Indexed(index) => {
            write_u8(out, 1);
            write_u8(out, index);
        }
        Color::Rgb(red, green, blue) => {
            write_u8(out, 2);
            write_u8(out, red);
            write_u8(out, green);
            write_u8(out, blue);
        }
    }
}

fn read_color(reader: &mut Reader<'_>) -> Result<Color, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(Color::Default),
        1 => Ok(Color::Indexed(reader.read_u8()?)),
        2 => Ok(Color::Rgb(
            reader.read_u8()?,
            reader.read_u8()?,
            reader.read_u8()?,
        )),
        value => Err(SnapshotEnvelopeError::InvalidEnum("Color", value)),
    }
}

fn write_optional_color(out: &mut Vec<u8>, color: Option<Color>) {
    match color {
        Some(color) => {
            write_u8(out, 1);
            write_color(out, color);
        }
        None => write_u8(out, 0),
    }
}

fn read_optional_color(reader: &mut Reader<'_>) -> Result<Option<Color>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => read_color(reader).map(Some),
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
    }
}

fn encode_cursor_style(style: CursorStyle) -> u8 {
    match style {
        CursorStyle::Block => 0,
        CursorStyle::Underline => 1,
        CursorStyle::Bar => 2,
    }
}

fn decode_cursor_style(value: u8) -> Result<CursorStyle, SnapshotEnvelopeError> {
    match value {
        0 => Ok(CursorStyle::Block),
        1 => Ok(CursorStyle::Underline),
        2 => Ok(CursorStyle::Bar),
        value => Err(SnapshotEnvelopeError::InvalidEnum("CursorStyle", value)),
    }
}

fn encode_underline_style(style: UnderlineStyle) -> u8 {
    match style {
        UnderlineStyle::None => 0,
        UnderlineStyle::Straight => 1,
        UnderlineStyle::Double => 2,
        UnderlineStyle::Curly => 3,
        UnderlineStyle::Dotted => 4,
        UnderlineStyle::Dashed => 5,
    }
}

fn decode_underline_style(value: u8) -> Result<UnderlineStyle, SnapshotEnvelopeError> {
    match value {
        0 => Ok(UnderlineStyle::None),
        1 => Ok(UnderlineStyle::Straight),
        2 => Ok(UnderlineStyle::Double),
        3 => Ok(UnderlineStyle::Curly),
        4 => Ok(UnderlineStyle::Dotted),
        5 => Ok(UnderlineStyle::Dashed),
        value => Err(SnapshotEnvelopeError::InvalidEnum("UnderlineStyle", value)),
    }
}

fn encode_mouse_tracking(tracking: MouseTracking) -> u8 {
    match tracking {
        MouseTracking::Off => 0,
        MouseTracking::X10 => 1,
        MouseTracking::Normal => 2,
        MouseTracking::ButtonEvent => 3,
        MouseTracking::AnyEvent => 4,
    }
}

fn decode_mouse_tracking(value: u8) -> Result<MouseTracking, SnapshotEnvelopeError> {
    match value {
        0 => Ok(MouseTracking::Off),
        1 => Ok(MouseTracking::X10),
        2 => Ok(MouseTracking::Normal),
        3 => Ok(MouseTracking::ButtonEvent),
        4 => Ok(MouseTracking::AnyEvent),
        value => Err(SnapshotEnvelopeError::InvalidEnum("MouseTracking", value)),
    }
}

fn encode_mouse_encoding(encoding: MouseEncoding) -> u8 {
    match encoding {
        MouseEncoding::Default => 0,
        MouseEncoding::Utf8 => 1,
        MouseEncoding::Sgr => 2,
        MouseEncoding::Urxvt => 3,
        MouseEncoding::SgrPixel => 4,
    }
}

fn decode_mouse_encoding(value: u8) -> Result<MouseEncoding, SnapshotEnvelopeError> {
    match value {
        0 => Ok(MouseEncoding::Default),
        1 => Ok(MouseEncoding::Utf8),
        2 => Ok(MouseEncoding::Sgr),
        3 => Ok(MouseEncoding::Urxvt),
        4 => Ok(MouseEncoding::SgrPixel),
        value => Err(SnapshotEnvelopeError::InvalidEnum("MouseEncoding", value)),
    }
}

fn encode_prompt_kind(out: &mut Vec<u8>, kind: PromptKind) {
    match kind {
        PromptKind::PromptStart => write_u8(out, 0),
        PromptKind::OutputStart => write_u8(out, 1),
        PromptKind::CommandEnd { exit } => {
            write_u8(out, 2);
            encode_optional_exit(out, exit);
        }
        // Appended tag (same optional-exit shape as CommandEnd). An older
        // decoder reading a snapshot that contains this tag fails cleanly
        // with InvalidEnum rather than misreading the stream.
        PromptKind::PromptStartAfterEnd { prev_exit } => {
            write_u8(out, 3);
            encode_optional_exit(out, prev_exit);
        }
    }
}

fn encode_optional_exit(out: &mut Vec<u8>, exit: Option<i32>) {
    match exit {
        Some(exit) => {
            write_u8(out, 1);
            out.extend_from_slice(&exit.to_le_bytes());
        }
        None => write_u8(out, 0),
    }
}

fn decode_prompt_kind(reader: &mut Reader<'_>) -> Result<PromptKind, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(PromptKind::PromptStart),
        1 => Ok(PromptKind::OutputStart),
        2 => Ok(PromptKind::CommandEnd {
            exit: decode_optional_exit(reader)?,
        }),
        3 => Ok(PromptKind::PromptStartAfterEnd {
            prev_exit: decode_optional_exit(reader)?,
        }),
        value => Err(SnapshotEnvelopeError::InvalidEnum("PromptKind", value)),
    }
}

fn decode_optional_exit(reader: &mut Reader<'_>) -> Result<Option<i32>, SnapshotEnvelopeError> {
    match reader.read_u8()? {
        0 => Ok(None),
        1 => {
            let raw = reader.read_bytes(4)?;
            Ok(Some(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])))
        }
        value => Err(SnapshotEnvelopeError::InvalidBool(value)),
    }
}

fn default_tab_stops(columns: usize) -> Vec<bool> {
    let mut stops = vec![false; columns];
    for column in (8..columns).step_by(8) {
        stops[column] = true;
    }
    stops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_prompt_mark_count_fails_cleanly_without_over_reserve() {
        // A declared mark count at the cap with a near-empty payload must fail
        // on the first short read, not force a count-sized up-front
        // allocation. The capped reserve keeps the attempt bounded by what
        // the payload could actually encode; behavior (a clean error) is
        // unchanged for hostile and honest inputs alike.
        let caps = SnapshotEnvelopeCaps::default();
        let max = caps.max_scrollback_rows.saturating_add(caps.max_rows);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(max as u32).to_be_bytes());
        // One truncated mark's worth of payload, nowhere near `max` marks.
        bytes.extend_from_slice(&[0, 0]);
        assert!(
            decode_prompt_marks(&bytes, caps).is_err(),
            "a short payload behind a huge count must error"
        );

        // An honest small section still decodes.
        let marks = [SnapshotPromptMark {
            row: 3,
            kind: PromptKind::PromptStart,
        }];
        let encoded = encode_prompt_marks(&marks);
        let decoded = decode_prompt_marks(&encoded, caps).expect("honest section decodes");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].row, 3);
    }

    #[test]
    fn prompt_kind_wire_bytes_are_exact() {
        // Exact-byte pins for every PromptKind tag, including the appended
        // merged-prompt tag 3 (same optional-exit shape as CommandEnd). These
        // bytes are the cross-version contract; a drift here breaks decode on
        // the other side of an attach.
        let mut out = Vec::new();
        encode_prompt_kind(&mut out, PromptKind::PromptStart);
        assert_eq!(out, [0]);

        out.clear();
        encode_prompt_kind(&mut out, PromptKind::OutputStart);
        assert_eq!(out, [1]);

        out.clear();
        encode_prompt_kind(&mut out, PromptKind::CommandEnd { exit: None });
        assert_eq!(out, [2, 0]);

        out.clear();
        encode_prompt_kind(&mut out, PromptKind::CommandEnd { exit: Some(7) });
        assert_eq!(out, [2, 1, 7, 0, 0, 0]);

        out.clear();
        encode_prompt_kind(
            &mut out,
            PromptKind::PromptStartAfterEnd { prev_exit: None },
        );
        assert_eq!(out, [3, 0]);

        out.clear();
        encode_prompt_kind(
            &mut out,
            PromptKind::PromptStartAfterEnd {
                prev_exit: Some(258),
            },
        );
        assert_eq!(out, [3, 1, 2, 1, 0, 0]);
    }

    #[test]
    fn merged_prompt_mark_round_trips_through_the_section() {
        let caps = SnapshotEnvelopeCaps::default();
        let marks = [
            SnapshotPromptMark {
                row: 0,
                kind: PromptKind::PromptStart,
            },
            SnapshotPromptMark {
                row: 2,
                kind: PromptKind::PromptStartAfterEnd { prev_exit: Some(1) },
            },
            SnapshotPromptMark {
                row: 5,
                kind: PromptKind::PromptStartAfterEnd { prev_exit: None },
            },
        ];
        let encoded = encode_prompt_marks(&marks);
        let decoded = decode_prompt_marks(&encoded, caps).expect("section decodes");
        assert_eq!(decoded, marks);
    }

    #[test]
    fn unknown_prompt_kind_tag_fails_cleanly() {
        // Defensive decode: a future/corrupt tag is a clean InvalidEnum, not a
        // misread stream. This is what an older decoder reports when handed a
        // snapshot containing a tag it does not know.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes()); // count
        bytes.extend_from_slice(&3u32.to_le_bytes()); // row
        bytes.push(200); // unknown kind tag
        assert!(matches!(
            decode_prompt_marks(&bytes, SnapshotEnvelopeCaps::default()),
            Err(SnapshotEnvelopeError::InvalidEnum("PromptKind", 200))
        ));
    }

    fn sample_terminal() -> Terminal {
        let mut terminal = Terminal::new(16, 3);
        terminal.set_scrollback_limit(8);
        terminal.set_base_colors(
            RgbColor::new(0x11, 0x22, 0x33),
            RgbColor::new(0x04, 0x05, 0x06),
            RgbColor::new(0xAA, 0xBB, 0xCC),
        );
        terminal.advance(b"\x1b]2;Snapshot Test\x1b\\");
        terminal.advance(b"\x1b]7;file://localhost/tmp/odytty-snapshot\x1b\\");
        terminal.advance(
            b"alpha\nbeta\n\x1b[31mgamma\x1b[0m\n\x1b[?2004h\x1b[?1004h\x1b[?1006h\x1b[?1003h\x1b[?2026h\x1b[?1h\x1b=\x1b[6 q",
        );
        terminal.advance(b"\x1b]133;A\x07prompt\n\x1b]133;C\x07out\n\x1b]133;D;7\x07");
        terminal.advance(b"\x1b[2;3r\x1b[9G\x1b[0g");
        terminal.advance("wide \u{1f680}\ncomb e\u{301}".as_bytes());
        terminal
    }

    #[test]
    fn oversized_title_and_cwd_are_bounded_at_capture_so_reattach_succeeds() {
        // An OSC 2 title / OSC 7 cwd longer than the decoder's per-string cap
        // must not brick reattach: capture bounds each to the cap so the whole
        // envelope (grid, scrollback, modes) still decodes, with the strings
        // truncated rather than the file rejected.
        let mut terminal = Terminal::new(16, 3);
        let long_title = "T".repeat(5000);
        let long_cwd = format!("/tmp/{}", "d".repeat(5000));
        terminal.advance(format!("\x1b]2;{long_title}\x1b\\").as_bytes());
        terminal.advance(format!("\x1b]7;file://localhost{long_cwd}\x1b\\").as_bytes());
        terminal.advance(b"grid stays intact\n");

        let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        let bytes = envelope.encode().expect("encode");
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
            .expect("an oversized title/cwd must still decode, truncated");

        let title = decoded.metadata.title.expect("title present");
        assert!(
            title.len() <= DEFAULT_MAX_STRING_BYTES,
            "title bounded to the decode cap"
        );
        assert!(
            title.starts_with('T'),
            "title content preserved (truncated)"
        );
        let cwd = decoded
            .metadata
            .working_directory
            .expect("working directory present");
        assert!(
            cwd.len() <= DEFAULT_MAX_STRING_BYTES,
            "cwd bounded to the decode cap"
        );
        // The grid content survives the reattach that the unbounded string
        // would otherwise have aborted.
        assert_eq!(decoded.terminal.dimensions, Dimensions::new(16, 3));
    }

    #[test]
    fn truncate_to_char_boundary_never_splits_a_codepoint() {
        // A multibyte codepoint straddling the cut point is dropped whole, so
        // the result is always valid UTF-8 and within the byte bound.
        let value = "a".repeat(4095) + "\u{1f680}"; // 4-byte emoji crosses 4096
        let cut = truncate_to_char_boundary(&value, DEFAULT_MAX_STRING_BYTES);
        assert!(cut.len() <= DEFAULT_MAX_STRING_BYTES);
        assert_eq!(cut.len(), 4095, "the straddling emoji is dropped whole");
        // A string already within the bound is returned unchanged.
        assert_eq!(truncate_to_char_boundary("short", 4096), "short");
    }

    #[test]
    fn envelope_round_trip_is_byte_stable() {
        let terminal = sample_terminal();
        let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        let bytes = envelope.encode().expect("encode");
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        assert_eq!(decoded.encode().expect("encode"), bytes);
        assert_eq!(decoded.terminal.dimensions, Dimensions::new(16, 3));
        assert!(decoded.terminal.basic_modes.bracketed_paste);
        assert!(decoded.terminal.basic_modes.focus_reporting);
        assert_eq!(
            decoded.terminal.basic_modes.mouse.tracking,
            MouseTracking::AnyEvent
        );
        assert_eq!(
            decoded.terminal.basic_modes.mouse.encoding,
            MouseEncoding::Sgr
        );
        assert!(decoded.terminal.basic_modes.synchronized_output);
        assert!(decoded.terminal.basic_modes.keyboard.application_cursor);
        assert!(decoded.terminal.basic_modes.keyboard.application_keypad);
        assert_eq!(decoded.terminal.cursor_style, CursorStyle::Bar);
        assert!(!decoded.terminal.cursor_blink);
        assert_eq!(
            decoded.dynamic_colors.foreground,
            RgbColor::new(0x11, 0x22, 0x33)
        );
        assert_eq!(decoded.metadata.title.as_deref(), Some("Snapshot Test"));
        assert_eq!(
            decoded.metadata.working_directory.as_deref(),
            Some("/tmp/odytty-snapshot")
        );
        assert!(!decoded.prompt_marks.is_empty());
        assert_eq!(
            decoded.layout.scroll_region,
            Some(SnapshotScrollRegion { top: 1, bottom: 2 })
        );
        assert!(!decoded.layout.tab_stops[8]);
    }

    #[test]
    fn decoded_envelope_restores_fresh_terminal_state() {
        let original = sample_terminal();
        let limits = SnapshotCaptureLimits::default();
        let envelope = SnapshotEnvelope::from_terminal(&original, limits);
        let bytes = envelope.encode().expect("encode");
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();

        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(
            restored.snapshot_state(limits.max_scrollback_rows),
            decoded.terminal
        );
        assert_eq!(restored.dynamic_colors(), &decoded.dynamic_colors);
        assert_eq!(SnapshotMetadata::from_terminal(&restored), decoded.metadata);
        assert_eq!(
            restored
                .prompt_marks()
                .into_iter()
                .map(|(row, kind)| SnapshotPromptMark { row, kind })
                .collect::<Vec<_>>(),
            decoded.prompt_marks
        );
        assert_eq!(restored.snapshot_layout_state(), decoded.layout);
        assert_eq!(restored.snapshot(), original.snapshot());
        assert_eq!(restored.cursor_style(), CursorStyle::Bar);
        assert!(!restored.cursor_blinking());
        assert!(restored.synchronized_output_enabled());
        assert!(restored.bracketed_paste_enabled());
        assert!(restored.focus_reporting());
    }

    #[test]
    fn restore_into_existing_terminal_replaces_state_and_resets_parser() {
        let original = sample_terminal();
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&original, SnapshotCaptureLimits::default())
                .encode()
                .expect("encode"),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        let mut restored = Terminal::new(4, 1);
        restored.advance(b"stale\x1b[31");

        restored.restore_from_envelope(&decoded).unwrap();
        restored.advance(b"Z");

        let mut expected = Terminal::from_snapshot_envelope(&decoded).unwrap();
        expected.advance(b"Z");
        assert_eq!(restored.snapshot(), expected.snapshot());
    }

    #[test]
    fn v1_envelope_restores_with_current_defaults() {
        let terminal = sample_terminal();
        let mut terminal_payload =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .terminal
                .encode();
        // A v1 terminal payload predates the format v3 charset byte (the
        // last prelude byte, offset 30); strip it so the fixture is v1-shaped.
        terminal_payload.remove(30);
        let bytes = encode_sections_for_version(
            1,
            "v1-test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[SectionPayload {
                id: SECTION_TERMINAL_STATE,
                flags: SECTION_FLAG_REQUIRED,
                payload: terminal_payload,
            }],
        );
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(restored.dynamic_colors(), &DynamicColors::default());
        assert_eq!(
            SnapshotMetadata::from_terminal(&restored),
            SnapshotMetadata::default()
        );
        assert!(restored.prompt_marks().is_empty());
        assert_eq!(
            restored.snapshot_layout_state(),
            SnapshotLayoutState::defaults_for(decoded.terminal.dimensions)
        );
        assert_eq!(
            restored.snapshot_state(SnapshotCaptureLimits::default().max_scrollback_rows),
            decoded.terminal
        );
    }

    #[test]
    fn minimal_terminal_restores() {
        let terminal = Terminal::new(1, 1);
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .encode()
                .expect("encode"),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(restored.snapshot_state(0), decoded.terminal);
        assert_eq!(restored.snapshot(), terminal.snapshot());
    }

    #[test]
    fn capped_scrollback_restores_only_captured_tail() {
        let mut terminal = Terminal::new(8, 2);
        terminal.set_scrollback_limit(0);
        for index in 0..12 {
            terminal.advance(format!("line{index:02}\n").as_bytes());
        }
        let limits = SnapshotCaptureLimits {
            max_scrollback_rows: 4,
        };
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&terminal, limits)
                .encode()
                .expect("encode"),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        assert_eq!(decoded.terminal.scrollback_rows.len(), 4);

        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert_eq!(restored.screen().scrollback_len(), 4);
        assert_eq!(
            restored.snapshot_state(usize::MAX).scrollback_rows,
            decoded.terminal.scrollback_rows
        );
    }

    #[test]
    fn direct_encode_matches_the_table_driven_encoder() {
        // The shipping encoder writes the terminal section straight into the
        // frame buffer with a backpatched section table; its bytes must match
        // the retained copy-based table-driven construction exactly, for a
        // richly populated envelope (marks, metadata, scroll region, colors).
        let terminal = sample_terminal();
        let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        let oracle = encode_sections(
            &envelope.producer_version,
            envelope.protocol_version,
            &[
                SectionPayload {
                    id: SECTION_TERMINAL_STATE,
                    flags: SECTION_FLAG_REQUIRED,
                    payload: envelope.terminal.encode(),
                },
                SectionPayload {
                    id: SECTION_DYNAMIC_COLORS,
                    flags: 0,
                    payload: encode_dynamic_colors(&envelope.dynamic_colors),
                },
                SectionPayload {
                    id: SECTION_METADATA,
                    flags: 0,
                    payload: envelope.metadata.encode(),
                },
                SectionPayload {
                    id: SECTION_PROMPT_MARKS,
                    flags: 0,
                    payload: encode_prompt_marks(&envelope.prompt_marks),
                },
                SectionPayload {
                    id: SECTION_LAYOUT_STATE,
                    flags: 0,
                    payload: envelope.layout.encode(),
                },
            ],
        );
        assert_eq!(envelope.encode().expect("encode"), oracle);
    }

    #[test]
    fn maximal_cell_wire_len_is_pinned() {
        // Worst-case cell: RGB underline color, RGB foreground/background,
        // hyperlink id, all boolean payload bytes, and a full complement of
        // combining marks. Its encoded size must match the constant the
        // capture/resize budgets are derived from; if the wire format grows,
        // this test forces the budget constant to move with it.
        let cell = SnapshotCell {
            ch: '\u{10FFFD}',
            attrs: SnapshotAttrs {
                bold: true,
                dim: true,
                italic: true,
                underline: true,
                blink: true,
                strikethrough: true,
                inverse: true,
                hidden: true,
                underline_style: UnderlineStyle::Curly,
                underline_color: Some(Color::Rgb(1, 2, 3)),
                foreground: Color::Rgb(4, 5, 6),
                background: Color::Rgb(7, 8, 9),
                hyperlink: Some(77),
            },
            protected: true,
            wide_continuation: true,
            combining: vec!['\u{301}'; super::super::types::MAX_COMBINING],
        };
        let mut out = Vec::new();
        cell.encode(&mut out);
        assert_eq!(out.len(), MAX_CELL_WIRE_BYTES);
    }

    #[test]
    fn terminal_state_prelude_wire_len_is_pinned() {
        let state = Terminal::new(4, 2).snapshot_state(0);
        let mut out = Vec::new();
        state.encode_prelude(&mut out);
        assert_eq!(out.len(), TERMINAL_STATE_PRELUDE_WIRE_BYTES);
    }

    /// A blank-padded terminal state with `scrollback` scrollback rows, each
    /// row's first cell tagged with a row index so truncation order is
    /// observable.
    fn synthetic_state(columns: usize, rows: usize, scrollback: usize) -> SnapshotTerminalState {
        let make_row = |tag: char| {
            let mut cells: Vec<SnapshotCell> = vec![Cell::blank().into(); columns];
            cells[0].ch = tag;
            SnapshotRow {
                wrapped: false,
                cells,
            }
        };
        SnapshotTerminalState {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: true,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            basic_modes: Terminal::new(4, 2).snapshot_state(0).basic_modes,
            scrollback_rows: (0..scrollback)
                .map(|index| make_row(if index + 1 == scrollback { 'N' } else { 'o' }))
                .collect(),
            visible_rows: (0..rows).map(|_| make_row('v')).collect(),
        }
    }

    #[test]
    fn capture_bounding_truncates_oldest_scrollback_to_the_section_budget() {
        // 200 columns x 10k scrollback rows encodes a terminal section past
        // the decoder's section cap while staying under the frame cap: the
        // exact shape that made a session permanently un-attachable (the host
        // served a snapshot its own default consumers rejected). Bounding
        // must shed the OLDEST rows until the state is self-decodable.
        let mut state = synthetic_state(200, 50, 10_000);
        let unbounded = state.clone().encode();
        assert!(
            unbounded.len() > SnapshotEnvelopeCaps::default().max_section_len,
            "repro shape must exceed the section budget to prove the coupling"
        );

        let dropped = state.bound_to_decode_budget();
        assert!(dropped > 0, "over-budget state must shed rows");
        assert!(
            state.encode().len() <= SnapshotEnvelopeCaps::default().max_section_len,
            "bounded terminal section fits the decode budget"
        );
        // Newest row survives; only the oldest rows were shed.
        assert_eq!(
            state.scrollback_rows.last().expect("rows remain").cells[0].ch,
            'N'
        );

        // Pairwise invariant: the bounded capture decodes under the same
        // default caps its consumers use.
        let envelope = SnapshotEnvelope {
            producer_version: "test".to_owned(),
            protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            terminal: state,
            dynamic_colors: DynamicColors::default(),
            metadata: SnapshotMetadata::default(),
            prompt_marks: Vec::new(),
            layout: SnapshotLayoutState::defaults_for(Dimensions::new(200, 50)),
        };
        let decoded = SnapshotEnvelope::decode(
            &envelope.encode().expect("encode"),
            SnapshotEnvelopeCaps::default(),
        )
        .expect("bounded capture output decodes under matching defaults");
        assert_eq!(
            decoded.terminal.scrollback_rows.len(),
            envelope.terminal.scrollback_rows.len()
        );
    }

    #[test]
    fn capture_bounding_enforces_row_and_cell_budgets() {
        // Small custom caps make the count-based arms observable without
        // building 100k-row fixtures: the row-count cap and the total-cell
        // cap must each shed oldest scrollback independently of byte size.
        let caps = SnapshotEnvelopeCaps {
            max_scrollback_rows: 3,
            ..SnapshotEnvelopeCaps::default()
        };
        let mut state = synthetic_state(4, 2, 10);
        assert_eq!(state.bound_to_decode_budget_with(&caps), 7);
        assert_eq!(state.scrollback_rows.len(), 3);
        assert_eq!(state.scrollback_rows[2].cells[0].ch, 'N');

        let caps = SnapshotEnvelopeCaps {
            // 4 columns x (2 visible + 10 scrollback) = 48 cells; a 28-cell
            // budget leaves room for 5 scrollback rows beside the visible 2.
            max_cells: 28,
            ..SnapshotEnvelopeCaps::default()
        };
        let mut state = synthetic_state(4, 2, 10);
        assert_eq!(state.bound_to_decode_budget_with(&caps), 5);
        assert_eq!(state.scrollback_rows.len(), 5);
    }

    #[test]
    fn deep_wide_session_snapshot_decodes_and_restores_end_to_end() {
        // End-to-end regression for the 200-column repro: a real terminal
        // with capture-limit-deep scrollback and prompt marks near the tail
        // must produce an envelope that decodes AND restores under default
        // caps, with marks rebased onto the truncated history.
        let mut terminal = Terminal::new(200, 50);
        terminal.set_scrollback_limit(SnapshotCaptureLimits::default().max_scrollback_rows);
        for index in 0..10_100u32 {
            terminal.advance(format!("row {index}\r\n").as_bytes());
        }
        terminal.advance(b"\x1b]133;A\x07tail prompt\r\n");

        let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        let bytes = envelope.encode().expect("encode");
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
            .expect("deep wide capture decodes under default caps");
        assert!(
            decoded.terminal.scrollback_rows.len() < 10_000,
            "scrollback was truncated to fit the decode budget"
        );
        let total_rows =
            decoded.terminal.scrollback_rows.len() + decoded.terminal.visible_rows.len();
        assert!(!decoded.prompt_marks.is_empty(), "tail mark survives");
        assert!(
            decoded
                .prompt_marks
                .iter()
                .all(|mark| mark.row < total_rows),
            "marks are rebased inside the truncated history"
        );
        let restored =
            Terminal::from_snapshot_envelope(&decoded).expect("bounded snapshot restores");
        assert!(!restored.prompt_marks().is_empty());
    }

    #[test]
    fn capture_limit_truncation_rebases_prompt_marks() {
        // Marks on rows older than the capture window are dropped; marks on
        // captured rows shift down by the number of rows cut ahead of them.
        let mut terminal = Terminal::new(8, 2);
        terminal.set_scrollback_limit(0);
        terminal.advance(b"\x1b]133;A\x07p0\n");
        for index in 0..10 {
            terminal.advance(format!("line{index:02}\n").as_bytes());
        }
        terminal.advance(b"\x1b]133;A\x07p1\n");

        let limits = SnapshotCaptureLimits {
            max_scrollback_rows: 4,
        };
        let envelope = SnapshotEnvelope::from_terminal(&terminal, limits);
        assert_eq!(envelope.terminal.scrollback_rows.len(), 4);
        let full_marks = terminal.prompt_marks();
        assert_eq!(full_marks.len(), 2, "both marks live in full history");
        let dropped = terminal.screen().scrollback_len() - 4;
        let expected: Vec<_> = full_marks
            .into_iter()
            .filter_map(|(row, kind)| {
                row.checked_sub(dropped)
                    .map(|row| SnapshotPromptMark { row, kind })
            })
            .collect();
        assert_eq!(expected.len(), 1, "the pre-window mark is dropped");
        assert_eq!(envelope.prompt_marks, expected);
        // The rebased envelope restores cleanly (an unrebased mark would be
        // rejected as out of range or land on the wrong row).
        Terminal::from_snapshot_envelope(&envelope).expect("rebased marks restore");
    }

    #[test]
    fn resize_budget_guarantees_worst_case_visible_grid_decodes() {
        // The advertised visible-cell budget must be honest: a grid at the
        // budget filled entirely with worst-case cells still encodes within
        // the section cap, with zero scrollback room required.
        let caps = SnapshotEnvelopeCaps::default();
        let budget = caps.max_self_decodable_visible_cells();
        assert!(budget >= 500_000, "budget covers any realistic display");
        assert!(budget <= caps.max_cells);
        // Worst-case per-cell wire size at the budget fits the section cap.
        let worst = budget
            .checked_mul(MAX_CELL_WIRE_BYTES)
            .and_then(|cells| {
                cells.checked_add(caps.max_rows.checked_mul(ROW_WIRE_OVERHEAD_BYTES)?)
            })
            .and_then(|total| total.checked_add(TERMINAL_STATE_PRELUDE_WIRE_BYTES + 8))
            .expect("budget arithmetic stays in range");
        assert!(worst <= caps.max_section_len);
    }

    #[test]
    fn active_alternate_screen_restores_active_grid_and_mode_flag() {
        let mut terminal = Terminal::new(10, 3);
        terminal.advance(b"primary\nhistory\n");
        terminal.advance(b"\x1b[?1049h\x1b[2J\x1b[Halt-one\nalt-two\x1b[4 q");
        let decoded = SnapshotEnvelope::decode(
            &SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .encode()
                .expect("encode"),
            SnapshotEnvelopeCaps::default(),
        )
        .unwrap();
        assert!(decoded.terminal.basic_modes.alternate_screen);

        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

        assert!(restored.on_alternate_screen());
        assert_eq!(
            restored.snapshot_state(SnapshotCaptureLimits::default().max_scrollback_rows),
            decoded.terminal
        );
        assert_eq!(restored.snapshot(), terminal.snapshot());
        assert_eq!(restored.cursor_style(), CursorStyle::Underline);
    }

    #[test]
    fn invalid_prompt_mark_is_rejected_on_restore() {
        let terminal = Terminal::new(2, 1);
        let mut envelope =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        envelope.prompt_marks.push(SnapshotPromptMark {
            row: 99,
            kind: PromptKind::PromptStart,
        });

        assert!(matches!(
            Terminal::from_snapshot_envelope(&envelope),
            Err(SnapshotEnvelopeError::InvalidPromptMark { row: 99, rows: 1 })
        ));
    }

    #[test]
    fn unknown_optional_section_is_ignored() {
        let terminal = sample_terminal();
        let terminal_payload =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .terminal
                .encode();
        let bytes = encode_sections(
            "test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[
                SectionPayload {
                    id: 77,
                    flags: 0,
                    payload: vec![1, 2, 3],
                },
                SectionPayload {
                    id: SECTION_TERMINAL_STATE,
                    flags: SECTION_FLAG_REQUIRED,
                    payload: terminal_payload,
                },
            ],
        );
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        assert_eq!(decoded.producer_version, "test");
    }

    #[test]
    fn unknown_required_section_is_rejected() {
        let bytes = encode_sections(
            "test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[SectionPayload {
                id: 88,
                flags: SECTION_FLAG_REQUIRED,
                payload: vec![1, 2, 3],
            }],
        );
        assert_eq!(
            SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()),
            Err(SnapshotEnvelopeError::UnknownRequiredSection(88))
        );
    }

    #[test]
    fn version_mismatch_is_rejected_cleanly() {
        let terminal = sample_terminal();
        let mut bytes =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .encode()
                .expect("encode");
        bytes[SNAPSHOT_MAGIC.len()] = 99;
        assert_eq!(
            SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()),
            Err(SnapshotEnvelopeError::UnsupportedVersion {
                format_version: 99,
                protocol_version: SNAPSHOT_PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn oversized_section_is_rejected_by_cap() {
        let terminal = sample_terminal();
        let bytes = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
            .encode()
            .expect("encode");
        let caps = SnapshotEnvelopeCaps {
            max_section_len: 1,
            ..SnapshotEnvelopeCaps::default()
        };
        let err = SnapshotEnvelope::decode(&bytes, caps).unwrap_err();
        assert!(matches!(err, SnapshotEnvelopeError::SectionTooLarge { .. }));
    }

    #[test]
    fn v1_envelope_decodes_with_v2_defaults() {
        let terminal = sample_terminal();
        let mut terminal_payload =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
                .terminal
                .encode();
        // A v1 terminal payload predates the format v3 charset byte (the
        // last prelude byte, offset 30); strip it so the fixture is v1-shaped.
        terminal_payload.remove(30);
        let bytes = encode_sections_for_version(
            1,
            "v1-test",
            SNAPSHOT_PROTOCOL_VERSION,
            &[SectionPayload {
                id: SECTION_TERMINAL_STATE,
                flags: SECTION_FLAG_REQUIRED,
                payload: terminal_payload,
            }],
        );
        let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
        assert_eq!(decoded.producer_version, "v1-test");
        assert_eq!(decoded.dynamic_colors, DynamicColors::default());
        assert_eq!(decoded.metadata, SnapshotMetadata::default());
        assert!(decoded.prompt_marks.is_empty());
        assert_eq!(
            decoded.layout,
            SnapshotLayoutState::defaults_for(decoded.terminal.dimensions)
        );
    }
}

/// Wire-bound validation (fallible encode) and byte-identity coverage: encode
/// refuses externally constructed envelopes whose `usize` fields exceed their
/// narrowed on-wire integer widths instead of truncating them into bytes the
/// envelope's own decoder cannot read, while the capture path
/// (`from_terminal`) remains structurally bounded and byte-identical.
#[cfg(test)]
mod wire_bound_tests {
    use super::*;

    fn sample_envelope() -> SnapshotEnvelope {
        let mut terminal = Terminal::new(4, 2);
        terminal.advance(b"hi");
        SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
    }

    fn expect_too_large(envelope: &SnapshotEnvelope, what: &str) {
        match envelope.encode() {
            Err(SnapshotEnvelopeError::ValueTooLarge { what: got, .. }) => {
                assert_eq!(got, what);
            }
            Err(other) => panic!("expected ValueTooLarge({what}), got {other:?}"),
            Ok(_) => panic!("expected ValueTooLarge({what}), got Ok"),
        }
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn oversized_u32_fields_refuse_to_encode() {
        const OVER: usize = u32::MAX as usize + 1;

        let mut envelope = sample_envelope();
        envelope.terminal.cursor.row = OVER;
        expect_too_large(&envelope, "cursor row");

        let mut envelope = sample_envelope();
        envelope.terminal.dimensions = Dimensions::new(OVER, 2);
        expect_too_large(&envelope, "columns");

        let mut envelope = sample_envelope();
        envelope.prompt_marks.push(SnapshotPromptMark {
            row: OVER,
            kind: PromptKind::PromptStart,
        });
        expect_too_large(&envelope, "prompt mark row");

        let mut envelope = sample_envelope();
        envelope.layout.scroll_region = Some(SnapshotScrollRegion {
            top: OVER,
            bottom: OVER,
        });
        expect_too_large(&envelope, "scroll region top");
    }

    #[test]
    fn oversized_title_refuses_to_encode() {
        let mut envelope = sample_envelope();
        envelope.metadata.title = Some("t".repeat(u16::MAX as usize + 1));
        expect_too_large(&envelope, "title length");
    }

    #[test]
    fn oversized_producer_version_refuses_to_encode() {
        // Header sibling of the section-string checks: without validation a
        // producer version one past the u16 width would encode with a
        // zero-truncated length prefix and desync every byte after the
        // header. `from_terminal` cannot hit this (compile-time package
        // version), so only externally constructed envelopes are affected.
        let mut envelope = sample_envelope();
        envelope.producer_version = "v".repeat(u16::MAX as usize + 1);
        assert!(matches!(
            envelope.validate_wire_bounds(),
            Err(SnapshotEnvelopeError::ValueTooLarge {
                what: "producer version length",
                ..
            })
        ));
        expect_too_large(&envelope, "producer version length");
    }

    #[test]
    fn producer_version_at_the_u16_wire_maximum_round_trips() {
        let mut envelope = sample_envelope();
        envelope.producer_version = "v".repeat(u16::MAX as usize);
        envelope
            .validate_wire_bounds()
            .expect("boundary producer version validates");
        let bytes = envelope
            .encode()
            .expect("boundary producer version encodes");
        let caps = SnapshotEnvelopeCaps {
            max_string_bytes: 80_000,
            ..SnapshotEnvelopeCaps::default()
        };
        let decoded =
            SnapshotEnvelope::decode(&bytes, caps).expect("boundary producer version decodes");
        assert_eq!(decoded.producer_version, envelope.producer_version);
    }

    #[test]
    fn oversized_combining_count_refuses_to_encode() {
        let mut envelope = sample_envelope();
        envelope.terminal.visible_rows[0].cells[0].combining =
            vec!['\u{0301}'; u8::MAX as usize + 1];
        expect_too_large(&envelope, "combining mark count");
    }

    #[test]
    fn title_at_the_u16_wire_maximum_round_trips() {
        let mut envelope = sample_envelope();
        envelope.metadata.title = Some("t".repeat(u16::MAX as usize));
        let bytes = envelope.encode().expect("boundary title encodes");
        let caps = SnapshotEnvelopeCaps {
            max_string_bytes: 80_000,
            ..SnapshotEnvelopeCaps::default()
        };
        let decoded = SnapshotEnvelope::decode(&bytes, caps).expect("boundary title decodes");
        assert_eq!(decoded.metadata.title, envelope.metadata.title);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn prompt_mark_row_at_the_u32_wire_maximum_round_trips() {
        let mut envelope = sample_envelope();
        envelope.prompt_marks.push(SnapshotPromptMark {
            row: u32::MAX as usize,
            kind: PromptKind::PromptStart,
        });
        let bytes = envelope.encode().expect("boundary mark encodes");
        let decoded =
            SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).expect("decodes");
        assert_eq!(decoded.prompt_marks, envelope.prompt_marks);
    }

    #[test]
    fn from_terminal_encode_bytes_are_pinned() {
        // Full-envelope byte identity for a fixed capture: any change to this
        // fixture is a deliberate wire-format change (bump the snapshot format
        // version and regenerate), never an accident of refactoring.
        let mut terminal = Terminal::new(4, 2);
        terminal.advance(b"hi");
        let mut envelope =
            SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
        envelope.producer_version = "pin".to_owned();
        let bytes = envelope.encode().expect("encode");
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let expected = concat!(
            "4f44595454592d534e415053484f5403000100030070696e050001000100b900",
            "0000000000000200000009010000000000000300000002000000000000000400",
            "0000040000000000000005000000090000000000000004000000020000000000",
            "0000020000000100010001000000000000000000000000000002000000000400",
            "0000680000000000000000000000000000000069000000000000000000000000",
            "0000000020000000000000000000000000000000002000000000000000000000",
            "0000000000000004000000200000000000000000000000000000000020000000",
            "0000000000000000000000000020000000000000000000000000000000002000",
            "000000000000000000000000000000cccccc0b0c10cccccc0000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000004",
            "00000000000000",
        );
        assert_eq!(hex, expected);
    }
}

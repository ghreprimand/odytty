// SPDX-License-Identifier: GPL-3.0-only
//! Bounds every field must satisfy before it is narrowed onto the wire, and
//! the structural checks a decoded layout must satisfy against its own
//! dimensions.
//!
//! Encoding validates first and refuses, rather than truncating a `usize`
//! field into bytes this envelope's own decoder cannot read.

use crate::core::types::Dimensions;

use super::error::SnapshotEnvelopeError;
use super::model::{
    SnapshotCell, SnapshotEnvelope, SnapshotLayoutState, SnapshotMetadata, SnapshotTerminalState,
};

impl SnapshotEnvelope {
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
        self.layout.validate_streaming_owner(&self.terminal)?;
        Ok(())
    }
}

impl SnapshotMetadata {
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
}

impl SnapshotLayoutState {
    pub(in crate::core) fn validate_streaming_owner(
        &self,
        terminal: &SnapshotTerminalState,
    ) -> Result<(), SnapshotEnvelopeError> {
        let columns = terminal.dimensions.columns;
        if self.pending_wrap && terminal.cursor.column != columns.saturating_sub(1) {
            return Err(SnapshotEnvelopeError::InvalidCursor {
                cursor: terminal.cursor,
            });
        }
        if let Some(owner) = self.cluster_owner {
            let cell = terminal
                .visible_rows
                .get(owner.row)
                .and_then(|r| r.cells.get(owner.column));
            let valid = cell.is_some_and(|c| !c.wide_continuation && !c.layout_padding)
                && owner.row == terminal.cursor.row
                && owner.row < terminal.dimensions.rows
                && owner.column < columns;
            if !valid {
                return Err(SnapshotEnvelopeError::InvalidCursor { cursor: owner });
            }
            let wide = terminal.visible_rows[owner.row]
                .cells
                .get(owner.column + 1)
                .is_some_and(|c| c.wide_continuation);
            let after = owner
                .column
                .saturating_add(if wide {
                    2
                } else {
                    crate::core::char_width::char_display_width(cell.unwrap().ch, false).max(1)
                })
                .min(columns.saturating_sub(1));
            if terminal.cursor.column != after {
                return Err(SnapshotEnvelopeError::InvalidCursor { cursor: owner });
            }
        }
        Ok(())
    }

    /// The layout half of [`SnapshotEnvelope::validate_wire_bounds`]: the
    /// scroll-region bounds and the tab-stop count travel as `u32`.
    fn validate_wire_bounds(&self) -> Result<(), SnapshotEnvelopeError> {
        if let Some(region) = self.scroll_region {
            check_u32(region.top, "scroll region top")?;
            check_u32(region.bottom, "scroll region bottom")?;
        }
        if let Some(owner) = self.cluster_owner {
            check_u32(owner.row, "cluster owner row")?;
            check_u32(owner.column, "cluster owner column")?;
        }
        check_u32(self.tab_stops.len(), "tab stop count")?;
        if self.pending_utf8.len() > super::format::MAX_PENDING_UTF8_BYTES {
            return Err(SnapshotEnvelopeError::ValueTooLarge {
                what: "pending UTF-8 bytes",
                value: self.pending_utf8.len(),
                max: super::format::MAX_PENDING_UTF8_BYTES,
            });
        }
        Ok(())
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
        // The pending bytes must be the start of one scalar and nothing else:
        // anything a later byte could not complete would restore as U+FFFD.
        if !self.pending_utf8.is_empty()
            && !std::str::from_utf8(&self.pending_utf8)
                .is_err_and(|error| error.valid_up_to() == 0 && error.error_len().is_none())
        {
            return Err(SnapshotEnvelopeError::InvalidUtf8);
        }
        Ok(())
    }
}

impl SnapshotTerminalState {
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
                if cell.combining.len() > crate::core::types::MAX_COMBINING {
                    return Err(SnapshotEnvelopeError::ValueTooLarge {
                        what: "combining mark count",
                        value: cell.combining.len(),
                        max: crate::core::types::MAX_COMBINING,
                    });
                }
                if cell.layout_padding
                    && (cell.ch != ' '
                        || cell.protected
                        || cell.wide_continuation
                        || !cell.combining.is_empty())
                {
                    return Err(SnapshotEnvelopeError::InvalidEnum("layout padding", 2));
                }
            }
        }
        Ok(())
    }
}

impl SnapshotTerminalState {
    /// Refuse a scrollback logical line holding more source cells than the
    /// live store retains in one line (`MAX_LOGICAL_LINE_CELLS`). The live
    /// store trims an open line back below that ceiling and never extends a
    /// closed one, so no honest capture exceeds it; restore refuses rather than
    /// trimming, which would hide a corrupt envelope.
    ///
    /// Only scrollback rows count: a line continuing into wrapped visible rows
    /// is measured by its scrollback part, as the live store holds it. Layout
    /// padding slots are not source cells, and the blank fill on the final row
    /// of each run is not either: capture re-projects history at the current
    /// width, which drops and adds padding and pads the last row to full width,
    /// so counting those slots would refuse an honest capture taken after a
    /// width change.
    pub(in crate::core) fn validate_logical_line_ceiling(
        &self,
    ) -> Result<(), SnapshotEnvelopeError> {
        use crate::core::scrollback::MAX_LOGICAL_LINE_CELLS;
        let fill = SnapshotCell::from(crate::core::types::Cell::blank());
        let last = self.scrollback_rows.len().saturating_sub(1);
        let mut cells = 0usize;
        for (index, row) in self.scrollback_rows.iter().enumerate() {
            let ends_run = !row.wrapped || index == last;
            let counted = if ends_run {
                let end = row
                    .cells
                    .iter()
                    .rposition(|cell| *cell != fill && !cell.layout_padding)
                    .map_or(0, |position| position + 1);
                &row.cells[..end]
            } else {
                &row.cells[..]
            };
            cells = cells.saturating_add(counted.iter().filter(|c| !c.layout_padding).count());
            if cells > MAX_LOGICAL_LINE_CELLS {
                return Err(SnapshotEnvelopeError::LogicalLineTooLarge {
                    cells,
                    max: MAX_LOGICAL_LINE_CELLS,
                });
            }
            if !row.wrapped {
                cells = 0;
            }
        }
        Ok(())
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

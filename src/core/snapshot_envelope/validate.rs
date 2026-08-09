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
    SnapshotEnvelope, SnapshotLayoutState, SnapshotMetadata, SnapshotTerminalState,
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

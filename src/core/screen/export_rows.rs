// SPDX-License-Identifier: GPL-3.0-only
//! Forward-order chunks of the whole scrollback-plus-screen buffer, for
//! exporting every row without re-projecting the scrollback per viewport.

use super::*;
use crate::graphics::PlacementId;

/// Consecutive rows of the combined buffer, read by
/// [`Screen::export_chunk`].
#[derive(Debug, Default)]
pub struct ExportChunk {
    /// The rows, oldest first, with their soft-wrap flags.
    pub rows: Vec<VisibleRow>,
    /// Image placements visible in this chunk, each with the chunk-relative
    /// row where it first appears (clamped to the chunk's top row when the
    /// placement starts above it). Includes Kitty Unicode-placeholder
    /// placements resolved from the chunk's cells.
    pub placements: Vec<(PlacementId, usize)>,
}

impl Screen {
    /// Rows in the combined buffer: projected scrollback plus the live screen.
    pub fn export_row_count(&self) -> usize {
        self.scrollback.physical_len(self.dimensions.columns) + self.rows.len()
    }

    /// Rows of the combined buffer starting at absolute row `start` (row `0`
    /// is the oldest scrollback row). Inside the scrollback the chunk holds up
    /// to `max_rows` rows and stops at the scrollback's end; from the first
    /// live-screen row on it holds the rest of the live screen, which is
    /// bounded by the screen height. Each call projects only the rows it
    /// returns, so walking the buffer front to back is linear in its size.
    pub fn export_chunk(&self, start: usize, max_rows: usize) -> ExportChunk {
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.physical_len(columns);
        let mut chunk = ExportChunk::default();
        if start < scrollback_len {
            let count = max_rows.min(scrollback_len - start);
            let lines = self.scrollback.physical_range(columns, start, count);
            // The graphics scene anchors rows relative to the live screen top,
            // so a window whose top is `start` sits `scrollback_len - start`
            // rows above it.
            let visible = self.graphics.visible_placements(
                scrollback_len - start,
                lines.len(),
                columns,
                self.cell_metrics.height_px,
            );
            chunk.placements = visible.iter().map(|p| (p.id, p.row)).collect();
            self.push_export_rows(lines, &mut chunk);
        } else {
            let first = start - scrollback_len;
            if first >= self.rows.len() {
                return chunk;
            }
            let visible = self.graphics.visible_placements(
                0,
                self.rows.len(),
                columns,
                self.cell_metrics.height_px,
            );
            chunk.placements = visible
                .iter()
                .filter(|p| p.row + p.display_rows > first)
                .map(|p| (p.id, p.row.saturating_sub(first)))
                .collect();
            self.push_export_rows(self.rows[first..].iter().cloned(), &mut chunk);
        }
        chunk
    }

    fn push_export_rows(&self, lines: impl IntoIterator<Item = Line>, chunk: &mut ExportChunk) {
        let placeholders = self.graphics.has_virtual_placements();
        let mut resolved = Vec::new();
        for line in lines {
            if placeholders {
                placeholder::collect_row_placeholders(
                    &self.graphics,
                    chunk.rows.len(),
                    &line.cells,
                    &mut resolved,
                );
            }
            chunk.rows.push(VisibleRow {
                cells: line.cells,
                wrapped: line.wrapped,
            });
        }
        chunk
            .placements
            .extend(resolved.iter().map(|p| (p.id, p.row)));
    }
}

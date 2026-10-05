// SPDX-License-Identifier: GPL-3.0-only
//! Test-only bidi display gate for the live single-pane frame and its pointer.
//!
//! While a test sets `bidi_display_for_test`, the single-pane frame plans a
//! [`BidiDisplayMap`] for the content grid (with the paragraph context above
//! the viewport), resets every row an overlay painter changed back to the
//! identity layout, places the map inside the decorated frame (chrome stays in
//! identity layout), and hands it to the renderer. The pointer maps a content
//! cell under the pointer to the logical cell drawn there through the same
//! last-built map. Cursor, selection, search, copy, and every terminal
//! protocol value stay logical.
//!
//! No setting, flag, env var, or menu reaches the gate: in a shipping build
//! every method here returns the identity answer. Multi-pane tabs are outside
//! this slice and never take a map. Platform-neutral.

use super::*;
use crate::core::Snapshot;
use crate::grid::BidiDisplayMap;

impl App {
    /// The display map for the single-pane content grid `snapshot` at
    /// scrollback `offset`, or `None` while the test-only gate is off, in a
    /// multi-pane tab, or in a shipping build.
    pub(super) fn bidi_content_map(
        &self,
        terminal: &crate::core::Terminal,
        snapshot: &Snapshot,
        offset: usize,
    ) -> Option<BidiDisplayMap> {
        #[cfg(test)]
        if self.bidi_display_for_test && self.sessions.active_is_single_pane() {
            return Some(plan_content_map(terminal, snapshot, offset));
        }
        let _ = (terminal, snapshot, offset);
        None
    }

    /// Record the map the presented single-pane frame used for its content
    /// grid, after overlay rows were reset. The pointer maps through it.
    pub(super) fn set_bidi_frame_map(&mut self, map: Option<BidiDisplayMap>) {
        #[cfg(test)]
        {
            self.bidi_frame_map = map;
        }
        #[cfg(not(test))]
        let _ = map;
    }

    /// The logical content cell drawn at the content cell `point` the pointer
    /// is over. Identity unless the test-only gate built a frame map.
    pub(super) fn bidi_logical_point(&self, point: CellPoint) -> CellPoint {
        #[cfg(test)]
        if self.bidi_display_for_test
            && self.sessions.active_is_single_pane()
            && let Some(map) = self.bidi_frame_map.as_ref()
        {
            return CellPoint {
                row: point.row,
                column: map.logical_column(point.row, point.column),
            };
        }
        point
    }
}

/// Plan the content map from the terminal's soft-wrap flags and the
/// paragraph context above the viewport.
#[cfg(test)]
pub(super) fn plan_content_map(
    terminal: &crate::core::Terminal,
    snapshot: &Snapshot,
    offset: usize,
) -> BidiDisplayMap {
    use crate::grid::{BidiParagraphContext, max_bidi_context_rows};
    let wrapped: Vec<bool> = terminal
        .visible_search_rows(offset)
        .iter()
        .map(|row| row.wrapped)
        .collect();
    let max_rows = max_bidi_context_rows(snapshot.dimensions.columns);
    let (rows, overflow) = terminal.paragraph_context_rows(offset, max_rows);
    let context = BidiParagraphContext {
        rows: rows.into_iter().map(|row| row.cells).collect(),
        overflow,
    };
    BidiDisplayMap::plan_with_context(snapshot, &wrapped, &context)
}

#[cfg(test)]
impl App {
    /// Test seam: turn the display gate on and record the content map a
    /// presented frame of the current viewport would use (no overlay is
    /// painted in a headless test), so pointer tests run without a GPU.
    pub(in crate::native) fn present_bidi_frame_map_for_test(&mut self) {
        self.bidi_display_for_test = true;
        let map = {
            let terminal = crate::native::lock_recover(&self.terminal);
            let offset = self
                .viewport
                .offset()
                .min(terminal.screen().scrollback_len());
            let snapshot = terminal.snapshot_with_scrollback(offset);
            self.bidi_content_map(&terminal, &snapshot, offset)
        };
        self.set_bidi_frame_map(map);
    }

    /// Test seam: the content map the pointer currently maps through.
    pub(in crate::native) fn bidi_frame_map_for_test(&self) -> Option<&BidiDisplayMap> {
        self.bidi_frame_map.as_ref()
    }
}

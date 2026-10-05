// SPDX-License-Identifier: GPL-3.0-only
//! Test-only bidi display gate for the live frame, its pointer, and the
//! mouse reports the pointer sends.
//!
//! While a test sets `bidi_display_for_test`, the single-pane frame and each
//! content pane of a split frame plan a [`BidiDisplayMap`] for their content
//! grid (with the paragraph context above the viewport), reset every row an
//! overlay painter changed back to the identity layout, and hand the map to
//! the renderer. The single-pane map is placed inside the decorated frame, so
//! chrome stays in identity layout; split chrome strips never take a map. The
//! pointer maps a content cell under the pointer to the logical cell drawn
//! there through the focused content map, so selection, hover, and every
//! cell-encoded mouse report address logical cells. An SGR-pixel report moves
//! by whole cells onto the logical cell and keeps its offset inside the cell.
//! Cursor, selection, search, copy, and every terminal protocol value stay
//! logical.
//!
//! No setting, flag, env var, or menu reaches the gate: in a shipping build
//! every method here returns the identity answer. Platform-neutral.

use super::*;
use crate::core::Snapshot;
use crate::grid::BidiDisplayMap;

impl App {
    /// The display map for the single-pane content grid `snapshot` at
    /// scrollback `offset`, or `None` while the test-only gate is off, in a
    /// multi-pane tab (see [`Self::bidi_pane_plan`]), or in a shipping build.
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

    /// The plan for one split pane's content grid `snapshot` at scrollback
    /// `offset`, with a copy of the snapshot it was planned from so rows an
    /// overlay painter changes can be reset. `None` while the test-only gate
    /// is off and in a shipping build.
    pub(super) fn bidi_pane_plan(
        &self,
        terminal: &crate::core::Terminal,
        snapshot: &Snapshot,
        offset: usize,
    ) -> Option<(BidiDisplayMap, Snapshot)> {
        #[cfg(test)]
        if self.bidi_display_for_test {
            return Some((
                plan_content_map(terminal, snapshot, offset),
                snapshot.clone(),
            ));
        }
        let _ = (terminal, snapshot, offset);
        None
    }

    /// Record the maps each content pane of the presented split frame used,
    /// after overlay rows were reset. The pointer maps through the focused
    /// pane's map.
    pub(super) fn set_bidi_pane_maps(&mut self, maps: Vec<(SessionToken, BidiDisplayMap)>) {
        #[cfg(test)]
        {
            self.bidi_pane_maps = maps;
        }
        #[cfg(not(test))]
        let _ = maps;
    }

    /// The content map of the focused content grid: the single-pane frame
    /// map, or the focused pane's map in a split tab.
    #[cfg(test)]
    fn bidi_focused_map(&self) -> Option<&BidiDisplayMap> {
        if !self.bidi_display_for_test {
            return None;
        }
        if self.sessions.active_is_single_pane() {
            return self.bidi_frame_map.as_ref();
        }
        let focused = self.sessions.active_id();
        self.bidi_pane_maps
            .iter()
            .find(|(token, _)| *token == focused)
            .map(|(_, map)| map)
    }

    /// The logical content cell drawn at the focused content cell `point` the
    /// pointer is over. Identity unless the test-only gate built a map.
    pub(super) fn bidi_logical_point(&self, point: CellPoint) -> CellPoint {
        #[cfg(test)]
        if let Some(map) = self.bidi_focused_map() {
            return CellPoint {
                row: point.row,
                column: map.logical_column(point.row, point.column),
            };
        }
        point
    }

    /// The screen column the presented frame drew the focused content cell
    /// `point` at. Identity unless the test-only gate built a map.
    pub(super) fn bidi_visual_column(&self, point: CellPoint) -> usize {
        #[cfg(test)]
        if let Some(map) = self.bidi_focused_map() {
            return map.visual_column(point.row, point.column);
        }
        point.column
    }

    /// Move a 1-based grid-relative SGR-pixel report coordinate onto the
    /// logical cell drawn under it, keeping its offset inside the cell, so a
    /// pixel report addresses the same cell the cell-encoded reports name.
    /// Identity unless the test-only gate built a map.
    pub(super) fn bidi_logical_report_px(
        &self,
        (px, py): (usize, usize),
        cell: CellSize,
    ) -> (usize, usize) {
        #[cfg(test)]
        if let Some(map) = self.bidi_focused_map() {
            let width = (cell.width as usize).max(1);
            let height = (cell.height as usize).max(1);
            let x = px.saturating_sub(1);
            let row = py.saturating_sub(1) / height;
            let visual = x / width;
            let logical = map.logical_column(row, visual);
            return (logical * width + x % width + 1, py);
        }
        let _ = cell;
        (px, py)
    }
}

/// The finished map of one pane: its plan with every row the overlay painters
/// changed since planning reset to the identity layout.
pub(super) fn finish_bidi_plan(
    plan: Option<(BidiDisplayMap, Snapshot)>,
    painted: &Snapshot,
) -> Option<BidiDisplayMap> {
    plan.map(|(mut map, planned)| {
        map.reset_rows_changed_between(&planned, painted);
        map
    })
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

    /// Test seam: turn the display gate on or off without presenting a frame.
    pub(in crate::native) fn set_bidi_display_for_test(&mut self, on: bool) {
        self.bidi_display_for_test = on;
    }

    /// Test seam: the map the last split frame presented for pane `token`.
    pub(in crate::native) fn bidi_pane_map_for_test(
        &self,
        token: SessionToken,
    ) -> Option<&BidiDisplayMap> {
        self.bidi_pane_maps
            .iter()
            .find(|(pane, _)| *pane == token)
            .map(|(_, map)| map)
    }

    /// Test seam: the screen column the IME candidate window anchors at.
    pub(in crate::native) fn ime_anchor_column_for_test(
        &self,
        cursor: crate::core::Position,
    ) -> usize {
        self.ime_anchor_column(cursor)
    }

    /// Test seam: [`Self::bidi_logical_report_px`] for a 1-based pixel.
    pub(in crate::native) fn bidi_logical_report_px_for_test(
        &self,
        px: (usize, usize),
        cell: CellSize,
    ) -> (usize, usize) {
        self.bidi_logical_report_px(px, cell)
    }
}

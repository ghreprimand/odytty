// SPDX-License-Identifier: GPL-3.0-only
//! Bidirectional display reordering for the live frame, its pointer, and the
//! mouse reports the pointer sends (`bidi_reorder`, off by default).
//!
//! While the setting is on, the single-pane frame and each content pane of a
//! split frame showing the primary screen plan a [`BidiDisplayMap`] for their
//! content grid (with the paragraph context above the viewport and a
//! left-to-right paragraph level), reset every row an overlay painter changed
//! back to the identity layout, and hand the map to the renderer. The
//! alternate screen is never planned. The single-pane map is placed inside the
//! decorated frame, so chrome stays in identity layout; split chrome strips
//! never take a map. The pointer maps a content cell under the pointer to the
//! logical cell drawn there through the focused content map, so selection,
//! hover, and every cell-encoded mouse report address logical cells. An
//! SGR-pixel report moves by whole cells onto the logical cell and keeps its
//! offset inside the cell. The cursor effects (slide, trail, follower, and
//! aura) run in drawn columns, so they start and end where the cursor block is
//! drawn. Cursor, selection, search, copy, and every terminal protocol value
//! stay logical.
//!
//! With the setting off every method here returns the identity answer and no
//! map is planned. Platform-neutral.

use super::*;
use crate::core::Snapshot;
use crate::grid::BidiDisplayMap;

impl App {
    /// Whether frames plan a bidi display map: the `bidi_reorder` setting, or
    /// the test override.
    pub(super) fn bidi_reorder_on(&self) -> bool {
        #[cfg(test)]
        if self.bidi_display_for_test {
            return true;
        }
        self.settings.bidi_reorder
    }

    /// Flip `bidi_reorder` live (the right-click Reorder Right-to-Left Text
    /// row), through the same apply path as a Settings edit. Not written to
    /// odytty.conf; Settings > Rendering > Bidirectional text with Save
    /// persists it.
    pub(super) fn toggle_bidi_reorder(&mut self) {
        let mut settings = self.settings.clone();
        settings.bidi_reorder = !settings.bidi_reorder;
        self.apply_overlay_settings(settings);
    }

    /// The display map for the single-pane content grid `snapshot` at
    /// scrollback `offset`, or `None` while reordering is off, on the
    /// alternate screen, or in a multi-pane tab (see [`Self::bidi_pane_plan`]).
    pub(super) fn bidi_content_map(
        &self,
        terminal: &crate::core::Terminal,
        snapshot: &Snapshot,
        offset: usize,
    ) -> Option<BidiDisplayMap> {
        (self.bidi_reorder_on() && self.sessions.active_is_single_pane())
            .then(|| plan_content_map(terminal, snapshot, offset))
            .flatten()
    }

    /// Record the map the presented single-pane frame used for its content
    /// grid, after overlay rows were reset. The pointer maps through it.
    pub(super) fn set_bidi_frame_map(&mut self, map: Option<BidiDisplayMap>) {
        self.bidi_frame_map = map;
    }

    /// The plan for one split pane's content grid `snapshot` at scrollback
    /// `offset`, with a copy of the snapshot it was planned from so rows an
    /// overlay painter changes can be reset. `None` while reordering is off
    /// and on the alternate screen.
    pub(super) fn bidi_pane_plan(
        &self,
        terminal: &crate::core::Terminal,
        snapshot: &Snapshot,
        offset: usize,
    ) -> Option<(BidiDisplayMap, Snapshot)> {
        if !self.bidi_reorder_on() {
            return None;
        }
        plan_content_map(terminal, snapshot, offset).map(|map| (map, snapshot.clone()))
    }

    /// Record the maps each content pane of the presented split frame used,
    /// after overlay rows were reset. The pointer maps through the focused
    /// pane's map.
    pub(super) fn set_bidi_pane_maps(&mut self, maps: Vec<(SessionToken, BidiDisplayMap)>) {
        self.bidi_pane_maps = maps;
    }

    /// The content map of the focused content grid: the single-pane frame
    /// map, or the focused pane's map in a split tab.
    fn bidi_focused_map(&self) -> Option<&BidiDisplayMap> {
        if !self.bidi_reorder_on() {
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
    /// pointer is over. Identity unless the presented frame planned a map.
    pub(super) fn bidi_logical_point(&self, point: CellPoint) -> CellPoint {
        if let Some(map) = self.bidi_focused_map() {
            return CellPoint {
                row: point.row,
                column: map.logical_column(point.row, point.column),
            };
        }
        point
    }

    /// The screen column the presented frame drew the focused content cell
    /// `point` at. Identity unless the presented frame planned a map.
    pub(super) fn bidi_visual_column(&self, point: CellPoint) -> usize {
        if let Some(map) = self.bidi_focused_map() {
            return map.visual_column(point.row, point.column);
        }
        point.column
    }

    /// Whether the presented frame drew the focused logical content cell
    /// `point` inside a right-to-left run (an odd embedding level). `false`
    /// unless the presented frame planned a map.
    pub(super) fn bidi_cell_is_right_to_left(&self, point: CellPoint) -> bool {
        self.bidi_focused_map()
            .is_some_and(|map| map.level(point.row, point.column) % 2 == 1)
    }

    /// Move a 1-based grid-relative SGR-pixel report coordinate onto the
    /// logical cell drawn under it, keeping its offset inside the cell, so a
    /// pixel report addresses the same cell the cell-encoded reports name.
    /// Identity unless the presented frame planned a map.
    pub(super) fn bidi_logical_report_px(
        &self,
        (px, py): (usize, usize),
        cell: CellSize,
    ) -> (usize, usize) {
        if let Some(map) = self.bidi_focused_map() {
            let width = (cell.width as usize).max(1);
            let height = (cell.height as usize).max(1);
            let x = px.saturating_sub(1);
            let row = py.saturating_sub(1) / height;
            let visual = x / width;
            let logical = map.logical_column(row, visual);
            return (logical * width + x % width + 1, py);
        }
        (px, py)
    }
}

/// The cell the cursor effects (slide, trail, follower, and aura) move
/// between: the screen cell `map` draws the logical `cursor` at. Identity
/// without a map, which is every frame while reordering is off.
pub(super) fn effect_cursor(map: Option<&BidiDisplayMap>, cursor: Position) -> Position {
    map.map_or(cursor, |map| Position {
        row: cursor.row,
        column: map.visual_column(cursor.row, cursor.column),
    })
}

impl App {
    /// Advance the cursor slide and the large-jump follower for this frame in
    /// drawn columns: the snapshot cursor is moved to `effect` for the two
    /// updates and restored, so a glide starts and ends where the cursor block
    /// is drawn. With no map `effect` is the logical cursor and this is
    /// exactly the two updates.
    pub(super) fn advance_cursor_motion_at(
        &mut self,
        now: Instant,
        snapshot: &mut Snapshot,
        style: crate::core::CursorStyle,
        cell: CellSize,
        effect: Position,
    ) {
        let logical = snapshot.cursor;
        snapshot.cursor = effect;
        self.update_cursor_motion(now, snapshot, cell);
        self.update_cursor_streak(now, snapshot, style, cell);
        snapshot.cursor = logical;
    }

    /// The single-pane frame advances its cursor effects from the plan made
    /// before the overlay painters ran. An overlay that writes text into the
    /// cursor's row returns that row to identity, so the final `map` can draw
    /// the cursor at a different column than `advanced`. The effects then snap
    /// (no glide, no follower) rather than draw away from the block. Returns
    /// the drawn cursor for the trail and the next frame's comparison.
    pub(super) fn settle_bidi_cursor_effects(
        &mut self,
        advanced: Position,
        map: Option<&BidiDisplayMap>,
        cursor: Position,
    ) -> Position {
        let drawn = effect_cursor(map, cursor);
        if drawn != advanced {
            self.cursor_slide_start = None;
            self.cursor_slide_deadline = None;
            self.cursor_anim_offset = [0.0, 0.0];
            self.clear_cursor_streak();
        }
        drawn
    }

    /// The drawn cell of the focused pane's logical `cursor` in a split tab,
    /// through the map its presented frame uses. Identity unless the
    /// presented frame planned a map.
    pub(super) fn bidi_focused_effect_cursor(&self, cursor: Position) -> Position {
        if let Some(map) = self.bidi_focused_map() {
            return effect_cursor(Some(map), cursor);
        }
        cursor
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
/// paragraph context above the viewport. `None` on the alternate screen,
/// which is never reordered.
pub(super) fn plan_content_map(
    terminal: &crate::core::Terminal,
    snapshot: &Snapshot,
    offset: usize,
) -> Option<BidiDisplayMap> {
    if terminal.on_alternate_screen() {
        return None;
    }
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
    Some(BidiDisplayMap::plan_with_context(
        snapshot, &wrapped, &context,
    ))
}

#[cfg(test)]
impl App {
    /// Test seam: turn the display gate on and record the content map a
    /// presented frame of the current viewport would use (no overlay is
    /// painted in a headless test), so pointer tests run without a GPU.
    pub(in crate::native) fn present_bidi_frame_map_for_test(&mut self) {
        self.bidi_display_for_test = true;
        let _ = self.present_bidi_frame_for_test(Instant::now());
    }

    /// Test seam: run the single-pane frame's cell and cursor-effect steps
    /// for the current viewport at `now`, in the frame's order and through
    /// its helpers: plan, advance the cursor effects, paint the cell
    /// manifest, reset overlay rows, settle the effects, paint the trail, and
    /// record the presented map and the next frame's cursor comparison. Only
    /// the GPU hand-off and chrome decoration are skipped (no headless GPU).
    /// Honors reordering as set: with it off every step is the off path.
    pub(in crate::native) fn present_bidi_frame_for_test(
        &mut self,
        now: Instant,
    ) -> BidiFrameProbe {
        let cell = self.test_cell.unwrap_or(CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        });
        let (mut snapshot, scrollback_len, cursor_style, visible_buttons, ambiguous_wide, plan) = {
            let terminal = crate::native::lock_recover(&self.terminal);
            let scrollback_len = terminal.screen().scrollback_len();
            let offset = self.viewport.offset().min(scrollback_len);
            let snapshot = terminal.snapshot_with_scrollback(offset);
            let plan = self
                .bidi_content_map(&terminal, &snapshot, offset)
                .map(|map| (map, snapshot.clone()));
            (
                snapshot,
                scrollback_len,
                terminal.cursor_style(),
                terminal.visible_button_spans(offset),
                terminal.ambiguous_wide(),
                plan,
            )
        };
        let advanced = effect_cursor(plan.as_ref().map(|(map, _)| map), snapshot.cursor);
        self.advance_cursor_motion_at(now, &mut snapshot, cursor_style, cell, advanced);
        let ctx = self.overlay_ctx(
            scrollback_len,
            cell,
            snapshot.cursor,
            snapshot.cursor_visible,
            now,
        );
        self.paint_single_pane_cells(&mut snapshot, &ctx, &visible_buttons, ambiguous_wide);
        let map = plan.map(|(mut map, planned)| {
            map.reset_rows_changed_between(&planned, &snapshot);
            map
        });
        let effect_cursor =
            self.settle_bidi_cursor_effects(advanced, map.as_ref(), snapshot.cursor);
        let ctx = super::overlay_registry::OverlayCtx {
            cursor: effect_cursor,
            ..ctx
        };
        self.set_bidi_frame_map(map);
        let mut trail = Vec::new();
        self.paint_cursor_trail_quads(&ctx, &mut trail);
        let streak = self.cursor_streak_request(
            now,
            [
                0.0,
                0.0,
                snapshot.dimensions.columns as f32 * cell.width as f32,
                snapshot.dimensions.rows as f32 * cell.height as f32,
            ],
        );
        let mut comparison = crate::native::session::CursorComparison::of(&snapshot);
        comparison.cursor = effect_cursor;
        self.last_cursor_comparison_snapshot = Some(comparison);
        BidiFrameProbe {
            params: self.cursor_render_params(),
            painted: snapshot,
            streak,
            trail,
        }
    }

    /// Test seam: the content map the pointer currently maps through.
    pub(in crate::native) fn bidi_frame_map_for_test(&self) -> Option<&BidiDisplayMap> {
        self.bidi_frame_map.as_ref()
    }

    /// Test seam: apply `settings` through the Settings edit path.
    pub(in crate::native) fn apply_overlay_settings_for_test(&mut self, settings: Settings) {
        self.apply_overlay_settings(settings);
    }

    /// Test seam: the live `bidi_reorder` setting value.
    pub(in crate::native) fn bidi_reorder_setting_for_test(&self) -> bool {
        self.settings.bidi_reorder
    }

    /// Test seam: turn the test override on or off without presenting a frame.
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

/// What [`App::present_bidi_frame_for_test`] presented: the content snapshot
/// after the cell manifest, the live cursor parameters, the follower request,
/// and the trail quads.
#[cfg(test)]
pub(in crate::native) struct BidiFrameProbe {
    pub(in crate::native) painted: Snapshot,
    pub(in crate::native) params: CursorRenderParams,
    pub(in crate::native) streak: Option<crate::native::gpu::CursorStreakRequest>,
    pub(in crate::native) trail: Vec<SolidQuad>,
}

// SPDX-License-Identifier: GPL-3.0-only
//! Snapshot decoration and presentation assembly for the native app: the
//! decorated snapshot spaces handed to the GPU and the chrome quads drawn over
//! them.

use super::*;

impl App {
    /// SCROLL-CHROME-BOUNCE: the composited-chrome geometry to hand the GPU so it
    /// pins the tab bar / rail against the sub-row scroll glide. `decorated_cols`
    /// is the tab-chrome-decorated snapshot's column count (the coordinate space
    /// the rail band indices live in). `None` when nothing is composited (no bar,
    /// no rail), which keeps the pin inert / byte-identical.
    pub(super) fn chrome_pin_geom(&self, decorated_cols: usize) -> Option<ChromePinGeom> {
        let reserve = self.tab_reserve();
        let top_rows = reserve.top_rows;
        let (rail_col_start, rail_col_end) = if reserve.left_cols > 0 {
            // Left rail occupies the leftmost reserved columns.
            (0, reserve.left_reserved_cols())
        } else if reserve.right_cols > 0 {
            // Right rail occupies the rightmost reserved columns.
            (
                decorated_cols.saturating_sub(reserve.right_reserved_cols()),
                decorated_cols,
            )
        } else {
            (0, 0)
        };
        if top_rows == 0 && rail_col_start == rail_col_end {
            None
        } else {
            // TAB-LABEL-CENTERING: recenter each band's single label line on its
            // true pixel center. The top bar places its label at `rows / 2`
            // (biased low on even heights); the rail slot at `(slot_rows - 1) / 2`
            // (biased high on even heights). The shared helper yields the exact
            // sub-row correction for each convention (0.0 on single-row / odd).
            let band_glyph_dy_rows = if top_rows > 0 {
                crate::grid::band_label_descender_safe_dy_rows(
                    top_rows,
                    top_rows / 2,
                    self.resolved_cell().map_or(1, |cell| cell.height),
                )
            } else {
                0.0
            };
            let rail_glyph_dy_rows = if rail_col_start != rail_col_end {
                let slot_rows = self.rail_geom().slot_rows;
                crate::grid::rail_label_descender_safe_dy_rows(
                    slot_rows,
                    slot_rows.saturating_sub(1) / 2,
                    self.resolved_cell().map_or(1, |cell| cell.height),
                )
            } else {
                0.0
            };
            // CHROME-GAP: the chrome-facing padding gaps for the composited
            // single-pane frame. The vertex builders use these to shift the
            // content (and, past a left rail, the top bar) off the pinned band
            // by the same padding that separates content from the window edges.
            // Both zero when padding is zero, keeping that frame byte-identical.
            let padding = self
                .resolved_surface()
                .map(|(_, _, padding)| padding)
                .unwrap_or(WindowPadding::ZERO);
            let gap = reserve.chrome_gap(padding);
            Some(ChromePinGeom {
                top_rows,
                rail_col_start,
                rail_col_end,
                band_glyph_dy_rows,
                rail_glyph_dy_rows,
                gap_x: gap.left + gap.right,
                gap_y: gap.top,
            })
        }
    }

    /// Shift window-content overlay quads by the tab-chrome offset so they stay
    /// registered with the content grid after chrome is reserved: `+Y` for the
    /// top bar, `+X` for the left rail (F4-V2). `(0, 0)` on the plain path leaves
    /// every quad untouched (byte-identical).
    pub(super) fn shift_overlays_for_tab_chrome(
        &self,
        overlays: &mut [SolidQuad],
        dx: f32,
        dy: f32,
    ) {
        if dx <= 0.0 && dy <= 0.0 {
            return;
        }
        for overlay in overlays {
            overlay.rect[0] += dx;
            overlay.rect[2] += dx;
            overlay.rect[1] += dy;
            overlay.rect[3] += dy;
        }
    }

    pub(super) fn decorate_snapshot_with_tab_bar(
        &self,
        snapshot: Snapshot,
        cursor_visible: bool,
        cell: CellSize,
    ) -> (Snapshot, Vec<SolidQuad>) {
        let show_top = self.should_show_tab_bar();
        // F4-P3: under rail auto-hide the rail is NOT decorated into the content
        // snapshot — it draws only as a floating overlay (`build_rail_overlay`)
        // over full-bleed content. The top bar is never auto-hidden, so it stays
        // pinned regardless.
        let show_rail = self.should_show_workspace_rail() && !self.rail_autohide_active();
        if !show_top && !show_rail {
            // No chrome: the undecorated snapshot IS the render snapshot; move
            // it out instead of deep-copying every cell.
            return (snapshot, Vec::new());
        }
        // Rail-only frame (a background workspace pair whose active tab needs no
        // top bar): grow columns off the side directly.
        if !show_top {
            let side = self.workspace_rail_side();
            return self.decorate_snapshot_with_tab_rail(&snapshot, cursor_visible, cell, side);
        }
        // Top bar always grows rows off the top. The workspace rail then grows
        // columns off its side, spanning the FULL height (including the tab bar)
        // for a VS Code-style full-height sidebar. Each band alone reproduces its
        // single-band behaviour byte-for-byte.
        let (mut decorated, mut quads) =
            self.decorate_snapshot_with_top_bar(&snapshot, cursor_visible, cell);
        if show_rail {
            let side = self.workspace_rail_side();
            let (deco, rail_quads) =
                self.decorate_snapshot_with_tab_rail(&decorated, cursor_visible, cell, side);
            decorated = deco;
            quads.extend(rail_quads);
        }
        (decorated, quads)
    }

    /// Prepare the two snapshot coordinate spaces used by a single-pane frame.
    ///
    /// The GPU receives the decorated snapshot, whose dimensions and cursor are
    /// shifted to make room for pinned tab chrome. Cursor motion compares the
    /// next terminal snapshot against the undecorated content snapshot instead;
    /// otherwise visible chrome looks like a resize or reflow every frame and
    /// forces cursor effects to snap.
    pub(super) fn prepare_single_pane_snapshots(
        &self,
        snapshot: Snapshot,
        cursor_visible: bool,
        cell: CellSize,
    ) -> (
        Snapshot,
        Vec<SolidQuad>,
        crate::native::session::CursorComparison,
    ) {
        // Cursor motion needs only the undecorated cursor and dimensions, not
        // the cells, so the comparison keeps metadata instead of a full clone.
        let comparison = crate::native::session::CursorComparison::of(&snapshot);
        let (decorated, quads) =
            self.decorate_snapshot_with_tab_bar(snapshot, cursor_visible, cell);
        (decorated, quads, comparison)
    }

    /// Top tab-bar decoration: grow the snapshot by [`TAB_BAR_ROWS`] rows off the
    /// top, shift the content (and cursor) down, and paint the active workspace's
    /// tab strip into the reserved row. Extracted from
    /// [`Self::decorate_snapshot_with_tab_bar`] so it composes with the rail
    /// decoration (a frame can show both bands).
    pub(super) fn decorate_snapshot_with_top_bar(
        &self,
        snapshot: &Snapshot,
        cursor_visible: bool,
        cell: CellSize,
    ) -> (Snapshot, Vec<SolidQuad>) {
        let columns = snapshot.dimensions.columns;
        // Adjustable height: reserve `bar_rows` off the top (one on the classic
        // path) and shift the content + cursor down by that band.
        let bar_rows = self.tab_bar_rows();
        let rows = snapshot.dimensions.rows + bar_rows;
        let mut decorated = Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position {
                row: snapshot.cursor.row + bar_rows,
                column: snapshot.cursor.column,
            },
            cursor_visible,
            colors: snapshot.colors.clone(),
            cells: vec![crate::core::Cell::default(); columns * rows],
        };
        let top = columns * bar_rows;
        decorated.cells[top..top + snapshot.cells.len()].clone_from_slice(&snapshot.cells);

        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let output = self.render_top_bar_widget(columns, padding.as_f32(), cell, padding);
        // Fill the reserved top band per column and center the label row (rows
        // 0..bar_rows), leaving the shifted content untouched below.
        panes::place_tab_bar_glyphs(&mut decorated.cells, output.glyphs, columns, bar_rows, 0);
        (decorated, output.quads)
    }

    /// Single-pane vertical-rail decoration (F4-V2): grow the snapshot by the
    /// rail band on the rail side, shift the
    /// original content (and the cursor) into the content band, paint the rail
    /// glyphs into the rail band of every row. The reservation used here MUST
    /// match the resize path (ODP-8) or the cursor/pointer desync.
    pub(super) fn decorate_snapshot_with_tab_rail(
        &self,
        snapshot: &Snapshot,
        cursor_visible: bool,
        cell: CellSize,
        side: RailSide,
    ) -> (Snapshot, Vec<SolidQuad>) {
        let rail_cols = self.rail_cols();
        let old_cols = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        let new_cols = old_cols + rail_cols;
        // Left rail: content shifts right by the rail band, which paints at
        // column 0. Right rail: content stays at column 0 and the rail paints at
        // the far right.
        let content_col_offset = match side {
            RailSide::Left => rail_cols,
            RailSide::Right => 0,
        };
        let rail_col_start = match side {
            RailSide::Left => 0,
            RailSide::Right => old_cols,
        };
        let mut decorated = Snapshot {
            dimensions: Dimensions::new(new_cols, rows),
            cursor: Position {
                row: snapshot.cursor.row,
                column: snapshot.cursor.column + content_col_offset,
            },
            cursor_visible,
            colors: snapshot.colors.clone(),
            cells: vec![crate::core::Cell::default(); new_cols * rows],
        };
        // Copy each original row into the content band of the wider row.
        for r in 0..rows {
            let src = &snapshot.cells[r * old_cols..(r + 1) * old_cols];
            let dst_start = r * new_cols + content_col_offset;
            decorated.cells[dst_start..dst_start + old_cols].clone_from_slice(src);
        }

        let output =
            self.render_rail_widget(rail_cols, rows, self.rail_origin_px(cell), cell, side);
        for glyph in output.glyphs {
            let col = rail_col_start + glyph.col;
            if glyph.row < rows && col < new_cols {
                decorated.cells[glyph.row * new_cols + col] =
                    crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        (decorated, output.quads)
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Multi-pane render dispatch (design doc §3.2, Phase 1c-3).
//!
//! When the active tab holds more than one pane, the redraw path branches here
//! instead of the single-pane `update_from_snapshot*` machinery. Each visible
//! pane of the active tab is snapshotted from its own terminal at its own
//! scrollback viewport and drawn into its laid-out sub-rect; the focused pane
//! draws a live cursor, the others none. The 1px themed dividers fill the gaps
//! between panes. When two or more *tabs* exist the tab strip is drawn as an
//! extra one-row pane along the top.
//!
//! Hard boundary: the **single-pane** path never reaches this module — the
//! caller only calls [`App::rebuild_multipane`] when `active_is_single_pane()`
//! is false — so the byte-identical fast path is wholly untouched. This path is
//! deliberately the only new behaviour.
//!
//! Scope note (v1, 1c-3): per-pane interactive overlays (selection / search /
//! hints) are not yet composited for non-focused panes; each pane renders its
//! base terminal content plus, for the focused pane, the cursor. Per-pane
//! overlay parity and the inactive-pane dim are later checkboxes on the Phase 1
//! plan.

use super::*;
use crate::native::gpu::{OverlayTop, PaneRender, RailOverlay};
use crate::native::layout::{PaneRect, divider_rects, grid_dims_for_rect};
use crate::native::overlay::{apply_overlay, overlay_rect};

/// Width of the divider gap between panes, in physical pixels. A crisp hairline
/// matching the §8 pixel-smoke invariants.
pub(super) const PANE_DIVIDER_PX: f32 = 1.0;

/// How far on each side of a 1px divider a press still grabs it, in physical
/// pixels — the hairline divider would be near-impossible to hit otherwise.
pub(super) const DIVIDER_GRAB_PX: f32 = 6.0;

/// The per-pane focus-dim value for the multi-pane render path: the focused
/// pane is never dimmed (`0.0`), the inactive panes recede by `inactive_dim`.
/// When `inactive_dim` is `0.0` (the default, and the value the plain renderer
/// profile forces) this returns `0.0` for every pane, so the assignment is an
/// exact identity and the multi-pane frame is byte-identical to before the knob
/// existed.
pub(super) fn pane_focus_dim(is_focused: bool, inactive_dim: f32) -> f32 {
    if is_focused { 0.0 } else { inactive_dim }
}

/// The tab-chrome reservation: how many cell-rows are taken off the top (the
/// horizontal bar) and how many cell-columns off the left/right (the vertical
/// rail, F4-V2). Exactly one axis is ever non-zero for a given placement, and
/// `NONE` (all zero) is the byte-identical no-chrome case. Callers build it from
/// [`App::tab_reserve`]; the free `pane_content_rect` stays pure/GPU-free.
///
/// `gap_cols` is the cell-quantized wallpaper gap between a side rail and the
/// content (R1.1): the rail's visual band is `left_cols`/`right_cols` wide, then
/// `gap_cols` blank columns replicate the window-padding band between the rail
/// and the terminal so text never abuts the rail seam. It only ever applies on
/// the side the rail occupies (0 for the top bar / no chrome). The rail widget's
/// visual width (`App::rail_cols`) excludes it; the content reservation
/// (`pane_content_rect`, the pointer/graphics offsets, the resize column count)
/// includes it — so grid, cursor, and pointer stay in lockstep (ODP-8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TabReserve {
    /// Rows reserved off the top (horizontal bar).
    pub(super) top_rows: usize,
    /// Columns reserved off the left (left rail) — the rail's visual band width.
    pub(super) left_cols: usize,
    /// Columns reserved off the right (right rail — R2; always 0 in R1) — the
    /// rail's visual band width.
    pub(super) right_cols: usize,
    /// Blank wallpaper-gap columns between the side rail and the content (R1.1).
    /// Zero for the top bar / no chrome.
    pub(super) gap_cols: usize,
}

impl TabReserve {
    pub(super) const NONE: Self = Self {
        top_rows: 0,
        left_cols: 0,
        right_cols: 0,
        gap_cols: 0,
    };

    /// The classic top horizontal strip reservation (`TAB_BAR_ROWS` rows).
    #[cfg(test)]
    pub(super) fn top() -> Self {
        Self {
            top_rows: TAB_BAR_ROWS as usize,
            left_cols: 0,
            right_cols: 0,
            gap_cols: 0,
        }
    }

    /// Total columns reserved off the left of the content: the left rail's band
    /// plus its wallpaper gap. Zero unless a left rail is active.
    pub(super) fn left_reserved_cols(&self) -> usize {
        if self.left_cols > 0 {
            self.left_cols + self.gap_cols
        } else {
            0
        }
    }

    /// Total columns reserved off the right of the content: the right rail's
    /// band plus its wallpaper gap. Zero unless a right rail is active.
    pub(super) fn right_reserved_cols(&self) -> usize {
        if self.right_cols > 0 {
            self.right_cols + self.gap_cols
        } else {
            0
        }
    }
}

/// Cell-quantized wallpaper gap between a side rail and the content, replicating
/// the window-padding band: `ceil(pad_px / cell_w)` columns. Zero when there is
/// no window padding (a faithful "replicate the padding" — no padding, no gap)
/// or a degenerate cell. Pure so both render paths and the reservation share one
/// definition.
pub(super) fn rail_content_gap_cols(padding: WindowPadding, cell: CellSize) -> usize {
    let cw = cell.width as usize;
    if cw == 0 {
        return 0;
    }
    let pad = padding.physical_px() as usize;
    pad.div_ceil(cw)
}

/// The pixel rectangle available to panes: the surface minus window padding on
/// all sides, and minus the tab chrome — the top strip (`reserve.top_rows`) or
/// the side rail (`reserve.left_cols`/`right_cols`) when shown. For a
/// single-pane tab this rect's cell dimensions equal `self.grid`, so the
/// single-pane resize/render geometry is unchanged (it never calls this).
///
/// `TabReserve::NONE` and `TabReserve::top()` reproduce the pre-rail behaviour
/// byte-for-byte (top strip off the top, nothing off the sides).
pub(super) fn pane_content_rect(
    width_px: u32,
    height_px: u32,
    cell: CellSize,
    padding: WindowPadding,
    reserve: TabReserve,
) -> PaneRect {
    let pad = padding.as_f32();
    let tab_h = cell.height as f32 * reserve.top_rows as f32;
    // Include the rail↔content wallpaper gap (R1.1) so the content origin sits a
    // padding-band's width off the rail, not flush against its seam.
    let left_w = cell.width as f32 * reserve.left_reserved_cols() as f32;
    let right_w = cell.width as f32 * reserve.right_reserved_cols() as f32;
    let w = (width_px as f32 - 2.0 * pad - left_w - right_w).max(0.0);
    let h = (height_px as f32 - 2.0 * pad - tab_h).max(0.0);
    PaneRect::new(pad + left_w, pad + tab_h, w, h)
}

/// Map a physical pointer position to a cell in **window-overlay space** — the
/// content grid an open window-level overlay centers within (multi-pane). Pure
/// so the multi-pane hit-test geometry is unit-testable without a GPU. Returns
/// `None` when the content rect spans no cells. The pointer is clamped into the
/// grid so a press just outside the content area still resolves to the nearest
/// edge cell (matching the single-pane clamp behaviour).
fn window_overlay_cell(
    content: PaneRect,
    cell: CellSize,
    x_px: f64,
    y_px: f64,
) -> Option<CellPoint> {
    let (cols, rows) = grid_dims_for_rect(content, cell.width, cell.height);
    if cols == 0 || rows == 0 {
        return None;
    }
    let col = ((x_px as f32 - content.x).max(0.0) / cell.width.max(1) as f32) as usize;
    let row = ((y_px as f32 - content.y).max(0.0) / cell.height.max(1) as f32) as usize;
    Some(CellPoint {
        row: row.min(rows - 1),
        column: col.min(cols - 1),
    })
}

/// Map an absolute physical pointer position to a cell **relative to a pane's
/// sub-rect** — origin at the pane's top-left, clamped into `dims` (the focused
/// pane's grid). Mirrors [`selection::cell_at_physical_with_padding`] but uses
/// the pane rect as the origin instead of the window padding, so a press inside
/// an offset pane anchors at the right cell rather than against the window
/// origin. Pure, so the per-pane selection mapping is unit-testable without a
/// GPU.
fn pane_relative_cell(
    rect: PaneRect,
    cell: CellSize,
    dims: Dimensions,
    x_px: f64,
    y_px: f64,
) -> CellPoint {
    let column = ((x_px as f32 - rect.x).max(0.0) / cell.width.max(1) as f32) as usize;
    let row = ((y_px as f32 - rect.y).max(0.0) / cell.height.max(1) as f32) as usize;
    CellPoint {
        row: row.min(dims.rows.saturating_sub(1)),
        column: column.min(dims.columns.saturating_sub(1)),
    }
}

/// Copy a rectangular sub-region of `src` into a new snapshot of size
/// `width`×`height`, starting at cell `(top, left)`. Used to crop a painted
/// window-overlay snapshot down to the panel's opaque rect so it composites as
/// a clean box over the multi-pane content. Out-of-bounds source cells fall back
/// to the default cell (defensive; the caller always passes an in-bounds rect).
fn crop_snapshot(src: &Snapshot, left: usize, top: usize, width: usize, height: usize) -> Snapshot {
    let src_cols = src.dimensions.columns;
    let mut cells = Vec::with_capacity(width * height);
    for r in 0..height {
        for c in 0..width {
            let sr = top + r;
            let sc = left + c;
            let cell = src
                .cells
                .get(sr * src_cols + sc)
                .copied()
                .unwrap_or_default();
            cells.push(cell);
        }
    }
    Snapshot {
        dimensions: Dimensions::new(width, height),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: false,
        colors: src.colors.clone(),
        cells,
    }
}

impl App {
    /// The pane content rect + cell metrics for the active **multi-pane** tab's
    /// pointer math, or `None` when the active tab is single-pane (the
    /// byte-identical path) or there is no GPU yet. Pointer coordinates
    /// (`pointer_px`) share this absolute physical-pixel basis.
    pub(super) fn multipane_geometry(&self) -> Option<(PaneRect, CellSize)> {
        if self.sessions.active_is_single_pane() {
            return None;
        }
        let cell = self.resolved_cell()?;
        let (w, h, padding) = self.resolved_surface()?;
        let content = pane_content_rect(w, h, cell, padding, self.tab_reserve());
        Some((content, cell))
    }

    /// The surface size and window padding for pane geometry: from the GPU in
    /// production; in headless tests (no GPU) a [`App::test_surface`] override
    /// stands in so the multi-pane pointer/cursor path is testable. In non-test
    /// builds the override field does not exist, so this is exactly
    /// `self.gpu.as_ref().map(|g| (g.surface_size(), g.window_padding()))` —
    /// byte-identical to the previous inline `gpu.surface_size()` /
    /// `gpu.window_padding()` reads.
    ///
    /// `pub(super)` so the close-collapse single-pane reflow in
    /// [`App::reflow_active_panes_and_redraw`] can resolve the full content rect
    /// the same way (it mirrors `multipane_geometry`'s own access).
    pub(super) fn resolved_surface(&self) -> Option<(u32, u32, WindowPadding)> {
        #[cfg(test)]
        if let Some((size, padding)) = self.test_surface {
            return Some((size.0, size.1, padding));
        }
        let gpu = self.gpu.as_ref()?;
        let (w, h) = gpu.surface_size();
        Some((w, h, gpu.window_padding()))
    }

    /// The cell dimensions a **window-level overlay** (context menu / settings /
    /// palette / connections / replay) centers within. Overlays are window-level,
    /// so in a multi-pane tab they use the whole content grid, NOT the focused
    /// pane's smaller sub-grid (`self.grid`). In a single-pane tab this returns
    /// `self.grid` exactly, so the single-pane overlay geometry is unchanged.
    pub(super) fn overlay_grid_dims(&self) -> (usize, usize) {
        if let Some((content, cell)) = self.multipane_geometry() {
            grid_dims_for_rect(content, cell.width, cell.height)
        } else {
            (self.grid.columns, self.grid.rows)
        }
    }

    /// The column count the tab-bar strip is laid out across: the **window**
    /// content columns, not the focused pane's sub-grid. The strip renders edge
    /// to edge across the whole window ([`Self::tab_bar_strip`] uses
    /// `(surface_w - 2·pad)/cell.width`), so its hit-test must use the same
    /// window columns — otherwise, in a multi-pane tab where `self.grid` Derefs
    /// to the narrower focused-pane sub-grid, tabs render at window-width
    /// positions but hit-test across the sub-grid and clicks/hover miss. In a
    /// single-pane tab [`Self::overlay_grid_dims`] returns `self.grid`, so this
    /// is exactly `self.grid.columns` and the tab-bar hit-test is byte-identical.
    pub(super) fn tab_bar_grid_cols(&self) -> usize {
        self.overlay_grid_dims().0
    }

    /// The row count the vertical tab rail is laid out across (F4-V2): the
    /// **window** content rows, the exact analog of [`Self::tab_bar_grid_cols`]
    /// on the column axis. The rail render (`tab_rail_strip` / the single-pane
    /// `decorate_snapshot_with_tab_rail`) spans these rows, so its hit-test must
    /// share the basis or the row→slot mapping drifts. Single-pane returns
    /// `self.grid.rows` (a left rail reserves columns, not rows).
    pub(super) fn tab_rail_grid_rows(&self) -> usize {
        // The workspace rail is a full-height sidebar (design doc §7): it spans
        // the content rows PLUS the top tab-bar rows it sits alongside, so its
        // slot layout matches the rail decoration (which paints across the
        // top-bar-decorated snapshot's full height). With no top bar reserved
        // (`top_rows == 0`, the single-band case) this is byte-identical to the
        // content rows.
        self.overlay_grid_dims().1 + self.tab_reserve().top_rows
    }

    /// The pointer position in **window-overlay cell space** (the content grid),
    /// for overlay hit-testing and context-menu spawn. In a single-pane tab this
    /// is exactly `self.pointer_cell` (already window-space), so the single-pane
    /// path is unchanged. In a multi-pane tab `self.pointer_cell` is relative to
    /// the focused pane's sub-grid — wrong for a window-level overlay — so this
    /// recomputes it against the content rect from the cached physical pointer.
    pub(super) fn overlay_pointer_cell(&self) -> Option<CellPoint> {
        let Some((content, cell)) = self.multipane_geometry() else {
            return self.pointer_cell;
        };
        let (x_px, y_px) = self.pointer_px?;
        window_overlay_cell(content, cell, x_px, y_px)
    }

    /// The pointer cell **relative to the focused pane's sub-rect** in a
    /// multi-pane tab — the basis local text selection anchors and extends
    /// against, since `self.grid` / `self.terminal` / `self.selection` all
    /// operate on the focused pane's sub-grid, which is offset from the window
    /// origin. Returns `None` on a single-pane tab (where the byte-identical
    /// window-origin mapping in `update_pointer_cell` is correct) or when the
    /// geometry / cached pointer is unavailable. Uses the cached absolute
    /// `pointer_px` — the same physical-pixel basis `multipane_geometry` and the
    /// divider hit-tests use — and the focused pane's rect origin already
    /// includes the tab-bar offset, so no separate tab-bar adjustment is needed.
    pub(super) fn active_pane_pointer_cell(&self) -> Option<CellPoint> {
        let (x_px, y_px) = self.pointer_px?;
        self.active_pane_pointer_cell_at(x_px, y_px)
    }

    /// Resolve the focused pane's cell for an EXPLICIT window-pixel coordinate,
    /// rather than the active session's stored `pointer_px`.
    ///
    /// C11: `pointer_px` is a per-*session* field reached through App's `Deref`
    /// to the active session. After a focus-follows-click `set_active_focus`,
    /// `self.pointer_px` resolves to the newly-focused pane's OWN last-stored
    /// coordinate (stale — from when it was previously focused), not the click
    /// that is switching focus. The caller must therefore capture the live click
    /// coords BEFORE the focus switch and pass them here so the selection anchor
    /// lands under the actual click.
    pub(super) fn active_pane_pointer_cell_at(&self, x_px: f64, y_px: f64) -> Option<CellPoint> {
        let (content, cell) = self.multipane_geometry()?;
        let focused = self.sessions.active_id();
        let rect = self
            .sessions
            .active_pane_rects(content, PANE_DIVIDER_PX)
            .into_iter()
            .find(|(token, _)| *token == focused)
            .map(|(_, rect)| rect)?;
        Some(pane_relative_cell(rect, cell, self.grid, x_px, y_px))
    }

    /// Build the topmost window-level overlay panel for the multi-pane render
    /// path: a window-content-grid snapshot with the open overlay painted into
    /// it (via the same [`apply_overlay`] the single-pane path uses), cropped to
    /// the panel's rect so it composites as an opaque box. Returns the cropped
    /// snapshot plus its physical-pixel window-space origin, or `None` when no
    /// overlay is open. The cell math matches [`Self::overlay_grid_dims`] /
    /// [`Self::overlay_pointer_cell`] so render and hit-test agree exactly.
    fn build_overlay_top(
        &mut self,
        content: PaneRect,
        cell: CellSize,
    ) -> Option<(Snapshot, [f32; 2])> {
        if !self.overlay.is_open() {
            return None;
        }
        let (cols, rows) = grid_dims_for_rect(content, cell.width, cell.height);
        if cols == 0 || rows == 0 {
            return None;
        }
        // A blank window-content-grid snapshot; `apply_overlay` paints the panel
        // box into it at the rect it computes from these same dims.
        let mut overlay_snap = Snapshot {
            dimensions: Dimensions::new(cols, rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); cols * rows],
        };
        apply_overlay(&mut overlay_snap, &mut self.overlay);
        // Crop to the panel rect so only opaque panel cells are composited
        // (blank cells outside the box would otherwise overdraw the panes).
        let rect = overlay_rect(&self.overlay, cols, rows)?;
        let cropped = crop_snapshot(&overlay_snap, rect.left, rect.top, rect.width, rect.height);
        let origin = [
            content.x + rect.left as f32 * cell.width as f32,
            content.y + rect.top as f32 * cell.height as f32,
        ];
        Some((cropped, origin))
    }

    /// Apply an in-progress divider drag to the current pointer position,
    /// re-deriving the grabbed split's ratio and reflowing the affected panes'
    /// terminal **models + cell metrics** (via [`WorkspaceSet::reflow_all_panes_for_drag`])
    /// before requesting a repaint. No-op unless a divider is grabbed and the
    /// active tab is multi-pane. The full-window grid is unchanged by a divider
    /// drag, so this reflows pane sub-rects directly rather than through the
    /// window-resize debouncer (which keys on the whole-window grid and would
    /// early-return).
    ///
    /// COALESCING (Phase H): a drag fires one move per pixel; the kernel-side
    /// PTY resize is deliberately NOT issued here. The on-screen grid reflows
    /// live per-move, but the shell learns its new size from exactly one
    /// coalesced `resize_all_panes` the release handler flushes at drag-end —
    /// avoiding a `ResizePseudoConsole`/`SIGWINCH` flood that scrambles the
    /// shell's prompt repaint on ConPTY.
    pub(super) fn drag_divider_to_pointer(&mut self) {
        let Some(target) = self.divider_drag else {
            return;
        };
        let Some((content, cell)) = self.multipane_geometry() else {
            return;
        };
        let Some((x, y)) = self.pointer_px else {
            return;
        };
        if self
            .sessions
            .drag_active_divider(content, PANE_DIVIDER_PX, target, x as f32, y as f32)
            .is_some()
        {
            self.sessions.reflow_all_panes_for_drag(
                content,
                cell.width,
                cell.height,
                PANE_DIVIDER_PX,
            );
            self.sessions.active_mut().needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Rebuild the GPU geometry for a **multi-pane** active tab. Mirrors the
    /// single-pane rebuild's GPU hand-off but assembles one [`PaneRender`] per
    /// visible pane and calls [`GpuState::update_from_panes`].
    pub(super) fn rebuild_multipane(&mut self) {
        let Some((cell, padding, surface)) = self
            .gpu
            .as_ref()
            .map(|gpu| (gpu.cell(), gpu.window_padding(), gpu.surface_size()))
        else {
            return;
        };
        let (surface_w, surface_h) = surface;
        let show_tab_bar = self.should_show_tab_bar();
        let reserve = self.tab_reserve();
        let content = pane_content_rect(surface_w, surface_h, cell, padding, reserve);
        let treatment = self.background_treatment_params();
        let focused = self.sessions.active_id();

        // Per-pane owned snapshots (PaneRender borrows them, so they must
        // outlive the render call). Each pane is snapshotted from its own
        // terminal at its own scrollback offset.
        let rects = self.sessions.active_pane_rects(content, PANE_DIVIDER_PX);
        let mut panes_owned: Vec<(Snapshot, [f32; 2], bool, crate::core::CursorStyle)> =
            Vec::with_capacity(rects.len());
        // The focused pane's overlay inputs, captured while its terminal is
        // locked: (index into `panes_owned`, viewport offset, scrollback len).
        // Used after the loop to paint that pane's own selection + search
        // highlights with a pane-scoped ctx (1c-3c).
        let mut focused_overlay: Option<(usize, usize, usize)> = None;
        for (token, rect) in &rects {
            let Some(session) = self.sessions.get(*token) else {
                continue;
            };
            let offset = session.viewport.offset();
            let Ok(terminal) = session.terminal.lock() else {
                continue;
            };
            let snapshot = terminal.snapshot_with_scrollback(offset);
            let cursor_style = terminal.cursor_style();
            let is_focused = *token == focused;
            if is_focused {
                focused_overlay = Some((
                    panes_owned.len(),
                    offset,
                    terminal.screen().scrollback_len(),
                ));
            }
            drop(terminal);
            // Absorb each pane's sub-cell remainder onto its window-margin side
            // so the grid edge facing a divider sits flush to it: the visible
            // inter-pane separation is then exactly the 1px divider, uniform
            // across both axes (a single-pane / zoomed rect == content yields a
            // zero offset, so the byte-identical path is unchanged). The divider
            // position itself is untouched — only the grid content shifts within
            // the pane — so smooth per-pixel divider drag is preserved.
            let origin =
                crate::native::layout::pane_grid_origin(*rect, content, cell.width, cell.height);
            panes_owned.push((snapshot, origin, is_focused, cursor_style));
        }

        // Paint the focused pane's selection + search highlights onto its own
        // snapshot, keyed to that pane's grid / scrollback / viewport (not the
        // whole-window overlay_ctx). `self.selection` / `self.search` Deref to
        // the focused pane, so only the geometry inputs are pane-specific.
        if let Some((idx, viewport_offset, scrollback_len)) = focused_overlay
            && let Some((snapshot, _, _, _)) = panes_owned.get_mut(idx)
        {
            let pane_grid = snapshot.dimensions;
            self.paint_focused_pane_overlays(
                snapshot,
                pane_grid,
                viewport_offset,
                scrollback_len,
                cell,
            );
        }

        // The tab chrome (only when the bar is shown) is drawn as its own region
        // at the window's top-left, beside/above the content rect: a one-row
        // strip along the top, or the vertical rail down a side (F4-V2). Both
        // sit at `[pad, pad]` and never overlap the content rect (reserved out of
        // it above), so push order relative to the content panes is irrelevant.
        //
        // F4-P3: under rail auto-hide the pinned strip is suppressed — the rail
        // draws only as the floating overlay (`build_rail_overlay`, below). This
        // guard mirrors the single-pane `decorate_snapshot_with_tab_bar` fix: the
        // `rail_side()` dispatch reads the (zeroed) auto-hide reservation and
        // reports `None`, which would otherwise fall through to `tab_bar_strip`
        // and leak a phantom TOP bar on a side-placed window.
        //
        // Dual-band (design doc §7): the top tab bar (tabs, content width, shifted
        // right of a left rail) and the full-height workspace rail sidebar are
        // independent strips that can coexist. Each is its own owned snapshot +
        // origin + quads, pushed as a chrome pane. Auto-hide floats the rail
        // (drawn below as the overlay), so it contributes no pinned strip.
        let show_top = show_tab_bar;
        let show_rail = self.should_show_workspace_rail() && !self.rail_autohide_active();
        let mut chrome_strips: Vec<(Snapshot, [f32; 2], Vec<SolidQuad>)> = Vec::new();
        if show_top && let Some((snapshot, quads)) = self.tab_bar_strip(cell, padding) {
            chrome_strips.push((snapshot, self.top_bar_origin_px(cell), quads));
        }
        if show_rail {
            let side = self.workspace_rail_side();
            if let Some((snapshot, quads)) = self.tab_rail_strip(cell, padding, surface_h, side) {
                chrome_strips.push((snapshot, self.rail_origin_px(cell), quads));
            }
        }

        // Themed 1px dividers in the gaps between panes. None while zoomed: the
        // focused pane is full-bleed and the layout tree underneath is hidden,
        // so no divider should overdraw it.
        let divider_quads = if self.sessions.active_is_zoomed() {
            Vec::new()
        } else {
            self.sessions
                .active_layout()
                .map(|layout| {
                    let (r, g, b) = self.effective_theme.border;
                    let mut color = text::foreground_linear(Color::Rgb(r, g, b));
                    color[3] = 1.0;
                    divider_rects(layout, content, PANE_DIVIDER_PX)
                        .into_iter()
                        .map(|d| SolidQuad {
                            rect: [d.x, d.y, d.x + d.w, d.y + d.h],
                            color,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        // Assemble the borrow-bound `PaneRender` list.
        let mut panes: Vec<PaneRender> =
            Vec::with_capacity(panes_owned.len() + chrome_strips.len());
        // Each chrome strip renders from its own origin: the top bar past a left
        // rail, the rail at the window edge (right rail: far side). They never
        // overlap the content rect (reserved out of it above).
        for (snapshot, origin, _) in &chrome_strips {
            panes.push(PaneRender {
                snapshot,
                origin: *origin,
                focused: false,
                cursor_style: crate::core::CursorStyle::default(),
                focus_dim: 0.0,
                overlays: &[],
                treatment,
            });
        }
        // Inactive-pane dimming: the focused pane is never dimmed (`0.0`), the
        // others recede by the configured amount. `0.0` (the default, and the
        // forced value on the plain renderer profile) is an exact identity, so
        // every pane renders undimmed and the multi-pane frame stays
        // byte-identical to before this knob existed.
        let inactive_dim = self.settings.effective_inactive_pane_dim();
        for (snapshot, origin, is_focused, cursor_style) in &panes_owned {
            panes.push(PaneRender {
                snapshot,
                origin: *origin,
                focused: *is_focused,
                cursor_style: *cursor_style,
                focus_dim: pane_focus_dim(*is_focused, inactive_dim),
                overlays: &[],
                treatment,
            });
        }

        // Topmost window-level overlay (context menu / settings / palette /
        // connections / replay). Built against the window content grid so it
        // centers over the whole window, not the focused pane, and composited
        // last so it draws over every pane + divider. `None` when no overlay is
        // open, leaving the multi-pane frame unchanged. Owned here so the
        // `OverlayTop` borrow outlives the GPU call.
        let overlay_top = self.build_overlay_top(content, cell);
        let treatment_for_overlay = treatment;
        // Solid quads composited over the pane snapshots: the inter-pane
        // dividers plus the tab strip's own quads (the active-tab outline). Both
        // draw above the pane content; the active-tab outline lands on the
        // top-row strip and the dividers in the content gaps, so they never
        // overlap. Empty when the strip is hidden / no outline is emitted, so
        // the zoomed and single-tab multi-pane frames are unchanged.
        let mut frame_quads = divider_quads;
        for (_, _, strip_quads) in &chrome_strips {
            frame_quads.extend_from_slice(strip_quads);
        }
        // F4-P1 unified tab panel + seam: background-segment quads behind the tab
        // chrome (same layer as the NF11 edge wash). Empty when the bar is hidden
        // / panel off / seam off, so the multi-pane frame stays byte-identical.
        let tab_bg_quads = self.tab_panel_bg_quads(cell);
        // F4-P3: the revealed rail auto-hide overlay strip (floating over the
        // multi-pane content). Owned here so its snapshot outlives the GPU call;
        // `None` unless the floating rail is currently revealed.
        let rail_overlay_data = self.build_rail_overlay(cell);
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_scroll_frac_offset(0.0);
            let overlay = overlay_top.as_ref().map(|(snapshot, origin)| OverlayTop {
                snapshot,
                origin: *origin,
                treatment: treatment_for_overlay,
            });
            let rail_overlay = rail_overlay_data.as_ref().map(|data| RailOverlay {
                snapshot: &data.snapshot,
                origin: data.origin,
                treatment: treatment_for_overlay,
                wash: data.wash,
                seam: data.seam,
            });
            gpu.update_from_panes(&panes, &frame_quads, overlay, &tab_bg_quads, rail_overlay);
        }
        // Multi-pane v1 does not participate in the single-pane render-signature
        // cache; it rebuilds whenever a visible pane requests a redraw. Reset
        // the cache so the first frame after returning to a single-pane tab does
        // a full rebuild rather than trusting a stale signature.
        self.last_render_signature = None;
    }

    /// Build a one-row snapshot carrying the tab strip glyphs, for the
    /// multi-pane path to draw as the topmost pane. Returns the snapshot plus
    /// the tab bar's solid quads (empty in the current cell-background-based
    /// integration, but threaded through for parity).
    fn tab_bar_strip(
        &self,
        cell: CellSize,
        padding: WindowPadding,
    ) -> Option<(Snapshot, Vec<SolidQuad>)> {
        // The top bar spans the CONTENT columns (window width minus a side rail
        // band); `tab_bar_grid_cols` shares the reserve basis, so the strip and
        // its hit-test agree. With no rail this equals the full window columns
        // (byte-identical to the pre-rail top-only strip).
        let columns = self.tab_bar_grid_cols();
        if columns == 0 {
            return None;
        }
        let mut snapshot = Snapshot {
            dimensions: Dimensions::new(columns, TAB_BAR_ROWS as usize),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); columns * TAB_BAR_ROWS as usize],
        };
        let output = self.tab_bar.render(
            &self.sessions,
            columns,
            padding.as_f32(),
            cell,
            padding,
            self.tab_bar_colors(),
            self.tab_panel_strength(),
        );
        for glyph in output.glyphs {
            if glyph.col < columns {
                snapshot.cells[glyph.col] = crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        Some((snapshot, output.quads))
    }

    /// Build the vertical tab rail as a `rail_cols × grid_rows` snapshot for the
    /// multi-pane path to draw as its own region (F4-V2). Mirrors
    /// [`Self::tab_bar_strip`] but on the column axis: the rail spans the window
    /// content rows and is drawn at the window's top-left (`[pad, pad]`), beside
    /// the content rect. Returns the snapshot plus the rail widget's overlay
    /// quads (empty under Phosphor Flat — the F4-P1 panel wash + seam are
    /// separate background-segment quads built by `tab_panel_bg_quads`).
    fn tab_rail_strip(
        &self,
        cell: CellSize,
        padding: WindowPadding,
        surface_h: u32,
        side: RailSide,
    ) -> Option<(Snapshot, Vec<SolidQuad>)> {
        let pad = padding.physical_px();
        let rail_cols = self.rail_cols();
        let grid_rows =
            (surface_h.saturating_sub(pad.saturating_mul(2)) / cell.height.max(1)) as usize;
        if rail_cols == 0 || grid_rows == 0 {
            return None;
        }
        let mut snapshot = Snapshot {
            dimensions: Dimensions::new(rail_cols, grid_rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); rail_cols * grid_rows],
        };
        let output = self.tab_rail.render(
            &self.sessions.rail_source(),
            rail_cols,
            grid_rows,
            [padding.as_f32(), padding.as_f32()],
            cell,
            side,
            self.tab_bar_colors(),
            self.rail_geom(),
            self.tab_panel_strength(),
        );
        for glyph in output.glyphs {
            if glyph.row < grid_rows && glyph.col < rail_cols {
                let idx = glyph.row * rail_cols + glyph.col;
                snapshot.cells[idx] = crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        Some((snapshot, output.quads))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::CellSize;
    use crate::native::app::TAB_BAR_ROWS;
    use crate::native::viewport::grid_dimensions_for_with_padding;

    fn cell() -> CellSize {
        CellSize {
            width: 10,
            height: 20,
            baseline: 0,
        }
    }

    #[test]
    fn single_pane_content_rect_matches_the_legacy_grid_math() {
        // The pane content rect's cell dimensions must equal the dims the
        // single-pane resize path produces, so a lone-leaf tab stays
        // byte-identical (no tab bar case).
        let cell = cell();
        let padding = WindowPadding::from_logical(8.0, 1.0);
        let (w, h) = (1280u32, 800u32);
        let rect = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
        let (cols, rows) = crate::native::layout::grid_dims_for_rect(rect, cell.width, cell.height);
        let legacy = grid_dimensions_for_with_padding(w, h, cell, padding);
        assert_eq!((cols, rows), (legacy.columns, legacy.rows));
    }

    #[test]
    fn tab_bar_shrinks_the_content_rect_by_exactly_the_strip() {
        let cell = cell();
        let padding = WindowPadding::from_logical(8.0, 1.0);
        let (w, h) = (1280u32, 800u32);
        let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
        let with = pane_content_rect(w, h, cell, padding, TabReserve::top());
        // Same width and x; the strip eats `TAB_BAR_ROWS` cell-heights off the
        // top, shifting y down and reducing height by the same amount.
        assert!((without.w - with.w).abs() < f32::EPSILON);
        assert!((without.x - with.x).abs() < f32::EPSILON);
        let strip = cell.height as f32 * TAB_BAR_ROWS as f32;
        assert!((with.y - (without.y + strip)).abs() < f32::EPSILON);
        assert!((with.h - (without.h - strip)).abs() < f32::EPSILON);
    }

    #[test]
    fn left_rail_shrinks_the_content_rect_by_exactly_the_rail_width() {
        // F4-V2: a left rail reserves `left_cols` off the LEFT — content width
        // shrinks and its x-origin shifts right by the rail width; height and y
        // are unchanged (the rail reserves columns, not rows).
        let cell = cell();
        let padding = WindowPadding::from_logical(8.0, 1.0);
        let (w, h) = (1280u32, 800u32);
        let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
        let reserve = TabReserve {
            top_rows: 0,
            left_cols: 16,
            right_cols: 0,
            gap_cols: 0,
        };
        let with = pane_content_rect(w, h, cell, padding, reserve);
        let rail = cell.width as f32 * 16.0;
        assert!((with.y - without.y).abs() < f32::EPSILON, "y unchanged");
        assert!(
            (with.h - without.h).abs() < f32::EPSILON,
            "height unchanged"
        );
        assert!(
            (with.x - (without.x + rail)).abs() < f32::EPSILON,
            "x shifts right by the rail width"
        );
        assert!(
            (with.w - (without.w - rail)).abs() < f32::EPSILON,
            "width shrinks by the rail width"
        );
    }

    #[test]
    fn right_rail_shrinks_the_content_rect_from_the_right_keeping_the_left_origin() {
        // F4-P2 layout mirror: a right rail reserves `right_cols` (+ gap) off the
        // RIGHT — the content width shrinks but its x-origin stays put (content on
        // the LEFT), the mirror of the left rail (which shifts the origin right).
        // y/height are unchanged (a rail reserves columns, not rows).
        let cell = cell();
        let padding = WindowPadding::from_logical(8.0, 1.0);
        let (w, h) = (1280u32, 800u32);
        let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
        let reserve = TabReserve {
            top_rows: 0,
            left_cols: 0,
            right_cols: 16,
            gap_cols: 2,
        };
        let with = pane_content_rect(w, h, cell, padding, reserve);
        let reserved = cell.width as f32 * (16.0 + 2.0);
        assert!(
            (with.x - without.x).abs() < f32::EPSILON,
            "x-origin stays put (content on the left)"
        );
        assert!((with.y - without.y).abs() < f32::EPSILON, "y unchanged");
        assert!(
            (with.h - without.h).abs() < f32::EPSILON,
            "height unchanged"
        );
        assert!(
            (with.w - (without.w - reserved)).abs() < f32::EPSILON,
            "width shrinks from the right by band + gap"
        );
        // The gap only counts on the rail side: no left reservation here.
        assert_eq!(reserve.left_reserved_cols(), 0);
        assert_eq!(reserve.right_reserved_cols(), 18);
    }

    #[test]
    fn left_rail_gap_shifts_content_by_the_band_plus_the_gap() {
        // R1.1: the rail↔content wallpaper gap widens the reservation beyond the
        // rail band, so content shifts right (and shrinks) by band + gap.
        let cell = cell();
        let padding = WindowPadding::from_logical(8.0, 1.0);
        let (w, h) = (1280u32, 800u32);
        let without = pane_content_rect(w, h, cell, padding, TabReserve::NONE);
        let reserve = TabReserve {
            top_rows: 0,
            left_cols: 16,
            right_cols: 0,
            gap_cols: 2,
        };
        let with = pane_content_rect(w, h, cell, padding, reserve);
        let reserved = cell.width as f32 * (16.0 + 2.0);
        assert!(
            (with.x - (without.x + reserved)).abs() < f32::EPSILON,
            "x shifts by band + gap"
        );
        assert!(
            (with.w - (without.w - reserved)).abs() < f32::EPSILON,
            "width shrinks by band + gap"
        );
        // The gap only counts on the rail side: no right reservation here.
        assert_eq!(reserve.right_reserved_cols(), 0);
        assert_eq!(reserve.left_reserved_cols(), 18);
    }

    #[test]
    fn rail_content_gap_replicates_the_padding_band_cell_quantized() {
        let cell = cell(); // width 10
        // pad 8px @1x → ceil(8/10) = 1 column.
        let one = WindowPadding::from_logical(8.0, 1.0);
        assert_eq!(rail_content_gap_cols(one, cell), 1);
        // pad 24px → ceil(24/10) = 3 columns.
        let three = WindowPadding::from_logical(24.0, 1.0);
        assert_eq!(rail_content_gap_cols(three, cell), 3);
        // No padding → no gap (faithful "replicate the padding band").
        let zero = WindowPadding::from_logical(0.0, 1.0);
        assert_eq!(rail_content_gap_cols(zero, cell), 0);
    }

    #[test]
    fn gap_cols_off_the_top_bar_reserves_nothing_extra() {
        // The top-bar reservation carries no side gap, so the content columns are
        // unchanged (byte-identical top-bar path).
        let r = TabReserve::top();
        assert_eq!(r.left_reserved_cols(), 0);
        assert_eq!(r.right_reserved_cols(), 0);
    }

    #[test]
    fn pane_focus_dim_focused_is_always_identity() {
        // The focused pane is never dimmed regardless of the configured amount.
        assert_eq!(pane_focus_dim(true, 0.0), 0.0);
        assert_eq!(pane_focus_dim(true, 0.3), 0.0);
        assert_eq!(pane_focus_dim(true, 1.0), 0.0);
    }

    #[test]
    fn pane_focus_dim_inactive_uses_configured_amount() {
        // Non-focused panes recede by exactly the configured value.
        assert_eq!(pane_focus_dim(false, 0.25), 0.25);
        assert_eq!(pane_focus_dim(false, 1.0), 1.0);
    }

    #[test]
    fn pane_focus_dim_off_is_byte_identical_for_every_pane() {
        // The default-off path: with `inactive_dim == 0.0` both the focused and
        // inactive panes get `0.0`, identical to the pre-feature hardcoded
        // value, so the multi-pane frame is byte-identical. The grid layer
        // already proves `focus_dim == 0.0` is an exact no-op.
        assert_eq!(pane_focus_dim(true, 0.0), 0.0);
        assert_eq!(pane_focus_dim(false, 0.0), 0.0);
    }

    // --- Window-level overlay geometry (multi-pane) ---

    fn filled_snapshot(cols: usize, rows: usize, ch: char) -> Snapshot {
        Snapshot {
            dimensions: Dimensions::new(cols, rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: (0..cols * rows)
                .map(|i| {
                    // Encode the linear index so the crop can be checked cell-wise.
                    let c = char::from_u32('a' as u32 + (i as u32 % 26)).unwrap_or(ch);
                    crate::core::Cell::new(c, crate::core::Attrs::default())
                })
                .collect(),
        }
    }

    #[test]
    fn crop_snapshot_copies_the_requested_subrect() {
        // A 6x4 source cropped to a 3x2 box at (left=2, top=1) yields exactly
        // those cells, in order, with the cropped dimensions.
        let src = filled_snapshot(6, 4, 'x');
        let cropped = crop_snapshot(&src, 2, 1, 3, 2);
        assert_eq!(cropped.dimensions, Dimensions::new(3, 2));
        for r in 0..2 {
            for c in 0..3 {
                let src_idx = (1 + r) * 6 + (2 + c);
                let dst_idx = r * 3 + c;
                assert_eq!(
                    cropped.cells[dst_idx].ch, src.cells[src_idx].ch,
                    "cell ({r},{c}) mismatch"
                );
            }
        }
    }

    #[test]
    fn crop_snapshot_out_of_bounds_falls_back_to_default() {
        // A crop that runs past the source edge fills the overflow with default
        // cells rather than panicking (defensive path).
        let src = filled_snapshot(3, 3, 'x');
        let cropped = crop_snapshot(&src, 2, 2, 3, 3);
        assert_eq!(cropped.dimensions, Dimensions::new(3, 3));
        // Top-left came from src (2,2); the rest overflow to default (space).
        assert_eq!(cropped.cells[0].ch, src.cells[2 * 3 + 2].ch);
        let default_ch = crate::core::Cell::default().ch;
        assert_eq!(cropped.cells[8].ch, default_ch);
    }

    #[test]
    fn window_overlay_cell_maps_into_the_content_grid() {
        // Content rect offset from the window origin by (x=10, y=40) — e.g. a
        // tab bar pushes y down. A pointer inside maps to the content-grid cell
        // relative to that origin, NOT the raw window origin.
        let cell = cell(); // 10x20
        let content = PaneRect::new(10.0, 40.0, 200.0, 200.0); // 20x10 cells
        // Pointer at window px (35, 75): col = (35-10)/10 = 2, row = (75-40)/20 = 1.
        let mapped = window_overlay_cell(content, cell, 35.0, 75.0).expect("cell");
        assert_eq!(mapped, CellPoint { row: 1, column: 2 });
    }

    #[test]
    fn window_overlay_cell_clamps_to_grid_bounds() {
        let cell = cell();
        let content = PaneRect::new(10.0, 40.0, 200.0, 200.0); // 20x10 cells
        // Far past the bottom-right: clamps to the last cell.
        let mapped = window_overlay_cell(content, cell, 9000.0, 9000.0).expect("cell");
        assert_eq!(mapped, CellPoint { row: 9, column: 19 });
        // Above/left of the content origin clamps to (0,0).
        let mapped = window_overlay_cell(content, cell, 0.0, 0.0).expect("cell");
        assert_eq!(mapped, CellPoint { row: 0, column: 0 });
    }

    #[test]
    fn tab_bar_hit_test_columns_match_the_rendered_strip_width() {
        // Bug A guard: the tab strip renders across the window content columns
        // (`tab_bar_strip`: (surface_w - 2·pad)/cell.width), and the hit-test
        // must use the *same* column count (`tab_bar_grid_cols` ==
        // `overlay_grid_dims().0` == grid_dims_for_rect over the content rect).
        // If these diverged, multi-pane tabs would render at one set of columns
        // and hit-test at another (the focused pane's narrower sub-grid),
        // misaligning hover and dropping clicks. This proves the two formulas
        // agree across a matrix of widths and paddings.
        let cell = cell(); // 10x20
        for surface_w in [320u32, 800, 1280, 1366, 1920, 37] {
            for pad_logical in [0.0f32, 1.0, 8.0, 12.0] {
                let padding = WindowPadding::from_logical(pad_logical, 1.0);
                let pad = padding.physical_px();
                // The render-side strip column formula.
                let strip_cols =
                    (surface_w.saturating_sub(pad.saturating_mul(2)) / cell.width.max(1)) as usize;
                // The hit-test-side content column count (what tab_bar_grid_cols
                // returns in multi-pane: grid_dims_for_rect over the content
                // rect). Tab bar shown, so the height arg is irrelevant to cols.
                let content = pane_content_rect(surface_w, 800, cell, padding, TabReserve::top());
                let (content_cols, _) =
                    crate::native::layout::grid_dims_for_rect(content, cell.width, cell.height);
                // `grid_dims_for_rect` floors at 1, so compare against the strip
                // formula clamped the same way (the strip bails when cols == 0).
                assert_eq!(
                    content_cols,
                    strip_cols.max(1),
                    "surface_w={surface_w} pad={pad}: render/hit-test column mismatch"
                );
            }
        }
    }

    #[test]
    fn window_overlay_cell_degenerate_content_clamps_to_origin() {
        // `grid_dims_for_rect` floors a degenerate rect at a 1x1 grid, so the
        // mapping clamps any pointer to the single (0,0) cell rather than
        // returning `None` — defensive against a zero-size content rect.
        let cell = cell();
        let content = PaneRect::new(0.0, 0.0, 0.0, 0.0);
        assert_eq!(
            window_overlay_cell(content, cell, 5.0, 5.0),
            Some(CellPoint { row: 0, column: 0 })
        );
    }
}

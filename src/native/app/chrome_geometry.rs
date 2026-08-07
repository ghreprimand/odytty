// SPDX-License-Identifier: GPL-3.0-only
//! Shared window-pixel geometry for top-tab and workspace-rail chrome.

use super::tab_bar::{self, TabBarSource, TabHit};
use super::tab_rail;
use super::*;
use crate::theme::Srgb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewSlot {
    Tab(usize),
    Gap,
}

/// Render order for an armed drag. The origin is removed and one gap is
/// inserted at the position the commit engine would place it.
pub(super) fn preview_order(count: usize, origin: usize, drop_idx: usize) -> Vec<PreviewSlot> {
    if count == 0 || origin >= count {
        return (0..count).map(PreviewSlot::Tab).collect();
    }
    let mut order: Vec<_> = (0..count)
        .filter(|idx| *idx != origin)
        .map(PreviewSlot::Tab)
        .collect();
    let destination = if drop_idx > origin {
        drop_idx - 1
    } else {
        drop_idx
    }
    .min(order.len());
    order.insert(destination, PreviewSlot::Gap);
    order
}

pub(super) struct PreviewSource<'a> {
    source: &'a dyn TabBarSource,
    order: Vec<PreviewSlot>,
    origin: usize,
}

impl<'a> PreviewSource<'a> {
    pub(super) fn new(source: &'a dyn TabBarSource, origin: usize, drop_idx: usize) -> Self {
        Self {
            order: preview_order(source.tab_count(), origin, drop_idx),
            source,
            origin,
        }
    }

    pub(super) fn gap_idx(&self) -> Option<usize> {
        self.order.iter().position(|slot| *slot == PreviewSlot::Gap)
    }
}

impl TabBarSource for PreviewSource<'_> {
    fn tab_count(&self) -> usize {
        self.order.len()
    }

    fn tab_title(&self, idx: usize) -> &str {
        match self.order[idx] {
            PreviewSlot::Tab(source_idx) => self.source.tab_title(source_idx),
            PreviewSlot::Gap => self.source.tab_title(self.origin),
        }
    }

    fn active_tab(&self) -> usize {
        let active = self.source.active_tab();
        self.order
            .iter()
            .position(|slot| match slot {
                PreviewSlot::Tab(source_idx) => *source_idx == active,
                PreviewSlot::Gap => self.origin == active,
            })
            .unwrap_or(usize::MAX)
    }

    fn tab_bound(&self, idx: usize) -> bool {
        match self.order[idx] {
            PreviewSlot::Tab(source_idx) => self.source.tab_bound(source_idx),
            PreviewSlot::Gap => self.source.tab_bound(self.origin),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PxPoint {
    pub(super) x: f64,
    pub(super) y: f64,
}

impl PxPoint {
    pub(super) const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PxRect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) width: f64,
    pub(super) height: f64,
}

impl PxRect {
    fn contains(self, point: PxPoint) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }

    fn main_start(self, axis: Axis) -> f64 {
        match axis {
            Axis::Horizontal => self.x,
            Axis::Vertical => self.y,
        }
    }

    fn main_span(self, axis: Axis) -> f64 {
        match axis {
            Axis::Horizontal => self.width,
            Axis::Vertical => self.height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SlotGeom {
    pub(super) idx: usize,
    pub(super) rect: PxRect,
    pub(super) close: Option<PxRect>,
    pub(super) label_origin: PxPoint,
}

/// Frame-consistent chrome slot geometry in window physical pixels.
#[derive(Debug, Clone)]
pub(super) struct ChromeSlotGeom {
    pub(super) band: PxRect,
    pub(super) axis: Axis,
    pub(super) slots: Vec<SlotGeom>,
    pub(super) new_slot: Option<PxRect>,
    /// RAIL-AUTOHIDE-CTL: the auto-hide toggle rect at the rail's bottom edge, or
    /// `None` for the top bar (which has no such control). Hit-tested ahead of
    /// the slots so a click on it toggles rather than switching a workspace.
    pub(super) autohide: Option<PxRect>,
    pub(super) cell: CellSize,
}

impl ChromeSlotGeom {
    pub(super) fn top(
        source: &dyn TabBarSource,
        grid_cols: usize,
        rows: usize,
        origin: [f32; 2],
        cell: CellSize,
    ) -> Self {
        let cw = f64::from(cell.width);
        let ch = f64::from(cell.height);
        let ox = f64::from(origin[0]);
        let oy = f64::from(origin[1]);
        let layout = tab_bar::compute_layout(source, grid_cols);
        let band_height = rows.max(1) as f64 * ch;
        let slots = layout
            .slots
            .iter()
            .map(|slot| {
                let rect = PxRect {
                    x: ox + slot.start_col as f64 * cw,
                    y: oy,
                    width: (slot.end_col - slot.start_col) as f64 * cw,
                    height: band_height,
                };
                let close = slot.close_col.map(|col| PxRect {
                    x: ox + col as f64 * cw,
                    y: oy,
                    width: cw,
                    height: band_height,
                });
                SlotGeom {
                    idx: slot.idx,
                    rect,
                    close,
                    label_origin: PxPoint::new(
                        ox + slot.label_col as f64 * cw,
                        oy + rows.saturating_sub(1) as f64 * ch / 2.0,
                    ),
                }
            })
            .collect();
        let new_slot = layout.new_tab_col.map(|col| PxRect {
            x: ox + col as f64 * cw,
            y: oy,
            width: 3.0 * cw,
            height: band_height,
        });
        Self {
            band: PxRect {
                x: ox,
                y: oy,
                width: grid_cols as f64 * cw,
                height: band_height,
            },
            axis: Axis::Horizontal,
            slots,
            new_slot,
            autohide: None,
            cell,
        }
    }

    pub(super) fn rail(
        source: &dyn TabBarSource,
        rail_cols: usize,
        grid_rows: usize,
        origin: [f32; 2],
        cell: CellSize,
        geom: tab_rail::RailGeom,
    ) -> Self {
        let cw = f64::from(cell.width);
        let ch = f64::from(cell.height);
        let ox = f64::from(origin[0]);
        let oy = f64::from(origin[1]);
        let layout = tab_rail::compute_rail_layout(source, rail_cols, grid_rows, geom);
        let slots = layout
            .slots
            .iter()
            .map(|slot| {
                let rect = PxRect {
                    x: ox,
                    y: oy + slot.start_row as f64 * ch,
                    width: rail_cols as f64 * cw,
                    height: (slot.end_row - slot.start_row) as f64 * ch,
                };
                let close = slot.close_cell.map(|(row, col)| PxRect {
                    x: ox + col as f64 * cw,
                    y: oy + row as f64 * ch,
                    width: cw,
                    height: ch,
                });
                SlotGeom {
                    idx: slot.idx,
                    rect,
                    close,
                    label_origin: PxPoint::new(
                        ox + tab_rail::SLOT_LABEL_START_COL as f64 * cw,
                        oy + slot.label_row as f64 * ch,
                    ),
                }
            })
            .collect();
        let new_slot = layout.new_tab_rows.map(|(start, end)| PxRect {
            x: ox,
            y: oy + start as f64 * ch,
            width: rail_cols as f64 * cw,
            height: (end - start) as f64 * ch,
        });
        // RAIL-AUTOHIDE-CTL: one-row rect at the rail's bottom edge.
        let autohide = layout.autohide_row.map(|row| PxRect {
            x: ox,
            y: oy + row as f64 * ch,
            width: rail_cols as f64 * cw,
            height: tab_rail::AUTOHIDE_CONTROL_ROWS as f64 * ch,
        });
        Self {
            band: PxRect {
                x: ox,
                y: oy,
                width: rail_cols as f64 * cw,
                height: grid_rows as f64 * ch,
            },
            axis: Axis::Vertical,
            slots,
            new_slot,
            autohide,
            cell,
        }
    }

    pub(super) fn hit(&self, point: PxPoint) -> TabHit {
        if !self.band.contains(point) {
            return TabHit::None;
        }
        // RAIL-AUTOHIDE-CTL: the toggle is armed ahead of the slot hits (same
        // laddering discipline as the button/OSC-8 arms), so a click on the
        // bottom-edge control toggles auto-hide rather than switching a slot.
        if self.autohide.is_some_and(|rect| rect.contains(point)) {
            return TabHit::AutohideToggle;
        }
        if self.new_slot.is_some_and(|rect| rect.contains(point)) {
            return TabHit::NewTab;
        }
        for slot in self.slots.iter().rev() {
            if !slot.rect.contains(point) {
                continue;
            }
            if slot.close.is_some_and(|rect| rect.contains(point)) {
                return TabHit::Close(slot.idx);
            }
            return TabHit::Switch(slot.idx);
        }
        TabHit::None
    }

    pub(super) fn drop_index(&self, main_axis_px: f64, origin: usize) -> Option<usize> {
        let main_cell = match self.axis {
            Axis::Horizontal => self.cell.width,
            Axis::Vertical => self.cell.height,
        };
        if main_cell == 0 {
            return None;
        }
        self.slots.iter().find(|slot| slot.idx == origin)?;
        let retained: Vec<_> = self
            .slots
            .iter()
            .filter(|slot| slot.idx != origin)
            .collect();
        let mut insert = retained.last()?.idx + 1;
        for slot in retained {
            let midpoint = slot.rect.main_start(self.axis) + slot.rect.main_span(self.axis) / 2.0;
            if main_axis_px < midpoint {
                insert = slot.idx;
                break;
            }
        }
        Some(insert)
    }

    pub(super) fn grab_metrics(&self, point: PxPoint, origin: usize) -> Option<(f64, f64)> {
        let main_cell = match self.axis {
            Axis::Horizontal => self.cell.width,
            Axis::Vertical => self.cell.height,
        };
        if main_cell == 0 {
            return None;
        }
        let slot = self.slots.iter().find(|slot| slot.idx == origin)?;
        let position = match self.axis {
            Axis::Horizontal => point.x,
            Axis::Vertical => point.y,
        };
        let start = slot.rect.main_start(self.axis);
        let span = slot.rect.main_span(self.axis);
        Some(((position - start).clamp(0.0, span), span))
    }

    pub(super) fn insertion_boundary_px(&self, drop_idx: usize, origin: usize) -> f64 {
        let retained: Vec<_> = self
            .slots
            .iter()
            .filter(|slot| slot.idx != origin)
            .collect();
        retained
            .iter()
            .find(|slot| slot.idx == drop_idx)
            .map_or_else(
                || {
                    retained
                        .last()
                        .map_or(self.band.main_start(self.axis), |slot| {
                            slot.rect.main_start(self.axis) + slot.rect.main_span(self.axis)
                        })
                },
                |slot| slot.rect.main_start(self.axis),
            )
    }

    pub(super) fn insertion_indicator(
        &self,
        drop_idx: usize,
        origin: usize,
        color: [f32; 4],
    ) -> SolidQuad {
        let boundary = self.insertion_boundary_px(drop_idx, origin);
        let cross_inset = 2.0;
        let rect = match self.axis {
            Axis::Horizontal => [
                (boundary - 1.0) as f32,
                (self.band.y + cross_inset) as f32,
                (boundary + 1.0) as f32,
                (self.band.y + self.band.height - cross_inset) as f32,
            ],
            Axis::Vertical => [
                (self.band.x + cross_inset) as f32,
                (boundary - 1.0) as f32,
                (self.band.x + self.band.width - cross_inset) as f32,
                (boundary + 1.0) as f32,
            ],
        };
        SolidQuad { rect, color }
    }
}

pub(super) fn chrome_accent_color(color: Srgb) -> [f32; 4] {
    [
        text::srgb_to_linear(color.0),
        text::srgb_to_linear(color.1),
        text::srgb_to_linear(color.2),
        1.0,
    ]
}

impl App {
    pub(super) fn top_strip_geom(&self, cell: CellSize) -> Option<ChromeSlotGeom> {
        self.should_show_tab_bar().then(|| {
            ChromeSlotGeom::top(
                &self.sessions,
                self.tab_bar_grid_cols(),
                self.tab_bar_rows(),
                self.top_bar_origin_px(cell),
                cell,
            )
        })
    }

    pub(super) fn rail_geom_px(&self, cell: CellSize) -> Option<ChromeSlotGeom> {
        let (cols, origin) = if self.rail_autohide_active() {
            let side = self.rail_autohide_side()?;
            if !self.rail_overlay_visible() {
                return None;
            }
            (
                self.rail_overlay_cols(),
                self.rail_overlay_origin_px(cell, side),
            )
        } else if self.should_show_workspace_rail() {
            (self.rail_cols(), self.rail_origin_px(cell))
        } else {
            return None;
        };
        Some(ChromeSlotGeom::rail(
            &self.sessions.rail_source(),
            cols,
            self.tab_rail_grid_rows(),
            origin,
            cell,
            self.rail_geom(),
        ))
    }
}

impl App {
    pub(super) fn tab_bar_height_px(&self, cell: CellSize) -> f32 {
        cell.height as f32 * self.tab_reserve().top_rows as f32
    }

    /// The tab-chrome reservation for the current frame. The top tab bar (tabs of
    /// the active workspace) reserves rows off the top whenever it is shown; the
    /// workspace rail reserves columns off its side whenever it is shown and not
    /// auto-hidden (auto-hide floats the rail as an overlay, reserving nothing).
    /// The two are independent — a frame can reserve BOTH (tabs on top,
    /// workspaces down the side), just one, or `NONE` (the byte-identical
    /// no-chrome case: a single workspace whose active tab needs no bar).
    pub(super) fn tab_reserve(&self) -> panes::TabReserve {
        let top_rows = if self.should_show_tab_bar() {
            self.tab_bar_rows()
        } else {
            0
        };
        // F4-P3: under rail auto-hide the rail reserves NOTHING — it draws as a
        // floating overlay when revealed (never reflows content). The top bar is
        // never auto-hidden, so its rows stay reserved independently.
        let (left_cols, right_cols, gap_cols) =
            if self.should_show_workspace_rail() && !self.rail_autohide_active() {
                // F4-P1/P4: the band width resolves the `tab_rail_width` mode —
                // `Manual(cols)` clamps the fixed width, `Auto` sizes to the
                // longest workspace name (`rail_auto_want_cols`).
                let rail_cols = self.settings.rail_width_cols(self.rail_auto_want_cols());
                match self.workspace_rail_side() {
                    RailSide::Left => (rail_cols, 0, 0),
                    // F4-P2: a right rail reserves its band off the RIGHT;
                    // the content stays at column 0 (mirror of the left arm).
                    RailSide::Right => (0, rail_cols, 0),
                }
            } else {
                (0, 0, 0)
            };
        if top_rows == 0 && left_cols == 0 && right_cols == 0 {
            return panes::TabReserve::NONE;
        }
        panes::TabReserve {
            top_rows,
            left_cols,
            right_cols,
            gap_cols,
        }
    }

    pub(super) fn rail_geom(&self) -> tab_rail::RailGeom {
        tab_rail::RailGeom {
            slot_rows: self.settings.rail_slot_rows(),
            slot_gap: self.settings.rail_slot_gap_rows(),
        }
    }

    /// The longest tab title in cells (F4-P4 auto-width): each Unicode scalar
    /// counts as one column, matching the rail widget's `truncate_label` (the
    /// wide-glyph display-width caveat is F4P-NF1, out of scope). Trimmed like
    /// the widget so trailing spaces never pad the auto width.
    pub(super) fn rail_longest_title_cols(&self) -> usize {
        use tab_bar::TabBarSource;
        // The rail lists WORKSPACES, so auto-width sizes to the longest workspace
        // name, not the active workspace's tab titles (§7.1).
        let source = self.sessions.rail_source();
        (0..source.tab_count())
            .map(|idx| source.tab_title(idx).trim().chars().count())
            .max()
            .unwrap_or(0)
    }

    /// The rail width (cells) `Auto` mode wants: the longest title plus the
    /// widget's label chrome (F4-P4). `Settings::rail_width_cols` clamps it to
    /// the auto max; in `Manual` mode this is ignored.
    pub(super) fn rail_auto_want_cols(&self) -> usize {
        self.rail_longest_title_cols() + tab_rail::RAIL_LABEL_CHROME_COLS
    }

    /// F4-P4 auto-width reconcile: when the resolved rail band width diverges
    /// from what the content grid was last reserved against — a tab added or
    /// closed, a title renamed, or a shell-set (OSC 0/2) title changing the
    /// longest title — reflow the grid once so the content matches the new rail
    /// width. Gated on the width actually changing, so a stable frame is a
    /// single `usize` comparison; a no-rail / manual-width frame never diverges.
    /// Run once per redraw before the frame is built, so the rail and content
    /// stay pixel-aligned within the frame.
    pub(super) fn reconcile_rail_auto_width(&mut self) {
        if self.gpu.is_none() || self.window.is_none() {
            return;
        }
        if self.rail_cols() != self.rail_reserved_cols {
            // `recompute_grid_for_tab_bar` refreshes `rail_reserved_cols`, so a
            // no-change follow-up frame won't reflow again.
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
        }
    }

    /// The rail band width in cells when a vertical rail is active this frame,
    /// else 0. The band meets content directly at the shared seam.
    pub(super) fn rail_cols(&self) -> usize {
        let r = self.tab_reserve();
        r.left_cols + r.right_cols
    }

    /// Whether the current pointer sits over the tab-chrome band (the horizontal
    /// top bar or a side rail) rather than the terminal content. Used to route an
    /// empty-area right-click to the `TabStripEmpty` surface instead of leaking
    /// the content menu over the bar (NF-F7-2). Returns `false` off a shown bar,
    /// and — under rail auto-hide — only while the floating rail is actually
    /// revealed under the pointer.
    ///
    /// CHROME-GAP: the band is bounded at its DRAWN edge, not at the gap-inset
    /// content rect — the padding-wide neutral strips between the bands and the
    /// content route to content, consistently with left-click. The bar's
    /// horizontal extent is its joined-band background span, which abuts a
    /// pinned rail band, so the chrome-chrome junction strip stays chrome.
    pub(super) fn pointer_in_tab_chrome_band(&self) -> bool {
        if !self.any_chrome_shown() {
            return false;
        }
        // The workspace rail owns its column band (including the corner over the
        // top bar — it is a full-height sidebar), so test it first.
        if self.pointer_in_workspace_rail_band() {
            return true;
        }
        // The top tab bar owns the rows above the content, within the content
        // columns (to the right of a left rail / left of a right rail).
        if !self.should_show_tab_bar() {
            return false;
        }
        let Some((x_px, y_px)) = self.window_pointer_px else {
            return false;
        };
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        let Some((w, h, padding)) = self.resolved_surface() else {
            return false;
        };
        let content = pane_content_rect(w, h, cell, padding, self.tab_reserve());
        let gap = self.tab_reserve().chrome_gap(padding);
        // Drawn bar extent: bottom at the band's painted edge (a gap above the
        // content top), horizontally the joined-band background span (out to a
        // pinned rail band's edge on either side; the window edge otherwise).
        // The right bound uses the same grid basis the bands are painted from
        // (see `chrome_right_hit_boundary_px`): the content rect's pixel width
        // carries a sub-cell remainder the drawn seam does not.
        (y_px as f32) < content.y - gap.top
            && (x_px as f32) >= content.x - gap.left
            && (x_px as f32) < self.chrome_right_hit_boundary_px(content, gap.right, cell)
    }

    /// The right-hand hit boundary of the chrome bands, in physical px. With a
    /// pinned RIGHT rail this is the band's painted origin
    /// ([`Self::rail_origin_px`], grid basis: `pad + columns·cell_w + gap`), so
    /// the hit test meets the drawn seam exactly — the content rect's
    /// un-floored pixel width carries a sub-cell remainder (`width % cell_w`),
    /// and bounding at `content.x + content.w` would put the boundary that
    /// remainder RIGHT of the painted edge, routing the innermost sliver of
    /// the drawn band to the content menu. The left twin has no such drift: a
    /// left band's whole-column reserve keeps `content.x` grid-exact, with the
    /// remainder accumulating on the content's right edge. Without a pinned
    /// right rail the historical content-rect bound is kept byte-identically
    /// (`gap.right` is 0 there).
    pub(super) fn chrome_right_hit_boundary_px(
        &self,
        content: PaneRect,
        gap_right: f32,
        cell: CellSize,
    ) -> f32 {
        if self.tab_reserve().right_cols > 0 {
            self.rail_origin_px(cell)[0]
        } else {
            content.x + content.w + gap_right
        }
    }

    /// Whether the pointer sits over the workspace-rail column band this frame
    /// (its full height, incl. the corner over the top bar). Used to route an
    /// empty-rail right-click to `WorkspaceRailEmpty` rather than the top-bar
    /// empty menu, and by [`Self::pointer_in_tab_chrome_band`]. Under auto-hide
    /// only the revealed floating band counts.
    pub(super) fn pointer_in_workspace_rail_band(&self) -> bool {
        if !self.should_show_workspace_rail() {
            return false;
        }
        let Some((x_px, _y_px)) = self.window_pointer_px else {
            return false;
        };
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        if self.rail_autohide_active() {
            return match self.rail_autohide_side() {
                Some(side) => {
                    self.rail_overlay_visible() && self.pointer_in_reveal_band(x_px, cell, side)
                }
                None => false,
            };
        }
        let Some((w, h, padding)) = self.resolved_surface() else {
            return false;
        };
        let content = pane_content_rect(w, h, cell, padding, self.tab_reserve());
        let gap = self.tab_reserve().chrome_gap(padding);
        // CHROME-GAP: the band ends at its DRAWN content-facing edge, a gap
        // short of the content rect — the neutral strip between them is not
        // rail chrome (it routes to content, like left-click already does).
        // The Right arm binds at the band's painted origin (grid basis) via
        // the shared boundary helper, not at the content rect's pixel edge —
        // see `chrome_right_hit_boundary_px` for the sub-cell remainder this
        // avoids annexing from the drawn band.
        match self.workspace_rail_side() {
            RailSide::Left => (x_px as f32) < content.x - gap.left,
            RailSide::Right => {
                (x_px as f32) >= self.chrome_right_hit_boundary_px(content, gap.right, cell)
            }
        }
    }

    /// The empty-chrome context-menu surface for the current pointer position:
    /// `WorkspaceRailEmpty` over the rail band, `TabStripEmpty` over the top
    /// bar band (including the joined-band junction strip its background paints
    /// up to a pinned rail), or `None` over content — which includes the
    /// padding-wide neutral gap strips between content and the chrome bands,
    /// so right-click routing there matches left-click and the neutral render.
    pub(super) fn empty_chrome_menu_surface(&self) -> Option<ContextMenuSurface> {
        if !self.pointer_in_tab_chrome_band() {
            return None;
        }
        Some(if self.pointer_in_workspace_rail_band() {
            ContextMenuSurface::WorkspaceRailEmpty
        } else {
            ContextMenuSurface::TabStripEmpty
        })
    }

    /// Which side the rail occupies this frame, or `None` when no rail is active
    /// (top bar or hidden).
    pub(super) fn rail_side(&self) -> Option<RailSide> {
        let r = self.tab_reserve();
        if r.left_cols > 0 {
            Some(RailSide::Left)
        } else if r.right_cols > 0 {
            Some(RailSide::Right)
        } else {
            None
        }
    }

    /// Physical-pixel top-left of the top tab-bar strip: the window padding,
    /// shifted right past a left workspace rail. A
    /// right rail / no rail leaves it at `[pad, pad]`, byte-identical to the
    /// top-only strip.
    ///
    /// CHROME-GAP: past a pinned LEFT rail the bar shifts by the rail band PLUS
    /// the chrome-facing gap, keeping the bar's columns pixel-aligned with the
    /// gap-inset content columns below it (one uniform column basis for render,
    /// hit-testing, and the composited decorated snapshot).
    pub(super) fn top_bar_origin_px(&self, cell: CellSize) -> [f32; 2] {
        let padding = self
            .resolved_surface()
            .map(|(_, _, padding)| padding)
            .unwrap_or(WindowPadding::ZERO);
        let pad = padding.as_f32();
        let reserve = self.tab_reserve();
        let left_off = reserve.left_reserved_cols() as f32 * cell.width as f32;
        [pad + left_off + reserve.chrome_gap(padding).left, pad]
    }

    /// The physical-pixel top-left of the rail band this frame — the origin the
    /// rail widget's hit-test maps against and the multi-pane strip renders from.
    /// A left rail (and the byte-identical no-rail case) sits at the window
    /// padding `[pad, pad]`; a right rail sits at the far side, after the content
    /// columns: `pad + content_cols·cell_w`. This
    /// is the same grid basis the reserve/decorate/panel-seam paths use, so the
    /// rail's glyphs, seam, and click targets stay pixel-aligned (F4-P2).
    pub(super) fn rail_origin_px(&self, cell: CellSize) -> [f32; 2] {
        let padding = self
            .resolved_surface()
            .map(|(_, _, padding)| padding)
            .unwrap_or(WindowPadding::ZERO);
        let pad = padding.as_f32();
        // CHROME-GAP: a RIGHT rail band sits past the content columns AND the
        // chrome-facing padding gap (the gap opens between content and band; a
        // left rail stays at the padded window edge and the CONTENT shifts
        // instead). Zero gap keeps the historical origin exactly.
        let x = match self.rail_side() {
            Some(RailSide::Right) => {
                pad + self.grid.columns as f32 * cell.width as f32
                    + self.tab_reserve().chrome_gap(padding).right
            }
            _ => pad,
        };
        [x, pad]
    }

    /// The physical-pixel X of the rail's inner (content-facing) seam this frame,
    /// or `None` when no rail is active (F4-P4). A left rail's seam is the RIGHT
    /// edge of its band (`origin_x + rail_cols·cell_w`); a right rail's seam is
    /// the LEFT edge of its band (`origin_x`). This is the edge the drag-resize
    /// grabs and the resize cursor tracks.
    pub(super) fn rail_seam_x_px(&self, cell: CellSize) -> Option<f32> {
        let side = self.effective_rail_seam_side()?;
        if self.rail_autohide_active() {
            return Some(self.rail_overlay_seam_x(cell, side));
        }
        let origin_x = self.rail_origin_px(cell)[0];
        match side {
            RailSide::Left => Some(origin_x + self.rail_cols() as f32 * cell.width as f32),
            RailSide::Right => Some(origin_x),
        }
    }

    /// The manual rail width (cells) a seam-drag pointer at `px_x` maps to
    /// (F4-P4). Gathers the pixel geometry (padding, surface width) from the
    /// resolved live or injected surface — 0 defaults keep the left rail (which
    /// needs neither) usable before either exists — and defers the snap/clamp math to
    /// [`rail_width_cols_from_pointer`].
    pub(super) fn rail_width_from_pointer(&self, px_x: f64, cell: CellSize) -> Option<u16> {
        let side = self.effective_rail_seam_side()?;
        let (surface_w, pad) = self
            .resolved_surface()
            .map(|(width, _height, padding)| (width as f32, padding.as_f32()))
            .unwrap_or((0.0, 0.0));
        Some(rail_width_cols_from_pointer(
            side,
            px_x as f32,
            pad,
            cell.width as f32,
            surface_w,
            MIN_TAB_RAIL_WIDTH as u16,
            MAX_TAB_RAIL_WIDTH as u16,
        ))
    }

    /// Whether the pointer at raw `px_x` is within the seam grab band this frame
    /// and should start / show a rail resize rather than a tab hit (F4-P4).
    /// Yields to a live scroll thumb (ODP-5 right-rail rule) so a scrollbar drag
    /// wins the shared edge. `false` off a rail, so the plain path never grabs.
    pub(super) fn pointer_over_rail_seam(&self, px_x: f64, cell: CellSize) -> bool {
        if (!self.rail_autohide_active() && !self.should_show_tab_bar())
            || self.effective_rail_seam_side().is_none()
        {
            return false;
        }
        let Some(seam_x) = self.rail_seam_x_px(cell) else {
            return false;
        };
        if (px_x as f32 - seam_x).abs() > DIVIDER_GRAB_PX {
            return false;
        }
        // Yield the shared edge to a grabbable scroll thumb (right rail: the
        // content scrollbar sits just inside the seam).
        !(self.settings.scrollbar_drag && self.scrollbar_hit_test().is_some())
    }

    /// F4-P4: drive an in-progress rail seam drag to the pointer — set the manual
    /// width the pointer maps to and reflow the content grid. Resets the seam
    /// click tracker on an actual move so a drag-then-grab is never misread as a
    /// double-click (reset-to-auto).
    pub(super) fn drag_rail_seam_to_pointer(&mut self, px_x: f64) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let Some(cols) = self.rail_width_from_pointer(px_x, cell) else {
            return;
        };
        let next = crate::settings::TabRailWidth::Manual(cols);
        if self.settings.tab_rail_width != next {
            self.settings.tab_rail_width = next;
            self.rail_seam_clicks = ClickTracker::default();
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// F4-P4: set the rail width mode and persist it to `odytty.conf` (drag
    /// release → the dragged `Manual` width; double-click → `Auto`). The live
    /// setting is already applied, so this only writes it through so it survives
    /// a restart; a missing config path or write error is logged, not fatal.
    pub(super) fn persist_rail_width(&mut self) {
        let value = self.settings.tab_rail_width.as_config_string();
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        let changes = [SettingEdit {
            key: "tab_rail_width",
            env: TAB_RAIL_WIDTH_ENV,
            value,
        }];
        if let Err(error) = write_settings_changes_to_path(path, &changes) {
            tracing::warn!(error = %error, "could not persist tab rail width");
        }
    }

    /// F4-P4: reset the rail to `Auto` width (double-click the seam), reflow, and
    /// persist. A no-op when already `Auto`.
    pub(super) fn reset_rail_width_to_auto(&mut self) {
        if self.settings.tab_rail_width == crate::settings::TabRailWidth::Auto {
            return;
        }
        self.settings.tab_rail_width = crate::settings::TabRailWidth::Auto;
        self.recompute_grid_for_tab_bar();
        self.persist_rail_width();
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    // --- adjustable top tab-bar height (draggable bottom seam) ---------------

    /// The resolved top tab-bar height in text rows this frame. One row on the
    /// classic (`Auto`) path; a `Manual` height clamps to the widget bounds. The
    /// single source every tab-bar consumer reads (reservation, snapshot sizing,
    /// hit-test Y band, panel wash), so they never drift.
    pub(super) fn tab_bar_rows(&self) -> usize {
        self.settings.tab_bar_height.resolved_rows()
    }

    /// Physical-pixel Y of the top tab bar's bottom seam this frame — the band's
    /// top (`pad`) plus its resolved height — or `None` when the top bar is not
    /// shown. This is the horizontal edge the height drag grabs.
    pub(super) fn tab_bar_seam_y_px(&self, cell: CellSize) -> Option<f32> {
        if !self.should_show_tab_bar() {
            return None;
        }
        let top = self.top_bar_origin_px(cell)[1];
        Some(top + self.tab_bar_rows() as f32 * cell.height as f32)
    }

    /// Whether the pointer at raw `(px_x, px_y)` is within the tab-bar bottom
    /// seam grab band this frame and should start / show a height resize rather
    /// than a tab hit. The horizontal bounds are the exact drawn panel span, so
    /// a rail-junction segment cannot be visible without owning RowResize.
    /// `false` when no top bar is shown, so the plain path never grabs.
    pub(super) fn pointer_over_tab_bar_seam(&self, px_x: f64, px_y: f64, cell: CellSize) -> bool {
        let Some(seam_y) = self.tab_bar_seam_y_px(cell) else {
            return false;
        };
        if (px_y as f32 - seam_y).abs() > DIVIDER_GRAB_PX {
            return false;
        }
        let x = px_x as f32;
        if let Some((surface_w, _, padding)) = self.resolved_surface() {
            return tab_panel::top_span_contains_x(
                self.top_panel_span(cell, surface_w as f32, padding),
                surface_w as f32,
                x,
            );
        }
        // Pre-GPU/headless fallback retains the historical strip basis.
        let origin_x = self.top_bar_origin_px(cell)[0];
        let width = self.tab_bar_grid_cols() as f32 * cell.width as f32;
        x >= origin_x && x < origin_x + width
    }

    /// The manual bar height (rows) a seam-drag pointer at `px_y` maps to.
    /// Gathers the window padding (0 default keeps it usable headlessly for
    /// tests) and defers the pure snap/clamp math to [`tab_bar_rows_from_pointer`].
    pub(super) fn tab_bar_height_from_pointer(&self, px_y: f64, cell: CellSize) -> u16 {
        let pad = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO)
            .as_f32();
        tab_bar_rows_from_pointer(
            px_y as f32,
            pad,
            cell.height as f32,
            MIN_TAB_BAR_ROWS as u16,
            MAX_TAB_BAR_ROWS as u16,
        )
    }

    /// Drive an in-progress tab-bar height drag to the pointer — set the manual
    /// height the pointer maps to and reflow the content grid. Resets the seam
    /// click tracker on an actual move so a drag-then-grab is never misread as a
    /// double-click (reset-to-auto).
    pub(super) fn drag_tab_bar_seam_to_pointer(&mut self, px_y: f64) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let rows = self.tab_bar_height_from_pointer(px_y, cell);
        let next = TabBarHeight::Manual(rows);
        if self.settings.tab_bar_height != next {
            self.settings.tab_bar_height = next;
            self.overlay
                .rebase_settings_panel_onto_external(&self.settings);
            self.tab_bar_seam_clicks = ClickTracker::default();
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Set the tab-bar height mode and persist it to `odytty.conf` (drag release
    /// -> the dragged `Manual` height; double-click -> `Auto`). The live setting
    /// is already applied, so this only writes it through so it survives a
    /// restart; a missing config path or write error is logged, not fatal.
    pub(super) fn persist_tab_bar_height(&mut self) {
        let value = self.settings.tab_bar_height.as_config_string();
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        let changes = [SettingEdit {
            key: "tab_bar_height",
            env: TAB_BAR_HEIGHT_ENV,
            value,
        }];
        if let Err(error) = write_settings_changes_to_path(path, &changes) {
            tracing::warn!(error = %error, "could not persist tab bar height");
        }
    }

    /// Reset the tab bar to `Auto` height (double-click the seam), reflow, and
    /// persist. A no-op when already `Auto`.
    pub(super) fn reset_tab_bar_height_to_auto(&mut self) {
        if self.settings.tab_bar_height == TabBarHeight::Auto {
            return;
        }
        self.settings.tab_bar_height = TabBarHeight::Auto;
        self.overlay
            .rebase_settings_panel_onto_external(&self.settings);
        self.recompute_grid_for_tab_bar();
        self.persist_tab_bar_height();
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// The live display scale factor (physical px per logical px), or a headless
    /// test override, defaulting to 1.0 before the GPU/window exists. Used to
    /// convert logical-px pointer thresholds into the physical-px space winit's
    /// `CursorMoved` reports in.
    pub(super) fn effective_scale(&self) -> f32 {
        #[cfg(test)]
        if let Some(scale) = self.test_scale {
            return scale;
        }
        self.gpu.as_ref().map(GpuState::scale).unwrap_or(1.0)
    }

    /// Pixels to subtract from a raw pointer `(x, y)` before mapping it to a grid
    /// cell, accounting for tab chrome. Top bar → `(0, tab_h + gap)`; left rail →
    /// `(rail_w + gap, 0)`; right rail / none → `(0, 0)` (content origin
    /// unmoved). This is the single placement-aware pointer transform every
    /// single-pane hit path applies; on the top path `left_reserved_cols() == 0`
    /// so it is byte-identical. The content pointer stays registered with the
    /// shifted content origin.
    ///
    /// CHROME-GAP: each shown band's offset includes the chrome-facing padding
    /// gap (`TabReserve::chrome_gap`), so hit-testing, selection, drag
    /// autoscroll, SGR-pixel mouse reports, and every overlay painter shifted by
    /// this transform stay registered with the gap-inset content origin. Zero
    /// gap (no band / zero padding) keeps the historical values exactly.
    pub(super) fn tab_chrome_offset_px(&self, cell: CellSize) -> (f64, f64) {
        let r = self.tab_reserve();
        let padding = self
            .resolved_surface()
            .map(|(_, _, padding)| padding)
            .unwrap_or(WindowPadding::ZERO);
        let gap = r.chrome_gap(padding);
        (
            cell.width as f64 * r.left_reserved_cols() as f64 + f64::from(gap.left),
            cell.height as f64 * r.top_rows as f64 + f64::from(gap.top),
        )
    }

    pub(super) fn recompute_grid_for_tab_bar(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let _ = self.resize_grid_with_padding(
            gpu.cell(),
            gpu.window_padding(),
            window.inner_size().width,
            window.inner_size().height,
        );
        // F4-P4: record the rail width now baked into the content reservation so
        // `reconcile_rail_auto_width` reflows exactly once when auto-sizing (or a
        // manual/max-width change) moves the band. 0 on the top-bar/hidden path.
        self.rail_reserved_cols = self.rail_cols();
    }

    /// Rows the single-pane graphics layer shifts down for the top tab bar
    /// (0 for a rail — a rail reserves columns, not rows).
    pub(super) fn tab_bar_row_offset(&self) -> usize {
        self.tab_reserve().top_rows
    }

    /// Columns the single-pane graphics layer shifts right for a left rail
    /// (0 for the top bar or a right rail — content origin unmoved).
    pub(super) fn tab_bar_col_offset(&self) -> usize {
        self.tab_reserve().left_reserved_cols()
    }
}

/// F4-P4: the manual rail width (cells) a seam-drag pointer at `px_x` maps to.
/// The rail's OUTER edge is pinned to the window edge it hugs (left rail → the
/// left padding; right rail → `surface_w − pad`), so the width is the cell-
/// snapped distance from that edge to the pointer, clamped to `[min, max]`.
/// Measuring from the pinned window edge avoids the circularity of the right
/// rail's inner seam depending on the very width being set. Pure so the drag
/// geometry is unit-tested without a GPU/window. Module-private (its `RailSide`
/// parameter is `crate::native::app`-scoped); the tab_rail unit tests reach it
/// as a descendant module.
pub(super) fn rail_width_cols_from_pointer(
    side: RailSide,
    px_x: f32,
    pad: f32,
    cell_w: f32,
    surface_w: f32,
    min: u16,
    max: u16,
) -> u16 {
    let cw = cell_w.max(1.0);
    let raw = match side {
        RailSide::Left => (px_x - pad) / cw,
        RailSide::Right => (surface_w - pad - px_x) / cw,
    };
    raw.round().clamp(min as f32, max as f32) as u16
}

/// The manual tab-bar height in rows a seam-drag pointer at physical `px_y` maps
/// to: the pointer distance below the bar top (`pad`) in cell-heights, snapped
/// and clamped to `[min, max]`. Pure snap/clamp math, unit-tested without a GPU.
pub(super) fn tab_bar_rows_from_pointer(
    px_y: f32,
    pad: f32,
    cell_h: f32,
    min: u16,
    max: u16,
) -> u16 {
    let ch = cell_h.max(1.0);
    let raw = (px_y - pad) / ch;
    raw.round().clamp(min as f32, max as f32) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source {
        titles: Vec<String>,
    }

    impl TabBarSource for Source {
        fn tab_count(&self) -> usize {
            self.titles.len()
        }

        fn tab_title(&self, idx: usize) -> &str {
            &self.titles[idx]
        }

        fn active_tab(&self) -> usize {
            0
        }
    }

    fn committed_order(count: usize, origin: usize, drop_idx: usize) -> Vec<usize> {
        let mut order: Vec<_> = (0..count).collect();
        let moved = order.remove(origin);
        let destination = if drop_idx > origin {
            drop_idx - 1
        } else {
            drop_idx
        }
        .min(order.len());
        order.insert(destination, moved);
        order
    }

    #[test]
    fn preview_gap_replacement_equals_commit_for_every_drag_position() {
        for count in 1..=7 {
            for origin in 0..count {
                for drop_idx in 0..=count {
                    let preview = preview_order(count, origin, drop_idx);
                    assert_eq!(
                        preview
                            .iter()
                            .filter(|slot| **slot == PreviewSlot::Gap)
                            .count(),
                        1
                    );
                    let applied: Vec<_> = preview
                        .iter()
                        .map(|slot| match slot {
                            PreviewSlot::Tab(idx) => *idx,
                            PreviewSlot::Gap => origin,
                        })
                        .collect();
                    assert_eq!(applied, committed_order(count, origin, drop_idx));
                }
            }
        }
    }

    #[test]
    fn preview_source_places_each_title_exactly_once() {
        let source = Source {
            titles: (0..6).map(|idx| format!("title-{idx}")).collect(),
        };
        for origin in 0..source.tab_count() {
            for drop_idx in 0..=source.tab_count() {
                let preview = PreviewSource::new(&source, origin, drop_idx);
                let titles: Vec<_> = (0..preview.tab_count())
                    .map(|idx| preview.tab_title(idx))
                    .collect();
                assert!(preview.gap_idx().is_some());
                for retained in 0..source.tab_count() {
                    assert_eq!(
                        titles
                            .iter()
                            .filter(|title| **title == source.tab_title(retained))
                            .count(),
                        1
                    );
                }
            }
        }
    }

    #[test]
    fn rendered_drag_preview_keeps_every_label_and_uses_no_indicator_glyph() {
        let source = Source {
            titles: vec!["A".into(), "B".into(), "C".into()],
        };
        let preview = PreviewSource::new(&source, 0, 3);
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let output = tab_bar::TabBar::default().render(
            &preview,
            80,
            0.0,
            cell,
            WindowPadding::ZERO,
            tab_bar::TabBarColors {
                foreground: (240, 240, 240),
                background: (10, 10, 10),
                inactive: (120, 120, 120),
                active_bg: (40, 60, 80),
            },
            0.0,
        );
        assert_eq!(
            output.glyphs.iter().filter(|glyph| glyph.ch == 'A').count(),
            1
        );
        assert_eq!(
            output.glyphs.iter().filter(|glyph| glyph.ch == 'B').count(),
            1
        );
        assert_eq!(
            output.glyphs.iter().filter(|glyph| glyph.ch == 'C').count(),
            1
        );
        assert!(
            output
                .glyphs
                .iter()
                .all(|glyph| !matches!(glyph.ch, '\u{2501}' | '\u{2503}')),
            "drag feedback never overwrites cells with indicator glyphs"
        );
    }
}

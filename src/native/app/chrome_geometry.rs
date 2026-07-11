// SPDX-License-Identifier: GPL-3.0-only
//! Shared window-pixel geometry for top-tab and workspace-rail chrome.

use super::tab_bar::{self, TabBarSource, TabHit};
use super::tab_rail;
use super::*;

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
            cell,
        }
    }

    pub(super) fn hit(&self, point: PxPoint) -> TabHit {
        if !self.band.contains(point) {
            return TabHit::None;
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

    #[allow(dead_code)] // consumed by the quad indicator in Phase A commit 4
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

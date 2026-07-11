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

    pub(super) fn active_marker(&self, active_idx: usize, color: [f32; 4]) -> Option<SolidQuad> {
        let rect = self.slots.iter().find(|slot| slot.idx == active_idx)?.rect;
        let marker = match self.axis {
            Axis::Horizontal => {
                let bottom = rect.y + rect.height;
                [
                    rect.x as f32,
                    (bottom - 4.0).max(rect.y) as f32,
                    (rect.x + rect.width) as f32,
                    bottom as f32,
                ]
            }
            Axis::Vertical => [
                rect.x as f32,
                rect.y as f32,
                (rect.x + 3.0_f64.min(rect.width)) as f32,
                (rect.y + rect.height) as f32,
            ],
        };
        Some(SolidQuad {
            rect: marker,
            color,
        })
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
    fn active_marker_is_present_at_every_supported_chrome_height() {
        let source = Source {
            titles: vec!["active".into(), "other".into()],
        };
        let cell = CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        };
        let color = [0.2, 0.4, 0.6, 1.0];
        for rows in 1..=5 {
            let geometry = ChromeSlotGeom::top(&source, 80, rows, [4.0, 6.0], cell);
            let slot = geometry.slots.iter().find(|slot| slot.idx == 0).unwrap();
            let marker = geometry.active_marker(0, color).expect("top marker");
            assert_eq!(
                marker.rect[1],
                (slot.rect.y + slot.rect.height - 4.0).max(slot.rect.y) as f32
            );
            assert_eq!(marker.rect[3], (slot.rect.y + slot.rect.height) as f32);
            assert_eq!(marker.rect[0], slot.rect.x as f32);
            assert_eq!(marker.rect[2], (slot.rect.x + slot.rect.width) as f32);
            assert_eq!(marker.color, color);
        }
        for slot_rows in [1, 2] {
            let geometry = ChromeSlotGeom::rail(
                &source,
                16,
                20,
                [4.0, 6.0],
                cell,
                tab_rail::RailGeom {
                    slot_rows,
                    slot_gap: 0,
                },
            );
            let slot = geometry.slots.iter().find(|slot| slot.idx == 0).unwrap();
            let marker = geometry.active_marker(0, color).expect("rail marker");
            assert_eq!(marker.rect[0], slot.rect.x as f32);
            assert_eq!(marker.rect[2], (slot.rect.x + 3.0) as f32);
            assert_eq!(marker.rect[1], slot.rect.y as f32);
            assert_eq!(marker.rect[3], (slot.rect.y + slot.rect.height) as f32);
            assert_eq!(marker.color, color);
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

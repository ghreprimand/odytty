// SPDX-License-Identifier: GPL-3.0-only
//! Stacked and floating tab arrangements over the session model.
//!
//! The split tree on each [`Tab`] stays the one source of truth for which panes
//! exist and their stable order; the [`Arrangement`] only decides geometry.
//! Switching a tab between tiled, stacked, and floating therefore never
//! restarts a pane, and switching back to tiled restores the tiled geometry
//! exactly. Pure model code: nothing here talks to a backend or the GPU.

use super::model::{SessionToken, Tab, WorkspaceSet};
use crate::native::float_layout::{
    Arrangement, FloatLayout, content_grid, floating_inner_rect, floating_pixel_rects,
};
use crate::native::layout::{PaneRect, layout_rects, pane_inner_rect};

/// What an arrangement change did, for the caller's reflow and notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum ArrangeOutcome {
    /// The arrangement changed; geometry must be reflowed.
    Changed,
    /// The tab already had that arrangement.
    Unchanged,
    /// The tab has a single pane, where an arrangement is meaningless.
    NeedsSplit,
}

/// One whole-cell move or resize step of the focused floating pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum FloatStep {
    Move { d_col: isize, d_row: isize },
    Resize { d_cols: isize, d_rows: isize },
}

impl Tab {
    /// True when this tab renders as exactly one full-bleed pane because it is
    /// zoomed or stacked: the focused pane fills the content rectangle and the
    /// other panes stay alive behind it. Stacked is meaningless on a single
    /// pane, so it needs two or more.
    pub(super) fn shows_only_focused_pane(&self) -> bool {
        self.is_effectively_zoomed()
            || (self.arrangement.is_stacked()
                && !self.layout.is_single_pane()
                && self.layout.contains(self.focused))
    }

    /// True when this tab places its panes as floating rectangles.
    pub(in crate::native) fn is_floating(&self) -> bool {
        self.arrangement.is_floating() && !self.layout.is_single_pane()
    }

    /// Every pane's outer pixel rectangle in paint order (back to front; the
    /// last entry is topmost). Zoomed and stacked tabs yield only the focused
    /// pane over the whole content rectangle.
    pub(super) fn pane_rects(
        &self,
        content: PaneRect,
        divider_px: f32,
        cell: (u32, u32),
    ) -> Vec<(SessionToken, PaneRect)> {
        if self.shows_only_focused_pane() {
            return vec![(self.focused, content)];
        }
        if let Arrangement::Floating(layout) = &self.arrangement
            && !self.layout.is_single_pane()
        {
            return floating_pixel_rects(
                layout,
                &self.layout.leaves(),
                self.focused,
                content,
                cell.0,
                cell.1,
            );
        }
        layout_rects(&self.layout, content, divider_px)
    }

    /// The drawable rectangle inside `rect`: floating panes inset by `pad` on
    /// every side, tiled panes on their divider-facing edges only.
    pub(super) fn inner_rect(&self, rect: PaneRect, content: PaneRect, pad: f32) -> PaneRect {
        if self.is_floating() && !self.shows_only_focused_pane() {
            floating_inner_rect(rect, pad)
        } else {
            pane_inner_rect(rect, content, pad)
        }
    }

    /// Raise the focused pane to the front of a floating tab's z-order. A no-op
    /// for every other arrangement.
    pub(super) fn raise_focused(&mut self) {
        let focused = self.focused;
        let leaves = self.layout.leaves();
        if let Arrangement::Floating(layout) = &mut self.arrangement {
            layout.normalize(&leaves);
            layout.raise(focused);
        }
    }
}

impl WorkspaceSet {
    /// Whether the active tab shows only its focused pane (zoomed or stacked).
    pub(in crate::native) fn active_shows_only_focused(&self) -> bool {
        self.active_tab_ref()
            .is_some_and(Tab::shows_only_focused_pane)
    }

    /// The creation identity of the active tab, stable across splits, closes,
    /// and moves between windows.
    pub(in crate::native) fn active_tab_identity(&self) -> Option<SessionToken> {
        self.active_tab_ref().map(|tab| tab.identity)
    }

    /// Whether the active tab places its panes as floating rectangles.
    pub(in crate::native) fn active_is_floating(&self) -> bool {
        self.active_tab_ref().is_some_and(Tab::is_floating)
    }

    /// Whether the active tab is tiled (the default arrangement).
    pub(in crate::native) fn active_arrangement_is_tiled(&self) -> bool {
        self.active_tab_ref()
            .is_none_or(|tab| tab.arrangement.is_tiled())
    }

    /// The panes of the active tab in their stable accessible order (tree
    /// order), each flagged when focused. This is the order focus cycling and
    /// the palette use, whatever the z-order.
    pub(in crate::native) fn active_pane_order(&self) -> Vec<(SessionToken, bool)> {
        self.active_tab_ref()
            .map(|tab| {
                tab.layout
                    .leaves()
                    .into_iter()
                    .map(|token| (token, token == tab.focused))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Switch the active tab back to the tiled split tree. The tree was never
    /// discarded, so the prior tiled geometry returns exactly.
    pub(in crate::native) fn set_active_tiled(&mut self) -> ArrangeOutcome {
        let Some(tab) = self.active_tab_mut() else {
            return ArrangeOutcome::Unchanged;
        };
        if tab.arrangement.is_tiled() {
            return ArrangeOutcome::Unchanged;
        }
        tab.arrangement = Arrangement::Tiled;
        ArrangeOutcome::Changed
    }

    /// Stack the active tab: the focused pane fills the content rectangle and
    /// the others stay alive behind it.
    pub(in crate::native) fn set_active_stacked(&mut self) -> ArrangeOutcome {
        let Some(tab) = self.active_tab_mut() else {
            return ArrangeOutcome::Unchanged;
        };
        if tab.layout.is_single_pane() {
            return ArrangeOutcome::NeedsSplit;
        }
        if tab.arrangement.is_stacked() {
            return ArrangeOutcome::Unchanged;
        }
        tab.arrangement = Arrangement::Stacked;
        // Stacked is the persistent form of "one pane visible"; a pending zoom
        // would only hide that the tab is now stacked.
        tab.zoomed = false;
        ArrangeOutcome::Changed
    }

    /// Float the active tab. The rectangles start as the current tiled tiles
    /// snapped to whole cells, so the panes do not move at the moment of
    /// switching. Switching from stacked reuses the tiled tree's geometry too.
    pub(in crate::native) fn set_active_floating(
        &mut self,
        content: PaneRect,
        divider_px: f32,
        cell: (u32, u32),
    ) -> ArrangeOutcome {
        let Some(tab) = self.active_tab_mut() else {
            return ArrangeOutcome::Unchanged;
        };
        if tab.layout.is_single_pane() {
            return ArrangeOutcome::NeedsSplit;
        }
        if tab.arrangement.is_floating() {
            return ArrangeOutcome::Unchanged;
        }
        let tiles = layout_rects(&tab.layout, content, divider_px);
        let mut layout = FloatLayout::from_tiled(&tiles, content, cell.0, cell.1);
        layout.normalize(&tab.layout.leaves());
        layout.raise(tab.focused);
        tab.arrangement = Arrangement::Floating(layout);
        tab.zoomed = false;
        ArrangeOutcome::Changed
    }

    /// Move or resize the focused pane of a floating active tab by whole
    /// cells. Returns whether the rectangle changed.
    pub(in crate::native) fn step_active_floating(
        &mut self,
        step: FloatStep,
        content: PaneRect,
        cell: (u32, u32),
    ) -> bool {
        let grid = content_grid(content, cell.0, cell.1);
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        let focused = tab.focused;
        let leaves = tab.layout.leaves();
        let Arrangement::Floating(layout) = &mut tab.arrangement else {
            return false;
        };
        match step {
            FloatStep::Move { d_col, d_row } => {
                layout.move_by(&leaves, focused, focused, d_col, d_row, grid)
            }
            FloatStep::Resize { d_cols, d_rows } => {
                layout.resize_by(&leaves, focused, focused, d_cols, d_rows, grid)
            }
        }
    }
}

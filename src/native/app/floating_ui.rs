// SPDX-License-Identifier: GPL-3.0-only
//! Stacked and floating pane layouts: the palette actions, the keyboard
//! arrange mode, the frames around floating panes, and the mode label.
//!
//! A tab keeps its split tree whatever its arrangement, so choosing Stack Panes,
//! Float Panes, or Tile Panes only changes geometry: every shell keeps running
//! and Tile Panes restores the tiled geometry exactly. The layouts are
//! in-window rectangles inside the content grid, never native windows, so they
//! behave the same on Wayland, X11, macOS, and Windows.
//!
//! Keyboard reach: pane focus cycles in the stable tree order (the existing
//! Focus Next Pane action and the arrange mode's Tab), so every pane,
//! including a buried one, is reachable without a pointer. Moving and resizing
//! a floating pane is a keyboard modal armed from the palette: bare arrows
//! reach the shell until Arrange Floating Pane is chosen, and Escape or Enter
//! leaves the mode. The mode is announced by a label on the focused pane.
//!
//! Pointer drag of a floating frame is not implemented: the divider gesture
//! code has no hit-test for panes that overlap, and a second pointer path would
//! bypass its focus-loss cancel. Keyboard move and resize are the supported
//! route.

use super::*;
use crate::core::{Attrs, Cell};
use crate::native::float_layout::border_strips;
use crate::native::session::{ArrangeOutcome, FloatStep};

/// The arrangements a tab can be switched between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum PaneLayoutMode {
    Tiled,
    Stacked,
    Floating,
}

/// Notice shown when a layout is chosen on a tab with a single pane.
pub(in crate::native) const LAYOUT_NEEDS_SPLIT_NOTICE: &str =
    "Stacked and floating layouts need two or more panes in the tab";

/// Notice shown when the arrange mode is chosen outside a floating tab.
pub(in crate::native) const ARRANGE_NEEDS_FLOAT_NOTICE: &str =
    "Arranging needs a floating layout: choose Float Panes first";

/// Label painted on the focused pane while the arrange mode owns the keyboard.
pub(in crate::native) const ARRANGE_LABEL: &str =
    " ARRANGE  arrows move  Shift+arrows resize  Tab next pane  Esc done ";
const ARRANGE_LABEL_SHORT: &str = " ARRANGE  Esc done ";
const ARRANGE_LABEL_TINY: &str = " ARRANGE ";

impl App {
    /// Frame quads for every floating pane: a 1px line inside each outer
    /// rectangle, the focused pane's in the theme's cursor color, with the parts
    /// a pane in front covers cut away.
    pub(super) fn floating_border_quads(
        &self,
        rects: &[(SessionToken, PaneRect)],
        focused: SessionToken,
    ) -> Vec<SolidQuad> {
        let linear = |(r, g, b): (u8, u8, u8)| {
            let mut color = text::foreground_linear(crate::core::Color::Rgb(r, g, b));
            color[3] = 1.0;
            color
        };
        let plain = linear(self.chrome_theme.border);
        let focus = linear(self.chrome_theme.cursor);
        border_strips(rects, PANE_DIVIDER_PX)
            .into_iter()
            .map(|(index, rect)| SolidQuad {
                rect,
                color: if rects[index].0 == focused {
                    focus
                } else {
                    plain
                },
            })
            .collect()
    }

    /// Switch the active tab to `mode`. A single-pane tab (where no geometry is
    /// resolved) gets a notice instead; an unchanged mode is silent.
    pub(super) fn set_pane_layout(&mut self, mode: PaneLayoutMode) {
        self.finish_divider_drag();
        let geometry = self.multipane_geometry();
        let outcome =
            match (mode, geometry) {
                (PaneLayoutMode::Tiled, _) => self.sessions.set_active_tiled(),
                (_, None) => ArrangeOutcome::NeedsSplit,
                (PaneLayoutMode::Stacked, Some(_)) => self.sessions.set_active_stacked(),
                (PaneLayoutMode::Floating, Some((content, cell))) => self
                    .sessions
                    .set_active_floating(content, PANE_DIVIDER_PX, (cell.width, cell.height)),
            };
        match outcome {
            ArrangeOutcome::NeedsSplit => {
                self.raise_neutral_notice(LAYOUT_NEEDS_SPLIT_NOTICE.to_owned());
            }
            ArrangeOutcome::Unchanged => {}
            ArrangeOutcome::Changed => {
                if mode != PaneLayoutMode::Floating {
                    self.float_arrange = None;
                }
                self.reflow_active_panes_and_redraw();
                self.sessions.active_mut().needs_rebuild = true;
                self.request_selection_redraw();
            }
        }
    }

    /// Whether the arrange mode currently owns the keyboard: armed, and the
    /// active tab is still a floating tab with two or more panes.
    pub(super) fn float_arrange_active(&self) -> bool {
        self.float_arrange.is_some()
            && self.float_arrange == self.sessions.active_tab_identity()
            && self.sessions.active_is_floating()
    }

    /// Arm the keyboard arrange mode on a floating tab.
    pub(super) fn enter_float_arrange(&mut self) {
        if !self.sessions.active_is_floating() {
            self.raise_neutral_notice(ARRANGE_NEEDS_FLOAT_NOTICE.to_owned());
            return;
        }
        self.float_arrange = self.sessions.active_tab_identity();
        self.sessions.active_mut().needs_rebuild = true;
        self.request_selection_redraw();
    }

    fn exit_float_arrange(&mut self) {
        if self.float_arrange.take().is_some() {
            self.sessions.active_mut().needs_rebuild = true;
            self.request_selection_redraw();
        }
    }

    /// Handle a key while the arrange mode is armed. Every key is consumed: a
    /// bound key acts, anything else is swallowed and never reaches the shell.
    pub(super) fn float_arrange_key(&mut self, key: &WinitKey) {
        if !self.float_arrange_active() {
            // The tab stopped being floating (or lost its other panes) under
            // the mode; end it so keys return to the shell.
            self.exit_float_arrange();
            return;
        }
        let shift = self.modifiers.shift;
        let step = |d_col: isize, d_row: isize| {
            if shift {
                FloatStep::Resize {
                    d_cols: d_col,
                    d_rows: d_row,
                }
            } else {
                FloatStep::Move { d_col, d_row }
            }
        };
        let step = match key {
            WinitKey::Named(NamedKey::Escape | NamedKey::Enter) => {
                self.exit_float_arrange();
                return;
            }
            WinitKey::Named(NamedKey::Tab) => {
                self.apply_pane_action(BindableAction::FocusPaneNext);
                self.request_selection_redraw();
                return;
            }
            WinitKey::Named(NamedKey::ArrowLeft) => step(-1, 0),
            WinitKey::Named(NamedKey::ArrowRight) => step(1, 0),
            WinitKey::Named(NamedKey::ArrowUp) => step(0, -1),
            WinitKey::Named(NamedKey::ArrowDown) => step(0, 1),
            _ => return,
        };
        let Some((content, cell)) = self.multipane_geometry() else {
            return;
        };
        if self
            .sessions
            .step_active_floating(step, content, (cell.width, cell.height))
        {
            self.reflow_active_panes_and_redraw();
            self.sessions.active_mut().needs_rebuild = true;
            self.request_selection_redraw();
        }
    }

    /// Labels for the palette's "Focus Pane k of n" rows: one per pane in the
    /// stable tab order with the focused pane marked, and only for a stacked or
    /// floating tab (a tiled tab is reachable by direction and by pointer). The
    /// same order serves as the accessible list of panes for either layout.
    pub(in crate::native) fn pane_focus_row_labels(&self) -> Vec<String> {
        if self.sessions.active_arrangement_is_tiled() {
            return Vec::new();
        }
        let order = self.sessions.active_pane_order();
        if order.len() < 2 {
            return Vec::new();
        }
        let total = order.len();
        let marker = if self.sessions.active_is_floating() {
            "front"
        } else {
            "shown"
        };
        order
            .into_iter()
            .enumerate()
            .map(|(index, (_, focused))| {
                let number = index + 1;
                if focused {
                    format!("Focus Pane {number} of {total} ({marker})")
                } else {
                    format!("Focus Pane {number} of {total}")
                }
            })
            .collect()
    }

    /// Focus pane `token` from a palette row. A pane that has closed, or that is
    /// not in the active tab any more, does nothing.
    pub(super) fn focus_pane_token(&mut self, token: SessionToken) {
        self.finish_divider_drag();
        if self.sessions.set_active_focus(token) {
            self.reflow_active_panes_and_redraw();
            self.sessions.active_mut().needs_rebuild = true;
            self.request_selection_redraw();
        }
    }

    /// Test seam: whether the arrange mode currently owns the keyboard.
    #[cfg(test)]
    pub(in crate::native) fn float_arrange_active_for_test(&self) -> bool {
        self.float_arrange_active()
    }
}

/// The arrange label a pane `columns` wide paints, or `None` when even the
/// shortest form does not fit.
pub(in crate::native) fn arrange_label_text(columns: usize) -> Option<&'static str> {
    [ARRANGE_LABEL, ARRANGE_LABEL_SHORT, ARRANGE_LABEL_TINY]
        .into_iter()
        .find(|text| text.len() < columns)
}

/// Paint the arrange label at the focused pane's top-left in inverse video, as
/// application chrome over the snapshot (never a grid mutation). A pane too
/// narrow for any form gets nothing, and `active == false` leaves the snapshot
/// untouched so the default frame is byte-identical.
pub(in crate::native) fn paint_arrange_label(snapshot: &mut Snapshot, active: bool) {
    if !active || snapshot.dimensions.rows == 0 {
        return;
    }
    let Some(text) = arrange_label_text(snapshot.dimensions.columns) else {
        return;
    };
    let mut attrs = Attrs::default();
    attrs.set_bold(true);
    attrs.set_inverse(true);
    let len = text.chars().count();
    // A wide glyph straddling the label's end would leave its spacer half
    // orphaned; blank it.
    if let Some(next) = snapshot.cells.get_mut(len)
        && next.wide_continuation
    {
        *next = Cell::new(' ', Attrs::default());
    }
    for (offset, ch) in text.chars().enumerate() {
        if let Some(cell) = snapshot.cells.get_mut(offset) {
            *cell = Cell::new(ch, attrs);
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::native::overlay::{OverlayInput, OverlayRect, PointerButton};
use crate::native::viewport::carry_add;
use winit::event::MouseScrollDelta;

/// Pixel travel per context-menu wheel row, in cell-heights. Same value as
/// the shared macOS overlay damper's detent, but a separate constant so a
/// damper retune cannot change this menu (and vice versa).
const WHEEL_ROW_CELLS: f64 = 3.0;

/// Which overflow mark a border press landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum OverflowArrow {
    /// The top-border mark shown while rows are hidden above the window.
    Up,
    /// The bottom-border mark shown while rows are hidden below the window.
    Down,
}

impl ContextMenuUi {
    pub(super) fn focus_prev(&mut self) {
        let n = self.item_count();
        self.focused = (self.focused + n - 1) % n;
    }

    pub(super) fn focus_next(&mut self) {
        let n = self.item_count();
        self.focused = (self.focused + 1) % n;
    }

    pub(super) fn activate_focused(&self) -> ContextMenuOutcome {
        let item = self.visible_items()[self.focused];
        if self.item_enabled(item) {
            ContextMenuOutcome::Activate(item)
        } else {
            // A disabled focused item swallows the activation (D-IN2-6).
            ContextMenuOutcome::Consumed
        }
    }

    /// Handle a keyboard event: Esc closes; Up/Down cycle focus with wrap
    /// (skipping the separator — focus cycles only through selectable items);
    /// Enter/Space activate the focused item; everything else is swallowed so
    /// nothing leaks to the PTY behind the menu (D-IN2-8).
    pub(in crate::native) fn handle_input(&mut self, input: OverlayInput) -> ContextMenuOutcome {
        match input {
            OverlayInput::Close => ContextMenuOutcome::Close,
            OverlayInput::Up => {
                self.focus_prev();
                ContextMenuOutcome::Consumed
            }
            OverlayInput::Down => {
                self.focus_next();
                ContextMenuOutcome::Consumed
            }
            OverlayInput::Activate | OverlayInput::Char(' ') => self.activate_focused(),
            _ => ContextMenuOutcome::Consumed,
        }
    }

    /// Handle a press on a body row (already resolved to a body-relative row by
    /// the overlay, i.e. relative to the *visible* window). `body_height` is the
    /// box-clamped visible row count, so the press is offset by the current
    /// [`Self::scroll_offset`] to reach the true body row. Activation happens on
    /// PRESS. A press past the visible window, on the separator row, or past the
    /// last body row is inert. The pressed item also takes focus. Disabled items
    /// swallow the press (D-IN2-6).
    pub(in crate::native) fn handle_press(
        &mut self,
        row_in_body: usize,
        body_height: usize,
        _button: PointerButton,
    ) -> ContextMenuOutcome {
        if row_in_body >= body_height {
            return ContextMenuOutcome::Consumed;
        }
        self.commit_scroll(body_height);
        let body_row = self.scroll_offset(body_height) + row_in_body;
        if body_row >= self.body_row_count() {
            return ContextMenuOutcome::Consumed;
        }
        let Some(item_index) = self.body_row_to_item_index(body_row) else {
            // Separator row: inert.
            return ContextMenuOutcome::Consumed;
        };
        self.focused = item_index;
        let item = self.visible_items()[item_index];
        if self.item_enabled(item) {
            ContextMenuOutcome::Activate(item)
        } else {
            ContextMenuOutcome::Consumed
        }
    }

    /// Clear the wheel remainder and the scroll anchor. Every open path calls
    /// this so a partial flick or a stale window never carries into a new menu.
    pub(super) fn reset_scroll_input(&mut self) {
        self.wheel_remainder = 0.0;
        self.scroll_anchor = 0;
    }

    /// Commit the currently displayed window as the scroll anchor. Called by
    /// every handler that knows `body_height` before it acts, so keyboard focus
    /// moves (which run without the height) start from the window on screen.
    pub(in crate::native) fn commit_scroll(&mut self, body_height: usize) {
        self.scroll_anchor = self.scroll_offset(body_height);
    }

    /// Scroll the visible window by `rows` (negative = toward earlier rows),
    /// clamped to `[0, max_scroll]`. Focus that falls outside the new window
    /// moves to the nearest visible selectable item. Returns the number of
    /// rows the window actually moved.
    pub(in crate::native) fn scroll_window(&mut self, rows: isize, body_height: usize) -> usize {
        self.commit_scroll(body_height);
        let max_scroll = self.body_row_count().saturating_sub(body_height);
        if body_height == 0 || max_scroll == 0 {
            return 0;
        }
        let from = self.scroll_anchor;
        let to = from.saturating_add_signed(rows).min(max_scroll);
        self.scroll_anchor = to;
        self.clamp_focus_into_window(body_height);
        from.abs_diff(to)
    }

    /// Move focus to the nearest selectable item inside the committed window,
    /// if it is outside. Separators are skipped; a window holding no item
    /// leaves focus unchanged.
    fn clamp_focus_into_window(&mut self, body_height: usize) {
        let top = self.scroll_anchor;
        let end = (top + body_height).min(self.body_row_count());
        let focused_row = self.focused_body_row();
        let target = if focused_row < top {
            (top..end).find_map(|row| self.body_row_to_item_index(row))
        } else if focused_row >= end {
            (top..end)
                .rev()
                .find_map(|row| self.body_row_to_item_index(row))
        } else {
            None
        };
        if let Some(item_index) = target {
            self.focused = item_index;
        }
    }

    /// Column of the overflow marks: horizontally centered on the menu border.
    /// Shared by the painter and the press hit-test so they cannot drift.
    pub(in crate::native) fn overflow_arrow_column(rect: &OverlayRect) -> usize {
        rect.left + rect.width / 2
    }

    /// Whether rows are hidden above / below the visible window, i.e. whether
    /// the top / bottom overflow mark is drawn.
    pub(in crate::native) fn overflow_marks(&self, body_height: usize) -> (bool, bool) {
        let scroll = self.scroll_offset(body_height);
        (scroll > 0, scroll + body_height < self.body_row_count())
    }

    /// The overflow mark under `cell`, if that mark is currently drawn. A
    /// border cell without a drawn mark returns `None` (inert).
    pub(in crate::native) fn overflow_arrow_at(
        &self,
        cell: CellPoint,
        rect: &OverlayRect,
    ) -> Option<OverflowArrow> {
        if cell.column != Self::overflow_arrow_column(rect) {
            return None;
        }
        let (above, below) = self.overflow_marks(rect.body_height);
        let bottom = rect.top + rect.height.saturating_sub(1);
        if above && cell.row == rect.top {
            Some(OverflowArrow::Up)
        } else if below && cell.row == bottom && bottom != rect.top {
            Some(OverflowArrow::Down)
        } else {
            None
        }
    }

    /// Handle a left press on an overflow mark: scroll the window one row
    /// toward the hidden rows; focus that leaves the window is clamped to the
    /// nearest visible item. Nothing activates. Returns `true` when the press
    /// hit a drawn mark; other buttons and unmarked cells are inert.
    pub(in crate::native) fn handle_arrow_press(
        &mut self,
        cell: CellPoint,
        rect: &OverlayRect,
        button: PointerButton,
    ) -> bool {
        if button != PointerButton::Left {
            return false;
        }
        let Some(arrow) = self.overflow_arrow_at(cell, rect) else {
            return false;
        };
        let rows = match arrow {
            OverflowArrow::Up => -1,
            OverflowArrow::Down => 1,
        };
        self.scroll_window(rows, rect.body_height);
        true
    }

    /// Feed a wheel delta to the menu; the wheel scrolls the WINDOW, not focus.
    /// A `LineDelta` notch scrolls exactly one row (never the x3 `wheel_lines`
    /// magnitude) and clears the pixel remainder. A `PixelDelta` accumulates
    /// into the remainder and scrolls one row per whole
    /// `WHEEL_ROW_CELLS * cell_height` pixels, keeping the leftover so a flick
    /// yields several rows and a slow tail still finishes the next one. A
    /// direction reversal drops the stale remainder. Positive `y` is wheel-up,
    /// toward earlier rows. Horizontal travel is ignored.
    ///
    /// After scrolling, focus follows the item under the pointer when
    /// `pointer_row` (body-relative, visible window) names one, as a native
    /// menu does; otherwise focus is clamped into the window. A later hover on
    /// the same cell therefore changes neither the window nor focus. Returns
    /// the number of rows the window moved.
    pub(in crate::native) fn handle_wheel(
        &mut self,
        delta: MouseScrollDelta,
        cell_height: u32,
        body_height: usize,
        pointer_row: Option<usize>,
    ) -> usize {
        let rows: f64 = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                if y == 0.0 {
                    return 0;
                }
                self.wheel_remainder = 0.0;
                f64::from(y.signum())
            }
            MouseScrollDelta::PixelDelta(pos) => {
                if pos.y == 0.0 {
                    return 0;
                }
                let threshold = f64::from(cell_height.max(1)) * WHEEL_ROW_CELLS;
                self.wheel_remainder = carry_add(self.wheel_remainder, pos.y);
                let whole = (self.wheel_remainder / threshold).trunc();
                self.wheel_remainder -= whole * threshold;
                whole
            }
        };
        // Bounded by the row count, so the cast cannot overflow.
        let magnitude = (rows.abs() as usize).min(self.body_row_count()) as isize;
        if magnitude == 0 {
            return 0;
        }
        let moved =
            self.scroll_window(if rows > 0.0 { -magnitude } else { magnitude }, body_height);
        self.handle_hover(pointer_row, body_height);
        moved
    }

    /// Move focus to the item under a hovering pointer (D-IN2-6). `row_in_body`
    /// is `None` when the pointer is on the border / off a body row, leaving
    /// focus unchanged. `body_height` is the box-clamped visible row count; the
    /// hovered row is offset by the current [`Self::scroll_offset`] to reach the
    /// true body row. A hover past the visible window or on a separator row is
    /// skipped (focus stays on its last position). The hovered item is always
    /// inside the window, so hover never moves the window.
    pub(in crate::native) fn handle_hover(
        &mut self,
        row_in_body: Option<usize>,
        body_height: usize,
    ) {
        self.commit_scroll(body_height);
        if let Some(row) = row_in_body
            && row < body_height
            && let Some(item_index) =
                self.body_row_to_item_index(self.scroll_offset(body_height) + row)
        {
            self.focused = item_index;
        }
    }
}

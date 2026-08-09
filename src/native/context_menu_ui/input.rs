// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::native::overlay::{OverlayInput, PointerButton};

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

    /// Move focus to the item under a hovering pointer (D-IN2-6). `row_in_body`
    /// is `None` when the pointer is on the border / off a body row, leaving
    /// focus unchanged. `body_height` is the box-clamped visible row count; the
    /// hovered row is offset by the current [`Self::scroll_offset`] to reach the
    /// true body row. A hover past the visible window or on a separator row is
    /// skipped (focus stays on its last position).
    pub(in crate::native) fn handle_hover(
        &mut self,
        row_in_body: Option<usize>,
        body_height: usize,
    ) {
        if let Some(row) = row_in_body
            && row < body_height
            && let Some(item_index) =
                self.body_row_to_item_index(self.scroll_offset(body_height) + row)
        {
            self.focused = item_index;
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Paragraph bidi properties on the screen: BDSM and SCP reception, the
//! stamping rules, and the read accessors. See [`crate::core::paragraph_bidi`].

use super::*;
use crate::core::paragraph_bidi::{ParagraphBidiChange, ParagraphDirection};

impl Screen {
    /// SCP, `CSI Ps1 ; Ps2 SP k`. `Ps1` 0 (the default) selects the terminal
    /// default direction, 1 left to right, 2 right to left; any other value
    /// is ignored. Only `Ps2` 0 is implemented: values 1 and 2 would update
    /// the model from the view, so the whole sequence is ignored with them.
    pub(super) fn select_character_path(&mut self, params: &Params) {
        if param_or(params, 1, 0) != 0 {
            return;
        }
        let direction = match param_or(params, 0, 0) {
            0 => ParagraphDirection::Default,
            1 => ParagraphDirection::LeftToRight,
            2 => ParagraphDirection::RightToLeft,
            _ => return,
        };
        self.receive_paragraph_bidi(ParagraphBidiChange::Direction(direction));
    }

    /// BDSM, ANSI mode 8: set (`CSI 8 h`, the default) is implicit mode,
    /// reset (`CSI 8 l`) is explicit mode.
    pub(super) fn set_bidi_implicit(&mut self, implicit: bool) {
        self.receive_paragraph_bidi(ParagraphBidiChange::Explicit(!implicit));
    }

    /// Update the current value, and with the cursor at the first position of
    /// its paragraph apply the changed property to that paragraph at once,
    /// even when the current value already had it.
    fn receive_paragraph_bidi(&mut self, change: ParagraphBidiChange) {
        self.bidi_current = change.apply(self.bidi_current);
        if self.cursor.column == 0
            && !self.pending_wrap
            && self.row_starts_paragraph(self.cursor.row)
        {
            self.update_paragraph_from(self.cursor.row, |value| change.apply(value));
        }
    }

    /// Whether `row` is the first row of its paragraph. Visible row 0 on the
    /// primary screen continues an open scrollback tail; the alternate grid's
    /// row 0 never continues anything.
    fn row_starts_paragraph(&self, row: usize) -> bool {
        match row.checked_sub(1) {
            Some(above) => !self.rows[above].wrapped,
            None => {
                self.primary_screen.is_some() || self.scrollback.trailing_paragraph_bidi().is_none()
            }
        }
    }

    /// Rewrite the value on `row` and every row its soft-wrap chain reaches.
    fn update_paragraph_from(
        &mut self,
        row: usize,
        update: impl Fn(ParagraphBidi) -> ParagraphBidi,
    ) {
        for line in self.rows.iter_mut().skip(row) {
            line.paragraph_bidi = update(line.paragraph_bidi);
            if !line.wrapped {
                break;
            }
        }
    }

    /// After a line feed moved or scrolled the cursor onto a row: a row the
    /// previous one wraps into joins that paragraph and takes its value (a
    /// join keeps the first paragraph's properties); otherwise the cursor
    /// entered a new paragraph, which takes the current value.
    pub(super) fn paragraph_bidi_after_line_feed(&mut self) {
        let row = self.cursor.row;
        let joined = match row.checked_sub(1) {
            Some(above) => {
                let line = &self.rows[above];
                line.wrapped.then_some(line.paragraph_bidi)
            }
            None if self.primary_screen.is_none() => self.scrollback.trailing_paragraph_bidi(),
            None => None,
        };
        let value = joined.unwrap_or(self.bidi_current);
        self.update_paragraph_from(row, |_| value);
    }

    /// A blank row created by an erase or scroll: a one-row paragraph with
    /// the current properties.
    pub(super) fn fresh_row(&self, background: Color) -> Line {
        let mut line = blank_row_with_bg(self.dimensions.columns, background);
        line.paragraph_bidi = self.bidi_current;
        line
    }

    /// RIS and DECSTR restore implicit mode and the default direction.
    pub(super) fn reset_paragraph_bidi(&mut self) {
        self.bidi_current = ParagraphBidi::DEFAULT;
    }

    /// ANSI DECRQM status for BDSM: 1 implicit (set), 2 explicit (reset).
    pub(super) fn bidi_mode_report(&self) -> u8 {
        if self.bidi_current.explicit() { 2 } else { 1 }
    }

    /// The current paragraph bidi properties, stamped onto new paragraphs.
    pub fn paragraph_bidi_current(&self) -> ParagraphBidi {
        self.bidi_current
    }

    /// The bidi properties of the paragraph that holds visible row `row` of
    /// the live grid; the default past the last row.
    pub fn paragraph_bidi_at(&self, row: usize) -> ParagraphBidi {
        self.rows
            .get(row)
            .map_or(ParagraphBidi::DEFAULT, |line| line.paragraph_bidi)
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Owner geometry for the rename / name prompt's editable field.
//!
//! The field stores its caret and selection anchor as scalar (char) indices,
//! but they only ever sit on terminal-owner boundaries
//! ([`crate::core::text_owner_spans`]): caret motion, deletion, the visible
//! window, the painter and the mouse hit-test all work in whole owners, so a
//! combining mark, an emoji sequence or a script cluster is one caret step,
//! one deletion and one painted glyph, exactly as it is in the terminal grid.

use crate::core::{Attrs, Snapshot, TextOwnerSpan, text_owner_spans};

/// The owners of the field text and its scalar count.
pub(super) struct FieldOwners {
    spans: Vec<TextOwnerSpan>,
    char_count: usize,
}

impl FieldOwners {
    pub(super) fn of(text: &str) -> Self {
        Self {
            spans: text_owner_spans(text, false),
            char_count: text.chars().count(),
        }
    }

    /// The caret positions, in order: each owner's first scalar, then the end.
    fn boundaries(&self) -> impl Iterator<Item = usize> + '_ {
        self.spans
            .iter()
            .map(|span| span.chars.start)
            .chain(std::iter::once(self.char_count))
    }

    /// The first caret position at or after `index` (the end of the owner
    /// that contains it), clamped to the text.
    pub(super) fn ceil(&self, index: usize) -> usize {
        self.boundaries()
            .find(|&boundary| boundary >= index)
            .unwrap_or(self.char_count)
    }

    /// The last caret position at or before `index` (the start of the owner
    /// that contains it).
    pub(super) fn floor(&self, index: usize) -> usize {
        self.boundaries()
            .take_while(|&boundary| boundary <= index)
            .last()
            .unwrap_or(0)
    }

    /// The caret position one owner before `caret`.
    pub(super) fn previous(&self, caret: usize) -> usize {
        self.boundaries()
            .take_while(|&boundary| boundary < caret)
            .last()
            .unwrap_or(0)
    }

    /// The caret position one owner after `caret`.
    pub(super) fn next(&self, caret: usize) -> usize {
        self.boundaries()
            .find(|&boundary| boundary > caret)
            .unwrap_or(self.char_count)
    }

    /// The index of the owner the caret sits before, or the owner count when
    /// the caret is at the end.
    fn caret_owner(&self, caret: usize) -> usize {
        self.spans
            .iter()
            .position(|span| span.chars.start >= caret)
            .unwrap_or(self.spans.len())
    }

    /// The first visible owner for a field `width` cells wide. Text that fits
    /// is shown from its start; longer text scrolls so the owner under the
    /// caret (or one cell for a caret at the end) is the last thing shown.
    pub(super) fn visible_start(&self, caret: usize, width: usize) -> usize {
        let total: usize = self.spans.iter().map(|span| span.width).sum();
        if total <= width {
            return 0;
        }
        let caret_owner = self.caret_owner(caret);
        let mut used = self.spans.get(caret_owner).map_or(1, |span| span.width);
        let mut start = caret_owner;
        while start > 0 && used + self.spans[start - 1].width <= width {
            used += self.spans[start - 1].width;
            start -= 1;
        }
        start
    }

    /// The caret position for a click `column` cells into the field: the
    /// start of the owner drawn there, or the position after the last drawn
    /// owner for a click to its right.
    pub(super) fn hit(&self, caret: usize, width: usize, column: usize) -> usize {
        let mut x = 0usize;
        for span in &self.spans[self.visible_start(caret, width)..] {
            if x + span.width > width {
                return span.chars.start;
            }
            if column < x + span.width {
                return span.chars.start;
            }
            x += span.width;
        }
        self.char_count
    }

    /// Paint the field at `row`, `column`, `width` cells wide: owners from the
    /// visible start, the caret owner in `caret_attrs`, owners inside the
    /// selection `[lo, hi)` in `selected_attrs`, and blank cells after the
    /// text. A caret at the end takes the cell after the text, or the last
    /// drawn owner when the text fills the field.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint(
        &self,
        snapshot: &mut Snapshot,
        row: usize,
        column: usize,
        width: usize,
        caret: usize,
        selection: Option<(usize, usize)>,
        attrs: [Attrs; 3],
    ) {
        let [plain, caret_attrs, selected_attrs] = attrs;
        let columns = snapshot.dimensions.columns;
        if row >= snapshot.dimensions.rows {
            return;
        }
        let right = (column + width).min(columns);
        let mut put = |x: usize, mut cell: crate::core::Cell, wide: bool, attrs: Attrs| {
            if x >= right {
                return;
            }
            cell.attrs = attrs;
            snapshot.cells[row * columns + x] = cell;
            if wide && x + 1 < right {
                let mut tail = crate::core::Cell::new(' ', attrs);
                tail.wide_continuation = true;
                snapshot.cells[row * columns + x + 1] = tail;
            }
        };
        let mut x = column;
        let mut last_drawn = None;
        for span in &self.spans[self.visible_start(caret, width)..] {
            if x + span.width > right {
                break;
            }
            let start = span.chars.start;
            let cell_attrs = if start == caret {
                caret_attrs
            } else if selection.is_some_and(|(lo, hi)| start >= lo && start < hi) {
                selected_attrs
            } else {
                plain
            };
            put(x, span.cell, span.width == 2, cell_attrs);
            last_drawn = Some((x, span));
            x += span.width;
        }
        let at_end = caret >= self.char_count;
        if at_end && x < right {
            put(x, crate::core::Cell::new(' ', plain), false, caret_attrs);
            x += 1;
        } else if at_end && let Some((drawn_at, span)) = last_drawn {
            put(drawn_at, span.cell, span.width == 2, caret_attrs);
        }
        while x < right {
            put(x, crate::core::Cell::new(' ', plain), false, plain);
            x += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FieldOwners;

    const ZWJ: &str = "\u{1f469}\u{200d}\u{1f4bb}";

    /// Caret steps and snapping move by whole owners.
    #[test]
    fn caret_positions_are_owner_boundaries() {
        let text = format!("e\u{301}{ZWJ}x");
        let owners = FieldOwners::of(&text);
        assert_eq!(owners.next(0), 2);
        assert_eq!(owners.next(2), 5);
        assert_eq!(owners.previous(6), 5);
        assert_eq!(owners.previous(5), 2);
        assert_eq!(owners.previous(0), 0);
        assert_eq!(owners.next(6), 6);
        assert_eq!(owners.ceil(1), 2);
        assert_eq!(owners.ceil(3), 5);
        assert_eq!(owners.floor(4), 2);
        assert_eq!(owners.floor(1), 0);
    }

    /// The visible window and the click map count columns per owner: a wide
    /// owner takes two cells, so a long field scrolls by owner and a click on
    /// a wide owner's tail lands before it.
    #[test]
    fn window_and_hit_count_owner_columns() {
        let text = format!("ab{ZWJ}cd");
        let owners = FieldOwners::of(&text);
        let end = text.chars().count();
        assert_eq!(owners.visible_start(end, 10), 0, "6 columns fit 10");
        // Caret at the end of a 4-cell field: "cd" plus the caret cell need 3
        // cells, and the 2-cell emoji does not fit before them.
        assert_eq!(owners.visible_start(end, 4), 3);
        assert_eq!(owners.visible_start(end, 5), 2);
        assert_eq!(owners.hit(end, 10, 2), 2, "the emoji's first cell");
        assert_eq!(owners.hit(end, 10, 3), 2, "the emoji's tail");
        assert_eq!(owners.hit(end, 10, 4), 5);
        assert_eq!(owners.hit(end, 10, 9), end, "right of the text");
        // ASCII keeps the scalar-per-cell window of the original field.
        let ascii = "abcdefghij";
        let owners = FieldOwners::of(ascii);
        for caret in 0..=10 {
            assert_eq!(owners.visible_start(caret, 4), caret.saturating_sub(3));
        }
        assert_eq!(FieldOwners::of("abcd").visible_start(4, 4), 0, "fits");
    }
}

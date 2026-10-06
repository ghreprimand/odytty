// SPDX-License-Identifier: GPL-3.0-only
//! Bounded source-preserving streaming owners and footprint replacement.

use super::*;

impl Screen {
    fn extend_cluster(&mut self, ch: char, scalar_width: usize) -> bool {
        let Some(owner) = self.cluster_owner else {
            return false;
        };
        let cell = self.rows[owner.row][owner.column];
        let spacing = super::super::char_width::thai_lao_spacing_extension(cell.ch, ch);
        let indic = super::super::indic::extends(cell.ch, cell.combining(), ch);
        let emoji = super::super::emoji_width::extends(cell.ch, cell.combining(), ch);
        if scalar_width != 0 && !spacing && !indic && !emoji {
            return false;
        }
        let mut extended = cell;
        if !extended.push_combining(ch) {
            return false;
        }
        let old_width = super::super::char_width::owner_display_width(
            cell.ch,
            cell.combining(),
            self.ambiguous_wide,
        )
        .max(1);
        let width = super::super::char_width::owner_display_width(
            extended.ch,
            extended.combining(),
            self.ambiguous_wide,
        )
        .max(1);
        if width == old_width {
            self.rows[owner.row][owner.column] = extended;
            self.mark_dirty();
        } else {
            // Rebuild the complete footprint through the ordinary write path.
            // At the edge it generates padding and wraps the whole owner.
            if self.insert_mode
                && width > old_width
                && owner.column + old_width < self.dimensions.columns
            {
                self.cursor = Position {
                    row: owner.row,
                    column: owner.column + old_width,
                };
                self.pending_wrap = false;
                self.insert_chars(width - old_width);
            }
            self.rows[owner.row][owner.column] = self.current_blank();
            if old_width == 2 && owner.column + 1 < self.dimensions.columns {
                self.rows[owner.row][owner.column + 1] = self.current_blank();
            }
            self.cursor = owner;
            self.pending_wrap = false;
            let insert_mode = self.insert_mode;
            self.insert_mode = false;
            self.print_owner(extended, width);
            self.insert_mode = insert_mode;
        }
        self.output_since_last_resize = true;
        true
    }

    pub(super) fn print_char(&mut self, ch: char) {
        // Charset seam: translate through DEC Special Graphics BEFORE width
        // computation and `last_graphic_char` capture, so the grid, wrap
        // logic, and REP all operate on the final Unicode glyph. Only
        // single-byte-range characters (`0x5F..=0x7E`) can map; multi-byte
        // UTF-8 decodes above that range and passes through untouched. The
        // map is idempotent, so a REP replay of a stored translated glyph is
        // unaffected even if the charset changed in between.
        let ch = if self.charsets.active_graphics() && matches!(ch, '\x5f'..='\x7e') {
            charset::dec_special_graphics(ch)
        } else {
            ch
        };
        let width = super::super::char_width::char_display_width(ch, self.ambiguous_wide);
        if self.extend_cluster(ch, width) {
            return;
        }
        // Unattached width-zero format controls and selectors retain their
        // historical zero-column, unretained behavior. Ordinary leading
        // marks and bounded-owner overflow retain their source in a new cell.
        if width == 0
            && self.cluster_owner.is_none()
            && super::super::char_width::is_default_ignorable(ch)
        {
            return;
        }
        let cell = Cell::new_protected(ch, self.current_print_attrs(), self.current_protected);
        self.print_owner(cell, width.max(1));
    }

    fn print_owner(&mut self, cell: Cell, width: usize) {
        // A grid too narrow for a pair degrades a wide owner to one column,
        // the same fallback reflow applies, instead of wrapping it forever.
        let width = if self.dimensions.columns < 2 {
            width.min(1)
        } else {
            width
        };
        let ch = cell.ch;
        self.last_graphic_char = Some(ch);
        // The shell applied output: a width-changing resize that follows can
        // trust that a repaint is in the loop and honor the cursor-anchor
        // override (see `output_since_last_resize`).
        self.output_since_last_resize = true;

        if self.pending_wrap {
            // The row we are leaving filled to the right edge and the logical
            // line continues here: mark it as a soft wrap so resize can rejoin.
            self.rows[self.cursor.row].wrapped = true;
            self.carriage_return();
            self.line_feed();
            self.pending_wrap = false;
        }

        if self.auto_wrap && self.cursor.column + width > self.dimensions.columns {
            // A wide glyph does not fit in the remaining columns. xterm does not
            // split it across rows: blank the trailing cell(s) and soft-wrap the
            // glyph onto the next row, marking the row wrapped so resize rejoins
            // the logical line.
            let blank = Cell::layout_blank(self.current_blank().attrs);
            let r = self.cursor.row;
            let c = self.cursor.column;
            self.clear_wide_orphans(r, c, self.dimensions.columns - c);
            for col in c..self.dimensions.columns {
                self.rows[r][col] = blank;
            }
            self.transform_row_button_spans(
                r,
                RowButtonMutation::Overwrite {
                    start: c,
                    end: self.dimensions.columns,
                },
            );
            self.rows[r].wrapped = true;
            self.carriage_return();
            self.line_feed();
        }

        if self.insert_mode {
            // IRM: open `width` blank cells at the cursor, shifting the rest of
            // the line right (cells past the edge drop off), then write into the
            // freshly cleared slot. `insert_chars` handles the right-edge
            // truncation and wide-pair sanitization.
            self.insert_chars(width);
        }

        let row = self.cursor.row;
        let column = self.cursor.column;
        // Overwriting either half of an existing wide pair must clear its
        // partner so no half-wide orphan survives.
        self.clear_wide_orphans(row, column, width);
        // Ordinary output over a finished button label destroys it, the same
        // as an erase over those cells.
        self.transform_row_button_spans(
            row,
            RowButtonMutation::Overwrite {
                start: column,
                end: (column + width).min(self.dimensions.columns),
            },
        );
        let attrs = cell.attrs;
        self.rows[row][column] = cell;

        if width == 2 && column + 1 < self.dimensions.columns {
            self.rows[row][column + 1] = Cell::wide_spacer_protected(attrs, cell.protected);
        }

        if self.auto_wrap && self.cursor.column + width >= self.dimensions.columns {
            self.cursor.column = self.dimensions.columns - 1;
            self.pending_wrap = true;
        } else if self.cursor.column + width >= self.dimensions.columns {
            self.cursor.column = self.dimensions.columns - 1;
            self.pending_wrap = false;
        } else {
            self.cursor.column += width;
        }
        self.cluster_owner = Some(Position { row, column });
        self.mark_dirty();
    }
}

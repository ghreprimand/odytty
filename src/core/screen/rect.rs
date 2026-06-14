// SPDX-License-Identifier: GPL-3.0-only
//! DEC rectangular-area operations and character protection.
//!
//! Rectangle coordinates are 1-based, inclusive, and clamp to the visible page.
//! With DECOM set, row coordinates are relative to the active vertical margins;
//! columns remain screen-relative because OdyTTY does not implement horizontal
//! margins yet. Any rectangle write sanitizes affected rows so a rectangle edge
//! that slices a wide glyph clears the pair instead of leaving an orphan.
//!
//! DECSACE controls only DECCARA/DECRARA here. In exact mode, attributes apply
//! to the selected rectangle. In stream mode, coordinates name a wrapped stream:
//! the first row from the left coordinate to the right edge, middle rows fully,
//! and the last row from column 0 to the right coordinate. OdyTTY does not track
//! xterm's uninitialized-cell distinction, so stream mode applies to blank cells
//! too.

use unicode_width::UnicodeWidthChar;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    top: usize,
    left: usize,
    bottom: usize,
    right: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct RectAttrMask {
    bold: Option<bool>,
    underline: Option<bool>,
    blink: Option<bool>,
    inverse: Option<bool>,
}

impl Rect {
    fn width(self) -> usize {
        self.right - self.left + 1
    }

    fn height(self) -> usize {
        self.bottom - self.top + 1
    }

    fn row_bounds(self, extent: RectAttributeExtent, row: usize, columns: usize) -> (usize, usize) {
        match extent {
            RectAttributeExtent::Exact => (self.left, self.right),
            RectAttributeExtent::Stream if self.top == self.bottom => (self.left, self.right),
            RectAttributeExtent::Stream if row == self.top => (self.left, columns - 1),
            RectAttributeExtent::Stream if row == self.bottom => (0, self.right),
            RectAttributeExtent::Stream => (0, columns - 1),
        }
    }
}

impl Screen {
    /// DECSCA (`CSI Ps " q`): select the protection bit applied to future
    /// printed/filled cells. Ps=1 protects; Ps=0/2 clears; other values ignored.
    pub(super) fn set_char_protection(&mut self, mode: usize) {
        match mode {
            0 | 2 => self.current_protected = false,
            1 => self.current_protected = true,
            _ => {}
        }
    }

    /// DECSACE (`CSI Ps * x`): select the extent used by DECCARA/DECRARA.
    /// Ps=0/1 choose stream mode; Ps=2 chooses exact rectangle mode.
    pub(super) fn set_rect_attr_extent(&mut self, mode: usize) {
        match mode {
            0 | 1 => self.rect_attr_extent = RectAttributeExtent::Stream,
            2 => self.rect_attr_extent = RectAttributeExtent::Exact,
            _ => {}
        }
    }

    pub(super) fn selective_erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.selective_erase_row_range(
                    self.cursor.row,
                    self.cursor.column,
                    self.dimensions.columns - 1,
                );
                for row in self.cursor.row + 1..self.dimensions.rows {
                    self.selective_erase_row_range(row, 0, self.dimensions.columns - 1);
                }
            }
            1 => {
                for row in 0..self.cursor.row {
                    self.selective_erase_row_range(row, 0, self.dimensions.columns - 1);
                }
                self.selective_erase_row_range(self.cursor.row, 0, self.cursor.column);
            }
            2 | 3 => {
                for row in 0..self.dimensions.rows {
                    self.selective_erase_row_range(row, 0, self.dimensions.columns - 1);
                }
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => return,
        }
        self.pending_wrap = false;
        self.mark_dirty();
    }

    pub(super) fn selective_erase_line(&mut self, mode: usize) {
        match mode {
            0 => self.selective_erase_row_range(
                self.cursor.row,
                self.cursor.column,
                self.dimensions.columns - 1,
            ),
            1 => self.selective_erase_row_range(self.cursor.row, 0, self.cursor.column),
            2 => self.selective_erase_row_range(self.cursor.row, 0, self.dimensions.columns - 1),
            _ => return,
        }
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DECCRA (`CSI Pt;Pl;Pb;Pr;Pp;Pt;Pl;Pp $ v`): copy a rectangle within
    /// the visible page. Page parameters are accepted but ignored because
    /// OdyTTY exposes one page.
    pub(super) fn copy_rect(&mut self, params: &Params) {
        let Some(source) = self.rect_from_params(params, 0) else {
            return;
        };
        let Some((dest_top, dest_left)) = self.dest_from_params(params, 5) else {
            return;
        };

        let height = source
            .height()
            .min(self.dimensions.rows.saturating_sub(dest_top));
        let width = source
            .width()
            .min(self.dimensions.columns.saturating_sub(dest_left));
        if height == 0 || width == 0 {
            return;
        }

        let blank = self.current_blank();
        let mut copied = Vec::with_capacity(height);
        for row in source.top..source.top + height {
            let mut cells = self.rows[row][source.left..source.left + width].to_vec();
            sanitize_wide_row(&mut cells, blank);
            copied.push(cells);
        }

        for (row_offset, cells) in copied.into_iter().enumerate() {
            let row = dest_top + row_offset;
            for (column_offset, cell) in cells.into_iter().enumerate() {
                self.rows[row][dest_left + column_offset] = cell;
            }
            sanitize_wide_row(&mut self.rows[row], blank);
        }

        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DECFRA (`CSI Pch;Pt;Pl;Pb;Pr $ x`): fill a rectangle with a printable
    /// single-cell character using current SGR, hyperlink, and DECSCA state.
    pub(super) fn fill_rect(&mut self, params: &Params) {
        let Some(rect) = self.rect_from_params(params, 1) else {
            return;
        };
        let cell = self.fill_cell(param_or(params, 0, b' ' as usize));
        let blank = self.current_blank();

        for row in rect.top..=rect.bottom {
            for column in rect.left..=rect.right {
                self.rows[row][column] = cell;
            }
            sanitize_wide_row(&mut self.rows[row], blank);
        }

        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DECERA (`CSI Pt;Pl;Pb;Pr $ z`): erase a rectangle regardless of DECSCA.
    pub(super) fn erase_rect(&mut self, params: &Params) {
        let Some(rect) = self.rect_from_params(params, 0) else {
            return;
        };
        self.erase_rect_inner(rect, false);
    }

    /// DECSERA (`CSI Pt;Pl;Pb;Pr $ {`): erase only unprotected cells in a
    /// rectangle.
    pub(super) fn selective_erase_rect(&mut self, params: &Params) {
        let Some(rect) = self.rect_from_params(params, 0) else {
            return;
        };
        self.erase_rect_inner(rect, true);
    }

    /// DECCARA (`CSI Pt;Pl;Pb;Pr;Pm $ r`): change selected presentation
    /// attributes inside the current DECSACE extent. The DEC/xterm subset
    /// supported here is bold, plain underline, blink, and inverse. Extended
    /// underline subparameters (`4:x`) are intentionally ignored in this path.
    pub(super) fn change_rect_attrs(&mut self, params: &Params) {
        let Some(rect) = self.rect_from_params(params, 0) else {
            return;
        };
        let mask = change_rect_attr_mask(params);
        if mask.is_empty() {
            return;
        }
        self.apply_rect_attr_mask(rect, |attrs| mask.apply(attrs));
    }

    /// DECRARA (`CSI Pt;Pl;Pb;Pr;Pm $ t`): toggle selected presentation
    /// attributes inside the current DECSACE extent. Applying the same toggle
    /// sequence twice restores the original attributes.
    pub(super) fn reverse_rect_attrs(&mut self, params: &Params) {
        let Some(rect) = self.rect_from_params(params, 0) else {
            return;
        };
        let mask = reverse_rect_attr_mask(params);
        if mask.is_empty() {
            return;
        }
        self.apply_rect_attr_mask(rect, |attrs| mask.toggle(attrs));
    }

    fn erase_rect_inner(&mut self, rect: Rect, selective: bool) {
        let blank = self.current_blank();
        for row in rect.top..=rect.bottom {
            for column in rect.left..=rect.right {
                if !selective || !self.rows[row][column].protected {
                    self.rows[row][column] = blank;
                }
            }
            sanitize_wide_row(&mut self.rows[row], blank);
        }
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn apply_rect_attr_mask(&mut self, rect: Rect, mut apply: impl FnMut(&mut Attrs)) {
        for row in rect.top..=rect.bottom {
            let (left, right) =
                rect.row_bounds(self.rect_attr_extent, row, self.dimensions.columns);
            for column in left..=right {
                apply(&mut self.rows[row][column].attrs);
            }
        }
        self.pending_wrap = false;
        self.mark_dirty();
    }

    fn selective_erase_row_range(&mut self, row: usize, left: usize, right: usize) {
        let blank = self.current_blank();
        for column in left..=right {
            if !self.rows[row][column].protected {
                self.rows[row][column] = blank;
            }
        }
        sanitize_wide_row(&mut self.rows[row], blank);
    }

    fn fill_cell(&self, value: usize) -> Cell {
        let ch = char::from_u32(value as u32)
            .filter(|ch| !ch.is_control() && UnicodeWidthChar::width(*ch) == Some(1))
            .unwrap_or(' ');
        Cell::new_protected(ch, self.current_print_attrs(), self.current_protected)
    }

    fn rect_from_params(&self, params: &Params, start: usize) -> Option<Rect> {
        let row_default = self.row_default();
        let top_raw = rect_param_or(params, start, 1);
        let left_raw = rect_param_or(params, start + 1, 1);
        let bottom_raw = rect_param_or(params, start + 2, row_default);
        let right_raw = rect_param_or(params, start + 3, self.dimensions.columns);

        if top_raw > bottom_raw || left_raw > right_raw {
            return None;
        }

        let top = self.rect_row(top_raw);
        let bottom = self.rect_row(bottom_raw);
        let left = one_based_to_index(left_raw, self.dimensions.columns);
        let right = one_based_to_index(right_raw, self.dimensions.columns);

        (top <= bottom && left <= right).then_some(Rect {
            top,
            left,
            bottom,
            right,
        })
    }

    fn dest_from_params(&self, params: &Params, start: usize) -> Option<(usize, usize)> {
        let row = self.rect_row(rect_param_or(params, start, 1));
        let column =
            one_based_to_index(rect_param_or(params, start + 1, 1), self.dimensions.columns);
        (row < self.dimensions.rows && column < self.dimensions.columns).then_some((row, column))
    }

    fn row_default(&self) -> usize {
        if self.origin_mode {
            let (top, bottom) = self.effective_region();
            bottom - top + 1
        } else {
            self.dimensions.rows
        }
    }

    fn rect_row(&self, raw: usize) -> usize {
        let raw = raw.max(1);
        if self.origin_mode {
            let (top, bottom) = self.effective_region();
            (top + raw - 1).min(bottom)
        } else {
            one_based_to_index(raw, self.dimensions.rows)
        }
    }
}

fn rect_param_or(params: &Params, index: usize, default: usize) -> usize {
    let value = param_or(params, index, default);
    if value == 0 { default } else { value }
}

fn one_based_to_index(value: usize, limit: usize) -> usize {
    value.saturating_sub(1).min(limit - 1)
}

impl RectAttrMask {
    fn all_on() -> Self {
        Self {
            bold: Some(true),
            underline: Some(true),
            blink: Some(true),
            inverse: Some(true),
        }
    }

    fn all_toggle() -> Self {
        Self::all_on()
    }

    fn is_empty(self) -> bool {
        self.bold.is_none()
            && self.underline.is_none()
            && self.blink.is_none()
            && self.inverse.is_none()
    }

    fn apply(self, attrs: &mut Attrs) {
        if let Some(bold) = self.bold {
            attrs.set_bold(bold);
        }
        if let Some(underline) = self.underline {
            attrs.set_underline_style(if underline {
                UnderlineStyle::Straight
            } else {
                UnderlineStyle::None
            });
        }
        if let Some(blink) = self.blink {
            attrs.set_blink(blink);
        }
        if let Some(inverse) = self.inverse {
            attrs.set_inverse(inverse);
        }
    }

    fn toggle(self, attrs: &mut Attrs) {
        if self.bold.is_some() {
            attrs.set_bold(!attrs.bold());
        }
        if self.underline.is_some() {
            let underline = attrs.effective_underline_style() == UnderlineStyle::None;
            attrs.set_underline_style(if underline {
                UnderlineStyle::Straight
            } else {
                UnderlineStyle::None
            });
        }
        if self.blink.is_some() {
            attrs.set_blink(!attrs.blink());
        }
        if self.inverse.is_some() {
            attrs.set_inverse(!attrs.inverse());
        }
    }
}

fn change_rect_attr_mask(params: &Params) -> RectAttrMask {
    let mut mask = RectAttrMask::default();
    for group in sgr_params(params).into_iter().skip(4) {
        match group {
            [0] => {
                mask.bold = Some(false);
                mask.underline = Some(false);
                mask.blink = Some(false);
                mask.inverse = Some(false);
            }
            [1] => mask.bold = Some(true),
            [4] => mask.underline = Some(true),
            [5] => mask.blink = Some(true),
            [7] => mask.inverse = Some(true),
            [22] => mask.bold = Some(false),
            [24] => mask.underline = Some(false),
            [25] => mask.blink = Some(false),
            [27] => mask.inverse = Some(false),
            _ => {}
        }
    }
    mask
}

fn reverse_rect_attr_mask(params: &Params) -> RectAttrMask {
    let mut mask = RectAttrMask::default();
    for group in sgr_params(params).into_iter().skip(4) {
        match group {
            [0] => mask = RectAttrMask::all_toggle(),
            [1] => mask.bold = Some(true),
            [4] => mask.underline = Some(true),
            [5] => mask.blink = Some(true),
            [7] => mask.inverse = Some(true),
            _ => {}
        }
    }
    mask
}

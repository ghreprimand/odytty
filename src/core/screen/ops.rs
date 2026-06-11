//! Screen operations split out of the parent module for the modularity cap:
//! scrolling, line/char insert-delete, erase, cursor motion, mode setting,
//! and reset. These are inherent `Screen` methods moved verbatim from
//! `super` (M4 mechanical split); private methods were widened to
//! `pub(super)` so the parent module can still call them.

use super::*;

impl Screen {
    pub(super) fn scroll_up_full(&mut self) {
        let removed = self.rows.remove(0);
        let background = self.current_attrs.background;

        if self.primary_screen.is_none() {
            self.scrollback.push_row(removed);
        }

        self.rows
            .push(blank_row_with_bg(self.dimensions.columns, background));
        let scrollback_rows =
            if self.primary_screen.is_none() && !self.graphics.placements().is_empty() {
                self.scrollback.physical_len(self.dimensions.columns)
            } else {
                0
            };
        self.graphics.scroll_full_up(1, scrollback_rows);
        self.mark_dirty();
    }

    pub(super) fn scroll_up_region(&mut self) {
        if let Some(region) = self.scroll_region {
            let background = self.current_attrs.background;
            self.rows.remove(region.top);
            self.rows.insert(
                region.bottom,
                blank_row_with_bg(self.dimensions.columns, background),
            );
            self.graphics.scroll_region_up(region.top, region.bottom, 1);
            self.mark_dirty();
        }
    }

    /// SU (CSI Ps S): scroll the active region up by `count` lines, discarding
    /// lines off the top of the region and filling at the bottom with BCE-aware
    /// blank rows. Falls back to the full screen when no DECSTBM region is set.
    /// Never feeds scrollback (no pollution) and does not move the cursor.
    pub(super) fn scroll_region_up(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        let count = count.max(1).min(bottom - top + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(top);
            self.rows.insert(
                bottom,
                blank_row_with_bg(self.dimensions.columns, background),
            );
        }
        self.graphics.scroll_region_up(top, bottom, count);
        self.mark_dirty();
    }

    /// SD (CSI Ps T): scroll the active region down by `count` lines, discarding
    /// lines off the bottom of the region and filling at the top with BCE-aware
    /// blank rows. Falls back to the full screen when no DECSTBM region is set.
    /// Never feeds scrollback (no pollution) and does not move the cursor.
    pub(super) fn scroll_region_down(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        let count = count.max(1).min(bottom - top + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(bottom);
            self.rows
                .insert(top, blank_row_with_bg(self.dimensions.columns, background));
        }
        self.graphics.scroll_region_down(top, bottom, count);
        self.mark_dirty();
    }

    /// Active vertical scroll margins. Falls back to the full screen when no
    /// explicit DECSTBM region is set (the standard behaviour for RI/IL/DL).
    pub(super) fn effective_region(&self) -> (usize, usize) {
        match self.scroll_region {
            Some(region) => (region.top, region.bottom),
            None => (0, self.dimensions.rows - 1),
        }
    }

    /// RI (ESC M): at the top margin, scroll the region down by one; otherwise
    /// move the cursor up one row. Never feeds scrollback.
    pub(super) fn reverse_index(&mut self) {
        self.pending_wrap = false;
        let (top, bottom) = self.effective_region();
        let background = self.current_attrs.background;

        if self.cursor.row == top {
            self.rows.remove(bottom);
            self.rows
                .insert(top, blank_row_with_bg(self.dimensions.columns, background));
            self.graphics.scroll_region_down(top, bottom, 1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
        self.mark_dirty();
    }

    /// IL (CSI Ps L): insert `count` blank lines at the cursor row, scrolling
    /// the rows below it down within the region. Lines pushed past the region
    /// bottom are discarded (never to scrollback). No-op outside the region.
    pub(super) fn insert_lines(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }

        let count = count.max(1).min(bottom - self.cursor.row + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(bottom);
            self.rows.insert(
                self.cursor.row,
                blank_row_with_bg(self.dimensions.columns, background),
            );
        }

        self.graphics
            .scroll_region_down(self.cursor.row, bottom, count);
        self.cursor.column = 0;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DL (CSI Ps M): delete `count` lines at the cursor row, scrolling the
    /// rows below it up within the region and filling blanks at the region
    /// bottom. No-op outside the region.
    pub(super) fn delete_lines(&mut self, count: usize) {
        let (top, bottom) = self.effective_region();
        if self.cursor.row < top || self.cursor.row > bottom {
            return;
        }

        let count = count.max(1).min(bottom - self.cursor.row + 1);
        let background = self.current_attrs.background;
        for _ in 0..count {
            self.rows.remove(self.cursor.row);
            self.rows.insert(
                bottom,
                blank_row_with_bg(self.dimensions.columns, background),
            );
        }

        self.graphics
            .scroll_region_up(self.cursor.row, bottom, count);
        self.cursor.column = 0;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// ICH (CSI Ps @): insert `count` blank cells at the cursor, shifting the
    /// rest of the line right. Cells pushed past the right edge are discarded.
    /// Row-local: no wrap, no scroll, cursor stays in place. Fill blanks use
    /// the active background color and otherwise default attributes, matching
    /// xterm-style background-color-erase behavior for insert fills.
    pub(super) fn insert_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);
        let blank = self.current_blank();

        let row = &mut self.rows[self.cursor.row];
        for _ in 0..count {
            row.insert(column, blank);
        }
        row.truncate(columns);

        sanitize_wide_row(row, blank);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// DCH (CSI Ps P): delete `count` cells at the cursor, shifting the rest of
    /// the line left and filling blanks at the right edge. Row-local: no wrap,
    /// no scroll, cursor stays in place. Fill blanks use the active background
    /// color and otherwise default attributes, matching xterm-style
    /// background-color-erase behavior for delete fills.
    pub(super) fn delete_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);
        let blank = self.current_blank();

        let row = &mut self.rows[self.cursor.row];
        for _ in 0..count {
            row.remove(column);
        }
        while row.len() < columns {
            row.push(blank);
        }

        sanitize_wide_row(row, blank);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// ECH (CSI Ps X): erase `count` cells from the cursor in place, overwriting
    /// them with blanks WITHOUT shifting the rest of the line. Row-local: no
    /// wrap, no scroll, cursor stays put. Blanks use the active background
    /// color and otherwise default attributes, matching xterm-style
    /// background-color-erase behavior.
    pub(super) fn erase_chars(&mut self, count: usize) {
        let columns = self.dimensions.columns;
        let column = self.cursor.column;
        let count = count.max(1).min(columns - column);
        let blank = self.current_blank();

        let row = &mut self.rows[self.cursor.row];
        for cell in &mut row[column..column + count] {
            *cell = blank;
        }

        sanitize_wide_row(row, blank);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// REP (CSI Ps b): repeat the last printed graphic character `count` times,
    /// using normal print processing (so the current SGR attrs apply and
    /// autowrap behaves as if the character were typed again). Omitted/zero
    /// count = 1. No-op when no graphic character has been printed yet.
    /// Replaying through `print_char` means a wide last character repeats as a
    /// wide glyph and wraps correctly.
    pub(super) fn repeat_char(&mut self, count: usize) {
        let Some(ch) = self.last_graphic_char else {
            return;
        };
        let count = count.max(1);
        for _ in 0..count {
            self.print_char(ch);
        }
    }

    pub(super) fn move_up(&mut self, count: usize) {
        self.cursor.row = self.cursor.row.saturating_sub(count);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    pub(super) fn move_down(&mut self, count: usize) {
        self.cursor.row = (self.cursor.row + count).min(self.dimensions.rows - 1);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    pub(super) fn move_right(&mut self, count: usize) {
        self.cursor.column = (self.cursor.column + count).min(self.dimensions.columns - 1);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    pub(super) fn move_left(&mut self, count: usize) {
        self.cursor.column = self.cursor.column.saturating_sub(count);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    pub(super) fn move_to(&mut self, row: usize, column: usize) {
        self.cursor.row = row.saturating_sub(1).min(self.dimensions.rows - 1);
        self.cursor.column = column.saturating_sub(1).min(self.dimensions.columns - 1);
        self.pending_wrap = false;
        self.mark_dirty();
    }

    /// CUP/HVP/VPA addressing honoring DECOM (origin mode).
    ///
    /// `row`/`column` are 1-based. When origin mode is off this is identical to
    /// [`Screen::move_to`] (full-screen absolute addressing). When origin mode
    /// is on, the row is interpreted relative to the active scroll region top
    /// and clamped to the region bottom, so a program that set DECSTBM + DECOM
    /// can address rows `1..=region_height` without escaping the region. The
    /// column is unaffected by origin mode (no horizontal margins here).
    pub(super) fn move_to_origin(&mut self, row: usize, column: usize) {
        let column = column.saturating_sub(1).min(self.dimensions.columns - 1);
        if self.origin_mode {
            let (top, bottom) = self.effective_region();
            let target = top + row.max(1) - 1;
            self.cursor.row = target.min(bottom);
        } else {
            self.cursor.row = row.saturating_sub(1).min(self.dimensions.rows - 1);
        }
        self.cursor.column = column;
        self.pending_wrap = false;
        self.mark_dirty();
    }

    pub(super) fn erase_display(&mut self, mode: usize) {
        let background = self.current_attrs.background;
        match mode {
            0 => {
                self.erase_line_from_cursor();
                for row in self.cursor.row + 1..self.dimensions.rows {
                    self.rows[row] = blank_row_with_bg(self.dimensions.columns, background);
                }
            }
            1 => {
                for row in 0..self.cursor.row {
                    self.rows[row] = blank_row_with_bg(self.dimensions.columns, background);
                }
                self.erase_line_to_cursor();
            }
            2 | 3 => {
                for row in &mut self.rows {
                    *row = blank_row_with_bg(self.dimensions.columns, background);
                }
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
        self.graphics.erase_display(
            mode,
            self.cursor.row,
            self.cursor.column,
            self.dimensions.rows,
            self.dimensions.columns,
        );
        self.mark_dirty();
    }

    pub(super) fn erase_line(&mut self, mode: usize) {
        match mode {
            0 => self.erase_line_from_cursor(),
            1 => self.erase_line_to_cursor(),
            2 => {
                self.rows[self.cursor.row] = self.current_blank_row();
            }
            _ => {}
        }
        self.mark_dirty();
    }

    pub(super) fn erase_line_from_cursor(&mut self) {
        let blank = self.current_blank();
        let row = self.cursor.row;
        for column in self.cursor.column..self.dimensions.columns {
            self.rows[row][column] = blank;
        }
        // Erasing the lead-side boundary can orphan a wide pair (a continuation
        // at the cursor whose lead is just left of it); repair the row.
        sanitize_wide_row(&mut self.rows[row], blank);
        self.mark_dirty();
    }

    pub(super) fn erase_line_to_cursor(&mut self) {
        let blank = self.current_blank();
        let row = self.cursor.row;
        for column in 0..=self.cursor.column {
            self.rows[row][column] = blank;
        }
        // Erasing up to the cursor can orphan a wide lead at the cursor whose
        // continuation sits just right of it; repair the row.
        sanitize_wide_row(&mut self.rows[row], blank);
        self.mark_dirty();
    }

    pub(super) fn current_blank(&self) -> Cell {
        Cell::blank_with_bg(self.current_attrs.background)
    }

    pub(super) fn current_blank_row(&self) -> Line {
        blank_row_with_bg(self.dimensions.columns, self.current_attrs.background)
    }

    pub(super) fn current_print_attrs(&self) -> Attrs {
        let mut attrs = self.current_attrs;
        attrs.hyperlink = self.active_hyperlink;
        attrs
    }

    pub(super) fn mark_dirty(&mut self) {
        self.dirty = DirtyRegion::Full;
        self.render_revision = self.render_revision.wrapping_add(1);
    }

    pub(super) fn apply_sgr(&mut self, params: &Params) {
        let codes = sgr_codes(params);
        let codes = if codes.is_empty() { vec![0] } else { codes };
        let mut index = 0;

        while index < codes.len() {
            match codes[index] {
                0 => self.current_attrs = Attrs::default(),
                1 => self.current_attrs.bold = true,
                2 => self.current_attrs.dim = true,
                3 => self.current_attrs.italic = true,
                4 => self.current_attrs.underline = true,
                7 => self.current_attrs.inverse = true,
                8 => self.current_attrs.hidden = true,
                9 => self.current_attrs.strikethrough = true,
                22 => {
                    self.current_attrs.bold = false;
                    self.current_attrs.dim = false;
                }
                23 => self.current_attrs.italic = false,
                24 => self.current_attrs.underline = false,
                27 => self.current_attrs.inverse = false,
                28 => self.current_attrs.hidden = false,
                29 => self.current_attrs.strikethrough = false,
                30..=37 => {
                    self.current_attrs.foreground = Color::Indexed((codes[index] - 30) as u8)
                }
                39 => self.current_attrs.foreground = Color::Default,
                40..=47 => {
                    self.current_attrs.background = Color::Indexed((codes[index] - 40) as u8)
                }
                49 => self.current_attrs.background = Color::Default,
                90..=97 => {
                    self.current_attrs.foreground = Color::Indexed((codes[index] - 90 + 8) as u8);
                }
                100..=107 => {
                    self.current_attrs.background = Color::Indexed((codes[index] - 100 + 8) as u8);
                }
                38 | 48 => {
                    if let Some((color, consumed)) = parse_extended_color(&codes[index..]) {
                        if codes[index] == 38 {
                            self.current_attrs.foreground = color;
                        } else {
                            self.current_attrs.background = color;
                        }
                        index += consumed - 1;
                    }
                }
                _ => {}
            }

            index += 1;
        }
    }

    pub(super) fn set_cursor_mode(&mut self, params: &Params, intermediates: &[u8], action: char) {
        if intermediates != b"?" {
            return;
        }

        for mode in private_mode_params(params) {
            match mode {
                1 => {
                    self.keyboard.application_cursor = action == 'h';
                }
                6 => {
                    // DECOM: toggling origin mode homes the cursor to the
                    // (region-relative when on, screen when off) origin.
                    self.origin_mode = action == 'h';
                    self.move_to_origin(1, 1);
                }
                25 => {
                    self.cursor_visible = action == 'h';
                    self.mark_dirty();
                }
                // DECSDM (sixel display mode): set = cursor stays, reset =
                // cursor moves below the image.
                80 => {
                    self.sixel_display_mode = action == 'h';
                }
                // Alternate-screen modes per xterm ctlseqs:
                // 47: plain switch (no cursor save, no clear on enter or leave).
                // 1047: switch; clear alt on leave (not on enter).
                // 1048: DECSC/DECRC only (cursor save/restore, no switch).
                // 1049: DECSC + switch + clear on enter / switch + DECRC on
                //        leave. Equivalent to 1048h + 1047h on set, and
                //        1047l + 1048l on reset.
                47 => {
                    if action == 'h' {
                        self.enter_alternate_screen(false);
                    } else {
                        self.leave_alternate_screen(false, false);
                    }
                }
                1047 => {
                    if action == 'h' {
                        self.enter_alternate_screen(false);
                    } else {
                        self.leave_alternate_screen(false, true);
                    }
                }
                1048 => {
                    if action == 'h' {
                        self.save_cursor();
                    } else {
                        self.restore_cursor();
                    }
                }
                1049 => {
                    if action == 'h' {
                        self.save_cursor();
                        self.enter_alternate_screen(true);
                    } else {
                        self.leave_alternate_screen(true, false);
                        self.restore_cursor();
                    }
                }
                2004 => {
                    self.bracketed_paste = action == 'h';
                }
                // Mouse tracking modes (single active mode; later DECSET wins,
                // any DECRST returns to Off). See `MouseTracking`.
                9 => self.set_mouse_tracking(MouseTracking::X10, action),
                1000 => self.set_mouse_tracking(MouseTracking::Normal, action),
                1002 => self.set_mouse_tracking(MouseTracking::ButtonEvent, action),
                1003 => self.set_mouse_tracking(MouseTracking::AnyEvent, action),
                // 1004: focus reporting. DECSET enables, DECRST disables.
                1004 => self.focus_reporting = action == 'h',
                // Mouse encoding extensions (single active encoding; later
                // DECSET wins, any DECRST returns to Default). See `MouseEncoding`.
                1005 => self.set_mouse_encoding(MouseEncoding::Utf8, action),
                1006 => self.set_mouse_encoding(MouseEncoding::Sgr, action),
                1015 => self.set_mouse_encoding(MouseEncoding::Urxvt, action),
                _ => {}
            }
        }
    }

    pub(super) fn set_mouse_tracking(&mut self, mode: MouseTracking, action: char) {
        self.mouse.tracking = if action == 'h' {
            mode
        } else {
            MouseTracking::Off
        };
    }

    pub(super) fn set_mouse_encoding(&mut self, encoding: MouseEncoding, action: char) {
        self.mouse.encoding = if action == 'h' {
            encoding
        } else {
            MouseEncoding::Default
        };
    }

    pub(super) fn device_attributes(&mut self, params: &Params, intermediates: &[u8]) {
        if intermediates.is_empty() && param_or(params, 0, 0) == 0 {
            self.host_output.extend_from_slice(b"\x1b[?1;2c");
        }
    }

    /// DSR (ESC [ Ps n): answer the host status queries that line editors rely
    /// on. `5n` reports "terminal OK" (`ESC [ 0 n`); `6n` reports the cursor
    /// position as `ESC [ row ; col R` (1-based). Shells such as fish issue
    /// `6n` to locate the cursor while drawing the completion pager and
    /// multi-line prompts; without a reply their screen model desyncs and
    /// completion listings render in the wrong place or fail to refresh. The
    /// reported row honors origin mode (DECOM): when set, it is relative to the
    /// scroll-region top, mirroring [`Screen::move_to_origin`]. Private-marker
    /// (DECDSR, `?`-intermediate) requests are ignored here.
    pub(super) fn device_status_report(&mut self, params: &Params, intermediates: &[u8]) {
        if !intermediates.is_empty() {
            return;
        }
        match param_or(params, 0, 0) {
            5 => self.host_output.extend_from_slice(b"\x1b[0n"),
            6 => {
                let row = if self.origin_mode {
                    let (top, _) = self.effective_region();
                    self.cursor.row.saturating_sub(top) + 1
                } else {
                    self.cursor.row + 1
                };
                let column = self.cursor.column + 1;
                self.host_output
                    .extend_from_slice(format!("\x1b[{row};{column}R").as_bytes());
            }
            _ => {}
        }
    }

    pub(super) fn save_cursor(&mut self) {
        self.saved_cursor = Some(SavedCursor {
            position: self.cursor,
            pending_wrap: self.pending_wrap,
        });
    }

    pub(super) fn restore_cursor(&mut self) {
        if let Some(saved_cursor) = self.saved_cursor {
            self.cursor = Position {
                row: saved_cursor.position.row.min(self.dimensions.rows - 1),
                column: saved_cursor
                    .position
                    .column
                    .min(self.dimensions.columns - 1),
            };
            self.pending_wrap = saved_cursor.pending_wrap;
            self.mark_dirty();
        }
    }

    /// Enter the alternate screen buffer.
    ///
    /// `clear_alt`: when true (mode 1049), the new alt buffer starts blank and
    ///   the cursor homes to (0,0). When false (modes 47/1047), the alt buffer
    ///   starts blank on first entry but its content persists across re-entries
    ///   (only mode 47 — 1047 clears on *leave*; the re-entry path is a no-op
    ///   regardless).
    pub(super) fn enter_alternate_screen(&mut self, clear_alt: bool) {
        if self.primary_screen.is_some() {
            return;
        }

        let alt_rows = if clear_alt {
            vec![blank_row(self.dimensions.columns); self.dimensions.rows]
        } else {
            vec![blank_row(self.dimensions.columns); self.dimensions.rows]
        };

        let primary_screen = StoredScreen {
            rows: std::mem::replace(&mut self.rows, alt_rows),
            scrollback: std::mem::replace(&mut self.scrollback, Scrollback::new()),
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            pending_wrap: self.pending_wrap,
            saved_cursor: self.saved_cursor,
            scroll_region: self.scroll_region,
            origin_mode: self.origin_mode,
            current_attrs: self.current_attrs,
            active_hyperlink: self.active_hyperlink,
        };

        if clear_alt {
            self.cursor = Position::default();
            self.pending_wrap = false;
        }
        self.saved_cursor = None;
        self.scroll_region = None;
        self.origin_mode = false;
        self.primary_screen = Some(primary_screen);
        self.graphics.enter_alternate(clear_alt);
        self.mark_dirty();
    }

    /// Leave the alternate screen and restore the primary buffer.
    ///
    /// `restore_cursor`: when true (mode 1049), the cursor position/wrap saved
    ///   at enter time is restored. When false (modes 47/1047), the cursor
    ///   retains its current position (clamped to screen bounds).
    ///
    /// `clear_alt`: when true (mode 1047), the alt buffer is cleared before
    ///   restoring primary (so stale alt content cannot leak if re-entered via
    ///   mode 47). When false (modes 47/1049), no extra clear is performed
    ///   (1049 already cleared on enter; 47 intentionally preserves content).
    pub(super) fn leave_alternate_screen(&mut self, restore_cursor: bool, _clear_alt: bool) {
        if let Some(primary_screen) = self.primary_screen.take() {
            self.rows = primary_screen.rows;
            self.scrollback = primary_screen.scrollback;
            if restore_cursor {
                self.cursor = Position {
                    row: primary_screen.cursor.row.min(self.dimensions.rows - 1),
                    column: primary_screen
                        .cursor
                        .column
                        .min(self.dimensions.columns - 1),
                };
                self.pending_wrap = primary_screen.pending_wrap;
                self.cursor_visible = primary_screen.cursor_visible;
                self.current_attrs = primary_screen.current_attrs;
                self.active_hyperlink = primary_screen.active_hyperlink;
            } else {
                // Modes 47/1047: cursor stays where it is (clamped).
                self.cursor.row = self.cursor.row.min(self.dimensions.rows - 1);
                self.cursor.column = self.cursor.column.min(self.dimensions.columns - 1);
                // Still restore visibility and attrs since those are screen-level
                // state that belongs to the primary, not the alt-screen app.
                self.cursor_visible = primary_screen.cursor_visible;
                self.current_attrs = primary_screen.current_attrs;
                self.active_hyperlink = primary_screen.active_hyperlink;
            }
            self.saved_cursor = primary_screen.saved_cursor;
            self.scroll_region = primary_screen.scroll_region;
            self.origin_mode = primary_screen.origin_mode;
            self.graphics.leave_alternate();
            self.mark_dirty();
        }
    }

    pub(super) fn set_scroll_region(&mut self, params: &Params) {
        let top = param_or(params, 0, 1).saturating_sub(1);
        let bottom = param_or(params, 1, self.dimensions.rows).saturating_sub(1);

        self.scroll_region = if top < bottom && bottom < self.dimensions.rows {
            Some(ScrollRegion { top, bottom })
        } else {
            None
        };

        // DECSTBM homes the cursor: to the region top-left when origin mode is
        // on, otherwise to the screen top-left (consistent with prior behavior).
        self.move_to_origin(1, 1);
        self.mark_dirty();
    }

    /// RIS (ESC c): hard reset. Returns the terminal to its power-on state —
    /// exits the alternate screen, clears the visible grid and scrollback,
    /// drops saved cursor / scroll region, resets attributes, cursor
    /// visibility, bracketed paste and pending wrap, homes the cursor, and
    /// discards any pending host output.
    pub(super) fn hard_reset(&mut self) {
        self.primary_screen = None;
        self.rows = vec![blank_row(self.dimensions.columns); self.dimensions.rows];
        self.scrollback.clear();
        self.cursor = Position::default();
        self.cursor_visible = true;
        self.pending_wrap = false;
        self.saved_cursor = None;
        self.scroll_region = None;
        self.origin_mode = false;
        self.bracketed_paste = false;
        self.current_attrs = Attrs::default();
        self.active_hyperlink = None;
        self.host_output.clear();
        self.last_graphic_char = None;
        self.graphics.hard_reset();
        self.hyperlinks.clear();
        self.dcs_capture = None;
        self.graphics_stats = GraphicsStats::default();
        // RIS returns mouse reporting to its power-on (off) state. The title is
        // a persistent window property and is intentionally left untouched.
        self.mouse = MouseProtocol::default();
        self.keyboard = KeyboardModes::default();
        self.focus_reporting = false;
        // RIS returns the cursor shape/blink to the host default policy.
        self.cursor_style = self.default_cursor_style;
        self.cursor_blink = self.default_cursor_blink;
        // RIS restores the default every-8 tab stops (DECSTR does not — see
        // soft_reset).
        self.tab_stops = default_tab_stops(self.dimensions.columns);
        self.sixel_display_mode = false;
        self.mark_dirty();
    }

    /// DECSTR (CSI ! p): soft reset. Resets modes and cursor state without
    /// touching the visible cells or scrollback. Cursor policy: homed to the
    /// top-left (documented in tests), matching xterm's DECSTR behaviour.
    /// Tab stops are deliberately PRESERVED — per the VT220 soft-reset
    /// definition, DECSTR does not clear tab stops; only RIS does.
    pub(super) fn soft_reset(&mut self) {
        self.cursor = Position::default();
        self.cursor_visible = true;
        self.pending_wrap = false;
        self.saved_cursor = None;
        self.scroll_region = None;
        self.origin_mode = false;
        self.bracketed_paste = false;
        self.keyboard = KeyboardModes::default();
        self.current_attrs = Attrs::default();
        self.active_hyperlink = None;
        self.host_output.clear();
        self.last_graphic_char = None;
        // DECSTR returns the cursor shape/blink to the host default policy.
        self.cursor_style = self.default_cursor_style;
        self.cursor_blink = self.default_cursor_blink;
        self.sixel_display_mode = false;
        self.mark_dirty();
    }
}

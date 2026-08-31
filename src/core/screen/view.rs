// SPDX-License-Identifier: GPL-3.0-only
//! Screen snapshots, persisted-state projection, search, and presentation queries.

use super::*;

impl Screen {
    pub fn dimensions(&self) -> Dimensions {
        self.dimensions
    }

    pub fn cursor(&self) -> Position {
        self.cursor
    }

    /// Pending host-bound responses (DA/DSR replies) accumulated by dispatch.
    /// Exposed within the crate so parser golden fixtures can include host
    /// output. Test-only.
    #[cfg(test)]
    pub(crate) fn host_output_bytes(&self) -> &[u8] {
        &self.host_output
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.physical_len(self.dimensions.columns)
    }

    /// Heap bytes the visible grid occupies, for memory attribution.
    ///
    /// Includes the stored primary buffer when the alternate screen is active:
    /// that buffer is retained, so omitting it would under-report an alt-screen
    /// pane and push a cost this project controls into the unaccounted
    /// remainder.
    pub fn grid_bytes(&self) -> u64 {
        let stored = self
            .primary_screen
            .as_ref()
            .map_or(0, |primary| rows_bytes(&primary.rows));
        rows_bytes(&self.rows).saturating_add(stored)
    }

    /// Heap bytes scrollback occupies across the active buffer and the stored
    /// primary buffer, for memory attribution, split into the logical-line ring
    /// and its memoized physical projection; see [`crate::core::scrollback`].
    ///
    /// The stored primary buffer is included for the same reason
    /// [`Screen::grid_bytes`] includes it: while the alternate screen is active
    /// that scrollback is still resident, and omitting it would push a cost
    /// this project controls into the unaccounted remainder.
    pub fn scrollback_bytes(&self) -> ScrollbackBytes {
        let stored = self
            .primary_screen
            .as_ref()
            .map_or(ScrollbackBytes::default(), |primary| {
                primary.scrollback.stored_bytes()
            });
        self.scrollback.stored_bytes().saturating_add(stored)
    }

    /// Decoded bytes held by this screen's terminal-graphics image store, for
    /// memory attribution. Zero for a session that has never received a
    /// graphics-protocol image.
    pub fn graphics_store_bytes(&self) -> u64 {
        self.graphics.store().decoded_bytes() as u64
    }

    /// Monotonic notice that the absolute-row origin moved because retained
    /// history was removed from the front. Include an off-screen stored primary
    /// buffer so trimming it while the alternate screen is active is observed.
    pub fn scrollback_trim_epoch(&self) -> u64 {
        let stored = self
            .primary_screen
            .as_ref()
            .map_or(0, |primary| primary.scrollback.trim_epoch());
        self.scrollback.trim_epoch().wrapping_add(stored)
    }

    pub fn cell(&self, row: usize, column: usize) -> Option<Cell> {
        self.rows
            .get(row)
            .and_then(|line| line.get(column))
            .copied()
    }

    /// Resize the grid to `columns` × `rows`, preserving content.
    ///
    /// The active **primary** screen reflows: soft-wrapped physical rows are
    /// rejoined into logical lines (using each row's [`Line::wrapped`] marker),
    /// then re-wrapped to the new width across the combined scrollback + visible
    /// buffer. This means text that wraps off a narrowed window is recoverable
    /// when it is widened again, rather than being truncated at the right edge.
    ///
    /// The **alternate** screen does not reflow: full-screen TUI applications
    /// own their layout and repaint on resize (`SIGWINCH`), so the alternate
    /// grid is simply truncated/padded to the new size. The stored primary
    /// screen behind it is still reflowed so leaving the alternate screen after
    /// a resize is coherent. Alternate-screen isolation and the no-scrollback
    /// rule for the alternate buffer are preserved.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let dimensions = Dimensions::new(columns, rows);
        let width_unchanged = dimensions.columns == self.dimensions.columns;
        // A resize re-anchors everything; an open button run's absolute start
        // row is meaningless afterwards, so abandon it (refcounts for stamped
        // spans are rebuilt below).
        self.cancel_button_run();
        // DECSTBM margins are absolute row indices in the old grid. A row-count
        // change invalidates them on both the active and stored-primary screen;
        // a width-only reflow leaves them meaningful.
        let rows_changed = dimensions.rows != self.dimensions.rows;
        // A resize re-wraps/re-anchors marks, so absolute-row mark positions can
        // shift; flag the change for the poll API (only when marks exist).
        let had_prompt_marks = self.has_any_prompt_mark();

        // Capture the trace inputs BEFORE any mutation (old dims, incoming
        // cursor + pending-wrap, and the load-bearing discriminator). Cheap
        // plain copies; only formatted/written when ODYTTY_REFLOW_TRACE is on.
        let trace_old_cols = self.dimensions.columns;
        let trace_old_rows = self.dimensions.rows;
        let trace_output_since_last_resize = self.output_since_last_resize;
        let trace_alt_screen_active = self.primary_screen.is_some();
        let trace_cursor_in = self.cursor;
        let trace_pending_in = self.pending_wrap;

        if self.primary_screen.is_some() {
            // Alternate screen active: truncate/pad the app-managed grid (it
            // repaints), but never feed the alternate buffer into scrollback
            // (the alternate screen keeps none).
            let mut discard = Vec::new();
            resize_buffer_rows(&mut self.rows, &mut discard, dimensions, true);
            self.cursor.row = self.cursor.row.min(dimensions.rows - 1);
            self.cursor.column = self.cursor.column.min(dimensions.columns - 1);

            if let Some(mut primary) = self.primary_screen.take() {
                // The stored primary shares the (old) width, so the same
                // width-unchanged decision applies.
                let result = resize_lazy_with_options(
                    &mut primary.scrollback,
                    &mut primary.rows,
                    dimensions,
                    primary.cursor,
                    width_unchanged,
                    ResizeOptions {
                        preserve_cursor_physical_line: false,
                        cursor_pending_wrap: primary.pending_wrap,
                        collapse_prompt_start_row: None,
                        // The override never fires for the stored primary
                        // (preserve_cursor_physical_line is false here), so the
                        // discriminator is inert; pass false.
                        repaint_expected: false,
                        // Defer cursor placement to the shell on a backend that
                        // repaints absolutely (ConPTY). preserve is already
                        // false here, so this just keeps the stored-primary
                        // cursor at its incoming position clamped to new dims.
                        shell_owns_cursor_on_resize: self.shell_owns_cursor_on_resize,
                    },
                );
                primary.cursor = result.cursor;
                primary.pending_wrap = result.pending_wrap;
                if rows_changed {
                    primary.scroll_region = None;
                }
                self.primary_screen = Some(primary);
            }
        } else {
            let old_scrollback_rows = self.scrollback.physical_len(self.dimensions.columns);
            let collapse_prompt_start_row = active_prompt_start_visible_row(
                self.active_prompt_start,
                old_scrollback_rows,
                self.cursor.row,
                self.rows.len(),
                width_unchanged,
            );
            // Lazy resize: re-wrap only the bottom of the buffer needed for the
            // new window; deep history stays logical and is projected on access.
            // The width-unchanged path uses the O(rows) keep-width fast path
            // (preserving P1-a).
            let result = resize_lazy_with_options(
                &mut self.scrollback,
                &mut self.rows,
                dimensions,
                self.cursor,
                width_unchanged,
                ResizeOptions {
                    preserve_cursor_physical_line: !width_unchanged,
                    cursor_pending_wrap: self.pending_wrap,
                    collapse_prompt_start_row,
                    repaint_expected: self.output_since_last_resize,
                    shell_owns_cursor_on_resize: self.shell_owns_cursor_on_resize,
                },
            );
            self.cursor = result.cursor;
            self.pending_wrap = result.pending_wrap;
            if let Some(row) = result.collapsed_prompt_start_row {
                let scrollback_rows = self.scrollback.physical_len(dimensions.columns);
                if let Some(start) = self.active_prompt_start.as_mut() {
                    start.absolute_row = scrollback_rows + row;
                }
            }
            // Re-anchor the OSC 133 `B` input-start mark through the resize.
            // `active_prompt_input_start` caches an ABSOLUTE row
            // (`physical_len(old_columns) + cursor.row`) captured when `B`
            // arrived. A width change rewraps scrollback to a different
            // `physical_len`, so the cached row no longer equals the LIVE
            // `scrollback_len + cursor.row` the consumer gate
            // (`editable_input_selection_for_context_menu`) compares against —
            // which silently disables prompt-aware select+Delete after a
            // side-by-side split until the next prompt re-emits `A`/`B`. The
            // input line is the cursor's logical line while editing, and the
            // resize repositions the cursor faithfully, so recompute the anchor
            // from the cursor's current logical-line start at the new width.
            //
            // Semantics (decision gate → reflow-preserved mark + cursor line):
            // only re-anchor when the cursor still sits on a prompt-marked
            // logical line (marks travel with their logical line through reflow).
            // If the cursor has moved off the prompt (e.g. output without a `C`),
            // the anchor is left as-is so the gate still declines — matching
            // today's no-resize behavior. The column is preserved: the prompt
            // prefix is unchanged by a rewrap of a single short input line, and
            // `start = selected_start.max(input_column)` keeps bounding the
            // editable region. Width-unchanged resizes keep `physical_len`
            // constant, so the cached row stays valid and we skip the recompute
            // (byte-identical fast path).
            if !width_unchanged && let Some((_, input_column)) = self.active_prompt_input_start {
                let mut row = self.cursor.row.min(self.rows.len().saturating_sub(1));
                while row > 0 && self.rows[row - 1].wrapped {
                    row -= 1;
                }
                if self
                    .rows
                    .get(row)
                    .and_then(|line| line.prompt_mark)
                    .is_some()
                {
                    let scrollback_rows = self.scrollback.physical_len(dimensions.columns);
                    self.active_prompt_input_start = Some((scrollback_rows + row, input_column));
                }
            }
        }

        self.dimensions = dimensions;
        if self.primary_screen.is_some() {
            self.pending_wrap = false;
        }
        // Reset the repaint discriminator: any output that arrives AFTER this
        // resize re-arms the override for the NEXT resize. Back-to-back resizes
        // with no intervening output therefore see `false` and skip the override
        // (the no-repaint case that would otherwise ratchet the cursor).
        self.output_since_last_resize = false;
        self.resize_tab_stops(dimensions.columns);
        if rows_changed {
            self.scroll_region = None;
        }
        self.graphics
            .resize(self.dimensions.rows, self.dimensions.columns);
        self.prompt_marks_changed |= had_prompt_marks;
        // Reflow re-projected button spans wholesale (splits/merges change the
        // span count); replace the incremental refcount bookkeeping with an
        // authoritative rebuild. No-op when no buttons exist.
        self.rebuild_button_refcounts();
        self.mark_dirty();

        // Passive, env-gated diagnostic (no-op unless ODYTTY_REFLOW_TRACE is
        // set). Emits one line capturing how this resize moved the cursor and,
        // critically, whether output had arrived since the previous resize.
        trace_resize(&ResizeTrace {
            old_cols: trace_old_cols,
            old_rows: trace_old_rows,
            new_cols: dimensions.columns,
            new_rows: dimensions.rows,
            width_unchanged,
            output_since_last_resize: trace_output_since_last_resize,
            alt_screen_active: trace_alt_screen_active,
            shell_owns_cursor_on_resize: self.shell_owns_cursor_on_resize,
            cursor_in_row: trace_cursor_in.row,
            cursor_in_col: trace_cursor_in.column,
            pending_wrap_in: trace_pending_in,
            cursor_out_row: self.cursor.row,
            cursor_out_col: self.cursor.column,
            pending_wrap_out: self.pending_wrap,
        });
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            dimensions: self.dimensions,
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            colors: self.dynamic_colors.clone(),
            cells: self
                .rows
                .iter()
                .flat_map(|line| line.iter())
                .copied()
                .collect(),
        }
    }

    /// Copy the constrained Phase 2 persistence subset into owned DTOs.
    ///
    /// This is intentionally separate from the render [`Snapshot`] surface:
    /// scrollback rows and mode state are needed for resumable sessions, but
    /// private `Screen` / `Scrollback` storage remains behind this owned copy.
    pub fn snapshot_state(&self, max_scrollback_rows: usize) -> SnapshotTerminalState {
        let columns = self.dimensions.columns;
        // Only the trailing `max_scrollback_rows` are persisted, so only they
        // are projected.
        let scrollback_rows = self
            .scrollback
            .physical_tail(columns, max_scrollback_rows)
            .iter()
            .map(|row| snapshot_row_from_line(row, columns))
            .collect();
        let visible_rows = self
            .rows
            .iter()
            .map(|row| snapshot_row_from_line(row, columns))
            .collect();

        SnapshotTerminalState {
            dimensions: self.dimensions,
            cursor: self.cursor,
            cursor_visible: self.cursor_visible,
            cursor_style: self.cursor_style,
            cursor_blink: self.cursor_blink,
            basic_modes: SnapshotBasicModes {
                bracketed_paste: self.bracketed_paste,
                alternate_scroll: self.alternate_scroll,
                alternate_screen: self.primary_screen.is_some(),
                synchronized_output: self.synchronized_output,
                focus_reporting: self.focus_reporting,
                mouse: self.mouse,
                keyboard: self.keyboard,
                charsets: self.charsets,
            },
            scrollback_rows,
            visible_rows,
        }
    }

    /// Copy layout-affecting terminal state that is not part of the render
    /// [`Snapshot`] surface.
    pub fn snapshot_layout_state(&self) -> SnapshotLayoutState {
        SnapshotLayoutState {
            scroll_region: self.scroll_region.map(|region| SnapshotScrollRegion {
                top: region.top,
                bottom: region.bottom,
            }),
            tab_stops: self.tab_stops.clone(),
        }
    }

    /// Restore this screen from an owned Phase 2 snapshot envelope.
    ///
    /// The envelope carries active-buffer terminal state only. When it records
    /// `alternate_screen = true`, the active alternate grid is restored and a
    /// blank primary placeholder is installed so the mode bit remains true; the
    /// stored primary buffer is not present in v2 snapshots.
    pub fn restore_from_envelope(
        &mut self,
        envelope: &SnapshotEnvelope,
    ) -> Result<(), SnapshotEnvelopeError> {
        restore_validate_terminal_state(&envelope.terminal)?;
        envelope.layout.validate(envelope.terminal.dimensions)?;

        let columns = envelope.terminal.dimensions.columns;
        let mut scrollback_rows =
            restore_lines_from_snapshot_rows(&envelope.terminal.scrollback_rows, columns)?;
        let mut visible_rows =
            restore_lines_from_snapshot_rows(&envelope.terminal.visible_rows, columns)?;
        restore_apply_prompt_marks(
            &mut scrollback_rows,
            &mut visible_rows,
            &envelope.prompt_marks,
        )?;

        let mut restored = Screen::new(
            envelope.terminal.dimensions.columns,
            envelope.terminal.dimensions.rows,
        );
        restored.rows = visible_rows;
        restored.scrollback = Scrollback::from_physical_rows(&scrollback_rows);
        restored.cursor = envelope.terminal.cursor;
        restored.cursor_visible = envelope.terminal.cursor_visible;
        restored.cursor_style = envelope.terminal.cursor_style;
        restored.cursor_blink = envelope.terminal.cursor_blink;
        restored.bracketed_paste = envelope.terminal.basic_modes.bracketed_paste;
        restored.alternate_scroll = envelope.terminal.basic_modes.alternate_scroll;
        restored.synchronized_output = envelope.terminal.basic_modes.synchronized_output;
        restored.focus_reporting = envelope.terminal.basic_modes.focus_reporting;
        restored.mouse = envelope.terminal.basic_modes.mouse;
        restored.keyboard = envelope.terminal.basic_modes.keyboard;
        restored.charsets = envelope.terminal.basic_modes.charsets;
        restored.dynamic_colors = envelope.dynamic_colors.clone();
        restored.title = envelope.metadata.title.clone();
        restored.title_changed = envelope.metadata.title.is_some();
        restored.working_directory = envelope.metadata.working_directory.clone();
        restored.working_directory_changed = envelope.metadata.working_directory.is_some();
        restored.prompt_marks_changed = !envelope.prompt_marks.is_empty();
        restored.scroll_region = envelope.layout.scroll_region.map(|region| ScrollRegion {
            top: region.top,
            bottom: region.bottom,
        });
        restored.tab_stops = envelope.layout.tab_stops.clone();
        if envelope.terminal.basic_modes.alternate_screen {
            restored.primary_screen = Some(blank_stored_primary(restored.dimensions));
        }
        restored.mark_dirty();

        *self = restored;
        Ok(())
    }

    /// Produce a visible-grid snapshot at a scrollback viewport offset.
    ///
    /// `offset_rows` counts how many rows the viewport is paged *upward* into
    /// scrollback. Offset `0` is the live visible screen and is byte-for-byte
    /// identical to [`snapshot`](Self::snapshot). Positive offsets page upward;
    /// the offset is clamped to the available scrollback so callers cannot read
    /// past the oldest stored row.
    ///
    /// The composed buffer is `scrollback` (oldest→newest) followed by the live
    /// `rows`; the returned viewport is the `dimensions.rows`-tall window whose
    /// bottom edge sits `offset_rows` above the live bottom. Each emitted row is
    /// normalized to `dimensions.columns` so the `cells` length always equals
    /// `dimensions.rows * dimensions.columns`.
    ///
    /// Cursor policy: at offset `0` the live cursor and its visibility carry
    /// through unchanged; for any nonzero (scrolled-back) offset the cursor is
    /// hidden (`cursor_visible == false`) because it does not belong to the
    /// historical viewport. The cursor position is reported unchanged.
    ///
    /// Alternate-screen isolation is preserved for free: entering the alternate
    /// screen moves the primary scrollback into off-screen storage, so an
    /// alternate-screen `Screen` has empty scrollback and every offset clamps to
    /// the live grid — primary history never leaks into alternate snapshots.
    pub fn snapshot_with_scrollback(&self, offset_rows: usize) -> Snapshot {
        let height = self.dimensions.rows;
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.physical_len(columns);
        let offset = offset_rows.min(scrollback_len);

        if offset == 0 {
            return self.snapshot();
        }

        // The viewport is `height` rows of the combined scrollback ++ live
        // buffer whose bottom edge sits `offset` rows above the live bottom,
        // so its window over that buffer starts at `scrollback_len - offset`.
        // Skipping that many rows of `scrollback ++ live` is the same sequence
        // as the last `offset` scrollback rows followed by the live grid, which
        // is why only the tail has to be projected.
        let tail = self.scrollback.physical_tail(columns, offset);

        let mut cells = Vec::with_capacity(height * columns);
        for row in tail.iter().chain(self.rows.iter()).take(height) {
            for column in 0..columns {
                cells.push(row.get(column).copied().unwrap_or_else(Cell::blank));
            }
        }

        Snapshot {
            dimensions: self.dimensions,
            cursor: self.cursor,
            cursor_visible: false,
            colors: self.dynamic_colors.clone(),
            cells,
        }
    }

    /// Terminal-owned graphics state. Render integration is intentionally
    /// separate; this lets tests and future render work inspect placements
    /// without changing the text [`Snapshot`] surface.
    pub fn graphics(&self) -> &ImageScene {
        &self.graphics
    }

    pub fn graphics_mut(&mut self) -> &mut ImageScene {
        &mut self.graphics
    }

    /// Graphics placements visible in the current viewport. `offset_rows`
    /// follows [`Self::snapshot_with_scrollback`]: `0` is the live screen,
    /// positive values page upward into scrollback.
    ///
    /// Two sources are merged. Real placements are cell-anchored scene records
    /// projected onto the viewport. Kitty Unicode placeholders (`U=1`) have no
    /// scene record: they are resolved here by reading the placeholder cells out
    /// of the same viewport the snapshot draws, which is what makes them scroll,
    /// reflow, and erase with the text carrying them at no bookkeeping cost.
    /// The merged list keeps the `(z_index, generation)` paint order the render
    /// layer relies on.
    ///
    /// Gate-scoped: the placeholder scan runs only when a virtual placement
    /// exists. Without one no placeholder cell could resolve to anything, so a
    /// session that never uses the feature does zero extra work per frame and
    /// its frames stay byte-identical.
    pub fn visible_graphics(&self, offset_rows: usize) -> Vec<VisiblePlacement> {
        let mut placements = self.graphics.visible_placements(
            offset_rows,
            self.dimensions.rows,
            self.dimensions.columns,
            self.cell_metrics.height_px,
        );
        if self.graphics.has_virtual_placements() {
            self.collect_placeholder_placements(offset_rows, &mut placements);
            placements.sort_by_key(|placement| (placement.z_index, placement.generation));
        }
        placements
    }

    /// Clock reading at which a visible animated image needs its next frame, or
    /// `None` when nothing visible is animating (Kitty `a=f`/`a=a`).
    ///
    /// Gate-scoped exactly like the placeholder scan: with no animated image in
    /// the store this returns before computing the visible set, so a session
    /// that never sends a frame command pays nothing per frame and schedules no
    /// wake. Images shown through Unicode placeholders count as visible, because
    /// the placeholder resolution feeds the same visible list.
    pub fn graphics_animation_deadline_ms(&self, offset_rows: usize) -> Option<u64> {
        if !self.graphics.has_animations() {
            return None;
        }
        let visible = self.visible_graphics(offset_rows);
        self.graphics.next_animation_deadline_ms(&visible)
    }

    /// Advance visible animations to the frame due at `now_ms`, where `now_ms`
    /// is a monotonic millisecond reading supplied by the caller (the core keeps
    /// no clock of its own). Returns whether displayed pixels changed; a change
    /// marks the screen dirty, so the frame gate repaints and the renderer
    /// re-uploads the image whose generation moved.
    pub fn advance_graphics_animations(&mut self, now_ms: u64, offset_rows: usize) -> bool {
        if !self.graphics.has_animations() {
            return false;
        }
        let visible = self.visible_graphics(offset_rows);
        if !self.graphics.advance_animations(now_ms, &visible) {
            return false;
        }
        self.mark_dirty();
        true
    }

    /// Resolve Unicode-placeholder cells in the viewport into placements.
    ///
    /// The row walk mirrors [`Self::visible_button_spans`] exactly — same
    /// `scrollback ++ live` window as [`Self::snapshot_with_scrollback`] — so
    /// the placements it emits line up with the cells the matching snapshot
    /// draws, by construction rather than by coincidence.
    fn collect_placeholder_placements(&self, offset_rows: usize, out: &mut Vec<VisiblePlacement>) {
        let height = self.dimensions.rows;
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.physical_len(columns);
        let offset = offset_rows.min(scrollback_len);

        if offset == 0 {
            for (row, line) in self.rows.iter().enumerate() {
                placeholder::collect_row_placeholders(&self.graphics, row, &line.cells, out);
            }
        } else {
            // Same tail window as `snapshot_with_scrollback`, for the same
            // reason: the walks have to agree row for row.
            let tail = self.scrollback.physical_tail(columns, offset);
            for (row, line) in tail.iter().chain(self.rows.iter()).take(height).enumerate() {
                placeholder::collect_row_placeholders(&self.graphics, row, &line.cells, out);
            }
        }
    }

    /// Buttons visible in the current viewport, projected onto viewport rows
    /// for rendering (Button Protocol B2). `offset_rows` follows
    /// [`Self::snapshot_with_scrollback`]: `0` is the live screen, positive
    /// values page upward into scrollback. Row and column coordinates line up
    /// with the cells the matching [`Snapshot`] draws, so the render layer can
    /// index straight into its flattened cell grid.
    ///
    /// Gate-scoped: when the button protocol is off (the default) this returns
    /// an empty vector immediately — no row walk, no table lookups, no
    /// allocation — so the render path does zero extra work and frames stay
    /// byte-identical. The button table is empty on that path anyway; the gate
    /// makes the zero-work guarantee explicit rather than incidental.
    pub fn visible_button_spans(&self, offset_rows: usize) -> Vec<SnapshotButton> {
        if !self.buttons_enabled {
            return Vec::new();
        }
        let height = self.dimensions.rows;
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.physical_len(columns);
        let offset = offset_rows.min(scrollback_len);

        let mut out = Vec::new();
        if offset == 0 {
            // Live viewport: the visible rows are `self.rows` verbatim, so their
            // button spans are already row-local and viewport-aligned.
            for (row, line) in self.rows.iter().enumerate() {
                self.collect_row_buttons(row, line, &mut out);
            }
        } else {
            // Scrolled viewport: the same `scrollback ++ live` window
            // `snapshot_with_scrollback` draws. Physical scrollback rows carry
            // button spans re-projected to row-local columns by the reflow
            // projection, so they align with the drawn cells here too.
            let tail = self.scrollback.physical_tail(columns, offset);
            for (row, line) in tail.iter().chain(self.rows.iter()).take(height).enumerate() {
                self.collect_row_buttons(row, line, &mut out);
            }
        }
        out
    }

    /// Resolve a line's button spans against the interned table and push the
    /// live entries onto `out` at viewport `row`. A span whose entry has been
    /// removed is skipped (defensive; canonical storage and the table stay in
    /// refcount lockstep, so this should not arise in practice).
    ///
    /// A Tier 1 point span (`len == 0`) is resolved to its chip rect here —
    /// the same [`point_chip_rect`] the pointer hit-test uses — so what the
    /// render layer paints and what [`Self::button_at`] resolves are the same
    /// cells by construction. A point chip with no room on its row is dropped.
    pub(super) fn collect_row_buttons(
        &self,
        row: usize,
        line: &Line,
        out: &mut Vec<SnapshotButton>,
    ) {
        let mut content_end: Option<usize> = None;
        for span in &line.button_spans {
            let Some(entry) = self.buttons.get(span.id) else {
                continue;
            };
            let (start_col, len, point) = if span.len > 0 {
                (span.start_col, span.len, false)
            } else {
                let end = *content_end.get_or_insert_with(|| line_content_end(&line.cells));
                let Some((start, len)) =
                    point_chip_rect(end, span.start_col, entry.code, self.dimensions.columns)
                else {
                    continue;
                };
                (start, len, true)
            };
            out.push(SnapshotButton {
                row,
                start_col,
                len,
                code: entry.code,
                icon: entry.icon,
                state: entry.state,
                point,
            });
        }
    }

    /// Number of sixel decode failures since power-on (debug diagnostic).
    pub fn sixel_decode_errors(&self) -> u64 {
        self.graphics_stats.sixel_decode_errors
    }

    pub fn plain_text(&self) -> String {
        self.rows
            .iter()
            .map(|row| {
                let mut line = String::new();
                for cell in row.iter().filter(|cell| !cell.wide_continuation) {
                    line.push(cell.ch);
                    for &mark in cell.combining() {
                        line.push(mark);
                    }
                }
                line.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    /// DECSET 1007 (alternate scroll mode) state. Default on; the host layer
    /// only acts on it while the alternate screen is active and the application
    /// is not tracking the mouse.
    pub fn alternate_scroll_enabled(&self) -> bool {
        self.alternate_scroll
    }

    /// Whether the alternate screen buffer is currently active (the primary
    /// buffer is stored). The alternate screen has no scrollback, so the host
    /// uses this to decide between scrollback movement and alternate-scroll
    /// cursor-key translation.
    pub fn on_alternate_screen(&self) -> bool {
        self.primary_screen.is_some()
    }

    /// The current window title, or `None` if no OSC 0/2 has set one. An
    /// explicit empty title is reported as `Some("")`.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Return whether the title changed since the last call and clear the flag.
    /// Lets a front end poll once per frame and update the OS window title only
    /// when it actually changed.
    pub fn take_title_changed(&mut self) -> bool {
        std::mem::take(&mut self.title_changed)
    }

    /// The current working directory reported via OSC 7, or `None` if none has
    /// been reported. The value is the percent-decoded path component of the
    /// last well-formed `file://host/path` URL (the host is validated then
    /// dropped). Advisory only — the core never touches the filesystem.
    pub fn current_working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    /// Return whether the working directory changed since the last call and
    /// clear the flag. Mirrors [`Screen::take_title_changed`] so a front end can
    /// poll once per frame and react (e.g. retitle a tab) only on change.
    pub fn take_working_directory_changed(&mut self) -> bool {
        std::mem::take(&mut self.working_directory_changed)
    }

    /// Set or clear the local hostname accepted by OSC 7. With `None`, OSC 7
    /// hostname behavior is byte-identical to the historical default: only empty
    /// host and `localhost` are accepted.
    pub fn set_local_hostname(&mut self, local_hostname: Option<String>) {
        self.local_hostname = local_hostname.filter(|host| !host.is_empty());
    }

    /// The OSC 133 prompt mark anchored to absolute row `row`, or `None` if the
    /// row carries no mark (SH1). The coordinate convention matches
    /// [`Screen::snapshot_with_scrollback`] and [`super::search`]: row `0` is the
    /// oldest physical scrollback row, counting down through scrollback into the
    /// live grid. Out-of-range rows return `None`. Advisory state only — nothing
    /// on the render path consults it; this is the sole reader of `prompt_mark`.
    pub fn prompt_mark_at(&self, row: usize) -> Option<PromptKind> {
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.physical_len(columns);
        if row < scrollback_len {
            self.scrollback.prompt_mark_at(columns, row)
        } else {
            self.rows
                .get(row - scrollback_len)
                .and_then(|l| l.prompt_mark)
        }
    }

    /// Every OSC 133 prompt mark in the buffer, as `(absolute_row, kind)` pairs
    /// in ascending row order. The coordinate convention matches
    /// [`Screen::prompt_mark_at`]: row `0` is the oldest physical scrollback
    /// row, counting down through scrollback into the live grid. Rows without a
    /// mark are skipped, so the result is the *set* of marked rows, not one
    /// entry per row.
    ///
    /// This is the enumeration counterpart to the point-query
    /// [`Screen::prompt_mark_at`]: a command-aware front end caches this `Vec`
    /// and rebuilds it only when [`Screen::take_prompt_marks_changed`] reports a
    /// change, rather than scanning every row each frame. Advisory read only —
    /// no mutation, no render-path effect, nothing reaches the
    /// [`super::types::Snapshot`]. When the alternate screen is active the active
    /// buffer carries no marks (they ride the stored primary), so this returns
    /// an empty `Vec`, consistent with [`Screen::prompt_mark_at`].
    pub fn prompt_marks(&self) -> Vec<(usize, PromptKind)> {
        let columns = self.dimensions.columns;
        // Served from the projection's cached shape: a mark rides a logical
        // line's first physical row, so the marked rows are exactly those
        // first-row indices and no row has to be materialized to find them.
        let mut marks = self.scrollback.prompt_mark_rows(columns);
        let base = self.scrollback.physical_len(columns);
        for (offset, line) in self.rows.iter().enumerate() {
            if let Some(kind) = line.prompt_mark {
                marks.push((base + offset, kind));
            }
        }
        marks
    }

    /// Return whether the set of prompt marks may have changed since the last
    /// call, and clear the flag. Mirrors [`Screen::take_working_directory_changed`]
    /// so a front end can poll once per frame and rebuild per-command UI only on
    /// change.
    ///
    /// The flag is set not only when a new mark is stamped but also whenever an
    /// operation can clear or reposition existing marks — RIS, erase-display /
    /// erase-line row replacement, resize/reflow, and alternate-screen
    /// enter/leave (which swaps the marked primary out and back) — so a consumer
    /// that trusts "rebuild only on change" never sees
    /// [`Screen::prompt_mark_at`] return a different result while this reads
    /// `false`. It is *conservative*: those
    /// clearing/repositioning paths only raise it when marks are actually
    /// present, but a raised flag does not guarantee a mark's value changed
    /// (e.g. a resize that left every mark on the same absolute row).
    pub fn take_prompt_marks_changed(&mut self) -> bool {
        std::mem::take(&mut self.prompt_marks_changed)
    }

    /// Whether any prompt mark is held anywhere in the terminal — the active
    /// screen's live rows or scrollback, *or* the stored primary screen when the
    /// alternate screen is active. Used to keep
    /// [`Self::take_prompt_marks_changed`] honest: the clear/reposition paths
    /// (RIS, resize) only raise the change flag when there is actually a mark to
    /// clear or move, and an alt-active resize re-anchors the stored primary's
    /// marks too, so they must be counted here.
    pub(super) fn has_any_prompt_mark(&self) -> bool {
        if self.rows.iter().any(|l| l.prompt_mark.is_some()) || self.scrollback.any_prompt_mark() {
            return true;
        }
        if let Some(primary) = &self.primary_screen {
            return primary.rows.iter().any(|l| l.prompt_mark.is_some())
                || primary.scrollback.any_prompt_mark();
        }
        false
    }

    /// The active mouse reporting protocol (tracking mode + encoding).
    pub fn mouse_protocol(&self) -> MouseProtocol {
        self.mouse
    }

    /// Keyboard modes that affect front-end key encoding.
    pub fn keyboard_modes(&self) -> KeyboardModes {
        self.keyboard
    }

    /// G0/G1 charset designations and the SO/SI GL selection.
    pub fn charset_modes(&self) -> CharsetModes {
        self.charsets
    }

    /// Whether DECSET 1004 focus reporting is enabled. When true, a front end
    /// should emit `ESC [ I` on focus gain and `ESC [ O` on focus loss (see
    /// [`encode_focus_event`]).
    pub fn focus_reporting(&self) -> bool {
        self.focus_reporting
    }

    /// Whether OSC 133 click-to-position (SH-CLICK) is currently enabled by the
    /// shell. Advisory: the native pointer layer reads this to decide whether a
    /// click on the live input region should synthesize cursor-key presses
    /// (F2); core never acts on it. Off until a `click_events=1` prompt
    /// attribute opts in.
    pub fn click_events_enabled(&self) -> bool {
        self.click_events_enabled
    }

    /// Master gate for the button protocol (Button Protocol B1). Off (the
    /// default), both button spellings are parsed-and-ignored: no table
    /// growth, no spans, no observable state — feature-off byte identity.
    /// Enforced at this OSC chokepoint; the future pointer arm gates
    /// independently on the same setting so no partial-gate hole exists.
    pub fn set_buttons_enabled(&mut self, enabled: bool) {
        self.buttons_enabled = enabled;
    }

    /// Sub-gate: accept the iTerm2 `OSC 1337 ; Button=` spelling (Tier 1).
    pub fn set_buttons_iterm_compat(&mut self, enabled: bool) {
        self.buttons_iterm_compat = enabled;
    }

    /// Sub-gate: honor `scope=sticky`; off downgrades definitions to block
    /// scope.
    pub fn set_buttons_sticky(&mut self, enabled: bool) {
        self.buttons_sticky = enabled;
    }

    /// Number of interned button entries (live + invalidated-but-referenced).
    pub fn button_entry_count(&self) -> usize {
        self.buttons.len()
    }

    /// Test window into the interned button table.
    #[cfg(test)]
    pub(in crate::core) fn button_table(&self) -> &ButtonTable {
        &self.buttons
    }

    /// Test window into a visible row's button spans (row-local columns).
    #[cfg(test)]
    pub(in crate::core) fn visible_row_button_spans(&self, row: usize) -> &[ButtonSpan] {
        self.rows
            .get(row)
            .map(|line| line.button_spans.as_slice())
            .unwrap_or(&[])
    }

    /// Test window into the scrollback store (logical-line span assertions).
    #[cfg(test)]
    pub(in crate::core) fn scrollback_store(&self) -> &Scrollback {
        &self.scrollback
    }

    /// Active OSC 133 `B` input-start boundary as `(absolute_row, column)`.
    /// Returns `None` before a cooperating shell reports `B`, after command
    /// output starts, or after reset. Advisory state only.
    pub fn active_prompt_input_start(&self) -> Option<(usize, usize)> {
        self.active_prompt_input_start
    }

    /// The live editable prompt-input region, derived in core from the OSC 133
    /// `B` mark, the soft-wrap flags, the cursor, and (when a cooperating
    /// shell emits the private edit-region OSC) the authoritative buffer
    /// geometry. `None` means no editable input is present — callers must
    /// no-op rather than guess. See [`crate::core::input_region`] for the model and
    /// the certainty gate.
    pub fn input_region(&self) -> Option<crate::core::input_region::InputRegion> {
        crate::core::input_region::derive_input_region(
            &self.rows,
            self.scrollback.physical_len(self.dimensions.columns),
            self.dimensions.columns,
            self.active_prompt_input_start,
            self.cursor,
            self.active_edit_region.as_ref(),
        )
    }

    pub fn hyperlink(&self, id: LinkId) -> Option<&Hyperlink> {
        self.hyperlinks.get(id)
    }

    #[cfg(test)]
    pub(in crate::core) fn hyperlink_count(&self) -> usize {
        self.hyperlinks.len()
    }

    /// Resolve the button (if any) under a visible viewport cell — the pointer
    /// arm's hit-test (Button Protocol B3).
    ///
    /// The master gate is enforced HERE as well as at the OSC arm, so turning
    /// `buttons` off kills clickability outright, not just new definitions —
    /// spans left in scrollback go inert immediately (the partial-gate hole
    /// class). The gate-off/button-free fast path is two branches, no row walk.
    ///
    /// `offset_rows`/`row`/`column` use the [`Self::visible_search_rows`]
    /// viewport convention: offset `0` is the live screen, positive offsets
    /// page upward into scrollback (clamped), `row 0` = top visible row.
    /// Alt-screen viewports resolve nothing (buttons are refused there and
    /// primary-screen spans must not be clickable through alt content).
    ///
    /// Hit box: a labeled span covers `[start_col, start_col + len)`; a Tier 1
    /// point button covers its resolved chip rect ([`point_chip_rect`], the
    /// same geometry the render layer paints), so the click target is exactly
    /// the pill the user sees — a chip with no room on its row is not
    /// clickable at all. Pure; never panics.
    pub fn button_at(&self, offset_rows: usize, row: usize, column: usize) -> Option<ButtonHit> {
        if !self.buttons_enabled || self.buttons.is_empty() || self.primary_screen.is_some() {
            return None;
        }
        if row >= self.dimensions.rows || column >= self.dimensions.columns {
            return None;
        }
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.physical_len(columns);
        // Same window as visible_search_rows / snapshot_with_scrollback.
        let offset = offset_rows.min(scrollback_len);
        let window_start = scrollback_len - offset;
        let row_index = window_start + row;
        // A point query projects the one logical line that owns this row.
        let scrollback_line;
        let line = if row_index < scrollback_len {
            scrollback_line = self.scrollback.physical_row(columns, row_index)?;
            &scrollback_line
        } else {
            self.rows.get(row_index - scrollback_len)?
        };
        let mut content_end: Option<usize> = None;
        for span in &line.button_spans {
            let Some(entry) = self.buttons.get(span.id) else {
                continue;
            };
            let (start_col, len) = if span.len > 0 {
                (span.start_col, span.len)
            } else {
                let end = *content_end.get_or_insert_with(|| line_content_end(&line.cells));
                match point_chip_rect(end, span.start_col, entry.code, self.dimensions.columns) {
                    Some(rect) => rect,
                    None => continue,
                }
            };
            if column >= start_col && column < start_col + len {
                return Some(ButtonHit {
                    id: span.id,
                    code: entry.code,
                    scope: entry.scope,
                    state: entry.state,
                    row,
                    start_col,
                    len,
                });
            }
        }
        None
    }

    /// Whether a cooperating shell currently reports an active prompt (an OSC
    /// 133 `A` boundary with no `C`/`D` since). Advisory: `false` without
    /// shell integration. The pointer arm consults this for the sticky-button
    /// report-suppression policy (B3).
    pub fn prompt_active(&self) -> bool {
        self.active_prompt_start.is_some()
    }

    /// Search the combined scrollback + visible buffer for `query`, returning
    /// every match as an absolute cell range (row `0` = oldest scrollback;
    /// see [`super::search`] for the coordinate convention and limitations).
    /// Matches are returned in reading order, sorted ascending by `start`.
    pub fn search(&self, query: &str, options: SearchOptions) -> Vec<SearchMatch> {
        // One of the two consumers that genuinely needs every physical row.
        // Materialized transiently and dropped when the search returns, rather
        // than retained: a full-buffer search is user-initiated, so paying for
        // the projection here costs nothing between searches.
        let scrollback = self.scrollback.physical_all(self.dimensions.columns);
        let rows: Vec<SearchRow<'_>> = scrollback
            .iter()
            .chain(self.rows.iter())
            .map(|line| SearchRow {
                cells: &line.cells,
                wrapped: line.wrapped,
            })
            .collect();
        search_rows(&rows, query, options)
    }

    /// The visible viewport's physical rows at scrollback `offset_rows`, as owned
    /// [`VisibleRow`]s carrying each row's `wrapped` flag — the windowed input the
    /// hint / quick-select scanner consumes (it needs the soft-wrap flags the
    /// flat [`Snapshot`] does not carry).
    ///
    /// Mirrors the [`search`](Self::search) row-build but windows to the visible
    /// viewport only — the same window as
    /// [`snapshot_with_scrollback`](Self::snapshot_with_scrollback). Offset `0` is
    /// the live screen (`self.rows`); positive offsets page upward into
    /// scrollback, clamped so callers cannot read past the oldest row. Rows are
    /// emitted top-to-bottom in screen order, so a scanner's row indices are
    /// viewport-relative (row `0` = the top visible row) — exactly the coordinate
    /// the renderer paints hint labels in. Pure; never panics.
    pub fn visible_search_rows(&self, offset_rows: usize) -> Vec<VisibleRow> {
        let height = self.dimensions.rows;
        let columns = self.dimensions.columns;
        let scrollback_len = self.scrollback.physical_len(columns);
        // Same window as snapshot_with_scrollback: bottom edge `offset` rows above
        // the live tail. window_start = (scrollback_len + height) - offset - height
        // = scrollback_len - offset, i.e. the last `offset` scrollback rows.
        let offset = offset_rows.min(scrollback_len);
        let tail = self.scrollback.physical_tail(columns, offset);
        tail.iter()
            .chain(self.rows.iter())
            .take(height)
            .map(|line| VisibleRow {
                cells: line.cells.clone(),
                wrapped: line.wrapped,
            })
            .collect()
    }
}

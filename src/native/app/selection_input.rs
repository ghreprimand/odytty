// SPDX-License-Identifier: GPL-3.0-only
//! Selection and viewport input for the native app: drag selection, word and
//! line selection, autoscroll, click-to-position, and scrollback viewport
//! movement.

use super::mouse_protocol::{click_position_bytes, click_travel_delta};
use super::*;
use crate::core::{InputRegion, RowJoin};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EditableInputSelection {
    pub(super) text: String,
    pub(super) edit_bytes: Vec<u8>,
}

/// Resolution of the selection-delete fallback ladder (B-DESIGN §4) for the
/// current selection; see [`App::selection_delete_outcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SelectionDeleteOutcome {
    /// R4: exact geometry, real buffer edit — send these bytes.
    Synthesize(EditableInputSelection),
    /// R2/R3/default: on the input region, but no certain geometry — consume
    /// the key, clear the selection, show the shell-integration hint.
    NoOpWithHint,
    /// R0 fail or selection not on the input region — normal key encode.
    FallThrough,
}

impl App {
    pub(super) fn begin_selection(&mut self) {
        let Some(point) = self.pointer_cell else {
            return;
        };
        // NF21-8: a selection gesture starts with the left button physically
        // down; record that so the motion path can refuse to extend once the
        // button is up (see the guard in the grid `CursorMoved` path).
        self.grid_left_held = true;
        // MOUSE-RECT: Alt makes the whole gesture a rectangular/column (block)
        // selection; every non-Alt gesture is wrapped. The mode is decided once
        // here at the single selection entry point, so the word/line/drag
        // sub-paths below all inherit the right mode and a prior block selection
        // can never leak into a new wrapped one. Alt is reached only on the
        // local path (the mouse-reporting gate already returned for a reporting
        // app, where Shift is the only selection-vs-passthrough seam), so
        // Alt+drag never steals Alt+motion from a TUI that wants it. Block
        // selection is inherently char-granularity, so Alt suppresses the
        // double/triple-click word/line semantics and starts a fresh block drag.
        self.selection_block = self.modifiers.alt;
        if self.modifiers.alt {
            self.begin_block_drag(point);
            return;
        }
        // MOUSE-EXTEND: Shift+click extends an existing selection (keep the
        // anchor, move the focus to the click) instead of starting a new one.
        // Reached only on the local path (the report decision already ran), so
        // Shift stays the selection-vs-passthrough seam untouched. Gated by the
        // feature flag and an existing selection; otherwise fall through to the
        // historical click-count dispatch.
        if self.settings.selection_drag_extend
            && self.modifiers.shift
            && self.selection.range().is_some()
        {
            let scrollback_len = self.scrollback_len();
            let viewport_offset = self.viewport.offset();
            self.selection.update(selection::visible_to_absolute(
                point,
                viewport_offset,
                scrollback_len,
            ));
            self.pointer_drag = PointerDrag::Select {
                granularity: SelectGranularity::Char,
                block: false,
            };
            self.drag_anchor_unit = None;
            self.last_selection_autoscroll = None;
            self.request_selection_redraw();
            return;
        }
        match self.clicks.register_click(point, Instant::now()) {
            1 => self.begin_drag_selection(point),
            2 => self.select_word(point),
            _ => self.select_line(point),
        }
    }

    pub(super) fn begin_drag_selection(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let viewport_offset = self.viewport.offset();
        self.selection.begin(selection::visible_to_absolute(
            point,
            viewport_offset,
            scrollback_len,
        ));
        self.pointer_drag = PointerDrag::Select {
            granularity: SelectGranularity::Char,
            block: false,
        };
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    /// MOUSE-RECT: begin a rectangular/column (block) selection at `point`. The
    /// press cell is the anchor; the column band then grows as the pointer
    /// drags, reusing the existing Char-granularity `extend_drag_to` arm (a
    /// block drag follows the pointer exactly like a normal drag — only how the
    /// range renders and copies differs). `self.selection_block` is already set
    /// by the caller, so the render/copy paths treat the live selection as a
    /// block. Constructs the reserved `PointerDrag::Select { block: true }`.
    fn begin_block_drag(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let viewport_offset = self.viewport.offset();
        self.selection.begin(selection::visible_to_absolute(
            point,
            viewport_offset,
            scrollback_len,
        ));
        self.pointer_drag = PointerDrag::Select {
            granularity: SelectGranularity::Char,
            block: true,
        };
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    fn select_word(&mut self, point: CellPoint) {
        let (snapshot, scrollback_len) = self.selection_snapshot();
        let Some(range) = selection::word_range_at(&snapshot, point) else {
            // No word under the pointer: clear and finalize exactly as before
            // (nothing to anchor a word-drag to), regardless of the flag.
            self.selection.clear();
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
            self.request_selection_redraw();
            return;
        };

        let absolute =
            selection::absolute_range_from_visible(range, self.viewport.offset(), scrollback_len);
        self.selection.set_range(absolute);
        self.finalize_or_arm_unit_drag(SelectGranularity::Word, absolute);
        self.request_selection_redraw();
    }

    fn select_line(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let Some(range) = selection::line_range_at(point, self.grid) else {
            return;
        };

        let absolute =
            selection::absolute_range_from_visible(range, self.viewport.offset(), scrollback_len);
        self.selection.set_range(absolute);
        self.finalize_or_arm_unit_drag(SelectGranularity::Line, absolute);
        self.request_selection_redraw();
    }

    /// MOUSE-EXTEND: after a double/triple-click sets a word/line range, either
    /// keep the drag live so a follow-on drag extends by that unit (flag on) or
    /// finalize byte-identically to the historical click-to-finish behavior
    /// (flag off). The off branch is the mandated parity path.
    fn finalize_or_arm_unit_drag(
        &mut self,
        granularity: SelectGranularity,
        anchor: AbsoluteSelectionRange,
    ) {
        if self.settings.selection_drag_extend {
            self.pointer_drag = PointerDrag::Select {
                granularity,
                block: false,
            };
            self.drag_anchor_unit = Some(anchor);
        } else {
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
        }
    }

    /// Extend the in-progress drag-selection to a visible cell, honoring the
    /// active granularity (MOUSE-EXTEND). Char follows the pointer exactly (the
    /// historical drag); Word/Line snap to and union with whole words/lines.
    pub(super) fn extend_drag_to(&mut self, point: CellPoint) {
        match self.pointer_drag {
            PointerDrag::Select {
                granularity: SelectGranularity::Char,
                ..
            } => {
                let scrollback_len = self.scrollback_len();
                let viewport_offset = self.viewport.offset();
                self.selection.update(selection::visible_to_absolute(
                    point,
                    viewport_offset,
                    scrollback_len,
                ));
            }
            PointerDrag::Select {
                granularity: SelectGranularity::Word,
                ..
            } => self.extend_word_drag(point),
            PointerDrag::Select {
                granularity: SelectGranularity::Line,
                ..
            } => self.extend_line_drag(point),
            PointerDrag::None | PointerDrag::Scrollbar { .. } => {}
        }
    }

    fn extend_word_drag(&mut self, point: CellPoint) {
        let Some(anchor) = self.drag_anchor_unit else {
            return;
        };
        let (snapshot, scrollback_len) = self.selection_snapshot();
        let offset = self.viewport.offset();
        let focus_unit = selection::word_range_at(&snapshot, point)
            .map(|range| selection::absolute_range_from_visible(range, offset, scrollback_len))
            .unwrap_or_else(|| {
                // No word under the pointer (e.g. whitespace): extend to the
                // pointer cell as a degenerate unit so the drag still grows.
                let p = selection::visible_to_absolute(point, offset, scrollback_len);
                AbsoluteSelectionRange { start: p, end: p }
            });
        self.selection
            .set_range(selection::union_absolute_ranges(anchor, focus_unit));
    }

    fn extend_line_drag(&mut self, point: CellPoint) {
        let Some(anchor) = self.drag_anchor_unit else {
            return;
        };
        let scrollback_len = self.scrollback_len();
        let offset = self.viewport.offset();
        let Some(range) = selection::line_range_at(point, self.grid) else {
            return;
        };
        let focus_unit = selection::absolute_range_from_visible(range, offset, scrollback_len);
        self.selection
            .set_range(selection::union_absolute_ranges(anchor, focus_unit));
    }

    pub(super) fn request_selection_redraw(&mut self) {
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn selection_snapshot(&self) -> (Snapshot, usize) {
        // P0-3: drag-autoscroll selection path — poison-recover, never abort.
        let terminal = crate::native::lock_recover(&self.terminal);
        (
            terminal.snapshot_with_scrollback(self.viewport.offset()),
            terminal.screen().scrollback_len(),
        )
    }

    pub(super) fn autoscroll_selection_if_needed(
        &mut self,
        y_px: f64,
        cell: CellSize,
        padding: WindowPadding,
    ) {
        // MOUSE-AUTOSCROLL-VEL: the step magnitude ramps with how far the pointer
        // is dragged past the edge band, up to the configured cap. `legacy` mode
        // returns a cap of 1, which makes the helper yield exactly ±1/0 —
        // byte-identical to the historical fixed one-row-per-tick autoscroll.
        let max_rows = self.settings.autoscroll_max_rows();
        let delta =
            selection::drag_autoscroll_step_with_padding(y_px, cell, self.grid, padding, max_rows);
        if delta == 0 {
            return;
        }

        let now = Instant::now();
        if self
            .last_selection_autoscroll
            .is_some_and(|last| now.saturating_duration_since(last) < SELECTION_AUTOSCROLL_INTERVAL)
        {
            return;
        }
        self.last_selection_autoscroll = Some(now);
        self.scroll_viewport(delta);
    }

    pub(super) fn finish_selection(&mut self) {
        // NF21-8: the left button is up once the gesture finalizes; drop the
        // held flag before any early return so a subsequent bare `CursorMoved`
        // cannot extend a stale latch.
        self.grid_left_held = false;
        if !self.pointer_drag.is_selecting() {
            return;
        }
        // SH-CLICK: a bare left click (no drag, so the selection stayed empty —
        // `range()` is `None` for a zero-width selection) on the live shell
        // prompt repositions the input cursor. Decided here at release, NOT at
        // press, so a real drag (a non-empty selection) always wins: drag-select
        // and click-to-position are mutually exclusive by construction (D-SHC-2).
        // `try_click_to_position` returns `false` whenever it does not fire
        // (feature off, shell not advertising, wrong row, same-cell, modified
        // click), so the off path falls straight through to the historical
        // finalize below — byte-identical to today (T1).
        if self.selection.range().is_none() && self.try_click_to_position() {
            // The click positioned the cursor; nothing was selected to copy.
        } else if self.drag_selection_should_write_primary() {
            // MOUSE-EXTEND parity: a plain double/triple-click that never dragged
            // must stay byte-identical to the historical finalize, which wrote
            // nothing to PRIMARY. Only write when a char drag ran (today's
            // behavior) or a word/line drag actually grew past its clicked unit.
            self.write_primary_selection();
            // MOUSE-COPYSELECT: when enabled, also write the CLIPBOARD via the
            // exact copy-shortcut path. Off by default, so the historical
            // PRIMARY-only finish is byte-identical.
            if self.settings.copy_on_select {
                self.handle_copy_shortcut();
            }
        }
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    /// Whether finishing the current drag should write PRIMARY (MOUSE-EXTEND).
    /// Char drags write as before (an empty selection no-ops in
    /// `current_selection_text`, so a plain single click stays a no-op too).
    /// Word/Line drags write only when the selection grew beyond the anchored
    /// click unit, so a plain double/triple-click without a drag stays no-write
    /// — byte-identical to the historical finalize.
    pub(super) fn drag_selection_should_write_primary(&self) -> bool {
        match self.pointer_drag {
            PointerDrag::Select {
                granularity: SelectGranularity::Char,
                ..
            } => true,
            PointerDrag::Select { .. } => match (self.selection.range(), self.drag_anchor_unit) {
                (Some(current), Some(anchor)) => current != anchor,
                _ => true,
            },
            PointerDrag::None | PointerDrag::Scrollbar { .. } => false,
        }
    }

    /// SH-CLICK: whether click-to-position is live right now — the `sh_click`
    /// setting is on AND the shell has advertised OSC 133 `click_events=1` on
    /// its prompt. Doubly off by default: the setting defaults off, and a
    /// non-integrated shell never sets the core flag. When the setting is off
    /// this short-circuits before locking the terminal, so the off path does no
    /// work at all (T1 off-path identity).
    fn sh_click_enabled(&self) -> bool {
        self.settings.sh_click
            && self
                .terminal
                .lock()
                .map(|terminal| terminal.click_events_enabled())
                .unwrap_or(false)
    }

    /// SH-CLICK (F2): emit the cursor-positioning key burst for a bare left
    /// click on the live prompt's input region, returning whether the click was
    /// consumed.
    ///
    /// Returns `false` (the caller falls through to the historical finalize,
    /// byte-identical to today) in every case but the narrow one the feature
    /// targets:
    /// - the feature is off or the shell has not advertised click-events (T1);
    /// - the click carries any modifier — Shift is the selection/passthrough
    ///   seam, Alt is block-select, Ctrl is hyperlink-open, so only a *plain*
    ///   click repositions (T2 — Shift seam preserved);
    /// - the viewport is scrolled off the live tail (a click in scrollback is
    ///   never a prompt edit);
    /// - the alternate screen is active — a full-screen app owns its layout, so
    ///   click-to-position never fires there (defense-in-depth; the live-prompt
    ///   gate below already excludes it in practice);
    /// - the live command block is not awaiting input — i.e. there is no live
    ///   prompt because the command already executed (an `OutputStart` exists)
    ///   or there are no marks at all. This is the real prompt-context gate
    ///   (T4): the click-events flag alone can linger across a running command,
    ///   so we require the last [`crate::core::CommandBlock`] to have no output
    ///   yet;
    /// - there is no core-derived [`crate::core::InputRegion`] (no OSC 133 `B`
    ///   input-start mark, or nothing typed): with no modeled input there is
    ///   nothing to click into (F2 G1);
    /// - the click resolves to no travel under the certainty ladder in
    ///   [`click_travel_delta`] — off the region's rows, on the prompt side of
    ///   the input start, on a hard-newline (multi-logical-line) buffer, or on
    ///   the cursor's own position (F2 G2/R-None/same-cell).
    ///
    /// When it does fire, the glyph delta from [`click_travel_delta`] is encoded
    /// as `|delta|` Left/Right cursor keys through the live key modes
    /// ([`click_position_bytes`]) — honoring DECCKM application-cursor mode, the
    /// load-bearing encoding trap — and written through the same PTY writer a
    /// real arrow keypress uses (T5), after returning to the live tail. Only
    /// Left/Right are ever synthesized, never Up/Down (which carry
    /// history-recall semantics in every shell — a synthesized Up could replace
    /// the user's buffer with a history entry).
    ///
    /// TUI mouse reporting (DECSET 1000/1002/1003/1006…) never reaches here: the
    /// reporting gate in [`App::handle_mouse_input`] returns earlier, so a
    /// reporting app's click is sent to the app, not to click-to-position (T3).
    fn try_click_to_position(&mut self) -> bool {
        if !self.sh_click_enabled() {
            return false;
        }
        // T2: only a plain left click repositions; any modifier defers to its
        // existing meaning (Shift=select/passthrough, Alt=block, Ctrl=open).
        if self.modifiers.shift || self.modifiers.alt || self.modifiers.ctrl || self.super_key {
            return false;
        }
        // A scrolled-back viewport is never a live-prompt edit.
        if self.viewport.offset() != 0 {
            return false;
        }
        let Some(point) = self.pointer_cell else {
            return false;
        };
        // HALF-CELL targeting (all platforms): resolve whether the live click
        // fell in the right half of its cell before locking the terminal, so
        // the caret target snaps to the nearest column boundary rather than
        // flooring to the cell's left edge. `false` (floor) when no live pointer
        // pixel is available, preserving the prior behaviour.
        let subcell_round_up = self.click_subcell_rounds_up(point);
        let delta = {
            let Ok(terminal) = self.terminal.lock() else {
                return false;
            };
            // F11: a full-screen app on the alternate screen owns its layout.
            if terminal.screen().on_alternate_screen() {
                return false;
            }
            // T4 prompt-context gate: the last command block must be awaiting
            // input (no OutputStart) for a live prompt to exist.
            let blocks = crate::core::command_blocks(&terminal.prompt_marks());
            let at_live_prompt = blocks
                .last()
                .is_some_and(|block| block.output_start.is_none());
            if !at_live_prompt {
                return false;
            }
            // F2 G1: the core-derived input region is the click target model.
            let Some(region) = terminal.input_region() else {
                return false;
            };
            let scrollback_len = terminal.screen().scrollback_len();
            let cursor = terminal.screen().cursor();
            let snapshot = terminal.snapshot_with_scrollback(0);
            click_travel_delta(
                &snapshot,
                &region,
                point,
                subcell_round_up,
                cursor,
                scrollback_len,
                self.grid.rows,
            )
        };
        let Some(delta) = delta else {
            return false;
        };
        let bytes = click_position_bytes(delta, self.key_modes());
        if bytes.is_empty() {
            return false;
        }
        // T5: the positioning burst goes to the host through the exact keystroke
        // writer, after snapping to the live tail like any typed input.
        self.return_to_live();
        self.write_pty_bytes(&bytes);
        true
    }

    /// HALF-CELL (nearest-boundary) click-to-position targeting: whether the
    /// live pointer fell in the RIGHT half of its resolved cell. Click-to-place
    /// snaps the caret target to the nearest column BOUNDARY — before a
    /// left-half click, after a right-half click — instead of flooring to the
    /// cell's left edge, matching universal text-editor caret hit-testing. (A
    /// floor target lands the caret one cell left of a click that fell a hair
    /// right of a cell boundary, the reported "clicking between two characters
    /// sometimes lands one cell left" symptom.)
    ///
    /// The sub-cell fraction is recovered from the cached `pointer_px` using the
    /// exact horizontal adjustments [`Self::update_pointer_cell`] applies before
    /// it resolves the cell, so the fraction lines up with the resolved column:
    /// single-pane subtracts the tab-chrome dx (a left rail shifts X; the top
    /// bar does not) and the window padding; multi-pane uses the focused pane's
    /// content-rect x origin (which already folds in the chrome/rail offset).
    /// Returns `false` — floor targeting, the shipped behaviour — when the
    /// pointer pixel or cell metrics are unavailable, so a synthesized press
    /// with no live coordinates stays byte-identical.
    ///
    /// Platform-agnostic: a pointer past the right edge (column clamped) yields
    /// a fraction >= 0.5 and rounds up, which is harmless because the travel
    /// flatten clamps the target to the input's end; a pointer left of the
    /// origin yields a negative fraction and rounds down. Platform-uniform:
    /// [`click_travel_delta`] carries no per-platform behaviour, so this rounding
    /// is the whole of the click-to-place boundary fix on every OS.
    fn click_subcell_rounds_up(&self, point: CellPoint) -> bool {
        let Some((x_px, _)) = self.pointer_px else {
            return false;
        };
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        let cell_w = f64::from(cell.width.max(1));
        // Origin of the column axis in the same physical-x basis the cell was
        // resolved against.
        let origin_x = if let Some((rect, _)) = self.focused_pane_inner_rect() {
            // Multi-pane: the focused pane's PADDED content sub-rect x origin
            // (matches the render origin's per-divider inset).
            f64::from(rect.x)
        } else {
            // Single-pane: window padding after the tab-chrome dx. Both are 0 on
            // the plain top-bar path, so this is the bare padding there.
            let (chrome_dx, _) = self.tab_chrome_offset_px(cell);
            let pad = self
                .gpu
                .as_ref()
                .map(GpuState::window_padding)
                .unwrap_or(WindowPadding::ZERO);
            chrome_dx + f64::from(pad.physical_px())
        };
        // Fraction of the pointer within the resolved cell: >= 0.5 targets the
        // trailing boundary (caret after the glyph).
        let frac = (x_px - origin_x) / cell_w - point.column as f64;
        frac >= 0.5
    }

    /// Number of rows a Shift+PageUp/PageDown press scrolls: one screenful less
    /// one row of overlap for continuity (at least one row).
    pub(super) fn page_lines(&self) -> usize {
        self.grid.rows.saturating_sub(1).max(1)
    }

    /// Current scrollback length from the shared model (0 if the lock is
    /// poisoned), used to clamp upward scrolling.
    pub(super) fn scrollback_len(&self) -> usize {
        self.scrollback_len_of(self.sessions.active_id())
    }

    pub(super) fn scrollback_len_of(&self, token: SessionToken) -> usize {
        let Some(session) = self.sessions.get(token) else {
            return 0;
        };
        session
            .terminal
            .lock()
            .map(|t| t.screen().scrollback_len())
            .unwrap_or(0)
    }

    pub(super) fn scroll_viewport(&mut self, delta: isize) {
        self.scroll_viewport_of(self.sessions.active_id(), delta);
    }

    /// Adjust one pane's scrollback viewport. Wheel events in a split route to
    /// the pane under the pointer; keyboard/page actions keep using the focused
    /// pane through [`Self::scroll_viewport`].
    pub(super) fn scroll_viewport_of(&mut self, token: SessionToken, delta: isize) {
        let scrollback_len = self.scrollback_len_of(token);
        // SCROLL-GLIDE: capture where the follower currently renders BEFORE the
        // offset jumps, so a notch stream re-arms from its lagging position.
        let glide_start_visual = self.scroll_glide_start_visual(token);
        let changed = {
            let Some(session) = self.sessions.get_mut(token) else {
                return;
            };
            match delta.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    session.viewport.scroll_up(delta as usize, scrollback_len)
                }
                std::cmp::Ordering::Less => session.viewport.scroll_down((-delta) as usize),
                std::cmp::Ordering::Equal => false,
            }
        };
        if changed {
            // Discrete notch scrolling moves the integer `Viewport::offset`
            // immediately. `on_viewport_changed_of` snaps by default (clearing
            // any continuous-lane remainder AND the glide follower); re-arm the
            // SCROLL-GLIDE follower right after so the RENDERED viewport eases
            // toward the new offset (a no-op / instant jump when the knob is off
            // or the glide is ineligible).
            self.on_viewport_changed_of(token);
            self.arm_scroll_glide_of(token, glide_start_visual);
        }
    }

    /// Whether wheel events should be translated into cursor keys (alternate
    /// scroll mode, DECSET 1007). True only on the alternate screen with the
    /// mode enabled; the caller has already excluded the mouse-reporting case.
    pub(super) fn alternate_scroll_active(&self) -> bool {
        self.terminal
            .lock()
            .map(|t| t.on_alternate_screen() && t.alternate_scroll_enabled())
            .unwrap_or(false)
    }

    /// Translate a wheel movement of `lines` into that many Up/Down cursor-key
    /// presses sent to the PTY (alternate scroll mode). `lines > 0` is a
    /// scroll-up (toward earlier content) → Up; `lines < 0` → Down. Arrows are
    /// encoded through the shared key encoder so DECCKM application-cursor mode
    /// gets the SS3 form (`\x1bOA`/`\x1bOB`), byte-identical to a real arrow key.
    pub(super) fn send_wheel_as_arrows(&mut self, lines: isize) {
        let key = if lines > 0 { Key::Up } else { Key::Down };
        let count = lines.unsigned_abs();
        if count == 0 {
            return;
        }
        let modes = self.key_modes();
        let arrow = input::encode_key_event(key, Modifiers::NONE, modes, KeyEventType::Press);
        if arrow.is_empty() {
            return;
        }
        let mut bytes = Vec::with_capacity(arrow.len() * count);
        for _ in 0..count {
            bytes.extend_from_slice(&arrow);
        }
        self.write_pty_bytes(&bytes);
    }

    /// Return the viewport to the live bottom (offset 0). Called whenever input
    /// is written to the PTY so typing always jumps back to the prompt.
    pub(super) fn return_to_live(&mut self) {
        if self.viewport.reset_to_live() {
            self.on_viewport_changed();
        }
    }

    /// Shared side effects of a viewport offset change: keep absolute
    /// selections intact and request one rebuild/redraw so their visible
    /// intersection is recomputed.
    pub(super) fn on_viewport_changed(&mut self) {
        self.on_viewport_changed_of(self.sessions.active_id());
    }

    pub(super) fn on_viewport_changed_of(&mut self, token: SessionToken) {
        // Snap by default — clear any sub-row scroll remainder the continuous
        // (pixel) lane left, so every viewport change (return-to-live, search
        // nav, scrollbar-thumb drag, resize) lands exactly on the integer
        // offset. The continuous lane re-writes the remainder after this call.
        // No-op at rest (byte-identical off path).
        self.clear_scroll_frac_of(token);
        // SCROLL-GLIDE: the same snap-by-default seam settles the forward-chase
        // follower to the exact offset; the scroll path re-arms it afterward.
        self.snap_scroll_glide_of(token);
        self.hovered_hyperlink = self
            .pointer_cell
            .and_then(|point| self.visible_cell_hyperlink(point));
        if let Some(session) = self.sessions.get_mut(token) {
            session.needs_rebuild = true;
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

pub(super) fn selected_columns_on_row(
    range: AbsoluteSelectionRange,
    row: usize,
    columns: usize,
) -> Option<(usize, usize)> {
    if row < range.start.row || row > range.end.row || columns == 0 {
        return None;
    }
    let start = if row == range.start.row {
        range.start.column
    } else {
        0
    };
    let end = if row == range.end.row {
        range.end.column
    } else {
        columns - 1
    };
    Some((start.min(columns - 1), end.min(columns - 1)))
}

pub(super) fn snapshot_row_text(
    snapshot: &Snapshot,
    row: usize,
    start: usize,
    end: usize,
) -> String {
    snapshot_row_cells(snapshot, row, start, end)
        .filter(|cell| !cell.wide_continuation)
        .map(|cell| cell.grapheme())
        .collect()
}

pub(super) fn snapshot_row_cell_count(
    snapshot: &Snapshot,
    row: usize,
    start: usize,
    end: usize,
) -> usize {
    snapshot_row_cells(snapshot, row, start, end)
        .filter(|cell| !cell.wide_continuation)
        .count()
}

pub(super) fn snapshot_row_cells(
    snapshot: &Snapshot,
    row: usize,
    start: usize,
    end: usize,
) -> impl Iterator<Item = &crate::core::Cell> {
    let columns = snapshot.dimensions.columns;
    let offset = row * columns;
    snapshot.cells[offset + start..=offset + end].iter()
}

pub(super) fn delete_selection_bytes(
    snapshot: &Snapshot,
    row: usize,
    selection_start: usize,
    cursor: Position,
    delete_count: usize,
    modes: KeyModes,
) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    if selection_start < cursor.column {
        let move_count = snapshot_row_cell_count(snapshot, row, selection_start, cursor.column - 1);
        let left = input::encode_key_event(Key::Left, Modifiers::NONE, modes, KeyEventType::Press);
        if move_count > 0 && left.is_empty() {
            return None;
        }
        bytes.extend(left.repeat(move_count));
    } else if selection_start > cursor.column {
        let move_count = snapshot_row_cell_count(snapshot, row, cursor.column, selection_start - 1);
        let right =
            input::encode_key_event(Key::Right, Modifiers::NONE, modes, KeyEventType::Press);
        if move_count > 0 && right.is_empty() {
            return None;
        }
        bytes.extend(right.repeat(move_count));
    }
    let delete = input::encode_key_event(Key::Delete, Modifiers::NONE, modes, KeyEventType::Press);
    if delete.is_empty() {
        return None;
    }
    bytes.extend(delete.repeat(delete_count));
    Some(bytes)
}

/// R5 (B-DESIGN §4, B1 soft-wrap slice): resolve Delete/Backspace over a
/// selection on an `Exact` soft-wrapped multi-row input region by flattening
/// the core-provided per-row spans into one logical horizontal axis. A soft
/// wrap has no newline in the edit buffer, so motion is horizontal-only: the
/// synthesized bytes are Left/Right×n to the selection start then Delete×count,
/// exactly like the single-row rung but with glyph offsets summed across rows.
/// Wrap-filler and decoration cells sit outside the spans and contribute
/// nothing. Any geometric doubt degrades to the hinted no-op (charter: a wrong
/// delete is worse than a no-op).
pub(super) fn flattened_selection_delete_outcome(
    snapshot: &Snapshot,
    region: &InputRegion,
    range: AbsoluteSelectionRange,
    cursor: Position,
    scrollback_len: usize,
    grid_rows: usize,
    modes: KeyModes,
) -> SelectionDeleteOutcome {
    let row_count = region.end_row - region.start_row + 1;
    // Defensive re-check of the R5 preconditions: core only populates
    // `row_spans` under Exact, and HardNewline joins force Unknown at R2.
    if region.row_spans.len() != row_count || region.joins.contains(&RowJoin::HardNewline) {
        return SelectionDeleteOutcome::NoOpWithHint;
    }
    let Some(base_visible) = region.start_row.checked_sub(scrollback_len) else {
        return SelectionDeleteOutcome::FallThrough;
    };
    if base_visible + row_count > grid_rows {
        return SelectionDeleteOutcome::FallThrough;
    }
    let columns = snapshot.dimensions.columns;
    // Flattened glyph offset at each row's span start.
    let mut prefix = Vec::with_capacity(row_count);
    let mut total_glyphs = 0usize;
    for (rel, &(span_start, span_end)) in region.row_spans.iter().enumerate() {
        prefix.push(total_glyphs);
        if span_start < span_end {
            total_glyphs +=
                snapshot_row_cell_count(snapshot, base_visible + rel, span_start, span_end - 1);
        }
    }
    // Cursor → flattened offset. The Exact walk validated the cursor against
    // the shell's report, so a cursor outside the region here means the
    // region and grid raced — degrade rather than guess.
    let Some(cursor_rel) = cursor.row.checked_sub(base_visible) else {
        return SelectionDeleteOutcome::NoOpWithHint;
    };
    if cursor_rel >= row_count {
        return SelectionDeleteOutcome::NoOpWithHint;
    }
    let cursor_flat = {
        let (span_start, span_end) = region.row_spans[cursor_rel];
        let col = cursor.column.clamp(span_start, span_end);
        prefix[cursor_rel]
            + if col > span_start {
                snapshot_row_cell_count(snapshot, base_visible + cursor_rel, span_start, col - 1)
            } else {
                0
            }
    };
    // Selection → flattened start + glyph count: per-row intersection with the
    // input spans. Middle selection rows span the full width, and the spans
    // concatenate in flattened order, so the covered glyphs are contiguous.
    let mut start_flat: Option<usize> = None;
    let mut delete_count = 0usize;
    let mut text = String::new();
    for (rel, &(span_start, span_end)) in region.row_spans.iter().enumerate() {
        if span_start >= span_end {
            continue;
        }
        let Some((sel_start, sel_end)) =
            selected_columns_on_row(range, region.start_row + rel, columns)
        else {
            continue;
        };
        let start = sel_start.max(span_start);
        let end = sel_end.min(span_end - 1);
        if start > end {
            continue;
        }
        let row = base_visible + rel;
        let glyphs = snapshot_row_cell_count(snapshot, row, start, end);
        if glyphs == 0 {
            continue;
        }
        if start_flat.is_none() {
            let before = if start > span_start {
                snapshot_row_cell_count(snapshot, row, span_start, start - 1)
            } else {
                0
            };
            start_flat = Some(prefix[rel] + before);
        }
        delete_count += glyphs;
        text.push_str(&snapshot_row_text(snapshot, row, start, end));
    }
    let Some(start_flat) = start_flat else {
        // Selection touches the region's rows but only non-input cells
        // (prompt, wrap filler, decorations): ladder default no-op.
        return SelectionDeleteOutcome::NoOpWithHint;
    };
    if delete_count == 0 || text.is_empty() {
        return SelectionDeleteOutcome::NoOpWithHint;
    }
    let mut edit_bytes = Vec::new();
    if start_flat < cursor_flat {
        let left = input::encode_key_event(Key::Left, Modifiers::NONE, modes, KeyEventType::Press);
        if left.is_empty() {
            return SelectionDeleteOutcome::NoOpWithHint;
        }
        edit_bytes.extend(left.repeat(cursor_flat - start_flat));
    } else if start_flat > cursor_flat {
        let right =
            input::encode_key_event(Key::Right, Modifiers::NONE, modes, KeyEventType::Press);
        if right.is_empty() {
            return SelectionDeleteOutcome::NoOpWithHint;
        }
        edit_bytes.extend(right.repeat(start_flat - cursor_flat));
    }
    let delete = input::encode_key_event(Key::Delete, Modifiers::NONE, modes, KeyEventType::Press);
    if delete.is_empty() {
        return SelectionDeleteOutcome::NoOpWithHint;
    }
    edit_bytes.extend(delete.repeat(delete_count));
    SelectionDeleteOutcome::Synthesize(EditableInputSelection { text, edit_bytes })
}

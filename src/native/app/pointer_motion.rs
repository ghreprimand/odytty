// SPDX-License-Identifier: GPL-3.0-only
//! Pointer motion, window focus, cursor icon, and scrollbar drag routing for
//! the native app.
//!
//! Owns per-motion pointer cell resolution and the latch clearing a focus or
//! occlusion transition performs before the next target is used.

use super::*;

impl App {
    /// Compute the fractional body-relative x coordinate from the cached
    /// physical pixel position. Returns `None` when pixel data or GPU cell info
    /// is unavailable (tests, headless mode).
    ///
    /// The value is body-relative: 0.0 = left edge of the first body cell,
    /// 1.0 = right edge of the first body cell, etc. Non-integer values give
    /// sub-cell precision for smooth slider tracking.
    pub(super) fn pointer_x_in_body(
        &self,
        rect: &crate::native::overlay::OverlayRect,
    ) -> Option<f32> {
        let (x_px, _) = self.pointer_px?;
        let cell = self.gpu.as_ref().map(GpuState::cell)?;
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let body_left_px = rect.body_left as f32 * cell.width as f32 + padding.as_f32();
        Some((x_px as f32 - body_left_px) / cell.width.max(1) as f32)
    }

    /// The pointer left the window surface.
    ///
    /// Some Wayland compositors terminate the implicit pointer grab at the
    /// surface edge without forwarding the paired release, so leaving during a
    /// divider gesture is treated as that gesture's final boundary.
    ///
    /// F4-P3: the auto-hide machine also gets an empty sample so a rail revealed
    /// at the edge starts its hide grace (no `CursorMoved` fires once the pointer
    /// is gone). Inert unless auto-hide is active.
    pub(super) fn on_cursor_left(&mut self) {
        self.settle_divider_for_cursor_leave();
        if self.rail_autohide_active() {
            // Drop the motion-aware trigger's previous sample so the next entry
            // starts fresh (a stale pre-leave x would fabricate a segment across
            // the whole surface on re-entry).
            self.last_rail_pointer_px = None;
            if self.rail_autohide.on_pointer(false, false, Instant::now())
                && let Some(window) = self.window.as_ref()
            {
                window.request_redraw();
            }
        }
    }

    /// Record activity for the active cursor without touching terminal input.
    /// The application-controlled DECSCUSR blink flag remains authoritative:
    /// steady shapes keep no deadline, while a blinking focused cursor holds
    /// solid until the activity quiet period expires.
    pub(in crate::native) fn note_cursor_keyboard_activity(&mut self, now: Instant) {
        let blinking = self
            .terminal
            .lock()
            .map(|terminal| terminal.cursor_blinking())
            .unwrap_or(false);
        let focused = self.focused;
        self.cursor_blink.note_activity(now, blinking, focused);
        if blinking && focused {
            self.hold_cursor_easing_visible(now);
        }
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// BLACK-SCREEN-ON-RESTORE: clear the minimized flag and reset the
    /// skipped-frame retry budget when the window returns from minimized via a
    /// signal OTHER than a non-zero `Resized` (on Windows a restore can fire
    /// only `Focused(true)` / `Occluded(false)`). While `window_minimized`
    /// stays set, the first surface acquire after restore returns `Skipped` and
    /// [`should_schedule_skipped_retry`] vetoes the retry-wake, so no frame
    /// paints until an unrelated input event — the window is black until a
    /// click. Clearing the flag + resetting the budget lets the bounded retry
    /// schedule, and a repaint is requested so the recovered surface paints.
    ///
    /// Mirrors the recovery the non-zero `Resized` arm already performs, and is
    /// idempotent: a normal restore also fires `Resized(non-zero)` which already
    /// cleared the flag, so this is then a no-op; and on Linux/macOS, where
    /// un-minimize goes through `Resized`, the flag is already false by the time
    /// `Focused`/`Occluded` fire, so the callers below do nothing. Returns
    /// whether a minimized state was actually cleared.
    pub(super) fn restore_from_minimized(&mut self) -> bool {
        if !self.window_minimized {
            return false;
        }
        self.window_minimized = false;
        self.consecutive_skipped_frames = 0;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        true
    }

    /// Handle `WindowEvent::Focused`. Factored out of the event arm (so it needs
    /// no `ActiveEventLoop` and is unit-testable) with behavior unchanged except
    /// the added minimize-restore recovery: gaining focus while minimized
    /// (Windows restore-without-`Resized`) clears the flag so the vetoed repaint
    /// can schedule. The redraw the arm already requests then actually paints.
    pub(super) fn on_window_focus_changed(&mut self, focused: bool) {
        self.observe_osc52_window_focus();
        self.focused = focused;
        if focused {
            // B3 focus-transfer exclusion (#11167 class): the click that
            // activates the window must never fire a button. Arm the marker;
            // the next content left press consumes it.
            self.focus_click_pending = true;
            // A focus gain is a fresh visible-hold boundary for the active
            // cursor. This is presentation-only and leaves focus-report bytes
            // below unchanged.
            self.note_cursor_keyboard_activity(Instant::now());
            self.rearm_bell_attention_on_focus_gain();
            // Read the reset-immune episode before restore clears the bounded
            // retry counter. A fresh focus gain remains byte-identical.
            if self.skip_episode.is_active() {
                self.pending_surface_reconfigure = true;
            }
            // A restore may deliver `Focused(true)` before (or without) a
            // non-zero `Resized`; recover the paint here. No-op when not
            // minimized, so the ordinary focus-gain path is unchanged.
            self.restore_from_minimized();
        } else {
            self.cancel_pending_text_paste();
            // A compositor may end a pointer grab by transferring focus without
            // delivering the paired button release. Settle a pane divider while
            // its original tab and geometry are still active, before clearing
            // the remaining window-level pointer latches below.
            self.finish_divider_drag();
            // Clipboard-write consent is valid only while the emitting session
            // remains visibly focused. Drop the pending value before any focus
            // report or later key can act on it.
            self.cancel_osc52_prompt();
            // Drop the deadline immediately rather than waiting for the next
            // render sample, so an unfocused window cannot retain a stale blink
            // wake while the event loop is otherwise idle.
            self.cursor_blink.park();
            self.window_pointer_px = None;
            self.cancel_overlay_drag_on_focus_loss();
            self.pointer_left_held = false;
            self.pointer_drag = PointerDrag::None;
            self.rail_seam_drag = false;
            self.tab_bar_seam_drag = false;
            self.rail_ws_drag = None;
            self.top_tab_drag = None;
            self.report_button = None;
            // B3: a focus loss strands the latched button press (its release
            // may be delivered to another window); drop it so a later release
            // can never fire a stale button.
            self.pressed_button = None;
            // NF21-8: an alt-tab can deliver the button release to another
            // window, stranding the grid selection's held flag. Drop it so a
            // `CursorMoved` on focus regain cannot resume a buttonless drag.
            self.grid_left_held = false;
            // Same NF21-8 class for the rename/save-layout text field: a
            // press-drag interrupted by an alt-tab never sees its release, so
            // `rename_dragging` would otherwise stay armed and a later bare
            // `CursorMoved` on focus regain would relocate the rename caret.
            self.rename_dragging = false;
            // A modifier held across an alt-tab may be released over the other
            // window, so no paired `ModifiersChanged` returns here and the cache
            // stays stuck (a phantom Ctrl/Alt/Shift/Super on the next keypress).
            // Clear it; the next `ModifiersChanged` re-syncs the true state.
            self.modifiers = Modifiers::default();
            self.super_key = false;
            // WHEEL-SENS (T-reset): drop any partially-accumulated wheel
            // notch so a gesture interrupted by an alt-tab does not
            // resume against the next surface on focus regain.
            self.wheel_accum.reset();
            // SCROLL-FEEL Tier 2: drop any sub-row scroll remainder too, so a
            // partial continuous glide does not resume after an alt-tab.
            let token = self.sessions.active_id();
            self.clear_scroll_frac_of(token);
            // P1-8: drop the overlay damper's pixel carry too, for the
            // same reason (a half-detent flick must not resume later).
            self.overlay_wheel.reset();
            // §7: drop any pending multiplexer prefix on focus loss so a
            // half-entered prefix does not survive an alt-tab and capture
            // the first key on focus regain.
            self.prefix_engine.cancel();
        }
        // Force the cursor solid-on immediately on focus loss (and
        // resume blinking on focus gain) by rebuilding next frame.
        self.needs_rebuild = true;
        // ID2 focus dimming: a focus transition changes the effective
        // focus-dim amount applied to every cell, so the cell geometry
        // (not just the cursor) must be rebuilt. Bump the presentation
        // epoch — folded into the content render signature — so this
        // frame resolves to a Full geometry update rather than a
        // CursorOnly/Retained one. Harmless when focus_dim is off (the
        // rebuilt vertices are byte-identical).
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
        self.send_focus_report(focused);
    }

    /// Handle `WindowEvent::Occluded`. Only the un-occlude (`false`) direction is
    /// acted on: on some platforms a Windows restore surfaces as
    /// `Occluded(false)` without a non-zero `Resized`, leaving the window black
    /// until a click. The occlude (`true`) direction is deliberately NOT treated
    /// as a minimize — occlusion (another window covering ours) is not minimize
    /// on every platform, and true minimize is already tracked via the 0x0
    /// `Resized` path — so setting the flag here could wrongly suppress repaints
    /// of a merely-covered window. `restore_from_minimized` is a no-op unless a
    /// minimized state is actually pending, so this is harmless on Linux/macOS
    /// where un-minimize goes through `Resized`.
    pub(super) fn on_window_occluded(&mut self, occluded: bool) -> bool {
        if !occluded {
            // Wayland workspace return is commonly not a minimize, so restore
            // cannot be relied on to request the redraw that consumes this flag.
            let recovering_skip_episode = self.skip_episode.is_active();
            if recovering_skip_episode {
                self.pending_surface_reconfigure = true;
            }
            self.restore_from_minimized();
            if recovering_skip_episode {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                return true;
            }
        }
        false
    }

    /// Push a mouse-cursor shape to the window, but only when it actually
    /// changes — winit issues a platform request on every `set_cursor` call, so
    /// the dedupe keeps `CursorMoved` (which fires on every pixel of motion)
    /// from spamming the windowing system. The terminal grid shows an I-beam
    /// (`Text`), a hovered hyperlink shows a hand (`Pointer`), and window chrome
    /// (tab bar, open overlay) plus mouse-reporting TUIs show the arrow
    /// (`Default`). Before this, OdyTTY never called `set_cursor` at all, so the
    /// pointer stayed the OS default arrow everywhere.
    pub(super) fn apply_cursor_icon(&mut self, icon: CursorIcon) {
        if self.cursor_icon == icon {
            return;
        }
        self.cursor_icon = icon;
        if let Some(window) = self.window.as_ref() {
            window.set_cursor(icon);
        }
    }

    /// The resize cursor for a divider of the given split axis: a column split
    /// (panes side-by-side, vertical divider) drags horizontally → `ColResize`
    /// (`↔`); a row split (panes stacked, horizontal divider) drags vertically →
    /// `RowResize` (`↕`). Pure mapping, shared by the hover and active-drag
    /// cursor paths so both agree.
    pub(super) fn divider_resize_icon(axis: SplitAxis) -> CursorIcon {
        match axis {
            SplitAxis::Columns => CursorIcon::ColResize,
            SplitAxis::Rows => CursorIcon::RowResize,
        }
    }

    /// True when, in a multi-pane tab, the pointer is over a pane OTHER than the
    /// focused one — or in a divider gap with no pane content beneath it. This is
    /// the case where hover resolution must be suppressed: `self.grid` /
    /// `self.terminal` belong to the focused pane, so mapping an off-pane pointer
    /// into them resolves a false link/path/URL. Always `false` on a single-pane
    /// tab (`multipane_geometry` is `None`), keeping the single-pane and
    /// focused-pane hover paths byte-identical.
    pub(super) fn pointer_over_nonfocused_pane(&self) -> bool {
        let Some((content, _)) = self.multipane_geometry() else {
            return false;
        };
        let Some((px, py)) = self.pointer_px else {
            return false;
        };
        match self
            .sessions
            .active_pane_at_point(content, PANE_DIVIDER_PX, px as f32, py as f32)
        {
            Some(token) => token != self.sessions.active_id(),
            None => true,
        }
    }

    /// Drop any hovered hyperlink / path / URL span (and the armed-underline
    /// cells that mirror them), requesting a rebuild/redraw only when something
    /// was actually cleared. Used when hover must be suppressed (pointer over a
    /// non-focused pane) so a stale span from a prior focused hover does not
    /// keep a hand cursor or decoration alive.
    fn clear_hovered_link_spans(&mut self) {
        let had_span = self.hovered_hyperlink.is_some()
            || self.hovered_path.is_some()
            || self.hovered_path_cells.is_some()
            || self.hovered_url.is_some()
            || self.hovered_url_cells.is_some()
            || self.hovered_button.is_some();
        if !had_span {
            return;
        }
        self.hovered_hyperlink = None;
        self.hovered_path = None;
        self.hovered_path_cells = None;
        self.hovered_url = None;
        self.hovered_url_cells = None;
        self.hovered_button = None;
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    pub(super) fn update_pointer_cell(&mut self, x_px: f64, y_px: f64) {
        self.window_pointer_px = Some((x_px, y_px));
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        self.pointer_px = Some((x_px, y_px));
        // RAIL-DRAG: while a workspace-rail drag gesture is live, pointer motion
        // arms it past the threshold and tracks the drop target, and nothing else
        // — a drag owns the pointer for its lifetime. Placed before the auto-hide
        // reveal feed and every hover/selection path so the gesture is not
        // disturbed by them. Inert when no rail drag is in flight.
        if self.pointer_left_held && self.rail_ws_drag.is_some() {
            self.drag_workspace_to_pointer(x_px, y_px, cell);
            return;
        }
        if self.pointer_left_held && self.top_tab_drag.is_some() {
            self.drag_top_tab_to_pointer(x_px, y_px, cell);
            return;
        }
        // A grabbed floating-rail seam owns pointer motion before the auto-hide
        // band hover path. The pointer remains inside that band while shrinking
        // the rail, so handling hover first would swallow the resize motion.
        if self.pointer_left_held && self.rail_seam_drag {
            self.drag_rail_seam_to_pointer(x_px);
            self.apply_cursor_icon(CursorIcon::ColResize);
            return;
        }
        // F4-P3 rail auto-hide: feed the live pointer to the reveal machine
        // (arms/holds/hides the floating overlay). While the rail is revealed and
        // the pointer is over its band, the overlay owns the pointer — do rail
        // hover and nothing else, so a click there hits the rail, not the
        // terminal beneath it. Inert unless autohide is active.
        if self.rail_autohide_active() {
            self.update_rail_autohide_pointer(x_px, cell, Instant::now());
            if let Some(side) = self.rail_autohide_side()
                && self.rail_overlay_visible()
                && self.pointer_in_reveal_band(x_px, cell, side)
            {
                // The content-facing seam is part of the floating band, but it
                // owns that thin grab region so the resize cursor remains
                // discoverable. All other band motion stays rail-only.
                if !self.pointer_over_rail_seam(x_px, cell) {
                    self.update_rail_overlay_hover(x_px, y_px, cell, side);
                    return;
                }
            }
        }
        // While the tab-bar bottom seam is grabbed, pointer motion resizes the
        // bar height (sets the manual rows + reflows) and nothing else. The seam
        // is a horizontal edge -> a row-resize cursor for the gesture. Held only
        // while the top bar is shown, so the rail / single-pane path is
        // unaffected.
        if self.pointer_left_held && self.tab_bar_seam_drag {
            self.drag_tab_bar_seam_to_pointer(y_px);
            self.apply_cursor_icon(CursorIcon::RowResize);
            return;
        }
        // While a divider is grabbed, pointer motion reflows the split and
        // nothing else — no selection or hover work. `divider_drag` is only ever
        // `Some` in a multi-pane tab, so the single-pane motion path below is
        // byte-identical. Keep the matching resize cursor (`↔`/`↕`) for the
        // dragged divider's axis even as the pointer strays off the hairline, so
        // the affordance is stable through the whole gesture; fall back to the
        // arrow only if the divider can't be resolved.
        if let Some(idx) = self.divider_drag.filter(|_| self.pointer_left_held) {
            self.drag_divider_to_pointer();
            let icon = self
                .multipane_geometry()
                .and_then(|(content, _)| {
                    self.sessions
                        .active_divider_axis(content, PANE_DIVIDER_PX, idx)
                })
                .map(Self::divider_resize_icon)
                .unwrap_or(CursorIcon::Default);
            self.apply_cursor_icon(icon);
            return;
        }
        // Tab-bar bottom-seam hover — a row-resize cursor over the seam grab band
        // so drag-to-resize the bar height is discoverable (the press path grabs
        // the same band). Wins at the rail junction because the horizontal seam
        // is visibly drawn there; skipped while a selection is in progress.
        // Clears any stale slot highlight beneath the resize cursor. Inert when
        // no top bar is shown.
        if !self.pointer_drag.is_selecting() && self.pointer_over_tab_bar_seam(x_px, y_px, cell) {
            if self.tab_bar.hover.is_some() {
                self.tab_bar.set_hover(None);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            self.apply_cursor_icon(CursorIcon::RowResize);
            return;
        }
        // F4-P4: rail seam hover — a column-resize cursor over the remaining
        // vertical seam grab band. Yields to the top seam at their junction and
        // to the scroll thumb inside `pointer_over_rail_seam`.
        if !self.pointer_drag.is_selecting() && self.pointer_over_rail_seam(x_px, cell) {
            if self.tab_rail.hover.is_some() {
                self.tab_rail.set_hover(None);
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            self.apply_cursor_icon(CursorIcon::ColResize);
            return;
        }
        // Tab-chrome hover: a vertical rail hit-tests with its own row-major
        // X-band and tracks hover on `tab_rail`; the top bar keeps the
        // column-major test on `tab_bar` (F4-V2). Whichever is inactive has its
        // hover cleared so a stale highlight can't linger after a placement flip.
        // Tab-chrome hover (dual band). `current_chrome_hit` resolves the rail
        // first (full-height sidebar) then the top bar; whichever the pointer is
        // over gets its widget hover set and the other cleared. Under workspace-
        // rail auto-hide, only the pinned-rail lookup is skipped: the floating
        // rail is handled earlier, while the top strip remains hoverable.
        let chrome_hit = if self.rail_autohide_active() {
            self.current_top_bar_hit()
        } else {
            self.current_chrome_hit()
        };
        let (tab_bar_hit, hit_is_rail) = match chrome_hit {
            Some((ChromeBand::WorkspaceRail, hit)) => {
                let hover = Some(hit);
                if self.tab_rail.hover != hover {
                    self.tab_rail.set_hover(hover);
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
                self.tab_bar.set_hover(None);
                (hover, true)
            }
            Some((ChromeBand::TopBar, hit)) => {
                let hover = Some(hit);
                if self.tab_bar.hover != hover {
                    self.tab_bar.set_hover(hover);
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
                self.tab_rail.set_hover(None);
                (hover, false)
            }
            None => {
                self.tab_bar.set_hover(None);
                self.tab_rail.set_hover(None);
                (None, false)
            }
        };
        if tab_bar_hit.is_some() {
            // Record a benign pointer cell in the chrome region so grid selection
            // / link hover below are skipped; the press path resolves the actual
            // action via `current_chrome_hit` (rail or bar), not this cell.
            let point = if hit_is_rail {
                let y = (y_px as f32 - padding.as_f32()).max(0.0);
                let row = (y / cell.height as f32) as usize;
                CellPoint {
                    row: row.min(self.tab_rail_grid_rows().saturating_sub(1)),
                    column: 0,
                }
            } else {
                let x = (x_px as f32 - padding.as_f32()).max(0.0);
                let col = (x / cell.width as f32) as usize;
                CellPoint {
                    row: 0,
                    column: col.min(self.tab_bar_grid_cols().saturating_sub(1)),
                }
            };
            self.pointer_cell = Some(point);
            self.apply_cursor_icon(CursorIcon::Default);
            return;
        }
        // Map into content-relative space by subtracting the tab chrome: the top
        // bar shifts Y, the left rail shifts X (F4-V2). Byte-identical on the
        // plain path (both offsets 0). Multi-pane maps via the pane content rect
        // (already reserve-offset) inside `active_pane_pointer_cell`, so only the
        // single-pane fallback below consumes these adjusted coordinates.
        let (chrome_dx, chrome_dy) = self.tab_chrome_offset_px(cell);
        let x_px = x_px - chrome_dx;
        let y_px = y_px - chrome_dy;
        // In a multi-pane tab, map only inside the focused pane's actual padded
        // inner rect. A collapsed leaf or pointer in its padding/remainder has
        // no terminal cell and must not clamp into the backing 1x1 PTY grid.
        // Single-pane keeps the established window-origin mapping exactly.
        let point = if self.sessions.active_is_single_pane() {
            Some(selection::cell_at_physical_with_padding(
                x_px, y_px, cell, self.grid, padding,
            ))
        } else {
            self.active_pane_pointer_cell()
        };
        self.pointer_cell = point;
        let Some(point) = point else {
            self.hovered_hyperlink = None;
            self.hovered_path = None;
            self.hovered_path_cells = None;
            self.hovered_url = None;
            self.hovered_url_cells = None;
            self.hovered_button = None;
            let divider_icon = self
                .multipane_geometry()
                .and_then(|(content, _)| {
                    self.pointer_px.and_then(|(px, py)| {
                        self.sessions.active_divider_axis_at_point(
                            content,
                            PANE_DIVIDER_PX,
                            px as f32,
                            py as f32,
                            DIVIDER_GRAB_PX,
                        )
                    })
                })
                .map(Self::divider_resize_icon);
            let icon = divider_icon.unwrap_or_else(|| {
                if self.overlay.is_open()
                    || !self.pointer_over_drawable_pane()
                    || self.mouse_reporting_enabled()
                {
                    CursorIcon::Default
                } else {
                    CursorIcon::Text
                }
            });
            self.apply_cursor_icon(icon);
            return;
        };
        // F4-RENAME-MOUSE: while a rename drag is live the field owns the
        // pointer — extend its selection to the new cell and stop, before any
        // grid hover/selection/PTY-report work. The handler resolves the same
        // window-level cell basis used by the single- and multi-pane painters.
        // `rename_dragging` is only ever set while the modal is open, so this is
        // inert on every other path.
        if self.rename_dragging {
            self.apply_cursor_icon(CursorIcon::Text);
            self.rename_drag_extend();
            return;
        }
        // UX4-P1/P2: while an overlay is open it owns the pointer. Keep caching
        // the coordinates above (a press needs them), but skip link hover, local
        // selection, and PTY motion reports — they belong to the terminal grid
        // beneath the panel. A move is forwarded to the overlay only to advance
        // an active slider drag (UX4-P2); non-drag hover is a no-op.
        if self.overlay.is_open() {
            self.apply_cursor_icon(CursorIcon::Default);
            self.handle_overlay_pointer_move();
            return;
        }
        // MOUSE-SCROLLBAR: a scroll-thumb drag owns the pointer move — scrub the
        // viewport to the offset the thumb-top maps to and stop. Placed before
        // hover/selection/PTY-report so a scrollbar drag does not update link
        // hover, extend a selection, or emit PTY motion. Mutually exclusive with
        // selection (one `pointer_drag` enum); the grab decision already ran at
        // press time.
        if let Some(grab_dy) = self
            .pointer_drag
            .scrollbar_grab()
            .filter(|_| self.pointer_left_held)
        {
            self.apply_cursor_icon(CursorIcon::Default);
            self.drag_scrollbar_to(y_px, grab_dy, cell, padding);
            return;
        }
        // Divider hover (multi-pane): show a resize cursor over a divider grab
        // zone so drag-to-resize is discoverable (the press path already grabs
        // the same band). Absolute pointer coords (`self.pointer_px`) match the
        // press-time hit-test basis — `content.y` already includes the tab-bar
        // offset, unlike the tab-bar-relative `y_px` shadowed above. Skipped
        // while a text selection is in progress (the gesture owns the pointer)
        // and never reached on a single-pane tab (`multipane_geometry` is
        // `None`), so the byte-identical path never shows a resize cursor.
        if !self.pointer_drag.is_selecting()
            && let Some((content, _)) = self.multipane_geometry()
            && let Some((px, py)) = self.pointer_px
            && let Some(axis) = self.sessions.active_divider_axis_at_point(
                content,
                PANE_DIVIDER_PX,
                px as f32,
                py as f32,
                DIVIDER_GRAB_PX,
            )
        {
            self.apply_cursor_icon(Self::divider_resize_icon(axis));
            return;
        }
        // Multi-pane hover analog of focus-follows-click: `self.grid` /
        // `self.terminal` are the FOCUSED pane's, so resolving hover while the
        // pointer is over a NON-focused pane (or a divider gap) would map the
        // pointer into the focused pane and light a false hyperlink / path / URL
        // hit (and hand cursor) there. Suppress hover in that case, clearing any
        // span left over from a prior focused-pane hover. Single-pane and
        // focused-pane hover are unaffected (`pointer_over_nonfocused_pane` is
        // always false on a single-pane tab), so the common path is byte-identical.
        if self.pointer_over_nonfocused_pane() {
            self.clear_hovered_link_spans();
        } else {
            self.update_hover_hyperlink();
            // Button Protocol chip hover: gated on the `buttons` setting inside
            // `update_hover_button`, so with the feature off (the default) this
            // is a single bool test and the hover path stays byte-identical.
            self.update_hover_button();
            // INTERACTIVE-PATHS (Phase 7): recompute the hovered path span. Gated
            // on the `interactive_paths` setting inside `update_hover_path`, so
            // with the feature off (the default) it returns before scanning and
            // this call is a single bool test — the hover path stays byte-identical.
            self.update_hover_path();
            // INTERACTIVE-URLS: recompute the hovered bare-URL span. Gated on the
            // `interactive_urls` setting inside `update_hover_url`; off makes this
            // a single bool test so the hover path stays byte-identical.
            self.update_hover_url();
        }
        // Cursor shape over the terminal grid: a hand on a hovered hyperlink OR a
        // resolved interactive path OR a bare URL, the arrow while a TUI has mouse
        // reporting enabled (it owns clicks, so an I-beam would mislead), and the
        // I-beam over plain selectable text — the standard terminal affordance
        // OdyTTY previously never set. The hovered spans are permanently `None`
        // while their features are off, so the default decision is unchanged. OSC
        // 8 wins ties (cosmetically identical icon; precedence matters for click).
        let grid_icon = if self.hovered_hyperlink.is_some()
            || self.hovered_path.is_some()
            || self.hovered_url.is_some()
            || self.hovered_button.is_some()
        {
            CursorIcon::Pointer
        } else if self.mouse_reporting_enabled() {
            CursorIcon::Default
        } else {
            CursorIcon::Text
        };
        self.apply_cursor_icon(grid_icon);
        if self.pointer_drag.is_selecting() {
            // NF21-8 button-held guard (grid analogue of SLIDER-GUARD): only
            // extend the selection while the left button is physically down. A
            // `Selecting` latch whose release was lost — a mid-drag tab/workspace
            // switch, or an alt-tab that delivered the release elsewhere — would
            // otherwise resume a buttonless drag on the next bare `CursorMoved`,
            // and its eventual unmatched release could reach PTY mouse reporting.
            if self.grid_left_held {
                self.autoscroll_selection_if_needed(y_px, cell, padding);
                self.extend_drag_to(point);
                self.request_selection_redraw();
            }
        } else if self.should_report_mouse_to_pty() || self.report_button.is_some() {
            self.send_mouse_motion_report();
        }
    }

    /// Scrub the viewport to the scrollback offset the dragged scroll thumb maps
    /// to (MOUSE-SCROLLBAR). `grab_dy` anchors the cursor to the grab point on
    /// the thumb. Locks the terminal once for the scrollback length and reuses
    /// it for both the geometry and the clamped jump.
    fn drag_scrollbar_to(
        &mut self,
        y_px: f64,
        grab_dy: f32,
        cell: CellSize,
        padding: WindowPadding,
    ) {
        let y_px = if self.should_show_tab_bar() {
            y_px - f64::from(self.tab_bar_height_px(cell))
        } else {
            y_px
        };
        let scrollback_len = self.scrollback_len();
        let Some(target) = scrollbar_offset_for_drag_with_padding(
            y_px as f32,
            grab_dy,
            scrollback_len,
            self.grid,
            cell,
            padding,
        ) else {
            return;
        };
        if self.viewport.jump_to(target, scrollback_len) {
            self.on_viewport_changed();
        }
    }
}

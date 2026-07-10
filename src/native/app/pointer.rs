// SPDX-License-Identifier: GPL-3.0-only
//! Window-level pointer dispatch for the native app: the `MouseInput` /
//! `MouseWheel` button-and-wheel handlers, middle-click PRIMARY paste, the
//! selection→clipboard text helpers, and the pointer-state resets the overlay
//! and focus-loss paths run.
//!
//! Mechanically split out of `app/mod.rs` to keep that file under the
//! source-size cap and to give the pointer gestures one home as they grow; no
//! behavior or API change. These are `App` methods living in a child module so
//! they reach `App`'s private fields and the sibling methods in `app/mod.rs`
//! and `app/interaction.rs` directly. Methods the parent `app` module (or its
//! other children) call back into are marked `pub(super)`.

use super::*;
use crate::core::{InputRegion, RowJoin};

/// Which chrome band a pointer hit landed on. The tab-bar `TabHit` enum is
/// shared by both bands (Switch/Close/NewTab), so the band discriminates whether
/// a hit acts on the active workspace's TABS (top bar) or the WORKSPACES (rail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum ChromeBand {
    /// The top tab strip — Switch/Close/NewTab act on tabs of the active
    /// workspace.
    TopBar,
    /// The workspace rail sidebar — Switch/Close/NewTab act on workspaces.
    WorkspaceRail,
}

/// Physical-pixel movement threshold shared by workspace-rail and top-tab
/// reorder gestures. Below this, press+release remains a plain activation.
pub(in crate::native) const CHROME_DRAG_THRESHOLD_PX: f64 = 5.0;

/// In-flight drag-to-reorder gesture in the horizontal top tab strip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::native) struct TopTabDrag {
    pub(in crate::native) origin_idx: usize,
    press_x: f64,
    press_y: f64,
    pub(in crate::native) pointer_x: f64,
    pub(in crate::native) armed: bool,
    pub(in crate::native) drop_idx: usize,
}

impl TopTabDrag {
    pub(in crate::native) fn new(idx: usize, press_x: f64, press_y: f64) -> Self {
        Self {
            origin_idx: idx,
            press_x,
            press_y,
            pointer_x: press_x,
            armed: false,
            drop_idx: idx,
        }
    }

    pub(in crate::native) fn update_arm(&mut self, x: f64, y: f64) -> bool {
        self.pointer_x = x;
        if !self.armed {
            let dx = x - self.press_x;
            let dy = y - self.press_y;
            if dx * dx + dy * dy >= CHROME_DRAG_THRESHOLD_PX * CHROME_DRAG_THRESHOLD_PX {
                self.armed = true;
            }
        }
        self.armed
    }
}

/// In-flight drag-to-reorder gesture on a workspace rail slot (RAIL-DRAG). A
/// left press on a workspace slot ARMS the gesture; pointer motion past
/// [`CHROME_DRAG_THRESHOLD_PX`] promotes it to an active drag (`armed`); release
/// commits the reorder through the shipped `move_workspace` engine, or — if the
/// threshold was never crossed — falls through to a plain workspace activate;
/// Escape cancels with the rail order untouched. The rail auto-hide is held
/// open for the gesture's lifetime so the drop target never vanishes mid-drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::native) struct RailWorkspaceDrag {
    /// Source workspace index the press landed on (the slot being moved).
    pub(in crate::native) origin_idx: usize,
    /// Physical-pixel X/Y of the initial press — the threshold origin.
    press_x: f64,
    press_y: f64,
    /// Latest physical-pixel Y. The render layer maps this to the floating
    /// proxy's top row, keeping the grabbed slot under the pointer.
    pub(in crate::native) pointer_y: f64,
    /// `true` once motion crossed [`CHROME_DRAG_THRESHOLD_PX`]: the gesture is a
    /// drag, so release commits a reorder rather than a click activate.
    pub(in crate::native) armed: bool,
    /// Live insertion index (0..=count) the current pointer maps to; only
    /// meaningful once `armed`. Drives the drop-target indicator and the commit.
    pub(in crate::native) drop_idx: usize,
}

impl RailWorkspaceDrag {
    /// Arm a fresh gesture at the press position on workspace slot `idx`.
    pub(in crate::native) fn new(idx: usize, press_x: f64, press_y: f64) -> Self {
        Self {
            origin_idx: idx,
            press_x,
            press_y,
            pointer_y: press_y,
            armed: false,
            drop_idx: idx,
        }
    }

    /// Feed a fresh pointer position. Promotes to `armed` once the straight-line
    /// distance from the press crosses [`CHROME_DRAG_THRESHOLD_PX`] (sticky — it
    /// never disarms). Returns whether the gesture is armed (a drag) after this
    /// sample, so the caller only tracks a drop target while dragging.
    pub(in crate::native) fn update_arm(&mut self, x: f64, y: f64) -> bool {
        self.pointer_y = y;
        if !self.armed {
            let dx = x - self.press_x;
            let dy = y - self.press_y;
            if dx * dx + dy * dy >= CHROME_DRAG_THRESHOLD_PX * CHROME_DRAG_THRESHOLD_PX {
                self.armed = true;
            }
        }
        self.armed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EditableInputSelection {
    pub(super) text: String,
    edit_bytes: Vec<u8>,
}

/// Resolution of the selection-delete fallback ladder (B-DESIGN §4) for the
/// current selection; see [`App::selection_delete_outcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectionDeleteOutcome {
    /// R4: exact geometry, real buffer edit — send these bytes.
    Synthesize(EditableInputSelection),
    /// R2/R3/default: on the input region, but no certain geometry — consume
    /// the key, clear the selection, show the shell-integration hint.
    NoOpWithHint,
    /// R0 fail or selection not on the input region — normal key encode.
    FallThrough,
}

impl App {
    /// Handle a window-level mouse button event (the `WindowEvent::MouseInput`
    /// dispatch). Precedence is unchanged: an open overlay captures the button
    /// first, then an in-progress local selection drag, then TUI mouse
    /// reporting, then local selection / hyperlink-open / middle-click paste.
    pub(super) fn handle_mouse_input(&mut self, state: ElementState, button: WinitMouseButton) {
        // UX4-P1: an open overlay captures the pointer before any
        // selection / PTY-report / hyperlink logic — the mouse analogue
        // of the keyboard `if self.overlay.is_open()` guard. Shift and
        // the TUI mouse mode are not consulted here.
        if self.overlay.is_open() {
            self.handle_overlay_pointer_button(state, button);
            return;
        }
        // Modal pointer capture (Wave-15 foundation): a mouse-owning modal
        // swallows the press beneath the overlay guard, suppressing both local
        // selection and PTY reporting. The tab-rename modal additionally routes
        // the button to its caret/selection handler (F4-RENAME-MOUSE); copy-mode
        // still swallows silently.
        if self.modal_captures_pointer() {
            if self.rename_state.is_some() {
                self.handle_rename_pointer_button(state, button);
            }
            return;
        }
        // RAIL-DRAG: an in-flight workspace-rail drag owns the left button for
        // its whole lifetime — a release ends it (commit-on-drag / activate-on-
        // click), swallowing the event wherever the pointer wandered (the drop
        // may land far from the origin slot, off the rail band entirely, so this
        // cannot rely on `current_chrome_hit`). Motion is handled in
        // `update_pointer_cell`; Escape cancels via the key path. Placed above the
        // seam/divider/tab-hit routing so a mid-drag release never leaks into
        // those paths.
        if self.rail_ws_drag.is_some() && button == WinitMouseButton::Left {
            if state == ElementState::Released {
                self.finish_workspace_drag();
            }
            return;
        }
        // TOP-TAB-DRAG: like the rail gesture, an in-flight tab press owns the
        // left button until release, even when the pointer leaves the strip.
        if self.top_tab_drag.is_some() && button == WinitMouseButton::Left {
            if state == ElementState::Released {
                self.finish_top_tab_drag();
            }
            return;
        }
        // F4-P4: the rail seam drag owns the left button — a release ends it
        // (persisting the dragged width); a press on the seam grab band starts a
        // drag, or resets to auto on a double-click. Placed before the multi-pane
        // pane/divider branch so a seam press on a split tab isn't swallowed by
        // pane focus, and before the tab-hit block so the seam wins its thin band
        // over a tab-slot click. Inert off a rail / on the plain path.
        if self.handle_rail_seam_button(state, button) {
            return;
        }
        // Adjustable tab-bar height: the bottom-seam drag owns the left button
        // the same way, on the horizontal edge. Placed beside the rail seam,
        // before the pane/divider and tab-hit branches, so a height-seam press
        // wins its thin band. Inert when no top bar is shown / on the plain path.
        if self.handle_tab_bar_seam_button(state, button) {
            return;
        }
        // A left-release always ends an in-progress divider drag (design doc
        // §4.2), before any other press routing. `divider_drag` is only ever
        // `Some` inside a multi-pane tab, so the single-pane path is unaffected.
        if let Some(target) = self.divider_drag
            && button == WinitMouseButton::Left
            && state == ElementState::Released
        {
            // RELEASE-SNAP + COALESCED FLUSH (Phase H decision gate → option (a),
            // flush-on-drag-end): the smooth per-pixel drag leaves the active
            // split at an arbitrary sub-cell ratio, whose floored-grid remainder
            // breathes at the outer window margin as the divider moves. On
            // release, snap the dragged split's divider onto a whole-cell
            // boundary (cosmetic; may be a no-op when already aligned), THEN
            // flush exactly one full `resize_all_panes` — PTY included.
            //
            // The flush is UNCONDITIONAL whenever a divider drag was in progress
            // (not gated on the snap changing the ratio): the live per-move drag
            // reflowed only the terminal models via `reflow_all_panes_for_drag`,
            // so the shell's PTY is still at the pre-drag size. This release is
            // the one place the kernel learns the final dimensions; gating it on
            // a snap delta would strand the PTY at the old size whenever the
            // divider happened to land on a cell boundary. Runs before clearing
            // `divider_drag` so the dragged split index is still known;
            // `multipane_geometry()` is `None` on a single-pane tab (which never
            // grabs a divider), so the byte-identical single-pane path is
            // untouched.
            if let Some((content, cell)) = self.multipane_geometry() {
                self.sessions.snap_active_divider(
                    content,
                    PANE_DIVIDER_PX,
                    target,
                    cell.width,
                    cell.height,
                );
                self.sessions
                    .resize_all_panes(content, cell.width, cell.height, PANE_DIVIDER_PX);
                self.sessions.active_mut().needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            self.divider_drag = None;
            return;
        }
        // Multi-pane left press: grab a divider to drag, else focus the clicked
        // pane (focus-follows-click, audit row #6). Returns before the
        // single-pane selection path; per-pane selection geometry is a later
        // Phase-1 checkbox. `multipane_geometry()` is `None` for a single-pane
        // tab, so this whole branch is skipped there and the press path stays
        // byte-identical.
        //
        // Gated on the press landing *inside* the content rect on BOTH axes:
        // the tab strip sits above the content rect (`content.y == pad + tab_h`)
        // and a side workspace rail sits beside it (`content.x` is the rail's
        // inner edge), so a click on either chrome band fails this guard and
        // falls through to the chrome routing below instead of being swallowed by
        // an unconditional `return` here. The y-bound alone shipped first and the
        // SIDE band was forgotten: with a left/right rail a rail-slot press has
        // `y >= content.y` but `x` outside the content rect, so it matched here,
        // resolved to no pane, and the bare `return` killed every rail
        // left-interaction (switch/drag/close/+) whenever the active tab was
        // split. The x-bound restores chrome routing for those presses; a
        // divider-gap press inside the content rect still resolves to no pane and
        // returns as before.
        if button == WinitMouseButton::Left
            && state == ElementState::Pressed
            && let Some((content, _cell)) = self.multipane_geometry()
            && let Some((x_px, y_px)) = self.pointer_px
            && y_px as f32 >= content.y
            && x_px as f32 >= content.x
            && (x_px as f32) < content.x + content.w
        {
            let (x, y) = (x_px as f32, y_px as f32);
            if let Some(idx) = self.sessions.active_divider_at_point(
                content,
                PANE_DIVIDER_PX,
                x,
                y,
                DIVIDER_GRAB_PX,
            ) {
                self.divider_drag = Some(idx);
                return;
            }
            // Not a divider grab: focus the clicked pane (focus-follows-click),
            // then begin a text selection anchored in THAT pane's sub-rect.
            // Recompute the pointer cell against the (possibly newly) focused
            // pane so the anchor is correct after a focus change, then dispatch
            // the same selection entry the single-pane press uses (click-count /
            // Shift-extend / Alt-block all flow through `begin_selection`). A
            // press in a divider gap resolves to no pane and just returns, as
            // before — only the unconditional swallow of an in-pane press is
            // removed.
            if let Some(token) = self
                .sessions
                .active_pane_at_point(content, PANE_DIVIDER_PX, x, y)
            {
                if self.sessions.set_active_focus(token) {
                    self.on_active_session_changed();
                }
                // C11: resolve the anchor from the click coords captured BEFORE
                // the focus switch. `active_pane_pointer_cell()` would re-read
                // `self.pointer_px`, which now derefs to the freshly-focused
                // pane's stale stored coordinate — anchoring the drag at the
                // wrong cell. `x_px`/`y_px` here are the live click position.
                if let Some(point) = self.active_pane_pointer_cell_at(x_px, y_px) {
                    self.pointer_cell = Some(point);
                }
                // Bug 4 (Ctrl+click in a split): the single-pane press path tries
                // the open helpers (OSC 8 hyperlink, interactive path incl. the
                // inline image viewer, bare URL) BEFORE selection; this branch
                // historically began a selection directly, so Ctrl+click never
                // reached `open_image_view` in a split. Mirror the single-pane
                // ladder here. Hover resolution is suppressed while the pointer is
                // over a NON-focused pane, so the latched hover spans would be
                // stale (or `None`) for a pane that was not focused when the
                // pointer moved over it. The clicked pane is now the focused pane
                // and `pointer_cell` was recomputed against its grid, so
                // re-resolving the hover spans here latches them exactly as a
                // single-pane focused hover would before the ladder reads them.
                self.update_hover_hyperlink();
                self.update_hover_path();
                self.update_hover_url();
                if !self.try_open_hovered_hyperlink()
                    && !self.try_open_hovered_path()
                    && !self.try_open_hovered_url()
                {
                    self.note_possible_path_misclick();
                    self.begin_selection();
                }
            }
            return;
        }
        if self.any_chrome_shown() {
            match (button, state, self.current_chrome_hit()) {
                // ----- Top tab bar: act on the active workspace's TABS -----
                (
                    WinitMouseButton::Left,
                    ElementState::Pressed,
                    Some((ChromeBand::TopBar, TabHit::Switch(idx))),
                ) => {
                    self.begin_top_tab_drag(idx);
                    return;
                }
                (
                    WinitMouseButton::Left,
                    ElementState::Pressed,
                    Some((ChromeBand::TopBar, TabHit::Close(idx))),
                ) => {
                    // The tab-strip `×` closes the WHOLE tab at `idx` — every
                    // pane it holds — mirroring the menu/keyboard "Close Tab".
                    // Exit keys on the last *tab* of the last *workspace*, never
                    // the last *pane*.
                    if self.sessions.tab_count() <= 1 && self.sessions.workspace_count() <= 1 {
                        self.pending_exit = true;
                        return;
                    }
                    let _ = self.sessions.close_tab_at(idx);
                    // Closing a tab may drop the active tab back to single-pane
                    // (or shift it); clear a pending multiplexer prefix so a
                    // stale state can't swallow the next key, matching
                    // `App::close_active_tab`.
                    if self.sessions.active_is_single_pane() {
                        self.prefix_engine.cancel();
                    }
                    self.on_active_session_changed();
                    return;
                }
                (
                    WinitMouseButton::Left,
                    ElementState::Pressed,
                    Some((ChromeBand::TopBar, TabHit::NewTab)),
                ) => {
                    self.handle_new_tab();
                    return;
                }
                // ----- Workspace rail: act on the WORKSPACES -----
                (
                    WinitMouseButton::Left,
                    ElementState::Pressed,
                    Some((ChromeBand::WorkspaceRail, TabHit::Switch(idx))),
                ) => {
                    // RAIL-DRAG: arm a drag-to-reorder gesture rather than
                    // activating immediately. A press+release without crossing
                    // the movement threshold falls through to `activate_workspace`
                    // in `finish_workspace_drag` (a plain click); motion past the
                    // threshold turns it into a reorder.
                    self.begin_workspace_drag(idx);
                    return;
                }
                (
                    WinitMouseButton::Left,
                    ElementState::Pressed,
                    Some((ChromeBand::WorkspaceRail, TabHit::Close(idx))),
                ) => {
                    self.close_workspace_at(idx);
                    return;
                }
                (
                    WinitMouseButton::Left,
                    ElementState::Pressed,
                    Some((ChromeBand::WorkspaceRail, TabHit::NewTab)),
                ) => {
                    self.handle_new_workspace();
                    return;
                }
                (WinitMouseButton::Left, ElementState::Released, Some(_)) => return,
                // ----- Right press: per-surface context menu (F7) -----
                (
                    WinitMouseButton::Right,
                    ElementState::Pressed,
                    Some((ChromeBand::TopBar, hit)),
                ) => {
                    // A right-click on a specific tab opens the tight, tab-scoped
                    // `TabSlot` menu targeting THAT tab's token (NF-F7-1). A hit on
                    // the `+`/`×` chrome resolves no token → empty-strip menu.
                    let surface = match hit {
                        TabHit::Switch(idx) => self
                            .sessions
                            .token_at_position(idx)
                            .map(ContextMenuSurface::TabSlot)
                            .unwrap_or(ContextMenuSurface::TabStripEmpty),
                        TabHit::Close(_) | TabHit::NewTab | TabHit::None => {
                            ContextMenuSurface::TabStripEmpty
                        }
                    };
                    self.open_context_menu(surface);
                    return;
                }
                (
                    WinitMouseButton::Right,
                    ElementState::Pressed,
                    Some((ChromeBand::WorkspaceRail, hit)),
                ) => {
                    // NF-F7-4: a right-click on a workspace slot opens the
                    // `WorkspaceSlot` menu targeting THAT slot; the `+`/gaps open
                    // the rail-empty menu.
                    let surface = match hit {
                        TabHit::Switch(idx) | TabHit::Close(idx) => {
                            ContextMenuSurface::WorkspaceSlot(idx)
                        }
                        TabHit::NewTab | TabHit::None => ContextMenuSurface::WorkspaceRailEmpty,
                    };
                    self.open_context_menu(surface);
                    return;
                }
                (WinitMouseButton::Right, ElementState::Released, Some(_)) => return,
                (WinitMouseButton::Right, ElementState::Pressed, None)
                    if self.pointer_in_tab_chrome_band() =>
                {
                    // NF-F7-2 / NF-F7-4: an empty-chrome right-click opens the
                    // surface for whichever band the pointer sits over instead of
                    // leaking the grid menu over the bar.
                    let surface = if self.pointer_in_workspace_rail_band() {
                        ContextMenuSurface::WorkspaceRailEmpty
                    } else {
                        ContextMenuSurface::TabStripEmpty
                    };
                    self.open_context_menu(surface);
                    return;
                }
                _ => {}
            }
        }
        if self.pointer_drag.is_selecting() {
            if button == WinitMouseButton::Left && state == ElementState::Released {
                self.finish_selection();
            }
            return;
        }
        // MOUSE-SCROLLBAR: while a scroll-thumb drag is in progress, swallow
        // button events (the release ends it) so the drag never leaks a press
        // to PTY reporting or local selection. `is_selecting()` is false for the
        // `Scrollbar` variant, so this needs its own guard alongside the one
        // above.
        if self.pointer_drag.scrollbar_grab().is_some() {
            if button == WinitMouseButton::Left && state == ElementState::Released {
                self.pointer_drag = PointerDrag::None;
            }
            return;
        }

        // MOUSE-SCROLLBAR: a left press on the visible scroll thumb grabs it to
        // scrub scrollback. Gated on the `scrollbar_drag` setting and the thumb
        // being visible (`viewport offset > 0`); the hit-test returns `None` at
        // the live tail and when disabled, so this branch is inert there and the
        // press routing below stays byte-identical. Sits before the TUI-report
        // branch so grabbing the thumb wins over mouse reporting — but only when
        // the press actually lands on the thumb; every other press (including in
        // a mouse-reporting app) falls through to exactly the historical path.
        if self.settings.scrollbar_drag
            && button == WinitMouseButton::Left
            && state == ElementState::Pressed
            && let Some(grab_dy) = self.scrollbar_hit_test()
        {
            self.pointer_drag = PointerDrag::Scrollbar { grab_dy };
            return;
        }

        // CTRL-CLICK-OPEN (matches kitty/iTerm2/GNOME Terminal): a left press
        // over a resolved OSC 8 hyperlink, interactive path, or bare URL, with
        // the platform open modifier held (Ctrl on Linux/Windows, Cmd on macOS),
        // opens the target even while a mouse-tracking TUI has mouse reporting
        // enabled. Placed BEFORE the report gate so the open wins over the PTY
        // report for this exact gesture. Every other press — a plain click, or
        // an open-modifier click NOT over a resolved span — fails this guard and
        // falls through to the report gate byte-identically, so a reporting app
        // still receives all of its clicks. With reporting off the open would
        // fire in the left-press arm below anyway; this only moves it one step
        // earlier with the same effect. Pure modifier/hover-state logic, so it
        // is identical on every platform (no PTY/path/spawn surface beyond the
        // existing open ladder).
        //
        // A fresh left press first clears any pending swallow latch: if an
        // earlier open's release was lost (focus change), it must not swallow
        // THIS gesture's release. The latch is re-armed only when this press
        // actually opens a target.
        if button == WinitMouseButton::Left && state == ElementState::Pressed {
            self.swallow_open_left_release = false;
            if open_modifier_held(
                self.modifiers,
                self.super_key,
                super::platform_opener::OpenerOs::host(),
            ) && (self.hovered_hyperlink.is_some()
                || self.hovered_path.is_some()
                || self.hovered_url.is_some())
                && (self.try_open_hovered_hyperlink()
                    || self.try_open_hovered_path()
                    || self.try_open_hovered_url())
            {
                // The press opened a target: swallow its paired release too, so
                // the reporting app sees neither half of the gesture.
                self.swallow_open_left_release = true;
                return;
            }
        }

        // Swallow the left release paired with a Ctrl+click that opened a
        // target. Without this the release would fall through to the report gate
        // below (its press did not, so `report_button` is `None` but
        // `should_report_mouse_to_pty` is true) and leak an unpaired release to
        // the app. Off the open path the latch is always `false`, so every other
        // release routes byte-identically.
        if button == WinitMouseButton::Left
            && state == ElementState::Released
            && self.swallow_open_left_release
        {
            self.swallow_open_left_release = false;
            return;
        }

        if (self.should_report_mouse_to_pty() || self.report_button.is_some())
            && let Some(button) = map_winit_mouse_button(button)
        {
            self.handle_reported_mouse_input(state, button);
            return;
        }

        // IN2: a right-click press opens the context menu. This sits AFTER the
        // TUI report gate above (step 6), so inside a TUI with mouse reporting
        // active the right-click is reported to the PTY and this is never
        // reached. Shift+right-click bypasses the report gate (Shift is excluded
        // from `should_report_mouse_to_pty`), so it falls through to here and
        // opens the menu even in a TUI — the same Shift override convention as
        // local selection. In a plain shell the gate is skipped and the menu
        // opens. No enable bool: the report gate IS the off switch (D-IN2-1).
        if button == WinitMouseButton::Right && state == ElementState::Pressed {
            self.open_context_menu(ContextMenuSurface::Content);
            return;
        }

        if button == WinitMouseButton::Left {
            match state {
                ElementState::Pressed => {
                    // OSC 8 hyperlink wins ties; then a resolved interactive
                    // path (Ctrl+click); then a bare URL (Ctrl+click); else begin
                    // a text selection. Each open helper returns false when its
                    // gate/feature is off, so the selection path stays
                    // byte-identical when none fires.
                    if !self.try_open_hovered_hyperlink()
                        && !self.try_open_hovered_path()
                        && !self.try_open_hovered_url()
                    {
                        // UX-A (Phase 11): neither open fired. If this was a plain
                        // click on a resolved path (the Ctrl gate failed), note it
                        // as a mis-click so the discoverability hint can raise
                        // after two-in-a-row. No-op off the feature path, so the
                        // selection path stays byte-identical.
                        self.note_possible_path_misclick();
                        self.begin_selection();
                    }
                }
                ElementState::Released => self.finish_selection(),
            }
        } else if button == WinitMouseButton::Middle && state == ElementState::Pressed {
            self.handle_primary_paste();
        }
    }

    /// F4-P4 rail seam button routing. Returns `true` when the event was
    /// consumed (so the caller returns without running any tab/selection/PTY
    /// routing):
    /// - while a drag is in progress, a left release ends it and persists the
    ///   dragged width, and any other left event is swallowed;
    /// - a left press on the seam grab band arms a drag (motion sets the manual
    ///   width) — unless it is the second press of a double-click, which resets
    ///   the rail to auto width instead.
    ///
    /// Off a rail (`pointer_over_rail_seam` is false) it consumes nothing, so
    /// the historical press path is byte-identical.
    fn handle_rail_seam_button(&mut self, state: ElementState, button: WinitMouseButton) -> bool {
        if button != WinitMouseButton::Left {
            return false;
        }
        if self.rail_seam_drag {
            if state == ElementState::Released {
                self.rail_seam_drag = false;
                self.persist_rail_width();
            }
            return true;
        }
        if state != ElementState::Pressed {
            return false;
        }
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        let Some((px_x, _)) = self.pointer_px else {
            return false;
        };
        if !self.pointer_over_rail_seam(px_x, cell) {
            return false;
        }
        // Double-click on the seam → reset to auto. Keyed on a fixed synthetic
        // point so two quick seam presses (anywhere on the band) count as a
        // double-click; `drag_rail_seam_to_pointer` resets this tracker on an
        // actual move so a drag-then-grab is never misread as a reset.
        let count = self
            .rail_seam_clicks
            .register_click(CellPoint { row: 0, column: 0 }, std::time::Instant::now());
        if count >= 2 {
            self.reset_rail_width_to_auto();
            return true;
        }
        // First press: arm the drag; pointer motion sets the manual width.
        self.rail_seam_drag = true;
        true
    }

    /// Left-button handling for the adjustable tab-bar height seam, mirroring
    /// [`Self::handle_rail_seam_button`] on the horizontal edge:
    ///
    /// - while a drag is in progress, a left release ends it and persists the
    ///   dragged height, and any other left event is swallowed;
    /// - a left press on the seam grab band arms a drag (motion sets the manual
    ///   height) — unless it is the second press of a double-click, which resets
    ///   the tab bar to auto height instead.
    ///
    /// Off the seam (`pointer_over_tab_bar_seam` is false) it consumes nothing,
    /// so the historical press path is byte-identical.
    fn handle_tab_bar_seam_button(
        &mut self,
        state: ElementState,
        button: WinitMouseButton,
    ) -> bool {
        if button != WinitMouseButton::Left {
            return false;
        }
        if self.tab_bar_seam_drag {
            if state == ElementState::Released {
                self.tab_bar_seam_drag = false;
                self.persist_tab_bar_height();
            }
            return true;
        }
        if state != ElementState::Pressed {
            return false;
        }
        let Some(cell) = self.resolved_cell() else {
            return false;
        };
        let Some((px_x, px_y)) = self.pointer_px else {
            return false;
        };
        if !self.pointer_over_tab_bar_seam(px_x, px_y, cell) {
            return false;
        }
        // Double-click on the seam -> reset to auto. Keyed on a fixed synthetic
        // point so two quick seam presses (anywhere on the band) count as a
        // double-click; `drag_tab_bar_seam_to_pointer` resets this tracker on an
        // actual move so a drag-then-grab is never misread as a reset.
        let count = self
            .tab_bar_seam_clicks
            .register_click(CellPoint { row: 0, column: 0 }, std::time::Instant::now());
        if count >= 2 {
            self.reset_tab_bar_height_to_auto();
            return true;
        }
        // First press: arm the drag; pointer motion sets the manual height.
        self.tab_bar_seam_drag = true;
        true
    }

    /// Hit-test the last cached pointer position against the draggable scroll
    /// thumb (MOUSE-SCROLLBAR), returning the grab offset within the thumb when
    /// the press lands on the visible thumb's grab band, else `None`. The thumb
    /// is visible only while scrolled back (`viewport offset > 0`), so a press
    /// at the live tail (the default) never grabs — keeping the plain press path
    /// byte-identical. Uses `pointer_px`, the same cached coordinates the
    /// SGR-pixel report path relies on (button events carry no coordinates).
    pub(super) fn scrollbar_hit_test(&self) -> Option<f32> {
        let (x_px, y_px) = self.pointer_px?;
        let cell = self.resolved_cell()?;
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        // Map into content-relative space: subtract the tab chrome (top bar → Y,
        // left rail → X). Byte-identical on the plain path (both offsets 0).
        let (chrome_dx, chrome_dy) = self.tab_chrome_offset_px(cell);
        let x_px = x_px - chrome_dx;
        let y_px = y_px - chrome_dy;
        let scrollback_len = self.scrollback_len();
        scroll_indicator_hit_with_padding(
            x_px as f32,
            y_px as f32,
            self.viewport.offset(),
            scrollback_len,
            self.grid,
            cell,
            padding,
        )
    }

    /// The chrome band and hit under the pointer this frame. The workspace rail
    /// is resolved FIRST: it is a full-height sidebar, so in the top-left corner
    /// (a rail column over the top-bar row) the rail wins. The top tab bar is
    /// then resolved in its own X space — a left rail reserves the left columns,
    /// so the bar is shifted right by that band. `None` off all chrome.
    pub(in crate::native) fn current_chrome_hit(&self) -> Option<(ChromeBand, TabHit)> {
        if !self.any_chrome_shown() {
            return None;
        }
        let (x_px, y_px) = self.pointer_px?;
        let cell = self.resolved_cell()?;
        // F4-P3: under rail auto-hide the rail floats, so it is hit-tested against
        // its overlay geometry and only while actually revealed.
        if self.rail_autohide_active() {
            if let Some(hit) = self.rail_overlay_hit() {
                return Some((ChromeBand::WorkspaceRail, hit));
            }
        } else if self.should_show_workspace_rail() {
            let hit = self.tab_rail.hit_test(
                x_px,
                y_px,
                &self.sessions.rail_source(),
                self.rail_cols(),
                self.tab_rail_grid_rows(),
                self.rail_origin_px(cell),
                cell,
                self.rail_geom(),
            );
            if hit != TabHit::None {
                return Some((ChromeBand::WorkspaceRail, hit));
            }
        }
        if self.should_show_tab_bar() {
            let padding = self
                .gpu
                .as_ref()
                .map(GpuState::window_padding)
                .unwrap_or(WindowPadding::ZERO);
            // Map the pointer into the bar's own X space: a left rail reserves the
            // left columns, shifting the bar right by that band (0 for a right
            // rail / no rail / floating rail, so the top-only path is unchanged).
            let left_off = self.tab_reserve().left_reserved_cols() as f64 * cell.width as f64;
            let hit = self.tab_bar.hit_test(
                x_px - left_off,
                y_px,
                &self.sessions,
                self.tab_bar_grid_cols(),
                padding.as_f32(),
                cell,
                padding,
                self.tab_bar_rows(),
            );
            if hit != TabHit::None {
                return Some((ChromeBand::TopBar, hit));
            }
        }
        None
    }

    /// The current cell size for pointer geometry. From the GPU in production;
    /// in headless tests (no GPU) a [`App::test_cell`] override stands in. In
    /// non-test builds the override does not exist, so this is exactly
    /// `self.gpu.as_ref().map(GpuState::cell)`.
    pub(in crate::native) fn resolved_cell(&self) -> Option<CellSize> {
        #[cfg(test)]
        if let Some(cell) = self.test_cell {
            return Some(cell);
        }
        self.gpu.as_ref().map(GpuState::cell)
    }

    /// Handle a window-level wheel event (the `WindowEvent::MouseWheel`
    /// dispatch). Precedence is unchanged: an open overlay scrolls its list
    /// first, then TUI reporting, then local scrollback movement at the
    /// configured per-notch multiplier.
    pub(super) fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        // UX4-P1: an open overlay captures the wheel to scroll its list,
        // before TUI reporting or scrollback movement.
        if self.overlay.is_open() {
            self.handle_overlay_pointer_wheel(delta);
            return;
        }
        // Modal pointer capture (Wave-15 foundation): a mouse-owning modal
        // swallows the wheel beneath the overlay guard. `false` today ⇒ dead
        // code ⇒ the wheel path is unchanged.
        if self.modal_captures_pointer() {
            return;
        }
        // Full mouse-tracking TUIs (vim with mouse, tmux, htop, Claude Code)
        // own the wheel: the SGR/legacy report carries direction, not
        // magnitude, so `scroll_wheel_lines` deliberately does not apply here.
        if self.should_report_mouse_to_pty() {
            let _ = self.handle_reported_wheel(delta);
            return;
        }

        // MOUSE-WHEEL (zoom): Ctrl+wheel adjusts the font size, but only while
        // mouse reporting is off. The report gate above already returned for a
        // reporting app, so Ctrl+wheel there passes through to the PTY untouched
        // — this branch is never reached in that case. Gated on the `wheel_zoom`
        // setting (default on); when off, Ctrl+wheel falls through to the plain
        // scrollback path below, byte-identical to today. Ctrl+wheel is a zoom
        // gesture, so it is consumed here and never also scrolls scrollback
        // (the early return holds even at the clamp boundary, where the zoom is
        // a no-op).
        // Use the resolved cell (GPU cell in production; the test override
        // headlessly) so the continuous-lane pixel→row conversion and the notch
        // coalescer read a real cell height off the GPU render path.
        let cell_height = self.resolved_cell().map_or(0, |cell| cell.height);
        if self.settings.wheel_zoom && self.modifiers.ctrl {
            // WHEEL-SENS: coalesce the burst before mapping to a font step so
            // one physical notch is exactly one step (cap one per notch). The
            // gesture is consumed unconditionally — even when the carry is still
            // sub-notch or the size is clamped at a bound — so Ctrl+wheel never
            // falls through to scrollback (T-zoom-clamp).
            if let Some(notch) = self.wheel_accum.coalesce_zoom(delta, cell_height) {
                let steps = wheel_zoom_steps(notch);
                if steps != 0 {
                    self.adjust_font_size_by(steps);
                }
            }
            return;
        }

        // SCROLL-FEEL Tier 2: high-resolution `PixelDelta` input drives the
        // continuous fractional lane (sub-row pixel-precise), bypassing the
        // notch coalescer entirely. Discrete `LineDelta`, and every
        // ineligible case (pixel_scroll off, multipane, alt-screen, Ctrl-zoom,
        // selection-drag), fall through to the unchanged notch path below.
        if let MouseScrollDelta::PixelDelta(pos) = delta {
            // Drive the continuous lane on the pane UNDER THE POINTER (in a split;
            // the active pane on a single-pane tab), not the focused/active pane —
            // matching the discrete notch lane's `local_wheel_scroll_target` and
            // fixing the focus/pointer mismatch. Eligibility is that pane's own
            // (its screen, the knob, no Ctrl/selection). Ineligible cases
            // (pixel_scroll off, alt-screen, Ctrl-zoom, selection-drag) fall
            // through to the notch path below.
            let target = self.local_wheel_scroll_target();
            if self.continuous_scroll_eligible_of(target) {
                self.drive_continuous_scroll(target, pos.y, cell_height);
                return;
            }
        }

        // WHEEL-SENS + MOUSE-WHEEL-SPEED: coalesce the burst into discrete
        // notches, then honor the configured per-notch multiplier
        // (`scroll_wheel_lines`, default 6) for BOTH local scrollback and
        // alternate-scroll (DECSET 1007) arrow emulation, so a pager scrolls
        // at the same rows-per-notch as the local viewport. The fixed
        // `WHEEL_STEP_LINES` stays the base only for the mouse-reporting and
        // overlay free-scroll paths, which own the wheel magnitude themselves.
        if let Some(notch) = self.wheel_accum.coalesce_scroll(delta, cell_height) {
            let lines = wheel_lines_scaled(notch, cell_height, self.settings.scroll_wheel_step());
            if lines == 0 {
                return;
            }
            // ALT-SCROLL (DECSET 1007): on the alternate screen (which has no
            // scrollback) a TUI that enables alternate-scroll WITHOUT full mouse
            // tracking — classic pagers like `less`, `man`, `git log` — expects
            // the wheel to move via cursor keys. Reporting is already off here
            // (the report gate returned above), so translate the wheel into
            // Up/Down presses at the SAME multiplied count as local scrollback
            // (`scroll_wheel_lines`), keeping pagers in step with the viewport;
            // otherwise move the local scrollback viewport by that count.
            if self.alternate_scroll_active() {
                self.send_wheel_as_arrows(lines);
            } else {
                let target = self.local_wheel_scroll_target();
                self.scroll_viewport_of(target, lines);
            }
        }
    }

    fn local_wheel_scroll_target(&self) -> SessionToken {
        if let Some((content, _)) = self.multipane_geometry()
            && let Some((x, y)) = self.pointer_px
            && let Some(token) =
                self.sessions
                    .active_pane_at_point(content, PANE_DIVIDER_PX, x as f32, y as f32)
        {
            return token;
        }
        self.sessions.active_id()
    }

    /// Adjust the live font size by `steps` pixels (MOUSE-WHEEL Ctrl+wheel
    /// zoom), clamped to the supported range, routed through the existing live
    /// settings-apply seam so the atlas rebuild and grid reflow run exactly as
    /// a `font_size` settings edit would — no separate resize path. A no-op when
    /// the clamp leaves the size unchanged (already at the min/max), so zooming
    /// past the bound does nothing. The change is applied live but not written
    /// to disk: the same transient behavior as dragging the overlay slider
    /// without saving.
    fn adjust_font_size_by(&mut self, steps: i32) {
        let current = self.settings.font_size_px;
        let next_px = (current + steps as f32).clamp(
            crate::settings::MIN_FONT_SIZE_PX,
            crate::settings::MAX_FONT_SIZE_PX,
        );
        if (next_px - current).abs() < f32::EPSILON {
            return;
        }
        let mut next = self.settings.clone();
        next.font_size_px = next_px;
        self.apply_overlay_settings(next);
    }

    fn handle_primary_paste(&mut self) {
        let Some(text) = self.clipboard.read_primary_text() else {
            return;
        };
        self.return_to_live();
        let _ = write_paste_text(&self.terminal, &self.writer, &text);
    }

    pub(super) fn current_selection_text(&self) -> Option<String> {
        let range = self.selection.range()?;
        // Extract the FULL absolute selection (spanning scrollback), never
        // clamped to the visible viewport: a selection scrolled partly off
        // screen copies in full. The scrollback-window walk is shared with the
        // copy-mode yank so mouse and keyboard copy can never diverge. This
        // single choke point is shared by PRIMARY, CLIPBOARD, copy-on-select,
        // and keyboard copy; `selection_block` picks the wrapped vs column-band
        // rule.
        self.absolute_selection_text(range, self.selection_block)
    }

    pub(super) fn editable_input_selection_for_context_menu(
        &self,
    ) -> Option<EditableInputSelection> {
        match self.selection_delete_outcome() {
            SelectionDeleteOutcome::Synthesize(selection) => Some(selection),
            SelectionDeleteOutcome::NoOpWithHint | SelectionDeleteOutcome::FallThrough => None,
        }
    }

    /// Resolve the approved fallback ladder (B-DESIGN §4) for the current
    /// selection against the core-derived input region.
    ///
    /// * [`SelectionDeleteOutcome::Synthesize`] — rung R4: the region is
    ///   `Exact`, single-row, and the selection clamps to a non-empty span of
    ///   real input; the edit bytes correspond to a known buffer edit.
    /// * [`SelectionDeleteOutcome::NoOpWithHint`] — rungs R2/R3 and the ladder
    ///   default: the selection touches the input region but the geometry is
    ///   not certain enough to synthesize (heuristic right edge, stale mark,
    ///   multi-row until B1, or a selection entirely over non-input cells such
    ///   as a right-prompt decoration). Consuming the key with a hint is the
    ///   charter behavior: a wrong delete is worse than a no-op.
    /// * [`SelectionDeleteOutcome::FallThrough`] — rung R0 fails (no local
    ///   selection at the live tail) or the selection does not touch the input
    ///   region at all: the key falls through to the normal encode path,
    ///   exactly as before.
    fn selection_delete_outcome(&self) -> SelectionDeleteOutcome {
        if self.selection_block || self.viewport.offset() != 0 {
            return SelectionDeleteOutcome::FallThrough;
        }
        let Some(range) = self.selection.range() else {
            return SelectionDeleteOutcome::FallThrough;
        };
        let Ok(terminal) = self.terminal.lock() else {
            return SelectionDeleteOutcome::FallThrough;
        };
        let modes = key_modes_from_core(terminal.keyboard_modes());
        // R1: region geometry is computed in core (B-DESIGN B0/B2). No region
        // means no editable input (mark missing => the caller's separate hint
        // path; mark present but nothing typed => pre-existing fall-through).
        let Some(region) = terminal.input_region() else {
            return SelectionDeleteOutcome::FallThrough;
        };
        let scrollback_len = terminal.screen().scrollback_len();
        let cursor = terminal.screen().cursor();
        // Scope the ladder to selections that touch the input region's rows;
        // anything else keeps today's fall-through contract (a selection over
        // unrelated output does not hijack Delete/Backspace).
        let touches_region = range.start.row <= region.end_row && range.end.row >= region.start_row;
        if !touches_region {
            return SelectionDeleteOutcome::FallThrough;
        }
        // R2: certainty Unknown (stale mark / hard newlines) can never back a
        // real buffer edit — unconditional no-op.
        if region.certainty == InputCertainty::Unknown {
            return SelectionDeleteOutcome::NoOpWithHint;
        }
        // R3, NF14-R (operator-ruled Option R): a SINGLE-ROW RightEdgeUnknown
        // region falls through to the R4 clamp below using the core-computed
        // heuristic right edge (last non-blank cell) — the shipped pre-B2
        // behavior. This deliberately re-accepts the bounded risk that a
        // right-aligned decoration on the input row is treated as input,
        // because the strict ODP-3 gate made select+Delete a complete no-op
        // for every shell without the private edit-region OSC (PowerShell on
        // Windows, bash, fish mid-edit). MULTI-ROW RightEdgeUnknown stays a
        // no-op: without a trustworthy edge, row joins cannot anchor a
        // synthesized multi-row edit (the ODP-3 remainder).
        if region.certainty == InputCertainty::RightEdgeUnknown
            && region.start_row != region.end_row
        {
            return SelectionDeleteOutcome::NoOpWithHint;
        }
        // R5 (B1 soft-wrap slice): an Exact multi-row region whose joins are
        // all soft wraps is ONE logical line; flatten the core-provided spans
        // and synthesize horizontal-only motion. Hard newlines never reach
        // here (they force Unknown at R2 under the ODP-2 default) and R6
        // remains the gated extension point.
        if region.start_row != region.end_row {
            let snapshot = terminal.snapshot_with_scrollback(0);
            return flattened_selection_delete_outcome(
                &snapshot,
                &region,
                range,
                cursor,
                scrollback_len,
                self.grid.rows,
                modes,
            );
        }
        let input_row = region.start_row;
        let Some(visible_row) = input_row.checked_sub(scrollback_len) else {
            return SelectionDeleteOutcome::FallThrough;
        };
        if visible_row >= self.grid.rows {
            return SelectionDeleteOutcome::FallThrough;
        }
        // R4: clamp the selection to the exact input span [start_col, end_col).
        let Some((selected_start, selected_end)) =
            selected_columns_on_row(range, input_row, self.grid.columns)
        else {
            // Touches the region span but not this row: single-row region, so
            // unreachable in practice; be conservative.
            return SelectionDeleteOutcome::NoOpWithHint;
        };
        let Some(editable_end) = region.end_col.checked_sub(1) else {
            return SelectionDeleteOutcome::NoOpWithHint;
        };
        let start = selected_start.max(region.start_col);
        let end = selected_end.min(editable_end);
        if start > end {
            // Selection is on the input row but entirely over non-input cells
            // (prompt, right-aligned decoration, autosuggestion): ladder
            // default => no-op, never a stray Delete byte.
            return SelectionDeleteOutcome::NoOpWithHint;
        }
        let snapshot = terminal.snapshot_with_scrollback(0);
        let text = snapshot_row_text(&snapshot, visible_row, start, end);
        let delete_count = snapshot_row_cell_count(&snapshot, visible_row, start, end);
        if text.is_empty() || delete_count == 0 {
            return SelectionDeleteOutcome::NoOpWithHint;
        }
        match delete_selection_bytes(&snapshot, visible_row, start, cursor, delete_count, modes) {
            Some(edit_bytes) => {
                SelectionDeleteOutcome::Synthesize(EditableInputSelection { text, edit_bytes })
            }
            None => SelectionDeleteOutcome::NoOpWithHint,
        }
    }

    pub(super) fn prompt_input_mark_missing_for_context_menu(&self) -> bool {
        if self.selection_block || self.viewport.offset() != 0 || self.selection.range().is_none() {
            return false;
        }
        self.terminal
            .lock()
            .ok()
            .is_some_and(|terminal| terminal.active_prompt_input_start().is_none())
    }

    pub(super) fn handle_context_menu_cut(&mut self) {
        let Some(selection) = self.editable_input_selection_for_context_menu() else {
            return;
        };
        // Fail-safe: if the clipboard write fails, do not delete the editable
        // input and do not clear the selection — the text stays in-place as
        // if Cut had not been invoked. Only proceed with the delete when the
        // write actually succeeded (D-IN2-CUT-SAFE).
        if self.clipboard.write_text(&selection.text).is_none() {
            return;
        }
        self.delete_editable_input_selection(selection);
    }

    pub(super) fn handle_context_menu_delete(&mut self) {
        let Some(selection) = self.editable_input_selection_for_context_menu() else {
            return;
        };
        self.delete_editable_input_selection(selection);
    }

    /// SELDEL-KEY: delete the selected editable prompt input via the Delete /
    /// Backspace key, sharing the exact gated path as the right-click Delete /
    /// Cut. Returns `true` (the key was consumed) only when a local selection
    /// intersects the current editable input line — i.e. the same condition that
    /// enables the menu's Delete item (shell integration reported the input
    /// boundary, the selection is on the live prompt row, and the viewport is at
    /// the live tail). Returns `false` in every other case so the caller falls
    /// through to the normal key encode and the byte sent to the shell is
    /// unchanged: Delete/Backspace with no selection, with a selection that is
    /// not on editable input, or without shell integration all behave exactly as
    /// before.
    pub(super) fn try_delete_selected_editable_input(&mut self) -> bool {
        match self.selection_delete_outcome() {
            SelectionDeleteOutcome::Synthesize(selection) => {
                self.delete_editable_input_selection(selection);
                true
            }
            // Ladder rungs R2/R3 + default (B-DESIGN §4): the selection is on
            // the input region but the geometry cannot back a real buffer
            // edit. Consume the key — never forward a blind Delete/Backspace —
            // clear the selection, and (NF17) raise the geometry-unavailable
            // hint. The input mark IS present here, so shell integration is
            // active; pointing at Settings would be misleading.
            SelectionDeleteOutcome::NoOpWithHint => {
                self.selection.clear();
                self.selection_block = false;
                self.show_selection_geometry_hint(std::time::Instant::now());
                self.request_selection_redraw();
                true
            }
            SelectionDeleteOutcome::FallThrough => false,
        }
    }

    pub(super) fn try_handle_unavailable_selection_delete(&mut self) -> bool {
        if !self.prompt_input_mark_missing_for_context_menu() {
            return false;
        }
        self.selection.clear();
        self.selection_block = false;
        self.show_shell_integration_hint(std::time::Instant::now());
        self.request_selection_redraw();
        true
    }

    fn delete_editable_input_selection(&mut self, selection: EditableInputSelection) {
        self.return_to_live();
        self.write_pty_bytes(&selection.edit_bytes);
        self.selection.clear();
        self.selection_block = false;
        self.request_selection_redraw();
    }

    pub(super) fn write_primary_selection(&mut self) {
        let Some(text) = self.current_selection_text() else {
            return;
        };
        let _ = self.clipboard.write_primary_text(text.as_str());
    }

    /// Reset terminal-grid pointer state when entering any overlay mode.
    ///
    /// An open overlay captures the pointer (UX4-P1), so any in-progress local
    /// selection — and crucially any TUI mouse-report button still held from a
    /// press before the overlay opened — must be cleared on entry. Overlay
    /// presses short-circuit before `handle_reported_mouse_input` and overlay
    /// releases are inert, so a stale `report_button` would otherwise survive
    /// the overlay and re-enter the held-button motion path after it closes.
    /// Clearing on entry is sufficient: nothing can re-arm `report_button` while
    /// the overlay is open, so it is guaranteed `None` on close.
    pub(super) fn reset_pointer_state_for_overlay(&mut self) {
        self.selection.clear();
        self.selection_block = false;
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.report_button = None;
        // An open target's press may open an overlay (the image lightbox); the
        // paired release is then consumed by the overlay, so drop the swallow
        // latch here too and never carry it past the overlay.
        self.swallow_open_left_release = false;
        // WHEEL-SENS (T-reset): clear the wheel carry on overlay entry so a
        // partial grid-scroll notch does not bleed into the overlay list scroll
        // (and vice-versa) once the overlay captures the wheel.
        self.wheel_accum.reset();
        // SCROLL-FEEL Tier 2: drop any sub-row scroll remainder too, so a
        // partial continuous glide does not resume against the overlay.
        let token = self.sessions.active_id();
        self.clear_scroll_frac_of(token);
        // P1-8: clear the overlay damper's pixel carry too, so a partial
        // terminal-scroll flick can't seed the first overlay detent.
        self.overlay_wheel.reset();
    }

    /// On focus loss, abandon any in-progress overlay slider drag (UX4-P2).
    ///
    /// A press may arm a drag whose release is then delivered to another window
    /// after an alt-tab; the overlay stays open, so without this the drag
    /// survives and the next bare hover Move on focus regain commits a phantom
    /// value (the overlay-stays-open analogue of the close/reopen lost-release
    /// case). No-op unless the overlay is open with a drag armed.
    pub(super) fn cancel_overlay_drag_on_focus_loss(&mut self) {
        if self.overlay.is_open() {
            self.overlay.cancel_settings_drag();
            // SLIDER-GUARD: clear the held flag so a focus-regain move cannot
            // advance a stale drag even if the Release event was lost.
            self.overlay_left_held = false;
        }
    }

    // -----------------------------------------------------------------------
    // TOP-TAB-DRAG: drag-to-reorder tabs in the horizontal strip
    // -----------------------------------------------------------------------

    pub(super) fn begin_top_tab_drag(&mut self, idx: usize) {
        match self.pointer_px {
            Some((x, y)) => {
                self.top_tab_drag = Some(TopTabDrag::new(idx, x, y));
                self.invalidate_chrome_drag_frame();
            }
            None => self.activate_tab(idx),
        }
    }

    pub(super) fn drag_top_tab_to_pointer(&mut self, x_px: f64, y_px: f64, cell: CellSize) {
        let Some(mut drag) = self.top_tab_drag else {
            return;
        };
        let armed = drag.update_arm(x_px, y_px);
        if armed
            && let Some(insert) = self.tab_bar.drop_index(
                x_px,
                &self.sessions,
                self.tab_bar_grid_cols(),
                cell,
                self.gpu
                    .as_ref()
                    .map(GpuState::window_padding)
                    .unwrap_or(WindowPadding::ZERO),
            )
        {
            drag.drop_idx = insert;
        }
        self.top_tab_drag = Some(drag);
        if armed {
            self.apply_cursor_icon(CursorIcon::Grabbing);
            self.invalidate_chrome_drag_frame();
        }
    }

    pub(super) fn finish_top_tab_drag(&mut self) {
        let Some(drag) = self.top_tab_drag.take() else {
            return;
        };
        if drag.armed {
            let _ = self.sessions.reorder_tab(drag.origin_idx, drag.drop_idx);
        } else {
            self.activate_tab(drag.origin_idx);
        }
        self.apply_cursor_icon(CursorIcon::Default);
        self.invalidate_chrome_drag_frame();
    }

    pub(super) fn cancel_top_tab_drag(&mut self) -> bool {
        if self.top_tab_drag.take().is_none() {
            return false;
        }
        self.apply_cursor_icon(CursorIcon::Default);
        self.invalidate_chrome_drag_frame();
        true
    }

    fn activate_tab(&mut self, idx: usize) {
        let Some(token) = self.sessions.token_at_position(idx) else {
            return;
        };
        if self.sessions.switch(token) {
            self.on_active_session_changed();
        }
    }

    // -----------------------------------------------------------------------
    // RAIL-DRAG: drag-to-reorder workspaces in the rail
    // -----------------------------------------------------------------------

    /// Arm a drag-to-reorder gesture on the workspace slot at `idx` (RAIL-DRAG).
    /// Records the press position as the threshold origin; the gesture stays a
    /// click (no reorder) until motion crosses `CHROME_DRAG_THRESHOLD_PX`. Setting
    /// `rail_ws_drag` is itself what holds an auto-hide rail open for the
    /// gesture: `rail_pinned_open()` reads `rail_ws_drag.is_some()`, so the rail
    /// stays revealed for the drag's whole lifetime and the drop target can
    /// never vanish mid-drag — this method sets no separate pin flag, the live
    /// drag IS the pin. If the press position is somehow unknown, degrade to a
    /// plain activate so the slot is never left inert.
    pub(super) fn begin_workspace_drag(&mut self, idx: usize) {
        match self.pointer_px {
            Some((x, y)) => {
                self.rail_ws_drag = Some(RailWorkspaceDrag::new(idx, x, y));
                self.invalidate_chrome_drag_frame();
            }
            None => self.activate_workspace(idx),
        }
    }

    /// Track pointer motion during a workspace-rail drag (RAIL-DRAG): promote the
    /// gesture to armed past the movement threshold, then recompute the live
    /// drop-target insertion index from the pointer Y and repaint. A no-op when
    /// no rail drag is in flight. Called from the pointer-move path before any
    /// other hover / selection work so a drag owns the pointer.
    pub(super) fn drag_workspace_to_pointer(&mut self, x_px: f64, y_px: f64, cell: CellSize) {
        let Some(mut drag) = self.rail_ws_drag else {
            return;
        };
        let armed = drag.update_arm(x_px, y_px);
        if armed && let Some(insert) = self.workspace_rail_drop_index(y_px, cell) {
            drag.drop_idx = insert;
        }
        self.rail_ws_drag = Some(drag);
        if armed {
            // A grabbing cursor and a fresh frame for the whole drag. The proxy
            // follows every pointer sample, even while its insertion boundary
            // remains unchanged.
            self.apply_cursor_icon(CursorIcon::Grabbing);
            self.invalidate_chrome_drag_frame();
        }
    }

    /// End a workspace-rail drag on button release (RAIL-DRAG). An armed gesture
    /// commits the reorder (walking the shipped single-step `move_workspace`
    /// engine so active-follow-by-identity and persisted order are reused
    /// verbatim); an un-armed one is a plain click that activates the pressed
    /// workspace. Always clears the drag state (releasing the auto-hide hold) and
    /// repaints so the drop indicator disappears.
    pub(super) fn finish_workspace_drag(&mut self) {
        let Some(drag) = self.rail_ws_drag.take() else {
            return;
        };
        if drag.armed {
            self.commit_workspace_drag(drag.origin_idx, drag.drop_idx);
        } else {
            self.activate_workspace(drag.origin_idx);
        }
        self.apply_cursor_icon(CursorIcon::Default);
        self.invalidate_chrome_drag_frame();
    }

    /// Cancel an in-flight workspace-rail drag (RAIL-DRAG) with the rail order
    /// untouched — the Escape path. Returns `true` when a drag was actually
    /// cancelled (so the key handler can consume the Escape), `false` when none
    /// was in flight (the key falls through to its normal meaning). Releases the
    /// auto-hide hold and repaints to drop the indicator.
    pub(super) fn cancel_workspace_drag(&mut self) -> bool {
        if self.rail_ws_drag.take().is_none() {
            return false;
        }
        self.apply_cursor_icon(CursorIcon::Default);
        self.invalidate_chrome_drag_frame();
        true
    }

    /// Dirty both the frame gate and the retained-geometry signature for rail
    /// drag visuals. `needs_rebuild` alone is insufficient: workspace order and
    /// pinned rail chrome are not terminal revisions, so the renderer can
    /// otherwise classify the rebuilt snapshot as retained and re-present the
    /// previous GPU geometry until an unrelated presentation epoch changes.
    fn invalidate_chrome_drag_frame(&mut self) {
        self.needs_rebuild = true;
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Commit a rail drag: move the workspace at `from` to insertion index `to`
    /// (0..=count, as `TabRail::drop_index` returns), by walking the shipped
    /// single-step `move_workspace` one slot at a time. Reusing that engine — not
    /// a bespoke splice — keeps the active-follow-by-identity and shape-snapshot
    /// persistence semantics identical to the context-menu Move Up/Down path.
    /// Flashes the rail and requests a redraw when the order actually changed.
    fn commit_workspace_drag(&mut self, from: usize, to: usize) {
        let count = self.sessions.workspace_count();
        if from >= count {
            return;
        }
        // An insertion index past `from` lands the item one slot lower than the
        // gap index (the item vacates its own slot as it moves down).
        let dest = if to > from { to - 1 } else { to };
        let dest = dest.min(count.saturating_sub(1));
        let mut cur = from;
        let mut moved = false;
        while cur > dest {
            if !self.sessions.move_workspace(cur, true) {
                break;
            }
            cur -= 1;
            moved = true;
        }
        while cur < dest {
            if !self.sessions.move_workspace(cur, false) {
                break;
            }
            cur += 1;
            moved = true;
        }
        if moved {
            self.flash_rail_autohide();
            self.request_selection_redraw();
        }
    }

    /// The live drop-target insertion index for the current pointer Y during a
    /// workspace-rail drag (RAIL-DRAG), resolved against whichever rail geometry
    /// is live this frame: the floating overlay band under auto-hide, else the
    /// pinned reservation. `None` off a rail / with no slots. Mirrors the mode
    /// split in `current_chrome_hit` so the drop math matches the hit-test.
    fn workspace_rail_drop_index(&self, y_px: f64, cell: CellSize) -> Option<usize> {
        let source = self.sessions.rail_source();
        let (cols, origin) = if self.rail_autohide_active() {
            let side = self.rail_autohide_side()?;
            (
                self.rail_overlay_cols(),
                self.rail_overlay_origin_px(cell, side),
            )
        } else if self.should_show_workspace_rail() {
            (self.rail_cols(), self.rail_origin_px(cell))
        } else {
            return None;
        };
        self.tab_rail.drop_index(
            y_px,
            &source,
            cols,
            self.tab_rail_grid_rows(),
            origin,
            cell,
            self.rail_geom(),
        )
    }
}

fn selected_columns_on_row(
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

fn snapshot_row_text(snapshot: &Snapshot, row: usize, start: usize, end: usize) -> String {
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

fn snapshot_row_cells(
    snapshot: &Snapshot,
    row: usize,
    start: usize,
    end: usize,
) -> impl Iterator<Item = &crate::core::Cell> {
    let columns = snapshot.dimensions.columns;
    let offset = row * columns;
    snapshot.cells[offset + start..=offset + end].iter()
}

fn delete_selection_bytes(
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
fn flattened_selection_delete_outcome(
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

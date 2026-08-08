// SPDX-License-Identifier: GPL-3.0-only
//! Presentation-facing session state: cursor comparison, titles, viewport
//! anchoring, animation timers, input latches, pane geometry, activity, and the
//! tab-bar data sources.
//!
//! Nothing here spawns, closes, or otherwise touches a backend. Timers park for
//! every pane without a render consumer, and only a visible pane of the active
//! tab can fan a redraw out.

use super::model::{Session, SessionToken, WorkspaceSet};
use crate::core::Snapshot;
use crate::native::app::TabBarSource;
use crate::native::layout::{
    FocusDir, PaneRect, SplitAxis, divider_at_point, divider_axis_at_point,
    divider_rects_with_axis, drag_divider_to, focus_move, layout_rects, pane_at_point,
    snap_divider_to_cells,
};
use crate::selection::PointerDrag;

/// Cursor-motion comparison metadata: the undecorated content snapshot's
/// cursor and dimensions. See `last_cursor_comparison_snapshot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native) struct CursorComparison {
    pub(in crate::native) cursor: crate::core::Position,
    pub(in crate::native) dimensions: crate::core::Dimensions,
}

impl CursorComparison {
    pub(in crate::native) fn of(snapshot: &Snapshot) -> Self {
        Self {
            cursor: snapshot.cursor,
            dimensions: snapshot.dimensions,
        }
    }
}

/// The result of one arena-wide bell / prompt-marks drain
/// ([`WorkspaceSet::drain_bells`]). The App turns this into a viewport flash,
/// window urgency, and a prompt-marks epoch bump; the per-tab activity latch is
/// applied inside the drain (it needs the token->tab mapping).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct BellSweep {
    /// The active-visible focused pane rang this pass — drives today's viewport
    /// flash (byte-identical single-pane behavior).
    pub(in crate::native) focused_bell: bool,
    /// The active-visible focused pane's prompt marks changed AND the
    /// command-status gutter is on — drives the prompt-marks epoch bump.
    pub(in crate::native) focused_prompt_changed: bool,
    /// At least one NON-focused session rang — drives window urgency
    /// (`request_user_attention`); the specific tabs are latched in the drain.
    pub(in crate::native) background_bell: bool,
}

impl TabBarSource for WorkspaceSet {
    fn tab_count(&self) -> usize {
        self.active_workspace().tabs.len()
    }

    fn tab_title(&self, idx: usize) -> &str {
        let Some(tab) = self.active_workspace().tabs.get(idx) else {
            return "odytty";
        };
        if let Some(name) = &tab.title_override {
            return name.as_str();
        }
        self.sessions
            .get(&tab.focused)
            .map(|session| session.tab_title.as_str())
            .unwrap_or("odytty")
    }

    fn active_tab(&self) -> usize {
        self.active_workspace().active_tab
    }
}

/// A borrow of the workspace list presented through [`TabBarSource`] so the F4
/// rail widget renders and hit-tests the WORKSPACES (name / active / count)
/// rather than the active workspace's tabs (design doc §7.1). The rail's
/// `TabHit::Switch(idx)` then dispatches to [`WorkspaceSet::switch_workspace`]
/// instead of `switch`; `TabHit::NewTab` (the `+` slot) creates a workspace.
/// Presentation-only: it reads `workspaces` directly, so it carries no per-tab
/// title-override or session lookup — a workspace's label is its `name`.
pub(in crate::native) struct WorkspaceRailSource<'a> {
    set: &'a WorkspaceSet,
}

impl TabBarSource for WorkspaceRailSource<'_> {
    fn tab_count(&self) -> usize {
        self.set.workspaces.len()
    }

    fn tab_bound(&self, idx: usize) -> bool {
        self.set
            .workspaces
            .get(idx)
            .is_some_and(|ws| ws.default_profile.is_some())
    }

    fn tab_title(&self, idx: usize) -> &str {
        self.set
            .workspaces
            .get(idx)
            .map(|ws| ws.name.as_str())
            .unwrap_or("workspace")
    }

    fn active_tab(&self) -> usize {
        self.set.active_ws
    }
}

impl Session {
    pub(in crate::native) fn refresh_tab_title(&mut self) {
        self.tab_title = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.title().map(ToOwned::to_owned))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "odytty".to_owned());
    }

    /// Anchor this pane's scrollback viewport across output growth and refresh
    /// its growth baseline, returning the live offset to snapshot at. This is
    /// the single "stay scrolled" bookkeeping shared by the single-pane render
    /// path and the multipane rebuild loop so the two can never diverge: a
    /// scrolled-back pane (foreground, background split, or the focused pane of
    /// a split tab) stays pinned to the same absolute rows as fresh PTY output
    /// arrives, and the baseline stays current so collapsing a split back to a
    /// single pane applies no accumulated jump. A no-op at the live tail
    /// (offset 0) and when nothing grew.
    pub(in crate::native) fn anchor_viewport_for_render(&mut self, scrollback_len: usize) -> usize {
        let added = scrollback_len.saturating_sub(self.last_scrollback_len);
        self.viewport.anchor_after_growth(added, scrollback_len);
        self.last_scrollback_len = scrollback_len;
        self.viewport.clamp(scrollback_len);
        self.viewport.offset()
    }

    /// Settle the cursor-animation timers — blink phase, ID1 easing fade, VE4
    /// slide — to their at-rest identity with no scheduled wake. These are the
    /// timers whose consumer is the focused render path's per-frame poll
    /// (`cursor_blink.poll` / `update_cursor_easing` / `update_cursor_motion`),
    /// so a pane with no render consumer strands its past toggle deadline in
    /// the wake set and busy-spins. Background panes are never rendered and use
    /// this reset; the focused pane of either a single-pane or split tab has a
    /// matching consumer. Idempotent; every animation re-arms from the current
    /// frame time when the pane next receives focus.
    pub(in crate::native) fn park_cursor_timers(&mut self) {
        self.cursor_blink.park();
        self.cursor_anim_alpha = 1.0;
        self.cursor_ease_deadline = None;
        self.cursor_ease_phase_on = true;
        self.cursor_ease_toggle_at = None;
        self.cursor_anim_offset = [0.0, 0.0];
        self.cursor_slide_deadline = None;
        self.cursor_slide_start = None;
        self.cursor_slide_from_px = [0.0, 0.0];
        self.cursor_streak.park();
    }

    /// Settle every timer of a never-rendered (background) pane: the cursor
    /// timers above PLUS the synchronized-output hold. A background pane is
    /// never rendered, so none of these has a consumer (NF20-B). The
    /// synchronized-output hold is parked ONLY here: unlike the cursor timers it
    /// is consumed by `should_hold` in the render branch (which runs before the
    /// single/multi split) and its 150 ms deadline is the crash-protection
    /// watchdog that auto-releases a frozen display — so the focused pane of a
    /// multi-pane tab keeps its hold live (parking it would defeat the watchdog)
    /// and parks only its cursor timers.
    pub(in crate::native) fn park_animation_timers(&mut self) {
        self.park_cursor_timers();
        self.synchronized_output_hold.clear();
    }

    /// Clear every piece of UI state whose coordinates are tied to the row /
    /// scrollback layout, so a reflow never leaves a selection, hover span,
    /// search match, hint label, or copy-mode caret pointing at cells the text
    /// no longer occupies.
    ///
    /// Run for EVERY session a resize reflows, not just the active one (NF21-3):
    /// [`WorkspaceSet::resize_all_panes`] reflows every tab's panes, but the clear that
    /// followed it in `App::apply_grid_resize` went through `Deref` = the ACTIVE
    /// session only. A background tab that crossed the reflow keeping stale
    /// absolute-row coordinates would, on switch-back, highlight the wrong text
    /// and copy the wrong bytes. The field set and order match that former
    /// active-only block exactly, so the active-tab path stays byte-identical.
    pub(in crate::native) fn invalidate_layout_dependent_state(&mut self) {
        self.selection.clear();
        self.selection_block = false;
        self.pointer_drag = PointerDrag::None;
        self.drag_anchor_unit = None;
        self.last_selection_autoscroll = None;
        self.report_button = None;
        self.swallow_open_left_release = false;
        // B3: a reflow strands the latched press's viewport coordinates; the
        // same-span release check would misfire against re-wrapped rows.
        self.pressed_button = None;
        self.pointer_cell = None;
        self.pointer_px = None;
        self.hovered_hyperlink = None;
        self.hovered_path = None;
        // UX-A (Phase 11): drop the armed-underline span alongside the hovered
        // path it mirrors; a reflow makes its old row coords stale.
        self.hovered_path_cells = None;
        // INTERACTIVE-URLS: drop the hovered-URL span too; its row coords are
        // equally stale after a reflow.
        self.hovered_url = None;
        self.hovered_url_cells = None;
        // Reflow changes the row/scrollback layout; return to the live bottom so
        // the offset is never stale against the new geometry.
        self.viewport.reset_to_live();
        // Search closes because its absolute row matches were computed against
        // the old layout.
        self.search.reset_for_reflow();
        self.search_restore_viewport = None;
        // HINTS label spans are absolute rows against the old layout; a reflow
        // makes them stale, so close the modal.
        self.hints = None;
        // COPY-MODE (C13): the caret + selection anchor are absolute-buffer
        // coords computed against the old scrollback/row layout; a reflow
        // re-wraps those rows and leaves them stale. Close the modal alongside
        // the other absolute-row overlays.
        self.copy_mode = None;
    }

    /// Drop the transient pointer-input latches so an active-session change
    /// cannot leave them stranded on the outgoing session or phantom-hovering on
    /// the incoming one (NF21-8 / NF21-9). Unlike
    /// [`Self::invalidate_layout_dependent_state`] this deliberately leaves the
    /// selection, viewport, search, hints and copy-mode state untouched — a tab
    /// or workspace switch is not a reflow, so a made selection must survive to
    /// be copied on switch-back. Only the in-flight drag and the last hover cell
    /// are cleared: a mid-drag switch must not resurrect a buttonless
    /// `Selecting` latch, and a stale `pointer_cell` must not paint a phantom
    /// hover (or open a stale Ctrl+click target) before the first real
    /// `CursorMoved` on the new surface.
    pub(in crate::native) fn clear_input_latches(&mut self) {
        self.pointer_drag = PointerDrag::None;
        self.pointer_cell = None;
        self.pointer_px = None;
        self.report_button = None;
    }
}

impl WorkspaceSet {
    /// Park the animation / render-hold timers of every pane that has no render
    /// consumer this frame, matching the fan-out of the `next_wake_deadline`
    /// sources with a consumer of equal reach (NF20-B / NF21-1).
    ///
    /// Consumer scope (§5 rule 2): the only pane with live animation timers is
    /// the focused pane of the active tab of the ACTIVE WORKSPACE — everything
    /// else (all background workspaces, all background tabs, all non-focused
    /// panes) is parked. Collectors iterate the flat arena (§5 rule 1), never
    /// the hierarchy; "active" is resolved once through `active_focused_token`
    /// so this and the redraw gate can never disagree about which pane is live.
    ///
    /// - Every pane of an inactive tab (in any workspace) and every non-focused
    ///   pane of the active tab is never rendered → fully parked
    ///   (`park_animation_timers`).
    /// - The focused pane of the active tab keeps ALL its timers. Both the
    ///   single-pane path and `rebuild_multipane` poll its blink/ease/slide;
    ///   `should_hold` consumes its render hold before either branch.
    ///
    /// Idempotent; cheap (few panes).
    pub(in crate::native) fn park_background_timers(&mut self) {
        let active = self.active_focused_token();
        for (token, session) in self.sessions.iter_mut() {
            if *token != active {
                session.park_animation_timers();
            }
        }
    }

    /// True when any currently visible pane of the active tab has its
    /// `needs_rebuild` flag set. The render gate ORs this across the whole tab so
    /// output streaming into a non-focused split pane repaints even while the
    /// focused pane is idle (NF21-7) — `self.needs_rebuild` alone is the focused
    /// pane's flag (the `Deref` target). For a single-pane tab this is exactly
    /// the focused pane's flag, so the single-pane gate decision is unchanged.
    pub(in crate::native) fn any_visible_pane_needs_rebuild(&self) -> bool {
        self.active_visible_tokens()
            .into_iter()
            .any(|token| self.sessions.get(&token).is_some_and(|s| s.needs_rebuild))
    }

    /// True when any currently visible pane of the active tab has an in-flight
    /// SCROLL-GLIDE follower. The multipane wake path sources a frame-paced
    /// repaint off this so a split's per-pane glide advances every frame until it
    /// settles (mirrors the focused-only `scroll_glide_deadline` for single-pane).
    /// For a single-pane tab this is exactly the focused pane's `glide_active`.
    pub(in crate::native) fn any_visible_pane_gliding(&self) -> bool {
        self.active_visible_tokens()
            .into_iter()
            .any(|token| self.sessions.get(&token).is_some_and(|s| s.glide_active))
    }

    /// Clear `needs_rebuild` on every visible pane of the active tab. Paired with
    /// [`Self::any_visible_pane_needs_rebuild`]: `rebuild_multipane` snapshots
    /// every visible pane, so it must clear every visible pane's flag — clearing
    /// only the focused pane's would leave a dirtied background pane's flag set
    /// and re-open the (now tab-wide) gate every frame, a rebuild storm (NF21-7).
    pub(in crate::native) fn clear_visible_pane_rebuild_flags(&mut self) {
        for token in self.active_visible_tokens() {
            if let Some(session) = self.sessions.get_mut(&token) {
                session.needs_rebuild = false;
            }
        }
    }

    /// The tokens of the panes currently on screen for the active tab: just the
    /// focused pane while zoomed (only it is rendered), otherwise every leaf of
    /// the active tab's layout. Mirrors [`Self::is_visible_pane`]'s membership.
    pub(super) fn active_visible_tokens(&self) -> Vec<SessionToken> {
        match self.active_tab_ref() {
            Some(tab) if tab.is_effectively_zoomed() => vec![tab.focused],
            Some(tab) => tab.layout.leaves(),
            None => Vec::new(),
        }
    }

    /// Clear the pointer-input latches on EVERY session in the arena (NF21-8 /
    /// NF21-9). Called from the active-session-change seam, which fires on both
    /// tab and workspace switches post-W1: sweeping the flat arena covers the
    /// outgoing session (whose in-flight drag must not survive), the incoming
    /// session (whose stale hover cell must not paint), and every background
    /// session across all workspaces in one pass. Selection and viewport state
    /// are intentionally preserved — see [`Session::clear_input_latches`].
    pub(in crate::native) fn clear_all_input_latches(&mut self) {
        for session in self.sessions.values_mut() {
            session.clear_input_latches();
        }
    }

    /// Clear stale absolute-coordinate state after scrollback front eviction.
    /// The terminal pump mutates the model asynchronously, so the app calls
    /// this at the start of each redraw before clipboard requests or painting.
    pub(in crate::native) fn reconcile_scrollback_trims(&mut self) {
        for session in self.sessions.values_mut() {
            let epoch = crate::native::lock_recover(&session.terminal).scrollback_trim_epoch();
            if epoch != session.last_scrollback_trim_epoch {
                session.invalidate_layout_dependent_state();
                session.last_scrollback_trim_epoch = epoch;
            }
        }
    }

    /// True when `token` is a currently visible pane of the **active** tab —
    /// i.e. its output should drive a redraw even when it is not the focused
    /// pane (design doc §2.5 audit row #4: redraw suppression must key on "any
    /// visible pane of the active tab", not just the focused one). For a
    /// single-pane tab this is exactly `active_id() == token`, so the
    /// single-pane redraw decision is unchanged.
    pub(in crate::native) fn is_visible_pane(&self, token: SessionToken) -> bool {
        match self.active_tab_ref() {
            // While zoomed only the focused pane is on screen, so background
            // panes' output must not drive a redraw (it would not be visible).
            Some(tab) if tab.is_effectively_zoomed() => tab.focused == token,
            Some(tab) => tab.layout.contains(token),
            None => false,
        }
    }

    /// Reconcile the scrollback-growth baseline (`last_scrollback_len`) of every
    /// pane of the active tab to its terminal's current scrollback length,
    /// WITHOUT anchoring the viewport. Called on activation (tab / workspace
    /// switch): a tab keeps producing output while it is backgrounded, but it is
    /// not rendered, so `anchor_viewport_for_render` never runs and its baseline
    /// freezes at the length from its last on-screen frame. Without this, the
    /// first render after switching back computes `added = current - stale`
    /// (all the backgrounded growth at once) and `anchor_after_growth` yanks a
    /// scrolled-up viewport toward the top of scrollback, stranding fresh output
    /// offscreen below — the user returns to a tab "stuck scrolled up" with new
    /// output invisible. Treating the backgrounded growth as already-past
    /// preserves the pane's scroll position across the switch: a pane at the
    /// live bottom (offset 0) stays live, and a scrolled-up pane keeps its
    /// offset relative to the now-current bottom rather than jumping into deep
    /// history. This is the viewport analogue of the new-output-fade
    /// discontinuity the activation path already clears (NF21-12). A no-op for a
    /// tab that was never backgrounded (its baseline already equals its current
    /// length). Platform-neutral: viewport/scrollback bookkeeping is identical
    /// on Unix and Windows.
    pub(in crate::native) fn reconcile_active_tab_scroll_baselines(&mut self) {
        let Some(tab) = self.active_tab_ref() else {
            return;
        };
        for token in tab.layout.leaves() {
            if let Some(session) = self.sessions.get_mut(&token) {
                let len = crate::native::lock_recover(&session.terminal)
                    .screen()
                    .scrollback_len();
                session.last_scrollback_len = len;
            }
        }
    }

    /// The (token, pixel-rect) layout of the **active** tab's panes within
    /// `content`, for the multi-pane render dispatch. Single-pane tabs yield one
    /// entry spanning the whole content rect — identical geometry to the
    /// single-pane path, which never calls this.
    pub(in crate::native) fn active_pane_rects(
        &self,
        content: PaneRect,
        divider_px: f32,
    ) -> Vec<(SessionToken, PaneRect)> {
        match self.active_tab_ref() {
            // Zoomed tab: only the focused pane is rendered, spanning the whole
            // content rect (the layout tree underneath is untouched, so un-zoom
            // restores the prior geometry exactly).
            Some(tab) if tab.is_effectively_zoomed() => vec![(tab.focused, content)],
            Some(tab) => layout_rects(&tab.layout, content, divider_px),
            None => Vec::new(),
        }
    }

    /// The pane of the active tab under a pixel point, or `None` in a divider
    /// gap / outside content. Focus-follows-click resolves the clicked pane
    /// through this (design doc §4.3 / audit row #6).
    pub(in crate::native) fn active_pane_at_point(
        &self,
        content: PaneRect,
        divider_px: f32,
        x: f32,
        y: f32,
    ) -> Option<SessionToken> {
        let rects = self.active_pane_rects(content, divider_px);
        pane_at_point(&rects, x, y)
    }

    /// Move focus within the active tab to the spatial neighbor of the focused
    /// pane in direction `dir` (tmux `Ctrl-b` arrows, §4.3 / §7). Builds the
    /// pane rects within `content` and resolves the neighbor via
    /// [`layout::focus_move`]. Returns true if focus changed.
    pub(in crate::native) fn focus_move_active(
        &mut self,
        content: PaneRect,
        divider_px: f32,
        dir: FocusDir,
    ) -> bool {
        let focused = self.active_id();
        let rects = self.active_pane_rects(content, divider_px);
        match focus_move(&rects, focused, dir) {
            Some(target) => self.set_active_focus(target),
            None => false,
        }
    }

    /// The tree-order index of the active tab's divider under a pixel point
    /// (widened by `grab_px`), to start a divider drag. `None` when no divider
    /// is grabbed.
    pub(in crate::native) fn active_divider_at_point(
        &self,
        content: PaneRect,
        divider_px: f32,
        x: f32,
        y: f32,
        grab_px: f32,
    ) -> Option<usize> {
        self.active_tab_ref()
            // No dividers are drawn while zoomed, so none can be grabbed.
            .filter(|tab| !tab.is_effectively_zoomed())
            .and_then(|tab| divider_at_point(&tab.layout, content, divider_px, x, y, grab_px))
    }

    /// The [`SplitAxis`] of the active tab's divider under a pixel point (widened
    /// by `grab_px`), or `None` when the point is over no divider. Drives the
    /// hover resize-cursor affordance (`ColResize` for a column split's vertical
    /// divider, `RowResize` for a row split's horizontal one). Mirrors
    /// [`Self::active_divider_at_point`]'s zoom and hit-test gating so hover and
    /// grab agree. A single-pane tab has no dividers, so this is always `None`
    /// there — the byte-identical path never sees a resize cursor.
    pub(in crate::native) fn active_divider_axis_at_point(
        &self,
        content: PaneRect,
        divider_px: f32,
        x: f32,
        y: f32,
        grab_px: f32,
    ) -> Option<SplitAxis> {
        self.active_tab_ref()
            .filter(|tab| !tab.is_effectively_zoomed())
            .and_then(|tab| divider_axis_at_point(&tab.layout, content, divider_px, x, y, grab_px))
    }

    /// The [`SplitAxis`] of the active tab's divider at tree-order `idx` (the
    /// index a divider drag started from), or `None` when no such divider
    /// exists. Lets an in-progress drag keep showing the matching resize cursor
    /// even when the pointer strays off the hairline. Same pre-order numbering
    /// as [`Self::active_divider_at_point`].
    pub(in crate::native) fn active_divider_axis(
        &self,
        content: PaneRect,
        divider_px: f32,
        idx: usize,
    ) -> Option<SplitAxis> {
        self.active_tab_ref()
            .and_then(|tab| {
                divider_rects_with_axis(&tab.layout, content, divider_px)
                    .into_iter()
                    .nth(idx)
            })
            .map(|(_, axis)| axis)
    }

    /// Drag the active tab's divider at tree-order `target` to a pixel point,
    /// re-deriving and clamping that split's ratio. Returns the new ratio when
    /// the split exists. Caller reflows the affected panes afterward.
    pub(in crate::native) fn drag_active_divider(
        &mut self,
        content: PaneRect,
        divider_px: f32,
        target: usize,
        x: f32,
        y: f32,
    ) -> Option<f32> {
        self.active_tab_mut()
            .and_then(|tab| drag_divider_to(&mut tab.layout, content, divider_px, target, x, y))
    }

    /// Snap the active tab's `target` divider onto a whole-cell boundary,
    /// returning the snapped ratio when the split exists. Called once on drag
    /// release so every rest position leaves identical outer margins; the caller
    /// reflows the affected panes afterward (same path the drag uses).
    pub(in crate::native) fn snap_active_divider(
        &mut self,
        content: PaneRect,
        divider_px: f32,
        target: usize,
        cell_w: u32,
        cell_h: u32,
        pad: f32,
    ) -> Option<f32> {
        self.active_tab_mut().and_then(|tab| {
            snap_divider_to_cells(
                &mut tab.layout,
                content,
                divider_px,
                target,
                cell_w,
                cell_h,
                pad,
            )
        })
    }

    /// The effective display title of the tab that contains `token`: the tab's
    /// user override if set, otherwise the focused pane's shell-derived title
    /// (design doc §2.4). Returns an owned string for the rename UI / test
    /// seams; the tab bar reads the borrowed form via `TabBarSource`.
    pub(in crate::native) fn effective_tab_title(&self, token: SessionToken) -> String {
        let Some(tab) = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .find(|tab| tab.layout.contains(token))
        else {
            return "odytty".to_owned();
        };
        if let Some(name) = &tab.title_override {
            return name.clone();
        }
        self.sessions
            .get(&tab.focused)
            .map(|session| session.tab_title.clone())
            .unwrap_or_else(|| "odytty".to_owned())
    }

    /// Set or clear the user title override for the tab that contains `token`,
    /// marking the focused pane for rebuild so the tab strip repaints.
    pub(in crate::native) fn set_title_override(
        &mut self,
        token: SessionToken,
        name: Option<String>,
    ) {
        let Some(tab) = self
            .workspaces
            .iter_mut()
            .flat_map(|ws| ws.tabs.iter_mut())
            .find(|tab| tab.layout.contains(token))
        else {
            return;
        };
        tab.title_override = name;
        let focused = tab.focused;
        if let Some(session) = self.sessions.get_mut(&focused) {
            session.needs_rebuild = true;
        }
    }

    /// The workspace list as a [`TabBarSource`] for the rail widget (§7.1): the
    /// same geometry / hit-test / panel code the tab strip uses, now listing
    /// workspaces. Borrows `self`, so it is built per render/hit-test frame.
    pub(in crate::native) fn rail_source(&self) -> WorkspaceRailSource<'_> {
        WorkspaceRailSource { set: self }
    }

    /// Drain the bell and prompt-marks-changed latches of EVERY session over the
    /// flat arena (design §5 rule 1 — never a hierarchy walk), routing each per
    /// NF21-6:
    ///
    /// - The active-visible focused pane keeps today's behavior: its bell drives
    ///   the viewport flash and its prompt-marks change (when the gutter is on)
    ///   bumps the epoch. The single-pane render fast path no longer drains —
    ///   this does — so that path stays byte-identical.
    /// - Any OTHER session that rang pings window urgency and latches its owning
    ///   tab's activity flag, UNLESS that tab is the active-visible one (a bell
    ///   in a background pane of the tab you are already viewing is "seen").
    ///   Background prompt-marks are drained and discarded so a stale change can
    ///   never bump the epoch spuriously on switch-back.
    ///
    /// The active-visible tab's activity flag is also cleared here every pass:
    /// viewing a tab is what clears its rollup signal.
    pub(in crate::native) fn drain_bells(&mut self, gutter_on: bool) -> BellSweep {
        let focused = self.active_focused_token();
        let active_ws = self.active_ws;
        let active_tab = self.active_workspace().active_tab;
        let mut sweep = BellSweep::default();
        let mut background_rang: Vec<SessionToken> = Vec::new();
        for session in self.sessions.values() {
            let Ok(mut terminal) = session.terminal.lock() else {
                continue;
            };
            let bell = terminal.take_bell();
            let prompt_changed = terminal.take_prompt_marks_changed();
            drop(terminal);
            if session.id == focused {
                sweep.focused_bell = bell;
                sweep.focused_prompt_changed = gutter_on && prompt_changed;
            } else if bell {
                background_rang.push(session.id);
            }
        }
        for token in background_rang {
            sweep.background_bell = true;
            if let Some((ws_idx, tab_idx)) = self.locate_token(token)
                && (ws_idx, tab_idx) != (active_ws, active_tab)
                && let Some(tab) = self
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|workspace| workspace.tabs.get_mut(tab_idx))
            {
                tab.activity = true;
            }
        }
        // Viewing the active-visible tab clears its rollup signal.
        if let Some(tab) = self
            .workspaces
            .get_mut(active_ws)
            .and_then(|workspace| workspace.tabs.get_mut(active_tab))
        {
            tab.activity = false;
        }
        sweep
    }

    /// Whether any tab of the workspace at `ws_idx` carries an unseen-activity
    /// latch (the DERIVED workspace-level rollup signal; the rail rollup UI will
    /// read this). No reader outside tests yet — the rollup UI is deferred.
    #[allow(dead_code)]
    pub(in crate::native) fn workspace_has_activity(&self, ws_idx: usize) -> bool {
        self.workspaces
            .get(ws_idx)
            .is_some_and(|workspace| workspace.tabs.iter().any(|tab| tab.activity))
    }

    /// The unseen-activity latch of the tab at `(ws_idx, tab_idx)` (test seam).
    #[cfg(test)]
    pub(in crate::native) fn tab_activity(&self, ws_idx: usize, tab_idx: usize) -> bool {
        self.workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.get(tab_idx))
            .is_some_and(|tab| tab.activity)
    }
}

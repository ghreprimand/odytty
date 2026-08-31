// SPDX-License-Identifier: GPL-3.0-only
//! Frame policy for the native window: redraw handling, frame-outcome mapping,
//! skip episodes, and bounded surface-recreation escalation.
//!
//! The redraw body is the same code the `RedrawRequested` arm ran, reached
//! through the same ingress. Recovery decisions stay pure functions over state
//! so they remain testable without a surface.

use super::*;

/// What the event loop should do after a render attempt. Pure mapping from the
/// [`FrameOutcome`]; the call site applies the spin guards (minimized window,
/// retry-budget cap) before acting on a [`FrameAction::RetryAfter`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FrameAction {
    /// The frame presented; nothing extra to schedule (rest at the normal
    /// event-driven wake deadline). The default, byte-identical idle path.
    Idle,
    /// The surface was outdated or validation failed: reconfigure it, then
    /// request a redraw so the recovered surface is actually painted.
    ReconfigureThenRedraw,
    /// Recreate the platform surface before repainting it.
    RecreateSurfaceThenRedraw,
    /// Stop presenting after a device loss until full GPU state can be rebuilt.
    DeviceLost,
    /// The frame was transiently skipped (`get_current_texture` returned
    /// Timeout/Occluded — e.g. the first frame as a Windows DX12 surface
    /// recovers on restore). Retry the frame after a bounded delay rather than
    /// busy-spinning. Subject to the call-site spin guards, and to the
    /// [`SkipEscalation`] rung that upgrades a *chronic* timeout run to
    /// [`FrameAction::RecreateSurfaceThenRedraw`] instead of retrying forever.
    RetryAfter(Duration),
}

/// One complete run of transient surface-acquire skips. The start timestamp is
/// set once and taken only after a frame presents, bounding diagnostics to one
/// record per episode regardless of the retry cadence.
#[derive(Default)]
pub(super) struct SkipEpisode {
    pub(super) started: Option<Instant>,
    pub(super) skips: u32,
}

impl SkipEpisode {
    pub(super) fn note_skipped(&mut self, now: Instant) {
        self.started.get_or_insert(now);
        self.skips = self.skips.saturating_add(1);
    }

    pub(super) fn note_presented(&mut self, now: Instant) -> Option<(Duration, u32)> {
        self.started.take().map(|started| {
            (
                now.saturating_duration_since(started),
                std::mem::take(&mut self.skips),
            )
        })
    }

    pub(super) fn is_active(&self) -> bool {
        self.started.is_some()
    }
}

/// Consecutive skipped frames before a chronic acquire timeout escalates to a
/// surface recreate. With the retry ladder ([`MAX_SKIPPED_RETRIES`] fast tries
/// at [`SKIPPED_FRAME_RETRY`], then the [`SKIPPED_FRAME_SLOW_RETRY`] keep-alive)
/// this puts the first recreate roughly 24 seconds into a persistent episode —
/// far beyond any transient acquire hiccup, and comfortably before the freeze
/// watchdog's multi-minute stall records. Chosen so a briefly-unavailable
/// surface never sees a recreate, while a stranded swapchain (an explicit-sync
/// fence that will never signal) recovers without user intervention.
pub(super) const SKIPPED_FRAME_ESCALATE_AFTER: u32 = 32;

/// Cap on surface-recreate attempts per skip episode. If the surface still will
/// not acquire after this many recreates, the driver (not the swapchain) is
/// wedged: fall back permanently to the event-driven keep-alive plus watchdog
/// logging rather than recreate-looping. Re-armed only by a successful present.
pub(super) const MAX_SKIPPED_FRAME_RECREATES: u32 = 2;

/// ANTI-FREEZE ESCALATION: policy state for routing a *chronic* acquire timeout
/// into the existing surface-recreate path. The retry ladder alone can strand a
/// window forever: when the compositor leaves an in-flight buffer's fence
/// unsignalled, every `get_current_texture` returns Timeout, the ladder retries
/// (fast, then the 1s keep-alive), and nothing ever escalates — a live window
/// froze for minutes exactly this way while the watchdog logged the stall. The
/// recreate machinery already existed for Lost/Outdated surfaces; this routes
/// persistent Timeout into it, bounded and re-armed on present.
///
/// Occluded skips never escalate: an occluded window's surface is *correctly*
/// unavailable, and recreating it on a timer would churn the swapchain of every
/// covered window (the Windows DXGI occlusion signal in particular). One edge
/// is accepted: the consecutive-skip counter is shared, so a genuine timeout
/// right after a long occlusion episode may escalate on its first skip — that
/// is bounded by the per-episode budget and self-corrects on present.
#[derive(Default)]
pub(super) struct SkipEscalation {
    pub(super) recreate_attempts: u32,
}

impl SkipEscalation {
    /// Decide whether this skipped frame escalates to a surface recreate, and
    /// spend one unit of the per-episode budget if so. Pure state machine (no
    /// GPU/winit) so the whole policy is unit-testable: only a non-occluded,
    /// non-minimized skip past [`SKIPPED_FRAME_ESCALATE_AFTER`] consecutive
    /// skips escalates, and only while the budget lasts.
    pub(super) fn should_recreate(
        &mut self,
        occluded: bool,
        minimized: bool,
        consecutive_skipped: u32,
    ) -> bool {
        let escalate = !occluded
            && !minimized
            && consecutive_skipped >= SKIPPED_FRAME_ESCALATE_AFTER
            && self.recreate_attempts < MAX_SKIPPED_FRAME_RECREATES;
        if escalate {
            self.recreate_attempts = self.recreate_attempts.saturating_add(1);
        }
        escalate
    }

    /// Re-arm the recreate budget. Called only when a frame actually presents
    /// (the same re-arm boundary as the freeze watchdog), so failed recreates
    /// cannot refill their own budget.
    pub(super) fn note_presented(&mut self) {
        self.recreate_attempts = 0;
    }

    #[cfg(test)]
    pub(super) fn attempts(&self) -> u32 {
        self.recreate_attempts
    }
}

/// Follow-up after a surface-recreate attempt. Pure (no GPU/winit) so the
/// failed-recreate wake guarantee is unit-testable: the loop must NEVER leave
/// a recreate attempt without either an immediate redraw or a scheduled timed
/// wake — a wake-less exit strands a background window with no incoming events
/// (the same freeze class the skip-escalation rung closes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RecreateFollowUp {
    /// The recreate succeeded: repaint the fresh surface immediately.
    Redraw,
    /// The recreate failed: retry after a slow bounded delay rather than
    /// redrawing into the same broken surface. The delay reuses the
    /// [`SKIPPED_FRAME_SLOW_RETRY`] keep-alive cadence (≤1 wake/sec, never a
    /// busy-spin), and the spent escalation budget is not refunded, so
    /// repeated failures bottom out at the keep-alive + watchdog fallback.
    RetryAfter(Duration),
}

/// Decide the post-recreate follow-up. Split out of the event arm so the
/// "every recreate attempt leaves a wake" invariant is pinned by tests.
pub(super) fn after_recreate_attempt(failed: bool) -> RecreateFollowUp {
    if failed {
        RecreateFollowUp::RetryAfter(SKIPPED_FRAME_SLOW_RETRY)
    } else {
        RecreateFollowUp::Redraw
    }
}

/// State-only escalation record (same privacy discipline as the stall and
/// skip-episode records: counters and flags, never terminal content).
pub(super) fn format_skip_escalation_record(
    attempt: u32,
    consecutive_skips: u32,
    focused: bool,
) -> String {
    format!(
        "skip_escalation_recreate attempt={attempt} consecutive_skips={consecutive_skips} focused={focused}"
    )
}

pub(super) fn episode_log_level(duration: Duration) -> tracing::Level {
    if duration >= Duration::from_secs(10) {
        tracing::Level::WARN
    } else {
        tracing::Level::DEBUG
    }
}

pub(super) fn emit_skip_episode_record(
    duration: Duration,
    skips: u32,
    focused: bool,
    minimized: bool,
) {
    let duration_ms = duration.as_millis();
    if episode_log_level(duration) == tracing::Level::WARN {
        let record = format_skip_episode_record(duration_ms, skips, focused, minimized);
        tracing::warn!("{record}");
    } else if tracing::enabled!(tracing::Level::DEBUG) {
        // Avoid even the bounded record allocation under the stock WARN filter.
        let record = format_skip_episode_record(duration_ms, skips, focused, minimized);
        tracing::debug!("{record}");
    }
}

pub(super) fn format_skip_episode_record(
    duration_ms: u128,
    skips: u32,
    focused: bool,
    minimized: bool,
) -> String {
    format!(
        "skip_episode_end duration_ms={duration_ms} skips={skips} focused={focused} minimized={minimized}"
    )
}

/// Consume the deferred reconfigure before attempting the next render. Taking
/// the flag first guarantees a failed recovery falls through to the existing
/// skipped-frame retry path instead of creating a reconfigure loop.
pub(super) fn take_pending_reconfigure(pending: &mut bool) -> bool {
    std::mem::take(pending)
}

/// Bounded delay before retrying a transiently-skipped frame. ~16ms ≈ one 60Hz
/// frame, so a recovering surface repaints within a frame without busy-spinning.
/// This is a real timed wake folded into the existing `WaitUntil` model, NOT a
/// poll loop.
pub(super) const SKIPPED_FRAME_RETRY: Duration = Duration::from_millis(16);

/// Slow keep-alive retry once the fast-retry budget ([`MAX_SKIPPED_RETRIES`]) is
/// spent. ANTI-FREEZE: without this, a surface that kept returning
/// Timeout/Occluded past the budget left the loop resting at `Wait` with NO
/// pending paint — so a long-lived, non-interacted background window (nothing
/// delivering a `Resized`/`Focused`/input event) latched into a permanent
/// no-repaint freeze until the user forced a window event. A ~1s cadence is not
/// a busy-spin (≤1 wake/sec) yet guarantees an idle window self-heals within a
/// second of the surface actually recovering. Only a minimized (0x0) window
/// opts out — it has nothing to paint and a restore event always re-arms it.
pub(super) const SKIPPED_FRAME_SLOW_RETRY: Duration = Duration::from_millis(1000);

/// Cap on consecutive *fast* `Skipped` retries with no successful present in
/// between. After this many fast tries the loop stops fast-retrying — but,
/// unlike before, it does NOT go silent: it falls back to the
/// [`SKIPPED_FRAME_SLOW_RETRY`] keep-alive (see [`next_skipped_retry_delay`]) so
/// a persistently-unavailable-then-recovered surface always repaints. The
/// counter resets on any successful present.
pub(super) const MAX_SKIPPED_RETRIES: u32 = 8;

/// Pure post-frame decision (see [`FrameAction`]). Split out so the
/// black-screen-on-restore recovery policy is unit-testable with zero GPU/winit:
/// `Reconfigure` must reconfigure AND repaint (or the recovered surface
/// stays black under `ControlFlow::Wait`), `Skipped` must schedule a bounded
/// retry (or a surface that came back Timeout/Occluded on restore never gets a
/// second chance and stays black), and `Presented` settles.
pub(super) fn after_frame(outcome: FrameOutcome) -> FrameAction {
    match outcome {
        FrameOutcome::Presented => FrameAction::Idle,
        FrameOutcome::Reconfigure => FrameAction::ReconfigureThenRedraw,
        FrameOutcome::RecreateSurface => FrameAction::RecreateSurfaceThenRedraw,
        FrameOutcome::RecreateDevice => FrameAction::DeviceLost,
        FrameOutcome::Skipped { .. } => FrameAction::RetryAfter(SKIPPED_FRAME_RETRY),
    }
}

/// Whether a [`FrameAction::RetryAfter`] should actually be scheduled, given the
/// spin guards. Pure (no surface/event-loop), so it is unit-testable. Returns
/// `false` when the window is minimized (a 0x0 surface — retrying an invisible
/// surface only burns wakeups) or once the consecutive-skip budget is exhausted
/// (fall back to the event-driven `Wait`). This is what keeps the bounded retry
/// from degrading into a busy-spin on a persistently-unavailable surface.
///
/// Production scheduling now goes through [`next_skipped_retry_delay`] (which
/// additionally distinguishes the fast retry from the slow keep-alive); this
/// predicate is retained as the "fast-retry allowed?" seam the restore/occlude
/// regression tests assert against, so it is test-only.
#[cfg(test)]
pub(super) fn should_schedule_skipped_retry(minimized: bool, consecutive_skipped: u32) -> bool {
    !minimized && consecutive_skipped < MAX_SKIPPED_RETRIES
}

/// The delay before the next skipped-frame retry, or `None` to schedule none.
/// Pure (no surface/event-loop), so the whole recovery policy is unit-testable
/// with zero GPU/winit. Three-way:
/// - `None` — window minimized (0x0): nothing to paint; a restore event re-arms.
/// - `Some(`[`SKIPPED_FRAME_RETRY`]`)` — under the fast-retry budget: recover
///   within a frame.
/// - `Some(`[`SKIPPED_FRAME_SLOW_RETRY`]`)` — budget spent: a slow keep-alive so
///   an idle background window still self-heals once the surface recovers,
///   instead of latching into a permanent freeze. This is the anti-freeze fix:
///   the previous policy returned "schedule nothing" here, which under
///   `ControlFlow::Wait` meant a window with no incoming events never repainted
///   again.
pub(super) fn next_skipped_retry_delay(
    minimized: bool,
    consecutive_skipped: u32,
) -> Option<Duration> {
    if minimized {
        None
    } else if consecutive_skipped < MAX_SKIPPED_RETRIES {
        Some(SKIPPED_FRAME_RETRY)
    } else {
        Some(SKIPPED_FRAME_SLOW_RETRY)
    }
}

impl App {
    /// Whether this frame must rebuild geometry. `self.needs_rebuild` Derefs to
    /// the FOCUSED pane's flag; single-pane that is the only visible pane, so the
    /// decision is byte-identical to before. Multi-pane: OR the flag across every
    /// visible pane of the active tab, so output streaming into a non-focused
    /// split pane repaints even while the focused pane is idle — otherwise a
    /// build in the other half of a split freezes until the user types into the
    /// focused pane (NF21-7). Paired with `clear_visible_pane_rebuild_flags` in
    /// the multi-pane rebuild branch, which must clear the same set.
    pub(super) fn should_rebuild_frame(&self) -> bool {
        self.needs_rebuild
            || (!self.sessions.active_is_single_pane()
                && self.sessions.any_visible_pane_needs_rebuild())
    }
}

// Redraw arm moved verbatim from the `ApplicationHandler` match; the match
// itself remains the stable ingress in `mod.rs`.
impl App {
    /// Handle one delivered `RedrawRequested`.
    ///
    /// Returns `true` when this frame took one of the two early-exit paths that
    /// left the event handler before its trailing pending-exit check. The
    /// `WindowEvent` match returns on `true` so that check is still reached on
    /// exactly the paths that reached it before, and skipped on exactly the
    /// paths that skipped it.
    pub(super) fn on_redraw_requested(&mut self) -> bool {
        // FREEZE-WATCHDOG: count the delivery, not the request. A
        // request we made that the windowing system never turned into
        // this event means the surface is not being painted (asleep
        // output / occluded / frame-callback throttled), which is not a
        // stall — see the field docs on `redraws_delivered`.
        self.redraws_delivered = self.redraws_delivered.saturating_add(1);
        self.flush_pending_overlay_settings();
        self.sessions.reconcile_scrollback_trims();
        self.handle_terminal_clipboard_requests();
        self.update_window_title();
        // F4-P4: reflow the content grid if auto-sizing (or a max-width
        // edit) moved the rail band since the last frame — a shell-set
        // title changing the longest tab title has no other trigger. A
        // no-change frame is a single width comparison.
        self.reconcile_rail_auto_width();
        // C4: clear the GPU image-viewer texture the frame after the
        // viewer overlay closes, so the closed-viewer frame is
        // byte-identical to the no-viewer path.
        self.sync_image_overlay();
        // Rebuild geometry at most once per redraw, no matter how many
        // pump wakes coalesced into this frame. Snapshot under the lock,
        // then drop it before touching the GPU.
        if self.should_rebuild_frame() {
            let now = Instant::now();
            let synchronized_output = self
                .terminal
                .lock()
                .map(|terminal| terminal.synchronized_output_enabled())
                .unwrap_or(false);
            let was_holding = self.synchronized_output_hold.is_holding();
            let is_holding = self
                .synchronized_output_hold
                .should_hold(synchronized_output, now);
            if was_holding && !is_holding {
                self.clear_cursor_streak();
            }
            if is_holding {
                let _ = self.update_held_cursor_frame(now);
            } else if !self.sessions.active_is_single_pane() {
                // Multi-pane active tab: branch to the per-pane render
                // dispatch (design doc §3.2, audit rows #2/#3/#10/#11).
                // The single-pane fast path below is never reached here,
                // so it stays byte-identical.
                self.rebuild_multipane();
                // Clear EVERY visible pane's flag, not just the focused
                // one (`self.needs_rebuild`): the widened gate above ORs
                // the flag across the tab, so leaving a dirtied background
                // pane's flag set would re-open the gate every frame — a
                // rebuild storm (NF21-7).
                self.sessions.clear_visible_pane_rebuild_flags();
            } else {
                let Some(cell) = self.gpu.as_ref().map(GpuState::cell) else {
                    return true;
                };
                let cached_image_ids = self
                    .gpu
                    .as_ref()
                    .map(GpuState::cached_image_generations)
                    .unwrap_or_default();
                let (
                    mut snapshot,
                    scrollback_len,
                    cursor_style,
                    cursor_blinking,
                    terminal_revision,
                    visible_graphics,
                    visible_buttons,
                    image_uploads,
                ) = {
                    // NF21-6: bell + prompt-marks latches are drained
                    // in the about-to-wait maintenance sweep (over the
                    // whole arena) now, not here — so a background /
                    // multipane bell is serviced instead of stranding.
                    // This paint only reads scrollback for viewport
                    // anchoring; the fast path is otherwise unchanged.
                    let scrollback_len = {
                        // P0-3: per-frame paint read — poison-recover.
                        let terminal = crate::native::lock_recover(&self.terminal);
                        terminal.screen().scrollback_len()
                    };
                    self.update_bell_flash(now);
                    // Expire the transient native status banner once it
                    // has outlived its lifetime; no-op when absent.
                    self.update_open_notice(now);
                    // UX-A (Phase 11): expire the click hint + drop a
                    // stale unpaired mis-click. No-op on the idle path.
                    self.update_click_hint(now);
                    // NF21-10: anchor + baseline via the shared pane
                    // helper (identical to the historical inline
                    // sequence) so the single-pane and multipane paths
                    // stay in lockstep.
                    let offset = self.anchor_viewport_for_render(scrollback_len);
                    // SCROLL-GLIDE: advance the forward-chase follower
                    // one frame toward the just-anchored offset and
                    // snapshot at its floored row (the sub-row remainder
                    // rides the existing scroll_frac_offset seam, so the
                    // shift is always under one cell). Both are a no-op /
                    // the logical offset unless a glide is in flight.
                    self.update_scroll_glide(now, cell.height, offset);
                    let offset = self.glide_render_offset(offset, scrollback_len);
                    let mut search = std::mem::take(&mut self.search);
                    // P0-3: same-frame search refresh + graphics read.
                    let terminal = crate::native::lock_recover(&self.terminal);
                    if search.is_open() {
                        search.refresh(&terminal);
                    }
                    let visible_graphics = terminal.visible_graphics(offset);
                    // Button Protocol B2: viewport-projected buttons for
                    // chip paint + the render-cache fragment. Gate-scoped
                    // to an empty vec when the protocol is off, so the
                    // default frame is byte-identical.
                    let visible_buttons = terminal.visible_button_spans(offset);
                    let image_uploads =
                        image_uploads_for_visible(&terminal, &visible_graphics, &cached_image_ids);
                    let snapshot = terminal.snapshot_with_scrollback(offset);
                    let cursor_style = terminal.cursor_style();
                    let cursor_blinking = terminal.cursor_blinking();
                    let terminal_revision = terminal.render_revision();
                    drop(terminal);
                    self.search = search;
                    (
                        snapshot,
                        scrollback_len,
                        cursor_style,
                        cursor_blinking,
                        terminal_revision,
                        visible_graphics,
                        visible_buttons,
                        image_uploads,
                    )
                };
                let pane_dims_reconciled = {
                    #[cfg(unix)]
                    {
                        let drifted = snapshot.dimensions != self.grid;
                        if drifted {
                            tracing::debug!(
                                snapshot_columns = snapshot.dimensions.columns,
                                snapshot_rows = snapshot.dimensions.rows,
                                window_columns = self.grid.columns,
                                window_rows = self.grid.rows,
                                "single-pane dimensions drifted from the window grid; reconciling"
                            );
                            self.reconcile_pane_dims_to_window();
                            if let Some(window) = self.window.as_ref() {
                                window.request_redraw();
                            }
                        }
                        drifted
                    }
                    #[cfg(not(unix))]
                    {
                        false
                    }
                };
                // Blink phase: hide the cursor during the off-phase. Only the
                // live view (offset 0) shows a cursor; the blink driver holds
                // it solid when not blinking or unfocused.
                let base_cursor_visible = snapshot.cursor_visible;
                let focused = self.focused;
                let cursor_on = self.cursor_blink.poll(now, cursor_blinking, focused);
                // ID1 easing + VE4 slide: refresh the precomputed cursor
                // animation params for this frame from the injected `now`
                // and the blink phase / logical cursor move. Both no-op to
                // the identity while their knobs are off.
                self.update_cursor_easing(now, cursor_on, cursor_blinking);
                self.update_cursor_motion(now, &snapshot, cell);
                self.update_cursor_streak(now, &snapshot, cursor_style, cell);
                // Blink off-phase hard-hide — skipped while easing is on,
                // where the precomputed alpha carries the fade instead (so
                // easing does not double-hide).
                if !cursor_on && (!self.settings.cursor_easing || self.settings.reduced_motion) {
                    snapshot.cursor_visible = false;
                }
                self.hovered_hyperlink = self.pointer_cell.and_then(|point| {
                    if point.row >= snapshot.dimensions.rows
                        || point.column >= snapshot.dimensions.columns
                    {
                        return None;
                    }
                    snapshot
                        .cells
                        .get(point.row * snapshot.dimensions.columns + point.column)
                        .and_then(|cell| cell.attrs.hyperlink)
                });
                // Frame-overlay cell-paint manifest (see overlay_registry).
                // Order = paint precedence; new slots strictly after the
                // existing four and no-op until their feature ships.
                // VE4 new-output fade: refresh the per-row fade-start
                // instants from the scrollback delta before building the
                // overlay context, so the fade quads this frame reflect
                // this rebuild's new rows. No-op while the knob is off.
                self.update_row_fade(now, scrollback_len);
                let ctx = self.overlay_ctx(
                    scrollback_len,
                    cell,
                    snapshot.cursor,
                    snapshot.cursor_visible,
                    now,
                );
                self.paint_selection_cells(&mut snapshot, &ctx);
                self.paint_search_cells(&mut snapshot, &ctx);
                // Button Protocol B2: program-defined button chips.
                // `visible_buttons` is empty on the gate-off / no-button
                // path, so this is a no-op there and the frame stays
                // byte-identical. Painted at the content layer — BEFORE
                // the overlay panel and the transient UI slots below —
                // so an open panel fully occludes any chip under it
                // (chips once painted last and bled through overlays).
                // The point-chip content-end scan also depends on this
                // spot: it must read terminal content, not panel cells.
                let hovered_button_key = self
                    .hovered_button
                    .as_ref()
                    .map(|hit| (hit.row, hit.start_col));
                button_chip::paint_button_cells(
                    &mut snapshot,
                    &visible_buttons,
                    hovered_button_key,
                );
                self.paint_overlay_cells(&mut snapshot, &ctx);
                self.paint_hyperlink_cells(&mut snapshot, &ctx);
                self.paint_hints_cells(&mut snapshot, &ctx);
                self.paint_copy_mode_cells(&mut snapshot, &ctx);
                self.paint_rename_tab_cells(&mut snapshot);
                // IME pre-edit: paint the in-progress composition inline
                // at the cursor; empty on the no-composition path.
                self.paint_ime_preedit_cells(&mut snapshot);
                // Transient status or OSC 52 consent banner across the
                // top of the grid; empty on the idle path.
                self.paint_open_notice_cells(&mut snapshot);
                let attention = &self.sessions.active().attention;
                self.paint_pane_attention_cell(
                    &mut snapshot,
                    attention.progress,
                    attention.unread,
                    attention.completed,
                    attention.failed,
                );
                // UX-A (Phase 11): the Ctrl+hover armed underline on the
                // hovered path span, then the transient bottom-left
                // "Ctrl+click to open" hint. Both no-op (byte-identical)
                // off their gates — armed underline needs interactive_paths
                // + Ctrl + a hovered path; the hint needs to be shown.
                self.paint_armed_path_underline_cells(&mut snapshot);
                self.paint_click_hint_cells(&mut snapshot);
                // Static centered feedback for bounded window-level gestures
                // such as Ctrl+wheel font zoom. No-op at rest.
                self.paint_transient_hud_cells(&mut snapshot);
                // Frame-overlay quad manifest: scroll indicator, then the
                // SH2 status gutter, then the no-op new slots.
                let mut overlays: Vec<SolidQuad> = Vec::new();
                self.paint_scroll_indicator_quads(&ctx, &mut overlays);
                self.paint_gutter_quads(&ctx, &mut overlays);
                self.paint_cursor_trail_quads(&ctx, &mut overlays);
                self.paint_background_quads(&ctx, &mut overlays);
                // ID4 themed window border: a thin frame in the padding
                // band, drawn over any background treatment; empty on the
                // off path.
                self.paint_window_border_quads(&ctx, &mut overlays);
                // VE4 new-output fade — a per-row FOREGROUND alpha ramp
                // applied inside the cell/color-glyph vertex builds (no
                // veil quads): capture this frame's multipliers here,
                // where the pre-decoration cursor row is known; handed
                // to the GPU below with the chrome offsets. `None` on
                // the off path and every settled frame.
                let new_row_fade_text = self.new_row_fade_text_multipliers(now, ctx.cursor.row);
                // BELL visual flash — a full-viewport decaying tint over
                // everything; empty on the off / urgent-only path.
                self.paint_bell_flash_quad(&ctx, &mut overlays);
                let (chrome_dx, chrome_dy) = self.tab_chrome_offset_px(cell);
                if chrome_dx > 0.0 || chrome_dy > 0.0 {
                    self.shift_overlays_for_tab_chrome(
                        &mut overlays,
                        chrome_dx as f32,
                        chrome_dy as f32,
                    );
                }
                let pad = ctx.window_padding.as_f32();
                let content_x0 = pad + chrome_dx as f32;
                let content_y0 = pad + chrome_dy as f32;
                let cursor_glow = self.cursor_glow_request([
                    content_x0,
                    content_y0,
                    content_x0 + self.grid.columns as f32 * cell.width as f32,
                    content_y0 + self.grid.rows as f32 * cell.height as f32,
                ]);
                let cursor_streak = self.cursor_streak_request(
                    now,
                    [
                        content_x0,
                        content_y0,
                        content_x0 + self.grid.columns as f32 * cell.width as f32,
                        content_y0 + self.grid.rows as f32 * cell.height as f32,
                    ],
                );
                let cursor_visible = snapshot.cursor_visible;
                let (snapshot, tab_bar_quads, cursor_comparison) =
                    self.prepare_single_pane_snapshots(snapshot, cursor_visible, cell);
                overlays.extend(tab_bar_quads);
                // R3 call-site parity + A2 cache observability: compute
                // the live cursor params ONCE so focus, animation key,
                // and the GPU CursorOnly/Full calls share one source.
                // Identity while both knobs are off ⇒ a constant key ⇒
                // `Retained` ⇒ byte-identical plain path.
                let cursor_params = self.cursor_render_params();
                let signature = RenderSignature {
                    content: RenderContentSignature {
                        terminal_revision,
                        viewport_offset: self.viewport.offset(),
                        scrollback_len,
                        // RV4: the smooth-scroll sub-row offset bits.
                        // Constant `0` on the off path / at rest (cache
                        // decision unchanged); changes every animating
                        // frame so the shifted vertices rebuild.
                        scroll_frac_bits: self.scroll_frac_bits(),
                        grid: self.grid,
                        cell,
                        selection: self.selection.range().map(|range| {
                            SelectionSignature::from_range(range, self.selection_block)
                        }),
                        search: self.search.render_signature(),
                        overlay: self.overlay.render_signature(),
                        hovered_hyperlink: self.hovered_hyperlink,
                        graphics: visible_graphics_signature(&visible_graphics),
                        presentation_epoch: self.presentation_epoch,
                        prompt_marks_epoch: self.prompt_marks_epoch,
                        // Overlay-registry composite (NEW contributors
                        // only; all Inert today ⇒ constant ⇒ decision
                        // unchanged). D-INFRA-1/D-INFRA-6.
                        overlays: OverlayCompositeSignature {
                            hints: self.hints_overlay_signature(),
                            copy_mode: self.copy_mode_overlay_signature(),
                            cursor_trail: self.cursor_trail_overlay_signature(),
                            cursor_glow: self.cursor_glow_overlay_signature(),
                            background: self.background_overlay_signature(),
                            new_row_fade: self.new_row_fade_overlay_signature(),
                            rename: self.rename_overlay_signature(),
                            bell_flash: self.bell_flash_overlay_signature(),
                            ime_preedit: self.ime_overlay_signature(),
                            open_notice: self.open_notice_overlay_signature(),
                            // UX-A (Phase 11): both Inert off their gates,
                            // so the composite stays constant on the
                            // default path; armed_path flips on Ctrl
                            // toggle / span move so it reclassifies Full.
                            click_hint: self.click_hint_overlay_signature(),
                            transient_hud: self.transient_hud.signature(),
                            armed_path: self.armed_path_overlay_signature(),
                            // Button Protocol B2: Inert on the gate-off /
                            // no-button path (composite stays constant);
                            // a folded hash otherwise so a define / move
                            // / invalidate / scroll re-keys the frame.
                            buttons: button_chip::buttons_overlay_signature(
                                &visible_buttons,
                                hovered_button_key,
                            ),
                        },
                        // F4-P3: fold the revealed rail overlay's
                        // visibility + geometry + visual state so a pure
                        // reveal / hide / hover / switch rebuilds the
                        // frame. `default()` (not revealed) is constant.
                        rail_overlay: self.rail_overlay_render_signature(cell),
                    },
                    cursor: CursorRenderSignature {
                        visible: snapshot.cursor_visible,
                        style: cursor_style,
                        anim: CursorAnimKey::from_params(&cursor_params),
                        streak_epoch: self.cursor_streak_epoch(),
                    },
                };
                let update =
                    RenderSignature::update_from(self.last_render_signature.as_ref(), &signature);
                // ID2 focus dimming: dim the whole grid only while the
                // window is unfocused. The focused window is never dimmed
                // (amount 0.0), so focused frames stay byte-identical; the
                // knob defaults to 0.0, which is also a no-op. grid.rs does
                // the perceptual math; the native layer only decides the
                // effective amount here.
                let focus_dim = if self.focused {
                    0.0
                } else {
                    self.settings.effective_focus_dim()
                };
                // ID3/U5 background treatment: resolved once per Full
                // rebuild (identity when the knob is off, so the plain
                // path is byte-identical). grid.rs applies it per cell
                // before the RV1 floor.
                let background_treatment = self.background_treatment_params();
                // `cursor_params` was hoisted above the signature literal
                // (it feeds the `anim` cache key); the CursorOnly arm
                // reuses the same value so the cached cursor matches.
                let scroll_frac_offset = self.scroll_frac_offset;
                // SCROLL-CHROME-BOUNCE: geometry (in decorated-snapshot
                // columns) so the GPU pins the tab bar / rail while the
                // terminal content glides. `None` when no chrome shown.
                let chrome_pin_geom = self.chrome_pin_geom(snapshot.dimensions.columns);
                let tab_bar_row_offset = self.tab_bar_row_offset();
                let tab_bar_col_offset = self.tab_bar_col_offset();
                // F4-P1 unified tab panel + seam: background-segment quads
                // behind the tab chrome. Empty when the bar is hidden /
                // panel off / seam off, so the plain path is unchanged.
                let tab_bg_quads = self.tab_panel_bg_quads(cell);
                // F4-P3: the revealed rail auto-hide overlay strip. Built
                // before the GPU borrow (it reads `&self`); `None` unless
                // the floating rail is currently revealed, so the pinned /
                // no-autohide path is byte-identical.
                let rail_overlay_data = self.build_rail_overlay(cell);
                // F4-P3 rail-overlay RETENTION: the rail overlay lives in
                // the trailing (post-`cell_vertex_count`) vertex segment,
                // alongside the cursor. The `CursorOnly` fast path
                // (`update_cursor_and_overlays`) rebuilds ONLY that segment
                // from the cursor vertices — it truncates to
                // `cell_vertex_count` and re-appends the cursor WITHOUT the
                // rail. So once the rail is steady-revealed, the very next
                // cursor blink (a `CursorOnly` update) drops the rail out
                // of the buffer, and it stays gone until an unrelated Full
                // rebuild (a hover change / terminal output / moving off
                // the window edge) re-runs `push_rail_overlay`. That is the
                // "reveals where expected, then vanishes as I inch further,
                // reappears past the edge, won't stay up" report: the state
                // machine holds `visible` rock-steady, but the blink keeps
                // eating the pixels. Promote `CursorOnly` to `Full` whenever
                // the rail overlay is present so it is re-appended every
                // frame it is visible; `Retained` is left alone (it never
                // touches the buffer, so the rail persists), and the
                // plain / no-autohide path (`None`) keeps its classification
                // exactly, so nothing off the revealed-rail path changes.
                let update = update.retaining_rail_overlay(rail_overlay_data.is_some());
                // TRANSPARENCY: window background alpha for this frame,
                // computed before the mutable GPU borrow.
                let win_bg_alpha = {
                    let capable = self
                        .gpu
                        .as_ref()
                        .is_some_and(GpuState::transparency_capable);
                    self.effective_window_bg_alpha(capable)
                };
                // TRANSPARENCY (MENU-OPACITY): while the window is
                // translucent, hold the open overlay panel's cell span
                // opaque so a menu/settings/picker stays readable without
                // resealing the whole window. `None` on the opaque path
                // (and when no overlay is open) keeps that path
                // byte-identical.
                let overlay_opaque_region = if win_bg_alpha < 1.0 {
                    self.single_pane_overlay_opaque_region()
                } else {
                    None
                };
                // VE4 new-output fade: map the content-row multipliers
                // captured above into decorated-snapshot coordinates —
                // chrome band rows above and rail columns beside the
                // content never fade. `None` (off / settled) keeps the
                // builders on their exact inert path.
                let row_fade_spec = new_row_fade_text.map(|multipliers| RowFadeSpec {
                    multipliers,
                    row_offset: tab_bar_row_offset,
                    col_start: tab_bar_col_offset,
                    col_end: tab_bar_col_offset + self.grid.columns,
                });
                if let Some(gpu) = self.gpu.as_mut() {
                    let rail_overlay = rail_overlay_data.as_ref().map(|data| RailOverlay {
                        snapshot: &data.snapshot,
                        origin: data.origin,
                        treatment: crate::grid::BackgroundTreatmentParams::default(),
                        rail_glyph_dy_rows: data.rail_glyph_dy_rows,
                        widget_quads: &data.widget_quads,
                        base_gaps: &data.base_gaps,
                        wash: data.wash,
                        seam: data.seam,
                    });
                    // RV4: push the current smooth-scroll offset so the
                    // vertex builders shift `content_origin` this frame.
                    // `0.0` at rest / on the off path leaves the origin
                    // byte-identical.
                    gpu.set_scroll_frac_offset(scroll_frac_offset);
                    gpu.set_chrome_pin_geom(chrome_pin_geom);
                    gpu.set_window_bg_alpha(win_bg_alpha);
                    gpu.set_overlay_opaque_region(overlay_opaque_region);
                    gpu.set_row_fade(row_fade_spec);
                    match update {
                        GeometryUpdate::Full => {
                            gpu.update_image_layer(
                                &visible_graphics,
                                &image_uploads,
                                tab_bar_row_offset,
                                tab_bar_col_offset,
                            );
                            if overlays.is_empty()
                                && tab_bg_quads.is_empty()
                                && rail_overlay.is_none()
                            {
                                gpu.update_from_snapshot(
                                    &snapshot,
                                    cursor_style,
                                    cursor_glow,
                                    cursor_streak,
                                    cursor_params,
                                    focus_dim,
                                    background_treatment,
                                );
                            } else {
                                gpu.update_from_snapshot_with_overlays(
                                    &snapshot,
                                    cursor_style,
                                    &overlays,
                                    cursor_glow,
                                    cursor_streak,
                                    cursor_params,
                                    focus_dim,
                                    background_treatment,
                                    PanelFrameQuads {
                                        base_gaps: &tab_bg_quads.base_gaps,
                                        overlays: &tab_bg_quads.overlays,
                                    },
                                    rail_overlay,
                                );
                            }
                        }
                        GeometryUpdate::CursorOnly => {
                            gpu.update_cursor_and_overlays(
                                &snapshot,
                                cursor_style,
                                &overlays,
                                cursor_glow,
                                cursor_streak,
                                cursor_params,
                            );
                        }
                        GeometryUpdate::Retained => {}
                    }
                }
                self.last_render_signature = Some(signature);
                let mut held_snapshot = snapshot;
                held_snapshot.cursor_visible = base_cursor_visible;
                self.last_presented_snapshot = Some(held_snapshot);
                self.last_cursor_comparison_snapshot = Some(cursor_comparison);
                self.last_presented_cursor_style = cursor_style;
                self.last_presented_cursor_blinking = cursor_blinking;
                // A drifted snapshot was captured before reconciliation,
                // so keep the focused session dirty for the requested
                // follow-up redraw. The next frame reads the corrected
                // dimensions and returns to the normal clean state.
                self.needs_rebuild = pane_dims_reconciled;
            }
        }
        let (action, recreate_failed) = {
            let Some(gpu) = self.gpu.as_mut() else {
                return true;
            };
            if take_pending_reconfigure(&mut self.pending_surface_reconfigure) {
                gpu.reconfigure();
            }
            let outcome = gpu.render();
            let mut action = after_frame(outcome);
            // ANTI-FREEZE ESCALATION: a chronic acquire timeout (the
            // retry ladder exhausted many consecutive skips with paint
            // work still pending — every retry here IS a pending paint)
            // escalates to the surface-recreate path instead of
            // retrying forever. Bounded per episode, exempt while
            // occluded or minimized, re-armed only by a present.
            if let FrameOutcome::Skipped { occluded } = outcome
                && self.skip_escalation.should_recreate(
                    occluded,
                    self.window_minimized,
                    self.consecutive_skipped_frames,
                )
            {
                // The frame WAS skipped; keep the episode totals true
                // even though the action below leaves the retry arm.
                self.skip_episode.note_skipped(Instant::now());
                tracing::warn!(
                    "{}",
                    format_skip_escalation_record(
                        self.skip_escalation.recreate_attempts,
                        self.consecutive_skipped_frames,
                        self.focused,
                    )
                );
                action = FrameAction::RecreateSurfaceThenRedraw;
            }
            // Recover outdated surfaces by reconfiguring (infallible —
            // no error path to strand on) and lost surfaces by
            // recreating them. Both request a redraw below; under
            // `ControlFlow::Wait` there is no automatic next frame.
            let mut recreate_failed = false;
            match action {
                FrameAction::ReconfigureThenRedraw => gpu.reconfigure(),
                FrameAction::RecreateSurfaceThenRedraw => {
                    if let Err(err) = gpu.recreate_surface() {
                        // ANTI-FREEZE: a failed recreate must NOT
                        // dead-end the loop wake-less (a background
                        // window with no incoming events would strand
                        // until an external event — the same freeze
                        // class the escalation rung closes). Record the
                        // failure; the post-borrow arm schedules a slow
                        // timed retry instead of the immediate redraw.
                        tracing::error!("failed to recreate GPU surface: {err}");
                        recreate_failed = true;
                    }
                }
                FrameAction::DeviceLost => {
                    // The callback only signals this event-loop thread.
                    // Rebuilding every GPU-owned atlas, texture, and
                    // pipeline needs an explicit state reconstruction;
                    // stop cleanly instead of spinning on a dead device.
                    // Deliberately NO wake: no timed retry can rebuild
                    // device-owned state, so scheduling one would only
                    // re-log the same dead-device error forever.
                    tracing::error!(
                        "GPU device was lost; rendering is paused until the window is restarted"
                    );
                }
                FrameAction::Idle | FrameAction::RetryAfter(_) => {}
            }
            (action, recreate_failed)
        };
        // Drop the `gpu` borrow before touching `self.window` (disjoint
        // fields, but `self.gpu.as_mut()` borrows all of `self`).
        match action {
            FrameAction::Idle => {
                if let Some((duration, skips)) = self.skip_episode.note_presented(Instant::now()) {
                    emit_skip_episode_record(duration, skips, self.focused, self.window_minimized);
                }
                // A present resets the skipped-frame retry budget so a
                // future transient skip gets a fresh set of retries.
                self.consecutive_skipped_frames = 0;
                self.skipped_frame_retry_deadline = None;
                // ...and re-arms the bounded surface-recreate budget
                // (the only place it refills — see `SkipEscalation`).
                self.skip_escalation.note_presented();
            }
            FrameAction::ReconfigureThenRedraw => {
                self.consecutive_skipped_frames = 0;
                self.skipped_frame_retry_deadline = None;
                // Single redraw request — not a loop; the post-reconfigure
                // render normally succeeds.
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            FrameAction::RecreateSurfaceThenRedraw => {
                self.consecutive_skipped_frames = 0;
                match after_recreate_attempt(recreate_failed) {
                    RecreateFollowUp::Redraw => {
                        self.skipped_frame_retry_deadline = None;
                        if let Some(window) = self.window.as_ref() {
                            window.request_redraw();
                        }
                    }
                    RecreateFollowUp::RetryAfter(delay) => {
                        // The recreate itself failed: an immediate
                        // redraw would just re-attempt against the same
                        // broken surface. A slow timed retry keeps a
                        // guaranteed wake (`about_to_wait` consumes the
                        // deadline and requests the redraw) without
                        // spinning; the spent escalation budget is NOT
                        // refunded, so repeated failures still bottom
                        // out at the keep-alive + watchdog fallback.
                        self.skipped_frame_retry_deadline = Some(Instant::now() + delay);
                    }
                }
            }
            FrameAction::DeviceLost => {
                // No later present can close the episode after rendering
                // pauses, so discard it without reporting a false
                // self-heal and drop any deferred surface work.
                self.skip_episode = SkipEpisode::default();
                self.skip_escalation = SkipEscalation::default();
                self.pending_surface_reconfigure = false;
                self.consecutive_skipped_frames = 0;
                self.skipped_frame_retry_deadline = None;
            }
            FrameAction::RetryAfter(delay) => {
                self.skip_episode.note_skipped(Instant::now());
                // BLACK-SCREEN-ON-RESTORE: a transiently-skipped frame
                // (Timeout/Occluded). Schedule ONE bounded timed retry —
                // folded into the `WaitUntil` wake set. The delay is
                // chosen by the spin-guard policy: fast (~16ms) while the
                // consecutive-skip budget lasts, then a slow (~1s)
                // keep-alive once it is spent — so an idle background
                // window whose surface has recovered self-heals within a
                // second WITHOUT needing an external event, while never
                // busy-spinning. A minimized (0x0) window is the only
                // veto: nothing to paint, and a restore event always
                // re-arms it. `about_to_wait` folds the deadline into the
                // control flow.
                let _ = delay; // policy owns the delay (fast vs. slow)
                match next_skipped_retry_delay(
                    self.window_minimized,
                    self.consecutive_skipped_frames,
                ) {
                    Some(retry) => {
                        self.consecutive_skipped_frames =
                            self.consecutive_skipped_frames.saturating_add(1);
                        self.skipped_frame_retry_deadline = Some(Instant::now() + retry);
                    }
                    None => {
                        self.skipped_frame_retry_deadline = None;
                    }
                }
            }
        }
        false
    }
}

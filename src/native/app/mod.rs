// SPDX-License-Identifier: GPL-3.0-only
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::core::{
    ClipboardRequest, Color, Dimensions, LinkId, MouseButton as CoreMouseButton, MouseEncoding,
    MouseEventKind, MouseModifiers, MouseProtocol, RgbColor, Snapshot, Terminal,
    encode_mouse_event_pixel,
};
use crate::input::{self, Key, KeyEventType, KeyModes, Modifiers};
use crate::pty::PtySession;
use crate::selection::{
    self, AbsoluteSelectionRange, AbsoluteSelectionState, CellPoint, ClickTracker, PointerDrag,
    SelectGranularity, SelectionStyle,
};
use crate::settings::{
    BindableAction, SettingEdit, Settings, SettingsReloadOutcome, SettingsReloader, THEME_ENV,
    apply_reloadable_values, write_settings_changes_to_path,
};
use crate::text::{self, CellSize};
use crate::theme::{Theme, VisualEffect};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::NamedKey;
use winit::keyboard::{Key as WinitKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Window, WindowId};

use super::bindings::{
    KeyBindings, changed_window_title, encode_native_focus_report, encode_native_mouse_report,
    map_keypad_physical_key, map_named_key, map_winit_mouse_button, motion_report_button,
    wheel_report_button,
};
use super::clipboard::{
    NativeClipboard, read_clipboard_selection, selected_clipboard_text, write_clipboard_selection,
    write_paste_text,
};
use super::gpu::{BloomOptions, CrtOptions, FrameOutcome, GpuState};
use super::options::{NativeError, NativeOptions};
use super::overlay::{
    OverlayOutcome, OverlayPointer, OverlayUi, PointerButton, apply_overlay,
    overlay_input_from_winit, overlay_rect,
};
use super::pty::{PtyWriter, UserEvent};
use super::render_helpers::{
    CursorRenderSignature, GeometryUpdate, RenderContentSignature, RenderSignature,
    SelectionSignature, apply_hyperlink_hover, hyperlink_action_allowed, image_uploads_for_visible,
    key_modes_from_core, openable_hyperlink_uri, visible_graphics_signature,
};
use super::theme_builder::{save_theme_to_dir, user_theme_dir_for_config};

pub(super) use super::cursor::{CURSOR_BLINK_INTERVAL, CursorBlinkState};
pub(super) use super::resize::{
    PendingResize, RESIZE_DEBOUNCE_INTERVAL, ResizeDebouncer, pending_resize_for_surface,
    scale_factor_changed,
};
use super::search_ui::{SearchStyle, SearchUi, apply_search_ui};
use super::viewport::{
    SELECTION_AUTOSCROLL_INTERVAL, Viewport, WindowPadding, grid_dimensions_for_with_padding,
    scroll_indicator_hit_with_padding, scroll_indicator_quad_with_padding,
    scrollbar_offset_for_drag_with_padding, wheel_lines, wheel_lines_scaled,
};

mod interaction;
mod pointer;

pub(super) const SYNCHRONIZED_OUTPUT_TIMEOUT: Duration = Duration::from_millis(150);

/// Native presenter policy for DECSET 2026 synchronized output.
///
/// The terminal core owns the mode bit. The native layer owns the safety policy:
/// once a hold is observed, grid-content uploads are deferred for at most 150 ms
/// so a crashed application that never sends DECRST 2026 cannot leave the
/// display frozen indefinitely. After the timeout, presentation is released
/// until the application resets the mode and starts a later synchronized batch.
#[derive(Debug, Default)]
pub(super) struct SynchronizedOutputHold {
    active_since: Option<Instant>,
    timed_out: bool,
}

impl SynchronizedOutputHold {
    pub(super) fn should_hold(&mut self, enabled: bool, now: Instant) -> bool {
        if !enabled {
            self.active_since = None;
            self.timed_out = false;
            return false;
        }

        let active_since = *self.active_since.get_or_insert(now);
        if self.timed_out {
            return false;
        }
        if now.saturating_duration_since(active_since) >= SYNCHRONIZED_OUTPUT_TIMEOUT {
            self.timed_out = true;
            return false;
        }
        true
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        (!self.timed_out)
            .then_some(self.active_since?)
            .map(|active_since| active_since + SYNCHRONIZED_OUTPUT_TIMEOUT)
    }

    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsApplySource {
    ConfigReload,
    OverlayEdit,
}

/// Application state driving the `winit` event loop.
///
/// The window is created lazily on `resumed` per `winit`'s portability
/// contract, and any startup failure is captured so it can be surfaced after
/// the loop returns.
pub(super) struct App {
    options: NativeOptions,
    /// Active presentation theme (selected once from `ODYTTY_THEME`). Used for
    /// the surface clear color; the default cell colors are applied process-wide
    /// at startup via `text::set_default_colors`. Presentation-only.
    theme: Theme,
    /// Active optional visual treatment (selected once from `ODYTTY_VISUAL`,
    /// default off). Drives the ambient scanline uniform; presentation-only and
    /// fully disableable. The core never sees it.
    visual: VisualEffect,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    /// The terminal model shared with the PTY pump thread. Snapshots are taken
    /// under this lock on the UI thread, then the lock is dropped before any GPU
    /// work so it is never held across `wgpu` calls.
    terminal: Arc<Mutex<Terminal>>,
    /// Set when the pump thread reports new output; the next `RedrawRequested`
    /// rebuilds the vertex buffer once (coalescing many wakes into one rebuild).
    needs_rebuild: bool,
    /// Signature of the geometry currently uploaded to the retained GPU vertex
    /// buffer. Used to distinguish full rebuilds, bounded cursor-tail updates,
    /// and retained-buffer redraws.
    last_render_signature: Option<RenderSignature>,
    /// Native presentation epoch for pixel-affecting state outside the terminal
    /// core revision: theme/default-color changes, atlas/font changes, and
    /// other settings that make identical snapshots build different vertices.
    presentation_epoch: u64,
    /// DECSET 2026 presentation hold state. While active, terminal model/input
    /// keep advancing, but new grid-content uploads are delayed until DECRST or
    /// this state machine's timeout releases the hold.
    synchronized_output_hold: SynchronizedOutputHold,
    /// Last uploaded content snapshot before cursor blink visibility was
    /// applied. This lets cursor-only redraws continue during a synchronized
    /// output hold without exposing newer terminal grid content.
    last_presented_snapshot: Option<Snapshot>,
    last_presented_cursor_style: crate::core::CursorStyle,
    last_presented_cursor_blinking: bool,
    /// The shared PTY writer. Key presses are encoded to bytes and written here,
    /// completing the read+write loop with the pump thread that owns the reader.
    writer: PtyWriter,
    /// The shared PTY session, used to push the new window size to the kernel
    /// (`TIOCSWINSZ`) on resize so shell/TUI programs see the updated `$COLUMNS`
    /// and `$LINES`. Shared with `run_native`, which reaps the child on exit.
    pty: Arc<Mutex<PtySession>>,
    /// The terminal grid size last applied to the model and PTY. Tracked so a
    /// `Resized` event that does not change the whole-cell grid skips redundant
    /// model/PTY resize work (idempotence): only surface reconfigure runs.
    pub(super) grid: Dimensions,
    /// Latest modifier state, tracked across `ModifiersChanged` events so a key
    /// press can be encoded with the Ctrl/Alt/Shift held at press time. `winit`
    /// delivers modifier changes separately from key events, so this must be
    /// remembered rather than read off each `KeyboardInput`.
    modifiers: Modifiers,
    /// Native-only Super/Logo modifier state. This is deliberately kept out of
    /// `input::Modifiers` because Super-based local shortcuts must not affect
    /// PTY key encoding.
    super_key: bool,
    key_bindings: KeyBindings,
    settings: Settings,
    settings_reloader: SettingsReloader,
    /// Current selection anchored to absolute rows in the
    /// scrollback+visible-screen space. Native owns this UI state; the terminal
    /// core remains unaware of selections and clipboard operations.
    selection: AbsoluteSelectionState,
    /// ID1: when set, the authored theme `cursor`/`selection`/`search` roles
    /// drive cursor color and selection/search highlight fills (with
    /// RV1-floored foregrounds) instead of the historical inverse / hardcoded
    /// treatments. Default-on by operator decision; `themed_ui_roles = off`
    /// restores the legacy rendering path.
    themed_ui_roles: bool,
    /// Most recent pointer position mapped to a terminal cell. `winit` mouse
    /// button events do not carry coordinates, so press/release use this cached
    /// cell from the latest cursor movement.
    pointer_cell: Option<CellPoint>,
    /// Most recent pointer position in physical pixels (the raw `winit`
    /// `CursorMoved` coordinates), cached alongside `pointer_cell`. SGR-pixel
    /// mouse reporting (DECSET 1016) needs true pixel coordinates, which the
    /// cell cache cannot reconstruct; button/wheel events carry no coordinates
    /// so they reuse this cached position. `None` until the first cursor move
    /// and after a resize (geometry changed).
    pointer_px: Option<(f64, f64)>,
    /// Test-only cell-size override (MOUSE-SCROLLBAR). Headless tests have no
    /// GPU, so the cell size that the pointer hit-tests need cannot come from
    /// `GpuState`. When set, [`App::resolved_cell`] returns it; in non-test
    /// builds this field does not exist and the cell always comes from the GPU,
    /// so production is byte-identical.
    #[cfg(test)]
    test_cell: Option<CellSize>,
    /// Hyperlink currently under the pointer in the visible viewport.
    hovered_hyperlink: Option<LinkId>,
    /// Typed pointer-drag state (MOUSE-EXTEND scaffold): `None` when idle,
    /// `Select { granularity, .. }` while the left button extends a selection
    /// (char / word / line). Replaces the bare `selecting` boolean so word/line
    /// drags can stay live and so later pointer gestures (block select, scroll
    /// thumb) get one mutually-exclusive home.
    pointer_drag: PointerDrag,
    /// The anchored word/line range for an in-progress word/line drag-extend
    /// (MOUSE-EXTEND). Fixed at the double/triple-click unit; each drag motion
    /// unions it with the unit under the pointer. `None` for char drags and when
    /// no drag is active.
    drag_anchor_unit: Option<AbsoluteSelectionRange>,
    /// Same-cell click counter for double-click word and triple-click line
    /// selection.
    clicks: ClickTracker,
    /// Last bounded drag-edge autoscroll step while extending a selection.
    last_selection_autoscroll: Option<Instant>,
    /// Button currently held for host mouse reporting. Kept separate from local
    /// selection so TUI mouse mode can suppress selection without losing drag
    /// reports.
    report_button: Option<CoreMouseButton>,
    /// Scrollback viewport offset (0 == live). Mouse wheel and Shift+PageUp/
    /// PageDown move it; any PTY-bound input snaps it back to live.
    viewport: Viewport,
    /// Native scrollback search state. It is UI-only: queries and highlights
    /// mutate snapshot copies, never terminal-core state.
    search: SearchUi,
    /// Native in-window overlay state. It is presentation-only: widgets
    /// composite into snapshot copies and never mutate terminal state or PTY.
    overlay: OverlayUi,
    /// Viewport offset to restore when the search bar closes. Search result
    /// navigation is temporary UI movement; closing search returns to the view
    /// the operator was inspecting before opening the bar.
    search_restore_viewport: Option<usize>,
    /// Scrollback length observed at the last rebuild, used to anchor the
    /// scrolled-back view as new output grows scrollback (the "stay scrolled"
    /// policy in [`Viewport::anchor_after_growth`]).
    last_scrollback_len: usize,
    /// Native-side clipboard owner. Kept alive across copy/paste operations so
    /// Linux clipboard contents remain served after Ctrl+Shift+C.
    clipboard: NativeClipboard,
    resize_debounce: ResizeDebouncer,
    /// Cursor blink phase driver. Toggles only when the active cursor style
    /// blinks and the window is focused; otherwise the cursor is solid and the
    /// loop is not woken for it.
    cursor_blink: CursorBlinkState,
    /// Whether the window currently holds focus. Blink pauses (cursor solid)
    /// while unfocused, matching common terminal behavior.
    focused: bool,
    autoclose: Option<Duration>,
    deadline: Option<Instant>,
    pub(super) startup_error: Option<NativeError>,
}

impl App {
    pub(super) fn new(
        options: NativeOptions,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
        settings: Settings,
        settings_reloader: SettingsReloader,
    ) -> Self {
        let grid = options.initial_grid;
        let theme = settings.theme;
        let visual = settings.visual;
        let key_bindings = KeyBindings::from_overrides(&settings.key_bindings);
        let autoclose = settings.native_autoclose;
        let themed_ui_roles = settings.themed_ui_roles;
        let overlay = OverlayUi::new(&settings);
        Self {
            options,
            theme,
            visual,
            window: None,
            gpu: None,
            terminal,
            needs_rebuild: true,
            last_render_signature: None,
            presentation_epoch: 0,
            synchronized_output_hold: SynchronizedOutputHold::default(),
            last_presented_snapshot: None,
            last_presented_cursor_style: crate::core::CursorStyle::default(),
            last_presented_cursor_blinking: true,
            writer,
            pty,
            grid,
            modifiers: Modifiers::default(),
            super_key: false,
            key_bindings,
            settings,
            settings_reloader,
            selection: AbsoluteSelectionState::default(),
            themed_ui_roles,
            pointer_cell: None,
            pointer_px: None,
            #[cfg(test)]
            test_cell: None,
            hovered_hyperlink: None,
            pointer_drag: PointerDrag::None,
            drag_anchor_unit: None,
            clicks: ClickTracker::default(),
            last_selection_autoscroll: None,
            report_button: None,
            viewport: Viewport::default(),
            search: SearchUi::default(),
            overlay,
            search_restore_viewport: None,
            last_scrollback_len: 0,
            clipboard: NativeClipboard::default(),
            resize_debounce: ResizeDebouncer::new(RESIZE_DEBOUNCE_INTERVAL),
            cursor_blink: CursorBlinkState::new(CURSOR_BLINK_INTERVAL),
            // Assume focused at startup; the first `Focused` event corrects it.
            focused: true,
            autoclose,
            deadline: None,
            startup_error: None,
        }
    }

    /// Resize the terminal model and PTY to fit the new physical surface size.
    ///
    /// Idempotent: when the computed whole-cell grid is unchanged (a sub-cell
    /// pixel change, or a duplicate event), no model or PTY resize is performed
    /// and `false` is returned. The GPU surface itself is reconfigured by the
    /// caller regardless, since it tracks pixel size, not the cell grid.
    ///
    /// Lock scopes are kept tight and never nested: the terminal mutex is taken
    /// and dropped for the model resize, then the PTY mutex is taken and dropped
    /// for the (non-blocking) `TIOCSWINSZ`. Neither is held across the other or
    /// across any GPU call.
    #[cfg(test)]
    pub(super) fn resize_grid(&mut self, cell: CellSize, width_px: u32, height_px: u32) -> bool {
        self.resize_grid_with_padding(cell, WindowPadding::ZERO, width_px, height_px)
    }

    /// Test seam (UX4-P1): open the settings overlay through the production
    /// keyboard entry path (so the pointer-state reset is genuinely exercised),
    /// without a window/GPU.
    #[cfg(test)]
    pub(super) fn open_settings_overlay_for_test(&mut self) {
        self.toggle_settings_overlay();
    }

    /// Test seam (UX4-P1): close the overlay (Esc-equivalent), without a
    /// window/GPU.
    #[cfg(test)]
    pub(super) fn close_overlay_for_test(&mut self) {
        self.overlay.close();
    }

    /// Test seam (UX4-P1): inject a cached pointer cell, as `update_pointer_cell`
    /// would after a `CursorMoved`, so a press has coordinates.
    #[cfg(test)]
    pub(super) fn set_pointer_cell_for_test(&mut self, row: usize, column: usize) {
        self.pointer_cell = Some(CellPoint { row, column });
    }

    /// Test seam (UX4-P1): the live overlay rect for the current grid.
    #[cfg(test)]
    pub(super) fn overlay_rect_for_test(&self) -> Option<super::overlay::OverlayRect> {
        overlay_rect(&self.overlay, self.grid.columns, self.grid.rows)
    }

    /// Test seam (UX4-P1): whether a local text selection is in progress.
    #[cfg(test)]
    pub(super) fn selecting_for_test(&self) -> bool {
        self.pointer_drag.is_selecting()
    }

    /// Test seam (MOUSE-EXTEND): force the drag-extend feature flag so a test can
    /// exercise both the default-on path and the byte-identical off branch.
    #[cfg(test)]
    pub(super) fn set_selection_drag_extend_for_test(&mut self, on: bool) {
        self.settings.selection_drag_extend = on;
    }

    /// Test seam (MOUSE-EXTEND): drive the left-press selection dispatch (the
    /// click-count / Shift+click-extend entry the `MouseInput` arm calls).
    #[cfg(test)]
    pub(super) fn begin_selection_for_test(&mut self) {
        self.begin_selection();
    }

    /// Test seam (MOUSE-EXTEND): drive the left-release finalize the
    /// `MouseInput` arm calls.
    #[cfg(test)]
    pub(super) fn finish_selection_for_test(&mut self) {
        self.finish_selection();
    }

    /// Test seam (MOUSE-EXTEND): drive the granularity-aware drag-extend the
    /// `CursorMoved` handler runs, without a GPU/pixel path. Wraps the
    /// production `extend_drag_to` (not a parallel reimplementation).
    #[cfg(test)]
    pub(super) fn extend_drag_to_cell_for_test(&mut self, row: usize, column: usize) {
        self.extend_drag_to(CellPoint { row, column });
    }

    /// Test seam (MOUSE-EXTEND): the text the current selection would copy,
    /// through the exact `current_selection_text` path PRIMARY/CLIPBOARD use.
    #[cfg(test)]
    pub(super) fn selection_text_for_test(&self) -> Option<String> {
        self.current_selection_text()
    }

    /// Test seam (MOUSE-EXTEND): whether finishing the current drag would write
    /// PRIMARY. Lets a regression prove a plain double/triple-click (no drag)
    /// stays no-write (parity) while a drag that extended does write.
    #[cfg(test)]
    pub(super) fn drag_should_write_primary_for_test(&self) -> bool {
        self.drag_selection_should_write_primary()
    }

    /// Test seam (MOUSE-EXTEND): set the Shift modifier so a Shift+click-extend
    /// gesture can be driven through `begin_selection`.
    #[cfg(test)]
    pub(super) fn set_shift_modifier_for_test(&mut self, shift: bool) {
        self.modifiers.shift = shift;
    }

    /// Test seam (UX4-P1): the held mouse-report button, if any.
    #[cfg(test)]
    pub(super) fn report_button_for_test(&self) -> Option<CoreMouseButton> {
        self.report_button
    }

    /// Test seam (UX4-P1): whether the no-overlay path would report the pointer
    /// to the PTY (TUI mouse mode active and Shift not held). Lets a precedence
    /// test assert reporting is armed yet an overlay press still does not leak.
    #[cfg(test)]
    pub(super) fn would_report_mouse_to_pty_for_test(&self) -> bool {
        self.should_report_mouse_to_pty()
    }

    /// Test seam (UX4-P1): the overlay render signature (mode + panel state).
    #[cfg(test)]
    pub(super) fn overlay_signature_for_test(&self) -> super::overlay::OverlayRenderSignature {
        self.overlay.render_signature()
    }

    /// Test seam (UX4-P2): absolute track-end cells for the first visible
    /// slider, so a test can drive a real press/drag/release through the App.
    #[cfg(test)]
    pub(super) fn overlay_first_slider_track_cells_for_test(
        &self,
    ) -> Option<(CellPoint, CellPoint)> {
        self.overlay
            .first_slider_track_cells(self.grid.columns, self.grid.rows)
    }

    /// Test seam (UX4-P2): whether a settings-panel slider drag is in progress.
    #[cfg(test)]
    pub(super) fn overlay_is_dragging_for_test(&self) -> bool {
        self.overlay.is_settings_dragging()
    }

    /// Test seam (UX4-P2 review): drive the exact focus-loss drag-cancel the
    /// `WindowEvent::Focused(false)` arm runs, so a regression can prove a lost
    /// release on focus loss cannot leave a slider drag armed while the overlay
    /// stays open. Wraps the production helper (not a parallel reimplementation).
    #[cfg(test)]
    pub(super) fn cancel_overlay_drag_on_focus_loss_for_test(&mut self) {
        self.cancel_overlay_drag_on_focus_loss();
    }

    /// Test seam (UX4-P1): arm a held TUI mouse-report button exactly as a real
    /// reported press would, so a regression test can prove overlay entry clears
    /// it. Wraps the (module-private) `handle_reported_mouse_input`.
    #[cfg(test)]
    pub(super) fn arm_reported_mouse_press_for_test(&mut self, button: CoreMouseButton) {
        self.handle_reported_mouse_input(ElementState::Pressed, button);
    }

    /// Test seam (MOUSE-SCROLLBAR): inject a cell size so the pointer hit-test
    /// can run headlessly (no GPU). See [`App::test_cell`].
    #[cfg(test)]
    pub(super) fn set_test_cell_for_test(&mut self, cell: CellSize) {
        self.test_cell = Some(cell);
    }

    /// Test seam (MOUSE-SCROLLBAR): toggle the `scrollbar_drag` setting so the
    /// inverted-gate (off-switch) parity can be pinned.
    #[cfg(test)]
    pub(super) fn set_scrollbar_drag_for_test(&mut self, on: bool) {
        self.settings.scrollbar_drag = on;
    }

    /// Test seam (MOUSE-SCROLLBAR): set the cached raw pointer pixel position the
    /// button handlers hit-test against (button events carry no coordinates).
    #[cfg(test)]
    pub(super) fn set_pointer_px_for_test(&mut self, x: f64, y: f64) {
        self.pointer_px = Some((x, y));
    }

    /// Test seam (MOUSE-SCROLLBAR): scroll the viewport up into history so the
    /// scroll thumb becomes visible (offset clamps to the scrollback length).
    #[cfg(test)]
    pub(super) fn scroll_up_for_test(&mut self, lines: usize) {
        let scrollback_len = self.scrollback_len();
        self.viewport.scroll_up(lines, scrollback_len);
    }

    /// Test seam (MOUSE-SCROLLBAR): the current scrollback length.
    #[cfg(test)]
    pub(super) fn scrollback_len_for_test(&self) -> usize {
        self.scrollback_len()
    }

    /// Test seam (MOUSE-SCROLLBAR): the live viewport offset.
    #[cfg(test)]
    pub(super) fn viewport_offset_for_test(&self) -> usize {
        self.viewport.offset()
    }

    /// Test seam (MOUSE-SCROLLBAR): enable a TUI mouse-reporting mode (DECSET
    /// 1000) on the underlying terminal, so a press routes through the report
    /// path unless the scroll-thumb grab captures it first.
    #[cfg(test)]
    pub(super) fn enable_mouse_reporting_for_test(&mut self) {
        if let Ok(mut terminal) = self.terminal.lock() {
            terminal.advance(b"\x1b[?1000h");
        }
    }

    /// Test seam (MOUSE-SCROLLBAR): drive a real left button event through the
    /// production routing and classify the outcome, so the press precedence
    /// (scroll-thumb grab vs PTY report vs local selection) can be pinned
    /// without a GPU or a winit event loop.
    #[cfg(test)]
    pub(super) fn left_button_outcome_for_test(&mut self, pressed: bool) -> &'static str {
        let state = if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        };
        self.handle_mouse_input(state, WinitMouseButton::Left);
        if self.pointer_drag.scrollbar_grab().is_some() {
            "grab"
        } else if self.report_button.is_some() {
            "report"
        } else if self.pointer_drag.is_selecting() {
            "select"
        } else {
            "idle"
        }
    }

    pub(super) fn resize_grid_with_padding(
        &mut self,
        cell: CellSize,
        padding: WindowPadding,
        width_px: u32,
        height_px: u32,
    ) -> bool {
        let new_grid = grid_dimensions_for_with_padding(width_px, height_px, cell, padding);
        if new_grid == self.grid {
            return false;
        }
        self.grid = new_grid;

        if let Ok(mut terminal) = self.terminal.lock() {
            terminal.resize(new_grid.columns, new_grid.rows);
            // Update cell metrics on every resize/rescale so new graphics
            // placements use the current pixel cell size.
            terminal.set_cell_metrics(cell.width, cell.height);
        }
        if let Ok(pty) = self.pty.lock() {
            let _ = pty.resize(new_grid);
        }
        true
    }

    fn apply_grid_resize(&mut self, resize: PendingResize) {
        if self.resize_grid_with_padding(
            resize.cell,
            resize.padding,
            resize.width_px,
            resize.height_px,
        ) {
            self.selection.clear();
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
            self.last_selection_autoscroll = None;
            self.report_button = None;
            self.pointer_cell = None;
            self.pointer_px = None;
            self.hovered_hyperlink = None;
            // Reflow changes the row/scrollback layout; return to the live
            // bottom so the offset is never stale against the new geometry.
            // Search closes because its absolute row matches were computed
            // against the old layout. clamp() in the rebuild guards bounds
            // regardless.
            self.viewport.reset_to_live();
            self.search.reset_for_reflow();
            self.search_restore_viewport = None;
            self.needs_rebuild = true;
        }
    }

    fn record_pending_resize(&mut self, resize: PendingResize, now: Instant) {
        if let Some(due) = self.resize_debounce.record(resize, now) {
            self.apply_grid_resize(due);
        }
    }

    fn update_control_flow_deadline(&self, event_loop: &ActiveEventLoop) {
        let next = [
            self.deadline,
            self.resize_debounce.deadline(),
            self.cursor_blink.deadline(),
            self.settings_reloader.deadline(),
            self.synchronized_output_hold.deadline(),
        ]
        .into_iter()
        .flatten()
        .min();
        match next {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn scroll_indicator_color(&self) -> [f32; 4] {
        let (r, g, b) = self.theme.foreground;
        let mut color = text::foreground_linear(Color::Rgb(r, g, b));
        color[3] = 0.62;
        color
    }

    /// Record a fatal startup error and ask the loop to exit.
    fn fail(&mut self, event_loop: &ActiveEventLoop, err: NativeError) {
        self.startup_error = Some(err);
        event_loop.exit();
    }

    /// Encode a key event and write its bytes to the PTY.
    ///
    /// Maps the `winit` logical key (plus the cached [`Modifiers`]) onto the
    /// neutral [`Key`] model and defers byte production to the shared
    /// [`input::encode_key`]. Keys the prototype does not encode are dropped. The
    /// PTY writer is flushed after each write so the keystroke reaches the shell
    /// without buffering latency.
    fn handle_key_event(
        &mut self,
        logical: WinitKey,
        binding_key: WinitKey,
        physical: PhysicalKey,
        event_type: KeyEventType,
    ) {
        let mods = self.modifiers;
        let key_modes = self.key_modes();
        if event_type != KeyEventType::Release {
            let action = self
                .key_bindings
                .action_for(&binding_key, mods, self.super_key);
            if action == Some(BindableAction::SettingsPanel) {
                self.toggle_settings_overlay();
                return;
            }
            if action == Some(BindableAction::ThemePicker) {
                self.open_theme_picker_overlay();
                return;
            }
            if self.overlay.is_open() {
                self.handle_overlay_key(&logical);
                return;
            }
            if action == Some(BindableAction::Search) {
                self.toggle_search();
                return;
            }
            if self.search.is_open() {
                self.handle_search_key(logical);
                return;
            }
            match action {
                Some(BindableAction::Copy) => {
                    self.handle_copy_shortcut();
                    return;
                }
                Some(BindableAction::Paste) => {
                    self.handle_paste_shortcut();
                    return;
                }
                Some(BindableAction::ScrollPageUp) => {
                    self.scroll_viewport(self.page_lines() as isize);
                    return;
                }
                Some(BindableAction::ScrollPageDown) => {
                    self.scroll_viewport(-(self.page_lines() as isize));
                    return;
                }
                Some(BindableAction::Search)
                | Some(BindableAction::SettingsPanel)
                | Some(BindableAction::ThemePicker)
                | None => {}
            }
        }
        if self.overlay.is_open() {
            return;
        }

        let mut bytes = Vec::new();
        if let Some(key) = map_keypad_physical_key(physical) {
            bytes = input::encode_key_event(key, mods, key_modes, event_type);
        } else {
            match logical {
                // `Key::Character` may carry more than one char (composed input);
                // encode each so multi-char text still reaches the shell intact.
                WinitKey::Character(text) => {
                    for ch in text.chars() {
                        bytes.extend_from_slice(&input::encode_key_event(
                            Key::Char(ch),
                            mods,
                            key_modes,
                            event_type,
                        ));
                    }
                }
                WinitKey::Named(named) => {
                    if let Some(key) = map_named_key(named, mods.shift) {
                        bytes = input::encode_key_event(key, mods, key_modes, event_type);
                    }
                }
                // Dead keys / unidentified: nothing to send.
                _ => {}
            }
        }

        if bytes.is_empty() {
            return;
        }
        // Any keystroke that reaches the shell snaps the viewport back to live,
        // so typing always returns to the prompt at the bottom.
        self.return_to_live();
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    }

    fn toggle_settings_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.toggle_settings();
        self.request_selection_redraw();
    }

    fn open_theme_picker_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_theme_picker(&self.settings);
        self.request_selection_redraw();
    }

    fn open_theme_builder_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_theme_builder(&self.settings);
        self.request_selection_redraw();
    }

    fn handle_overlay_key(&mut self, logical: &WinitKey) {
        let Some(input) = overlay_input_from_winit(logical, self.modifiers) else {
            self.request_selection_redraw();
            return;
        };

        let outcome = self.overlay.handle_input(input);
        self.apply_overlay_outcome(outcome);
        self.request_selection_redraw();
    }

    fn key_modes(&self) -> KeyModes {
        self.terminal
            .lock()
            .map(|terminal| key_modes_from_core(terminal.keyboard_modes()))
            .unwrap_or_default()
    }

    fn toggle_search(&mut self) {
        if self.overlay.is_open() {
            self.overlay.close();
            self.request_selection_redraw();
        }
        if self.search.is_open() {
            self.close_search(true);
        } else {
            self.search_restore_viewport = Some(self.viewport.offset());
            self.search.open();
            self.selection.clear();
            self.pointer_drag = PointerDrag::None;
            self.drag_anchor_unit = None;
            self.last_selection_autoscroll = None;
            self.refresh_search_matches();
        }
        self.request_selection_redraw();
    }

    fn close_search(&mut self, restore_viewport: bool) {
        self.search.close();
        let restore_offset = restore_viewport
            .then(|| self.search_restore_viewport.take())
            .flatten();
        self.search_restore_viewport = None;

        if let Some(offset) = restore_offset {
            let scrollback_len = self.scrollback_len();
            if self.viewport.jump_to(offset, scrollback_len) {
                self.on_viewport_changed();
            }
        }
    }

    fn handle_search_key(&mut self, logical: WinitKey) {
        match logical {
            WinitKey::Named(NamedKey::Escape) => {
                self.close_search(true);
                self.request_selection_redraw();
            }
            WinitKey::Named(NamedKey::Enter) => {
                self.refresh_search_matches();
                if self.modifiers.shift {
                    self.search.prev();
                } else {
                    self.search.next();
                }
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            WinitKey::Named(NamedKey::Backspace) => {
                self.search.backspace();
                self.refresh_search_matches();
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            WinitKey::Named(NamedKey::Space) if !self.modifiers.ctrl && !self.modifiers.alt => {
                self.search.push_char(' ');
                self.refresh_search_matches();
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            WinitKey::Character(text) if !self.modifiers.ctrl && !self.modifiers.alt => {
                for ch in text.chars() {
                    self.search.push_char(ch);
                }
                self.refresh_search_matches();
                self.jump_to_current_search_match();
                self.request_selection_redraw();
            }
            _ => {}
        }
    }

    fn refresh_search_matches(&mut self) {
        if !self.search.is_open() {
            return;
        }
        if let Ok(terminal) = self.terminal.lock() {
            self.search.refresh(&terminal);
        }
    }

    fn jump_to_current_search_match(&mut self) {
        let scrollback_len = self.scrollback_len();
        let Some(offset) = self
            .search
            .viewport_offset_for_current(scrollback_len, self.grid)
        else {
            return;
        };
        if self.viewport.jump_to(offset, scrollback_len) {
            self.on_viewport_changed();
        }
    }

    /// Paste clipboard text into the PTY if the platform clipboard is
    /// reachable. Clipboard failures are deliberately non-fatal: a terminal
    /// should keep running even when the compositor denies clipboard access.
    fn handle_paste_shortcut(&mut self) {
        let Some(text) = self.clipboard.read_text() else {
            return;
        };
        // Paste writes to the PTY, so treat it like typed input: return to live.
        self.return_to_live();
        let _ = write_paste_text(&self.terminal, &self.writer, &text);
    }

    /// Copy the current visible selection to the clipboard. This is kept fully
    /// native-side: the selected text is derived from a snapshot copy and no
    /// terminal state is mutated.
    fn handle_copy_shortcut(&mut self) {
        let Some(text) = self.current_selection_text() else {
            return;
        };
        let _ = self.clipboard.write_text(text.as_str());
    }

    fn handle_terminal_clipboard_requests(&mut self) {
        let requests = self
            .terminal
            .lock()
            .map(|mut terminal| terminal.take_clipboard_requests())
            .unwrap_or_default();

        for request in requests {
            match request {
                ClipboardRequest::Write { selection, text } => {
                    let _ = write_clipboard_selection(&mut self.clipboard, selection, &text);
                }
                ClipboardRequest::Read { selection } => {
                    if !self.settings.osc52_read {
                        continue;
                    }
                    let Some(text) = read_clipboard_selection(&mut self.clipboard, selection)
                    else {
                        continue;
                    };
                    let host_output = self
                        .terminal
                        .lock()
                        .map(|mut terminal| {
                            terminal.answer_clipboard_read(selection, &text);
                            terminal.take_host_output()
                        })
                        .unwrap_or_default();
                    if !host_output.is_empty() {
                        self.write_pty_bytes(&host_output);
                    }
                }
            }
        }
    }

    fn update_window_title(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let Some(title) = ({
            let mut terminal = self.terminal.lock().expect("terminal mutex");
            changed_window_title(&mut terminal, &self.options.title)
        }) else {
            return;
        };

        window.set_title(&title);
    }

    fn options_for_settings(&self, settings: &Settings) -> NativeOptions {
        let parsed = NativeOptions::from_settings(settings);
        NativeOptions {
            title: self.options.title.clone(),
            initial_grid: self.options.initial_grid,
            font_family: parsed.font_family,
            font_path: parsed.font_path,
            font_size_px: parsed.font_size_px,
            text_gamma: parsed.text_gamma,
            subpixel: parsed.subpixel,
            window_padding_px: parsed.window_padding_px,
        }
    }

    fn poll_config_reload(&mut self, now: Instant) {
        match self.settings_reloader.poll(now) {
            SettingsReloadOutcome::Unchanged | SettingsReloadOutcome::Deleted => {}
            SettingsReloadOutcome::Reloaded(settings) => self.apply_reloaded_settings(settings),
            SettingsReloadOutcome::Invalid { warnings } => {
                for warning in warnings {
                    eprintln!("odytty: config reload ignored: {warning}");
                }
            }
            SettingsReloadOutcome::Unreadable { message } => {
                eprintln!("odytty: config reload ignored: {message}");
            }
        }
    }

    fn apply_reloaded_settings(&mut self, reloaded: Settings) {
        self.apply_settings_through_reload_seam(reloaded, SettingsApplySource::ConfigReload);
    }

    fn apply_overlay_settings(&mut self, reloaded: Settings) {
        self.apply_settings_through_reload_seam(reloaded, SettingsApplySource::OverlayEdit);
    }

    fn save_overlay_settings(&mut self, changes: &[crate::settings::SettingEdit]) {
        let Some(path) = self.settings_reloader.config_path() else {
            self.overlay
                .save_failed("could not resolve odytty.conf path".to_owned());
            return;
        };
        match write_settings_changes_to_path(path, changes) {
            Ok(result) => self.overlay.save_succeeded(result.changed),
            Err(error) => self.overlay.save_failed(error.to_string()),
        }
    }

    fn save_overlay_theme(&mut self, request: super::theme_builder::ThemeBuilderSaveRequest) {
        let Some(config_path) = self.settings_reloader.config_path() else {
            self.overlay
                .save_failed("could not resolve odytty.conf path".to_owned());
            return;
        };
        let Some(theme_dir) = user_theme_dir_for_config(config_path) else {
            self.overlay
                .save_failed("could not resolve theme directory".to_owned());
            return;
        };
        let saved_name = request.name.clone();
        let path = match save_theme_to_dir(&theme_dir, &request) {
            Ok(path) => path,
            Err(error) => {
                self.overlay
                    .save_failed(format!("could not write theme file: {error}"));
                return;
            }
        };
        let changes = [SettingEdit {
            key: "theme",
            env: THEME_ENV,
            value: saved_name.clone(),
        }];
        match write_settings_changes_to_path(config_path, &changes) {
            Ok(result) => {
                self.overlay
                    .theme_builder_save_succeeded(&saved_name, &path, result.changed)
            }
            Err(error) => self.overlay.save_failed(error.to_string()),
        }
    }

    fn apply_settings_through_reload_seam(
        &mut self,
        reloaded: Settings,
        source: SettingsApplySource,
    ) {
        let mut next_settings = self.settings.clone();
        if !apply_reloadable_values(&mut next_settings, reloaded) {
            return;
        }

        let next_options = self.options_for_settings(&next_settings);
        let (text_rebuilt, padding_changed) = match self.gpu.as_mut() {
            Some(gpu) => {
                let text_rebuilt = match gpu
                    .apply_text_options(&next_options, next_settings.effective_stem_darken())
                {
                    Ok(changed) => changed,
                    Err(err) => {
                        eprintln!("odytty: config reload ignored: {err}");
                        return;
                    }
                };
                let padding_changed = gpu.set_window_padding_px(next_options.window_padding_px);
                (text_rebuilt, padding_changed)
            }
            None => (false, false),
        };

        self.settings = next_settings;
        self.options = next_options;
        self.theme = self.settings.theme;
        self.visual = self.settings.visual;
        self.themed_ui_roles = self.settings.themed_ui_roles;
        self.key_bindings = KeyBindings::from_overrides(&self.settings.key_bindings);
        match source {
            SettingsApplySource::ConfigReload => self.overlay.refresh_settings(&self.settings),
            SettingsApplySource::OverlayEdit => self.overlay.apply_settings(&self.settings),
        }
        text::set_default_colors(self.theme.foreground, self.theme.background);
        text::set_ansi_palette(&self.theme.palette);
        // RV1: republish the minimum-contrast floor so a live `min_contrast`
        // edit takes effect on the next frame (the grid resolve seam reads it
        // per cell). Mirrors the palette republish above; passthrough at 1.0.
        text::set_min_contrast(self.settings.effective_min_contrast());
        if let Ok(mut terminal) = self.terminal.lock() {
            // ID1: when themed UI roles are on, the cursor default color comes
            // from the theme `cursor` role; otherwise it stays the foreground
            // (today's behavior). A live OSC 12 dynamic-color override is a
            // separate mechanism in the core and still takes precedence.
            let cursor_default = if self.themed_ui_roles {
                rgb(self.theme.cursor)
            } else {
                rgb(self.theme.foreground)
            };
            terminal.set_base_colors(
                rgb(self.theme.foreground),
                rgb(self.theme.background),
                cursor_default,
            );
            terminal.set_osc52_read_enabled(self.settings.osc52_read);
            terminal.set_cursor_defaults(
                self.settings.cursor_style,
                self.settings.cursor_blink.enabled(),
            );
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_theme(self.theme);
            gpu.set_visual(self.visual);
            gpu.set_text_gamma(self.settings.text_gamma);
            gpu.set_bloom(bloom_options(&self.settings));
            gpu.set_crt(crt_options(&self.settings));
        }

        if text_rebuilt || padding_changed {
            let resize = self.gpu.as_ref().and_then(|gpu| {
                let cell = gpu.cell();
                if let Ok(mut terminal) = self.terminal.lock() {
                    terminal.set_cell_metrics(cell.width, cell.height);
                }
                self.window.as_ref().map(|window| {
                    pending_resize_for_surface(cell, gpu.window_padding(), window.inner_size())
                })
            });
            if let Some(resize) = resize {
                self.apply_grid_resize(resize);
            }
        }

        self.last_render_signature = None;
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);

        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// ID1: themed selection treatment, or `None` (today's inverse) when the
    /// operator opts out. The fill is the theme `selection` role verbatim; the
    /// foreground is the theme foreground floored over that fill through the
    /// RV1 minimum-contrast machinery, so it stays legible at the active
    /// `min_contrast` (identity at the default 1.0).
    fn themed_selection_style(&self) -> Option<SelectionStyle> {
        if !self.themed_ui_roles {
            return None;
        }
        let fill = [
            self.theme.selection.0,
            self.theme.selection.1,
            self.theme.selection.2,
        ];
        let fg = floor_fg_over(
            self.theme.foreground,
            fill,
            self.settings.effective_min_contrast(),
        );
        Some(SelectionStyle { fill, fg })
    }

    /// ID1: themed search-highlight treatment, or `None` (today's inverse /
    /// black-on-yellow) when the operator opts out. Non-active matches use the
    /// theme `search` role; the active match uses a brightened OKLab derivative
    /// of it. Both foregrounds are RV1-floored over their fills.
    fn themed_search_style(&self) -> Option<SearchStyle> {
        if !self.themed_ui_roles {
            return None;
        }
        let fill = [
            self.theme.search.0,
            self.theme.search.1,
            self.theme.search.2,
        ];
        let fill_lin = srgb_tuple_to_linear(self.theme.search);
        let active_fill_lin =
            crate::color::mix_oklab(fill_lin, [1.0, 1.0, 1.0], SEARCH_ACTIVE_BRIGHTEN);
        let active_fill = linear_to_srgb_tuple(active_fill_lin);
        let fg = floor_fg_over(
            self.theme.foreground,
            fill,
            self.settings.effective_min_contrast(),
        );
        let active_fg = floor_fg_over(
            self.theme.foreground,
            active_fill,
            self.settings.effective_min_contrast(),
        );
        Some(SearchStyle {
            fill,
            fg,
            active_fill,
            active_fg,
        })
    }

    fn update_held_cursor_frame(&mut self, now: Instant) -> bool {
        let Some(mut snapshot) = self.last_presented_snapshot.clone() else {
            return false;
        };
        let Some(previous_signature) = self.last_render_signature.clone() else {
            return false;
        };

        let cursor_on =
            self.cursor_blink
                .poll(now, self.last_presented_cursor_blinking, self.focused);
        if !cursor_on {
            snapshot.cursor_visible = false;
        }

        let signature = RenderSignature {
            content: previous_signature.content,
            cursor: CursorRenderSignature {
                visible: snapshot.cursor_visible,
                style: self.last_presented_cursor_style,
            },
        };
        let update = RenderSignature::update_from(self.last_render_signature.as_ref(), &signature);
        if let Some(gpu) = self.gpu.as_mut() {
            match update {
                GeometryUpdate::Full | GeometryUpdate::CursorOnly => {
                    gpu.update_cursor_and_overlays(
                        &snapshot,
                        self.last_presented_cursor_style,
                        &[],
                    );
                }
                GeometryUpdate::Retained => {}
            }
        }
        self.last_render_signature = Some(signature);
        true
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let (w, h) = self.options.window_logical_size();
        let attributes = Window::default_attributes()
            .with_title(self.options.title.clone())
            .with_inner_size(LogicalSize::new(w, h));

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                self.fail(event_loop, NativeError::WindowCreation(err.to_string()));
                return;
            }
        };

        // Seed the first buffer from the current shared-terminal snapshot (any
        // PTY output already pumped is picked up by the first redraw below).
        let initial_snapshot = self.terminal.lock().expect("terminal mutex").snapshot();
        match GpuState::new(
            window.clone(),
            &self.options,
            &initial_snapshot,
            self.theme,
            self.visual,
            self.settings.effective_stem_darken(),
            bloom_options(&self.settings),
            crt_options(&self.settings),
        ) {
            Ok(gpu) => {
                // Push live cell pixel metrics to the terminal core so graphics
                // placements (sixel/kitty) compute the correct cell extent.
                let cell = gpu.cell();
                if let Ok(mut term) = self.terminal.lock() {
                    term.set_cell_metrics(cell.width, cell.height);
                }
                self.last_presented_snapshot = Some(initial_snapshot.clone());
                self.gpu = Some(gpu);
            }
            Err(err) => {
                self.fail(event_loop, err);
                return;
            }
        }

        self.needs_rebuild = true;
        window.request_redraw();
        self.window = Some(window);

        if let Some(delay) = self.autoclose {
            let deadline = Instant::now() + delay;
            self.deadline = Some(deadline);
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                // Reconfigure the GPU surface (pixel size) and read the real
                // per-cell metric so the grid fit matches what is drawn. The
                // surface updates immediately; the terminal model + PTY winsize
                // are debounced so drag bursts do not reflow on every event.
                let resize = self.gpu.as_mut().map(|gpu| {
                    gpu.resize(size.width, size.height);
                    pending_resize_for_surface(gpu.cell(), gpu.window_padding(), size)
                });

                if let Some(resize) = resize {
                    self.record_pending_resize(resize, Instant::now());
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                self.update_control_flow_deadline(event_loop);
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                mut inner_size_writer,
            } => {
                let size = self
                    .window
                    .as_ref()
                    .map(|window| window.inner_size())
                    .unwrap_or_else(|| PhysicalSize::new(0, 0));
                let _ = inner_size_writer.request_inner_size(size);

                let resize = self.gpu.as_mut().and_then(|gpu| {
                    gpu.resize(size.width, size.height);
                    let scale = scale_factor as f32;
                    if !scale_factor_changed(gpu.scale(), scale) || !gpu.set_scale(scale) {
                        return None;
                    }
                    Some(pending_resize_for_surface(
                        gpu.cell(),
                        gpu.window_padding(),
                        size,
                    ))
                });

                if let Some(resize) = resize {
                    self.needs_rebuild = true;
                    self.last_render_signature = None;
                    self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
                    self.record_pending_resize(resize, Instant::now());
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                self.update_control_flow_deadline(event_loop);
            }
            WindowEvent::RedrawRequested => {
                self.handle_terminal_clipboard_requests();
                self.update_window_title();
                // Rebuild geometry at most once per redraw, no matter how many
                // pump wakes coalesced into this frame. Snapshot under the lock,
                // then drop it before touching the GPU.
                if self.needs_rebuild {
                    let now = Instant::now();
                    let synchronized_output = self
                        .terminal
                        .lock()
                        .map(|terminal| terminal.synchronized_output_enabled())
                        .unwrap_or(false);
                    if self
                        .synchronized_output_hold
                        .should_hold(synchronized_output, now)
                    {
                        let _ = self.update_held_cursor_frame(now);
                    } else {
                        let Some(cell) = self.gpu.as_ref().map(GpuState::cell) else {
                            return;
                        };
                        let cached_image_ids = self
                            .gpu
                            .as_ref()
                            .map(GpuState::cached_image_ids)
                            .unwrap_or_default();
                        let (
                            mut snapshot,
                            scrollback_len,
                            cursor_style,
                            cursor_blinking,
                            terminal_revision,
                            visible_graphics,
                            image_uploads,
                        ) = {
                            let terminal = self.terminal.lock().expect("terminal mutex");
                            let scrollback_len = terminal.screen().scrollback_len();
                            // "Stay scrolled": as new output grows scrollback while
                            // the user is scrolled back, anchor the view to the same
                            // absolute rows instead of letting it scroll away. Only
                            // explicit input (handle_key_press/paste) returns to live.
                            let added = scrollback_len.saturating_sub(self.last_scrollback_len);
                            self.viewport.anchor_after_growth(added, scrollback_len);
                            self.last_scrollback_len = scrollback_len;
                            self.viewport.clamp(scrollback_len);
                            if self.search.is_open() {
                                self.search.refresh(&terminal);
                            }
                            let offset = self.viewport.offset();
                            let visible_graphics = terminal.visible_graphics(offset);
                            let image_uploads = image_uploads_for_visible(
                                &terminal,
                                &visible_graphics,
                                &cached_image_ids,
                            );
                            (
                                terminal.snapshot_with_scrollback(offset),
                                scrollback_len,
                                terminal.cursor_style(),
                                terminal.cursor_blinking(),
                                terminal.render_revision(),
                                visible_graphics,
                                image_uploads,
                            )
                        };
                        // Blink phase: hide the cursor during the off-phase. Only the
                        // live view (offset 0) shows a cursor; the blink driver holds
                        // it solid when not blinking or unfocused.
                        let base_cursor_visible = snapshot.cursor_visible;
                        let cursor_on = self.cursor_blink.poll(now, cursor_blinking, self.focused);
                        if !cursor_on {
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
                        if let Some(range) = self.selection.range()
                            && let Some(visible_range) = selection::visible_range_from_absolute(
                                range,
                                self.viewport.offset(),
                                scrollback_len,
                                self.grid,
                            )
                        {
                            selection::apply_highlight(
                                &mut snapshot,
                                visible_range,
                                self.themed_selection_style(),
                            );
                        }
                        apply_search_ui(
                            &mut snapshot,
                            &self.search,
                            self.viewport.offset(),
                            scrollback_len,
                            self.grid,
                            self.themed_search_style(),
                        );
                        apply_overlay(&mut snapshot, &self.overlay);
                        apply_hyperlink_hover(&mut snapshot, self.hovered_hyperlink);
                        let content_snapshot = {
                            let mut content_snapshot = snapshot.clone();
                            content_snapshot.cursor_visible = base_cursor_visible;
                            content_snapshot
                        };
                        let color = self.scroll_indicator_color();
                        let overlay = self.gpu.as_ref().and_then(|gpu| {
                            scroll_indicator_quad_with_padding(
                                self.viewport.offset(),
                                scrollback_len,
                                self.grid,
                                gpu.cell(),
                                color,
                                gpu.window_padding(),
                            )
                        });
                        let overlays = overlay.into_iter().collect::<Vec<_>>();
                        let signature = RenderSignature {
                            content: RenderContentSignature {
                                terminal_revision,
                                viewport_offset: self.viewport.offset(),
                                scrollback_len,
                                grid: self.grid,
                                cell,
                                selection: self.selection.range().map(SelectionSignature::from),
                                search: self.search.render_signature(),
                                overlay: self.overlay.render_signature(),
                                hovered_hyperlink: self.hovered_hyperlink,
                                graphics: visible_graphics_signature(&visible_graphics),
                                presentation_epoch: self.presentation_epoch,
                            },
                            cursor: CursorRenderSignature {
                                visible: snapshot.cursor_visible,
                                style: cursor_style,
                            },
                        };
                        let update = RenderSignature::update_from(
                            self.last_render_signature.as_ref(),
                            &signature,
                        );
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
                        if let Some(gpu) = self.gpu.as_mut() {
                            match update {
                                GeometryUpdate::Full => {
                                    gpu.update_image_layer(&visible_graphics, &image_uploads);
                                    if overlays.is_empty() {
                                        gpu.update_from_snapshot(
                                            &snapshot,
                                            cursor_style,
                                            focus_dim,
                                        );
                                    } else {
                                        gpu.update_from_snapshot_with_overlays(
                                            &snapshot,
                                            cursor_style,
                                            &overlays,
                                            focus_dim,
                                        );
                                    }
                                }
                                GeometryUpdate::CursorOnly => {
                                    gpu.update_cursor_and_overlays(
                                        &snapshot,
                                        cursor_style,
                                        &overlays,
                                    );
                                }
                                GeometryUpdate::Retained => {}
                            }
                        }
                        self.last_render_signature = Some(signature);
                        self.last_presented_snapshot = Some(content_snapshot);
                        self.last_presented_cursor_style = cursor_style;
                        self.last_presented_cursor_blinking = cursor_blinking;
                        self.needs_rebuild = false;
                    }
                }
                let Some(gpu) = self.gpu.as_mut() else {
                    return;
                };
                match gpu.render() {
                    FrameOutcome::Presented | FrameOutcome::Skipped => {}
                    // Surface lost/outdated/suboptimal (e.g. after a resize or
                    // compositor change): reconfigure and try again next frame.
                    FrameOutcome::NeedsReconfigure => gpu.reconfigure(),
                }
            }
            // `winit` reports modifier state separately from key presses; cache
            // it so the next `KeyboardInput` encodes with Ctrl/Alt/Shift held.
            WindowEvent::ModifiersChanged(state) => {
                let state = state.state();
                self.modifiers = Modifiers {
                    ctrl: state.control_key(),
                    alt: state.alt_key(),
                    shift: state.shift_key(),
                };
                self.super_key = state.super_key();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused {
                    self.cancel_overlay_drag_on_focus_loss();
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
            WindowEvent::CursorMoved { position, .. } => {
                self.update_pointer_cell(position.x, position.y);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(state, button);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let event_type = match event.state {
                    ElementState::Pressed if event.repeat => KeyEventType::Repeat,
                    ElementState::Pressed => KeyEventType::Press,
                    ElementState::Released => KeyEventType::Release,
                };
                let binding_key = event.key_without_modifiers();
                self.handle_key_event(
                    event.logical_key,
                    binding_key,
                    event.physical_key,
                    event_type,
                );
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            // Coalesce: flag a rebuild and ask for one redraw. Many output
            // chunks between frames collapse into a single snapshot+rebuild
            // because `winit` merges redundant `request_redraw` calls and we
            // only rebuild when `needs_rebuild` is set.
            UserEvent::Redraw => {
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            // The shell exited (PTY EOF): close the window cleanly.
            UserEvent::ShellExited => event_loop.exit(),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if let Some(resize) = self.resize_debounce.take_due(now) {
            self.apply_grid_resize(resize);
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // A due cursor-blink toggle rebuilds once so the phase flips; the rebuild
        // path polls the blink driver and advances it.
        if self.cursor_blink.is_due(now) {
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        if self.synchronized_output_hold.is_due(now) {
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        self.poll_config_reload(now);

        if let Some(deadline) = self.deadline
            && now >= deadline
        {
            event_loop.exit();
            return;
        }

        self.update_control_flow_deadline(event_loop);
    }
}

fn rgb(color: (u8, u8, u8)) -> RgbColor {
    RgbColor::new(color.0, color.1, color.2)
}

/// How far the active search match is brightened toward white (OKLab mix) from
/// the `search` role, so it reads as distinct from non-active matches.
const SEARCH_ACTIVE_BRIGHTEN: f32 = 0.35;

fn srgb_tuple_to_linear(color: (u8, u8, u8)) -> crate::color::LinearRgb {
    [
        crate::color::srgb_to_linear(color.0),
        crate::color::srgb_to_linear(color.1),
        crate::color::srgb_to_linear(color.2),
    ]
}

fn linear_to_srgb_tuple(linear: crate::color::LinearRgb) -> [u8; 3] {
    [
        crate::color::linear_to_srgb_u8(linear[0]),
        crate::color::linear_to_srgb_u8(linear[1]),
        crate::color::linear_to_srgb_u8(linear[2]),
    ]
}

/// Floor a foreground over a fill so it meets `ratio` WCAG contrast (RV1).
/// Identity at `ratio <= 1.0` (the default `min_contrast`).
fn floor_fg_over(fg: (u8, u8, u8), bg: [u8; 3], ratio: f32) -> [u8; 3] {
    let fg_lin = srgb_tuple_to_linear(fg);
    let bg_lin = [
        crate::color::srgb_to_linear(bg[0]),
        crate::color::srgb_to_linear(bg[1]),
        crate::color::srgb_to_linear(bg[2]),
    ];
    linear_to_srgb_tuple(crate::color::enforce_min_contrast(fg_lin, bg_lin, ratio))
}

fn bloom_options(settings: &Settings) -> BloomOptions {
    BloomOptions {
        enabled: settings.effective_bloom_enabled(),
        threshold: settings.bloom_threshold,
        intensity: settings.bloom_intensity,
        radius: settings.bloom_radius,
    }
}

fn crt_options(settings: &Settings) -> CrtOptions {
    CrtOptions {
        enabled: settings.effective_crt_enabled(),
        scanline_intensity: settings.crt_scanline_intensity,
        scanline_period: settings.crt_scanline_period,
        vignette_strength: settings.crt_vignette_strength,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blink() -> CursorBlinkState {
        CursorBlinkState::new(Duration::from_millis(500))
    }

    #[test]
    fn plain_render_quality_forces_post_options_inactive() {
        let settings = Settings {
            render_quality: crate::settings::RenderQuality::Plain,
            bloom: true,
            crt: true,
            ..Settings::default()
        };

        let bloom = bloom_options(&settings);
        let crt = crt_options(&settings);

        assert!(!bloom.enabled);
        assert!(!crt.enabled);
    }

    #[test]
    fn blink_holds_solid_when_not_blinking() {
        let mut state = blink();
        let t0 = Instant::now();
        // Steady cursor: always on, no scheduled wake.
        assert!(state.poll(t0, false, true));
        assert_eq!(state.deadline(), None);
        assert!(state.poll(t0 + Duration::from_secs(10), false, true));
        assert_eq!(state.deadline(), None);
    }

    #[test]
    fn blink_holds_solid_when_unfocused() {
        let mut state = blink();
        let t0 = Instant::now();
        // Blinking requested but unfocused: solid, no wake scheduled.
        assert!(state.poll(t0, true, false));
        assert_eq!(state.deadline(), None);
    }

    #[test]
    fn blink_toggles_at_the_interval_when_focused() {
        let mut state = blink();
        let t0 = Instant::now();
        // First poll arms the phase (on) and schedules the next toggle.
        assert!(state.poll(t0, true, true));
        let deadline = state.deadline().expect("blink should schedule a wake");
        assert_eq!(deadline, t0 + Duration::from_millis(500));
        assert!(!state.is_due(t0));

        // Before the deadline: unchanged, still on.
        assert!(state.poll(t0 + Duration::from_millis(250), true, true));

        // At/after the deadline: flips to off and reschedules.
        assert!(state.is_due(t0 + Duration::from_millis(500)));
        assert!(!state.poll(t0 + Duration::from_millis(500), true, true));
        assert_eq!(
            state.deadline(),
            Some(t0 + Duration::from_millis(1000)),
            "next toggle is one interval later"
        );

        // Next interval flips back on.
        assert!(state.poll(t0 + Duration::from_millis(1000), true, true));
    }

    #[test]
    fn blink_resets_to_solid_when_focus_lost_mid_cycle() {
        let mut state = blink();
        let t0 = Instant::now();
        assert!(state.poll(t0, true, true));
        // Toggle to off-phase.
        assert!(!state.poll(t0 + Duration::from_millis(500), true, true));
        // Losing focus forces solid-on and clears the scheduled wake.
        assert!(state.poll(t0 + Duration::from_millis(600), true, false));
        assert_eq!(state.deadline(), None);
    }
}

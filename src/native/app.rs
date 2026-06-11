use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::core::{
    Color, Dimensions, KeyboardModes as CoreKeyboardModes, MouseButton as CoreMouseButton,
    MouseEventKind, MouseProtocol, Snapshot, Terminal,
};
use crate::graphics::{StoredImageId, VisiblePlacement};
use crate::input::{self, Key, KeyModes, Modifiers};
use crate::pty::PtySession;
use crate::selection::{self, AbsoluteSelectionState, CellPoint, ClickTracker};
use crate::settings::{
    BindableAction, Settings, SettingsReloadOutcome, SettingsReloader, apply_reloadable_values,
};
use crate::text::{self, CellSize};
use crate::theme::{Theme, VisualEffect};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::NamedKey;
use winit::keyboard::{Key as WinitKey, PhysicalKey};
use winit::window::{Window, WindowId};

use super::bindings::{
    KeyBindings, changed_window_title, encode_native_focus_report, encode_native_mouse_report,
    map_keypad_physical_key, map_named_key, map_winit_mouse_button, motion_report_button,
    wheel_report_button,
};
use super::clipboard::{NativeClipboard, selected_clipboard_text, write_paste_text};
use super::gpu::{FrameOutcome, GpuState};
use super::image_layer::ImageUpload;
use super::options::{NativeError, NativeOptions};
use super::pty::{PtyWriter, UserEvent};
use super::search_ui::{SearchUi, apply_search_ui};
use super::viewport::{
    SELECTION_AUTOSCROLL_INTERVAL, Viewport, grid_dimensions_for, scroll_indicator_quad,
    wheel_lines,
};

const RESIZE_DEBOUNCE_INTERVAL: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingResize {
    pub(super) cell: CellSize,
    pub(super) width_px: u32,
    pub(super) height_px: u32,
}

pub(super) fn pending_resize_for_surface(cell: CellSize, size: PhysicalSize<u32>) -> PendingResize {
    PendingResize {
        cell,
        width_px: size.width,
        height_px: size.height,
    }
}

pub(super) fn scale_factor_changed(current: f32, next: f32) -> bool {
    (next.max(1.0) - current.max(1.0)).abs() >= f32::EPSILON
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ResizeDebouncer {
    interval: Duration,
    pending: Option<PendingResize>,
    deadline: Option<Instant>,
    last_applied: Option<Instant>,
}

impl ResizeDebouncer {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            pending: None,
            deadline: None,
            last_applied: None,
        }
    }

    pub(super) fn record(&mut self, resize: PendingResize, now: Instant) -> Option<PendingResize> {
        if self
            .last_applied
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval)
        {
            self.pending = None;
            self.deadline = None;
            self.last_applied = Some(now);
            return Some(resize);
        }

        let deadline = self.last_applied.expect("checked") + self.interval;
        self.pending = Some(resize);
        self.deadline = Some(deadline);
        None
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Option<PendingResize> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            self.last_applied = Some(now);
            return self.pending.take();
        }
        None
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

/// Half-period of the cursor blink, i.e. the interval between on/off toggles.
/// ~530ms matches the long-standing xterm/VT default cadence.
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Drives the cursor blink on/off phase from injected time.
///
/// Policy (documented): the cursor only blinks when the active style requests it
/// (DECSCUSR or the host default) **and** the window is focused. When either is
/// false the cursor is held solid-on and no wake is scheduled, so an idle or
/// unfocused window never spins the event loop. While blinking, [`Self::poll`]
/// toggles at [`CURSOR_BLINK_INTERVAL`] and [`Self::deadline`] reports the next
/// toggle instant for `ControlFlow::WaitUntil`, bounding the wake rate.
#[derive(Debug, Clone, Copy)]
pub(super) struct CursorBlinkState {
    interval: Duration,
    on: bool,
    next_toggle: Option<Instant>,
}

impl CursorBlinkState {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            on: true,
            next_toggle: None,
        }
    }

    /// Update the blink phase for `now` and return whether the cursor is
    /// currently visible (on-phase). Solid-on (and deadline cleared) whenever the
    /// cursor is not blinking or the window is unfocused.
    pub(super) fn poll(&mut self, now: Instant, blinking: bool, focused: bool) -> bool {
        if !blinking || !focused {
            self.on = true;
            self.next_toggle = None;
            return true;
        }
        match self.next_toggle {
            None => {
                self.on = true;
                self.next_toggle = Some(now + self.interval);
            }
            Some(deadline) if now >= deadline => {
                self.on = !self.on;
                self.next_toggle = Some(now + self.interval);
            }
            Some(_) => {}
        }
        self.on
    }

    /// The next scheduled toggle instant, if the cursor is currently blinking.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.next_toggle
    }

    /// Whether a scheduled toggle is due at `now` (the loop should rebuild and
    /// redraw so the phase flips).
    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.next_toggle.is_some_and(|deadline| now >= deadline)
    }
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
    /// Most recent pointer position mapped to a terminal cell. `winit` mouse
    /// button events do not carry coordinates, so press/release use this cached
    /// cell from the latest cursor movement.
    pointer_cell: Option<CellPoint>,
    /// Whether the left mouse button is currently extending a selection.
    selecting: bool,
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
        Self {
            options,
            theme,
            visual,
            window: None,
            gpu: None,
            terminal,
            needs_rebuild: true,
            writer,
            pty,
            grid,
            modifiers: Modifiers::default(),
            super_key: false,
            key_bindings,
            settings,
            settings_reloader,
            selection: AbsoluteSelectionState::default(),
            pointer_cell: None,
            selecting: false,
            clicks: ClickTracker::default(),
            last_selection_autoscroll: None,
            report_button: None,
            viewport: Viewport::default(),
            search: SearchUi::default(),
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
    pub(super) fn resize_grid(&mut self, cell: CellSize, width_px: u32, height_px: u32) -> bool {
        let new_grid = grid_dimensions_for(width_px, height_px, cell);
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
        if self.resize_grid(resize.cell, resize.width_px, resize.height_px) {
            self.selection.clear();
            self.selecting = false;
            self.last_selection_autoscroll = None;
            self.report_button = None;
            self.pointer_cell = None;
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

    /// Encode a pressed key and write its bytes to the PTY.
    ///
    /// Maps the `winit` logical key (plus the cached [`Modifiers`]) onto the
    /// neutral [`Key`] model and defers byte production to the shared
    /// [`input::encode_key`]. Keys the prototype does not encode are dropped. The
    /// PTY writer is flushed after each write so the keystroke reaches the shell
    /// without buffering latency.
    fn handle_key_press(&mut self, logical: WinitKey, physical: PhysicalKey) {
        let mods = self.modifiers;
        let key_modes = self.key_modes();
        let action = self.key_bindings.action_for(&logical, mods, self.super_key);
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
            Some(BindableAction::Search) | None => {}
        }

        let mut bytes = Vec::new();
        if let Some(key) = map_keypad_physical_key(physical) {
            bytes = input::encode_key(key, mods, key_modes);
        } else {
            match logical {
                // `Key::Character` may carry more than one char (composed input);
                // encode each so multi-char text still reaches the shell intact.
                WinitKey::Character(text) => {
                    for ch in text.chars() {
                        bytes.extend_from_slice(&input::encode_key(Key::Char(ch), mods, key_modes));
                    }
                }
                WinitKey::Named(named) => {
                    if let Some(key) = map_named_key(named, mods.shift) {
                        bytes = input::encode_key(key, mods, key_modes);
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

    fn key_modes(&self) -> KeyModes {
        self.terminal
            .lock()
            .map(|terminal| key_modes_from_core(terminal.keyboard_modes()))
            .unwrap_or_default()
    }

    fn toggle_search(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        } else {
            self.search_restore_viewport = Some(self.viewport.offset());
            self.search.open();
            self.selection.clear();
            self.selecting = false;
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

    fn current_selection_text(&self) -> Option<String> {
        let Some(range) = self.selection.range() else {
            return None;
        };
        let terminal = self.terminal.lock().expect("terminal mutex");
        let scrollback_len = terminal.screen().scrollback_len();
        let visible_range = selection::visible_range_from_absolute(
            range,
            self.viewport.offset(),
            scrollback_len,
            self.grid,
        )?;
        let snapshot = terminal.snapshot_with_scrollback(self.viewport.offset());
        selected_clipboard_text(&snapshot, visible_range)
    }

    fn write_primary_selection(&mut self) {
        let Some(text) = self.current_selection_text() else {
            return;
        };
        let _ = self.clipboard.write_primary_text(text.as_str());
    }

    fn handle_primary_paste(&mut self) {
        let Some(text) = self.clipboard.read_primary_text() else {
            return;
        };
        self.return_to_live();
        let _ = write_paste_text(&self.terminal, &self.writer, &text);
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

    fn mouse_protocol(&self) -> MouseProtocol {
        self.terminal
            .lock()
            .map(|terminal| terminal.mouse_protocol())
            .unwrap_or_default()
    }

    fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_protocol().is_enabled()
    }

    /// Shift is the local-selection escape hatch while a TUI has enabled mouse
    /// reporting, matching the common xterm-family terminal convention.
    fn should_report_mouse_to_pty(&self) -> bool {
        self.mouse_reporting_enabled() && !self.modifiers.shift
    }

    fn write_pty_bytes(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    fn send_mouse_report(&mut self, button: CoreMouseButton, kind: MouseEventKind) -> bool {
        let protocol = self.mouse_protocol();
        let Some(point) = self.pointer_cell else {
            return false;
        };
        let Some(bytes) = encode_native_mouse_report(protocol, point, button, kind, self.modifiers)
        else {
            return false;
        };

        self.return_to_live();
        self.write_pty_bytes(&bytes);
        true
    }

    fn send_mouse_motion_report(&mut self) {
        let protocol = self.mouse_protocol();
        let Some(button) = motion_report_button(protocol, self.report_button) else {
            return;
        };
        let _ = self.send_mouse_report(button, MouseEventKind::Motion);
    }

    fn send_focus_report(&mut self, focused: bool) {
        let Some(bytes) = self
            .terminal
            .lock()
            .ok()
            .and_then(|terminal| encode_native_focus_report(&terminal, focused))
        else {
            return;
        };

        self.write_pty_bytes(&bytes);
    }

    fn handle_reported_mouse_input(&mut self, state: ElementState, button: CoreMouseButton) {
        match state {
            ElementState::Pressed => {
                self.report_button = Some(button);
                let _ = self.send_mouse_report(button, MouseEventKind::Press);
            }
            ElementState::Released => {
                let _ = self.send_mouse_report(button, MouseEventKind::Release);
                if self.report_button == Some(button) {
                    self.report_button = None;
                }
            }
        }
    }

    fn handle_reported_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let Some(button) = wheel_report_button(delta) else {
            return false;
        };
        self.send_mouse_report(button, MouseEventKind::Press)
    }

    fn update_pointer_cell(&mut self, x_px: f64, y_px: f64) {
        let Some(cell) = self.gpu.as_ref().map(GpuState::cell) else {
            return;
        };
        let point = selection::cell_at_physical(x_px, y_px, cell, self.grid);
        self.pointer_cell = Some(point);
        if self.selecting {
            self.autoscroll_selection_if_needed(y_px, cell);
            let scrollback_len = self.scrollback_len();
            self.selection.update(selection::visible_to_absolute(
                point,
                self.viewport.offset(),
                scrollback_len,
            ));
            self.request_selection_redraw();
        } else if self.should_report_mouse_to_pty() || self.report_button.is_some() {
            self.send_mouse_motion_report();
        }
    }

    fn begin_selection(&mut self) {
        let Some(point) = self.pointer_cell else {
            return;
        };
        match self.clicks.register_click(point, Instant::now()) {
            1 => self.begin_drag_selection(point),
            2 => self.select_word(point),
            _ => self.select_line(point),
        }
    }

    fn begin_drag_selection(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        self.selection.begin(selection::visible_to_absolute(
            point,
            self.viewport.offset(),
            scrollback_len,
        ));
        self.selecting = true;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    fn select_word(&mut self, point: CellPoint) {
        let (snapshot, scrollback_len) = self.selection_snapshot();
        let Some(range) = selection::word_range_at(&snapshot, point) else {
            self.selection.clear();
            self.selecting = false;
            self.request_selection_redraw();
            return;
        };

        self.selection
            .set_range(selection::absolute_range_from_visible(
                range,
                self.viewport.offset(),
                scrollback_len,
            ));
        self.selecting = false;
        self.request_selection_redraw();
    }

    fn select_line(&mut self, point: CellPoint) {
        let scrollback_len = self.scrollback_len();
        let Some(range) = selection::line_range_at(point, self.grid) else {
            return;
        };

        self.selection
            .set_range(selection::absolute_range_from_visible(
                range,
                self.viewport.offset(),
                scrollback_len,
            ));
        self.selecting = false;
        self.request_selection_redraw();
    }

    fn request_selection_redraw(&mut self) {
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn selection_snapshot(&self) -> (Snapshot, usize) {
        let terminal = self.terminal.lock().expect("terminal mutex");
        (
            terminal.snapshot_with_scrollback(self.viewport.offset()),
            terminal.screen().scrollback_len(),
        )
    }

    fn autoscroll_selection_if_needed(&mut self, y_px: f64, cell: CellSize) {
        let delta = selection::drag_autoscroll_delta(y_px, cell, self.grid);
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

    fn finish_selection(&mut self) {
        if !self.selecting {
            return;
        }
        self.write_primary_selection();
        self.selecting = false;
        self.last_selection_autoscroll = None;
        self.request_selection_redraw();
    }

    /// Number of rows a Shift+PageUp/PageDown press scrolls: one screenful less
    /// one row of overlap for continuity (at least one row).
    fn page_lines(&self) -> usize {
        self.grid.rows.saturating_sub(1).max(1)
    }

    /// Current scrollback length from the shared model (0 if the lock is
    /// poisoned), used to clamp upward scrolling.
    fn scrollback_len(&self) -> usize {
        self.terminal
            .lock()
            .map(|t| t.screen().scrollback_len())
            .unwrap_or(0)
    }

    /// Adjust the scrollback viewport. `delta > 0` pages up into history,
    /// `delta < 0` pages toward the live bottom. Selections are stored against
    /// absolute rows, so moving the viewport keeps their anchors meaningful.
    /// With no scrollback this is a clamped no-op (never panics).
    fn scroll_viewport(&mut self, delta: isize) {
        let changed = match delta.cmp(&0) {
            std::cmp::Ordering::Greater => self
                .viewport
                .scroll_up(delta as usize, self.scrollback_len()),
            std::cmp::Ordering::Less => self.viewport.scroll_down((-delta) as usize),
            std::cmp::Ordering::Equal => false,
        };
        if changed {
            self.on_viewport_changed();
        }
    }

    /// Return the viewport to the live bottom (offset 0). Called whenever input
    /// is written to the PTY so typing always jumps back to the prompt.
    fn return_to_live(&mut self) {
        if self.viewport.reset_to_live() {
            self.on_viewport_changed();
        }
    }

    /// Shared side effects of a viewport offset change: keep absolute
    /// selections intact and request one rebuild/redraw so their visible
    /// intersection is recomputed.
    fn on_viewport_changed(&mut self) {
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
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
        let mut next_settings = self.settings.clone();
        if !apply_reloadable_values(&mut next_settings, reloaded) {
            return;
        }

        let next_options = self.options_for_settings(&next_settings);
        let text_rebuilt = match self.gpu.as_mut() {
            Some(gpu) => match gpu.apply_text_options(&next_options) {
                Ok(changed) => changed,
                Err(err) => {
                    eprintln!("odytty: config reload ignored: {err}");
                    return;
                }
            },
            None => false,
        };

        self.settings = next_settings;
        self.options = next_options;
        self.theme = self.settings.theme;
        self.visual = self.settings.visual;
        self.key_bindings = KeyBindings::from_overrides(&self.settings.key_bindings);
        text::set_default_colors(self.theme.foreground, self.theme.background);
        if let Ok(mut terminal) = self.terminal.lock() {
            terminal.set_cursor_defaults(
                self.settings.cursor_style,
                self.settings.cursor_blink.enabled(),
            );
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_theme(self.theme);
            gpu.set_visual(self.visual);
            gpu.set_text_gamma(self.settings.text_gamma);
        }

        if text_rebuilt {
            let resize = self.gpu.as_ref().and_then(|gpu| {
                let cell = gpu.cell();
                if let Ok(mut terminal) = self.terminal.lock() {
                    terminal.set_cell_metrics(cell.width, cell.height);
                }
                self.window
                    .as_ref()
                    .map(|window| pending_resize_for_surface(cell, window.inner_size()))
            });
            if let Some(resize) = resize {
                self.apply_grid_resize(resize);
            }
        }

        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn key_modes_from_core(modes: CoreKeyboardModes) -> KeyModes {
    KeyModes {
        application_cursor: modes.application_cursor,
        application_keypad: modes.application_keypad,
    }
}

fn image_uploads_for_visible(
    terminal: &Terminal,
    visible: &[VisiblePlacement],
    cached: &BTreeSet<StoredImageId>,
) -> Vec<ImageUpload> {
    let mut requested = BTreeSet::new();
    visible
        .iter()
        .filter(|placement| {
            !cached.contains(&placement.image_id) && requested.insert(placement.image_id)
        })
        .filter_map(|placement| {
            terminal
                .graphics()
                .store()
                .get(placement.image_id)
                .map(ImageUpload::from)
        })
        .collect()
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
        ) {
            Ok(gpu) => {
                // Push live cell pixel metrics to the terminal core so graphics
                // placements (sixel/kitty) compute the correct cell extent.
                let cell = gpu.cell();
                if let Ok(mut term) = self.terminal.lock() {
                    term.set_cell_metrics(cell.width, cell.height);
                }
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
                    pending_resize_for_surface(gpu.cell(), size)
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
                    Some(pending_resize_for_surface(gpu.cell(), size))
                });

                if let Some(resize) = resize {
                    self.needs_rebuild = true;
                    self.record_pending_resize(resize, Instant::now());
                }
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                self.update_control_flow_deadline(event_loop);
            }
            WindowEvent::RedrawRequested => {
                self.update_window_title();
                // Rebuild geometry at most once per redraw, no matter how many
                // pump wakes coalesced into this frame. Snapshot under the lock,
                // then drop it before touching the GPU.
                if self.needs_rebuild {
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
                            visible_graphics,
                            image_uploads,
                        )
                    };
                    // Blink phase: hide the cursor during the off-phase. Only the
                    // live view (offset 0) shows a cursor; the blink driver holds
                    // it solid when not blinking or unfocused.
                    let now = Instant::now();
                    let cursor_on = self.cursor_blink.poll(now, cursor_blinking, self.focused);
                    if !cursor_on {
                        snapshot.cursor_visible = false;
                    }
                    if let Some(range) = self.selection.range()
                        && let Some(visible_range) = selection::visible_range_from_absolute(
                            range,
                            self.viewport.offset(),
                            scrollback_len,
                            self.grid,
                        )
                    {
                        selection::apply_highlight(&mut snapshot, visible_range);
                    }
                    apply_search_ui(
                        &mut snapshot,
                        &self.search,
                        self.viewport.offset(),
                        scrollback_len,
                        self.grid,
                    );
                    let color = self.scroll_indicator_color();
                    let overlay = self.gpu.as_ref().and_then(|gpu| {
                        scroll_indicator_quad(
                            self.viewport.offset(),
                            scrollback_len,
                            self.grid,
                            gpu.cell(),
                            color,
                        )
                    });
                    if let Some(gpu) = self.gpu.as_mut() {
                        gpu.update_image_layer(&visible_graphics, &image_uploads);
                        if let Some(overlay) = overlay {
                            gpu.update_from_snapshot_with_overlays(
                                &snapshot,
                                cursor_style,
                                std::slice::from_ref(&overlay),
                            );
                        } else {
                            gpu.update_from_snapshot(&snapshot, cursor_style);
                        }
                    }
                    self.needs_rebuild = false;
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
                // Force the cursor solid-on immediately on focus loss (and
                // resume blinking on focus gain) by rebuilding next frame.
                self.needs_rebuild = true;
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
                self.send_focus_report(focused);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.update_pointer_cell(position.x, position.y);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if self.selecting {
                    if button == WinitMouseButton::Left && state == ElementState::Released {
                        self.finish_selection();
                    }
                    return;
                }

                if (self.should_report_mouse_to_pty() || self.report_button.is_some())
                    && let Some(button) = map_winit_mouse_button(button)
                {
                    self.handle_reported_mouse_input(state, button);
                    return;
                }

                if button == WinitMouseButton::Left {
                    match state {
                        ElementState::Pressed => self.begin_selection(),
                        ElementState::Released => self.finish_selection(),
                    }
                } else if button == WinitMouseButton::Middle {
                    if state == ElementState::Pressed {
                        self.handle_primary_paste();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.should_report_mouse_to_pty() {
                    let _ = self.handle_reported_wheel(delta);
                    return;
                }

                let cell_height = self.gpu.as_ref().map_or(0, |gpu| gpu.cell().height);
                let lines = wheel_lines(delta, cell_height);
                if lines != 0 {
                    self.scroll_viewport(lines);
                }
            }
            // Only act on key-down (ignore key-up). Repeats are kept: holding a
            // key should autorepeat into the shell like a real terminal.
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.handle_key_press(event.logical_key, event.physical_key);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn blink() -> CursorBlinkState {
        CursorBlinkState::new(Duration::from_millis(500))
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

// SPDX-License-Identifier: GPL-3.0-only
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::process::{Command, Stdio};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::core::Terminal;
use crate::core::{
    ClipboardRequest, Color, Dimensions, LinkId, MouseButton as CoreMouseButton, MouseEncoding,
    MouseEventKind, MouseModifiers, MouseProtocol, Position, RgbColor, Snapshot,
    encode_mouse_event_pixel,
};
use crate::grid::{CursorRenderParams, SolidQuad};
use crate::input::{self, Key, KeyEventType, KeyModes, Modifiers};
use crate::pty::ForegroundJob;
#[cfg(test)]
use crate::pty::PtySession;
use crate::selection::{
    self, AbsoluteSelectionRange, CellPoint, PointerDrag, SelectGranularity, SelectionStyle,
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
use super::cvd_theme::CvdThemeCache;
use super::gpu::{BloomOptions, CrtOptions, FrameOutcome, GpuState};
use super::options::{NativeError, NativeOptions};
use super::overlay::{
    OverlayOutcome, OverlayPointer, OverlayUi, PointerButton, apply_overlay,
    overlay_input_from_winit, overlay_rect,
};
#[cfg(test)]
use super::pty::PtyWriter;
use super::pty::UserEvent;
use super::render_helpers::{
    CursorAnimKey, CursorRenderSignature, GeometryUpdate, OverlayCompositeSignature,
    OverlayFragment, RenderContentSignature, RenderSignature, SelectionSignature,
    apply_hyperlink_hover, hyperlink_action_allowed, image_uploads_for_visible,
    key_modes_from_core, openable_hyperlink_uri, visible_graphics_signature,
};
use super::theme_builder::{save_theme_to_dir, user_theme_dir_for_config};

pub(super) use super::cursor::{CURSOR_BLINK_INTERVAL, CursorBlinkState};
pub(super) use super::resize::{
    PendingResize, RESIZE_DEBOUNCE_INTERVAL, ResizeDebouncer, pending_resize_for_surface,
    scale_factor_changed,
};
use super::search_ui::{SearchStyle, apply_search_ui};
use super::session::{Session, SessionSet, SessionToken};
use super::viewport::{
    SELECTION_AUTOSCROLL_INTERVAL, WheelAccumulator, WindowPadding,
    grid_dimensions_for_with_padding, scroll_indicator_hit_with_padding,
    scroll_indicator_quad_with_padding, scrollbar_offset_for_drag_with_padding, wheel_lines,
    wheel_lines_scaled, wheel_zoom_steps,
};

mod background_ui;
mod copy_mode_ui;
mod cursor;
mod cursor_frame;
mod cursor_trail;
mod gutter_ui;
mod hints_ui;
mod interaction;
mod new_row_fade;
mod os_theme;
mod overlay_registry;
mod pointer;
mod prompt_jump;
mod scroll_anim;
mod tab_bar;
#[cfg(test)]
mod test_seams;
mod theme_roles;
mod window_border;

pub(in crate::native) use self::hints_ui::HintsUi;
pub(in crate::native) use self::scroll_anim::ScrollAnimState as SessionScrollAnimState;
pub(in crate::native) use self::tab_bar::TabBarSource;
use self::tab_bar::{TAB_BAR_ROWS, TabBar, TabHit};
pub(in crate::native) use overlay_registry::ActiveModal;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenameState {
    target: SessionToken,
    text: String,
    cursor: usize,
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
    /// Active *authored* presentation theme (from `ODYTTY_THEME`, updated on
    /// settings changes). The theme as written — what round-trips/authoring
    /// read; the colors published to the renderer are [`Self::effective_theme`].
    theme: Theme,
    /// Theme actually published to the renderer (U4): [`Self::theme`] after
    /// colour-vision-deficiency adaptation. Equal to `theme` when `cvd_mode` is
    /// off (byte-identical plain path). Recomputed only in `apply_settings` via
    /// [`Self::cvd_cache`], never per frame.
    effective_theme: Theme,
    /// One-entry cache for [`Self::effective_theme`] keyed on
    /// `(authored theme, cvd_mode, cvd_strength)` so repeated applies skip the
    /// palette re-floor.
    cvd_cache: CvdThemeCache,
    /// Active optional visual treatment (selected once from `ODYTTY_VISUAL`,
    /// default off). Drives the ambient scanline uniform; presentation-only and
    /// fully disableable. The core never sees it.
    visual: VisualEffect,
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    sessions: SessionSet,
    /// Native presentation epoch for pixel-affecting state outside the terminal
    /// core revision: theme/default-color changes, atlas/font changes, and
    /// other settings that make identical snapshots build different vertices.
    presentation_epoch: u64,
    /// SH2 status-gutter invalidation epoch. Bumped when the core reports prompt
    /// marks changed while the status gutter is enabled, so a pure OSC 133
    /// status transition (which need not move the terminal render revision)
    /// still forces a non-retained redraw and the gutter repaints. Stays at its
    /// initial value while the gutter is off — the default path is unaffected.
    prompt_marks_epoch: u64,
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
    /// Latest settings produced by a high-frequency overlay interaction
    /// (slider drag / key repeat). Coalesced so expensive live applies such as
    /// font-size atlas rebuilds happen at most once per frame/event burst.
    pending_overlay_settings: Option<Settings>,
    /// ID1: when set, the authored theme `cursor`/`selection`/`search` roles
    /// drive cursor color and selection/search highlight fills (with
    /// RV1-floored foregrounds) instead of the historical inverse / hardcoded
    /// treatments. Default-on by operator decision; `themed_ui_roles = off`
    /// restores the legacy rendering path.
    themed_ui_roles: bool,
    /// Native in-window overlay state. It is presentation-only: widgets
    /// composite into snapshot copies and never mutate terminal state or PTY.
    overlay: OverlayUi,
    /// Native-side clipboard owner. Kept alive across copy/paste operations so
    /// Linux clipboard contents remain served after Ctrl+Shift+C.
    clipboard: NativeClipboard,
    resize_debounce: ResizeDebouncer,
    /// Whether the window currently holds focus. Blink pauses (cursor solid)
    /// while unfocused, matching common terminal behavior.
    focused: bool,
    autoclose: Option<Duration>,
    deadline: Option<Instant>,
    /// OS-THEME: last known OS dark/light appearance preference, or `None` until
    /// the compositor surfaces one (always `None` on X11, where the signal is
    /// absent). Read only while [`Settings::follow_os_theme`] is on; off the
    /// default path it is never consulted.
    os_theme: Option<winit::window::Theme>,
    /// CLOSE-CONFIRM: set when the confirmation dialog is accepted (or the
    /// non-confirming close path decides to exit) so `window_event` can exit the
    /// loop after the overlay outcome is applied — `apply_overlay_outcome` only
    /// has `&mut self` and cannot reach the `ActiveEventLoop` itself.
    pending_exit: bool,
    /// WHEEL-SENS: coalesces high-resolution wheel bursts (sub-notch
    /// `PixelDelta` events, fractional `LineDelta`) into discrete notches so one
    /// physical detent is one scroll/zoom step. Identity for a clean
    /// `LineDelta(_, ±1.0)`. Reset on focus loss and overlay open.
    wheel_accum: WheelAccumulator,
    /// Visible multi-session tab strip state. Presentation-only; the session
    /// model stays in `SessionSet`.
    tab_bar: TabBar,
    rename_state: Option<RenameState>,
    /// SLIDER-GUARD: whether the left mouse button is currently held while the
    /// overlay is open. Set on `MouseInput { Pressed, Left }` and cleared on
    /// `MouseInput { Released, Left }` through the overlay pointer path. Used to
    /// gate overlay slider drag moves so that cursor movements after the button
    /// is released can NEVER advance an armed drag — even if the drag state is
    /// somehow stale. `CursorMoved` carries no button state, so this flag is the
    /// reliable held-button seam for the settings-slider path (D-SLIDER-GUARD).
    overlay_left_held: bool,
    pub(super) startup_error: Option<NativeError>,
}

impl Deref for App {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.sessions.active()
    }
}

impl DerefMut for App {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.sessions.active_mut()
    }
}

impl App {
    #[cfg(test)]
    pub(super) fn new(
        options: NativeOptions,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        pty: Arc<Mutex<PtySession>>,
        settings: Settings,
        settings_reloader: SettingsReloader,
    ) -> Self {
        let session = Session::new(
            crate::native::session::SessionToken(0),
            terminal,
            writer,
            pty,
            None,
        );
        Self::new_with_sessions(
            options,
            SessionSet::new(session, None),
            settings,
            settings_reloader,
        )
    }

    pub(super) fn new_with_sessions(
        options: NativeOptions,
        sessions: SessionSet,
        settings: Settings,
        settings_reloader: SettingsReloader,
    ) -> Self {
        let grid = options.initial_grid;
        let theme = settings.theme;
        // U4: warm the effective-theme cache so the first GPU bring-up publishes
        // the adapted theme. Off (the default) returns the authored theme.
        let mut cvd_cache = CvdThemeCache::default();
        let effective_theme = cvd_cache.resolve(&theme, settings.cvd_mode, settings.cvd_strength);
        let visual = settings.visual;
        let key_bindings = KeyBindings::from_overrides(&settings.key_bindings);
        let autoclose = settings.native_autoclose;
        let themed_ui_roles = settings.themed_ui_roles;
        let overlay = OverlayUi::new(&settings);
        // ONBOARD: decide before construction whether this is a first launch.
        let onboarding_override = std::env::var_os("ODYTTY_ONBOARDING").is_some();
        let mut app = Self {
            options,
            theme,
            effective_theme,
            cvd_cache,
            visual,
            window: None,
            gpu: None,
            sessions,
            presentation_epoch: 0,
            prompt_marks_epoch: 0,
            grid,
            modifiers: Modifiers::default(),
            super_key: false,
            key_bindings,
            settings,
            settings_reloader,
            pending_overlay_settings: None,
            themed_ui_roles,
            overlay,
            clipboard: NativeClipboard::default(),
            resize_debounce: ResizeDebouncer::new(RESIZE_DEBOUNCE_INTERVAL),
            // Assume focused at startup; the first `Focused` event corrects it.
            focused: true,
            autoclose,
            deadline: None,
            os_theme: None,
            pending_exit: false,
            wheel_accum: WheelAccumulator::default(),
            tab_bar: TabBar::default(),
            rename_state: None,
            overlay_left_held: false,
            startup_error: None,
        };
        // ONBOARD (D-OB-1/D-OB-2): open the first-run welcome card iff the
        // config file does not yet exist (or the env override is set). First-run
        // memory is the user-owned config's existence — no telemetry, no flag
        // file (U6). Materializing the config (saving any setting) retires it.
        if should_show_onboarding(onboarding_override, app.settings_reloader.config_path()) {
            app.overlay.open_onboarding();
        }
        app
    }

    pub(super) fn resize_grid_with_padding(
        &mut self,
        cell: CellSize,
        padding: WindowPadding,
        width_px: u32,
        height_px: u32,
    ) -> bool {
        let mut new_grid = grid_dimensions_for_with_padding(width_px, height_px, cell, padding);
        if self.should_show_tab_bar() {
            new_grid.rows = new_grid.rows.saturating_sub(TAB_BAR_ROWS as usize).max(1);
        }
        if new_grid == self.grid {
            return false;
        }
        self.grid = new_grid;

        for session in self.sessions.iter() {
            if let Ok(mut terminal) = session.terminal.lock() {
                terminal.resize(new_grid.columns, new_grid.rows);
                terminal.set_cell_metrics(cell.width, cell.height);
            }
            if let Ok(pty) = session.pty.lock() {
                let _ = pty.resize(new_grid);
            }
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
            self.selection_block = false;
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
            // HINTS label spans are absolute rows against the old layout; a
            // reflow makes them stale, so close the modal (trap #4).
            self.hints = None;
            self.needs_rebuild = true;
        }
    }

    fn record_pending_resize(&mut self, resize: PendingResize, now: Instant) {
        if let Some(due) = self.resize_debounce.record(resize, now) {
            self.apply_grid_resize(due);
        }
    }

    fn initialize_session_with(
        session: &mut Session,
        effective_theme: Theme,
        themed_ui_roles: bool,
        osc52_read: bool,
        cursor_style: crate::core::CursorStyle,
        cursor_blink: crate::settings::CursorBlink,
        cell: Option<CellSize>,
    ) {
        if let Ok(mut terminal) = session.terminal.lock() {
            let cursor_default = if themed_ui_roles {
                rgb(effective_theme.cursor)
            } else {
                rgb(effective_theme.foreground)
            };
            terminal.set_base_colors(
                rgb(effective_theme.foreground),
                rgb(effective_theme.background),
                cursor_default,
            );
            terminal.set_osc52_read_enabled(osc52_read);
            terminal.set_cursor_defaults(cursor_style, cursor_blink.enabled());
            if let Some(cell) = cell {
                terminal.set_cell_metrics(cell.width, cell.height);
            }
        }
    }

    fn handle_new_tab(&mut self) {
        if let Ok(session_id) = self.sessions.spawn(self.grid) {
            let effective_theme = self.effective_theme;
            let themed_ui_roles = self.themed_ui_roles;
            let osc52_read = self.settings.osc52_read;
            let cursor_style = self.settings.cursor_style;
            let cursor_blink = self.settings.cursor_blink;
            let cell = self.gpu.as_ref().map(GpuState::cell);
            if let Some(session) = self.sessions.get_mut(session_id) {
                Self::initialize_session_with(
                    session,
                    effective_theme,
                    themed_ui_roles,
                    osc52_read,
                    cursor_style,
                    cursor_blink,
                    cell,
                );
            }
            let _ = self.sessions.switch(session_id);
            self.on_active_session_changed();
        }
    }

    fn switch_to_next_tab(&mut self) {
        if self.sessions.next() {
            self.on_active_session_changed();
        }
    }

    fn switch_to_prev_tab(&mut self) {
        if self.sessions.prev() {
            self.on_active_session_changed();
        }
    }

    fn close_active_tab(&mut self) -> bool {
        if self.sessions.iter().count() <= 1 {
            self.pending_exit = true;
            return true;
        }
        let is_last = self.sessions.close(self.sessions.active_id());
        if !is_last {
            self.on_active_session_changed();
        } else {
            self.pending_exit = true;
        }
        is_last
    }

    pub(super) fn close_all_sessions(&mut self) {
        while !self.sessions.is_empty() {
            let Some(token) = self.sessions.token_at_position(0) else {
                break;
            };
            let _ = self.sessions.close(token);
        }
    }

    fn update_control_flow_deadline(&self, event_loop: &ActiveEventLoop) {
        let next = [
            self.deadline,
            self.resize_debounce.deadline(),
            self.sessions
                .iter()
                .filter_map(|session| session.cursor_blink.deadline())
                .min(),
            self.settings_reloader.deadline(),
            self.sessions
                .iter()
                .filter_map(|session| session.synchronized_output_hold.deadline())
                .min(),
            // Wave-15b: aggregated cursor-animation wake source. `None` at rest
            // (both contributor stubs return `None`), so the at-rest min is
            // unchanged and no spurious wakeup is scheduled.
            self.sessions
                .iter()
                .filter_map(|session| {
                    let mut next = session.cursor_ease_deadline;
                    if let Some(deadline) = session.cursor_slide_deadline {
                        next = Some(next.map_or(deadline, |current| current.min(deadline)));
                    }
                    next
                })
                .min(),
        ]
        .into_iter()
        .flatten()
        .min();
        match next {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
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
                self.handle_overlay_key(&logical, event_type);
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
            // Modal-input gate: a new keyboard modal captures keys beneath the
            // overlay/search guards, above the BindableAction match (precedence
            // D-INFRA-4). Always None today ⇒ falls through unchanged.
            match self.active_modal() {
                ActiveModal::None => {}
                modal => {
                    self.route_modal_key(modal, &logical);
                    return;
                }
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
                // The next four route to thin per-feature handlers that live in
                // sibling `app` modules so future feature work fills them in
                // disjoint files. Each returns whether it consumed the key; a
                // handler that does not act yet returns `false`, so the chord
                // falls through to the PTY encode path below exactly as an
                // unbound key would (the plain path stays byte-identical).
                Some(BindableAction::JumpPromptPrev) => {
                    if self.jump_prompt_prev() {
                        return;
                    }
                }
                Some(BindableAction::JumpPromptNext) => {
                    if self.jump_prompt_next() {
                        return;
                    }
                }
                Some(BindableAction::CopyMode) => {
                    if self.enter_copy_mode() {
                        return;
                    }
                }
                Some(BindableAction::Hints) => {
                    if self.activate_hints() {
                        return;
                    }
                }
                Some(BindableAction::ClearInput) => {
                    // IN1: clear the current shell input line. Sends a
                    // readline-style "move to start, kill to end" sequence
                    // (Ctrl+A, Ctrl+K) so the whole line is cleared regardless
                    // of cursor position. Returns the viewport to live like any
                    // keystroke that reaches the shell, then consumes the chord.
                    self.return_to_live();
                    self.write_pty_bytes(&[0x01, 0x0b]);
                    return;
                }
                Some(BindableAction::NewTab) => {
                    self.handle_new_tab();
                    return;
                }
                Some(BindableAction::NextTab) => {
                    self.switch_to_next_tab();
                    return;
                }
                Some(BindableAction::PrevTab) => {
                    self.switch_to_prev_tab();
                    return;
                }
                Some(BindableAction::CloseTab) => {
                    if self.close_active_tab() {
                        return;
                    }
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

    fn open_key_bindings_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_key_bindings(&self.settings);
        self.request_selection_redraw();
    }

    fn open_font_picker_overlay(&mut self) {
        if self.search.is_open() {
            self.close_search(true);
        }
        self.reset_pointer_state_for_overlay();
        self.overlay.open_font_picker(&self.settings);
        self.request_selection_redraw();
    }

    fn handle_overlay_key(&mut self, logical: &WinitKey, event_type: KeyEventType) {
        // KB-REMAP chord capture (R2 KILL-SHOT): when the key-remap modal is
        // armed to capture a chord, this MUST be the first thing we do — route
        // the raw key through `chord_from_winit` BEFORE the lossy
        // `overlay_input_from_winit` mapper, which would otherwise collapse a
        // chord like Ctrl+Shift+K into an `OverlayInput` (or, for Enter/Esc, an
        // Activate/Close) and the modifiers would be lost. `is_capturing_chord`
        // is `false` whenever the modal is closed or merely browsing, so this
        // never disturbs normal overlay navigation (R1).
        if self.overlay.is_capturing_chord() {
            let chord = super::bindings::chord_from_winit(logical, self.modifiers, self.super_key);
            let outcome = self.overlay.deliver_chord(chord);
            self.apply_overlay_outcome(outcome);
            self.request_selection_redraw();
            return;
        }

        let Some(input) = overlay_input_from_winit(logical, self.modifiers) else {
            self.request_selection_redraw();
            return;
        };

        let outcome = self.overlay.handle_input(input);
        self.apply_overlay_outcome_with_policy(outcome, event_type == KeyEventType::Repeat);
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
            self.selection_block = false;
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
        let session = self.sessions.active_mut();
        if let Ok(terminal) = session.terminal.lock() {
            session.search.refresh(&terminal);
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

    /// Open the right-click context menu (IN2) at the cached pointer cell, with
    /// Copy enabled iff a selection exists and Paste enabled iff the clipboard
    /// holds text — the per-item gating snapshot the menu renders. Deliberately
    /// does NOT call `reset_pointer_state_for_overlay`: that would clear the
    /// selection the Copy item needs. No pointer cell (e.g. before the first
    /// move) means no menu.
    pub(super) fn open_context_menu(&mut self, rename_target: Option<SessionToken>) {
        let Some(spawn) = self.pointer_cell else {
            return;
        };
        let copy_enabled = self.selection.range().is_some();
        let editable_selection = self.editable_input_selection_for_context_menu();
        let paste_enabled = self.clipboard.read_text().is_some();
        self.overlay.open_context_menu(
            spawn,
            copy_enabled,
            editable_selection.is_some(),
            paste_enabled,
            editable_selection.is_some(),
            rename_target,
        );
        self.request_selection_redraw();
    }

    /// Select the entire buffer — the full scrollback plus the visible grid
    /// (IN2 Select All). The range is stored in absolute row space, so it stays
    /// meaningful as the viewport scrolls; the copy path resolves whatever is
    /// visible at copy time (the app-wide selection→clipboard contract). Also
    /// mirrors the selection to PRIMARY like any other selection. No-op on an
    /// empty grid.
    fn handle_select_all(&mut self) {
        let columns = self.grid.columns;
        let rows = self.grid.rows;
        if columns == 0 || rows == 0 {
            return;
        }
        let end_row = self.scrollback_len() + rows - 1;
        self.selection.set_range(AbsoluteSelectionRange {
            start: selection::AbsoluteCellPoint { row: 0, column: 0 },
            end: selection::AbsoluteCellPoint {
                row: end_row,
                column: columns - 1,
            },
        });
        self.selection_block = false;
        self.write_primary_selection();
        self.request_selection_redraw();
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

    fn active_window_title(&self) -> String {
        self.terminal
            .lock()
            .ok()
            .and_then(|terminal| terminal.title().map(ToOwned::to_owned))
            .unwrap_or_else(|| self.options.title.clone())
    }

    fn sync_active_window_title(&self) {
        if let Some(window) = self.window.as_ref() {
            window.set_title(&self.active_window_title());
        }
    }

    fn on_active_session_changed(&mut self) {
        self.recompute_grid_for_tab_bar();
        self.tab_bar.set_hover(None);
        self.last_render_signature = None;
        self.needs_rebuild = true;
        self.sync_active_window_title();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn should_show_tab_bar(&self) -> bool {
        self.sessions.tab_count() >= 2
    }

    fn tab_bar_height_px(&self, cell: CellSize) -> f32 {
        if self.should_show_tab_bar() {
            cell.height as f32 * TAB_BAR_ROWS as f32
        } else {
            0.0
        }
    }

    fn recompute_grid_for_tab_bar(&mut self) {
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let _ = self.resize_grid_with_padding(
            gpu.cell(),
            gpu.window_padding(),
            window.inner_size().width,
            window.inner_size().height,
        );
    }

    fn shift_overlays_for_tab_bar(&self, overlays: &mut [SolidQuad], offset_px: f32) {
        if offset_px <= 0.0 {
            return;
        }
        for overlay in overlays {
            overlay.rect[1] += offset_px;
            overlay.rect[3] += offset_px;
        }
    }

    fn decorate_snapshot_with_tab_bar(
        &self,
        snapshot: &Snapshot,
        cursor_visible: bool,
        cell: CellSize,
    ) -> (Snapshot, Vec<SolidQuad>) {
        if !self.should_show_tab_bar() {
            return (snapshot.clone(), Vec::new());
        }
        let columns = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows + TAB_BAR_ROWS as usize;
        let mut decorated = Snapshot {
            dimensions: Dimensions::new(columns, rows),
            cursor: Position {
                row: snapshot.cursor.row + TAB_BAR_ROWS as usize,
                column: snapshot.cursor.column,
            },
            cursor_visible,
            colors: snapshot.colors.clone(),
            cells: vec![crate::core::Cell::default(); columns * rows],
        };
        let top = columns * TAB_BAR_ROWS as usize;
        decorated.cells[top..top + snapshot.cells.len()].clone_from_slice(&snapshot.cells);

        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let output = self.tab_bar.render(
            &self.sessions,
            columns,
            padding.as_f32(),
            cell,
            padding,
            self.effective_theme.foreground,
            self.effective_theme.background,
            self.effective_theme.selection,
        );
        for glyph in output.glyphs {
            if glyph.col < columns {
                decorated.cells[glyph.col] = crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        (decorated, output.quads)
    }

    fn tab_bar_row_offset(&self) -> usize {
        if self.should_show_tab_bar() {
            TAB_BAR_ROWS as usize
        } else {
            0
        }
    }

    fn apply_user_event(&mut self, event: UserEvent) -> bool {
        match event {
            UserEvent::Redraw { session } => {
                if let Some(target) = self.sessions.get_mut(session) {
                    target.needs_rebuild = true;
                    target.refresh_tab_title();
                }
                if self.sessions.active_id() == session
                    && let Some(window) = self.window.as_ref()
                {
                    window.request_redraw();
                }
                false
            }
            UserEvent::ShellExited { session } => {
                if self.sessions.position_of_token(session).is_some()
                    && self.sessions.iter().count() <= 1
                {
                    self.pending_exit = true;
                    return true;
                }
                let is_last = self.sessions.close_shell_exited(session);
                if is_last {
                    self.pending_exit = true;
                    true
                } else {
                    self.on_active_session_changed();
                    false
                }
            }
        }
    }

    fn options_for_settings(&self, settings: &Settings) -> NativeOptions {
        let parsed = NativeOptions::from_settings(settings);
        NativeOptions {
            title: self.options.title.clone(),
            initial_grid: self.options.initial_grid,
            font_family: parsed.font_family,
            font_weight: parsed.font_weight,
            font_path: parsed.font_path,
            font_size_px: parsed.font_size_px,
            text_gamma: parsed.text_gamma,
            subpixel: parsed.subpixel,
            window_padding_px: parsed.window_padding_px,
            line_height: parsed.line_height,
            box_thickness: parsed.box_thickness,
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

    fn queue_overlay_settings(&mut self, settings: Settings) {
        self.pending_overlay_settings = Some(settings);
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn flush_pending_overlay_settings(&mut self) {
        if let Some(settings) = self.pending_overlay_settings.take() {
            self.apply_overlay_settings(settings);
        }
    }

    fn save_overlay_settings(&mut self, changes: &[crate::settings::SettingEdit]) {
        self.flush_pending_overlay_settings();
        let Some(path) = self.settings_reloader.config_path() else {
            self.overlay
                .save_failed("could not resolve odytty.conf path".to_owned());
            return;
        };
        match write_settings_changes_to_path(path, changes) {
            Ok(result) => {
                self.overlay.save_succeeded(result.changed);
                // BUG 2 (FONT-SAVE-CORRECTNESS): a Save must also apply LIVE, not
                // only at restart. Re-read the just-written config as startup does
                // (`Settings::from_env`, same path + env) and route it through the
                // shared reload seam — no duplicated reload logic. Idempotent: a
                // live-previewed value and the later background poll both no-op.
                if result.changed > 0 {
                    let reloaded = Settings::from_env();
                    self.apply_overlay_settings(reloaded);
                }
            }
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
        // WIN-DECOR: apply a live decorations change immediately so the panel
        // toggle takes effect without a restart. `set_decorations` is
        // idempotent (calling it with the current value is a no-op), so this is
        // safe to call unconditionally on every reload. The window always
        // exists before a settings reload can fire.
        if let Some(window) = self.window.as_ref() {
            window.set_decorations(self.settings.window_decorations);
        }
        // OS-THEME: the active theme is the authored `settings.theme` unless an
        // OS dark/light override is active, in which case it wins — so a config
        // reload (which may change the authored theme or the dark/light pair)
        // re-derives the correct active theme rather than clobbering a live OS
        // override back to the authored theme. With `follow_os_theme` off this
        // returns exactly `self.settings.theme`, byte-identical to before.
        self.theme = self.resolve_active_theme();
        // U4: compute the effective (CVD-adapted) theme once at this change
        // chokepoint and publish IT to every renderer seam below. Off returns
        // the authored theme unchanged (byte-identical plain path); the cache
        // makes an unchanged theme/mode/strength a cheap clone. A later step can
        // route the theme builder's live preview around this compute (via
        // `cvd_theme::effective_theme`) so authoring stays WYSIWYG; that bypass
        // is not wired yet, so a preview is adapted like any other application
        // while a CVD mode is active (off by default).
        self.effective_theme = self.cvd_cache.resolve(
            &self.theme,
            self.settings.cvd_mode,
            self.settings.cvd_strength,
        );
        self.visual = self.settings.visual;
        self.themed_ui_roles = self.settings.themed_ui_roles;
        self.key_bindings = KeyBindings::from_overrides(&self.settings.key_bindings);
        match source {
            SettingsApplySource::ConfigReload => self.overlay.refresh_settings(&self.settings),
            SettingsApplySource::OverlayEdit => self.overlay.apply_settings(&self.settings),
        }
        // U4: all theme publishes read `effective_theme` (the authored theme
        // after CVD adaptation; identical to it when off), so the renderer sees
        // the adapted colors while `self.theme` keeps the authored one for
        // save/round-trip.
        text::set_default_colors(
            self.effective_theme.foreground,
            self.effective_theme.background,
        );
        text::set_ansi_palette(&self.effective_theme.palette);
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
                rgb(self.effective_theme.cursor)
            } else {
                rgb(self.effective_theme.foreground)
            };
            terminal.set_base_colors(
                rgb(self.effective_theme.foreground),
                rgb(self.effective_theme.background),
                cursor_default,
            );
            terminal.set_osc52_read_enabled(self.settings.osc52_read);
            terminal.set_cursor_defaults(
                self.settings.cursor_style,
                self.settings.cursor_blink.enabled(),
            );
        }
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_theme(self.effective_theme);
            gpu.set_visual(self.visual);
            gpu.set_text_gamma(self.settings.text_gamma);
            gpu.set_bloom(bloom_options(&self.settings));
            gpu.set_crt(crt_options(&self.settings));
            // ID3/U5: push the background-image settings. The scrim is computed
            // against `effective_theme` (the same CVD/OS-resolved background the
            // RV1 floor references), so the floor stays valid at any opacity.
            gpu.set_background_image(
                self.settings.effective_background_treatment()
                    == crate::settings::BackgroundTreatment::Image,
                self.settings.background_image.as_deref(),
                self.settings.background_blur_radius,
                self.settings.background_image_scrim,
                self.settings.cell_bg_opacity,
                self.effective_theme,
            );
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

    fn run_about_to_wait_maintenance(&mut self, now: Instant) {
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

        // A due cursor-animation tick (ID1 easing fade / VE4 slide) rebuilds once
        // so the eased alpha / slide offset advance. `animation_deadline()` is
        // `None` whenever nothing is animating (both knobs off, or the animation
        // settled), so this fires only while an animation is in flight and the
        // terminal returns to zero-wake idle once it completes (bounded wake).
        if self
            .animation_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
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
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let (w, h) = self.options.window_logical_size();
        // WIN-DECOR: request decorations per config at creation. Default `true`
        // matches `WindowAttributes::default()`, so the startup chain is
        // byte-identical when unset.
        let attributes = Window::default_attributes()
            .with_title(self.options.title.clone())
            .with_inner_size(LogicalSize::new(w, h))
            .with_decorations(self.settings.window_decorations);

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
            self.effective_theme,
            self.visual,
            self.settings.effective_stem_darken(),
            bloom_options(&self.settings),
            crt_options(&self.settings),
        ) {
            Ok(mut gpu) => {
                // Push live cell pixel metrics to the terminal core so graphics
                // placements (sixel/kitty) compute the correct cell extent.
                let cell = gpu.cell();
                if let Ok(mut term) = self.terminal.lock() {
                    term.set_cell_metrics(cell.width, cell.height);
                }
                self.last_presented_snapshot = Some(initial_snapshot.clone());
                // ID3/U5: seed the background-image pass from the launch config
                // so the very first frame already reflects an `image` treatment
                // (no-op / off path when no image is configured).
                gpu.set_background_image(
                    self.settings.effective_background_treatment()
                        == crate::settings::BackgroundTreatment::Image,
                    self.settings.background_image.as_deref(),
                    self.settings.background_blur_radius,
                    self.settings.background_image_scrim,
                    self.settings.cell_bg_opacity,
                    self.effective_theme,
                );
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

        // OS-THEME: seed the OS appearance from the window (Wayland delivers a
        // value here; X11 returns `None`) or the `ODYTTY_APPEARANCE` env
        // fallback, then apply the override so the very first frame already
        // reflects the OS preference. No-op when following is off (the resolve
        // returns the authored theme and the apply early-returns on equality),
        // so the default startup path is unchanged.
        if self.settings.follow_os_theme {
            self.os_theme = self
                .window
                .as_ref()
                .and_then(|window| window.theme())
                .or_else(os_theme::env_appearance_override);
            self.apply_os_theme_override();
        }

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
                // CLOSE-CONFIRM: when enabled and a foreground job is actually
                // running, intercept the close and raise the confirmation dialog
                // instead of exiting. Only `ForegroundJob::Running` prompts —
                // `None` (idle shell) and `Unknown` (query error / dead PTY) and
                // a poisoned lock all fall through to the immediate exit, so the
                // off path and the common idle-close path are unchanged (TRAP-1,
                // TRAP-5).
                if self.settings.confirm_close
                    && self
                        .pty
                        .lock()
                        .is_ok_and(|pty| pty.foreground_job() == ForegroundJob::Running)
                {
                    self.overlay.open_confirm_close();
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                } else {
                    event_loop.exit();
                }
            }
            WindowEvent::ThemeChanged(os_theme) => {
                // OS-THEME: the compositor reported a dark/light preference
                // change (Wayland). Record it always; re-resolve the active
                // theme only while following is on. `apply_os_theme_override`
                // bumps the presentation epoch and requests a redraw itself.
                self.os_theme = Some(os_theme);
                if self.settings.follow_os_theme {
                    self.apply_os_theme_override();
                }
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
                self.flush_pending_overlay_settings();
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
                            let (scrollback_len, prompt_marks_changed) = {
                                let mut terminal = self.terminal.lock().expect("terminal mutex");
                                (
                                    terminal.screen().scrollback_len(),
                                    self.settings.command_status_gutter
                                        && terminal.take_prompt_marks_changed(),
                                )
                            };
                            let added = scrollback_len.saturating_sub(self.last_scrollback_len);
                            self.viewport.anchor_after_growth(added, scrollback_len);
                            self.last_scrollback_len = scrollback_len;
                            self.viewport.clamp(scrollback_len);
                            if prompt_marks_changed {
                                self.prompt_marks_epoch = self.prompt_marks_epoch.wrapping_add(1);
                            }
                            let offset = self.viewport.offset();
                            let mut search = std::mem::take(&mut self.search);
                            let terminal = self.terminal.lock().expect("terminal mutex");
                            if search.is_open() {
                                search.refresh(&terminal);
                            }
                            let visible_graphics = terminal.visible_graphics(offset);
                            let image_uploads = image_uploads_for_visible(
                                &terminal,
                                &visible_graphics,
                                &cached_image_ids,
                            );
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
                                image_uploads,
                            )
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
                        // Blink off-phase hard-hide — skipped while easing is on,
                        // where the precomputed alpha carries the fade instead (so
                        // easing does not double-hide).
                        if !cursor_on && !self.settings.cursor_easing {
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
                        // RV4 smooth scroll: advance the eased glide for this
                        // frame (recompute `scroll_frac_offset`, settle when the
                        // bounded duration elapses). No-op while idle / off, so
                        // the offset stays 0.0 and the path is byte-identical.
                        self.update_scroll_anim(now);
                        let ctx = self.overlay_ctx(
                            scrollback_len,
                            cell,
                            snapshot.cursor,
                            snapshot.cursor_visible,
                            now,
                        );
                        self.paint_selection_cells(&mut snapshot, &ctx);
                        self.paint_search_cells(&mut snapshot, &ctx);
                        self.paint_overlay_cells(&mut snapshot, &ctx);
                        self.paint_hyperlink_cells(&mut snapshot, &ctx);
                        self.paint_hints_cells(&mut snapshot, &ctx);
                        self.paint_copy_mode_cells(&mut snapshot, &ctx);
                        self.paint_rename_tab_cells(&mut snapshot);
                        // Frame-overlay quad manifest: scroll indicator, then the
                        // off-by-default SH2 gutter, then the no-op new slots.
                        let mut overlays: Vec<SolidQuad> = Vec::new();
                        self.paint_scroll_indicator_quads(&ctx, &mut overlays);
                        self.paint_gutter_quads(&ctx, &mut overlays);
                        self.paint_cursor_trail_quads(&ctx, &mut overlays);
                        self.paint_cursor_glow_quads(&ctx, &mut overlays);
                        self.paint_background_quads(&ctx, &mut overlays);
                        // ID4 themed window border: a thin frame in the padding
                        // band, drawn over any background treatment; empty on the
                        // off path.
                        self.paint_window_border_quads(&ctx, &mut overlays);
                        // VE4 new-output fade quads — last so they obscure the
                        // freshly arrived rows on top of all other overlays;
                        // empty on the off path. The cursor block draws after
                        // overlays (ID1 reorder), so it is never hidden.
                        self.paint_new_row_fade_quads(&ctx, &mut overlays);
                        let tab_bar_offset = self.tab_bar_height_px(cell);
                        if tab_bar_offset > 0.0 {
                            self.shift_overlays_for_tab_bar(&mut overlays, tab_bar_offset);
                        }
                        let (snapshot, tab_bar_quads) = self.decorate_snapshot_with_tab_bar(
                            &snapshot,
                            snapshot.cursor_visible,
                            cell,
                        );
                        let content_snapshot = {
                            let mut content_snapshot = snapshot.clone();
                            content_snapshot.cursor_visible = base_cursor_visible;
                            content_snapshot
                        };
                        overlays.extend(tab_bar_quads);
                        // R3 call-site parity + A2 cache observability: compute
                        // the live cursor params ONCE so the signature `anim` key
                        // and the GPU CursorOnly call derive from the same source.
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
                                },
                            },
                            cursor: CursorRenderSignature {
                                visible: snapshot.cursor_visible,
                                style: cursor_style,
                                anim: CursorAnimKey::from_params(&cursor_params),
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
                        // ID3/U5 background treatment: resolved once per Full
                        // rebuild (identity when the knob is off, so the plain
                        // path is byte-identical). grid.rs applies it per cell
                        // before the RV1 floor.
                        let background_treatment = self.background_treatment_params();
                        // `cursor_params` was hoisted above the signature literal
                        // (it feeds the `anim` cache key); the CursorOnly arm
                        // reuses the same value so the cached cursor matches.
                        let scroll_frac_offset = self.scroll_frac_offset;
                        let tab_bar_row_offset = self.tab_bar_row_offset();
                        if let Some(gpu) = self.gpu.as_mut() {
                            // RV4: push the current smooth-scroll offset so the
                            // vertex builders shift `content_origin` this frame.
                            // `0.0` at rest / on the off path leaves the origin
                            // byte-identical.
                            gpu.set_scroll_frac_offset(scroll_frac_offset);
                            match update {
                                GeometryUpdate::Full => {
                                    gpu.update_image_layer(
                                        &visible_graphics,
                                        &image_uploads,
                                        tab_bar_row_offset,
                                    );
                                    if overlays.is_empty() {
                                        gpu.update_from_snapshot(
                                            &snapshot,
                                            cursor_style,
                                            focus_dim,
                                            background_treatment,
                                        );
                                    } else {
                                        gpu.update_from_snapshot_with_overlays(
                                            &snapshot,
                                            cursor_style,
                                            &overlays,
                                            focus_dim,
                                            background_treatment,
                                        );
                                    }
                                }
                                GeometryUpdate::CursorOnly => {
                                    gpu.update_cursor_and_overlays(
                                        &snapshot,
                                        cursor_style,
                                        &overlays,
                                        cursor_params,
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
                    // WHEEL-SENS (T-reset): drop any partially-accumulated wheel
                    // notch so a gesture interrupted by an alt-tab does not
                    // resume against the next surface on focus regain.
                    self.wheel_accum.reset();
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
        // CLOSE-CONFIRM: an overlay outcome dispatched during this event (the
        // confirmation dialog's Enter/Y) may have requested the window close.
        // The overlay apply path only holds `&mut self`, so it sets this flag
        // and the actual exit happens here where the event loop is in scope.
        // Stays `false` on every path that does not confirm a close, so the
        // off/default behavior is unchanged.
        if self.pending_exit {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if self.apply_user_event(event) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.run_about_to_wait_maintenance(now);

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

fn bloom_options(settings: &Settings) -> BloomOptions {
    BloomOptions {
        enabled: settings.effective_bloom_enabled(),
        threshold: settings.effective_bloom_threshold(),
        intensity: settings.effective_bloom_intensity(),
        radius: settings.effective_bloom_radius(),
    }
}

fn crt_options(settings: &Settings) -> CrtOptions {
    CrtOptions {
        enabled: settings.effective_crt_enabled(),
        scanline_intensity: settings.effective_crt_scanline_intensity(),
        scanline_period: settings.crt_scanline_period,
        vignette_strength: settings.effective_crt_vignette_strength(),
        curvature: settings.effective_crt_curvature(),
    }
}

/// Whether the first-run onboarding card should open at startup (ONBOARD).
/// `env_override` forces it on (the `ODYTTY_ONBOARDING` escape hatch / CI).
/// Otherwise it is a first launch iff the resolved `config_path` does not yet
/// exist. An unresolvable path (no writable config dir) returns `false` —
/// fail-safe to NOT nagging, since dismissal could not be persisted (D-OB-2).
fn should_show_onboarding(env_override: bool, config_path: Option<&std::path::Path>) -> bool {
    env_override || config_path.map(|path| !path.exists()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blink() -> CursorBlinkState {
        CursorBlinkState::new(Duration::from_millis(500))
    }

    #[test]
    fn onboarding_opens_only_on_first_run_or_override() {
        // Absent config ⇒ first run ⇒ show.
        let missing = std::path::Path::new("/nonexistent/odytty/odytty.conf");
        assert!(should_show_onboarding(false, Some(missing)));
        // A path that exists ⇒ NOT first run ⇒ do not show. Cargo guarantees
        // this manifest is present during the test.
        let present = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(present.exists());
        assert!(!should_show_onboarding(false, Some(present.as_path())));
        // Env override forces it on regardless of file state.
        assert!(should_show_onboarding(true, Some(present.as_path())));
        // Unresolvable path ⇒ fail-safe to not nagging.
        assert!(!should_show_onboarding(false, None));
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

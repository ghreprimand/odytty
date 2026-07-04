// SPDX-License-Identifier: GPL-3.0-only
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::core::Terminal;
use crate::core::{
    ClipboardRequest, Color, Dimensions, InputCertainty, LinkId, MouseButton as CoreMouseButton,
    MouseEncoding, MouseEventKind, MouseModifiers, MouseProtocol, Position, RgbColor, Snapshot,
    encode_mouse_event_pixel,
};
use crate::grid::{CursorRenderParams, SolidQuad};
use crate::input::{self, Key, KeyEventType, KeyModes, Modifiers};
#[cfg(test)]
use crate::pty::PtySession;
use crate::selection::{
    self, AbsoluteSelectionRange, CellPoint, ClickTracker, PointerDrag, SelectGranularity,
    SelectionStyle,
};
use crate::settings::{
    BindableAction, MAX_TAB_RAIL_WIDTH, MIN_TAB_RAIL_WIDTH, SettingEdit, Settings,
    SettingsReloadOutcome, SettingsReloader, TAB_RAIL_WIDTH_ENV, THEME_ENV, TabBarPlacement,
    apply_reloadable_values, ensure_config_file_exists_at, write_settings_changes_to_path,
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
use winit::window::{CursorIcon, Window, WindowId};

use super::bindings::{
    KeyBindings, PrefixEngine, PrefixOutcome, changed_window_title, encode_native_focus_report,
    encode_native_mouse_report, map_keypad_physical_key, map_named_key, map_winit_mouse_button,
    motion_report_button, prefix_chord_from_winit, wheel_report_button,
};
use super::clipboard::{
    NativeClipboard, read_clipboard_selection, selected_clipboard_text, write_clipboard_selection,
    write_paste_text,
};
use super::cvd_theme::CvdThemeCache;
use super::gpu::{BloomOptions, CrtOptions, FrameOutcome, GpuState, RailOverlay};
use super::options::{NativeError, NativeOptions};
use super::overlay::{
    OverlayInput, OverlayOutcome, OverlayPointer, OverlayUi, PointerButton, apply_overlay,
    overlay_input_from_winit, overlay_rect,
};
#[cfg(test)]
use super::pty::PtyWriter;
use super::pty::UserEvent;
use super::render_helpers::{
    CursorAnimKey, CursorRenderSignature, GeometryUpdate, OverlayCompositeSignature,
    OverlayFragment, RailOverlaySignature, RenderContentSignature, RenderSignature,
    SelectionSignature, hyperlink_action_allowed, image_uploads_for_visible, key_modes_from_core,
    openable_hyperlink_uri, visible_graphics_signature,
};
use super::theme_builder::{save_theme_to_dir, user_theme_dir_for_config};

use self::panes::{DIVIDER_GRAB_PX, PANE_DIVIDER_PX, pane_content_rect};
pub(super) use super::cursor::{CURSOR_BLINK_INTERVAL, CursorBlinkState};
use super::layout::{FocusDir, SplitAxis};
pub(super) use super::resize::{
    PendingResize, RESIZE_DEBOUNCE_INTERVAL, ResizeDebouncer, pending_resize_for_surface,
    scale_factor_changed,
};
use super::search_ui::{SearchStyle, apply_search_ui};
use super::session::{Session, SessionToken, WorkspaceSet};
use super::viewport::{
    OverlayWheelDamper, SELECTION_AUTOSCROLL_INTERVAL, WheelAccumulator, WindowPadding,
    grid_dimensions_for_with_padding, scroll_indicator_hit_with_padding,
    scroll_indicator_quad_with_padding, scrollbar_offset_for_drag_with_padding, wheel_lines,
    wheel_lines_scaled, wheel_zoom_steps,
};

mod background_ui;
mod bell;
pub(in crate::native) mod click_hint;
mod connections_ui;
mod copy_mode_ui;
mod cursor;
mod cursor_frame;
mod cursor_trail;
mod detach_switch;
mod gutter_ui;
mod hints_ui;
mod ime;
mod interaction;
pub(in crate::native) mod interactive_paths;
mod new_row_fade;
mod open_notice;
mod open_with_ui;
mod os_theme;
mod overlay_registry;
mod palette_ui;
mod panes;
pub(in crate::native) mod platform_opener;
mod pointer;
mod prompt_jump;
mod rail_autohide;
mod replay_ui;
mod scroll_anim;
mod session_attach_ui;
mod ssh_connect;
mod tab_bar;
// F4-RESKIN: shared "Phosphor Flat" treatment (color) for both tab-chrome axes.
mod tab_chrome;
// F4-P1: unified tab-panel + seam background-quad geometry (color from
// `tab_chrome`), spliced into the GPU background segment behind the chrome.
mod tab_panel;
// F4-V2 R1: vertical tab rail widget — the sibling of `tab_bar`, active when
// `tab_bar_placement` is a rail.
mod tab_rail;
#[cfg(test)]
mod test_seams;
mod theme_roles;
mod watchdog_probe;
mod window_border;

pub(in crate::native) use self::hints_ui::HintsUi;
pub(in crate::native) use self::scroll_anim::ScrollAnimState as SessionScrollAnimState;
pub(in crate::native) use self::tab_bar::{TAB_BAR_ROWS, TabBarSource};
use self::tab_bar::{TabBar, TabHit};
use self::tab_rail::{RailSide, TabRail};
pub(in crate::native) use overlay_registry::ActiveModal;

/// Linux desktop identity used for Wayland app_id/WM_CLASS matching.
///
/// macOS and Windows take process identity from their bundle/host metadata, so
/// the runtime use is cfg'd out there.
#[cfg(all(unix, not(target_os = "macos")))]
const APP_ID: &str = "io.unfinished_works.odytty";
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

    /// Release the hold with no scheduled wake (the `enabled = false` rest
    /// state). Used to settle a deactivated session's hold: a background tab is
    /// never rendered, so its hold deadline must not linger as a wake source that
    /// nothing consumes (NF20-B). A later synchronized batch on that session,
    /// once active again, re-arms the hold via [`Self::should_hold`].
    pub(super) fn clear(&mut self) {
        self.active_since = None;
        self.timed_out = false;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenameState {
    target: SessionToken,
    text: String,
    /// Caret position as a *character* index into `text` (not a byte offset).
    cursor: usize,
    /// F4-RENAME-MOUSE: the selection anchor as a character index. `Some` while
    /// a range is being (or has been) selected; the live selection spans
    /// `[min(anchor, cursor), max(anchor, cursor))`. Any caret motion or edit
    /// that is not a selection-extend clears it back to `None`. When
    /// `anchor == cursor` the selection is empty (an armed but collapsed drag).
    anchor: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsApplySource {
    ConfigReload,
    OverlayEdit,
}

#[cfg(test)]
thread_local! {
    /// F1 test seam: argv vectors that [`App::handle_new_window`] would have
    /// spawned. Under the test target the handler records here instead of
    /// launching a real second OdyTTY instance, so chord/menu dispatch can be
    /// asserted at the spawn boundary. Thread-local, so each libtest thread sees
    /// only its own recordings; tests clear it before driving the dispatch.
    static NEW_WINDOW_SPAWN_ARGV: std::cell::RefCell<Vec<Vec<String>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Owned inputs for the F4-P3 revealed rail overlay, built once per frame by
/// [`App::build_rail_overlay`]. Holds the strip snapshot by value (the GPU call
/// borrows it) plus the pre-resolved origin and wash/seam quads; the render path
/// lends a [`gpu::RailOverlay`] from it at the update call.
struct RailOverlayData {
    snapshot: Snapshot,
    origin: [f32; 2],
    wash: Option<SolidQuad>,
    seam: Option<SolidQuad>,
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
    /// Last mouse-cursor shape pushed to the window. Tracked so
    /// [`Self::apply_cursor_icon`] only calls `Window::set_cursor` on an actual
    /// change (winit issues a platform request each call). Starts at the winit
    /// default (`Default` arrow) which matches a freshly created window.
    cursor_icon: CursorIcon,
    sessions: WorkspaceSet,
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
    /// Multiplexer prefix engine (§7). Holds the configurable prefix chord, the
    /// pane-action table, and the transient prefix-pending state. Additive: when
    /// no prefix is pending (or the prefix is `off`), it leaves the input path
    /// byte-identical.
    prefix_engine: PrefixEngine,
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
    /// BLACK-SCREEN-ON-RESTORE: pending bounded retry for a transiently-skipped
    /// frame. When a render returns [`FrameOutcome::Skipped`] (the surface
    /// acquire timed out / was occluded, e.g. the first frame as a Windows DX12
    /// surface recovers on restore), this holds the instant to retry. Folded
    /// into [`Self::next_wake_deadline`] so the retry rides the existing
    /// `WaitUntil` model (no busy-poll); cleared once due or once a frame
    /// presents. `None` on the steady-state path, so the idle wake set is
    /// unchanged when nothing is skipping.
    skipped_frame_retry_deadline: Option<Instant>,
    /// BLACK-SCREEN-ON-RESTORE: count of consecutive `Skipped` frames with no
    /// successful present in between. Caps the bounded retry (see
    /// [`MAX_SKIPPED_RETRIES`]) so a persistently-unavailable surface can't
    /// wake-loop forever. Reset to 0 on any present / reconfigure and on a
    /// restore (`Resized` to a non-zero size).
    consecutive_skipped_frames: u32,
    /// BLACK-SCREEN-ON-RESTORE: whether the window is currently minimized (its
    /// surface reported a 0x0 size via `Resized`). Used to suppress the skipped-
    /// frame retry while minimized — there is nothing to paint, so a retry would
    /// only burn wakeups. Cleared on the next non-zero `Resized` (restore).
    window_minimized: bool,
    /// Active divider drag: the tree-order index of the active tab's divider the
    /// pointer grabbed, while a left-drag is in progress (design doc §4.2). Only
    /// ever `Some` inside a multi-pane tab; `None` otherwise, so the single-pane
    /// pointer path is unaffected.
    divider_drag: Option<usize>,
    /// F4-P4 auto-width cache: the rail band width (cells) currently baked into
    /// the content-grid reservation. `reconcile_rail_auto_width` reflows the
    /// grid only when the live resolved width diverges from this, so a title
    /// change / tab add-remove / max-width edit re-sizes the content exactly
    /// once. 0 on the top-bar / hidden path.
    rail_reserved_cols: usize,
    /// F4-P4 seam drag: `true` while the left button is held after grabbing the
    /// rail's inner (content-facing) edge to resize it. Pointer motion then sets
    /// a manual width; release persists it. Only ever `true` while a rail is
    /// shown, so the top-bar / single-pane paths are unaffected.
    rail_seam_drag: bool,
    /// F4-P4 double-click detection for the rail seam (reset-to-auto). Keyed on
    /// a fixed synthetic point so two quick seam presses register as a double-
    /// click; reset on an actual drag move so a drag-then-grab is not misread as
    /// one. Separate from the grid/rename trackers.
    rail_seam_clicks: ClickTracker,
    /// F4-P3 rail auto-hide timing state machine (ODP-4). Inert unless
    /// `tab_rail_autohide` is on and the chrome is a side rail; when active it
    /// drives the reveal/hide of the floating rail overlay from the pointer edge
    /// zone, keyboard flashes, and the debounce/grace timers. The reservation is
    /// removed (`tab_reserve` → NONE) the moment autohide is active, so reveal is
    /// a pure overlay and never reflows content.
    rail_autohide: rail_autohide::RailAutohide,
    /// The previous physical pointer x fed to the reveal machine, for the
    /// motion-aware trigger: the segment from this to the current sample is
    /// tested against the edge zone so a fast approach that jumps clean over a
    /// static point zone still arms. `None` before the first sample and after the
    /// pointer leaves the window (so a re-entry never fabricates a segment across
    /// the whole surface). Only meaningful while auto-hide is active.
    last_rail_pointer_px: Option<f64>,
    /// Whether the window currently holds focus. Blink pauses (cursor solid)
    /// while unfocused, matching common terminal behavior.
    focused: bool,
    /// BELL visual-flash start instant, set when a bell is drained while the
    /// bell mode wants a visual flash. `None` when no flash is in flight (the
    /// off / urgent-only path), so the default render path emits no flash quad.
    bell_flash_start: Option<Instant>,
    /// Monotonic epoch bumped once per rebuild while the bell flash is active so
    /// each animation frame reclassifies the render cache (the flash alpha moves
    /// while cell content does not). Constant while no flash is in flight.
    bell_flash_epoch: u64,
    /// OPEN-NOTICE (P0-2): a transient, non-blocking status line shown when an
    /// open/reveal spawn fails (a missing/broken `xdg-open`/`open`), so a failed
    /// open is never an indistinguishable silent no-op. `None` on every success
    /// and feature-off path, so the default render path is byte-identical (the
    /// painter and signature both early-out on `None`). Auto-expires after
    /// [`open_notice::NOTICE_DURATION`]; carries the message and the instant it
    /// was raised.
    open_notice: Option<open_notice::OpenNotice>,
    /// UX-A (Phase 11): in-memory, per-launch click-to-open discoverability
    /// state — the transient bottom-left "Ctrl+click to open" hint plus the
    /// mis-click bookkeeping that decides when to raise it. NOT persisted; resets
    /// every window launch. Idle (and byte-identity-irrelevant) on the default /
    /// feature-off path: the painter and signature both early-out when not shown.
    click_hint: click_hint::ClickHintState,
    /// Active IME pre-edit (composition) string as delivered by `winit`'s
    /// `Ime::Preedit`. Empty when no composition is in progress. Rendered inline
    /// at the terminal cursor; never sent to the PTY until the IME commits.
    ime_preedit: String,
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
    /// P1-8: macOS-only per-detent damper for the overlay list wheel path. Emits
    /// exactly one item per physical detent and absorbs the inertial momentum
    /// tail of a trackpad flick. Only consulted on the macOS handler branch of
    /// `handle_overlay_pointer_wheel`; idle (and byte-identity-irrelevant) on
    /// every other target. Reset alongside `wheel_accum` on focus loss and
    /// overlay open.
    overlay_wheel: OverlayWheelDamper,
    /// Visible multi-session tab strip state. Presentation-only; the session
    /// model stays in `WorkspaceSet`.
    tab_bar: TabBar,
    /// Vertical tab rail state (F4-V2 R1) — the sibling of `tab_bar`, active
    /// only when `tab_bar_placement` is a rail. Presentation-only.
    tab_rail: TabRail,
    rename_state: Option<RenameState>,
    /// F4-RENAME-MOUSE: double-click detection for the tab-rename field, kept
    /// separate from the terminal-grid `clicks` tracker so a rename word-select
    /// never interacts with a grid selection streak. Reset when a rename opens.
    rename_clicks: ClickTracker,
    /// F4-RENAME-MOUSE: a left-button drag is in progress inside the rename
    /// field. Set on a press that lands on the input line, cleared on release
    /// (or when the rename closes). While set, pointer motion extends the
    /// field selection instead of doing any grid hover/selection work.
    rename_dragging: bool,
    /// SLIDER-GUARD: whether the left mouse button is currently held while the
    /// overlay is open. Set on `MouseInput { Pressed, Left }` and cleared on
    /// `MouseInput { Released, Left }` through the overlay pointer path. Used to
    /// gate overlay slider drag moves so that cursor movements after the button
    /// is released can NEVER advance an armed drag — even if the drag state is
    /// somehow stale. `CursorMoved` carries no button state, so this flag is the
    /// reliable held-button seam for the settings-slider path (D-SLIDER-GUARD).
    overlay_left_held: bool,
    /// INTERACTIVE-PATHS (Phase 7): the process `$HOME`, cached once at startup
    /// (it never changes mid-process) so `~`-prefixed path spans can be expanded
    /// at hover time without a per-move `getenv`. `None` when `$HOME` is unset or
    /// not valid UTF-8; only consulted while `interactive_paths` is on.
    home_dir: Option<String>,
    /// C4 image viewer: the decoded RGBA buffer + dims for the image currently
    /// shown in the `ImageView` overlay, kept so a window resize can recompute
    /// the centered fit-rect without re-decoding. `None` whenever the viewer is
    /// closed; the per-frame [`Self::sync_image_overlay`] clears it (and the GPU
    /// overlay texture) once the overlay is no longer open.
    image_overlay: Option<interactive_paths::ImageOverlayState>,
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
            WorkspaceSet::new(session, None),
            settings,
            settings_reloader,
        )
    }

    pub(super) fn new_with_sessions(
        options: NativeOptions,
        sessions: WorkspaceSet,
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
        let prefix_engine = PrefixEngine::from_settings(&settings);
        let autoclose = settings.native_autoclose;
        let themed_ui_roles = settings.themed_ui_roles;
        let overlay = OverlayUi::new(&settings);
        // `mut` is consumed only by the `cfg(not(test))` onboarding block below;
        // test builds compile that out, so silence the unused_mut there.
        #[cfg_attr(test, allow(unused_mut))]
        let mut app = Self {
            options,
            theme,
            effective_theme,
            cvd_cache,
            visual,
            window: None,
            gpu: None,
            cursor_icon: CursorIcon::Default,
            sessions,
            presentation_epoch: 0,
            prompt_marks_epoch: 0,
            grid,
            modifiers: Modifiers::default(),
            super_key: false,
            key_bindings,
            prefix_engine,
            settings,
            settings_reloader,
            pending_overlay_settings: None,
            themed_ui_roles,
            overlay,
            clipboard: NativeClipboard::default(),
            resize_debounce: ResizeDebouncer::new(RESIZE_DEBOUNCE_INTERVAL),
            skipped_frame_retry_deadline: None,
            consecutive_skipped_frames: 0,
            window_minimized: false,
            divider_drag: None,
            rail_reserved_cols: 0,
            rail_seam_drag: false,
            rail_seam_clicks: ClickTracker::default(),
            rail_autohide: rail_autohide::RailAutohide::default(),
            last_rail_pointer_px: None,
            // Assume focused at startup; the first `Focused` event corrects it.
            focused: true,
            bell_flash_start: None,
            bell_flash_epoch: 0,
            open_notice: None,
            click_hint: click_hint::ClickHintState::default(),
            ime_preedit: String::new(),
            autoclose,
            deadline: None,
            os_theme: None,
            pending_exit: false,
            wheel_accum: WheelAccumulator::default(),
            overlay_wheel: OverlayWheelDamper::default(),
            tab_bar: TabBar::default(),
            tab_rail: TabRail::default(),
            rename_state: None,
            rename_clicks: ClickTracker::default(),
            rename_dragging: false,
            overlay_left_held: false,
            home_dir: std::env::var_os("HOME").and_then(|h| h.into_string().ok()),
            image_overlay: None,
            startup_error: None,
        };
        // ONBOARD (D-OB-1/D-OB-2): open the first-run welcome card iff the
        // config file does not yet exist (or the env override is set). First-run
        // memory is the user-owned config's existence — no telemetry, no flag
        // file (U6). Materializing the config (saving any setting) retires it.
        //
        // Gated out of test builds: the unit-test harness constructs many Apps
        // through this path with the *host* config-resolver, so a machine
        // without a materialized `odytty.conf` (every fresh CI runner, and any
        // contributor who hasn't saved a setting) would auto-open the overlay
        // and make every overlay-sensitive test non-hermetic. Production
        // (`cfg(not(test))`) is unchanged; the decision itself is covered by the
        // `should_show_onboarding` unit test and the overlay state-machine tests.
        #[cfg(not(test))]
        {
            let onboarding_override = std::env::var_os("ODYTTY_ONBOARDING").is_some();
            if should_show_onboarding(onboarding_override, app.settings_reloader.config_path()) {
                app.overlay.open_onboarding();
            }
        }
        // Phase 2 output recording: seed the initial session's recorder with the
        // configured `session_replay` state at startup, so a window launched
        // with recording already enabled records from the first output. Off (the
        // default) is a no-op.
        app.sessions
            .set_recording_enabled(app.settings.session_replay);
        app.sessions
            .set_shell_integration_enabled(app.settings.shell_integration);
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
        // Reserve the tab chrome off the grid: rows off the top for the
        // horizontal bar, or columns off the side for the vertical rail (F4-V2).
        // `reserve` is `NONE` when the bar is hidden, so the plain path is
        // byte-identical; the resize path and the snapshot-grow path
        // (`decorate_snapshot_with_tab_bar` / `..._rail`) read the SAME reserve so
        // the grid, cursor, and pointer can never desync (ODP-8).
        let reserve = self.tab_reserve();
        if reserve.top_rows > 0 {
            new_grid.rows = new_grid.rows.saturating_sub(reserve.top_rows).max(1);
        }
        // Reserve the rail band AND its wallpaper gap (R1.1) off the content
        // columns, so the grid the terminal reflows into matches the shrunken
        // content rect `pane_content_rect` returns.
        let reserved_cols = reserve.left_reserved_cols() + reserve.right_reserved_cols();
        if reserved_cols > 0 {
            new_grid.columns = new_grid.columns.saturating_sub(reserved_cols).max(1);
        }
        if new_grid == self.grid {
            return false;
        }
        self.grid = new_grid;

        // Size every pane of every tab to its laid-out sub-rect. For an all-
        // single-pane world each tab's lone leaf spans the whole content rect,
        // so this resizes each session to exactly `new_grid` — byte-identical to
        // the old per-session loop. Multi-pane tabs get per-pane sizing (#1).
        let content = pane_content_rect(width_px, height_px, cell, padding, reserve);
        self.sessions
            .resize_all_panes(content, cell.width, cell.height, PANE_DIVIDER_PX);
        true
    }

    fn apply_grid_resize(&mut self, resize: PendingResize) {
        // A minimized window can report a 0x0 drawable surface. The GPU surface
        // ignores that size, and the terminal model must do the same: passing
        // zero through grid fitting clamps to 1x1 and destructively reflows the
        // live screen while there is no drawable area.
        if resize.width_px == 0 || resize.height_px == 0 {
            return;
        }
        if self.resize_grid_with_padding(
            resize.cell,
            resize.padding,
            resize.width_px,
            resize.height_px,
        ) {
            // NF21-3: `resize_grid_with_padding` -> `resize_all_panes` reflows
            // EVERY session of every tab, so the stale-layout invalidation must
            // reach every session too — not only the active one via `Deref`. A
            // background tab that crossed the reflow keeping its old absolute-row
            // selection / search / hints / copy-mode coordinates would, on
            // switch-back, highlight the wrong text and copy the wrong bytes.
            // The per-session helper clears the exact same field set in the same
            // order as the old active-only block, so the active tab stays
            // byte-identical; `clamp()` in the rebuild still guards viewport
            // bounds regardless.
            self.sessions.invalidate_all_layout_dependent_state();
            self.needs_rebuild = true;
        }
    }

    fn record_pending_resize(&mut self, resize: PendingResize, now: Instant) {
        if let Some(due) = self.resize_debounce.record(resize, now) {
            self.apply_grid_resize(due);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn initialize_session_with(
        session: &mut Session,
        effective_theme: Theme,
        themed_ui_roles: bool,
        osc52_read: bool,
        cursor_style: crate::core::CursorStyle,
        cursor_blink: crate::settings::CursorBlink,
        cell: Option<CellSize>,
        scrollback_limit: usize,
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
            // C29: OSC 4 replies report the theme palette, not the xterm table.
            terminal.set_base_palette(effective_theme.palette.map(rgb));
            terminal.set_osc52_read_enabled(osc52_read);
            terminal.set_scrollback_limit(scrollback_limit);
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
            let scrollback_limit = self.settings.scrollback_limit();
            if let Some(session) = self.sessions.get_mut(session_id) {
                Self::initialize_session_with(
                    session,
                    effective_theme,
                    themed_ui_roles,
                    osc52_read,
                    cursor_style,
                    cursor_blink,
                    cell,
                    scrollback_limit,
                );
            }
            let _ = self.sessions.switch(session_id);
            // F4-P3: a new-tab chord flashes the auto-hidden rail so the new tab
            // is confirmed even with the pointer away from the edge.
            self.flash_rail_autohide();
            self.on_active_session_changed();
        }
    }

    /// F1: launch another top-level OdyTTY window — a fresh process instance,
    /// not a tab. Spawned from [`std::env::current_exe`] with no extra args, so
    /// the child inherits this process's environment (theme/env overrides carry
    /// over naturally). Routed through the reaper-backed [`spawn_detached`]
    /// (never a bare `Command::spawn`), so the child is reaped and never left a
    /// zombie. Best-effort: an unresolvable executable path or a spawn failure
    /// is logged and dropped — a new-window request must never crash the
    /// running window (consistent with the C6 log-and-drop philosophy). V1 does
    /// NOT propagate the focused pane's shell cwd (that needs OSC 7 / procfs
    /// plumbing; tracked as a follow-up).
    pub(in crate::native) fn handle_new_window(&mut self) {
        let Some(argv) = Self::new_window_argv() else {
            tracing::warn!(
                "new-window: could not resolve the current executable; not launching a window"
            );
            return;
        };
        #[cfg(test)]
        {
            // Test seam: record the argv that WOULD be spawned instead of
            // launching a real second instance, so the chord/menu dispatch can
            // be asserted at the spawn boundary without side effects.
            NEW_WINDOW_SPAWN_ARGV.with(|cell| cell.borrow_mut().push(argv));
        }
        #[cfg(not(test))]
        {
            if let Err(err) = interactive_paths::spawn_detached(&argv) {
                tracing::warn!(error = %err, "new-window: failed to launch a new OdyTTY window");
            }
        }
    }

    /// The argv that opens a new OdyTTY window: just the current executable, no
    /// args (the child inherits the environment). Pure — returns `None` when the
    /// current-exe path cannot be resolved or is not valid UTF-8 (the argv seam
    /// is `String`-based). Split out so the dispatch decision is unit-testable
    /// without spawning.
    fn new_window_argv() -> Option<Vec<String>> {
        let exe = std::env::current_exe().ok()?;
        Some(vec![exe.into_os_string().into_string().ok()?])
    }

    /// Attach to a detached, session-host-backed session by id and present it as
    /// a new live tab in this window — the production "reopen by id, full
    /// scrollback intact" path. The mirror terminal is restored from the host
    /// snapshot inside [`WorkspaceSet::attach_in_new_tab`]; here we apply the window's
    /// presentation policy (theme/cursor/scrollback cap) so the attached tab
    /// renders consistently, then switch focus to it. The grid content from the
    /// host snapshot is untouched. The next resize reconciles the mirror to this
    /// window's dimensions (a `Resize` frame to the host). `runtime_base` is
    /// `None` in production (derived from `XDG_RUNTIME_DIR`); tests pass a base.
    #[cfg(unix)]
    pub(in crate::native) fn attach_session_in_new_tab(
        &mut self,
        runtime_base: Option<&Path>,
        session_id: &str,
    ) -> std::io::Result<()> {
        let token = self.sessions.attach_in_new_tab(runtime_base, session_id)?;
        let effective_theme = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let osc52_read = self.settings.osc52_read;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink;
        let cell = self.gpu.as_ref().map(GpuState::cell);
        let scrollback_limit = self.settings.scrollback_limit();
        if let Some(session) = self.sessions.get_mut(token) {
            Self::initialize_session_with(
                session,
                effective_theme,
                themed_ui_roles,
                osc52_read,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
            );
        }
        let _ = self.sessions.switch(token);
        self.on_active_session_changed();
        Ok(())
    }

    /// Windows stub: the detached session-host (Unix-domain socket transport) is
    /// not available, so attach-by-id is rejected cleanly. Callers already treat
    /// an `Err` as "attach failed" (surface a notice / leave panes untouched), so
    /// the overlay-outcome paths and the startup `attach_session` hook stay
    /// well-behaved on Windows without panicking.
    #[cfg(not(unix))]
    pub(in crate::native) fn attach_session_in_new_tab(
        &mut self,
        _runtime_base: Option<&Path>,
        _session_id: &str,
    ) -> std::io::Result<()> {
        Err(std::io::Error::other(
            "resumable sessions are not supported on Windows yet",
        ))
    }

    /// Route an accepted session from the Attach-Session overlay (Phase 14).
    /// Dedup first: if the session is already open in a tab in this window,
    /// switch to that tab — no duplicate, no prompt (this kills the reported
    /// triple-open bug). Otherwise open the attach-choice dialog so the user
    /// picks New tab vs Replace current.
    pub(in crate::native) fn route_attach_session(&mut self, session_id: String) {
        if let Some(token) = self.sessions.find_attached_tab(&session_id) {
            // C5: close the summon overlay before switching. Unlike the
            // not-yet-attached branch below (which re-opens the overlay in
            // AttachChoice mode), this early return left the SessionAttach
            // overlay open==true, so keyboard dispatch kept routing every key
            // into its type-to-filter box instead of the switched-to session
            // until Esc was pressed.
            self.overlay.close();
            if self.sessions.switch(token) {
                self.on_active_session_changed();
            }
            return;
        }
        self.overlay.open_attach_choice(session_id);
    }

    /// Attach `session_id` in a new tab and then close the tab that was active
    /// when the Attach manager opened — the "Replace current" choice (Phase 14).
    /// Opening an overlay does not change the active session, so `active_id()`
    /// captured here is the correct replace target. Order: capture old active →
    /// attach new (appends + focuses it) → close the old tab via the existing
    /// whole-tab close path. That path routes each session through
    /// `Session::close`, which cleanly `Detach`es a hosted/attached tab (the host
    /// keeps the PTY, so it stays reattachable) and closes a local PTY tab
    /// directly — no nested confirm-close dialog, since the user explicitly chose
    /// Replace. A stale id (nothing attached) leaves the current tab untouched.
    fn attach_session_replacing_current(&mut self, session_id: String) {
        let replace_target = self.sessions.active_id();
        if self.attach_session_in_new_tab(None, &session_id).is_err() {
            return;
        }
        if let Some(tab_idx) = self.sessions.position_of_token(replace_target) {
            let _ = self.sessions.close_tab_at(tab_idx);
        }
        // A surviving single-pane tab may return input to the plain fast path;
        // clear any pending multiplexer prefix so stale state can't swallow keys
        // (mirrors `close_active_tab`).
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.on_active_session_changed();
    }

    fn switch_to_next_tab(&mut self) {
        if self.sessions.next() {
            self.flash_rail_autohide();
            self.on_active_session_changed();
        }
    }

    fn switch_to_prev_tab(&mut self) {
        if self.sessions.prev() {
            self.flash_rail_autohide();
            self.on_active_session_changed();
        }
    }

    fn close_active_tab(&mut self) -> bool {
        // "Close Tab" reaps the ENTIRE active tab — every leaf session in its
        // layout tree — and removes the tab, regardless of pane count. This is
        // distinct from "Close Pane" (`close_focused_pane`), which collapses a
        // single leaf and keeps a multi-pane tab alive (Director, explicit).
        //
        // Exit keys on the last tab of the LAST workspace, never on the last
        // pane: closing the sole tab of the sole workspace — even a multi-pane
        // one — signals app exit. We guard on that case first and return without
        // reaping, preserving the existing shutdown path exactly (the app tears
        // down sessions on exit; reaping here would empty the `WorkspaceSet` and
        // make any `active()` Deref before exit panic). With a single workspace
        // this is byte-identical to the old last-close path.
        //
        // Closing the last tab of a NON-last workspace instead closes that
        // workspace and switches to a neighbor (`WorkspaceSet::close_active_tab`);
        // that is not app exit, so it falls through to the reap branch below.
        if self.sessions.tab_count() <= 1 && self.sessions.workspace_count() <= 1 {
            self.pending_exit = true;
            return true;
        }
        // Another tab in this workspace, or another workspace, survives: reap the
        // whole active tab (every pane), removing the workspace too if it was its
        // last tab.
        let _ = self.sessions.close_active_tab();
        // F4-P3: a close chord flashes the auto-hidden rail so the dropped tab is
        // visible even with the pointer away from the edge.
        self.flash_rail_autohide();
        // Switching to a surviving tab may return the input path to the plain
        // single-pane fast path; clear any pending multiplexer prefix so a
        // stale state can't swallow the next key.
        if self.sessions.active_is_single_pane() {
            self.prefix_engine.cancel();
        }
        self.on_active_session_changed();
        false
    }

    /// Dispatch a multiplexer pane action resolved on the prefix (§7, K2). The
    /// prefix engine only ever returns pane actions here; the catch-all is for
    /// exhaustiveness. Each op routes onto the `WorkspaceSet` pane methods built in
    /// Phase 1c, then reflows pane geometry and repaints as needed.
    pub(super) fn apply_pane_action(&mut self, action: BindableAction) {
        match action {
            BindableAction::SplitColumns => self.split_active_pane(SplitAxis::Columns),
            BindableAction::SplitRows => self.split_active_pane(SplitAxis::Rows),
            BindableAction::FocusPaneLeft => self.focus_pane_dir(FocusDir::Left),
            BindableAction::FocusPaneRight => self.focus_pane_dir(FocusDir::Right),
            BindableAction::FocusPaneUp => self.focus_pane_dir(FocusDir::Up),
            BindableAction::FocusPaneDown => self.focus_pane_dir(FocusDir::Down),
            BindableAction::FocusPaneNext => {
                if self.sessions.focus_next_pane() {
                    self.on_active_session_changed();
                }
            }
            BindableAction::ClosePane => self.close_focused_pane(),
            BindableAction::EqualizePanes => {
                self.sessions.equalize_active();
                // Equalize changes split ratios, so each pane's cell dimensions
                // change — reflow before repaint.
                self.reflow_active_panes_and_redraw();
            }
            BindableAction::ZoomPane => {
                // Zoom / toggle-fullscreen-pane (tmux `Ctrl-b z`). Flips the
                // active tab's zoom flag (a no-op on a single-pane tab) without
                // mutating the layout tree, then reflows: the focused pane sizes
                // to the full content rect on zoom and back to its split sub-rect
                // on un-zoom, and the render path draws only that pane full-bleed
                // with no dividers (see `rebuild_multipane`).
                let toggled = self.sessions.toggle_active_zoom();
                if toggled {
                    self.reflow_active_panes_and_redraw();
                }
            }
            // The prefix engine only returns pane actions; other variants never
            // reach here.
            _ => {}
        }
    }

    /// Split the focused pane along `axis` (tmux `Ctrl-b %` / `"`). Spawns and
    /// initializes a new session for the new pane, then reflows every pane to
    /// its new sub-rect and repaints.
    fn split_active_pane(&mut self, axis: SplitAxis) {
        let Ok(new_token) = self.sessions.split_active(axis, self.grid) else {
            return;
        };
        let effective_theme = self.effective_theme;
        let themed_ui_roles = self.themed_ui_roles;
        let osc52_read = self.settings.osc52_read;
        let cursor_style = self.settings.cursor_style;
        let cursor_blink = self.settings.cursor_blink;
        let cell = self.gpu.as_ref().map(GpuState::cell);
        let scrollback_limit = self.settings.scrollback_limit();
        if let Some(session) = self.sessions.get_mut(new_token) {
            Self::initialize_session_with(
                session,
                effective_theme,
                themed_ui_roles,
                osc52_read,
                cursor_style,
                cursor_blink,
                cell,
                scrollback_limit,
            );
        }
        self.reflow_active_panes_and_redraw();
    }

    /// Directional pane focus (tmux `Ctrl-b` arrows). A no-op in a single-pane
    /// tab (`multipane_geometry` is `None`), so the single-pane path is
    /// unaffected.
    fn focus_pane_dir(&mut self, dir: FocusDir) {
        if let Some((content, _cell)) = self.multipane_geometry()
            && self
                .sessions
                .focus_move_active(content, PANE_DIVIDER_PX, dir)
        {
            self.on_active_session_changed();
        }
    }

    /// Close the focused pane (tmux `Ctrl-b x`). Collapses the split into its
    /// sibling; closing the last pane of the last tab exits, mirroring
    /// [`Self::close_active_tab`].
    fn close_focused_pane(&mut self) {
        let focused = self.sessions.active_id();
        if self.sessions.close(focused) {
            self.pending_exit = true;
        } else {
            // If closing collapsed the active tab back to a single pane, cancel
            // any pending multiplexer prefix. Once single-pane, the prefix
            // engine is gated out of the input path (byte-identical), so a
            // stale pending state must not linger to swallow the next key. The
            // safe, least-surprising boundary: dropping to one pane returns the
            // tab to the plain single-pane input path immediately.
            if self.sessions.active_is_single_pane() {
                self.prefix_engine.cancel();
            }
            self.reflow_active_panes_and_redraw();
        }
    }

    /// Reflow every pane of the active tab to its laid-out sub-rect (after a
    /// structural change: split, close, equalize) and request a repaint. A
    /// single-pane tab resizes its lone pane to the full content rect, matching
    /// the window-resize path.
    fn reflow_active_panes_and_redraw(&mut self) {
        if let Some((content, cell)) = self.multipane_geometry() {
            self.sessions
                .resize_all_panes(content, cell.width, cell.height, PANE_DIVIDER_PX);
        } else if let (Some(cell), Some((width_px, height_px, padding))) =
            (self.resolved_cell(), self.resolved_surface())
        {
            // Collapsed back to a single pane (the common case: closing one half
            // of a split). `multipane_geometry()` returns `None` once the tab is
            // single-pane, so the branch above is skipped — without this arm the
            // lone survivor keeps the narrow sub-grid it had as a split pane, and
            // text wrapping + selection stay clipped to the old half-width until
            // the next real window resize.
            //
            // Resize the survivor to the full content rect explicitly:
            // `resize_all_panes` over the full content sizes the tab's lone leaf
            // to the full grid (its single-pane arm). We can't lean on
            // `resize_grid_with_padding` alone here — `self.grid` is only ever the
            // *window* content grid, so it is already full at close time and that
            // call early-returns a no-op without ever resizing the (narrow)
            // survivor session. We still call it afterward to keep `self.grid`
            // current (wrapping + selection read it); it no-ops when unchanged,
            // so a genuinely single-pane tab is byte-identical here.
            let content = pane_content_rect(width_px, height_px, cell, padding, self.tab_reserve());
            self.sessions
                .resize_all_panes(content, cell.width, cell.height, PANE_DIVIDER_PX);
            let _ = self.resize_grid_with_padding(cell, padding, width_px, height_px);
        }
        self.on_active_session_changed();
    }

    pub(super) fn close_all_sessions(&mut self) {
        while !self.sessions.is_empty() {
            let Some(token) = self.sessions.token_at_position(0) else {
                break;
            };
            let _ = self.sessions.close(token);
        }
    }

    /// Pure computation of the next timer wake instant: the minimum over every
    /// scheduled wake source, or `None` when nothing is pending (the zero-wake
    /// idle case → `ControlFlow::Wait`). Split out from
    /// [`Self::update_control_flow_deadline`] so it is testable without an
    /// `ActiveEventLoop` (which cannot be constructed in a unit test). The
    /// caller maps `Some`/`None` onto `WaitUntil`/`Wait`.
    fn next_wake_deadline(&self) -> Option<Instant> {
        [
            self.deadline,
            self.resize_debounce.deadline(),
            // BLACK-SCREEN-ON-RESTORE: bounded retry for a transiently-skipped
            // frame. `None` at rest, so the idle wake set is unchanged; when a
            // frame was skipped this wakes the loop to repaint the recovered
            // surface instead of leaving it black until an unrelated event.
            self.skipped_frame_retry_deadline,
            // §7: wake when a pending multiplexer prefix times out, so the
            // pending state clears promptly even with no further input. `None`
            // (the at-rest case) leaves the min unchanged.
            self.prefix_engine.pending_deadline(),
            // NF20-B: the cursor blink of the ACTIVE pane only. `self.cursor_blink`
            // Derefs to the active session — the SAME pane the maintenance
            // consumer (`self.cursor_blink.is_due`) and the frame poll advance.
            // A background pane is never rendered, so its blink is never polled;
            // sourcing its stale deadline here (as the old `sessions.iter()` did)
            // left a wake with no consumer → `WaitUntil(<past>)` busy-spin after a
            // tab switch. Background panes are parked in maintenance, so this
            // active-only source is the whole live set.
            self.cursor_blink.deadline(),
            // Config-file live-reload poll. Only schedule its timer wake while
            // the window is focused: a backgrounded terminal that nobody is
            // editing config *and watching* has no reason to stat the file once
            // a second, so suppressing this drops idle-unfocused self-wakes to
            // zero (the only remaining wake source at rest). Edits made while
            // away still apply on focus regain — the `Focused(true)` redraw
            // walks `run_about_to_wait_maintenance` -> `poll_config_reload`,
            // which fires immediately because `next_poll` is by then in the
            // past — so live reload stays correct, it just defers the stat to
            // the moment you look at the window again.
            self.focused
                .then(|| self.settings_reloader.deadline())
                .flatten(),
            // NF20-B: the synchronized-output hold of the ACTIVE pane only, for
            // the same fan-out reason as the blink above. The maintenance
            // consumer (`self.synchronized_output_hold.is_due`) advances the
            // active pane; background panes are parked, so an active-only source
            // matches the consumer and cannot strand a stale hold in the wake set.
            self.synchronized_output_hold.deadline(),
            // Wave-15b cursor-animation wake source, ACTIVE pane only (NF20-B).
            // `None` at rest (both fields `None`). Sourcing all panes stranded a
            // backgrounded mid-animation deadline with no consumer; the active
            // pane's ease/slide are advanced by the frame path, background panes
            // are parked in maintenance.
            {
                let mut next = self.cursor_ease_deadline;
                if let Some(deadline) = self.cursor_slide_deadline {
                    next = Some(next.map_or(deadline, |current| current.min(deadline)));
                }
                next
            },
            // F4-P3: wake at the next rail auto-hide boundary (show debounce /
            // hide grace / flash expiry). `None` at rest — steady Hidden, or
            // Revealed with the pointer parked — so the idle wake set is
            // unchanged when nothing is animating.
            self.rail_autohide.wake_deadline(Instant::now()),
            // NF21-2: the overlay/scroll/bell/fade animation aggregator
            // (`animation_deadline()` — smooth-scroll glide, bell flash, new-row
            // fade, open-notice + click-hint auto-expiry, and the cursor
            // ease/slide it already folds). This entry was dropped when the
            // multi-session refactor replaced it with the cursor-only fan-out
            // above, stranding those five with a maintenance CONSUMER but no
            // wake SOURCE — they only advanced when an unrelated wake (a blink
            // toggle) happened to fire, so they froze outright when the cursor
            // was steady/unfocused/blink-off. Restored here, gated to the
            // single-pane active render path: that path (the `update_*` calls in
            // the single-pane rebuild) is the ONLY consumer that advances these
            // timers, so — per the NF20-B "a source must not fan wider than its
            // consumer" rule — sourcing a wake while multipane would be a wake
            // with no consumer (a spin). NF21-1/7 restores the multipane
            // advancement and widens this gate. `None` at rest (every
            // contributor `None`), so the idle wake set is unchanged.
            self.sessions
                .active_is_single_pane()
                .then(|| self.animation_deadline())
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn update_control_flow_deadline(&self, event_loop: &ActiveEventLoop) {
        match self.next_wake_deadline() {
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
            // Multiplexer prefix engine (§7, K2). Runs first so that, once a
            // prefix is pending, the next chord resolves against the prefix
            // table before any global chord (Settings/Search/etc.) — tmux
            // semantics: the key after the prefix is a pane command, and an
            // unknown one cancels. The engine is suppressed while an overlay /
            // search / modal is capturing, so those paths stay byte-identical;
            // and when not pending it returns `Inactive` for every non-prefix
            // chord, leaving the entire path below unchanged. Pane-management
            // chords (`%`, arrows, `x`, …) are excluded from the global table,
            // so they never reach the normal dispatch as bare keys.
            //
            // Single-pane gate (byte-identity): the prefix only intercepts once
            // the active tab is actually split (`panes > 1`). On a single-pane
            // tab — the default and overwhelmingly common case — the prefix key
            // (default `Ctrl-b` / `0x02`) and every other key flow straight
            // through to the focused pane's PTY, byte-identical to the pre-§7
            // path: readline `backward-char` still works in a lone shell. The
            // tmux prefix engages the moment the user splits. The disable knob
            // (`ODYTTY_PANE_PREFIX=off`) and the nested-multiplexer
            // `Ctrl-b Ctrl-b` passthrough are unchanged for multi-pane tabs.
            // `active_is_single_pane()` is a cheap read on the active tab and is
            // checked first so non-prefix keys on a single pane never touch the
            // engine.
            if !self.sessions.active_is_single_pane()
                && !self.overlay.is_open()
                && !self.search.is_open()
                && self.active_modal() == ActiveModal::None
                // Prefer the shifted logical character for the second key so
                // tmux punctuation chords (`%` = Shift+5, `"` = Shift+') match
                // their stored bindings; fall back to the unshifted base key for
                // `Ctrl+<letter>` second keys and the prefix itself. Passing
                // only `binding_key` (`key_without_modifiers()`) here is the bug
                // that made `%`/`"` silently no-op on hardware.
                && let Some(chord) =
                    prefix_chord_from_winit(&logical, &binding_key, mods, self.super_key)
            {
                match self.prefix_engine.on_chord(chord, Instant::now()) {
                    PrefixOutcome::Inactive => {}
                    PrefixOutcome::Entered => {
                        // Prefix captured; await the second key. Repaint so a
                        // future pending-state affordance can show (none yet).
                        if let Some(window) = self.window.as_ref() {
                            window.request_redraw();
                        }
                        return;
                    }
                    PrefixOutcome::Cancelled => {
                        // Unknown second key (or timed-out prefix that did not
                        // re-enter): swallow it, fire nothing, back to normal.
                        return;
                    }
                    PrefixOutcome::Passthrough => {
                        // Doubled prefix (`Ctrl-b Ctrl-b`) → send the literal
                        // prefix byte (e.g. `0x02`) to the focused pane's PTY so
                        // a multiplexer running *inside* OdyTTY still receives
                        // its own prefix (K3 nested-multiplexer story). Return to
                        // live first, like any keystroke that reaches the shell.
                        let bytes = self.prefix_engine.passthrough_bytes();
                        if !bytes.is_empty() {
                            self.return_to_live();
                            self.write_pty_bytes(&bytes);
                        }
                        return;
                    }
                    PrefixOutcome::Action(action) => {
                        self.apply_pane_action(action);
                        return;
                    }
                }
            }
            let action = self
                .key_bindings
                .action_for(&binding_key, mods, self.super_key);
            // C10 + C22: the Settings/ThemePicker shortcuts sit ABOVE the
            // overlay-open guard so they can open their overlay from the live
            // terminal. Two guards keep that from misbehaving:
            //  - `!is_capturing_chord()` (C10): while the key-remap UI is armed
            //    to capture a chord, let the chord fall through to
            //    `handle_overlay_key` so Ctrl+Shift+, / Ctrl+Shift+H can be
            //    *assigned* to an action instead of pre-empting capture. The
            //    normal open/close toggle is unaffected — capture is only armed
            //    on a remap row.
            //  - `Press`-only (C22): a held chord auto-repeats; firing the
            //    toggle on every Repeat open/close-flickered the overlay. Act on
            //    the initial Press only; Repeats fall through (to the overlay
            //    key path once it is open) and are harmless.
            if event_type == KeyEventType::Press && !self.overlay.is_capturing_chord() {
                if action == Some(BindableAction::SettingsPanel) {
                    self.toggle_settings_overlay();
                    return;
                }
                if action == Some(BindableAction::ThemePicker) {
                    self.open_theme_picker_overlay();
                    return;
                }
            }
            if self.overlay.is_open() {
                self.handle_overlay_key(&logical, event_type);
                return;
            }
            if action == Some(BindableAction::CommandPalette) {
                self.open_command_palette_overlay();
                return;
            }
            if action == Some(BindableAction::SessionReplay) {
                self.open_replay_overlay();
                return;
            }
            if action == Some(BindableAction::ConnectionManager) {
                self.open_connection_overlay();
                return;
            }
            if action == Some(BindableAction::SessionAttach) {
                self.open_session_attach_overlay();
                return;
            }
            if action == Some(BindableAction::ThemeBuilder) {
                self.open_theme_builder_overlay();
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
                Some(BindableAction::NewWindow) => {
                    self.handle_new_window();
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
                | Some(BindableAction::CommandPalette)
                | Some(BindableAction::SessionReplay)
                | Some(BindableAction::ConnectionManager)
                | Some(BindableAction::SessionAttach)
                | Some(BindableAction::ThemeBuilder)
                | Some(BindableAction::SettingsPanel)
                | Some(BindableAction::ThemePicker)
                | None => {}
                // Direct split chords (GUI, Ctrl+Shift+E / Ctrl+Shift+O). These
                // two *creation* splits have direct global bindings so the first
                // split on a single-pane tab is reachable without the prefix
                // (which is gated off at single-pane for byte-identity). They
                // dispatch the same action the prefix `%`/`"` path fires, and
                // work at single-pane (create the first split) and multi-pane.
                Some(action @ (BindableAction::SplitColumns | BindableAction::SplitRows)) => {
                    self.apply_pane_action(action);
                    return;
                }
                // The remaining pane-management actions (§7) resolve only on the
                // multiplexer prefix and are excluded from the flat global
                // binding table (`is_pane_action`), so `action_for` never
                // returns one here. These arms exist for match exhaustiveness;
                // the prefix engine (K2) dispatches them before this flat match.
                Some(BindableAction::FocusPaneLeft)
                | Some(BindableAction::FocusPaneRight)
                | Some(BindableAction::FocusPaneUp)
                | Some(BindableAction::FocusPaneDown)
                | Some(BindableAction::FocusPaneNext)
                | Some(BindableAction::ClosePane)
                | Some(BindableAction::ZoomPane)
                | Some(BindableAction::EqualizePanes) => {}
            }
            // SMART-CTRLC: a plain Ctrl+C that matched no binding copies + clears
            // a local selection when the copy-or-interrupt policy is on, then
            // swallows the chord. With the policy off, no selection, or any other
            // key it returns false and falls through to the interrupt-byte encode
            // below, so the ^C path stays byte-identical. Inside the press-only
            // guard, so a key release never triggers a copy.
            if self.smart_ctrl_c_intercept(&logical, mods) {
                return;
            }
            // SELDEL-KEY: a plain Delete/Backspace with a local selection on the
            // editable prompt line deletes that selection through the same gated,
            // shell-integration-aware path as the right-click Delete/Cut, then
            // swallows the key. If a selection exists but no OSC 133 input
            // boundary is known, consume the key, clear the stale visual
            // selection, and show the shell-integration hint instead of sending
            // blind edit bytes. With no selection, or with a selection that is
            // not on editable input despite a known boundary, Delete/Backspace
            // still falls through to the shell. Gated to no Ctrl/Alt/Super so
            // word-delete chords (Ctrl+W, Alt+Backspace) still reach the shell.
            // Press-only via the enclosing guard.
            if is_selection_delete_key(&logical)
                && !mods.ctrl
                && !mods.alt
                && !self.super_key
                && (self.try_delete_selected_editable_input()
                    || self.try_handle_unavailable_selection_delete())
            {
                return;
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
        // ABOUT: refresh the About data with the live GPU adapter before the
        // panel opens. Cheap to recompute; the adapter is present once the
        // renderer is up (`None` only on the headless/early path).
        let adapter = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.adapter_diagnostics().clone());
        self.overlay
            .set_about_info(crate::native::about::AboutInfo::collect(adapter));
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

    /// SMART-CTRLC: handle a plain `Ctrl+C` under the copy-or-interrupt policy.
    ///
    /// Returns `true` (the chord was consumed) only when the policy is active,
    /// the chord is *plain* `Ctrl+C` (Ctrl held; no Shift/Alt/Super — so the
    /// `Ctrl+Shift+C` copy binding, handled earlier, never reaches here), and a
    /// local OdyTTY selection exists: it copies the selection and clears it so an
    /// immediate second `Ctrl+C` interrupts. In every other case it returns
    /// `false` and the caller falls through to the normal interrupt-byte encode,
    /// so the `^C` path is byte-identical when the policy is off, when nothing is
    /// selected, or for any non-`Ctrl+C` key. Press-only by virtue of the
    /// enclosing `event_type != Release` guard at the call site.
    fn smart_ctrl_c_intercept(&mut self, logical: &WinitKey, mods: Modifiers) -> bool {
        if !self.settings.smart_ctrl_c.is_active() {
            return false;
        }
        if !mods.ctrl || mods.shift || mods.alt || self.super_key {
            return false;
        }
        if !is_ctrl_c_key(logical) {
            return false;
        }
        if self.selection.range().is_none() {
            return false;
        }
        self.handle_copy_shortcut();
        self.selection.clear();
        self.selection_block = false;
        self.request_selection_redraw();
        true
    }

    /// Open the right-click context menu (IN2) at the cached pointer cell, with
    /// Copy enabled iff a selection exists and Paste enabled iff the clipboard
    /// holds text — the per-item gating snapshot the menu renders. Deliberately
    /// does NOT call `reset_pointer_state_for_overlay`: that would clear the
    /// selection the Copy item needs. No pointer cell (e.g. before the first
    /// move) means no menu.
    pub(super) fn open_context_menu(&mut self, rename_target: Option<SessionToken>) {
        // Window-overlay cell space: in a single-pane tab this is exactly
        // `self.pointer_cell`; in a multi-pane tab it maps the pointer into the
        // whole content grid so the menu spawns where it renders (and clicks
        // land), not in the focused pane's sub-grid.
        let Some(spawn) = self.overlay_pointer_cell() else {
            return;
        };
        let copy_enabled = self.selection.range().is_some();
        let editable_selection = self.editable_input_selection_for_context_menu();
        let prompt_editing_hint =
            editable_selection.is_none() && self.prompt_input_mark_missing_for_context_menu();
        let paste_enabled = self.clipboard.read_text().is_some();
        // Part C: each item's *effective* keybind, derived from the live
        // `KeyBindings` (reverse action→chord lookup) so it reflects user
        // rebinds. Items with no bound chord get `None` (rendered blank). Reuses
        // `format_key_chord` for the chord decomposition; `humanize_chord` only
        // title-cases the tokens for display.
        let mut accelerators = super::context_menu_ui::ContextMenuItem::ALL.map(|item| {
            item.bindable_action()
                .and_then(|action| self.key_bindings.chord_for_action(action))
                .map(|chord| {
                    super::context_menu_ui::humanize_chord(crate::settings::format_key_chord(chord))
                })
        });
        // Close Pane is shown only in a multi-pane tab, and its chord lives in
        // the multiplexer prefix table (`Ctrl-b x`), not the flat global table —
        // so its accelerator is composed here from the prefix engine rather than
        // the generic `bindable_action` → `chord_for_action` path above.
        let multi_pane = !self.sessions.active_is_single_pane();
        if multi_pane
            && let Some(label) = self.close_pane_accelerator()
            && let Some(slot) = super::context_menu_ui::ContextMenuItem::ALL
                .iter()
                .position(|item| *item == super::context_menu_ui::ContextMenuItem::ClosePane)
        {
            accelerators[slot] = Some(label);
        }
        // C3: re-detect the interactive path at the click cell (do NOT reuse the
        // hover snapshot — a right-click may not pass through the hover path).
        // Gated on the setting so the default (feature-off) menu never scans and
        // is byte-identical. `None` hides the file section entirely.
        let path_target = if self.settings.interactive_paths {
            self.resolved_hovered_path()
        } else {
            None
        };
        self.overlay.open_context_menu_with_prompt_editing_hint(
            spawn,
            copy_enabled,
            editable_selection.is_some(),
            paste_enabled,
            editable_selection.is_some(),
            prompt_editing_hint,
            rename_target,
            multi_pane,
            path_target,
            accelerators,
        );
        self.request_selection_redraw();
    }

    /// The human-readable accelerator label for the context menu's Close Pane
    /// item: the multiplexer prefix chord followed by the prefix-table key bound
    /// to `ClosePane` (e.g. `Ctrl+B X` for the tmux `Ctrl-b x` default). `None`
    /// when the prefix is disabled (`ODYTTY_PANE_PREFIX=off`) or `ClosePane` has
    /// no prefix binding — the menu then renders the item with a blank
    /// accelerator. Reuses the same `format_key_chord` + `humanize_chord` pair
    /// the flat-table accelerators use, so the styling matches.
    fn close_pane_accelerator(&self) -> Option<String> {
        let prefix = self.prefix_engine.prefix()?;
        let second = self
            .prefix_engine
            .chord_for_action(crate::settings::BindableAction::ClosePane)?;
        let prefix_label =
            super::context_menu_ui::humanize_chord(crate::settings::format_key_chord(prefix));
        let second_label =
            super::context_menu_ui::humanize_chord(crate::settings::format_key_chord(second));
        Some(format!("{prefix_label} {second_label}"))
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
            // P0-3: OSC-title event path — poison-recover, never abort.
            let mut terminal = crate::native::lock_recover(&self.terminal);
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
        // ≥2 tabs always shows the bar; otherwise honor the opt-in
        // `always_show_tab_bar` setting, and show a lone tab when it carries a
        // custom name so a named single "workflow" tab is visible (F4 ODP-7 /
        // F4-NF1).
        self.sessions.tab_count() >= 2
            || self.settings.always_show_tab_bar
            || self.sessions.lone_tab_has_title_override()
    }

    /// The theme-role colors the tab bar paints with (F4). Reads
    /// `effective_theme` so every color is CVD-adapted like the rest of the
    /// chrome; nothing is hardcoded.
    fn tab_bar_colors(&self) -> tab_bar::TabBarColors {
        tab_bar::TabBarColors {
            foreground: self.effective_theme.foreground,
            background: self.effective_theme.background,
            inactive: self.effective_theme.inactive,
            active_bg: self.effective_theme.selection,
        }
    }

    fn tab_bar_height_px(&self, cell: CellSize) -> f32 {
        cell.height as f32 * self.tab_reserve().top_rows as f32
    }

    /// The placement actually honored by the render path this frame. All three
    /// placements now render (F4-P2 landed the right rail), so
    /// [`TabBarPlacement::effective`] is an identity; the indirection is kept as
    /// the single seam the render/reserve paths read.
    fn effective_placement(&self) -> TabBarPlacement {
        self.settings.tab_bar_placement.effective()
    }

    /// The tab-chrome reservation for the current frame: rows off the top (the
    /// horizontal bar) or columns off a side (the vertical rail). `NONE` when the
    /// bar is hidden — the byte-identical no-chrome case.
    fn tab_reserve(&self) -> panes::TabReserve {
        if !self.should_show_tab_bar() {
            return panes::TabReserve::NONE;
        }
        // F4-P3: under rail auto-hide the rail reserves NOTHING — it draws as a
        // floating overlay when revealed (never reflows content). The single
        // reflow the operator sees is exactly this reservation dropping to zero
        // when autohide is toggled on; reveal/hide after that never touches the
        // reserve. Gated on a side rail (the top bar keeps `always_show_tab_bar`
        // semantics), so the top-bar path is unchanged.
        if self.rail_autohide_active() {
            return panes::TabReserve::NONE;
        }
        let gap_cols = self.rail_gap_cols();
        // F4-P1/P4: the rail band width resolves the `tab_rail_width` mode —
        // `Manual(cols)` clamps the fixed width, `Auto` sizes to the longest tab
        // title (`rail_auto_want_cols`) clamped to the auto max.
        let rail_cols = self.settings.rail_width_cols(self.rail_auto_want_cols());
        match self.effective_placement() {
            TabBarPlacement::Top => panes::TabReserve::top(),
            TabBarPlacement::Left => panes::TabReserve {
                top_rows: 0,
                left_cols: rail_cols,
                right_cols: 0,
                gap_cols,
            },
            // F4-P2: the right rail reserves its band + gap off the RIGHT; the
            // content stays at column 0 (mirror of the left arm).
            TabBarPlacement::Right => panes::TabReserve {
                top_rows: 0,
                left_cols: 0,
                right_cols: rail_cols,
                gap_cols,
            },
        }
    }

    /// The live rail slot geometry (F4-P1 knobs: `tab_rail_slot_rows`,
    /// `tab_rail_gap`), passed to the rail widget's render/hit-test.
    fn rail_geom(&self) -> tab_rail::RailGeom {
        tab_rail::RailGeom {
            slot_rows: self.settings.rail_slot_rows(),
            slot_gap: self.settings.rail_slot_gap_rows(),
        }
    }

    /// The longest tab title in cells (F4-P4 auto-width): each Unicode scalar
    /// counts as one column, matching the rail widget's `truncate_label` (the
    /// wide-glyph display-width caveat is F4P-NF1, out of scope). Trimmed like
    /// the widget so trailing spaces never pad the auto width.
    fn rail_longest_title_cols(&self) -> usize {
        use tab_bar::TabBarSource;
        (0..self.sessions.tab_count())
            .map(|idx| self.sessions.tab_title(idx).trim().chars().count())
            .max()
            .unwrap_or(0)
    }

    /// The rail width (cells) `Auto` mode wants: the longest title plus the
    /// widget's label chrome (F4-P4). `Settings::rail_width_cols` clamps it to
    /// the auto max; in `Manual` mode this is ignored.
    fn rail_auto_want_cols(&self) -> usize {
        self.rail_longest_title_cols() + tab_rail::RAIL_LABEL_CHROME_COLS
    }

    /// F4-P4 auto-width reconcile: when the resolved rail band width diverges
    /// from what the content grid was last reserved against — a tab added or
    /// closed, a title renamed, or a shell-set (OSC 0/2) title changing the
    /// longest title — reflow the grid once so the content matches the new rail
    /// width. Gated on the width actually changing, so a stable frame is a
    /// single `usize` comparison; a no-rail / manual-width frame never diverges.
    /// Run once per redraw before the frame is built, so the rail and content
    /// stay pixel-aligned within the frame.
    fn reconcile_rail_auto_width(&mut self) {
        if self.gpu.is_none() || self.window.is_none() {
            return;
        }
        if self.rail_cols() != self.rail_reserved_cols {
            // `recompute_grid_for_tab_bar` refreshes `rail_reserved_cols`, so a
            // no-change follow-up frame won't reflow again.
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
        }
    }

    /// The live unified-panel strength (F4-P1 `tab_panel_strength`), passed to
    /// both tab-chrome widgets for the resting-cell tint and used to build the
    /// panel wash/seam background quads.
    fn tab_panel_strength(&self) -> f32 {
        self.settings.tab_panel_strength
    }

    /// Build the F4-P1 unified-panel background quads (ODP-1 wash + ODP-2 seam)
    /// for the current frame, in surface pixels. Empty when the bar is hidden,
    /// the GPU is not up yet, or the band is degenerate; the caller splices these
    /// into the GPU background segment (after the NF11 edge wash). The panel wash
    /// is emitted only when `p = strength × (1 − cell_bg_opacity) > 0`; the seam
    /// only when the seam knob is on AND the panel is live (`strength > 0`).
    fn tab_panel_bg_quads(&self, cell: CellSize) -> Vec<SolidQuad> {
        if !self.should_show_tab_bar() {
            return Vec::new();
        }
        let Some(gpu) = self.gpu.as_ref() else {
            return Vec::new();
        };
        let (axis, band_cells) = match self.effective_placement() {
            TabBarPlacement::Top => (tab_panel::PanelAxis::Top, TAB_BAR_ROWS as usize),
            TabBarPlacement::Left => (tab_panel::PanelAxis::Left, self.rail_cols()),
            TabBarPlacement::Right => (tab_panel::PanelAxis::Right, self.rail_cols()),
        };
        if band_cells == 0 {
            return Vec::new();
        }
        // For a right rail the seam must sit at the rail's grid-aligned content
        // edge (content columns + the wallpaper gap, both left of the band); it
        // is ignored for the top bar / left rail. Same basis as `rail_origin_px`
        // and the reserve/decorate paths, so wash, seam, glyphs, and hit-test all
        // agree to the pixel.
        let lead_cells = match axis {
            tab_panel::PanelAxis::Right => self.grid.columns + self.rail_gap_cols(),
            _ => 0,
        };
        let (surface_w, surface_h) = gpu.surface_size();
        let padding = gpu.window_padding();
        let strength = self.tab_panel_strength();
        let colors = self.tab_bar_colors();
        let panel_color = tab_chrome::panel_tint(colors, strength);
        let wash_alpha = tab_chrome::panel_wash_alpha(strength, self.settings.cell_bg_opacity);
        let seam = (self.settings.tab_seam && strength > 0.0)
            .then(|| tab_chrome::seam_color(colors, panel_color));
        let spec = tab_panel::PanelQuadSpec {
            axis,
            surface: [surface_w as f32, surface_h as f32],
            pad: [padding.as_f32(), padding.as_f32()],
            cell: [cell.width as f32, cell.height as f32],
            band_cells,
            lead_cells,
            scale_factor: gpu.scale(),
            panel_color,
            wash_alpha,
            seam,
            seam_alpha: tab_chrome::SEAM_ALPHA,
        };
        tab_panel::panel_quads(&spec)
    }

    /// The cell-quantized wallpaper gap (in columns) between a side rail and the
    /// content this frame — `ceil(window_pad / cell_w)`, replicating the padding
    /// band (R1.1). 0 before the GPU exists (no cell metrics yet).
    fn rail_gap_cols(&self) -> usize {
        let Some(gpu) = self.gpu.as_ref() else {
            return 0;
        };
        panes::rail_content_gap_cols(gpu.window_padding(), gpu.cell())
    }

    /// The rail band width in cells when a vertical rail is active this frame,
    /// else 0. This is the rail widget's VISUAL width — it excludes the
    /// rail↔content wallpaper gap, which is reserved separately (R1.1).
    fn rail_cols(&self) -> usize {
        let r = self.tab_reserve();
        r.left_cols + r.right_cols
    }

    /// Which side the rail occupies this frame, or `None` when no rail is active
    /// (top bar or hidden).
    fn rail_side(&self) -> Option<RailSide> {
        let r = self.tab_reserve();
        if r.left_cols > 0 {
            Some(RailSide::Left)
        } else if r.right_cols > 0 {
            Some(RailSide::Right)
        } else {
            None
        }
    }

    /// The physical-pixel top-left of the rail band this frame — the origin the
    /// rail widget's hit-test maps against and the multi-pane strip renders from.
    /// A left rail (and the byte-identical no-rail case) sits at the window
    /// padding `[pad, pad]`; a right rail sits at the far side, after the content
    /// columns and the wallpaper gap: `pad + (content_cols + gap)·cell_w`. This
    /// is the same grid basis the reserve/decorate/panel-seam paths use, so the
    /// rail's glyphs, seam, and click targets stay pixel-aligned (F4-P2).
    fn rail_origin_px(&self, cell: CellSize) -> [f32; 2] {
        let pad = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO)
            .as_f32();
        let x = match self.rail_side() {
            Some(RailSide::Right) => {
                pad + (self.grid.columns + self.rail_gap_cols()) as f32 * cell.width as f32
            }
            _ => pad,
        };
        [x, pad]
    }

    /// The physical-pixel X of the rail's inner (content-facing) seam this frame,
    /// or `None` when no rail is active (F4-P4). A left rail's seam is the RIGHT
    /// edge of its band (`origin_x + rail_cols·cell_w`); a right rail's seam is
    /// the LEFT edge of its band (`origin_x`). This is the edge the drag-resize
    /// grabs and the resize cursor tracks.
    fn rail_seam_x_px(&self, cell: CellSize) -> Option<f32> {
        let origin_x = self.rail_origin_px(cell)[0];
        match self.rail_side()? {
            RailSide::Left => Some(origin_x + self.rail_cols() as f32 * cell.width as f32),
            RailSide::Right => Some(origin_x),
        }
    }

    /// The manual rail width (cells) a seam-drag pointer at `px_x` maps to
    /// (F4-P4). Gathers the pixel geometry (padding, surface width) from the GPU
    /// where present — 0 defaults keep the left rail (which needs neither) usable
    /// headlessly for tests — and defers the pure snap/clamp math to
    /// [`rail_width_cols_from_pointer`].
    fn rail_width_from_pointer(&self, px_x: f64, cell: CellSize) -> Option<u16> {
        let side = self.rail_side()?;
        let pad = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO)
            .as_f32();
        let surface_w = self
            .gpu
            .as_ref()
            .map(|gpu| gpu.surface_size().0 as f32)
            .unwrap_or(0.0);
        Some(rail_width_cols_from_pointer(
            side,
            px_x as f32,
            pad,
            cell.width as f32,
            surface_w,
            MIN_TAB_RAIL_WIDTH as u16,
            MAX_TAB_RAIL_WIDTH as u16,
        ))
    }

    /// Whether the pointer at raw `px_x` is within the seam grab band this frame
    /// and should start / show a rail resize rather than a tab hit (F4-P4).
    /// Yields to a live scroll thumb (ODP-5 right-rail rule) so a scrollbar drag
    /// wins the shared edge. `false` off a rail, so the plain path never grabs.
    fn pointer_over_rail_seam(&self, px_x: f64, cell: CellSize) -> bool {
        if self.rail_side().is_none() || !self.should_show_tab_bar() {
            return false;
        }
        let Some(seam_x) = self.rail_seam_x_px(cell) else {
            return false;
        };
        if (px_x as f32 - seam_x).abs() > DIVIDER_GRAB_PX {
            return false;
        }
        // Yield the shared edge to a grabbable scroll thumb (right rail: the
        // content scrollbar sits just inside the seam).
        !(self.settings.scrollbar_drag && self.scrollbar_hit_test().is_some())
    }

    /// F4-P4: drive an in-progress rail seam drag to the pointer — set the manual
    /// width the pointer maps to and reflow the content grid. Resets the seam
    /// click tracker on an actual move so a drag-then-grab is never misread as a
    /// double-click (reset-to-auto).
    fn drag_rail_seam_to_pointer(&mut self, px_x: f64) {
        let Some(cell) = self.resolved_cell() else {
            return;
        };
        let Some(cols) = self.rail_width_from_pointer(px_x, cell) else {
            return;
        };
        let next = crate::settings::TabRailWidth::Manual(cols);
        if self.settings.tab_rail_width != next {
            self.settings.tab_rail_width = next;
            self.rail_seam_clicks = ClickTracker::default();
            self.recompute_grid_for_tab_bar();
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// F4-P4: set the rail width mode and persist it to `odytty.conf` (drag
    /// release → the dragged `Manual` width; double-click → `Auto`). The live
    /// setting is already applied, so this only writes it through so it survives
    /// a restart; a missing config path or write error is logged, not fatal.
    fn persist_rail_width(&mut self) {
        let value = self.settings.tab_rail_width.as_config_string();
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        let changes = [SettingEdit {
            key: "tab_rail_width",
            env: TAB_RAIL_WIDTH_ENV,
            value,
        }];
        if let Err(error) = write_settings_changes_to_path(path, &changes) {
            tracing::warn!(error = %error, "could not persist tab rail width");
        }
    }

    /// F4-P4: reset the rail to `Auto` width (double-click the seam), reflow, and
    /// persist. A no-op when already `Auto`.
    fn reset_rail_width_to_auto(&mut self) {
        if self.settings.tab_rail_width == crate::settings::TabRailWidth::Auto {
            return;
        }
        self.settings.tab_rail_width = crate::settings::TabRailWidth::Auto;
        self.recompute_grid_for_tab_bar();
        self.persist_rail_width();
        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    // -----------------------------------------------------------------------
    // F4-P3 rail auto-hide (ODP-4)
    // -----------------------------------------------------------------------

    /// Whether rail auto-hide is active this frame: the knob is on, the tab
    /// chrome is shown, AND the placement is a side rail (the top bar keeps
    /// `always_show_tab_bar` semantics — a hidden top bar is already expressible
    /// by turning that off). When active, `tab_reserve` returns `NONE` and the
    /// rail is a floating overlay.
    fn rail_autohide_active(&self) -> bool {
        self.should_show_tab_bar()
            && self.settings.tab_rail_autohide
            && matches!(
                self.effective_placement(),
                TabBarPlacement::Left | TabBarPlacement::Right
            )
    }

    /// The side an auto-hidden rail occupies (independent of `tab_reserve`, which
    /// is `NONE` under autohide). `None` when autohide is inactive.
    fn rail_autohide_side(&self) -> Option<RailSide> {
        if !self.rail_autohide_active() {
            return None;
        }
        match self.effective_placement() {
            TabBarPlacement::Left => Some(RailSide::Left),
            TabBarPlacement::Right => Some(RailSide::Right),
            TabBarPlacement::Top => None,
        }
    }

    /// The width (cells) of the auto-hidden rail overlay band — the same width
    /// the rail would resolve to if pinned (`Manual` clamp or `Auto` from the
    /// longest title), computed independently of the (now zero) reservation.
    fn rail_overlay_cols(&self) -> usize {
        self.settings.rail_width_cols(self.rail_auto_want_cols())
    }

    /// Physical-pixel top-left of the revealed rail overlay band. A left rail
    /// hugs the left padding (`[pad, pad]`); a right rail hugs the right window
    /// edge (`surface_w − pad − band_w`). Unlike the pinned right rail (which is
    /// grid-embedded after the full-width content), the overlay floats at the
    /// window edge — content underneath is already full-width. Surface + padding
    /// come from [`Self::resolved_surface`] so the drawn band, its seam, and the
    /// reveal-zone geometry all read the SAME basis (and are test-injectable).
    fn rail_overlay_origin_px(&self, cell: CellSize, side: RailSide) -> [f32; 2] {
        let (surface_w, pad) = self.reveal_surface_metrics();
        let (surface_w, pad) = (surface_w as f32, pad as f32);
        let band_w = self.rail_overlay_cols() as f32 * cell.width as f32;
        let x = match side {
            RailSide::Left => pad,
            RailSide::Right => (surface_w - pad - band_w).max(pad),
        };
        [x, pad]
    }

    /// The revealed overlay band's content-facing seam x (physical px): the
    /// right edge of a left band, the left edge of a right band.
    fn rail_overlay_seam_x(&self, cell: CellSize, side: RailSide) -> f32 {
        let origin_x = self.rail_overlay_origin_px(cell, side)[0];
        let band_w = self.rail_overlay_cols() as f32 * cell.width as f32;
        match side {
            RailSide::Left => origin_x + band_w,
            RailSide::Right => origin_x,
        }
    }

    /// The live display scale factor (physical px per logical px), or a headless
    /// test override, defaulting to 1.0 before the GPU/window exists. Used to
    /// convert logical-px pointer thresholds into the physical-px space winit's
    /// `CursorMoved` reports in.
    fn effective_scale(&self) -> f32 {
        #[cfg(test)]
        if let Some(scale) = self.test_scale {
            return scale;
        }
        self.gpu.as_ref().map(GpuState::scale).unwrap_or(1.0)
    }

    /// `(surface_w, window_pad)` in **physical** px for the reveal-zone geometry,
    /// via [`Self::resolved_surface`] — the same basis the drawn rail band uses,
    /// and test-injectable through `set_test_surface_for_test` so the reveal
    /// wiring can be exercised at a real scale + padding headlessly. `(0, 0)`
    /// before the GPU / a test surface exists.
    fn reveal_surface_metrics(&self) -> (f64, f64) {
        match self.resolved_surface() {
            Some((w, _h, padding)) => (w as f64, padding.as_f32() as f64),
            None => (0.0, 0.0),
        }
    }

    /// The reveal trigger-zone reach (physical px) inward from the rail's window
    /// edge: the window padding plus the scaled `tab_rail_reveal_px`. Both terms
    /// are physical: the padding is stored physical, and `tab_rail_reveal_px` is
    /// logical so it is scaled by [`Self::effective_scale`] — winit reports the
    /// pointer in physical px, so the whole comparison stays in one space.
    fn reveal_reach_px(&self) -> f64 {
        let reveal_px = self.settings.tab_rail_reveal_px as f64 * self.effective_scale() as f64;
        let (_surface_w, pad) = self.reveal_surface_metrics();
        pad + reveal_px
    }

    /// Whether a raw pointer x is inside the reveal **trigger** zone — an
    /// **interior** band measured from the rail's window edge inward by the
    /// window padding PLUS `tab_rail_reveal_px` (see [`reveal_edge_contains`]).
    fn pointer_in_reveal_edge(&self, px_x: f64, side: RailSide) -> bool {
        let (surface_w, _pad) = self.reveal_surface_metrics();
        reveal_edge_contains(side, px_x, self.reveal_reach_px(), surface_w)
    }

    /// Whether a raw pointer x is inside the reveal **keep-alive** region — the
    /// UNION of the trigger zone and the drawn overlay band, so the rail holds
    /// while the pointer is anywhere over either (see [`reveal_band_contains`]).
    fn pointer_in_reveal_band(&self, px_x: f64, cell: CellSize, side: RailSide) -> bool {
        let seam_x = self.rail_overlay_seam_x(cell, side) as f64;
        let (surface_w, _pad) = self.reveal_surface_metrics();
        reveal_band_contains(side, px_x, seam_x, self.reveal_reach_px(), surface_w)
    }

    /// Reveal the auto-hidden rail for a flash after a keyboard tab action
    /// (ODP-4 SHOULD). Inert (and cheap) unless autohide is active; requests a
    /// redraw and schedules the flash-expiry wake when it takes effect.
    fn flash_rail_autohide(&mut self) {
        if !self.rail_autohide_active() {
            return;
        }
        self.rail_autohide.flash(Instant::now());
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Feed the live pointer to the auto-hide machine and repaint on a
    /// visibility change. Called from the pointer-move path while autohide is
    /// active; also called with `in_edge = in_band = false` when the pointer
    /// leaves the window so a rail revealed at the edge can hide. `now` is the
    /// event time (`Instant::now()` in production; injected in tests so the
    /// reveal → hold → hide sequence is deterministic through the real contact
    /// geometry).
    fn update_rail_autohide_pointer(&mut self, px_x: f64, cell: CellSize, now: Instant) {
        let Some(side) = self.rail_autohide_side() else {
            return;
        };
        // Popup-tracking rule (ODP-4): hold the rail up while an overlay (its
        // right-click context menu) is open — the hide timer is suspended until
        // the menu closes.
        self.rail_autohide.set_suspend(self.overlay.is_open());
        // Motion-aware trigger: fold in the previous sample so the segment
        // prev→curr is tested against the edge zone (a fast approach jumps over a
        // static point zone). Record this sample as the next prev before feeding.
        let prev_px_x = self.last_rail_pointer_px;
        self.last_rail_pointer_px = Some(px_x);
        let (in_edge, in_band) = self.reveal_pointer_contact(px_x, prev_px_x, cell, side);
        // ODYTTY_RAIL_TRACE (operator-runnable): one privacy-clean line per
        // sample — pointer coordinate + reveal phase only, never terminal
        // content. Logs the phase both before and after the sample so a reveal /
        // abort / hide transition is visible in the log the operator hands back.
        let phase_from = self.rail_autohide.phase_name();
        let visible_from = self.rail_autohide.is_visible(now);
        let changed = self.rail_autohide.on_pointer(in_edge, in_band, now);
        if rail_trace_enabled() {
            tracing::warn!(
                target: "odytty::rail_reveal",
                px_x,
                in_edge,
                in_band,
                phase_from,
                phase_to = self.rail_autohide.phase_name(),
                visible_from,
                visible_to = self.rail_autohide.is_visible(now),
                changed,
                "rail reveal pointer sample"
            );
        }
        if changed {
            // A visibility flip must rebuild the frame, not merely re-present it:
            // the rail overlay is only assembled inside the `should_rebuild_frame`
            // gate (`build_rail_overlay`), and that gate reads `needs_rebuild`.
            // Requesting a redraw without marking the frame dirty lets the
            // RedrawRequested skip the rebuild and re-present the previous
            // (rail-less) frame — the reveal then only paints when some unrelated
            // event happens to set `needs_rebuild`, which over a quiescent
            // terminal is "not until the pointer crosses off the window edge".
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// The `(in_edge, in_band)` reveal contact the machine is fed for a raw
    /// pointer x, given the previous sample `prev_px_x` (`None` on the first
    /// sample after entry). Yields to an active scrollbar-thumb drag (ODP-5): a
    /// drag near the edge — the right rail's scrollbar sits just inside the seam —
    /// must not trigger or hold a reveal, so a drag-in-progress reports no
    /// contact.
    ///
    /// `in_edge` is **motion-aware**: the current point in the trigger zone OR
    /// the segment from the previous sample crossing it (see
    /// [`reveal_edge_segment_crosses`]) — natural mouse motion delivers samples
    /// 30–200 px apart, so a fast approach can jump clean over a static point
    /// zone. `in_band` stays a **point** test: the keep-alive / hide-grace logic
    /// needs "is the pointer *now* over the band", not "did the path ever touch
    /// it" (a motion-aware band would never let go once the pointer left).
    fn reveal_pointer_contact(
        &self,
        px_x: f64,
        prev_px_x: Option<f64>,
        cell: CellSize,
        side: RailSide,
    ) -> (bool, bool) {
        if self.pointer_drag.scrollbar_grab().is_some() {
            return (false, false);
        }
        let in_edge = self.pointer_in_reveal_edge(px_x, side)
            || prev_px_x.is_some_and(|prev| {
                let (surface_w, _pad) = self.reveal_surface_metrics();
                reveal_edge_segment_crosses(side, prev, px_x, self.reveal_reach_px(), surface_w)
            });
        (in_edge, self.pointer_in_reveal_band(px_x, cell, side))
    }

    /// Whether the auto-hidden rail overlay is drawn (and hit-tested) this
    /// frame: autohide active AND the state machine currently visible AND no
    /// window overlay is open.
    ///
    /// The last clause is the fix for the "can't right-click Settings" report:
    /// the revealed rail strip is composited *topmost* — over the panes AND over
    /// the `overlay_top` window overlay (context menu / Settings / palette). If
    /// the rail were drawn while a menu is open it would paint over the menu,
    /// hiding items the pointer is trying to click. An open window overlay owns
    /// the screen, so the floating rail steps aside until it closes (the
    /// reveal-machine phase is held via `set_suspend`, so the rail reappears
    /// afterward if the pointer is still near the edge). Hit-testing already
    /// short-circuits to the overlay while it is open, so suppressing the draw
    /// keeps render and hit-test consistent.
    fn rail_overlay_visible(&self) -> bool {
        self.rail_autohide_active()
            && !self.overlay.is_open()
            && self.rail_autohide.is_visible(Instant::now())
    }

    /// Hover the revealed rail overlay from the live pointer, using the overlay
    /// band geometry (window-edge origin, overlay width) rather than the pinned
    /// reservation (which is `NONE` under autohide). Clears any stale top-bar
    /// hover and keeps the default cursor over the band.
    fn update_rail_overlay_hover(&mut self, x_px: f64, y_px: f64, cell: CellSize, side: RailSide) {
        let hit = self.tab_rail.hit_test(
            x_px,
            y_px,
            &self.sessions,
            self.rail_overlay_cols(),
            self.tab_rail_grid_rows(),
            self.rail_overlay_origin_px(cell, side),
            cell,
            self.rail_geom(),
        );
        let hover = (hit != TabHit::None).then_some(hit);
        let mut redraw = false;
        if self.tab_rail.hover != hover {
            self.tab_rail.set_hover(hover);
            redraw = true;
        }
        if self.tab_bar.hover.is_some() {
            self.tab_bar.set_hover(None);
            redraw = true;
        }
        self.apply_cursor_icon(CursorIcon::Default);
        if redraw {
            // Rail hover highlight lives in the overlay signature, which is only
            // recomputed inside the `should_rebuild_frame` gate — mark the frame
            // dirty so the hover repaints over an otherwise-idle terminal.
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    /// Build the F4-P3 revealed rail overlay for this frame: the `rail_cols ×
    /// rows` strip snapshot (rail glyphs + baked panel tint), its window-edge
    /// origin, and the occluding wash + content-facing seam quads. `None` unless
    /// the overlay is currently revealed. The owned snapshot must outlive the GPU
    /// call, so the caller holds this and lends a [`gpu::RailOverlay`] from it.
    fn build_rail_overlay(&self, cell: CellSize) -> Option<RailOverlayData> {
        let side = self.rail_autohide_side()?;
        if !self.rail_overlay_visible() {
            return None;
        }
        let cols = self.rail_overlay_cols();
        let rows = self.tab_rail_grid_rows();
        if cols == 0 || rows == 0 {
            return None;
        }
        let origin = self.rail_overlay_origin_px(cell, side);
        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let output = self.tab_rail.render(
            &self.sessions,
            cols,
            rows,
            [padding.as_f32(), padding.as_f32()],
            cell,
            side,
            self.tab_bar_colors(),
            self.rail_geom(),
            self.tab_panel_strength(),
        );
        let mut snapshot = Snapshot {
            dimensions: Dimensions::new(cols, rows),
            cursor: Position { row: 0, column: 0 },
            cursor_visible: false,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); cols * rows],
        };
        for glyph in output.glyphs {
            if glyph.row < rows && glyph.col < cols {
                snapshot.cells[glyph.row * cols + glyph.col] =
                    crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        let (wash, seam) = self.build_rail_overlay_quads(cell, side);
        Some(RailOverlayData {
            snapshot,
            origin,
            wash,
            seam,
        })
    }

    /// The revealed rail overlay's occluding wash (`p_reveal = max(p, 0.85)`,
    /// near-opaque so live content never bleeds through the floating band) and
    /// its content-facing seam, in surface pixels. Reuses the panel colors +
    /// seam gate; the geometry hugs the window edge (not grid-embedded), so it
    /// goes through [`tab_panel::overlay_band_quads`] with the resolved seam x.
    fn build_rail_overlay_quads(
        &self,
        cell: CellSize,
        side: RailSide,
    ) -> (Option<SolidQuad>, Option<SolidQuad>) {
        let Some(gpu) = self.gpu.as_ref() else {
            return (None, None);
        };
        let (surface_w, surface_h) = gpu.surface_size();
        let strength = self.tab_panel_strength();
        let colors = self.tab_bar_colors();
        let panel_color = tab_chrome::panel_tint(colors, strength);
        let p = tab_chrome::panel_wash_alpha(strength, self.settings.cell_bg_opacity);
        let wash_alpha = p.max(rail_autohide::REVEAL_WASH_ALPHA);
        let seam = (self.settings.tab_seam && strength > 0.0)
            .then(|| tab_chrome::seam_color(colors, panel_color));
        let seam_x = self.rail_overlay_seam_x(cell, side);
        let axis = match side {
            RailSide::Left => tab_panel::PanelAxis::Left,
            RailSide::Right => tab_panel::PanelAxis::Right,
        };
        tab_panel::overlay_band_quads(
            axis,
            seam_x,
            surface_w as f32,
            surface_h as f32,
            gpu.scale().round().max(1.0),
            panel_color,
            wash_alpha,
            seam,
            tab_chrome::SEAM_ALPHA,
        )
    }

    /// The single-pane render-cache key for the revealed rail overlay (F4-P3).
    /// `default()` (not revealed) is a frame-to-frame constant, so the pinned /
    /// no-autohide path keeps its byte-identical cache behavior; when revealed,
    /// the visibility + geometry + a hash of the rail's visual state (active
    /// index, tab count, hover, titles) make a reveal / hide / switch / rename /
    /// hover / auto-width change reclassify to a Full rebuild.
    fn rail_overlay_render_signature(&self, cell: CellSize) -> RailOverlaySignature {
        use std::hash::{Hash, Hasher};
        let Some(side) = self.rail_autohide_side() else {
            return RailOverlaySignature::default();
        };
        if !self.rail_overlay_visible() {
            return RailOverlaySignature::default();
        }
        let cols = self.rail_overlay_cols();
        let origin = self.rail_overlay_origin_px(cell, side);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use tab_bar::TabBarSource;
        self.sessions.active_tab().hash(&mut hasher);
        self.sessions.tab_count().hash(&mut hasher);
        for idx in 0..self.sessions.tab_count() {
            self.sessions.tab_title(idx).hash(&mut hasher);
        }
        // Hover state changes the highlighted slot, so a hover move while
        // revealed must repaint.
        format!("{:?}", self.tab_rail.hover).hash(&mut hasher);
        RailOverlaySignature {
            visible: true,
            cols,
            origin_bits: [origin[0].to_bits(), origin[1].to_bits()],
            content_hash: hasher.finish(),
        }
    }

    /// The rail hit under the pointer via the **revealed overlay** geometry, or
    /// `None` when the overlay is not currently revealed / the pointer is off the
    /// band. Lets the press dispatch route clicks on the floating rail to
    /// switch/close/new-tab without any reservation (F4-P3).
    fn rail_overlay_hit(&self) -> Option<TabHit> {
        let side = self.rail_autohide_side()?;
        if !self.rail_overlay_visible() {
            return None;
        }
        let (x_px, y_px) = self.pointer_px?;
        let cell = self.resolved_cell()?;
        if !self.pointer_in_reveal_band(x_px, cell, side) {
            return None;
        }
        match self.tab_rail.hit_test(
            x_px,
            y_px,
            &self.sessions,
            self.rail_overlay_cols(),
            self.tab_rail_grid_rows(),
            self.rail_overlay_origin_px(cell, side),
            cell,
            self.rail_geom(),
        ) {
            TabHit::None => None,
            hit => Some(hit),
        }
    }

    /// Pixels to subtract from a raw pointer `(x, y)` before mapping it to a grid
    /// cell, accounting for tab chrome. Top bar → `(0, tab_h)`; left rail →
    /// `(rail_w + gap_w, 0)`; right rail / none → `(0, 0)` (content origin
    /// unmoved). This is the single placement-aware pointer transform every
    /// single-pane hit path applies; on the top path `left_reserved_cols() == 0`
    /// so it is byte-identical. Includes the rail↔content wallpaper gap (R1.1) so
    /// the content pointer stays registered with the shifted content origin.
    fn tab_chrome_offset_px(&self, cell: CellSize) -> (f64, f64) {
        let r = self.tab_reserve();
        (
            cell.width as f64 * r.left_reserved_cols() as f64,
            cell.height as f64 * r.top_rows as f64,
        )
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
        // F4-P4: record the rail width now baked into the content reservation so
        // `reconcile_rail_auto_width` reflows exactly once when auto-sizing (or a
        // manual/max-width change) moves the band. 0 on the top-bar/hidden path.
        self.rail_reserved_cols = self.rail_cols();
    }

    /// Shift window-content overlay quads by the tab-chrome offset so they stay
    /// registered with the content grid after chrome is reserved: `+Y` for the
    /// top bar, `+X` for the left rail (F4-V2). `(0, 0)` on the plain path leaves
    /// every quad untouched (byte-identical).
    fn shift_overlays_for_tab_chrome(&self, overlays: &mut [SolidQuad], dx: f32, dy: f32) {
        if dx <= 0.0 && dy <= 0.0 {
            return;
        }
        for overlay in overlays {
            overlay.rect[0] += dx;
            overlay.rect[2] += dx;
            overlay.rect[1] += dy;
            overlay.rect[3] += dy;
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
        // F4-P3: under rail auto-hide the pinned chrome is NOT decorated into the
        // content snapshot — the rail draws only as a floating overlay
        // (`build_rail_overlay`) over full-bleed content. This early return is the
        // fix for the phantom TOP bar: the dispatch below keys off `rail_side()`,
        // which reads the (deliberately zeroed) auto-hide reservation and so
        // reports `None`; without this guard an auto-hidden LEFT/RIGHT rail fell
        // through to the top-bar branch and grew a one-row bar across the top of
        // a side-placed window.
        if self.rail_autohide_active() {
            return (snapshot.clone(), Vec::new());
        }
        // Dispatch on placement: the vertical rail grows the snapshot by columns
        // off a side; the classic top bar grows it by rows off the top.
        if let Some(side) = self.rail_side() {
            return self.decorate_snapshot_with_tab_rail(snapshot, cursor_visible, cell, side);
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
            self.tab_bar_colors(),
            self.tab_panel_strength(),
        );
        for glyph in output.glyphs {
            if glyph.col < columns {
                decorated.cells[glyph.col] = crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        (decorated, output.quads)
    }

    /// Single-pane vertical-rail decoration (F4-V2): grow the snapshot by the
    /// rail band plus its wallpaper gap (R1.1) on the rail side, shift the
    /// original content (and the cursor) into the content band, paint the rail
    /// glyphs into the rail band of every row, and leave the gap columns blank so
    /// they render as the wallpaper-washed padding band (default cells composite
    /// the background at `cell_bg_opacity`, identical to the window padding). The
    /// reservation used here MUST match the resize path (ODP-8) or the
    /// cursor/pointer desync — both read `TabReserve` gap-inclusive column math.
    fn decorate_snapshot_with_tab_rail(
        &self,
        snapshot: &Snapshot,
        cursor_visible: bool,
        cell: CellSize,
        side: RailSide,
    ) -> (Snapshot, Vec<SolidQuad>) {
        let rail_cols = self.rail_cols();
        let gap_cols = self.rail_gap_cols();
        let old_cols = snapshot.dimensions.columns;
        let rows = snapshot.dimensions.rows;
        let new_cols = old_cols + rail_cols + gap_cols;
        // Left rail: content shifts right by the rail band + gap; the rail paints
        // at column 0 and the gap columns [rail_cols, rail_cols+gap_cols) stay
        // blank. Right rail (F4-P2): content stays at column 0, then the gap,
        // then the rail band at the far right.
        let content_col_offset = match side {
            RailSide::Left => rail_cols + gap_cols,
            RailSide::Right => 0,
        };
        let rail_col_start = match side {
            RailSide::Left => 0,
            RailSide::Right => old_cols + gap_cols,
        };
        let mut decorated = Snapshot {
            dimensions: Dimensions::new(new_cols, rows),
            cursor: Position {
                row: snapshot.cursor.row,
                column: snapshot.cursor.column + content_col_offset,
            },
            cursor_visible,
            colors: snapshot.colors.clone(),
            cells: vec![crate::core::Cell::default(); new_cols * rows],
        };
        // Copy each original row into the content band of the wider row.
        for r in 0..rows {
            let src = &snapshot.cells[r * old_cols..(r + 1) * old_cols];
            let dst_start = r * new_cols + content_col_offset;
            decorated.cells[dst_start..dst_start + old_cols].clone_from_slice(src);
        }

        let padding = self
            .gpu
            .as_ref()
            .map(GpuState::window_padding)
            .unwrap_or(WindowPadding::ZERO);
        let output = self.tab_rail.render(
            &self.sessions,
            rail_cols,
            rows,
            [padding.as_f32(), padding.as_f32()],
            cell,
            side,
            self.tab_bar_colors(),
            self.rail_geom(),
            self.tab_panel_strength(),
        );
        for glyph in output.glyphs {
            let col = rail_col_start + glyph.col;
            if glyph.row < rows && col < new_cols {
                decorated.cells[glyph.row * new_cols + col] =
                    crate::core::Cell::new(glyph.ch, glyph.attrs);
            }
        }
        (decorated, output.quads)
    }

    /// Rows the single-pane graphics layer shifts down for the top tab bar
    /// (0 for a rail — a rail reserves columns, not rows).
    fn tab_bar_row_offset(&self) -> usize {
        self.tab_reserve().top_rows
    }

    /// Columns the single-pane graphics layer shifts right for a left rail
    /// (0 for the top bar or a right rail — content origin unmoved). Includes the
    /// rail↔content wallpaper gap (R1.1) so images/sixels align with the content
    /// band rather than the rail seam.
    fn tab_bar_col_offset(&self) -> usize {
        self.tab_reserve().left_reserved_cols()
    }

    fn apply_user_event(&mut self, event: UserEvent) -> bool {
        match event {
            UserEvent::Redraw { session } => {
                if let Some(target) = self.sessions.get_mut(session) {
                    target.needs_rebuild = true;
                    target.refresh_tab_title();
                }
                // Redraw suppression (design doc §2.5 audit row #4): wake the
                // window when the session is *any visible pane of the active
                // tab*, not only the focused one — a background pane producing
                // output must repaint in a split. For a single-pane tab
                // `is_visible_pane` is exactly `active_id() == session`, so the
                // single-pane wake decision is unchanged.
                if self.sessions.is_visible_pane(session)
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
            working_directory: self.options.working_directory.clone(),
            command: self.options.command.clone(),
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
            attach_session: self.options.attach_session.clone(),
        }
    }

    fn poll_config_reload(&mut self, now: Instant) {
        match self.settings_reloader.poll(now) {
            SettingsReloadOutcome::Unchanged | SettingsReloadOutcome::Deleted => {}
            SettingsReloadOutcome::Reloaded(settings) => self.apply_reloaded_settings(settings),
            SettingsReloadOutcome::Invalid { warnings } => {
                for warning in warnings {
                    tracing::warn!(warning = %warning, "config reload ignored");
                }
            }
            SettingsReloadOutcome::Unreadable { message } => {
                tracing::warn!(message = %message, "config reload ignored");
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

    /// Persist a first-run marker when the onboarding card is dismissed, so it
    /// does not reshow on the next launch. Onboarding's gate is purely whether
    /// `odytty.conf` exists, and plain dismissal writes nothing; this ensures
    /// the file exists (without clobbering it if the user already has one).
    ///
    /// Best-effort: a write failure is logged but never blocks dismissal, and
    /// the onboarding overlay has no save UI to surface an error through.
    fn persist_first_run_config(&mut self) {
        let Some(path) = self.settings_reloader.config_path() else {
            return;
        };
        if let Err(error) = ensure_config_file_exists_at(path) {
            tracing::warn!(error = %error, "could not record first-run marker");
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

    /// Whether a settings reload that touched `shell_integration` should raise
    /// the "applies to new shells" notice. True only on a genuine OFF->ON
    /// transition while a live session exists — silent on startup (no
    /// transition), an ON->ON reload, the ON->OFF reverse toggle, or an OFF->ON
    /// with no running shell to inform. Pure so the gating is exhaustively
    /// unit-tested without standing up an App.
    fn should_announce_shell_integration_to_new_shells(
        was_enabled: bool,
        now_enabled: bool,
        has_live_session: bool,
    ) -> bool {
        !was_enabled && now_enabled && has_live_session
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
        // Capture the prior shell-integration state BEFORE `self.settings` is
        // replaced below, so a genuine OFF->ON toggle can be distinguished from
        // an unchanged reload (the new-shells notice fires only on the
        // transition, never on every reload).
        let shell_integration_was_enabled = self.settings.shell_integration;
        // F4 ODP-7: capture whether the tab bar is currently shown before the
        // settings swap, so a live `always_show_tab_bar` toggle can recompute the
        // content grid (the bar reserves a row; appearing/disappearing changes
        // the usable height). Nothing else in this reload path touches the tab
        // bar's visibility, so this is the only trigger for that recompute.
        let tab_bar_was_shown = self.should_show_tab_bar();
        // F4-V2: capture the effective placement too — a live top↔left flip
        // changes the reserved AXIS (rows vs columns) without changing the bar's
        // visibility, so it needs the same grid recompute.
        let tab_placement_was = self.effective_placement();

        let next_options = self.options_for_settings(&next_settings);
        let (text_rebuilt, padding_changed) = match self.gpu.as_mut() {
            Some(gpu) => {
                let text_rebuilt = match gpu
                    .apply_text_options(&next_options, next_settings.effective_stem_darken())
                {
                    Ok(changed) => changed,
                    Err(err) => {
                        tracing::warn!(error = %err, "config reload ignored: text options apply failed");
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
        // Phase 2 output recording: fan the live `session_replay` state out to
        // every session's recorder so a config-reload / settings-panel toggle
        // takes effect immediately. Off (the default) is a cheap no-op that
        // also frees any buffered frames, so the plain path is unaffected.
        self.sessions
            .set_recording_enabled(self.settings.session_replay);
        self.sessions
            .set_shell_integration_enabled(self.settings.shell_integration);
        // Shell-integration hooks are injected only at spawn time, so enabling
        // the setting mid-session cannot retroactively integrate the shell that
        // is already running — only new tabs/panes pick it up. Surface an honest
        // transient notice on the genuine OFF->ON transition while a shell is
        // live, instead of silently appearing to do nothing.
        if Self::should_announce_shell_integration_to_new_shells(
            shell_integration_was_enabled,
            self.settings.shell_integration,
            !self.sessions.is_empty(),
        ) {
            self.raise_open_notice(
                "Shell integration applies to new shells — open a new tab or split to activate."
                    .to_owned(),
            );
        }
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
        self.prefix_engine = PrefixEngine::from_settings(&self.settings);
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
            // C29: OSC 4 replies report the theme palette, not the xterm table.
            terminal.set_base_palette(self.effective_theme.palette.map(rgb));
            terminal.set_osc52_read_enabled(self.settings.osc52_read);
            terminal.set_cursor_defaults(
                self.settings.cursor_style,
                self.settings.cursor_blink.enabled(),
            );
        }
        // Apply the scrollback cap to *every* session, not just the active one:
        // a background tab streaming unbounded output must stay memory-bounded
        // regardless of focus. Lowering the cap trims existing history at once.
        let scrollback_limit = self.settings.scrollback_limit();
        for session in self.sessions.iter() {
            if let Ok(mut terminal) = session.terminal.lock() {
                terminal.set_scrollback_limit(scrollback_limit);
            }
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

        // F4 ODP-7 / F4-V2: if a live toggle flipped the bar's visibility OR its
        // placement (top↔left changes the reserved axis), reserve/reclaim the tab
        // chrome now so the content grid matches. No-op when both are unchanged.
        if self.should_show_tab_bar() != tab_bar_was_shown
            || self.effective_placement() != tab_placement_was
        {
            self.recompute_grid_for_tab_bar();
        }

        self.last_render_signature = None;
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);

        self.needs_rebuild = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Whether this frame must rebuild geometry. `self.needs_rebuild` Derefs to
    /// the FOCUSED pane's flag; single-pane that is the only visible pane, so the
    /// decision is byte-identical to before. Multi-pane: OR the flag across every
    /// visible pane of the active tab, so output streaming into a non-focused
    /// split pane repaints even while the focused pane is idle — otherwise a
    /// build in the other half of a split freezes until the user types into the
    /// focused pane (NF21-7). Paired with `clear_visible_pane_rebuild_flags` in
    /// the multi-pane rebuild branch, which must clear the same set.
    fn should_rebuild_frame(&self) -> bool {
        self.needs_rebuild
            || (!self.sessions.active_is_single_pane()
                && self.sessions.any_visible_pane_needs_rebuild())
    }

    fn run_about_to_wait_maintenance(&mut self, now: Instant) {
        // NF20-B: settle the cursor-animation / render-hold timers of every
        // non-active pane. Background panes are never rendered, so their timers
        // have no consumer; parking them here — the one place that runs before
        // every `next_wake_deadline` recompute in the loop — keeps them out of
        // the wake set and guarantees a pane switched back to starts from a clean
        // (non-stale) timer state. Idempotent and cheap. Paired with the
        // active-only deadline sources in `next_wake_deadline`.
        self.sessions.park_background_timers();

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

        // An animation tick (cursor ease/slide, smooth-scroll glide, bell flash,
        // new-row fade, open-notice / click-hint expiry) rebuilds once so the
        // frame advances. NF21-2: the predicate is "an animation is in flight",
        // NOT "now >= deadline". Three of the frame-paced contributors
        // (new_row_fade / scroll_anim / bell embed `Instant::now() + FRAME`), as
        // does the cursor ease/slide, so `now >= deadline` is essentially never
        // satisfied mid-flight — the old equality check silently never fired for
        // them and the animation only stepped when an unrelated wake (a blink
        // toggle) happened to rebuild. Treating "woken while animating" as
        // "request a frame" closes that: the collector schedules the wake at the
        // next frame boundary (`animation_deadline()` = now+FRAME), this repaint
        // advances the timer in the rebuild, and when it settles
        // `animation_deadline()` -> `None` ends the loop — bounded, so the
        // terminal returns to zero-wake idle with no wake and no redraw at rest.
        // Gated to the single-pane render path for the same reason the collector
        // source is (that path is the only consumer that advances these timers;
        // multipane advancement is NF21-1/7). The real-instant contributors
        // (open-notice / click-hint) still fire exactly once — the collector
        // wakes only at their expiry, so `is_some()` sees them due on that one
        // pass and the rebuild clears them.
        if self.sessions.active_is_single_pane() && self.animation_deadline().is_some() {
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

        // F4-P3: advance the rail auto-hide timers (show debounce / hide grace /
        // flash expiry). A due boundary flips the overlay's visibility; repaint
        // so the reveal appears or the hide takes effect. Returns `false` at rest
        // (no autohide, or steady visible/hidden), so this is inert on the plain
        // path and while the rail is parked open under the pointer. Keep the
        // suspend latch current so a menu closing lets the grace run again.
        self.rail_autohide.set_suspend(self.overlay.is_open());
        if self.rail_autohide.poll(now) {
            // A timer boundary that flips the rail's visibility (show debounce
            // elapsing, hide grace / flash expiring) must rebuild the frame so
            // the overlay is (re)assembled or dropped — `build_rail_overlay` runs
            // only inside the `should_rebuild_frame` gate, which reads
            // `needs_rebuild`. Requesting a redraw alone lets the rebuild be
            // skipped and the reveal never paints until an unrelated dirty frame.
            self.needs_rebuild = true;
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }

        // §7 multiplexer prefix: forget a pending prefix that has timed out.
        // `pending_deadline()` is a `next_wake_deadline` source, so a prefix left
        // pending after its timeout would keep the loop scheduling
        // `WaitUntil(<past instant>)` — a 0-timeout poll that returns immediately
        // every iteration and busy-spins a core — until the next key or focus
        // loss cleared it. Expiring it here on the timer (the same instant the
        // loop is woken at) breaks that spin. No repaint: the pending state has
        // no frame-path affordance yet; if one ships, request a redraw here.
        self.prefix_engine.expire_pending(now);

        // BLACK-SCREEN-ON-RESTORE: a due skipped-frame retry. Clear the pending
        // deadline and request a redraw so the next `RedrawRequested` re-attempts
        // the frame; if it skips again (and the guards still allow it) the
        // RedrawRequested arm re-arms a fresh bounded retry. This is a timed,
        // budget-capped retry — not a busy-poll.
        if let Some(deadline) = self.skipped_frame_retry_deadline
            && now >= deadline
        {
            self.skipped_frame_retry_deadline = None;
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
            .with_decorations(self.settings.window_decorations)
            // Runtime window/title-bar icon (Windows + X11; a no-op on macOS and
            // Wayland). `None` on any decode failure, so a bad icon can never
            // block window creation. The `.exe` file icon is embedded separately
            // at build time (build.rs / winresource).
            .with_window_icon(super::window_icon::load());
        #[cfg(all(unix, not(target_os = "macos")))]
        let attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes.with_name(APP_ID, "odytty")
        };

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(err) => {
                self.fail(event_loop, NativeError::WindowCreation(err.to_string()));
                return;
            }
        };

        // IME: allow composition input (CJK input methods, compose/dead-key
        // accents) to deliver `Ime::Preedit`/`Ime::Commit` events. Without this
        // winit suppresses IME and composed text never reaches the terminal.
        window.set_ime_allowed(true);

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
                // TRAP-5). An attached session reports not-running (the job lives
                // in the remote host), so closing an attached window detaches
                // immediately without prompting.
                if self.settings.confirm_close && self.foreground_job_running() {
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
                // BLACK-SCREEN-ON-RESTORE: track minimize (a 0x0 surface) so the
                // skipped-frame retry is suppressed while there is nothing to
                // paint. A restore (non-zero size) clears it AND resets the
                // skipped-frame retry budget so the recovering surface gets a
                // fresh set of bounded retries if its first acquire skips.
                self.window_minimized = size.width == 0 || size.height == 0;
                if !self.window_minimized {
                    self.consecutive_skipped_frames = 0;
                }
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
                // C4: re-center the image viewer for the new surface size.
                self.refresh_image_overlay_on_resize();
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
                    if self
                        .synchronized_output_hold
                        .should_hold(synchronized_output, now)
                    {
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
                            let (scrollback_len, prompt_marks_changed, bell_rang) = {
                                // P0-3: per-frame paint read — poison-recover.
                                let mut terminal = crate::native::lock_recover(&self.terminal);
                                (
                                    terminal.screen().scrollback_len(),
                                    self.settings.command_status_gutter
                                        && terminal.take_prompt_marks_changed(),
                                    terminal.take_bell(),
                                )
                            };
                            if bell_rang {
                                let window = self.window.clone();
                                self.note_bell(now, window.as_deref());
                            }
                            self.update_bell_flash(now);
                            // OPEN-NOTICE (P0-2): expire a transient open-failure
                            // banner once it has outlived its lifetime; no-op when
                            // none is in flight.
                            self.update_open_notice(now);
                            // UX-A (Phase 11): expire the click hint + drop a
                            // stale unpaired mis-click. No-op on the idle path.
                            self.update_click_hint(now);
                            let added = scrollback_len.saturating_sub(self.last_scrollback_len);
                            self.viewport.anchor_after_growth(added, scrollback_len);
                            self.last_scrollback_len = scrollback_len;
                            self.viewport.clamp(scrollback_len);
                            if prompt_marks_changed {
                                self.prompt_marks_epoch = self.prompt_marks_epoch.wrapping_add(1);
                            }
                            let offset = self.viewport.offset();
                            let mut search = std::mem::take(&mut self.search);
                            // P0-3: same-frame search refresh + graphics read.
                            let terminal = crate::native::lock_recover(&self.terminal);
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
                        // IME pre-edit: paint the in-progress composition inline
                        // at the cursor; empty on the no-composition path.
                        self.paint_ime_preedit_cells(&mut snapshot);
                        // OPEN-NOTICE (P0-2): a transient one-row failure banner
                        // across the top of the grid; empty on the success /
                        // no-notice path so the frame is byte-identical.
                        self.paint_open_notice_cells(&mut snapshot);
                        // UX-A (Phase 11): the Ctrl+hover armed underline on the
                        // hovered path span, then the transient bottom-left
                        // "Ctrl+click to open" hint. Both no-op (byte-identical)
                        // off their gates — armed underline needs interactive_paths
                        // + Ctrl + a hovered path; the hint needs to be shown.
                        self.paint_armed_path_underline_cells(&mut snapshot);
                        self.paint_click_hint_cells(&mut snapshot);
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
                                    bell_flash: self.bell_flash_overlay_signature(),
                                    ime_preedit: self.ime_overlay_signature(),
                                    open_notice: self.open_notice_overlay_signature(),
                                    // UX-A (Phase 11): both Inert off their gates,
                                    // so the composite stays constant on the
                                    // default path; armed_path flips on Ctrl
                                    // toggle / span move so it reclassifies Full.
                                    click_hint: self.click_hint_overlay_signature(),
                                    armed_path: self.armed_path_overlay_signature(),
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
                        if let Some(gpu) = self.gpu.as_mut() {
                            let rail_overlay = rail_overlay_data.as_ref().map(|data| RailOverlay {
                                snapshot: &data.snapshot,
                                origin: data.origin,
                                treatment: background_treatment,
                                wash: data.wash,
                                seam: data.seam,
                            });
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
                                        tab_bar_col_offset,
                                    );
                                    if overlays.is_empty()
                                        && tab_bg_quads.is_empty()
                                        && rail_overlay.is_none()
                                    {
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
                                            &tab_bg_quads,
                                            rail_overlay,
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
                let action = {
                    let Some(gpu) = self.gpu.as_mut() else {
                        return;
                    };
                    let outcome = gpu.render();
                    let action = after_frame(outcome);
                    // Surface lost/outdated/validation (e.g. after a resize,
                    // compositor change, or a Windows DX12 surface going Lost on
                    // idle-minimize): reconfigure here, then request a redraw
                    // below so the recovered surface is actually painted. Under
                    // `ControlFlow::Wait` there is no automatic next frame, so a
                    // reconfigure without a follow-up redraw leaves the surface
                    // valid-but-unpainted (black) until an unrelated event.
                    if matches!(action, FrameAction::ReconfigureThenRedraw) {
                        gpu.reconfigure();
                    }
                    action
                };
                // Drop the `gpu` borrow before touching `self.window` (disjoint
                // fields, but `self.gpu.as_mut()` borrows all of `self`).
                match action {
                    FrameAction::Idle => {
                        // A present resets the skipped-frame retry budget so a
                        // future transient skip gets a fresh set of retries.
                        self.consecutive_skipped_frames = 0;
                        self.skipped_frame_retry_deadline = None;
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
                    FrameAction::RetryAfter(delay) => {
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
            }
            // `winit` reports modifier state separately from key presses; cache
            // it so the next `KeyboardInput` encodes with Ctrl/Alt/Shift held.
            WindowEvent::ModifiersChanged(state) => {
                let state = state.state();
                let was_ctrl = self.modifiers.ctrl;
                self.modifiers = Modifiers {
                    ctrl: state.control_key(),
                    alt: state.alt_key(),
                    shift: state.shift_key(),
                };
                self.super_key = state.super_key();
                // UX-A (Phase 11): the Ctrl+hover armed underline appears/clears
                // as Ctrl toggles while a path is hovered, so a Ctrl transition
                // there must trigger a rebuild + redraw to repaint the span.
                // Gated on `interactive_paths` + a hovered path, so the default /
                // feature-off path is untouched (byte-identical).
                if was_ctrl != self.modifiers.ctrl
                    && self.settings.interactive_paths
                    && self.hovered_path.is_some()
                {
                    self.needs_rebuild = true;
                    if let Some(window) = self.window.as_ref() {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::Focused(focused) => {
                self.on_window_focus_changed(focused);
            }
            // BLACK-SCREEN-ON-RESTORE: a Windows restore can surface as
            // `Occluded(false)` without a non-zero `Resized`; recover the paint
            // there. Only the un-occlude direction is handled (see the method
            // doc) — occlusion is not treated as minimize.
            WindowEvent::Occluded(occluded) => {
                self.on_window_occluded(occluded);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.update_pointer_cell(position.x, position.y);
            }
            WindowEvent::CursorLeft { .. } => {
                // F4-P3: the pointer left the window — feed the auto-hide machine
                // an empty sample so a rail revealed at the edge starts its hide
                // grace (no `CursorMoved` fires once the pointer is gone). Inert
                // unless autohide is active.
                if self.rail_autohide_active() {
                    // Drop the motion-aware trigger's previous sample so the next
                    // entry starts fresh (a stale pre-leave x would fabricate a
                    // segment across the whole surface on re-entry).
                    self.last_rail_pointer_px = None;
                    if self.rail_autohide.on_pointer(false, false, Instant::now())
                        && let Some(window) = self.window.as_ref()
                    {
                        window.request_redraw();
                    }
                }
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
            WindowEvent::Ime(ime) => {
                self.handle_ime(ime);
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

/// Whether a logical key is the `C` of a `Ctrl+C` chord (SMART-CTRLC). `winit`
/// usually delivers the unmodified logical character (`"c"`/`"C"`), but some
/// platforms surface the control transform (`U+0003`, ETX); accept both so the
/// smart-Ctrl+C policy is robust across backends. Modifier state is checked by
/// the caller, so this only inspects the key identity.
fn is_ctrl_c_key(logical: &WinitKey) -> bool {
    match logical {
        WinitKey::Character(text) => text
            .chars()
            .next()
            .is_some_and(|ch| ch == '\u{3}' || ch.eq_ignore_ascii_case(&'c')),
        _ => false,
    }
}

/// Whether a logical key is `Delete` or `Backspace` (SELDEL-KEY) — either one
/// deletes a selection, matching the universal GUI convention. Modifier state is
/// checked by the caller; this only inspects the key identity.
fn is_selection_delete_key(logical: &WinitKey) -> bool {
    matches!(
        logical,
        WinitKey::Named(NamedKey::Delete) | WinitKey::Named(NamedKey::Backspace)
    )
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

/// F4-P4: the manual rail width (cells) a seam-drag pointer at `px_x` maps to.
/// The rail's OUTER edge is pinned to the window edge it hugs (left rail → the
/// left padding; right rail → `surface_w − pad`), so the width is the cell-
/// snapped distance from that edge to the pointer, clamped to `[min, max]`.
/// Measuring from the pinned window edge avoids the circularity of the right
/// rail's inner seam depending on the very width being set. Pure so the drag
/// geometry is unit-tested without a GPU/window. Module-private (its `RailSide`
/// parameter is `crate::native::app`-scoped); the tab_rail unit tests reach it
/// as a descendant module.
fn rail_width_cols_from_pointer(
    side: RailSide,
    px_x: f32,
    pad: f32,
    cell_w: f32,
    surface_w: f32,
    min: u16,
    max: u16,
) -> u16 {
    let cw = cell_w.max(1.0);
    let raw = match side {
        RailSide::Left => (px_x - pad) / cw,
        RailSide::Right => (surface_w - pad - px_x) / cw,
    };
    raw.round().clamp(min as f32, max as f32) as u16
}

/// Whether the operator-runnable rail reveal trace is enabled
/// (`ODYTTY_RAIL_TRACE=1` or `=true`). Read once and cached — a per-pointer-
/// sample env lookup would be wasteful. Privacy: the trace emits only pointer
/// coordinates and reveal-phase labels, never terminal content, titles, or PTY
/// bytes (the FREEZE-HARDEN logging privacy rule).
fn rail_trace_enabled() -> bool {
    static ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var_os("ODYTTY_RAIL_TRACE").is_some_and(|v| v == "1" || v == "true")
    });
    *ENABLED
}

/// F4-P3 reveal-zone regression: whether a raw physical pointer x is inside the
/// auto-hide reveal **trigger** zone — an interior band from the rail's window
/// edge inward by `reach` (= window padding + the scaled `tab_rail_reveal_px`).
/// Left: `x ≤ reach`; right: `x ≥ surface_w − reach`.
///
/// Padding-aware by construction: a zone that stopped at the bare surface edge
/// (`[0, reveal_px]`) sat *behind* the window's empty padding margin, so it was
/// only reachable by shoving the pointer into the extreme corner — the reported
/// "only reveals when the pointer leaves the window". Including the padding in
/// `reach` extends the zone through the margin and `reveal_px` into visible
/// content, reachable well before the pointer leaves. Pure so the geometry is
/// unit-tested with real padding without a GPU/window.
fn reveal_edge_contains(side: RailSide, px_x: f64, reach: f64, surface_w: f64) -> bool {
    match side {
        RailSide::Left => px_x <= reach,
        RailSide::Right => px_x >= surface_w - reach,
    }
}

/// F4-P3 motion-aware trigger: whether the pointer *segment* from `prev_px_x` to
/// `curr_px_x` intersects the reveal trigger band on the rail side. The left band
/// is `[0, reach]`; the right band is `[surface_w − reach, surface_w]`.
///
/// A live pointer trace showed the reveal armed reliably only when the pointer
/// overshot OFF the window edge (where the compositor clamps and delivers a run
/// of in-zone samples): at real cursor speed consecutive samples jump 30–200 px
/// and hop clean over a static point zone, so aiming *at* the edge frequently
/// registered nothing. Testing the whole segment arms a deliberate approach
/// regardless of speed — a move from `px_x = 60` to `px_x = −5` has neither
/// endpoint in `[0, reach]` yet its path crosses the band. The current point is
/// folded in by the caller as the first-sample fallback (no `prev`). Pure so the
/// geometry is unit-tested without a GPU/window.
fn reveal_edge_segment_crosses(
    side: RailSide,
    prev_px_x: f64,
    curr_px_x: f64,
    reach: f64,
    surface_w: f64,
) -> bool {
    let (lo, hi) = (prev_px_x.min(curr_px_x), prev_px_x.max(curr_px_x));
    match side {
        // Left band [0, reach]: the segment reaches into it (`lo ≤ reach`)
        // without lying entirely off the window to the left (`hi ≥ 0`).
        RailSide::Left => lo <= reach && hi >= 0.0,
        // Right band [surface_w − reach, surface_w]: the segment reaches into it
        // (`hi ≥ surface_w − reach`) without lying entirely off to the right.
        RailSide::Right => hi >= surface_w - reach && lo <= surface_w,
    }
}

/// F4-P3: whether a raw physical pointer x is inside the reveal **keep-alive**
/// region — the UNION of the trigger zone ([`reveal_edge_contains`]) and the
/// drawn overlay band (window edge → content-facing `seam_x`). Hide grace
/// begins only on leaving this union. Unioning the two explicitly (rather than
/// assuming the band always contains the trigger zone) keeps the keep-alive
/// correct even if a future width makes the band narrower than the padding-aware
/// trigger. Left band: `x < seam_x`; right band: `x > seam_x`.
fn reveal_band_contains(
    side: RailSide,
    px_x: f64,
    seam_x: f64,
    reach: f64,
    surface_w: f64,
) -> bool {
    if reveal_edge_contains(side, px_x, reach, surface_w) {
        return true;
    }
    match side {
        RailSide::Left => px_x < seam_x,
        RailSide::Right => px_x > seam_x,
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

/// What the event loop should do after a render attempt. Pure mapping from the
/// [`FrameOutcome`]; the call site applies the spin guards (minimized window,
/// retry-budget cap) before acting on a [`FrameAction::RetryAfter`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FrameAction {
    /// The frame presented; nothing extra to schedule (rest at the normal
    /// event-driven wake deadline). The default, byte-identical idle path.
    Idle,
    /// The surface was lost/outdated/validation-failed: reconfigure it, then
    /// request a redraw so the recovered surface is actually painted.
    ReconfigureThenRedraw,
    /// The frame was transiently skipped (`get_current_texture` returned
    /// Timeout/Occluded — e.g. the first frame as a Windows DX12 surface
    /// recovers on restore). Retry the frame after a bounded delay rather than
    /// busy-spinning. Subject to the call-site spin guards.
    RetryAfter(Duration),
}

/// Bounded delay before retrying a transiently-skipped frame. ~16ms ≈ one 60Hz
/// frame, so a recovering surface repaints within a frame without busy-spinning.
/// This is a real timed wake folded into the existing `WaitUntil` model, NOT a
/// poll loop.
const SKIPPED_FRAME_RETRY: Duration = Duration::from_millis(16);

/// Slow keep-alive retry once the fast-retry budget ([`MAX_SKIPPED_RETRIES`]) is
/// spent. ANTI-FREEZE: without this, a surface that kept returning
/// Timeout/Occluded past the budget left the loop resting at `Wait` with NO
/// pending paint — so a long-lived, non-interacted background window (nothing
/// delivering a `Resized`/`Focused`/input event) latched into a permanent
/// no-repaint freeze until the user forced a window event. A ~1s cadence is not
/// a busy-spin (≤1 wake/sec) yet guarantees an idle window self-heals within a
/// second of the surface actually recovering. Only a minimized (0x0) window
/// opts out — it has nothing to paint and a restore event always re-arms it.
const SKIPPED_FRAME_SLOW_RETRY: Duration = Duration::from_millis(1000);

/// Cap on consecutive *fast* `Skipped` retries with no successful present in
/// between. After this many fast tries the loop stops fast-retrying — but,
/// unlike before, it does NOT go silent: it falls back to the
/// [`SKIPPED_FRAME_SLOW_RETRY`] keep-alive (see [`next_skipped_retry_delay`]) so
/// a persistently-unavailable-then-recovered surface always repaints. The
/// counter resets on any successful present.
const MAX_SKIPPED_RETRIES: u32 = 8;

/// Pure post-frame decision (see [`FrameAction`]). Split out so the
/// black-screen-on-restore recovery policy is unit-testable with zero GPU/winit:
/// `NeedsReconfigure` must reconfigure AND repaint (or the recovered surface
/// stays black under `ControlFlow::Wait`), `Skipped` must schedule a bounded
/// retry (or a surface that came back Timeout/Occluded on restore never gets a
/// second chance and stays black), and `Presented` settles.
fn after_frame(outcome: FrameOutcome) -> FrameAction {
    match outcome {
        FrameOutcome::Presented => FrameAction::Idle,
        FrameOutcome::NeedsReconfigure => FrameAction::ReconfigureThenRedraw,
        FrameOutcome::Skipped => FrameAction::RetryAfter(SKIPPED_FRAME_RETRY),
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
fn should_schedule_skipped_retry(minimized: bool, consecutive_skipped: u32) -> bool {
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
fn next_skipped_retry_delay(minimized: bool, consecutive_skipped: u32) -> Option<Duration> {
    if minimized {
        None
    } else if consecutive_skipped < MAX_SKIPPED_RETRIES {
        Some(SKIPPED_FRAME_RETRY)
    } else {
        Some(SKIPPED_FRAME_SLOW_RETRY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blink() -> CursorBlinkState {
        CursorBlinkState::new(Duration::from_millis(500))
    }

    /// Pins the black-screen-on-restore recovery policy at the pure seam, with
    /// zero GPU/winit. Two failure modes are guarded:
    ///
    /// - `NeedsReconfigure` ⇒ reconfigure AND repaint (a lost/outdated surface,
    ///   e.g. Windows DX12 on idle-minimize; without the follow-up redraw the
    ///   recovered surface stays black under `ControlFlow::Wait`).
    /// - `Skipped` ⇒ a BOUNDED retry (a surface that came back Timeout/Occluded
    ///   on restore; the OLD policy did nothing here, so it stayed black until
    ///   an unrelated event — this is the residual the packet fixes).
    ///
    /// `Presented` settles. The GPU triggers themselves are on-device-only;
    /// this pins the decision deterministically.
    #[test]
    fn after_frame_maps_outcomes_to_recovery_actions() {
        assert_eq!(
            after_frame(FrameOutcome::NeedsReconfigure),
            FrameAction::ReconfigureThenRedraw,
            "a lost/outdated surface must reconfigure and request a redraw"
        );
        assert_eq!(
            after_frame(FrameOutcome::Presented),
            FrameAction::Idle,
            "a presented frame must settle (no extra paint scheduled)"
        );
        // The load-bearing assertion for THIS packet: a skipped frame must
        // schedule a bounded retry, not dead-end (the black-screen residual).
        match after_frame(FrameOutcome::Skipped) {
            FrameAction::RetryAfter(delay) => {
                assert!(
                    delay > Duration::ZERO && delay <= Duration::from_millis(100),
                    "a skipped frame must retry after a bounded, non-zero delay, got {delay:?}"
                );
            }
            other => panic!("a skipped frame must schedule a bounded retry, got {other:?}"),
        }
    }

    /// Pins the spin guards on the skipped-frame retry: a minimized window never
    /// retries (nothing to paint), and the consecutive-skip budget is finite so
    /// a persistently-unavailable surface falls back to event-driven `Wait`
    /// instead of wake-looping forever.
    #[test]
    fn skipped_retry_is_guarded_against_spin() {
        // Visible window, fresh budget: retry is allowed.
        assert!(
            should_schedule_skipped_retry(false, 0),
            "a visible window with budget remaining must retry a skipped frame"
        );
        // Minimized: never retry regardless of budget.
        assert!(
            !should_schedule_skipped_retry(true, 0),
            "a minimized (0x0) window must not retry — nothing to paint"
        );
        // Budget exhausted: stop retrying (fall back to Wait).
        assert!(
            !should_schedule_skipped_retry(false, MAX_SKIPPED_RETRIES),
            "the retry budget must be finite so a stuck surface can't wake-loop"
        );
        assert!(
            should_schedule_skipped_retry(false, MAX_SKIPPED_RETRIES - 1),
            "the last retry within budget must still be allowed"
        );
    }

    /// ANTI-FREEZE regression lock: once the fast-retry budget is spent, a
    /// visible surface must STILL schedule a retry — a slow keep-alive, not
    /// `None`. The previous policy dead-ended here, which under
    /// `ControlFlow::Wait` left a long-lived, non-interacted background window
    /// permanently unpainted (and apparently input-dead) until an external
    /// window event forced a repaint. The one legitimate opt-out is a minimized
    /// (0x0) window, which has nothing to paint and is re-armed by its restore
    /// event.
    #[test]
    fn skipped_retry_falls_back_to_slow_keepalive_never_silent() {
        // Minimized: no retry regardless of budget (nothing to paint).
        assert_eq!(
            next_skipped_retry_delay(true, 0),
            None,
            "a minimized (0x0) window schedules no retry"
        );
        assert_eq!(
            next_skipped_retry_delay(true, MAX_SKIPPED_RETRIES + 5),
            None,
            "a minimized window stays opted out even past the budget"
        );

        // Visible, under budget: fast retry (recover within a frame).
        assert_eq!(
            next_skipped_retry_delay(false, 0),
            Some(SKIPPED_FRAME_RETRY),
            "a fresh skip retries fast"
        );
        assert_eq!(
            next_skipped_retry_delay(false, MAX_SKIPPED_RETRIES - 1),
            Some(SKIPPED_FRAME_RETRY),
            "the last skip within budget still retries fast"
        );

        // Visible, budget spent: slow keep-alive — the load-bearing invariant.
        // It must be a real scheduled retry (never `None`), and slower than the
        // fast cadence so it is not a busy-spin.
        for spent in [MAX_SKIPPED_RETRIES, MAX_SKIPPED_RETRIES + 1, 10_000] {
            let delay = next_skipped_retry_delay(false, spent);
            assert_eq!(
                delay,
                Some(SKIPPED_FRAME_SLOW_RETRY),
                "budget spent (n={spent}) must keep-alive, not go silent"
            );
        }
        assert!(
            SKIPPED_FRAME_SLOW_RETRY > SKIPPED_FRAME_RETRY,
            "the keep-alive must be slower than the fast retry (no busy-spin)"
        );
    }

    /// BLACK-SCREEN-ON-RESTORE residual: a restore that arrives as `Focused(true)`
    /// WITHOUT a non-zero `Resized` first (the Windows case) must still clear the
    /// minimized state so the vetoed skipped-frame retry can schedule and the
    /// surface repaints. Drives the real `on_window_focus_changed` handler (the
    /// extracted event-arm body), not a reimplementation.
    #[test]
    fn focus_gain_clears_minimized_state_so_repaint_can_schedule() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // Simulate a minimize (a 0x0 `Resized`) followed by some skipped frames,
        // so the retry budget is partially spent and the spin guard is vetoing.
        app.window_minimized = true;
        app.consecutive_skipped_frames = 3;
        assert!(
            !should_schedule_skipped_retry(app.window_minimized, app.consecutive_skipped_frames),
            "precondition: while minimized the skipped-frame retry is vetoed (black screen)"
        );

        // The restore arrives ONLY as focus-gain (no non-zero Resized).
        app.on_window_focus_changed(true);

        assert!(
            !app.window_minimized,
            "focus-gain restore must clear the minimized flag"
        );
        assert_eq!(
            app.consecutive_skipped_frames, 0,
            "focus-gain restore must reset the skipped-frame retry budget"
        );
        assert!(
            should_schedule_skipped_retry(app.window_minimized, app.consecutive_skipped_frames),
            "after restore the bounded retry-wake must no longer be vetoed"
        );
    }

    /// Same residual via the other Windows restore signal: `Occluded(false)`
    /// without a non-zero `Resized`. Drives the real `on_window_occluded`
    /// handler. The occlude (`true`) direction must NOT set the flag (occlusion
    /// is not minimize).
    #[test]
    fn un_occlude_clears_minimized_state_and_occlude_does_not_set_it() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.window_minimized = true;
        app.consecutive_skipped_frames = 2;

        app.on_window_occluded(false);
        assert!(
            !app.window_minimized,
            "Occluded(false) restore must clear the minimized flag"
        );
        assert_eq!(
            app.consecutive_skipped_frames, 0,
            "Occluded(false) restore must reset the skipped-frame retry budget"
        );

        // Occlude (covered by another window) is NOT minimize: the flag must
        // stay false so a merely-covered window keeps repainting.
        app.on_window_occluded(true);
        assert!(
            !app.window_minimized,
            "Occluded(true) must not be treated as minimize"
        );
    }

    /// Guard: restoring when NOT minimized is a harmless no-op (the Linux/macOS
    /// path, where un-minimize goes through `Resized` and the flag is already
    /// false by the time Focused/Occluded fire). Must not clobber a live budget.
    #[test]
    fn restore_from_minimized_is_a_noop_when_not_minimized() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.window_minimized = false;
        app.consecutive_skipped_frames = 4;
        let cleared = app.restore_from_minimized();
        assert!(!cleared, "no minimized state to clear");
        assert_eq!(
            app.consecutive_skipped_frames, 4,
            "a no-op restore must not touch the retry budget"
        );
    }

    /// Build a fresh, un-driven `App` for wake-scheduling tests. Spawns a real
    /// (short-lived) PTY like the sibling `App`-level tests; returns `None` if
    /// the host cannot spawn one (skip rather than fail in constrained CI).
    fn build_idle_app() -> Option<App> {
        let dims = Dimensions::new(24, 80);
        let session = crate::native::test_support::spawn_test_pause_shell(dims).ok()?;
        let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
        let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
        let pty = Arc::new(Mutex::new(session));
        Some(App::new(
            NativeOptions::default(),
            terminal,
            writer,
            pty,
            Settings::default(),
            crate::settings::SettingsReloader::for_current_process(Instant::now()),
        ))
    }

    /// Regression guard for the focus-gated config-reload poll. On a fresh,
    /// un-driven `App` the live-reload watcher is the only focus-dependent wake
    /// source (cursor blink stays `None` until polled, and every other source
    /// is at rest), so toggling focus isolates the gate: focused schedules the
    /// 1 Hz config stat, unfocused suppresses it and the loop parks at
    /// zero-wake idle. A regression that drops the gate would bring back the
    /// once-a-second background wake this test forbids.
    #[test]
    fn config_reload_wake_is_suppressed_while_unfocused() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // No resolvable config path on this host ⇒ no deadline to gate; skip.
        let Some(config_deadline) = app.settings_reloader.deadline() else {
            return;
        };

        app.focused = true;
        assert_eq!(
            app.next_wake_deadline(),
            Some(config_deadline),
            "a focused window schedules the config-reload poll"
        );

        app.focused = false;
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "a backgrounded window schedules no timer wake (zero-wake idle)"
        );
    }

    /// NF20 regression: a multiplexer prefix (default Ctrl+B) that is pressed and
    /// then times out with no follow-up key must not busy-spin the event loop.
    ///
    /// `pending_deadline()` is a `next_wake_deadline` source, so a prefix left
    /// pending past its timeout kept the loop scheduling `WaitUntil(<past>)` — a
    /// 0-timeout poll that returns immediately every iteration and pins a core —
    /// until the next key or focus loss cleared it. The about-to-wait maintenance
    /// pass now expires the stale prefix on the timer, so the recomputed wait
    /// deadline is never a past instant. Drives the real deadline arithmetic
    /// (enter → wake at the boundary → maintenance → recompute); fails before the
    /// maintenance-side expiry existed (the final assert saw a past deadline).
    #[test]
    fn timed_out_prefix_does_not_spin_the_event_loop() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // Isolate the prefix as the only possible wake source: unfocused
        // suppresses the config-reload poll, autohide is off (no rail wake), and
        // nothing else is armed on a fresh idle app.
        app.focused = false;
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "idle app parks at zero wake"
        );

        // Press the multiplexer prefix at t0; it becomes pending and arms a
        // timeout deadline the loop will wait on.
        let t0 = Instant::now();
        let prefix = app
            .prefix_engine
            .prefix()
            .expect("the default pane prefix (Ctrl+B) is enabled");
        app.prefix_engine.on_chord(prefix, t0);
        assert!(app.prefix_engine.is_pending(), "prefix pending after entry");
        let deadline = app
            .prefix_engine
            .pending_deadline()
            .expect("a pending prefix arms a timeout boundary");
        assert_eq!(
            app.next_wake_deadline(),
            Some(deadline),
            "the pending prefix is the scheduled wake (a future boundary)"
        );

        // The loop wakes at/after the boundary and runs its maintenance pass.
        // That pass MUST forget the timed-out prefix; otherwise the recomputed
        // deadline is `deadline` again — now in the past — and the loop spins.
        let woken = deadline + Duration::from_millis(1);
        app.run_about_to_wait_maintenance_for_test(woken);
        assert!(
            !app.prefix_engine.is_pending(),
            "the timed-out prefix is expired on the timer, not left pending"
        );
        match app.next_wake_deadline() {
            None => {}
            Some(next) => assert!(
                next > woken,
                "no past-instant wake survives the maintenance pass \
                 (a deadline <= now re-arms WaitUntil(past) and busy-spins)"
            ),
        }
    }

    /// NF21-2 acceptance (ii): a single-pane terminal with nothing animating
    /// schedules NO animation wake — the restored `animation_deadline()`
    /// collector source contributes nothing at rest, so the strict zero-wake
    /// idle invariant is preserved.
    #[test]
    fn idle_single_pane_schedules_no_animation_wake() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.focused = false;
        assert_eq!(
            app.animation_deadline(),
            None,
            "no contributor is animating at rest"
        );
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "idle single-pane parks at zero wake — the NF21-2 source adds nothing at rest"
        );
    }

    /// NF21-2 acceptance (i, bell contributor): a bell flash schedules a repaint
    /// wake and a due wake requests a rebuild — even while the window is
    /// unfocused and the cursor is not blinking. Fails before both halves of the
    /// fix (no wake scheduled; no rebuild on the due wake).
    #[test]
    fn bell_flash_while_unfocused_schedules_a_wake_and_advances() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.focused = false;
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "precondition: the idle app parks at zero wake"
        );
        app.bell_flash_start = Some(Instant::now());
        let wake = app.next_wake_deadline();
        assert!(
            wake.is_some(),
            "an in-flight bell flash must schedule a repaint wake (NF21-2)"
        );
        app.needs_rebuild = false;
        app.run_about_to_wait_maintenance_for_test(wake.unwrap());
        assert!(
            app.needs_rebuild,
            "a due animation wake requests a rebuild (no wake-without-redraw)"
        );
    }

    /// NF21-2 acceptance (i, scroll contributor): a smooth-scroll glide settles
    /// even with the cursor blink not armed — it schedules its own repaint wake
    /// rather than depending on an unrelated blink toggle.
    #[test]
    fn scroll_glide_schedules_a_wake_without_a_blink_toggle() {
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.focused = false;
        assert_eq!(
            app.next_wake_deadline(),
            None,
            "precondition: no blink wake to piggyback on"
        );
        app.seed_scroll_glide_for_test(16.0);
        let wake = app.next_wake_deadline();
        assert!(
            wake.is_some(),
            "an in-flight smooth-scroll glide must schedule a repaint wake (NF21-2)"
        );
        app.needs_rebuild = false;
        app.run_about_to_wait_maintenance_for_test(wake.unwrap());
        assert!(
            app.needs_rebuild,
            "a due glide wake requests a rebuild so the offset advances toward rest"
        );
    }

    /// Reveal-zone regression (#1, padding-aware trigger): the trigger band is
    /// measured from the window edge inward by `pad + reveal_px`, so a pointer
    /// resting `reveal_px` into the *visible content* (just past the padding
    /// margin) reveals — it is not stranded behind the padding.
    #[test]
    fn reveal_trigger_zone_is_padding_aware_interior_band() {
        let pad = 12.0;
        let reveal_px = 8.0;
        let reach = pad + reveal_px; // 20
        let surface_w = 1000.0;

        // LEFT: content starts at x=pad(12). A pointer at x=15 (3px into visible
        // content) must trigger; the old edge-only zone [0, 8] would have
        // stranded it behind the padding.
        assert!(reveal_edge_contains(RailSide::Left, 15.0, reach, surface_w));
        assert!(reveal_edge_contains(RailSide::Left, 0.0, reach, surface_w));
        assert!(reveal_edge_contains(RailSide::Left, 20.0, reach, surface_w));
        assert!(!reveal_edge_contains(
            RailSide::Left,
            21.0,
            reach,
            surface_w
        ));

        // RIGHT: content ends at surface_w-pad(988). A pointer at x=985 (3px into
        // visible content from the right) must trigger.
        assert!(reveal_edge_contains(
            RailSide::Right,
            985.0,
            reach,
            surface_w
        ));
        assert!(reveal_edge_contains(
            RailSide::Right,
            surface_w,
            reach,
            surface_w
        ));
        assert!(reveal_edge_contains(
            RailSide::Right,
            surface_w - reach,
            reach,
            surface_w
        ));
        assert!(!reveal_edge_contains(
            RailSide::Right,
            surface_w - reach - 1.0,
            reach,
            surface_w
        ));
    }

    /// Reveal-zone regression (#2, keep-alive = union): the keep-alive region is
    /// the trigger zone UNIONED with the drawn band, so a pointer parked anywhere
    /// over the revealed band (or in the padding-aware trigger zone) holds the
    /// rail — hide grace begins only on leaving that union. This also pins the
    /// union so a future band narrower than the trigger cannot leave a gap.
    #[test]
    fn reveal_keep_alive_is_the_union_of_trigger_and_band() {
        let reach = 20.0;
        let surface_w = 1000.0;

        // LEFT band drawn out to seam_x=128. Mid-band (x=64) holds; the trigger
        // zone (x=5) holds; past the seam (x=200) does not.
        let seam_l = 128.0;
        assert!(reveal_band_contains(
            RailSide::Left,
            64.0,
            seam_l,
            reach,
            surface_w
        ));
        assert!(reveal_band_contains(
            RailSide::Left,
            5.0,
            seam_l,
            reach,
            surface_w
        ));
        assert!(!reveal_band_contains(
            RailSide::Left,
            200.0,
            seam_l,
            reach,
            surface_w
        ));

        // UNION guard: an artificially narrow band (seam at x=10, narrower than
        // reach=20) still keeps alive across the whole trigger zone — the trigger
        // fills the gap the thin band would otherwise leave.
        let thin_seam = 10.0;
        assert!(
            reveal_band_contains(RailSide::Left, 15.0, thin_seam, reach, surface_w),
            "trigger zone covers the gap a band narrower than the reach leaves"
        );

        // RIGHT band drawn from seam_x=872 rightward. Mid-band (x=936) holds; the
        // right trigger zone (x=995) holds; left of the seam (x=800) does not.
        let seam_r = 872.0;
        assert!(reveal_band_contains(
            RailSide::Right,
            936.0,
            seam_r,
            reach,
            surface_w
        ));
        assert!(reveal_band_contains(
            RailSide::Right,
            995.0,
            seam_r,
            reach,
            surface_w
        ));
        assert!(!reveal_band_contains(
            RailSide::Right,
            800.0,
            seam_r,
            reach,
            surface_w
        ));
    }

    /// Reveal-zone regression (motion-aware trigger, from the live pointer
    /// trace): a fast approach delivers samples 30–200 px apart that jump clean
    /// over the static point zone, so the arm must test the whole *segment*
    /// between consecutive samples — not just the current point.
    #[test]
    fn reveal_edge_segment_crosses_a_fast_sweep_over_the_point_zone() {
        let reach = 29.0; // ≈ the operator trace's reach
        let surface_w = 1000.0;

        // LEFT: the trace's dominant case — a move from x=60 to x=−5 has NEITHER
        // endpoint that a bounded [0, reach] point test would accept, yet the
        // path sweeps through the trigger band → the motion-aware test arms it.
        assert!(reveal_edge_segment_crosses(
            RailSide::Left,
            60.0,
            -5.0,
            reach,
            surface_w
        ));
        // A move that stops short of the band (60 → 40, both past the reach)
        // does NOT cross — the pointer never reached the edge.
        assert!(!reveal_edge_segment_crosses(
            RailSide::Left,
            60.0,
            40.0,
            reach,
            surface_w
        ));
        // A sweep from off-window INTO content past the band still crosses (the
        // pointer entered at the edge), where the current point alone would miss.
        assert!(reveal_edge_segment_crosses(
            RailSide::Left,
            -8.0,
            50.0,
            reach,
            surface_w
        ));

        // RIGHT: symmetric — a fast sweep toward the right edge that overshoots
        // past surface_w crosses the right band [surface_w − reach, surface_w].
        assert!(reveal_edge_segment_crosses(
            RailSide::Right,
            940.0,
            1010.0,
            reach,
            surface_w
        ));
        // Stopping short of the right band (940 → 960, both left of the band)
        // does not cross.
        assert!(!reveal_edge_segment_crosses(
            RailSide::Right,
            940.0,
            960.0,
            reach,
            surface_w
        ));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn app_id_matches_packaged_desktop_identity() {
        assert_eq!(APP_ID, "io.unfinished_works.odytty");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn desktop_file_startup_wm_class_matches_app_id() {
        let desktop = include_str!("../../../dist/linux/io.unfinished_works.odytty.desktop");
        assert!(desktop.contains("Icon=io.unfinished_works.odytty\n"));
        assert!(desktop.contains(&format!("StartupWMClass={APP_ID}\n")));
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

    // ---- Shell-integration "applies to new shells" notice ----

    /// The gating decision is pure so EVERY combination is pinned here —
    /// including the no-live-session case, which `build_idle_app` cannot
    /// construct (`App::new` always seeds one session, and there is no public
    /// close to drain it).
    #[test]
    fn new_shells_notice_fires_only_on_off_to_on_with_session() {
        // prior, next, has_live_session
        assert!(
            App::should_announce_shell_integration_to_new_shells(false, true, true),
            "OFF->ON with a live shell is the one honest case"
        );
        // No live session to inform -> stay silent.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            false, true, false
        ));
        // ON at startup / ON->ON reload: no transition.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            true, true, true
        ));
        // ON->OFF: the reverse toggle never nags.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            true, false, true
        ));
        // OFF->OFF: no transition.
        assert!(!App::should_announce_shell_integration_to_new_shells(
            false, false, true
        ));
    }

    /// Driving the real settings-reload seam OFF->ON while a live session
    /// exists must surface the transient notice — the wiring this packet adds.
    #[test]
    fn off_to_on_reload_raises_new_shells_notice() {
        // The reload seam republishes process-global render state (default
        // colors / palette / contrast floor), so serialize against the other
        // render-globals tests.
        let _guard = crate::test_lock::render_globals_lock();
        let Some(mut app) = build_idle_app() else {
            return;
        };
        // `build_idle_app` starts shell_integration OFF (the default) with one
        // live session, so flipping it ON is the genuine transition.
        assert!(!app.settings.shell_integration);
        assert!(!app.sessions.is_empty());
        assert!(
            app.open_notice_message_for_test().is_none(),
            "no notice before the toggle"
        );

        let mut next = app.settings.clone();
        next.shell_integration = true;
        app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

        assert_eq!(
            app.open_notice_message_for_test().as_deref(),
            Some("Shell integration applies to new shells — open a new tab or split to activate."),
            "an OFF->ON toggle with a live shell must surface the new-shells notice"
        );
    }

    /// The reverse transition (ON->OFF) genuinely applies through the seam
    /// (shell_integration changes, so it is not an early no-change return) yet
    /// must never raise the notice.
    #[test]
    fn on_to_off_reload_raises_no_notice() {
        let _guard = crate::test_lock::render_globals_lock();
        let Some(mut app) = build_idle_app() else {
            return;
        };
        app.settings.shell_integration = true;

        let mut next = app.settings.clone();
        next.shell_integration = false;
        app.apply_settings_through_reload_seam(next, SettingsApplySource::OverlayEdit);

        assert!(
            app.open_notice_message_for_test().is_none(),
            "an ON->OFF toggle must not surface the new-shells notice"
        );
    }
}

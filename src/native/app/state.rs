// SPDX-License-Identifier: GPL-3.0-only
//! `App` state ownership for the native window: field declarations,
//! construction, and the active-session dereference.
//!
//! `App` remains the single native-window state owner and the single point where
//! terminal/writer/search/viewport access retargets to the active session. Only
//! the declaration and the constructors live here; every behavior stays on its
//! existing call path. Field visibility is `pub(super)` (and
//! `pub(in crate::native)` where it already reached that far), which is exactly
//! the reach these fields had while they were declared in the parent module.

use super::*;

/// Application state driving the `winit` event loop.
///
/// The window is created lazily on `resumed` per `winit`'s portability
/// contract, and any startup failure is captured so it can be surfaced after
/// the loop returns.
pub(in crate::native) struct App {
    pub(super) options: NativeOptions,
    /// The launch-scoped local session eligible for `--hold`. Set only for the
    /// initial session and consumed at its first EOF, so tabs/panes created
    /// later never inherit the command-line option.
    pub(super) hold_session: Option<SessionToken>,
    /// A held, already-exited session awaiting its first non-release key event.
    pub(super) held_exit: Option<SessionToken>,
    /// Active *authored* presentation theme (from `ODYTTY_THEME`, updated on
    /// settings changes). The theme as written — what round-trips/authoring
    /// read; the colors published to the renderer are [`Self::effective_theme`].
    pub(super) theme: Theme,
    /// Theme actually published to the renderer (U4): [`Self::theme`] after
    /// colour-vision-deficiency adaptation. Equal to `theme` when `cvd_mode` is
    /// off (byte-identical plain path). Recomputed only in `apply_settings` via
    /// [`Self::cvd_cache`], never per frame.
    pub(super) effective_theme: Theme,
    /// One-entry cache for [`Self::effective_theme`] keyed on
    /// `(authored theme, cvd_mode, cvd_strength)` so repeated applies skip the
    /// palette re-floor.
    pub(super) cvd_cache: CvdThemeCache,
    /// Active optional visual treatment (selected once from `ODYTTY_VISUAL`,
    /// default off). Drives the ambient scanline uniform; presentation-only and
    /// fully disableable. The core never sees it.
    pub(super) visual: VisualEffect,
    pub(super) window: Option<Arc<Window>>,
    pub(super) gpu: Option<GpuState>,
    /// Last mouse-cursor shape pushed to the window. Tracked so
    /// [`Self::apply_cursor_icon`] only calls `Window::set_cursor` on an actual
    /// change (winit issues a platform request each call). Starts at the winit
    /// default (`Default` arrow) which matches a freshly created window.
    pub(super) cursor_icon: CursorIcon,
    /// Last pointer position in window physical pixels. Chrome is window-level
    /// UI, so its hit testing must survive active-session changes that clear the
    /// per-session pointer cache used by terminal-content interactions.
    pub(super) window_pointer_px: Option<(f64, f64)>,
    pub(super) sessions: WorkspaceSet,
    /// Native presentation epoch for pixel-affecting state outside the terminal
    /// core revision: theme/default-color changes, atlas/font changes, and
    /// other settings that make identical snapshots build different vertices.
    pub(super) presentation_epoch: u64,
    /// SH2 status-gutter invalidation epoch. Bumped when the core reports prompt
    /// marks changed while the status gutter is enabled, so a pure OSC 133
    /// status transition (which need not move the terminal render revision)
    /// still forces a non-retained redraw and the gutter repaints. Stays at its
    /// initial value while the gutter is off.
    pub(super) prompt_marks_epoch: u64,
    /// The terminal grid size last applied to the model and PTY. Tracked so a
    /// `Resized` event that does not change the whole-cell grid skips redundant
    /// model/PTY resize work (idempotence): only surface reconfigure runs.
    pub(in crate::native) grid: Dimensions,
    /// Latest modifier state, tracked across `ModifiersChanged` events so a key
    /// press can be encoded with the Ctrl/Alt/Shift held at press time. `winit`
    /// delivers modifier changes separately from key events, so this must be
    /// remembered rather than read off each `KeyboardInput`.
    pub(super) modifiers: Modifiers,
    /// Native-only Super/Logo modifier state. This is deliberately kept out of
    /// `input::Modifiers` because Super-based local shortcuts must not affect
    /// PTY key encoding.
    pub(super) super_key: bool,
    pub(super) key_bindings: KeyBindings,
    /// Multiplexer prefix engine (§7). Holds the configurable prefix chord, the
    /// pane-action table, and the transient prefix-pending state. Additive: when
    /// no prefix is pending (or the prefix is `off`), it leaves the input path
    /// byte-identical.
    pub(super) prefix_engine: PrefixEngine,
    /// Session that owned keyboard focus before the most recent activation.
    /// Used to route DEC focus reports to both sides of a tab/workspace switch.
    pub(super) last_active_session: SessionToken,
    pub(super) settings: Settings,
    pub(super) settings_reloader: SettingsReloader,
    /// Latest settings produced by a high-frequency overlay interaction
    /// (slider drag / key repeat). Coalesced so expensive live applies such as
    /// font-size atlas rebuilds happen at most once per frame/event burst.
    pub(super) pending_overlay_settings: Option<Settings>,
    /// ID1: when set, the authored theme `cursor`/`selection`/`search` roles
    /// drive cursor color and selection/search highlight fills (with
    /// RV1-floored foregrounds) instead of the historical inverse / hardcoded
    /// treatments. Default-on by design; `themed_ui_roles = off`
    /// restores the legacy rendering path.
    pub(super) themed_ui_roles: bool,
    /// Native in-window overlay state. It is presentation-only: widgets
    /// composite into snapshot copies and never mutate terminal state or PTY.
    pub(super) overlay: OverlayUi,
    /// Native-side clipboard owner. Kept alive across copy/paste operations so
    /// Linux clipboard contents remain served after Ctrl+Shift+C.
    pub(super) clipboard: NativeClipboard,
    pub(super) resize_debounce: ResizeDebouncer,
    /// BLACK-SCREEN-ON-RESTORE: pending bounded retry for a transiently-skipped
    /// frame. When a render returns [`FrameOutcome::Skipped`] (the surface
    /// acquire timed out / was occluded, e.g. the first frame as a Windows DX12
    /// surface recovers on restore), this holds the instant to retry. Folded
    /// into [`Self::next_wake_deadline`] so the retry rides the existing
    /// `WaitUntil` model (no busy-poll); cleared once due or once a frame
    /// presents. `None` on the steady-state path, so the idle wake set is
    /// unchanged when nothing is skipping.
    pub(super) skipped_frame_retry_deadline: Option<Instant>,
    /// A complete run of transiently skipped presents. This is independent of
    /// the bounded retry counter because restore and surface recovery reset that
    /// counter before the eventual successful present. The episode therefore
    /// retains the true duration and skip total for one end-of-episode record.
    pub(super) skip_episode: SkipEpisode,
    /// ANTI-FREEZE ESCALATION: bounded surface-recreate budget for a chronic
    /// acquire-timeout episode (see [`SkipEscalation`]). Re-armed only by a
    /// successful present, mirroring the freeze watchdog's semantics, so the
    /// budget cannot be refilled by the very retries that are failing.
    pub(super) skip_escalation: SkipEscalation,
    /// A restore signal arrived while a skip episode was active. Consumed once
    /// immediately before the next render to retire the starved swapchain using
    /// the same non-blocking reconfigure primitive as surface recovery.
    pub(super) pending_surface_reconfigure: bool,
    /// BLACK-SCREEN-ON-RESTORE: count of consecutive `Skipped` frames with no
    /// successful present in between. Caps the bounded retry (see
    /// [`MAX_SKIPPED_RETRIES`]) so a persistently-unavailable surface can't
    /// wake-loop forever. Reset to 0 on any present / reconfigure and on a
    /// restore (`Resized` to a non-zero size).
    pub(super) consecutive_skipped_frames: u32,
    /// FREEZE-WATCHDOG: monotonic count of `RedrawRequested` events actually
    /// DELIVERED to this app. The discriminator between the two ways "no frame
    /// presented" can look identical from the outside:
    ///
    /// - counter advancing, `frames_presented` flat → the windowing system is
    ///   asking us to draw and the render path is not answering. A real stall.
    /// - counter flat → the windowing system is not asking us to draw at all
    ///   (output asleep/DPMS-off, surface occluded, redraws throttled to a
    ///   frame callback the compositor has not returned). Zero frames is the
    ///   CORRECT steady state there, not a freeze.
    ///
    /// Never reset; the watchdog compares it against its own episode-start
    /// snapshot rather than resetting it here.
    pub(super) redraws_delivered: u64,
    /// BLACK-SCREEN-ON-RESTORE: whether the window is currently minimized (its
    /// surface reported a 0x0 size via `Resized`). Used to suppress the skipped-
    /// frame retry while minimized — there is nothing to paint, so a retry would
    /// only burn wakeups. Cleared on the next non-zero `Resized` (restore).
    pub(super) window_minimized: bool,
    /// Active divider drag: the tree-order index of the active tab's divider the
    /// pointer grabbed, while a left-drag is in progress (design doc §4.2). Only
    /// ever `Some` inside a multi-pane tab; `None` otherwise, so the single-pane
    /// pointer path is unaffected.
    pub(super) divider_drag: Option<usize>,
    /// F4-P4 auto-width cache: the rail band width (cells) currently baked into
    /// the content-grid reservation. `reconcile_rail_auto_width` reflows the
    /// grid only when the live resolved width diverges from this, so a title
    /// change / tab add-remove / max-width edit re-sizes the content exactly
    /// once. 0 on the top-bar / hidden path.
    pub(super) rail_reserved_cols: usize,
    /// F4-P4 seam drag: `true` while the left button is held after grabbing the
    /// rail's inner (content-facing) edge to resize it. Pointer motion then sets
    /// a manual width; release persists it. Only ever `true` while a rail is
    /// shown, so the top-bar / single-pane paths are unaffected.
    pub(super) rail_seam_drag: bool,
    /// F4-P4 double-click detection for the rail seam (reset-to-auto). Keyed on
    /// a fixed synthetic point so two quick seam presses register as a double-
    /// click; reset on an actual drag move so a drag-then-grab is not misread as
    /// one. Separate from the grid/rename trackers.
    pub(super) rail_seam_clicks: ClickTracker,
    /// Seam drag for the top tab bar's bottom edge (adjustable height): `true`
    /// while the left button is held after grabbing that edge. Pointer motion
    /// then sets a manual height in rows; release persists it. Only ever `true`
    /// while the top bar is shown, so the rail / no-chrome paths are unaffected.
    pub(super) tab_bar_seam_drag: bool,
    /// Double-click detection for the tab-bar bottom seam (reset-to-auto),
    /// mirroring `rail_seam_clicks` on the height axis. Keyed on a fixed
    /// synthetic point so two quick seam presses register as a double-click;
    /// reset on an actual drag move so a drag-then-grab is not misread as one.
    pub(super) tab_bar_seam_clicks: ClickTracker,
    /// Test-only clock injection for the next seam press. Production always
    /// reads the monotonic clock directly; deterministic tests consume this
    /// value once through `seam_click_instant`.
    #[cfg(test)]
    pub(super) seam_click_at_for_test: Option<Instant>,
    /// F4-P3 rail auto-hide timing state machine (ODP-4). Inert unless
    /// `tab_rail_autohide` is on and the chrome is a side rail; when active it
    /// drives the reveal/hide of the floating rail overlay from the pointer edge
    /// zone, keyboard flashes, and the debounce/grace timers. The reservation is
    /// removed (`tab_reserve` → NONE) the moment autohide is active, so reveal is
    /// a pure overlay and never reflows content.
    pub(super) rail_autohide: rail_autohide::RailAutohide,
    /// The previous physical pointer x fed to the reveal machine, for the
    /// motion-aware trigger: the segment from this to the current sample is
    /// tested against the edge zone so a fast approach that jumps clean over a
    /// static point zone still arms. `None` before the first sample and after the
    /// pointer leaves the window (so a re-entry never fabricates a segment across
    /// the whole surface). Only meaningful while auto-hide is active.
    pub(super) last_rail_pointer_px: Option<f64>,
    /// RAIL-DRAG: the in-flight drag-to-reorder gesture on a workspace rail
    /// slot, or `None` when no rail drag is active. Armed by a left press on a
    /// workspace slot, tracked on pointer motion (drop-target index), committed
    /// on release through the shipped `move_workspace` engine, and cancelled by
    /// Escape. Holds the rail auto-hide open for its lifetime (see
    /// `rail_pinned_open`).
    pub(super) rail_ws_drag: Option<RailWorkspaceDrag>,
    /// TOP-TAB-DRAG: the in-flight top-strip tab reorder gesture.
    pub(super) top_tab_drag: Option<TopTabDrag>,
    /// Whether the window currently holds focus. Blink pauses (cursor solid)
    /// while unfocused, matching common terminal behavior.
    pub(super) focused: bool,
    /// Button Protocol B3 focus-transfer exclusion (the Ghostty #11167 class):
    /// set on every focus gain, taken by the next content left press. That
    /// press — the click that activated the window — never latches a button;
    /// everything else about it (selection, opens, reports) is unchanged. A
    /// deliberate second click then works normally. Over-approximates for a
    /// keyboard focus gain followed by a click (that click is excluded too) —
    /// the safe side: a button fires a PTY write, so a click whose intent was
    /// "give this window focus" must never trigger one.
    pub(super) focus_click_pending: bool,
    /// Whether this unfocused episode has already requested platform user
    /// attention for a bell. Cleared when the window regains focus.
    pub(super) bell_attention: bell::BellAttentionLatch,
    /// Instant the context menu was last opened. A press that lands on the menu
    /// within [`CONTEXT_MENU_INPUT_DEBOUNCE`] of opening is swallowed: it can
    /// only be a stale queued click replaying (a human needs longer to see the
    /// menu, move to an item, and click). This hardens against the "phantom
    /// menu-item activation" seen when a burst of queued presses flushed into a
    /// freshly opened menu. `None` while no context menu is open.
    pub(super) context_menu_opened_at: Option<Instant>,
    /// Test-only: whether the last `open_context_menu` decided to run the
    /// interactive-path scan. Records the PATH-GATE decision at its exact site so
    /// a test can assert a chrome (rail/tab) right-click skips the stat-probing
    /// scan while a content right-click still runs it.
    #[cfg(test)]
    pub(super) last_menu_path_scan_for_test: bool,
    /// BELL visual-flash start instant, set when a bell is drained while the
    /// bell mode wants a visual flash. `None` when no flash is in flight (the
    /// off / urgent-only path), so the default render path emits no flash quad.
    pub(super) bell_flash_start: Option<Instant>,
    /// Monotonic epoch bumped once per rebuild while the bell flash is active so
    /// each animation frame reclassifies the render cache (the flash alpha moves
    /// while cell content does not). Constant while no flash is in flight.
    pub(super) bell_flash_epoch: u64,
    /// Transient native status line used for open failures and bounded neutral
    /// security notices. `None` on the idle path, so the default render path is
    /// byte-identical. Auto-expires after [`open_notice::NOTICE_DURATION`].
    pub(super) open_notice: Option<open_notice::OpenNotice>,
    /// OSC 52 write authority is native-window state: one bounded pending
    /// request, ephemeral per-PTY consent, and the neutral-notice rate limit.
    pub(super) osc52_write: osc52::Osc52WriteState,
    /// UX-A (Phase 11): in-memory, per-launch click-to-open discoverability
    /// state — the transient bottom-left "Ctrl+click to open" hint plus the
    /// mis-click bookkeeping that decides when to raise it. NOT persisted; resets
    /// every window launch. Idle (and byte-identity-irrelevant) on the default /
    /// feature-off path: the painter and signature both early-out when not shown.
    pub(super) click_hint: click_hint::ClickHintState,
    /// One reusable centered text HUD for bounded window-level feedback. Font
    /// zoom owns the current producer; resize feedback may share the same
    /// replace-in-place surface. `None` internally at rest, so it adds no paint
    /// or wake work on the default path.
    pub(super) transient_hud: transient_hud::TransientHud,
    /// Active IME pre-edit (composition) string as delivered by `winit`'s
    /// `Ime::Preedit`. Empty when no composition is in progress. Rendered inline
    /// at the terminal cursor; never sent to the PTY until the IME commits.
    pub(super) ime_preedit: String,
    /// Session that began the current IME composition. A delayed commit after
    /// activation must never be written to the newly focused PTY.
    pub(super) ime_session: Option<SessionToken>,
    #[cfg(test)]
    pub(super) focus_reports_for_test: Vec<(SessionToken, bool)>,
    #[cfg(test)]
    pub(super) osc52_background_empty_replies_for_test: usize,
    pub(super) autoclose: Option<Duration>,
    pub(super) deadline: Option<Instant>,
    /// OS-THEME: last known OS dark/light appearance preference, or `None` until
    /// the compositor surfaces one (always `None` on X11, where the signal is
    /// absent). Read only while [`Settings::follow_os_theme`] is on; off the
    /// default path it is never consulted.
    pub(super) os_theme: Option<winit::window::Theme>,
    /// CLOSE-CONFIRM: set when the confirmation dialog is accepted (or the
    /// non-confirming close path decides to exit) so `window_event` can exit the
    /// loop after the overlay outcome is applied — `apply_overlay_outcome` only
    /// has `&mut self` and cannot reach the `ActiveEventLoop` itself.
    pub(super) pending_exit: bool,
    /// Image paste-through confirm state (F6-i7). `Some` while a clipboard image
    /// pasted into a remote integrated tab awaits Enter/Esc: keys drive the
    /// prompt (Enter uploads, Esc/Ctrl+D cancels) instead of the shell. The
    /// image bytes are held here until confirmed so nothing leaves the machine
    /// on the paste keystroke alone.
    pub(super) pending_image_paste: Option<PendingImagePaste>,
    /// A background Test Connection probe (ODP-8) in flight from the Add / Edit
    /// connection form. The worker thread sends its tri-state result here and
    /// wakes a redraw; `run_about_to_wait_maintenance` drains it into the form.
    pub(super) connection_probe:
        Option<std::sync::mpsc::Receiver<Result<crate::ssh_connect::ProbeClass, String>>>,
    /// Test-only observation of what a confirmed image paste WOULD upload
    /// (session + PNG byte length), recorded instead of spawning a real `ssh`
    /// worker under `cfg(test)`. Lets the confirm-flow tests prove Enter commits
    /// and Esc cancels without touching the network.
    #[cfg(test)]
    pub(in crate::native) last_image_upload: Option<(SessionToken, usize)>,
    /// WHEEL-SENS: coalesces high-resolution wheel bursts (sub-notch
    /// `PixelDelta` events, fractional `LineDelta`) into discrete notches so one
    /// physical detent is one scroll/zoom step. Identity for a clean
    /// `LineDelta(_, ±1.0)`. Reset on focus loss and overlay open.
    pub(super) wheel_accum: WheelAccumulator,
    /// P1-8: macOS-only per-detent damper for the overlay list wheel path. Emits
    /// exactly one item per physical detent and absorbs the inertial momentum
    /// tail of a trackpad flick. Only consulted on the macOS handler branch of
    /// `handle_overlay_pointer_wheel`; idle (and byte-identity-irrelevant) on
    /// every other target. Reset alongside `wheel_accum` on focus loss and
    /// overlay open.
    pub(super) overlay_wheel: OverlayWheelDamper,
    /// Visible multi-session tab strip state. Presentation-only; the session
    /// model stays in `WorkspaceSet`.
    pub(super) tab_bar: TabBar,
    /// Vertical tab rail state (F4-V2 R1) — the sibling of `tab_bar`, active
    /// only when `tab_bar_placement` is a rail. Presentation-only.
    pub(super) tab_rail: TabRail,
    pub(super) rename_state: Option<RenameState>,
    /// F4-RENAME-MOUSE: double-click detection for the tab-rename field, kept
    /// separate from the terminal-grid `clicks` tracker so a rename word-select
    /// never interacts with a grid selection streak. Reset when a rename opens.
    pub(super) rename_clicks: ClickTracker,
    /// F4-RENAME-MOUSE: a left-button drag is in progress inside the rename
    /// field. Set on a press that lands on the input line, cleared on release
    /// (or when the rename closes). While set, pointer motion extends the
    /// field selection instead of doing any grid hover/selection work.
    pub(super) rename_dragging: bool,
    /// SLIDER-GUARD: whether the left mouse button is currently held while the
    /// overlay is open. Set on `MouseInput { Pressed, Left }` and cleared on
    /// `MouseInput { Released, Left }` through the overlay pointer path. Used to
    /// gate overlay slider drag moves so that cursor movements after the button
    /// is released can NEVER advance an armed drag — even if the drag state is
    /// somehow stale. `CursorMoved` carries no button state, so this flag is the
    /// reliable held-button seam for the settings-slider path (D-SLIDER-GUARD).
    pub(super) overlay_left_held: bool,
    /// Window-wide left-button state for chrome, divider, and scrollbar drag
    /// motion. Every left-button edge updates it before pointer routing, and
    /// focus loss clears it when the release may have landed in another window.
    /// `CursorMoved` carries no button state, so each non-grid drag checks this
    /// flag before advancing an in-flight latch.
    pub(super) pointer_left_held: bool,
    /// NF21-8 grid analogue of `overlay_left_held`: whether the left button is
    /// currently held during a terminal-grid text selection. Set when
    /// [`Self::begin_selection`] arms a drag, cleared on
    /// [`Self::finish_selection`], on focus loss, and at every active-session
    /// change. The motion path gates selection extension on it so a `Selecting`
    /// latch whose release was lost (mid-drag tab/workspace switch, or an
    /// alt-tab that delivers the release to another window) can NEVER extend
    /// without the button physically down. `CursorMoved` carries no button
    /// state, so this flag is the reliable held-button seam for the grid path.
    pub(super) grid_left_held: bool,
    /// INTERACTIVE-PATHS (Phase 7): the process `$HOME`, cached once at startup
    /// (it never changes mid-process) so `~`-prefixed path spans can be expanded
    /// at hover time without a per-move `getenv`. `None` when `$HOME` is unset or
    /// not valid UTF-8; only consulted while `interactive_paths` is on.
    pub(super) home_dir: Option<String>,
    /// C4 image viewer: the decoded RGBA buffer + dims for the image currently
    /// shown in the `ImageView` overlay, kept so a window resize can recompute
    /// the centered fit-rect without re-decoding. `None` whenever the viewer is
    /// closed; the per-frame [`Self::sync_image_overlay`] clears it (and the GPU
    /// overlay texture) once the overlay is no longer open.
    pub(super) image_overlay: Option<interactive_paths::ImageOverlayState>,
    /// WP2 autosave (sub-ODP 8c/8d): whether THIS instance may persist the
    /// workspace shape. Only the primary instance (the one holding the state-dir
    /// lock) autosaves or restores; a second concurrent window sets this `false`
    /// and never writes `workspaces.json`. Set once at startup.
    pub(super) autosave_is_primary: bool,
    /// Debounced-autosave deadline: `Some(t)` once a shape mutation is pending a
    /// write at `t`; re-armed on each further mutation so a burst coalesces into
    /// one write, cleared when the write fires. `None` at rest.
    pub(super) autosave_deadline: Option<Instant>,
    /// Last structural fingerprint the autosave observed. `None` until the first
    /// maintenance pass establishes the post-launch baseline (so restore's own
    /// shape does not trigger an immediate redundant write). A change from this
    /// value arms [`Self::autosave_deadline`].
    pub(super) autosave_fingerprint: Option<u64>,
    /// Test-only count of shape writes emitted, so the debounce-coalescing tests
    /// can assert exactly-once without touching the filesystem.
    #[cfg(test)]
    pub(super) autosave_saves: u32,
    pub(in crate::native) startup_error: Option<NativeError>,
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
    pub(in crate::native) fn new(
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

    /// Test-only constructor over a headless session source: no real PTY, OS
    /// child, pump thread, or wake pipe. Pure App/UI tests that need only a
    /// terminal model and a writable sink use this so they never inherit a real
    /// shell's synchronous kill+wait teardown (the macOS CI PTY-teardown wedge).
    /// The returned `App` owns a `SessionSource::Headless`; its backing state is
    /// reachable through the active session's `headless_session()` seam.
    #[cfg(test)]
    pub(in crate::native) fn new_headless(
        options: NativeOptions,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        headless: Arc<crate::native::session::HeadlessSession>,
        settings: Settings,
        settings_reloader: SettingsReloader,
    ) -> Self {
        let session = Session::new_headless(
            crate::native::session::SessionToken(0),
            terminal,
            writer,
            headless,
        );
        Self::new_with_sessions(
            options,
            WorkspaceSet::new(session, None),
            settings,
            settings_reloader,
        )
    }

    pub(in crate::native) fn new_with_sessions(
        options: NativeOptions,
        sessions: WorkspaceSet,
        settings: Settings,
        settings_reloader: SettingsReloader,
    ) -> Self {
        let grid = options.initial_grid;
        let hold_session = options.hold.then(|| sessions.active_id());
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
        let last_active_session = sessions.active_id();
        // `mut` is consumed only by the `cfg(not(test))` onboarding block below;
        // test builds compile that out, so silence the unused_mut there.
        #[cfg_attr(test, allow(unused_mut))]
        let mut app = Self {
            options,
            hold_session,
            held_exit: None,
            theme,
            effective_theme,
            cvd_cache,
            visual,
            window: None,
            gpu: None,
            cursor_icon: CursorIcon::Default,
            window_pointer_px: None,
            sessions,
            presentation_epoch: 0,
            prompt_marks_epoch: 0,
            grid,
            modifiers: Modifiers::default(),
            super_key: false,
            key_bindings,
            prefix_engine,
            last_active_session,
            settings,
            settings_reloader,
            pending_overlay_settings: None,
            themed_ui_roles,
            overlay,
            clipboard: NativeClipboard::default(),
            resize_debounce: ResizeDebouncer::new(RESIZE_DEBOUNCE_INTERVAL),
            skipped_frame_retry_deadline: None,
            skip_episode: SkipEpisode::default(),
            skip_escalation: SkipEscalation::default(),
            pending_surface_reconfigure: false,
            consecutive_skipped_frames: 0,
            redraws_delivered: 0,
            window_minimized: false,
            divider_drag: None,
            rail_reserved_cols: 0,
            rail_seam_drag: false,
            rail_seam_clicks: ClickTracker::default(),
            tab_bar_seam_drag: false,
            tab_bar_seam_clicks: ClickTracker::default(),
            #[cfg(test)]
            seam_click_at_for_test: None,
            rail_autohide: rail_autohide::RailAutohide::default(),
            last_rail_pointer_px: None,
            rail_ws_drag: None,
            top_tab_drag: None,
            // Assume focused at startup; the first `Focused` event corrects it.
            focused: true,
            // Startup counts as a focus gain: the very first click after
            // launch should not fire a button either.
            focus_click_pending: true,
            bell_attention: bell::BellAttentionLatch::default(),
            context_menu_opened_at: None,
            #[cfg(test)]
            last_menu_path_scan_for_test: false,
            bell_flash_start: None,
            bell_flash_epoch: 0,
            open_notice: None,
            osc52_write: osc52::Osc52WriteState::default(),
            click_hint: click_hint::ClickHintState::default(),
            transient_hud: transient_hud::TransientHud::default(),
            ime_preedit: String::new(),
            ime_session: None,
            #[cfg(test)]
            focus_reports_for_test: Vec::new(),
            #[cfg(test)]
            osc52_background_empty_replies_for_test: 0,
            autoclose,
            deadline: None,
            os_theme: None,
            pending_exit: false,
            pending_image_paste: None,
            connection_probe: None,
            #[cfg(test)]
            last_image_upload: None,
            wheel_accum: WheelAccumulator::default(),
            overlay_wheel: OverlayWheelDamper::default(),
            tab_bar: TabBar::default(),
            tab_rail: TabRail::default(),
            rename_state: None,
            rename_clicks: ClickTracker::default(),
            rename_dragging: false,
            overlay_left_held: false,
            pointer_left_held: false,
            grid_left_held: false,
            // D-10: resolve the interactive-paths `~` home through the shared
            // `restore_home_dir` helper so it uses `%USERPROFILE%` on Windows,
            // not a `HOME` that is normally unset there (which left `~`
            // expansion silently dead). `$HOME` still wins on Unix, unchanged.
            home_dir: crate::native::persistence::restore_home_dir()
                .and_then(|home| home.into_os_string().into_string().ok()),
            image_overlay: None,
            autosave_is_primary: false,
            autosave_deadline: None,
            autosave_fingerprint: None,
            #[cfg(test)]
            autosave_saves: 0,
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
}

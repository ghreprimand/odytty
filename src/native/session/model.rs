// SPDX-License-Identifier: GPL-3.0-only
//! Session, tab, and workspace model: the token arena and the structural
//! accessors over it.
//!
//! [`WorkspaceSet`] owns one flat arena of [`Session`]s keyed by
//! [`SessionToken`]; the workspace, tab, and pane trees carry tokens and active
//! indices only. Everything here is storage and lookup: dereferencing the set
//! resolves the focused pane of the active tab of the active workspace, and no
//! function in this module talks to a backend, mutates the tree's lifecycle, or
//! renders.

use super::presentation::CursorComparison;
use super::transport::{RemoteReconnect, RemoteUpload, SessionSource};
use crate::core::{LinkId, Snapshot, Terminal};
#[cfg(test)]
use crate::native::WindowPadding;
use crate::native::app::{CursorBlinkState, HintsUi, SynchronizedOutputHold};
use crate::native::copy_mode::CopyModeState;
use crate::native::layout::PaneNode;
use crate::native::output_recorder::RecorderHandle;
use crate::native::pty::{PtyWriter, UserEvent};
use crate::native::render_helpers::RenderSignature;
use crate::native::search_ui::SearchUi;
use crate::native::viewport::Viewport;
use crate::selection::{
    AbsoluteSelectionRange, AbsoluteSelectionState, CellPoint, ClickTracker, PointerDrag,
};
#[cfg(test)]
use crate::text::CellSize;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;
use winit::event_loop::EventLoopProxy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::native) struct SessionToken(pub(in crate::native) u64);

pub(in crate::native) struct Session {
    pub(in crate::native) id: SessionToken,
    pub(in crate::native) terminal: Arc<Mutex<Terminal>>,
    pub(in crate::native) writer: PtyWriter,
    pub(in crate::native) source: SessionSource,
    /// The terminal model changed while live pane dragging deliberately
    /// suppressed its backend resize. The next settlement flushes only sessions
    /// carrying this bit (or a newly changed geometry), then clears it after the
    /// backend accepts the final dimensions.
    pub(super) pty_resize_dirty: bool,
    /// The host session-id string this session was attached by (Phase 14), or
    /// `None` for a locally-spawned PTY. Drives attach dedup: selecting a
    /// session already open in a tab switches to it instead of appending a
    /// duplicate. Set only on the attached construction path; the local path
    /// leaves it `None`, so the default behavior is unchanged.
    pub(in crate::native) attached_session_id: Option<String>,
    pub(in crate::native) pump_thread: Option<JoinHandle<()>>,
    /// Bounded ring of recorded screen frames for the replay overlay (Phase 2).
    /// A clonable handle shared with this session's pump thread, which writes
    /// frames into it while recording is enabled (`session_replay`, default
    /// off). Empty and disabled by default, so it costs nothing on the plain
    /// path. For an attached session this handle exists but is not yet wired to
    /// the attach pump (recording an attached session is a documented
    /// follow-up), so it stays empty.
    pub(in crate::native) recorder: RecorderHandle,
    pub(in crate::native) tab_title: String,
    /// Transient OSC/completion/progress state owned by this exact pane.
    pub(in crate::native) attention: crate::native::notifications::PaneAttention,
    /// One-shot user authorization for the next explicit OSC 133 `D` edge.
    pub(in crate::native) notify_when_command_finishes: bool,
    /// Explicit one-shot monitors owned by this exact pane.
    pub(in crate::native) monitors: crate::native::notifications::PaneMonitors,
    pub(in crate::native) needs_rebuild: bool,
    pub(in crate::native) last_render_signature: Option<RenderSignature>,
    pub(in crate::native) synchronized_output_hold: SynchronizedOutputHold,
    /// Last GPU-presented snapshot. Single-pane tab chrome is included so a
    /// held blink or synchronized-output frame retains its rendered geometry.
    pub(in crate::native) last_presented_snapshot: Option<Snapshot>,
    /// Cursor and dimensions of the last terminal-content snapshot in
    /// undecorated grid coordinates. Cursor motion compares against this
    /// rather than the chrome-shifted render copy. Metadata only, by type:
    /// the comparison never reads cell content, so retaining a full snapshot
    /// clone here was a pure per-frame copy cost.
    pub(in crate::native) last_cursor_comparison_snapshot: Option<CursorComparison>,
    pub(in crate::native) last_presented_cursor_style: crate::core::CursorStyle,
    pub(in crate::native) last_presented_cursor_blinking: bool,
    pub(in crate::native) selection: AbsoluteSelectionState,
    pub(in crate::native) pointer_cell: Option<CellPoint>,
    /// INTERACTIVE-PATHS hover probe memo (security/efficiency): the
    /// `(pointer_cell, viewport offset, scrollback trim epoch)` for which
    /// `update_hover_path` last ran its filesystem stat probe. `CursorMoved`
    /// fires on every reported pointer motion, not once per cell, so without
    /// this the up-to-8 `symlink_metadata` syscalls re-run on every pixel of
    /// motion inside one character cell, and a path lexically under an autofs or
    /// stale-NFS mount could wedge the UI thread on every repeat. When the key
    /// is unchanged the probe is skipped entirely. `None` forces a recompute and
    /// is the resting state while `interactive_paths` is off.
    pub(in crate::native) hover_path_probe_key: Option<(CellPoint, usize, u64)>,
    pub(in crate::native) pointer_px: Option<(f64, f64)>,
    #[cfg(test)]
    pub(in crate::native) test_cell: Option<CellSize>,
    /// Headless multi-pane geometry seam: a `(surface_px, padding)` override so
    /// `multipane_geometry()` (and the divider hover/drag cursor path it feeds)
    /// can run without a GPU/window. `None` in production builds — the field
    /// only exists under `cfg(test)` — so the live path is unchanged.
    #[cfg(test)]
    pub(in crate::native) test_surface: Option<((u32, u32), WindowPadding)>,
    /// Headless scale-factor seam: a display scale override so DPI-aware pointer
    /// geometry (the F4-P3 rail reveal zone) can be exercised without a
    /// GPU/window. `None` in production (the field only exists under
    /// `cfg(test)`); the live path reads `GpuState::scale`.
    #[cfg(test)]
    pub(in crate::native) test_scale: Option<f32>,
    pub(in crate::native) hovered_hyperlink: Option<LinkId>,
    /// INTERACTIVE-PATHS (Phase 7): the path span currently under the pointer
    /// that resolved to a real filesystem entry, or `None`. Drives the pointer
    /// (hand) cursor exactly like `hovered_hyperlink`. Permanently `None` while
    /// the `interactive_paths` setting is off (the scanner is gated off before
    /// it can ever run), so the default hover path is byte-identical.
    pub(in crate::native) hovered_path: Option<crate::paths::Resolved>,
    /// UX-A (Phase 11): the visible-cell span of `hovered_path`, captured in the
    /// same hover computation so the Ctrl+hover armed underline can decorate
    /// exactly those cells without re-scanning the row at paint time. Kept in
    /// lockstep with `hovered_path` (set/cleared together); `None` whenever
    /// `hovered_path` is `None`, so it is permanently `None` while the feature is
    /// off and the default hover path is byte-identical.
    pub(in crate::native) hovered_path_cells:
        Option<crate::native::app::click_hint::HoverPathCells>,
    /// INTERACTIVE-URLS: the bare (non-OSC-8) URL currently under the pointer
    /// whose scheme is openable, or `None`. The full URI string to open; drives
    /// the pointer (hand) cursor and the Ctrl+click open exactly like an OSC 8
    /// hyperlink. Permanently `None` while the `interactive_urls` setting is off
    /// (the scanner is gated off before it runs), so the default hover path is
    /// byte-identical. Always `None` when the hovered cell already carries an
    /// OSC 8 hyperlink — that explicit path wins, so a cell is never
    /// double-decorated.
    pub(in crate::native) hovered_url: Option<String>,
    /// INTERACTIVE-URLS: the visible-cell span of `hovered_url`, captured in the
    /// same hover computation so the Ctrl+hover armed underline can decorate
    /// exactly those cells. Kept in lockstep with `hovered_url` (set/cleared
    /// together), so it is permanently `None` while the feature is off.
    pub(in crate::native) hovered_url_cells: Option<crate::native::app::click_hint::HoverPathCells>,
    /// Button Protocol chip hover: the LIVE button currently under the pointer,
    /// or `None`. Drives the pointer (hand) cursor and the chip's hovered
    /// visual state. Recomputed per pointer move, gated on the `buttons`
    /// setting before any terminal query, so the default hover path is
    /// byte-identical. Never holds an invalidated button (a dead chip is inert
    /// and must not invite a click).
    pub(in crate::native) hovered_button: Option<crate::core::ButtonHit>,
    /// Test seam (INTERACTIVE-PATHS): synthetic stat-gate so headless hover
    /// tests resolve path spans against an injected fs map, never the real
    /// filesystem. Production builds compile this out and use `FsResolveProbe`.
    #[cfg(test)]
    pub(in crate::native) test_path_probe: crate::native::app::interactive_paths::MapProbe,
    pub(in crate::native) pointer_drag: PointerDrag,
    pub(in crate::native) selection_block: bool,
    pub(in crate::native) drag_anchor_unit: Option<AbsoluteSelectionRange>,
    pub(in crate::native) clicks: ClickTracker,
    pub(in crate::native) last_selection_autoscroll: Option<Instant>,
    pub(in crate::native) report_button: Option<crate::core::MouseButton>,
    /// CTRL-CLICK-OPEN latch: `true` while the left button is held after a
    /// Ctrl/Cmd+click over a resolved span was intercepted and opened. The
    /// paired left release is then swallowed so a mouse-reporting app sees
    /// neither the press nor the release for that gesture (matching
    /// kitty/iTerm2/GNOME Terminal). Cleared at the start of every fresh left
    /// press, so a release lost to focus change never swallows a later click.
    pub(in crate::native) swallow_open_left_release: bool,
    /// Button Protocol B3 press latch: the button hit under a consumed plain
    /// left press, held until the paired release. The release fires the click
    /// report only when it resolves the SAME span (same id, viewport row, and
    /// start column) still `Live`; anything else — drag-off, scroll, block
    /// invalidation, a resize reflow — cancels silently. Cleared at the start
    /// of every fresh left press, so a release lost to a focus change never
    /// fires a stale button.
    pub(in crate::native) pressed_button: Option<crate::core::ButtonHit>,
    pub(in crate::native) viewport: Viewport,
    pub(in crate::native) search: SearchUi,
    pub(in crate::native) hints: Option<HintsUi>,
    pub(in crate::native) copy_mode: Option<CopyModeState>,
    pub(in crate::native) search_restore_viewport: Option<usize>,
    pub(in crate::native) last_scrollback_len: usize,
    /// Last scrollback front-trim epoch reconciled into absolute-coordinate UI
    /// state. A mismatch means row zero moved and stale selections cannot be
    /// trusted to name the same bytes.
    pub(in crate::native) last_scrollback_trim_epoch: u64,
    pub(in crate::native) cursor_blink: CursorBlinkState,
    pub(in crate::native) cursor_anim_alpha: f32,
    pub(in crate::native) cursor_ease_deadline: Option<Instant>,
    pub(in crate::native) cursor_ease_phase_on: bool,
    pub(in crate::native) cursor_ease_toggle_at: Option<Instant>,
    pub(in crate::native) cursor_anim_offset: [f32; 2],
    pub(in crate::native) cursor_slide_deadline: Option<Instant>,
    pub(in crate::native) cursor_slide_start: Option<Instant>,
    pub(in crate::native) cursor_slide_from_px: [f32; 2],
    pub(in crate::native) cursor_streak: crate::native::app::cursor_streak::CursorStreakState,
    pub(in crate::native) row_fade_starts: Vec<Option<Instant>>,
    pub(in crate::native) last_scrollback_len_for_fade: usize,
    pub(in crate::native) row_fade_epoch: u64,
    /// Sub-row scroll remainder in rows (SCROLL-FEEL Tier 2), invariant
    /// `(-1.0, 1.0)`; whole rows carry into `viewport`. Drives
    /// [`Self::scroll_frac_offset`]. `0.0` at rest.
    pub(in crate::native) scroll_frac_rows: f32,
    pub(in crate::native) scroll_frac_offset: f32,
    /// SCROLL-GLIDE forward-chase follower. `glide_visual` is the rendered
    /// viewport position in offset-rows; it eases toward the integer
    /// `viewport` offset while `glide_active`. `glide_target` is the logical
    /// offset being chased (a between-frame change of it, e.g. output growth,
    /// snaps the glide). `glide_last_tick` is the previous frame time for the
    /// frame-rate-independent step. Inactive/at rest: `glide_active == false`,
    /// `glide_visual == offset`, and the render path is byte-identical.
    pub(in crate::native) glide_visual: f32,
    pub(in crate::native) glide_active: bool,
    pub(in crate::native) glide_target: usize,
    pub(in crate::native) glide_last_tick: Option<Instant>,
    /// Remote reconnect anchor (F6-i4). `Some` only for sessions launched through
    /// the `ssh` connect path; `None` for a local shell, so exit classification
    /// and the reconnect prompt never engage for a local session. See
    /// [`RemoteReconnect`].
    pub(in crate::native) reconnect: Option<RemoteReconnect>,
    /// True while this remote session's link has dropped (`ssh` exit 255) and the
    /// in-pane reconnect prompt is showing. Keys drive the prompt (Enter to
    /// reconnect, Esc/Ctrl+D to dismiss) rather than the now-dead shell. Cleared
    /// on a successful reconnect or when the tab is closed.
    pub(in crate::native) awaiting_reconnect: bool,
    /// Image paste-through upload descriptor (F6-i7). `Some` only on a remote
    /// *integrated* ssh session; `None` for a local shell or an integration-off
    /// plain-ssh tab, so image paste-through never engages there. See
    /// [`RemoteUpload`].
    pub(in crate::native) upload: Option<RemoteUpload>,
    /// The remote host this session is connected to (RESTORE-REMOTE), or
    /// `None` for a local shell. Set by the `ssh` connect path to the
    /// saved-profile alias (when opened from a `hosts.conf` entry) or the
    /// literal `[user@]host[:port]` destination (ad-hoc). Captured into the
    /// shape snapshot so restore respawns the pane through the connect path
    /// instead of a local shell at the remote's cwd. Never set on a local
    /// session, so a local pane's capture/restore is unchanged.
    pub(in crate::native) remote_destination: Option<String>,
    /// Named launch profile that opened this local pane, if any. Captured into
    /// the workspace shape for faithful restore. Remote and attached panes keep
    /// this `None`.
    pub(in crate::native) launch_profile: Option<String>,
    /// Authored theme this session's profile selected (its `appearance.theme`),
    /// if the launch profile set one. Held as session state so a global theme
    /// sweep (settings write, OS-appearance flip, restore seed) re-derives per
    /// session instead of flattening a profile tab to the global theme, and so
    /// the window chrome can present the profile theme while this pane is active.
    /// `None` for a plain tab or a profile that inherits the global theme; the
    /// authored (non-CVD) theme is stored so the current CVD mode/strength apply
    /// on top at each sweep. Re-resolved on restore from `launch_profile`, so it
    /// need not be persisted separately.
    pub(in crate::native) profile_theme: Option<crate::theme::Theme>,
}

/// One tab in the strip. It owns a layout tree of panes (a binary
/// [`PaneNode`]) and tracks which pane within the tab is focused. A fresh tab
/// is a single [`PaneNode::Leaf`], which the render/resize paths treat
/// byte-identically to today's single-session window (design doc §2.3). Pane
/// splitting is wired in later work; for now every tab is a single
/// leaf, so `tabs.len()` equals the session count and behaviour is unchanged.
pub(in crate::native) struct Tab {
    pub(in crate::native) layout: PaneNode,
    pub(in crate::native) focused: SessionToken,
    /// Optional user-assigned tab name (the Phase-0 rename feature). When set it
    /// overrides the focused pane's shell-derived title in the tab strip. Once a
    /// tab can hold several panes the name is no longer 1:1 with a session, so
    /// the override lives on the tab, not the session (design doc §2.4/§9.5).
    pub(in crate::native) title_override: Option<String>,
    /// Zoom / toggle-fullscreen-pane state (tmux `Ctrl-b z`, §7 K2-zoom). When
    /// `true` the focused pane is rendered full-bleed across the whole content
    /// rect while the **layout tree underneath is preserved**, so un-zoom
    /// restores the exact prior geometry. A structural change (split, close,
    /// equalize) clears it. Zoom on a single-pane tab is meaningless and never
    /// set (the toggle is a no-op there), but every zoom-aware path also guards
    /// on pane count so a stray flag can never perturb the single-pane render.
    pub(in crate::native) zoomed: bool,
    /// Unseen-activity latch for the rollup indicator (NF21-6 / ODP-6 v2). Set
    /// when a bell rings in one of this tab's panes while the tab is NOT the
    /// active-visible tab; cleared once the tab is viewed (it is the active tab
    /// of the active workspace). Tab granularity is the finest useful rollup
    /// unit; workspace-level activity is DERIVED from its tabs
    /// ([`WorkspaceSet::workspace_has_activity`]) rather than stored twice. The
    /// rollup UI that renders this flag is deferred to a later cycle; for now
    /// only the signal is landed and maintained, so it has no reader yet.
    #[allow(dead_code)]
    pub(in crate::native) activity: bool,
}

impl Tab {
    /// A single-pane tab wrapping one session.
    pub(super) fn single(token: SessionToken) -> Self {
        Self {
            layout: PaneNode::leaf(token),
            focused: token,
            title_override: None,
            zoomed: false,
            activity: false,
        }
    }

    /// True when this tab should render/resize as a single full-bleed pane: it
    /// is in zoom mode AND zoom is meaningful (multi-pane, focused leaf present).
    /// A zoomed single-pane tab is impossible (the toggle is a no-op there), but
    /// the pane-count guard keeps the single-pane fast path byte-identical even
    /// if the flag were ever set spuriously.
    pub(super) fn is_effectively_zoomed(&self) -> bool {
        self.zoomed && !self.layout.is_single_pane() && self.layout.contains(self.focused)
    }
}

/// One workspace: a named, ordered list of tabs with a focused (active) tab
/// (design doc §3.1). The workspace layer sits ABOVE tabs — a [`WorkspaceSet`]
/// owns an ordered list of these plus the single, flat session arena that every
/// tab's panes reference by token. Per the §3.3 naming hazard this layer is
/// never called a "session".
pub(in crate::native) struct Workspace {
    /// User-visible, renameable label; defaults to "Workspace N". Read by the
    /// command palette / keyboard layer and the workspace-rail chrome.
    pub(in crate::native) name: String,
    /// The tabs of this workspace, in strip order. `Tab` is unchanged.
    pub(in crate::native) tabs: Vec<Tab>,
    /// Index into `tabs` of the focused tab.
    pub(in crate::native) active_tab: usize,
    /// The host alias this workspace is bound to (F6-W5, ODP-9). When `Some`, a
    /// New Tab opened while this workspace is active routes through the remote
    /// connect path for that host instead of spawning a local shell; the
    /// "New Local Tab" escape hatch always spawns a local shell regardless.
    /// `None` (the default) is byte-identical to the pre-W5 local-only behavior.
    pub(in crate::native) default_profile: Option<String>,
    /// Optional named launch profile bound to this workspace (v0.14). When set,
    /// New Tab uses the profile resolver unless a host binding wins. Distinct
    /// from [`Self::default_profile`], which remains a connection-host alias.
    pub(in crate::native) launch_profile: Option<String>,
}

impl Workspace {
    /// A fresh workspace wrapping a single single-pane tab for `token`.
    pub(super) fn single(name: String, token: SessionToken) -> Self {
        Self {
            name,
            tabs: vec![Tab::single(token)],
            active_tab: 0,
            default_profile: None,
            launch_profile: None,
        }
    }
}

/// The generated default label for the workspace that will sit at zero-based
/// rail `index` ("Workspace 1", "Workspace 2", …). Kept in one place so the
/// spawn sites and the PRISTINE-CONSUME default-name check
/// ([`WorkspaceSet::is_single_pristine_workspace`]) can never disagree about
/// what an untouched, never-renamed workspace is called.
pub(super) fn default_workspace_name(index: usize) -> String {
    format!("Workspace {}", index + 1)
}

/// The workspace list and the session arena that backs it (design doc §3.1).
///
/// The hierarchy is `WorkspaceSet` -> [`Workspace`] -> [`Tab`] -> pane. Sessions
/// live in ONE arena keyed by [`SessionToken`] so pump-thread lookup by token
/// stays O(1) and never has to walk the hierarchy (§5 rule 1); the workspace /
/// tab / pane tree carries only presentation and focus. Splitting `tabs` /
/// `active_tab` out of the set and into [`Workspace`] is a one-level push-down:
/// the arena, token counter, and every global toggle stay on the set, so the
/// ~35 `Deref` sites and all keyboard/cursor/selection paths are untouched by
/// construction. `Deref`/`DerefMut` resolve to the focused pane of the active
/// tab of the ACTIVE workspace. With a single workspace this is behaviourally
/// identical to the previous single-`Vec<Tab>` model.
pub(in crate::native) struct WorkspaceSet {
    pub(in crate::native) sessions: HashMap<SessionToken, Session>,
    pub(in crate::native) workspaces: Vec<Workspace>,
    pub(super) active_ws: usize,
    pub(super) next_token: u64,
    pub(super) proxy: Option<EventLoopProxy<UserEvent>>,
    /// Whether output recording is currently enabled (`session_replay`). Newly
    /// spawned sessions inherit this so recording follows the live setting;
    /// [`Self::set_recording_enabled`] fans a toggle out to every session's
    /// recorder handle. Default off ⇒ the plain path is untouched.
    pub(super) recording_enabled: bool,
    /// Local hostname injected into every terminal model so OSC 7
    /// `file://host/path` URLs from the local shell can update cwd while remote
    /// hosts remain rejected by the core.
    pub(super) local_hostname: Option<String>,
    /// Whether newly spawned local default shells should receive OdyTTY's OSC
    /// 133 integration wrapper. Existing sessions are not modified.
    pub(super) shell_integration_enabled: bool,
}

impl Deref for WorkspaceSet {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.active()
    }
}

impl DerefMut for WorkspaceSet {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.active_mut()
    }
}

impl WorkspaceSet {
    pub(in crate::native) fn new(
        initial: Session,
        proxy: Option<EventLoopProxy<UserEvent>>,
    ) -> Self {
        let token = initial.id;
        let next_token = token.0.saturating_add(1);
        let mut sessions = HashMap::new();
        sessions.insert(token, initial);
        Self {
            sessions,
            workspaces: vec![Workspace::single(default_workspace_name(0), token)],
            active_ws: 0,
            next_token,
            proxy,
            recording_enabled: false,
            local_hostname: None,
            shell_integration_enabled: false,
        }
    }

    /// The active workspace. A set never holds zero workspaces (the last one
    /// closing exits the app), and `active_ws` is kept in range by every
    /// workspace-removing path; the fallback to the first workspace mirrors
    /// `active_focused_token`'s defensive lookup so a stray index can never
    /// panic.
    pub(super) fn active_workspace(&self) -> &Workspace {
        self.workspaces
            .get(self.active_ws)
            .or_else(|| self.workspaces.first())
            .expect("WorkspaceSet always holds at least one workspace")
    }

    pub(super) fn active_workspace_mut(&mut self) -> &mut Workspace {
        let idx = if self.active_ws < self.workspaces.len() {
            self.active_ws
        } else {
            0
        };
        self.workspaces
            .get_mut(idx)
            .expect("WorkspaceSet always holds at least one workspace")
    }

    /// The active tab of the active workspace, if the workspace has one.
    pub(super) fn active_tab_ref(&self) -> Option<&Tab> {
        let ws = self.active_workspace();
        ws.tabs.get(ws.active_tab)
    }

    pub(super) fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let ws = self.active_workspace_mut();
        ws.tabs.get_mut(ws.active_tab)
    }

    /// Locate the `(workspace index, tab index)` of the tab whose layout tree
    /// contains `token`, scanning ALL workspaces. Pane close / shell-exit reap a
    /// token that may live in a background workspace, and attach dedup (ODP-10)
    /// deep-switches to whichever workspace owns a token - both need the full
    /// scan, not the active workspace alone.
    pub(in crate::native) fn locate_token(&self, token: SessionToken) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(ws_idx, ws)| {
            ws.tabs
                .iter()
                .position(|tab| tab.layout.contains(token))
                .map(|tab_idx| (ws_idx, tab_idx))
        })
    }

    /// Number of workspaces. The app's close-tab exit guard keys on this: the
    /// last tab of the last workspace exits, but the last tab of a non-last
    /// workspace merely closes that workspace.
    pub(in crate::native) fn workspace_count(&self) -> usize {
        self.workspaces.len()
    }

    /// PRISTINE-CONSUME: true when the whole set is exactly one untouched,
    /// freshly-spawned workspace — the state a bare launch produces. Used at
    /// open-layout time to decide whether an append should CONSUME this default
    /// workspace (replace it) rather than leave it beside the restored set, so a
    /// layout opened onto a fresh window yields exactly what was saved.
    ///
    /// Judged on SHAPE facts only — never shell activity: one workspace still
    /// bearing its generated ([`default_workspace_name`]) name, no host binding,
    /// a single tab with no title override, and that tab a single leaf pane (no
    /// splits). ANY real state — a second workspace, a rename, a host binding, a
    /// split, or an extra tab — is not pristine and appends as before.
    pub(in crate::native) fn is_single_pristine_workspace(&self) -> bool {
        if self.workspaces.len() != 1 {
            return false;
        }
        let ws = &self.workspaces[0];
        ws.name == default_workspace_name(0)
            && ws.default_profile.is_none()
            && ws.tabs.len() == 1
            && ws.tabs[0].title_override.is_none()
            && ws.tabs[0].layout.is_single_pane()
    }

    /// The token of the focused pane of the active tab - the `Deref` target.
    pub(super) fn active_focused_token(&self) -> SessionToken {
        let ws = self.active_workspace();
        ws.tabs
            .get(ws.active_tab)
            .or_else(|| ws.tabs.first())
            .map(|tab| tab.focused)
            .unwrap_or(SessionToken(0))
    }

    pub(in crate::native) fn active(&self) -> &Session {
        let token = self.active_focused_token();
        self.sessions
            .get(&token)
            .or_else(|| self.sessions.values().next())
            .expect("WorkspaceSet always holds at least one session while active() is called")
    }

    pub(in crate::native) fn active_mut(&mut self) -> &mut Session {
        let token = self.active_focused_token();
        if self.sessions.contains_key(&token) {
            return self
                .sessions
                .get_mut(&token)
                .expect("token presence just checked");
        }
        self.sessions
            .values_mut()
            .next()
            .expect("WorkspaceSet always holds at least one session while active_mut() is called")
    }

    pub(in crate::native) fn active_id(&self) -> SessionToken {
        self.active_focused_token()
    }

    /// A clone of the event-loop proxy, for background workers that need to wake
    /// a redraw when they finish (e.g. the Test Connection probe). `None` in
    /// headless / test builds without a real event loop.
    pub(in crate::native) fn event_proxy(&self) -> Option<EventLoopProxy<UserEvent>> {
        self.proxy.clone()
    }

    #[cfg(test)]
    pub(in crate::native) fn active_position(&self) -> usize {
        self.active_workspace().active_tab
    }

    pub(in crate::native) fn get_mut(&mut self, token: SessionToken) -> Option<&mut Session> {
        self.sessions.get_mut(&token)
    }

    /// Read access to a session by token (multi-pane render dispatch snapshots
    /// each visible pane's terminal through this).
    pub(in crate::native) fn get(&self, token: SessionToken) -> Option<&Session> {
        self.sessions.get(&token)
    }

    /// Every session, in tab order (and, within a tab, tree order). For
    /// single-pane tabs this is exactly the old `Vec<Session>` order, so
    /// position-indexed callers (resize, scrollback cap, test seams) are
    /// unchanged; it still visits every pane once.
    pub(in crate::native) fn iter(&self) -> impl Iterator<Item = &Session> {
        self.workspaces
            .iter()
            .flat_map(|ws| ws.tabs.iter())
            .flat_map(move |tab| {
                tab.layout
                    .leaves()
                    .into_iter()
                    .filter_map(move |token| self.sessions.get(&token))
            })
    }

    #[cfg(test)]
    pub(in crate::native) fn len(&self) -> usize {
        self.sessions.len()
    }

    pub(in crate::native) fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// The focused-pane token of the tab at `position` in the strip.
    pub(in crate::native) fn token_at_position(&self, position: usize) -> Option<SessionToken> {
        self.active_workspace()
            .tabs
            .get(position)
            .map(|tab| tab.focused)
    }

    /// The strip index of the tab that contains `token` as one of its panes.
    pub(in crate::native) fn position_of_token(&self, token: SessionToken) -> Option<usize> {
        self.active_workspace()
            .tabs
            .iter()
            .position(|tab| tab.layout.contains(token))
    }

    /// The token of an already-open session attached by host id `session_id`, if
    /// any (Phase 14 attach dedup). Scans every session for an attached one whose
    /// recorded host id matches, so a re-selected session can switch to its
    /// existing tab instead of appending a duplicate. `None` when no open tab
    /// mirrors that host.
    pub(in crate::native) fn find_attached_tab(&self, session_id: &str) -> Option<SessionToken> {
        self.sessions
            .iter()
            .find(|(_, session)| session.attached_session_id.as_deref() == Some(session_id))
            .map(|(token, _)| *token)
    }

    #[cfg(test)]
    pub(in crate::native) fn push(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.insert(id, session);
        self.active_workspace_mut().tabs.push(Tab::single(id));
        id
    }

    /// Insert `session` into the arena and append it as a brand-new
    /// single-pane workspace (test-only), WITHOUT switching to it — the
    /// workspace-level analogue of [`Self::push`], so headless tests can build
    /// multi-workspace sets without an event-loop proxy for `new_workspace`'s
    /// PTY spawn.
    #[cfg(test)]
    pub(in crate::native) fn push_workspace(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.insert(id, session);
        let name = default_workspace_name(self.workspaces.len());
        self.workspaces.push(Workspace::single(name, id));
        id
    }

    /// Insert a session into the arena **without** a tab (test-only), so headless
    /// tests can drive [`Self::split_active_with`] — the pure tree-mutation half
    /// of a split — without spawning a real PTY for the new pane.
    #[cfg(test)]
    pub(super) fn push_arena_only(&mut self, session: Session) -> SessionToken {
        let id = session.id;
        self.next_token = self.next_token.max(id.0.saturating_add(1));
        self.sessions.insert(id, session);
        id
    }

    /// True when the active tab is rendering one pane full-bleed (zoom mode).
    /// Drives the render path's divider suppression and the redraw decision.
    pub(in crate::native) fn active_is_zoomed(&self) -> bool {
        self.active_tab_ref()
            .map(Tab::is_effectively_zoomed)
            .unwrap_or(false)
    }

    /// The active tab's pane layout tree (for the render/geometry layer).
    pub(in crate::native) fn active_layout(&self) -> Option<&PaneNode> {
        self.active_tab_ref().map(|tab| &tab.layout)
    }

    /// Number of panes in the active tab (1 ⇒ the byte-identical single path).
    #[allow(dead_code)]
    pub(in crate::native) fn active_pane_count(&self) -> usize {
        self.active_tab_ref()
            .map(|tab| tab.layout.pane_count())
            .unwrap_or(1)
    }

    /// True when the active tab holds exactly one pane — the byte-identical
    /// render/resize fast path (design doc §2.3).
    pub(in crate::native) fn active_is_single_pane(&self) -> bool {
        self.active_tab_ref()
            .map(|tab| tab.layout.is_single_pane())
            .unwrap_or(true)
    }

    /// True when there is exactly one tab and it carries a custom
    /// `title_override` (F4 ODP-7 / F4-NF1). The tab bar's show rule uses this
    /// so a single renamed "workflow" tab is visible even below the usual
    /// two-tab threshold.
    pub(in crate::native) fn lone_tab_has_title_override(&self) -> bool {
        let ws = self.active_workspace();
        ws.tabs.len() == 1
            && ws
                .tabs
                .first()
                .is_some_and(|tab| tab.title_override.is_some())
    }

    /// The active workspace index (rail highlight / palette current-marker).
    pub(in crate::native) fn active_workspace_index(&self) -> usize {
        self.active_ws
    }

    /// The display name of the workspace at rail index `idx`, or `None` when out
    /// of range.
    pub(in crate::native) fn workspace_name(&self, idx: usize) -> Option<&str> {
        self.workspaces.get(idx).map(|ws| ws.name.as_str())
    }

    /// The display names of every workspace, in rail order. Feeds the command
    /// palette's per-workspace "switch to …" rows (W3); the index into this list
    /// is the [`Self::switch_workspace`] target.
    pub(in crate::native) fn workspace_names(&self) -> Vec<String> {
        self.workspaces.iter().map(|ws| ws.name.clone()).collect()
    }

    /// The host alias the active workspace is bound to (F6-W5 / ODP-9), or
    /// `None` when it is a plain local workspace. `handle_new_tab` routes New Tab
    /// through the remote connect path when this is `Some`.
    pub(in crate::native) fn active_workspace_default_profile(&self) -> Option<&str> {
        self.active_workspace().default_profile.as_deref()
    }

    /// The named launch profile bound to the active workspace, if any.
    pub(in crate::native) fn active_workspace_launch_profile(&self) -> Option<&str> {
        self.active_workspace().launch_profile.as_deref()
    }

    /// Clear any workspace-scoped launch-profile override that names `profile`,
    /// across every workspace in the set. Used when the named profile is deleted
    /// so a stale binding cannot outlive it. Returns whether any override was
    /// cleared so the caller can persist the change.
    pub(in crate::native) fn clear_launch_profile_named(&mut self, profile: &str) -> bool {
        let mut changed = false;
        for workspace in &mut self.workspaces {
            if workspace.launch_profile.as_deref() == Some(profile) {
                workspace.launch_profile = None;
                changed = true;
            }
        }
        changed
    }

    /// Rewrite any workspace-scoped launch-profile override naming `old` to
    /// `new`, across every workspace. Used when a named profile is renamed so a
    /// workspace binding follows the rename. Returns whether anything changed.
    pub(in crate::native) fn rename_launch_profile(&mut self, old: &str, new: &str) -> bool {
        let mut changed = false;
        for workspace in &mut self.workspaces {
            if workspace.launch_profile.as_deref() == Some(old) {
                workspace.launch_profile = Some(new.to_owned());
                changed = true;
            }
        }
        changed
    }

    /// The host alias the workspace at rail index `idx` is bound to (RAIL-BIND),
    /// or `None` when out of range or unbound. Read for the rail context menu's
    /// Bind/Unbind conditional, which targets the CLICKED slot rather than the
    /// active workspace.
    pub(in crate::native) fn workspace_default_profile_at(&self, idx: usize) -> Option<&str> {
        self.workspaces.get(idx)?.default_profile.as_deref()
    }
}

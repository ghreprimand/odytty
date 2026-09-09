// SPDX-License-Identifier: GPL-3.0-only
//! Cross-platform quick terminal: a dedicated, summonable drop-down terminal
//! window with its own identity, geometry, monitor policy, and summon/hide
//! lifecycle, kept distinct from ordinary windows and ordinary window
//! restoration (v0.15.0 A).
//!
//! Scope of this module (the parts that are pure, deterministic, and unit
//! testable headlessly):
//!
//! - **Identity** ([`QuickTerminalIdentity`]): the quick terminal is a single
//!   dedicated window, never one of the ordinary windows the process restores at
//!   startup. It is created lazily on first summon and excluded from ordinary
//!   session save/restore, so enabling it never delays the first usable default
//!   local terminal.
//! - **Geometry and policy** ([`QuickTerminalGeometry`], [`QuickTerminalEdge`],
//!   [`QuickTerminalExtent`], [`MonitorPolicy`]): where the window anchors, how
//!   much of the monitor it covers, and which monitor it uses, computed as a
//!   pure function of a monitor work area.
//! - **Settings** ([`QuickTerminalSettings`]) with accessibility-safe defaults:
//!   the feature, reveal motion, and hide-on-focus-loss default off. Reduced
//!   motion forces instant reveal even when slide is explicitly selected.
//! - **Lifecycle** ([`QuickTerminalController`]): the summon/hide/toggle state
//!   machine, lazy singleton creation, repeated-summon de-duplication (never a
//!   second quick window), focus-loss hide policy, and preserve-on-hide.
//! - **Shortcut capability seam** ([`GlobalShortcutAdapter`],
//!   [`ShortcutRegistration`]): a platform-honest registration contract that
//!   reports Registered / Unsupported / Unavailable as DISTINCT outcomes and
//!   never claims a registration it cannot confirm. Unsupported carries an
//!   actionable message (e.g. Wayland without a portal), never a silent failure.
//!
//! Owned by the live host wiring (`app::multi_window_host`), not this module:
//! creating the real winit surface, and driving the reveal slide across frames.
//! This module DOES own the reveal timeline ([`RevealTimeline`], a pure,
//! testable interpolation the host samples each tick) and the OS key-grab
//! backends: X11 `XGrabKey` ([`x11`]), Windows `RegisterHotKey` ([`windows`]),
//! and macOS `RegisterEventHotKey` ([`macos`]), each in a per-OS submodule.
//! Wayland has no in-process global grab (the compositor owns it); the adapter
//! reports an actionable Unsupported. The supported `xdg-desktop-portal`
//! `GlobalShortcuts` route is modeled as a pure, unit-tested protocol seam
//! ([`wayland`]); its live D-Bus transport is a separate, dependency-gated
//! follow-up that wires directly onto that seam.
//! A `ShortcutRegistration::Registered` is returned only after the OS confirms
//! the grab, so nothing ever falsely claims a working global shortcut.

use super::window_owner::ProcessWindowId;

// Platform global-shortcut backends live in per-OS submodules so all platform
// FFI is isolated and this core file stays within the source-size budget
// (v0.15.0 A). Each is compiled only on its platform; the key-code mappers
// (`x11_keysym` / `windows_vk` / `macos_keycode`) and the display-server
// detection stay here, cross-compiled and unit-tested on every host.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

/// Identity of the one dedicated quick-terminal window. Distinct from the
/// ordinary windows the process opens and restores: there is at most one quick
/// terminal, it is created lazily on first summon, and it is never written to or
/// read from ordinary session restoration state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct QuickTerminalIdentity {
    window: ProcessWindowId,
}

impl QuickTerminalIdentity {
    pub(in crate::native) fn new(window: ProcessWindowId) -> Self {
        Self { window }
    }

    pub(in crate::native) fn window(&self) -> ProcessWindowId {
        self.window
    }

    /// The quick terminal is never part of ordinary window restoration: it is
    /// summoned on demand, not reopened at startup, so it cannot delay the first
    /// usable default local terminal. Always `false`.
    // Asserted by the restoration-exclusion test; a staged status accessor for
    // the section-B non-sensitive status surface, not yet called in production.
    #[allow(dead_code)]
    pub(in crate::native) fn is_restorable(&self) -> bool {
        false
    }
}

/// Which monitor edge the quick terminal anchors to. Top is the conventional
/// drop-down ("quake") placement and the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::native) enum QuickTerminalEdge {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl QuickTerminalEdge {
    /// Map the normalized `quick_terminal_edge` setting string to an edge. The
    /// settings layer already validates and normalizes the value (warning and
    /// falling back to `top` on anything unrecognized), so an unexpected string
    /// here defaults to `Top` rather than failing.
    pub(in crate::native) fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "bottom" => Self::Bottom,
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Top,
        }
    }

    /// True when the edge anchors along the horizontal axis (Top/Bottom span the
    /// monitor width and cover a fraction of its height); false for Left/Right,
    /// which span the height and cover a fraction of the width.
    fn is_horizontal(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }
}

/// How large the quick terminal is along its coverage axis: a fraction of the
/// monitor work area, or an absolute pixel size. Fractions are the accessible
/// default because they scale with the display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::native) enum QuickTerminalExtent {
    /// A fraction in `(0.0, 1.0]` of the monitor work area along the axis.
    Fraction(f32),
    /// An absolute size in physical pixels, clamped to the work area.
    Pixels(u32),
}

impl QuickTerminalExtent {
    /// Map a normalized extent setting string (`NN%` or `NNpx`, as produced by
    /// the settings layer) to an extent. A percentage becomes a `Fraction`; a
    /// pixel value becomes `Pixels`. `fallback` is used when the string cannot
    /// be interpreted, which the settings layer already guards against.
    pub(in crate::native) fn from_setting(value: &str, fallback: Self) -> Self {
        let trimmed = value.trim().to_ascii_lowercase();
        if let Some(pct) = trimmed.strip_suffix('%') {
            if let Ok(v) = pct.trim().parse::<f32>()
                && v.is_finite()
                && v > 0.0
            {
                return Self::Fraction((v / 100.0).clamp(0.01, 1.0));
            }
            return fallback;
        }
        let digits = trimmed.strip_suffix("px").unwrap_or(&trimmed);
        match digits.trim().parse::<u32>() {
            Ok(px) if px > 0 => Self::Pixels(px),
            _ => fallback,
        }
    }

    /// Resolve to a concrete pixel size along an axis of length `available`,
    /// clamped to `1..=available`. A fraction is clamped to `(0, 1]` first so a
    /// malformed setting can never produce a zero-size or overlarge window.
    fn resolve(self, available: u32) -> u32 {
        let available = available.max(1);
        match self {
            Self::Fraction(f) => {
                let f = if f.is_finite() {
                    f.clamp(0.01, 1.0)
                } else {
                    1.0
                };
                let px = (f64::from(available) * f64::from(f)).round() as u32;
                px.clamp(1, available)
            }
            Self::Pixels(px) => px.clamp(1, available),
        }
    }
}

/// Which monitor the quick terminal appears on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::native) enum MonitorPolicy {
    /// Default policy: the monitor of the FOCUSED ordinary window - the monitor
    /// the user is on. When no ordinary window reports focus (focus is on
    /// another application) it falls back to any live ordinary window's monitor,
    /// then the primary. Pointer-position selection is intentionally not used:
    /// winit exposes no cross-platform global pointer location, so window focus
    /// is the portable "where the user is" signal.
    #[default]
    ActiveMonitor,
    /// Always the primary monitor.
    Primary,
    /// A fixed monitor index; if that monitor is gone at summon time the host
    /// falls back to the active monitor (never a silent no-show).
    Index(usize),
}

impl MonitorPolicy {
    /// Map the normalized `quick_terminal_monitor` setting string to a policy.
    /// `active`/`primary` map to their variants; a bare integer maps to
    /// `Index`. Anything else defaults to `ActiveMonitor`, matching the
    /// settings-layer fallback.
    pub(in crate::native) fn from_setting(value: &str) -> Self {
        let trimmed = value.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "primary" => Self::Primary,
            "active" => Self::ActiveMonitor,
            _ => match trimmed.parse::<usize>() {
                Ok(index) => Self::Index(index),
                Err(_) => Self::ActiveMonitor,
            },
        }
    }
}

/// Reveal animation policy. Collapses to `Instant` under reduced-motion.
/// Motion defaults off per the v0.15.0 foundation contract, so `Instant` is the
/// default variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::native) enum QuickTerminalAnimation {
    /// Appear instantly with no motion (the default). Always used under
    /// reduced-motion.
    #[default]
    Instant,
    /// Slide the window in from its anchored edge (opt-in).
    Slide,
}

impl QuickTerminalAnimation {
    /// Map the normalized `quick_terminal_animation` setting string to a policy.
    /// Only an explicit `slide` maps to `Slide`; everything else (including the
    /// `instant` default the settings layer emits for unrecognized input) maps
    /// to `Instant`, keeping motion off unless the user opts in. Reduced motion
    /// still forces `Instant` downstream regardless of this value.
    pub(in crate::native) fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "slide" => Self::Slide,
            _ => Self::Instant,
        }
    }
}

/// A monitor work area in physical pixels (already excluding panels/docks where
/// the platform reports them). The origin is the monitor's top-left in the
/// virtual desktop coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct MonitorRect {
    pub(in crate::native) x: i32,
    pub(in crate::native) y: i32,
    pub(in crate::native) width: u32,
    pub(in crate::native) height: u32,
}

/// The computed physical placement of the quick-terminal window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct QuickTerminalGeometry {
    pub(in crate::native) x: i32,
    pub(in crate::native) y: i32,
    pub(in crate::native) width: u32,
    pub(in crate::native) height: u32,
}

/// User-facing quick-terminal configuration. Defaults are accessibility-safe
/// and match the v0.15.0 foundation contract: the feature is OFF (opt-in),
/// motion defaults off (and is additionally forced off under reduced motion),
/// and hide-on-focus-loss defaults off so the window stays put until summoned
/// away.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::native) struct QuickTerminalSettings {
    /// Whether the quick terminal is enabled at all. Default `false`: no global
    /// shortcut is registered and no dedicated window is created until the user
    /// opts in, so startup readiness is unchanged.
    pub(in crate::native) enabled: bool,
    /// Human-readable global shortcut accelerator, e.g. `"F12"` or
    /// `"ctrl+shift+grave"`. Only meaningful when `enabled`.
    pub(in crate::native) shortcut: String,
    /// Which edge the window anchors to.
    pub(in crate::native) edge: QuickTerminalEdge,
    /// Coverage along the edge's axis (height for Top/Bottom, width for
    /// Left/Right).
    pub(in crate::native) coverage: QuickTerminalExtent,
    /// Span along the other axis (width for Top/Bottom, height for Left/Right).
    pub(in crate::native) span: QuickTerminalExtent,
    /// Which monitor the window uses.
    pub(in crate::native) monitor: MonitorPolicy,
    /// Hide the window automatically when it loses focus.
    pub(in crate::native) hide_on_focus_loss: bool,
    /// Reveal animation preference. Overridden to `Instant` when
    /// `reduced_motion` is set.
    pub(in crate::native) animation: QuickTerminalAnimation,
    /// Optional profile name to launch the quick session with; `None` uses the
    /// default profile.
    pub(in crate::native) profile: Option<String>,
    /// Mirrors the global reduced-motion preference. When `true` the reveal is
    /// always instant regardless of `animation`.
    pub(in crate::native) reduced_motion: bool,
}

impl Default for QuickTerminalSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            shortcut: "F12".to_owned(),
            edge: QuickTerminalEdge::default(),
            // 40% of the monitor height for a Top drop-down: visible without
            // covering the whole screen.
            coverage: QuickTerminalExtent::Fraction(0.4),
            span: QuickTerminalExtent::Fraction(1.0),
            monitor: MonitorPolicy::default(),
            hide_on_focus_loss: false,
            animation: QuickTerminalAnimation::default(),
            profile: None,
            reduced_motion: false,
        }
    }
}

impl QuickTerminalSettings {
    /// The animation actually used, honoring reduced-motion. Reduced-motion
    /// always wins: it forces an instant reveal even if `animation` is `Slide`.
    pub(in crate::native) fn effective_animation(&self) -> QuickTerminalAnimation {
        if self.reduced_motion {
            QuickTerminalAnimation::Instant
        } else {
            self.animation
        }
    }

    /// Compute the window placement on a given monitor work area. Pure: the same
    /// inputs always give the same rect, and the result is always inside the
    /// work area with a non-zero size.
    pub(in crate::native) fn geometry(&self, work_area: MonitorRect) -> QuickTerminalGeometry {
        let (width, height) = if self.edge.is_horizontal() {
            (
                self.span.resolve(work_area.width),
                self.coverage.resolve(work_area.height),
            )
        } else {
            (
                self.coverage.resolve(work_area.width),
                self.span.resolve(work_area.height),
            )
        };

        // Anchor to the chosen edge; center along the spanning axis.
        let (x, y) = match self.edge {
            QuickTerminalEdge::Top => (centered(work_area.x, work_area.width, width), work_area.y),
            QuickTerminalEdge::Bottom => (
                centered(work_area.x, work_area.width, width),
                work_area.y + i32::try_from(work_area.height.saturating_sub(height)).unwrap_or(0),
            ),
            QuickTerminalEdge::Left => {
                (work_area.x, centered(work_area.y, work_area.height, height))
            }
            QuickTerminalEdge::Right => (
                work_area.x + i32::try_from(work_area.width.saturating_sub(width)).unwrap_or(0),
                centered(work_area.y, work_area.height, height),
            ),
        };

        QuickTerminalGeometry {
            x,
            y,
            width,
            height,
        }
    }
}

/// Center a span of length `size` within `[origin, origin + available)`.
fn centered(origin: i32, available: u32, size: u32) -> i32 {
    let slack = available.saturating_sub(size) / 2;
    origin + i32::try_from(slack).unwrap_or(0)
}

const FALLBACK_MONITOR_RECT: MonitorRect = MonitorRect {
    x: 0,
    y: 0,
    width: 1920,
    height: 1080,
};

/// Resolve a monitor policy from one current monitor snapshot. The live host
/// converts winit monitor handles to [`MonitorRect`] first; keeping selection
/// here makes stale-index and display-removal behavior deterministic and
/// headlessly testable.
pub(in crate::native) fn resolve_monitor_rect(
    policy: MonitorPolicy,
    available: &[MonitorRect],
    active: Option<MonitorRect>,
    primary: Option<MonitorRect>,
) -> MonitorRect {
    let primary_or_first = || primary.or_else(|| available.first().copied());
    match policy {
        MonitorPolicy::Primary => primary_or_first(),
        MonitorPolicy::Index(index) => available
            .get(index)
            .copied()
            .or(active)
            .or_else(primary_or_first),
        MonitorPolicy::ActiveMonitor => active.or_else(primary_or_first),
    }
    .unwrap_or(FALLBACK_MONITOR_RECT)
}

/// The reveal slide duration. Short enough to feel immediate, long enough to
/// read as motion; collapsed to zero under reduced-motion / `Instant`.
pub(in crate::native) const REVEAL_DURATION_MS: u32 = 140;

/// A reveal-animation timeline: interpolates the quick window from an off-edge
/// start position to its final anchored geometry over [`REVEAL_DURATION_MS`]
/// with an ease-out curve. The window keeps its final SIZE throughout and only
/// its POSITION slides, so the reveal reads as a drop-in from the anchored edge.
///
/// Pure and deterministic: the caller owns wall-clock timing and calls
/// [`Self::sample`] with elapsed milliseconds. `Instant`/reduced-motion never
/// builds a timeline; the host applies the final geometry directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct RevealTimeline {
    from: QuickTerminalGeometry,
    to: QuickTerminalGeometry,
    duration_ms: u32,
}

impl RevealTimeline {
    /// Build a timeline that slides `to` in from just off `edge` of `work_area`.
    /// The start position places the (final-size) window fully off the anchored
    /// edge; the end is `to` unchanged.
    pub(in crate::native) fn new(
        edge: QuickTerminalEdge,
        to: QuickTerminalGeometry,
        work_area: MonitorRect,
    ) -> Self {
        let height = i32::try_from(to.height).unwrap_or(i32::MAX);
        let width = i32::try_from(to.width).unwrap_or(i32::MAX);
        let (from_x, from_y) = match edge {
            // Fully above the work area, sliding down to the top anchor.
            QuickTerminalEdge::Top => (to.x, work_area.y - height),
            // Fully below, sliding up to the bottom anchor.
            QuickTerminalEdge::Bottom => (
                to.x,
                work_area.y + i32::try_from(work_area.height).unwrap_or(i32::MAX),
            ),
            // Fully left, sliding right to the left anchor.
            QuickTerminalEdge::Left => (work_area.x - width, to.y),
            // Fully right, sliding left to the right anchor.
            QuickTerminalEdge::Right => (
                work_area.x + i32::try_from(work_area.width).unwrap_or(i32::MAX),
                to.y,
            ),
        };
        Self {
            from: QuickTerminalGeometry {
                x: from_x,
                y: from_y,
                width: to.width,
                height: to.height,
            },
            to,
            duration_ms: REVEAL_DURATION_MS,
        }
    }

    /// Sample the geometry at `elapsed_ms` since the reveal began. Returns the
    /// interpolated geometry and whether the animation has finished (elapsed at
    /// or past the duration), at which point the geometry equals the final `to`.
    pub(in crate::native) fn sample(&self, elapsed_ms: u32) -> (QuickTerminalGeometry, bool) {
        if elapsed_ms >= self.duration_ms {
            return (self.to, true);
        }
        let t = f64::from(elapsed_ms) / f64::from(self.duration_ms);
        // Ease-out cubic: fast start, gentle settle.
        let eased = 1.0 - (1.0 - t).powi(3);
        let lerp = |a: i32, b: i32| -> i32 {
            let delta = f64::from(b - a) * eased;
            a + delta.round() as i32
        };
        (
            QuickTerminalGeometry {
                x: lerp(self.from.x, self.to.x),
                y: lerp(self.from.y, self.to.y),
                width: self.to.width,
                height: self.to.height,
            },
            false,
        )
    }
}

/// Whether the quick terminal is currently on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum QuickVisibility {
    Hidden,
    Visible,
}

/// What a summon/hide request resolves to. The host executes exactly one of
/// these; every branch preserves the singleton session (nothing here ever
/// destroys the quick session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum QuickTerminalAction {
    /// First summon while no quick window exists: create the single dedicated
    /// window and session, then reveal it. Happens at most once per process
    /// lifetime (the window is preserved when hidden).
    CreateAndShow,
    /// Reveal the existing, hidden quick window. Its session is preserved
    /// exactly as it was left.
    Show,
    /// Hide the quick window, preserving its session for the next summon.
    Hide,
    /// The feature is disabled, or the request is a no-op (e.g. a hide while
    /// already hidden). The host does nothing.
    Nothing,
}

/// The summon/hide lifecycle state machine for the single quick terminal. Pure
/// and deterministic: it owns no OS handles, only the decision of what the host
/// should do next, so it is exercised headlessly.
#[derive(Debug, Clone)]
pub(in crate::native) struct QuickTerminalController {
    settings: QuickTerminalSettings,
    visibility: QuickVisibility,
    /// The identity of the dedicated window once created, or `None` before the
    /// first summon (lazy creation). There is never more than one.
    identity: Option<QuickTerminalIdentity>,
    /// True after the first summon has issued `CreateAndShow` and before the
    /// host either attaches the new window or reports creation failure through
    /// `detach_window`. This closes the otherwise duplicate-producing interval
    /// where repeated summons arrive before an identity exists.
    creation_pending: bool,
}

impl QuickTerminalController {
    pub(in crate::native) fn new(settings: QuickTerminalSettings) -> Self {
        Self {
            settings,
            visibility: QuickVisibility::Hidden,
            identity: None,
            creation_pending: false,
        }
    }

    pub(in crate::native) fn settings(&self) -> &QuickTerminalSettings {
        &self.settings
    }

    // Read by the lifecycle tests and staged for the section-B status surface;
    // the toggle path uses the internal field directly, so this reader has no
    // production caller yet.
    #[allow(dead_code)]
    pub(in crate::native) fn visibility(&self) -> QuickVisibility {
        self.visibility
    }

    pub(in crate::native) fn identity(&self) -> Option<QuickTerminalIdentity> {
        self.identity
    }

    /// Whether the dedicated window has been created yet.
    // Exercised by the singleton/summon tests; a staged status accessor with no
    // production caller yet (the host tracks the window via `identity`).
    #[allow(dead_code)]
    pub(in crate::native) fn exists(&self) -> bool {
        self.identity.is_some()
    }

    /// Replace the live settings (e.g. after a settings reload). Does not change
    /// visibility or the existing window; a geometry/edge change applies on the
    /// next summon.
    pub(in crate::native) fn update_settings(&mut self, settings: QuickTerminalSettings) {
        self.settings = settings;
    }

    /// Record that the host created the dedicated window with this identity.
    /// Called exactly once, in response to [`QuickTerminalAction::CreateAndShow`].
    pub(in crate::native) fn attach_window(&mut self, identity: QuickTerminalIdentity) {
        debug_assert!(self.identity.is_none(), "quick terminal is a singleton");
        self.identity = Some(identity);
        self.creation_pending = false;
        self.visibility = QuickVisibility::Visible;
    }

    /// Forget the dedicated window because it was closed or retired out from
    /// under the lifecycle (a user close of the quick window, or a merge that
    /// consumed it). A later summon recreates the singleton cleanly. Settings
    /// are preserved.
    pub(in crate::native) fn detach_window(&mut self) {
        self.identity = None;
        self.creation_pending = false;
        self.visibility = QuickVisibility::Hidden;
    }

    /// Toggle: hide when visible, summon when hidden. The primary shortcut
    /// action. A disabled feature is always `Nothing`.
    pub(in crate::native) fn toggle(&mut self) -> QuickTerminalAction {
        if !self.settings.enabled {
            return QuickTerminalAction::Nothing;
        }
        match self.visibility {
            QuickVisibility::Visible => self.hide(),
            QuickVisibility::Hidden => self.summon(),
        }
    }

    /// Summon (reveal) the quick terminal. Repeated summons never create a
    /// second window: the first creates the singleton, later ones only reveal
    /// the preserved one, and a summon while already visible is a no-op (the
    /// window is already up and focused).
    pub(in crate::native) fn summon(&mut self) -> QuickTerminalAction {
        if !self.settings.enabled {
            return QuickTerminalAction::Nothing;
        }
        match (self.identity, self.visibility) {
            // Already visible: repeated summon is a no-op (never a duplicate).
            (Some(_), QuickVisibility::Visible) => QuickTerminalAction::Nothing,
            // Exists but hidden: reveal the preserved window/session.
            (Some(_), QuickVisibility::Hidden) => {
                self.visibility = QuickVisibility::Visible;
                QuickTerminalAction::Show
            }
            // First summon: reserve the singleton creation before returning the
            // action. A second summon cannot issue another CreateAndShow while
            // the host is still constructing and attaching the first window.
            (None, _) if !self.creation_pending => {
                self.creation_pending = true;
                QuickTerminalAction::CreateAndShow
            }
            // Creation is already in flight. The host will attach it or clear
            // the reservation through `detach_window` after a failed spawn.
            (None, _) => QuickTerminalAction::Nothing,
        }
    }

    /// Hide the quick terminal, preserving its session. A hide while already
    /// hidden, or before the window exists, is a no-op.
    pub(in crate::native) fn hide(&mut self) -> QuickTerminalAction {
        match (self.identity, self.visibility) {
            (Some(_), QuickVisibility::Visible) => {
                self.visibility = QuickVisibility::Hidden;
                QuickTerminalAction::Hide
            }
            _ => QuickTerminalAction::Nothing,
        }
    }

    /// Called when the quick window loses focus. Hides it only when
    /// `hide_on_focus_loss` is set; otherwise it stays up.
    pub(in crate::native) fn on_focus_lost(
        &mut self,
        interaction_owned: bool,
    ) -> QuickTerminalAction {
        if self.settings.hide_on_focus_loss && !interaction_owned {
            self.hide()
        } else {
            QuickTerminalAction::Nothing
        }
    }

    /// Whether the given window id is the quick terminal. The host uses this to
    /// keep the quick window out of ordinary window-close/restoration handling.
    pub(in crate::native) fn owns_window(&self, id: ProcessWindowId) -> bool {
        self.identity.map(|i| i.window()) == Some(id)
    }

    /// Ordinary session persistence may write only non-quick windows. Keeping
    /// this query on the controller couples the live identity to the
    /// `QuickTerminalIdentity::is_restorable` contract instead of relying on the
    /// quick App's incidental secondary-instance state.
    pub(in crate::native) fn window_is_restorable(&self, id: ProcessWindowId) -> bool {
        self.identity
            .filter(|identity| identity.window() == id)
            .is_none_or(|identity| identity.is_restorable())
    }
}

// ---------------------------------------------------------------------------
// Global shortcut accelerator parsing
// ---------------------------------------------------------------------------

/// A parsed global-shortcut accelerator: zero or more modifiers plus one key.
/// Parsing is dep-free and platform-neutral; a backend maps this to the OS key
/// grab. An empty or malformed accelerator is rejected rather than silently
/// registering nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) struct Accelerator {
    pub(in crate::native) ctrl: bool,
    pub(in crate::native) alt: bool,
    pub(in crate::native) shift: bool,
    /// The platform-independent "super"/"meta"/"cmd"/"win" modifier.
    pub(in crate::native) meta: bool,
    /// The normalized key token (upper-cased, e.g. `"F12"`, `"GRAVE"`, `"A"`).
    pub(in crate::native) key: String,
}

/// Why an accelerator string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum AcceleratorError {
    Empty,
    /// No non-modifier key was given (e.g. `"ctrl+shift"`).
    NoKey,
    /// More than one non-modifier key was given (e.g. `"a+b"`).
    MultipleKeys,
}

impl Accelerator {
    /// Parse `"ctrl+shift+F12"`-style accelerators. Case-insensitive; `+` or `-`
    /// separated. Recognizes ctrl/control, alt/opt/option, shift, and
    /// super/meta/cmd/command/win as modifiers.
    pub(in crate::native) fn parse(s: &str) -> Result<Self, AcceleratorError> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(AcceleratorError::Empty);
        }
        let mut acc = Accelerator {
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
            key: String::new(),
        };
        let mut key_set = false;
        for raw in trimmed.split(['+', '-']) {
            let tok = raw.trim();
            if tok.is_empty() {
                continue;
            }
            match tok.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => acc.ctrl = true,
                "alt" | "opt" | "option" => acc.alt = true,
                "shift" => acc.shift = true,
                "super" | "meta" | "cmd" | "command" | "win" | "windows" => acc.meta = true,
                _ => {
                    if key_set {
                        return Err(AcceleratorError::MultipleKeys);
                    }
                    acc.key = normalize_key(tok);
                    key_set = true;
                }
            }
        }
        if !key_set {
            return Err(AcceleratorError::NoKey);
        }
        Ok(acc)
    }

    /// Whether any modifier is held.
    // Exercised by the accelerator-parsing tests; retained as part of the
    // accelerator surface, without a production caller yet.
    #[allow(dead_code)]
    pub(in crate::native) fn has_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.meta
    }
}

/// Normalize a key token to a stable upper-case spelling, mapping a few common
/// aliases so `` "`" `` and `"backtick"` both name the grave key.
fn normalize_key(tok: &str) -> String {
    let lower = tok.to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "`" | "backtick" | "grave" | "tilde" => "GRAVE",
        "space" | "spacebar" => "SPACE",
        "esc" | "escape" => "ESCAPE",
        "return" | "enter" => "ENTER",
        other => return other.to_ascii_uppercase(),
    };
    canonical.to_owned()
}

// ---------------------------------------------------------------------------
// Platform global-shortcut capability seam
// ---------------------------------------------------------------------------

/// The outcome of attempting to register a global shortcut. These are DISTINCT
/// states, never collapsed: a backend must not report `Registered` unless the OS
/// confirmed the grab. `Unsupported` means the platform/environment cannot grant
/// a global grab at all (and carries an actionable message); `Unavailable` means
/// no backend is compiled in or a transient registration failure occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native) enum ShortcutRegistration {
    /// The OS confirmed the grab. `backend` names the mechanism used.
    Registered { backend: &'static str },
    /// The platform/environment fundamentally cannot register this shortcut.
    /// `reason` is user-facing and actionable (what to do instead).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unsupported {
        platform: &'static str,
        reason: String,
    },
    /// No backend is compiled in, or registration failed at runtime. `reason`
    /// explains which.
    Unavailable { reason: String },
}

impl ShortcutRegistration {
    pub(in crate::native) fn is_registered(&self) -> bool {
        matches!(self, Self::Registered { .. })
    }
}

/// A callback the platform backend invokes (from its own thread) each time the
/// registered shortcut fires. The winit integration posts a
/// `UserEvent::QuickTerminalSummon` through an `EventLoopProxy`, which wakes an
/// idle loop and delivers the toggle to the main thread. It is `Send + Sync` so
/// a backend thread can hold and call it.
pub(in crate::native) type SummonSink = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A platform global-shortcut backend. Implementors own the OS key grab and its
/// teardown. The default [`NullShortcutAdapter`] never claims a registration it
/// cannot make; real backends (X11 `XGrabKey`, Windows `RegisterHotKey`, macOS
/// `RegisterEventHotKey`) confirm the grab with the OS before returning
/// `Registered` and invoke `sink` on each press.
pub(in crate::native) trait GlobalShortcutAdapter {
    /// Attempt to register `accelerator` for global summon, invoking `sink` each
    /// time it fires. Must never return `Registered` without OS confirmation.
    fn register(&mut self, accelerator: &Accelerator, sink: SummonSink) -> ShortcutRegistration;
    /// Release any registered grab. Idempotent.
    fn unregister(&mut self);
}

/// The honest default: registers nothing and says so. Used when a platform has
/// no compiled backend, so the lifecycle never believes a shortcut is live when
/// none is.
// Constructed only by `platform_shortcut_adapter` on a platform with no
// compiled backend (the `not(any(...))` arm) and by the adapter-contract tests;
// on Linux/Windows/macOS a real backend is built instead, so it reads as dead
// on the dev host while remaining the honest fallback elsewhere.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(in crate::native) struct NullShortcutAdapter;

impl GlobalShortcutAdapter for NullShortcutAdapter {
    fn register(&mut self, _accelerator: &Accelerator, _sink: SummonSink) -> ShortcutRegistration {
        ShortcutRegistration::Unavailable {
            reason: "no global-shortcut backend is compiled in for this build".to_owned(),
        }
    }

    fn unregister(&mut self) {}
}

// ---------------------------------------------------------------------------
// Key token -> platform key-code mappings
// ---------------------------------------------------------------------------
//
// The [`Accelerator`] key token is normalized upper-case (e.g. `"F12"`,
// `"GRAVE"`, `"A"`). Each platform grab needs that token as its own key code.
// The three mappers below are pure and unit-tested; each is only USED under its
// platform cfg, but all compile everywhere so the tables stay verifiable on the
// Linux dev host. A key the mapper does not know returns `None`, and the
// backend then reports a non-registered outcome rather than grabbing the wrong
// key.

/// Map a normalized accelerator key token to an X11 keysym (see
/// `X11/keysymdef.h`). Letters use the lower-case keysym, which resolves to the
/// same physical keycode as the upper-case one via `XKeysymToKeycode`.
#[allow(dead_code)]
pub(in crate::native) fn x11_keysym(key: &str) -> Option<std::os::raw::c_ulong> {
    let k = key.trim().to_ascii_uppercase();
    // Function keys F1..F24 (XK_F1 = 0xFFBE).
    if let Some(rest) = k.strip_prefix('F')
        && let Ok(n) = rest.parse::<u32>()
        && (1..=24).contains(&n)
    {
        return Some(0xFFBE + u64::from(n - 1) as std::os::raw::c_ulong);
    }
    if k.len() == 1 {
        let c = k.as_bytes()[0];
        if c.is_ascii_uppercase() {
            // XK_a = 0x61; use the lower-case keysym.
            return Some((0x61 + u64::from(c - b'A')) as std::os::raw::c_ulong);
        }
        if c.is_ascii_digit() {
            return Some(u64::from(c) as std::os::raw::c_ulong); // XK_0 = 0x30 == b'0'
        }
    }
    let sym: std::os::raw::c_ulong = match k.as_str() {
        "GRAVE" | "BACKTICK" | "BACKQUOTE" => 0x60, // XK_grave
        "SPACE" => 0x20,
        "TAB" => 0xFF09,
        "RETURN" | "ENTER" => 0xFF0D,
        "ESCAPE" | "ESC" => 0xFF1B,
        "LEFT" => 0xFF51,
        "UP" => 0xFF52,
        "RIGHT" => 0xFF53,
        "DOWN" => 0xFF54,
        "HOME" => 0xFF50,
        "END" => 0xFF57,
        "PAGEUP" | "PRIOR" => 0xFF55,
        "PAGEDOWN" | "NEXT" => 0xFF56,
        "INSERT" => 0xFF63,
        "DELETE" | "DEL" => 0xFFFF,
        "MINUS" => 0x2D,
        "EQUAL" => 0x3D,
        _ => return None,
    };
    Some(sym)
}

/// Map a normalized accelerator key token to a Windows virtual-key code.
// Cross-compiled and unit-tested on every host (see `key_mappers_agree_on_...`),
// but CALLED only by the Windows backend under `cfg(windows)`, so it reads as
// dead on the Linux/macOS build.
#[allow(dead_code)]
pub(in crate::native) fn windows_vk(key: &str) -> Option<u16> {
    let k = key.trim().to_ascii_uppercase();
    if let Some(rest) = k.strip_prefix('F')
        && let Ok(n) = rest.parse::<u16>()
        && (1..=24).contains(&n)
    {
        return Some(0x70 + (n - 1)); // VK_F1 = 0x70
    }
    if k.len() == 1 {
        let c = k.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(u16::from(c)); // VK for 'A'..'Z'/'0'..'9' == ASCII code
        }
    }
    let vk: u16 = match k.as_str() {
        "GRAVE" | "BACKTICK" | "BACKQUOTE" => 0xC0, // VK_OEM_3
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "RETURN" | "ENTER" => 0x0D,
        "ESCAPE" | "ESC" => 0x1B,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PRIOR" => 0x21,
        "PAGEDOWN" | "NEXT" => 0x22,
        "INSERT" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "MINUS" => 0xBD, // VK_OEM_MINUS
        "EQUAL" => 0xBB, // VK_OEM_PLUS
        _ => return None,
    };
    Some(vk)
}

/// Map a normalized accelerator key token to a macOS Carbon virtual keycode
/// (the `kVK_*` constants). The layout is non-contiguous, so the letters and
/// digits are an explicit table rather than an arithmetic offset.
// Cross-compiled and unit-tested on every host, but CALLED only by the macOS
// backend under `cfg(macos)`, so it reads as dead on the Linux/Windows build.
#[allow(dead_code)]
pub(in crate::native) fn macos_keycode(key: &str) -> Option<u32> {
    let k = key.trim().to_ascii_uppercase();
    if let Some(rest) = k.strip_prefix('F')
        && let Ok(n) = rest.parse::<u32>()
    {
        let code = match n {
            1 => 0x7A,
            2 => 0x78,
            3 => 0x63,
            4 => 0x76,
            5 => 0x60,
            6 => 0x61,
            7 => 0x62,
            8 => 0x64,
            9 => 0x65,
            10 => 0x6D,
            11 => 0x67,
            12 => 0x6F,
            13 => 0x69,
            14 => 0x6B,
            15 => 0x71,
            16 => 0x6A,
            17 => 0x40,
            18 => 0x4F,
            19 => 0x50,
            20 => 0x5A,
            _ => return None,
        };
        return Some(code);
    }
    if k.len() == 1 {
        let c = k.as_bytes()[0];
        let code = match c {
            b'A' => 0x00,
            b'B' => 0x0B,
            b'C' => 0x08,
            b'D' => 0x02,
            b'E' => 0x0E,
            b'F' => 0x03,
            b'G' => 0x05,
            b'H' => 0x04,
            b'I' => 0x22,
            b'J' => 0x26,
            b'K' => 0x28,
            b'L' => 0x25,
            b'M' => 0x2E,
            b'N' => 0x2D,
            b'O' => 0x1F,
            b'P' => 0x23,
            b'Q' => 0x0C,
            b'R' => 0x0F,
            b'S' => 0x01,
            b'T' => 0x11,
            b'U' => 0x20,
            b'V' => 0x09,
            b'W' => 0x0D,
            b'X' => 0x07,
            b'Y' => 0x10,
            b'Z' => 0x06,
            b'0' => 0x1D,
            b'1' => 0x12,
            b'2' => 0x13,
            b'3' => 0x14,
            b'4' => 0x15,
            b'5' => 0x17,
            b'6' => 0x16,
            b'7' => 0x1A,
            b'8' => 0x1C,
            b'9' => 0x19,
            _ => return None,
        };
        return Some(code);
    }
    let code: u32 = match k.as_str() {
        "GRAVE" | "BACKTICK" | "BACKQUOTE" => 0x32, // kVK_ANSI_Grave
        "SPACE" => 0x31,
        "TAB" => 0x30,
        "RETURN" | "ENTER" => 0x24,
        "ESCAPE" | "ESC" => 0x35,
        "LEFT" => 0x7B,
        "RIGHT" => 0x7C,
        "DOWN" => 0x7D,
        "UP" => 0x7E,
        "HOME" => 0x73,
        "END" => 0x77,
        "PAGEUP" | "PRIOR" => 0x74,
        "PAGEDOWN" | "NEXT" => 0x79,
        "DELETE" | "DEL" => 0x75, // kVK_ForwardDelete
        "MINUS" => 0x1B,
        "EQUAL" => 0x18,
        _ => return None,
    };
    Some(code)
}

/// Detected Linux display server, used to give an accurate global-shortcut
/// capability answer.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) enum LinuxDisplayServer {
    Wayland,
    X11,
    Unknown,
}

/// Detect the Linux display server from the environment. Wayland is detected by
/// `WAYLAND_DISPLAY` or `XDG_SESSION_TYPE=wayland`; X11 by `DISPLAY` /
/// `XDG_SESSION_TYPE=x11`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(in crate::native) fn detect_linux_display_server(
    wayland_display: Option<&str>,
    x11_display: Option<&str>,
    session_type: Option<&str>,
) -> LinuxDisplayServer {
    let session = session_type.map(|s| s.trim().to_ascii_lowercase());
    if session.as_deref() == Some("wayland")
        || wayland_display.is_some_and(|v| !v.trim().is_empty())
    {
        return LinuxDisplayServer::Wayland;
    }
    if session.as_deref() == Some("x11") || x11_display.is_some_and(|v| !v.trim().is_empty()) {
        return LinuxDisplayServer::X11;
    }
    LinuxDisplayServer::Unknown
}

/// Build the platform global-shortcut adapter for this build. Linux uses the
/// X11 `XGrabKey` backend (or reports the honest Wayland/headless limitation),
/// Windows uses `RegisterHotKey`, and macOS uses `RegisterEventHotKey`. Any
/// platform without a compiled backend falls back to the honest
/// [`NullShortcutAdapter`], which never claims a registration it cannot confirm.
// The adapter is `+ Send` so registration can run on a worker thread off the
// event-loop path (the live adapter is then stored and kept alive there); every
// concrete backend is `Send` (the macOS adapter via a documented `unsafe impl`
// because its refs are only ever touched on the main thread).
#[cfg(target_os = "linux")]
pub(in crate::native) fn platform_shortcut_adapter() -> Box<dyn GlobalShortcutAdapter + Send> {
    Box::new(x11::LinuxShortcutAdapter::from_env())
}

#[cfg(target_os = "windows")]
pub(in crate::native) fn platform_shortcut_adapter() -> Box<dyn GlobalShortcutAdapter + Send> {
    Box::new(windows::windows_grab::WindowsShortcutAdapter::new())
}

#[cfg(target_os = "macos")]
pub(in crate::native) fn platform_shortcut_adapter() -> Box<dyn GlobalShortcutAdapter + Send> {
    Box::new(macos::macos_grab::MacosShortcutAdapter::new())
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub(in crate::native) fn platform_shortcut_adapter() -> Box<dyn GlobalShortcutAdapter + Send> {
    // No compiled backend for this platform; the honest default reports
    // Unavailable so the lifecycle never believes a shortcut is live.
    Box::new(NullShortcutAdapter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work_area() -> MonitorRect {
        MonitorRect {
            x: 100,
            y: 200,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn defaults_are_accessibility_safe() {
        let s = QuickTerminalSettings::default();
        assert!(!s.enabled, "off by default (opt-in): startup unchanged");
        assert!(
            !s.hide_on_focus_loss,
            "focus-loss hiding defaults off per the foundation contract"
        );
        assert_eq!(
            s.effective_animation(),
            QuickTerminalAnimation::Instant,
            "motion defaults off per the foundation contract"
        );
        assert_eq!(s.monitor, MonitorPolicy::ActiveMonitor);
        assert_eq!(s.edge, QuickTerminalEdge::Top);
    }

    #[test]
    fn reduced_motion_forces_instant_reveal() {
        let mut s = QuickTerminalSettings {
            animation: QuickTerminalAnimation::Slide,
            ..QuickTerminalSettings::default()
        };
        assert_eq!(s.effective_animation(), QuickTerminalAnimation::Slide);
        s.reduced_motion = true;
        assert_eq!(
            s.effective_animation(),
            QuickTerminalAnimation::Instant,
            "reduced-motion always wins over a Slide preference"
        );
    }

    #[test]
    fn top_edge_geometry_spans_width_and_covers_height_fraction() {
        let s = QuickTerminalSettings {
            edge: QuickTerminalEdge::Top,
            coverage: QuickTerminalExtent::Fraction(0.4),
            span: QuickTerminalExtent::Fraction(1.0),
            ..QuickTerminalSettings::default()
        };
        let g = s.geometry(work_area());
        assert_eq!(g.x, 100, "full-width span sits at the work-area origin");
        assert_eq!(g.y, 200, "anchored to the top");
        assert_eq!(g.width, 1920);
        assert_eq!(g.height, 432, "40% of 1080");
    }

    #[test]
    fn bottom_edge_anchors_to_the_far_edge() {
        let s = QuickTerminalSettings {
            edge: QuickTerminalEdge::Bottom,
            coverage: QuickTerminalExtent::Fraction(0.5),
            span: QuickTerminalExtent::Fraction(1.0),
            ..QuickTerminalSettings::default()
        };
        let g = s.geometry(work_area());
        assert_eq!(g.height, 540);
        assert_eq!(g.y, 200 + (1080 - 540), "flush with the bottom");
    }

    #[test]
    fn left_edge_covers_width_and_centers_vertically() {
        let s = QuickTerminalSettings {
            edge: QuickTerminalEdge::Left,
            coverage: QuickTerminalExtent::Fraction(0.25),
            span: QuickTerminalExtent::Fraction(0.5),
            ..QuickTerminalSettings::default()
        };
        let g = s.geometry(work_area());
        assert_eq!(g.width, 480, "25% of 1920");
        assert_eq!(g.height, 540, "50% of 1080");
        assert_eq!(g.x, 100, "flush with the left");
        assert_eq!(g.y, 200 + (1080 - 540) / 2, "centered vertically");
    }

    #[test]
    fn monitor_policy_resolution_survives_index_removal() {
        let first = MonitorRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let second = MonitorRect {
            x: 1920,
            y: 0,
            width: 2560,
            height: 1440,
        };
        let available = [first, second];

        assert_eq!(
            resolve_monitor_rect(
                MonitorPolicy::Index(1),
                &available,
                Some(first),
                Some(first)
            ),
            second
        );
        assert_eq!(
            resolve_monitor_rect(
                MonitorPolicy::ActiveMonitor,
                &available,
                Some(second),
                Some(first)
            ),
            second
        );
        assert_eq!(
            resolve_monitor_rect(
                MonitorPolicy::Primary,
                &available,
                Some(second),
                Some(first)
            ),
            first
        );

        // The indexed display disappeared while the quick window was hidden.
        // The next summon sees a fresh one-monitor snapshot and falls back to
        // the active monitor rather than retaining off-screen geometry.
        assert_eq!(
            resolve_monitor_rect(MonitorPolicy::Index(1), &[first], Some(first), Some(first)),
            first
        );
        assert_eq!(
            resolve_monitor_rect(MonitorPolicy::Index(9), &[first], None, Some(first)),
            first
        );
        assert_eq!(
            resolve_monitor_rect(MonitorPolicy::Index(9), &[], None, None),
            FALLBACK_MONITOR_RECT
        );
    }

    #[test]
    fn extent_clamps_degenerate_fractions() {
        // Zero/negative/NaN never produce a zero-size window.
        assert_eq!(QuickTerminalExtent::Fraction(0.0).resolve(1000), 10);
        assert_eq!(QuickTerminalExtent::Fraction(-1.0).resolve(1000), 10);
        assert_eq!(QuickTerminalExtent::Fraction(f32::NAN).resolve(1000), 1000);
        // Over 1.0 is capped at the whole axis.
        assert_eq!(QuickTerminalExtent::Fraction(5.0).resolve(1000), 1000);
        // Pixels are clamped into range.
        assert_eq!(QuickTerminalExtent::Pixels(0).resolve(1000), 1);
        assert_eq!(QuickTerminalExtent::Pixels(5000).resolve(1000), 1000);
    }

    #[test]
    fn reveal_timeline_starts_off_edge_and_settles_on_the_final_geometry() {
        let s = QuickTerminalSettings {
            edge: QuickTerminalEdge::Top,
            coverage: QuickTerminalExtent::Fraction(0.4),
            span: QuickTerminalExtent::Fraction(1.0),
            ..QuickTerminalSettings::default()
        };
        let wa = work_area();
        let final_geometry = s.geometry(wa);
        let timeline = RevealTimeline::new(QuickTerminalEdge::Top, final_geometry, wa);

        // t=0: fully off the top edge (one full height above the work area),
        // final SIZE already applied so only the position slides.
        let (start, done0) = timeline.sample(0);
        assert!(!done0);
        assert_eq!(start.width, final_geometry.width);
        assert_eq!(start.height, final_geometry.height);
        assert_eq!(start.x, final_geometry.x, "top edge slides vertically only");
        assert_eq!(start.y, wa.y - final_geometry.height as i32);

        // Midway the window is between the start and the final anchor.
        let (mid, done_mid) = timeline.sample(REVEAL_DURATION_MS / 2);
        assert!(!done_mid);
        assert!(mid.y > start.y && mid.y < final_geometry.y);

        // At/after the duration it snaps to the final geometry and reports done.
        let (end, done_end) = timeline.sample(REVEAL_DURATION_MS);
        assert!(done_end);
        assert_eq!(end, final_geometry);
        let (past, done_past) = timeline.sample(REVEAL_DURATION_MS * 4);
        assert!(done_past);
        assert_eq!(past, final_geometry);
    }

    #[test]
    fn reveal_timeline_slides_bottom_up_from_below() {
        let s = QuickTerminalSettings {
            edge: QuickTerminalEdge::Bottom,
            coverage: QuickTerminalExtent::Fraction(0.5),
            span: QuickTerminalExtent::Fraction(1.0),
            ..QuickTerminalSettings::default()
        };
        let wa = work_area();
        let final_geometry = s.geometry(wa);
        let timeline = RevealTimeline::new(QuickTerminalEdge::Bottom, final_geometry, wa);
        let (start, _) = timeline.sample(0);
        assert_eq!(
            start.y,
            wa.y + wa.height as i32,
            "starts below the work area"
        );
        let (end, done) = timeline.sample(REVEAL_DURATION_MS);
        assert!(done);
        assert_eq!(end, final_geometry);
    }

    fn enabled_controller() -> QuickTerminalController {
        QuickTerminalController::new(QuickTerminalSettings {
            enabled: true,
            ..QuickTerminalSettings::default()
        })
    }

    #[test]
    fn disabled_feature_never_acts() {
        let mut c = QuickTerminalController::new(QuickTerminalSettings::default());
        assert_eq!(c.toggle(), QuickTerminalAction::Nothing);
        assert_eq!(c.summon(), QuickTerminalAction::Nothing);
        assert!(!c.exists());
    }

    #[test]
    fn first_summon_creates_singleton_then_reuses_it() {
        let mut c = enabled_controller();
        // First summon asks the host to create the dedicated window.
        assert_eq!(c.summon(), QuickTerminalAction::CreateAndShow);
        assert!(!c.exists(), "not created until the host attaches it");

        // Host creates the window and reports its identity.
        let id = QuickTerminalIdentity::new(ProcessWindowId(7));
        c.attach_window(id);
        assert!(c.exists());
        assert_eq!(c.visibility(), QuickVisibility::Visible);
        assert!(c.owns_window(ProcessWindowId(7)));
        assert!(!c.owns_window(ProcessWindowId(8)));

        // Repeated summon while visible is a no-op: never a second window.
        assert_eq!(c.summon(), QuickTerminalAction::Nothing);

        // Hide preserves the window; next summon only reveals it.
        assert_eq!(c.hide(), QuickTerminalAction::Hide);
        assert_eq!(c.visibility(), QuickVisibility::Hidden);
        assert_eq!(c.summon(), QuickTerminalAction::Show);
        assert!(c.exists(), "the singleton is preserved across hide/show");
    }

    #[test]
    fn creation_reservation_blocks_duplicate_then_clears_for_retry() {
        let mut c = enabled_controller();
        assert_eq!(c.summon(), QuickTerminalAction::CreateAndShow);
        assert_eq!(
            c.summon(),
            QuickTerminalAction::Nothing,
            "a summon before attach must not request a second window"
        );
        assert_eq!(
            c.toggle(),
            QuickTerminalAction::Nothing,
            "rapid toggles while creation is pending remain deduplicated"
        );

        // A failed host creation clears the reservation so summon can retry.
        c.detach_window();
        assert_eq!(c.summon(), QuickTerminalAction::CreateAndShow);
        c.attach_window(QuickTerminalIdentity::new(ProcessWindowId(9)));
        assert_eq!(c.summon(), QuickTerminalAction::Nothing);
    }

    #[test]
    fn toggle_alternates_show_and_hide() {
        let mut c = enabled_controller();
        assert_eq!(c.toggle(), QuickTerminalAction::CreateAndShow);
        c.attach_window(QuickTerminalIdentity::new(ProcessWindowId(1)));
        assert_eq!(c.toggle(), QuickTerminalAction::Hide);
        assert_eq!(c.toggle(), QuickTerminalAction::Show);
        assert_eq!(c.toggle(), QuickTerminalAction::Hide);
    }

    #[test]
    fn focus_loss_hides_only_when_configured() {
        // hide-on-focus-loss defaults off (foundation contract), so the "hides"
        // case must opt in explicitly.
        let mut c = QuickTerminalController::new(QuickTerminalSettings {
            enabled: true,
            hide_on_focus_loss: true,
            ..QuickTerminalSettings::default()
        });
        c.summon();
        c.attach_window(QuickTerminalIdentity::new(ProcessWindowId(1)));
        assert_eq!(c.on_focus_lost(false), QuickTerminalAction::Hide);
        assert_eq!(c.visibility(), QuickVisibility::Hidden);

        // With the option off, focus loss leaves it up.
        let mut c = QuickTerminalController::new(QuickTerminalSettings {
            enabled: true,
            hide_on_focus_loss: false,
            ..QuickTerminalSettings::default()
        });
        c.summon();
        c.attach_window(QuickTerminalIdentity::new(ProcessWindowId(2)));
        assert_eq!(c.on_focus_lost(false), QuickTerminalAction::Nothing);
        assert_eq!(c.visibility(), QuickVisibility::Visible);
    }

    #[test]
    fn focus_loss_does_not_hide_while_an_interaction_owns_input() {
        let mut c = QuickTerminalController::new(QuickTerminalSettings {
            enabled: true,
            hide_on_focus_loss: true,
            ..QuickTerminalSettings::default()
        });
        assert_eq!(c.summon(), QuickTerminalAction::CreateAndShow);
        c.attach_window(QuickTerminalIdentity::new(ProcessWindowId(4)));

        assert_eq!(c.on_focus_lost(true), QuickTerminalAction::Nothing);
        assert_eq!(c.visibility(), QuickVisibility::Visible);
        assert_eq!(c.on_focus_lost(false), QuickTerminalAction::Hide);
    }

    #[test]
    fn quick_identity_is_never_restorable() {
        let window = ProcessWindowId(3);
        let id = QuickTerminalIdentity::new(window);
        assert!(!id.is_restorable());

        let mut c = enabled_controller();
        c.summon();
        c.attach_window(id);
        assert!(!c.window_is_restorable(window));
        assert!(c.window_is_restorable(ProcessWindowId(8)));
    }

    #[test]
    fn accelerator_parses_modifiers_and_key() {
        let a = Accelerator::parse("ctrl+shift+F12").expect("valid");
        assert!(a.ctrl && a.shift && !a.alt && !a.meta);
        assert_eq!(a.key, "F12");
        assert!(a.has_modifier());

        // Aliases and separators.
        let b = Accelerator::parse("Super-`").expect("valid");
        assert!(b.meta);
        assert_eq!(b.key, "GRAVE");

        let c = Accelerator::parse("f12").expect("valid");
        assert_eq!(c.key, "F12");
        assert!(!c.has_modifier());
    }

    #[test]
    fn accelerator_rejects_malformed_input() {
        assert_eq!(Accelerator::parse(""), Err(AcceleratorError::Empty));
        assert_eq!(Accelerator::parse("   "), Err(AcceleratorError::Empty));
        assert_eq!(
            Accelerator::parse("ctrl+shift"),
            Err(AcceleratorError::NoKey)
        );
        assert_eq!(
            Accelerator::parse("a+b"),
            Err(AcceleratorError::MultipleKeys)
        );
    }

    #[test]
    fn null_adapter_never_claims_registration() {
        let mut a = NullShortcutAdapter;
        let acc = Accelerator::parse("F12").unwrap();
        let r = a.register(&acc, std::sync::Arc::new(|| {}));
        assert!(!r.is_registered(), "the honest default registers nothing");
        assert!(matches!(r, ShortcutRegistration::Unavailable { .. }));
        a.unregister(); // idempotent, no panic
    }

    #[test]
    fn edge_from_setting_maps_and_defaults() {
        assert_eq!(
            QuickTerminalEdge::from_setting("bottom"),
            QuickTerminalEdge::Bottom
        );
        assert_eq!(
            QuickTerminalEdge::from_setting("LEFT"),
            QuickTerminalEdge::Left
        );
        assert_eq!(
            QuickTerminalEdge::from_setting("right"),
            QuickTerminalEdge::Right
        );
        assert_eq!(
            QuickTerminalEdge::from_setting("top"),
            QuickTerminalEdge::Top
        );
        // Unknown -> Top (matches the settings-layer fallback).
        assert_eq!(
            QuickTerminalEdge::from_setting("sideways"),
            QuickTerminalEdge::Top
        );
    }

    #[test]
    fn extent_from_setting_parses_percent_and_pixels() {
        assert_eq!(
            QuickTerminalExtent::from_setting("40%", QuickTerminalExtent::Fraction(0.4)),
            QuickTerminalExtent::Fraction(0.4)
        );
        assert_eq!(
            QuickTerminalExtent::from_setting("600px", QuickTerminalExtent::Fraction(0.4)),
            QuickTerminalExtent::Pixels(600)
        );
        assert_eq!(
            QuickTerminalExtent::from_setting("800", QuickTerminalExtent::Fraction(0.4)),
            QuickTerminalExtent::Pixels(800)
        );
        // Malformed falls back.
        assert_eq!(
            QuickTerminalExtent::from_setting("huge", QuickTerminalExtent::Fraction(0.4)),
            QuickTerminalExtent::Fraction(0.4)
        );
    }

    #[test]
    fn monitor_and_animation_from_setting() {
        assert_eq!(
            MonitorPolicy::from_setting("active"),
            MonitorPolicy::ActiveMonitor
        );
        assert_eq!(
            MonitorPolicy::from_setting("primary"),
            MonitorPolicy::Primary
        );
        assert_eq!(MonitorPolicy::from_setting("2"), MonitorPolicy::Index(2));
        assert_eq!(
            MonitorPolicy::from_setting("weird"),
            MonitorPolicy::ActiveMonitor
        );
        assert_eq!(
            QuickTerminalAnimation::from_setting("instant"),
            QuickTerminalAnimation::Instant
        );
        assert_eq!(
            QuickTerminalAnimation::from_setting("slide"),
            QuickTerminalAnimation::Slide
        );
    }

    #[test]
    fn key_mappers_agree_on_common_keys() {
        // F-keys: base offsets are known-good anchors on each platform.
        assert_eq!(x11_keysym("F12"), Some(0xFFC9));
        assert_eq!(windows_vk("F12"), Some(0x7B));
        assert_eq!(macos_keycode("F12"), Some(0x6F));
        // Letters resolve on every platform.
        assert_eq!(x11_keysym("A"), Some(0x61));
        assert_eq!(windows_vk("A"), Some(0x41));
        assert_eq!(macos_keycode("A"), Some(0x00));
        // Grave / backtick, the common drop-down key.
        assert_eq!(x11_keysym("GRAVE"), Some(0x60));
        assert_eq!(windows_vk("GRAVE"), Some(0xC0));
        assert_eq!(macos_keycode("GRAVE"), Some(0x32));
        // An unknown token maps to nothing on every platform (never a wrong key).
        assert_eq!(x11_keysym("MOONBASE"), None);
        assert_eq!(windows_vk("MOONBASE"), None);
        assert_eq!(macos_keycode("MOONBASE"), None);
    }

    #[test]
    fn linux_display_server_detection() {
        assert_eq!(
            detect_linux_display_server(Some("wayland-0"), None, None),
            LinuxDisplayServer::Wayland
        );
        assert_eq!(
            detect_linux_display_server(None, Some(":0"), None),
            LinuxDisplayServer::X11
        );
        // session_type wins even without a *_DISPLAY var.
        assert_eq!(
            detect_linux_display_server(None, None, Some("wayland")),
            LinuxDisplayServer::Wayland
        );
        // Empty strings are not a live display.
        assert_eq!(
            detect_linux_display_server(Some("  "), Some("  "), None),
            LinuxDisplayServer::Unknown
        );
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Typed settings identities, value models, and defaults.

use super::*;

/// Default cursor blink policy (`ODYTTY_CURSOR_BLINK`). This is the host default
/// applied at power-on and after DECSCUSR 0 / RIS / DECSTR; an application's
/// DECSCUSR can still override it at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorBlink {
    /// Cursor blinks by default.
    On,
    /// Cursor is steady by default.
    Off,
    /// Conventional terminal default. Resolves to **blinking** — the historical
    /// VT default a fresh terminal powers on with. Linux (Wayland and X11)
    /// exposes no OS-level caret-blink *preference* to winit, so `Auto` honestly
    /// defers to that conventional default rather than to a system setting that
    /// does not exist. An application's DECSCUSR / DECSET 12 can still override
    /// it at runtime, exactly as for `On`/`Off`.
    #[default]
    Auto,
}

impl CursorBlink {
    /// Resolve the policy to a concrete default blink flag for the core.
    ///
    /// `Auto` resolves to `true` (blinking): on Linux there is no OS caret-blink
    /// preference exposed to the windowing layer, so the honest default is the
    /// conventional blinking cursor. `On`/`Off` are the explicit overrides.
    pub fn enabled(self) -> bool {
        match self {
            Self::On | Self::Auto => true,
            Self::Off => false,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Auto => "auto",
        }
    }

    pub(super) fn parse(raw: &str) -> Option<Self> {
        match normalize_name(raw).as_str() {
            "on" | "true" | "yes" | "blink" | "blinking" => Some(Self::On),
            "off" | "false" | "no" | "steady" | "solid" => Some(Self::Off),
            "auto" | "default" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KeyBindingModifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyBindingKey {
    Character(char),
    Named(KeyBindingNamedKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyBindingNamedKey {
    Enter,
    Backspace,
    Escape,
    Tab,
    Space,
    PageUp,
    PageDown,
    Home,
    End,
    Delete,
    Insert,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub modifiers: KeyBindingModifiers,
    pub key: KeyBindingKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBindingOverride {
    pub chord: KeyChord,
    pub action: BindableAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingEdit {
    pub key: &'static str,
    pub env: &'static str,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingEditError {
    pub key: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsEditOverlay {
    pub(super) base_values: BTreeMap<&'static str, String>,
    pub(super) values: BTreeMap<&'static str, String>,
    pub(super) settings: Settings,
}

/// Master render-quality profile. `Balanced` is the current renderer behavior;
/// `Plain` derives a hard fast path by neutralizing optional visual work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderQuality {
    Plain,
    Balanced,
    #[default]
    High,
}

/// Linked response profile for nearby cursor echoes and the large-jump follower.
/// `Balanced` preserves the original nearby echo's opacity and lag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorTrailStrength {
    Subtle,
    #[default]
    Balanced,
    Expressive,
}

impl CursorTrailStrength {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subtle => "subtle",
            Self::Balanced => "balanced",
            Self::Expressive => "expressive",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "subtle" | "low" | "quiet" => Some(Self::Subtle),
            "balanced" | "default" | "normal" => Some(Self::Balanced),
            "expressive" | "high" | "strong" => Some(Self::Expressive),
            _ => None,
        }
    }
}

impl RenderQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Balanced => "balanced",
            Self::High => "high",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "plain" | "fast" | "minimal" | "minimum" => Some(Self::Plain),
            "balanced" | "default" | "normal" => Some(Self::Balanced),
            "high" | "quality" => Some(Self::High),
            _ => None,
        }
    }

    pub fn is_plain(self) -> bool {
        self == Self::Plain
    }
}

/// ID3/U5 background-treatment selector. `Off` (the default) leaves the cell
/// background exactly as resolved, so the plain/fast path is pixel-identical.
/// The others apply a readability-safe per-cell luminance modulation in the
/// grid cell-vertex path, before the RV1 minimum-contrast floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundTreatment {
    /// No treatment — background drawn unchanged.
    Off,
    /// Vertical gradient: darkens toward the bottom rows.
    Gradient,
    /// Radial vignette: darkens toward the edges and corners.
    Vignette,
    /// PNG/WebP background image drawn behind the grid, with a readability scrim
    /// and `cell_bg_opacity` controlling how much shows through behind text.
    /// Unlike `Gradient`/`Vignette`, this treatment does **not** modulate
    /// per-cell background colours — the image lives on its own GPU pass and the
    /// RV1 floor stays valid via the scrim (see [`Settings::cell_bg_opacity`]).
    /// The shipped default (v0.6.0): paired with the bundled default background.
    #[default]
    Image,
}

impl BackgroundTreatment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Gradient => "gradient",
            Self::Vignette => "vignette",
            Self::Image => "image",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            // `color` is the documented opt-out: draw the theme background colour
            // only, disabling the bundled default image (v0.6.0).
            "off" | "none" | "false" | "plain" | "color" | "colour" => Some(Self::Off),
            "gradient" | "linear" => Some(Self::Gradient),
            "vignette" | "radial" => Some(Self::Vignette),
            "image" | "picture" | "photo" => Some(Self::Image),
            _ => None,
        }
    }

    pub fn is_off(self) -> bool {
        self == Self::Off
    }
}

/// Drag-edge autoscroll speed profile (MOUSE-AUTOSCROLL-VEL).
///
/// `Ramp` (default) makes the autoscroll step grow with how far the pointer is
/// dragged past the edge band, up to [`MAX_AUTOSCROLL_ROWS`] rows per tick.
/// `Legacy` pins the step to exactly one row per tick — byte-identical to the
/// pre-feature behavior, and the opt-out for anyone who preferred the fixed
/// rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollDragSpeed {
    #[default]
    Ramp,
    Legacy,
}

impl ScrollDragSpeed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ramp => "ramp",
            Self::Legacy => "legacy",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "ramp" | "velocity" | "accelerated" | "on" => Some(Self::Ramp),
            "legacy" | "fixed" | "constant" | "off" => Some(Self::Legacy),
            _ => None,
        }
    }
}

/// What plain `Ctrl+C` does in the focused terminal (SMART-CTRLC).
///
/// `Off` is the historical, byte-identical opt-out: plain `Ctrl+C` is never
/// intercepted and always encodes the interrupt byte (`0x03`) to the PTY.
/// `CopyOrInterrupt` is the shipped default and overloads it on a single
/// unambiguous signal — the
/// presence of a local OdyTTY selection: with text selected, `Ctrl+C` copies
/// the selection to the clipboard and clears it; with no selection, it sends
/// `0x03` exactly as before. The interrupt is always reachable (no selection,
/// the second press after a copy cleared the selection, `Esc` then `Ctrl+C`, or
/// the unambiguous `Ctrl+Shift+C` copy), and a full-screen TUI never holds a
/// local selection so its `Ctrl+C` keeps interrupting. The enum (rather than a
/// bool) lets the settings panel's value column read `copy-or-interrupt`
/// verbatim and leaves room for future policies without a breaking rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SmartCtrlC {
    /// Plain `Ctrl+C` always sends the interrupt byte (`0x03`). Byte-identical
    /// to a build without the feature.
    Off,
    /// Plain `Ctrl+C` copies and clears a local selection when one exists;
    /// otherwise it sends the interrupt byte. The shipped default (v0.6.0).
    #[default]
    CopyOrInterrupt,
}

impl SmartCtrlC {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::CopyOrInterrupt => "copy-or-interrupt",
        }
    }

    /// Whether plain `Ctrl+C` should be intercepted on a present selection.
    /// `false` for [`SmartCtrlC::Off`], the byte-identical interrupt-always path.
    pub fn is_active(self) -> bool {
        self != Self::Off
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "none" | "disabled" | "interrupt" => Some(Self::Off),
            "copyorinterrupt" | "copy" | "smart" | "on" | "selectioncopy" => {
                Some(Self::CopyOrInterrupt)
            }
            _ => None,
        }
    }
}

/// Colour-vision-deficiency (colour-blindness) adaptation mode (U4).
///
/// `Off` (default) is the pixel-identical baseline: the palette is published
/// exactly as authored. The three deficiency modes daltonise the palette so
/// confusable colours separate for that viewer — `Protan`/`Deutan` target the
/// red–green confusion (the common deficiencies), `Tritan` the blue–yellow. The
/// native theme-wiring layer maps each mode to its colour-vision model and
/// re-floors the result so the adapted palette stays readable; the strength of
/// the correction is [`Settings::cvd_strength`]. Presentation-only — it remaps
/// only how colours are painted, never the terminal model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CvdMode {
    /// No adaptation; the authored palette is published unchanged.
    #[default]
    Off,
    /// Red-cone deficiency (protanopia): red–green confusion.
    Protan,
    /// Green-cone deficiency (deuteranopia, the most common): red–green confusion.
    Deutan,
    /// Blue-cone deficiency (tritanopia, rare): blue–yellow confusion.
    Tritan,
}

impl CvdMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Protan => "protan",
            Self::Deutan => "deutan",
            Self::Tritan => "tritan",
        }
    }

    /// Whether any adaptation is active. `false` for [`CvdMode::Off`], the
    /// pixel-identical baseline the wiring layer short-circuits on.
    pub fn is_active(self) -> bool {
        self != Self::Off
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "none" | "disabled" => Some(Self::Off),
            "protan" | "protanopia" | "protanope" => Some(Self::Protan),
            "deutan" | "deuteran" | "deuteranopia" | "deuteranope" => Some(Self::Deutan),
            "tritan" | "tritanopia" | "tritanope" => Some(Self::Tritan),
            _ => None,
        }
    }
}

/// Whether a clipboard image pasted into a remote *integrated* ssh tab is
/// offered for upload (F6-i7). `Ask` (default) prompts before every upload —
/// image bytes never leave the machine on a keystroke without confirmation;
/// `Off` disables image paste-through entirely, so such a paste is a no-op.
/// There is deliberately no silent `Always`: confirm-first is the only enabled
/// mode. A per-host override is a follow-up; this is the global default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoteImagePaste {
    /// Prompt before uploading; the safe default.
    #[default]
    Ask,
    /// Image paste-through disabled — a clipboard-image paste does nothing.
    Off,
}

/// Policy for OSC 52 clipboard writes emitted by terminal applications.
/// Window focus and active-session checks are mandatory in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Osc52WritePolicy {
    /// Drain and discard all OSC 52 writes.
    Off,
    /// Require an explicit, ephemeral per-session decision. This is the default:
    /// a program setting the system clipboard first asks for consent, so an
    /// unattended write cannot silently replace the clipboard.
    #[default]
    Ask,
    /// Apply focused writes immediately and show a bounded neutral notice.
    On,
}

impl Osc52WritePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ask => "ask",
            Self::On => "on",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "no" | "disabled" | "false" => Some(Self::Off),
            "ask" | "confirm" | "prompt" => Some(Self::Ask),
            "on" | "yes" | "allow" | "true" => Some(Self::On),
            _ => None,
        }
    }
}

impl RemoteImagePaste {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Off => "off",
        }
    }

    /// Whether a clipboard-image paste should prompt for upload (i.e. the
    /// feature is enabled). `Off` returns `false`, so the paste falls through.
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Ask)
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "ask" | "on" | "confirm" | "prompt" => Some(Self::Ask),
            "off" | "no" | "disabled" | "false" => Some(Self::Off),
            _ => None,
        }
    }
}

/// Where the tab bar/rail sits relative to the terminal content (F4-V2).
/// `Top` (default) is the shipped horizontal strip; `Left`/`Right` are the
/// vertical rails on either side. All three placements render (F4-P2 landed the
/// `Right` arm), so [`TabBarPlacement::effective`] is now an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabBarPlacement {
    /// Horizontal strip across the top (the shipped default).
    #[default]
    Top,
    /// Vertical rail down the left side (F4-V2 R1).
    Left,
    /// Vertical rail down the right side (F4-P2). Mirror of the left rail: the
    /// content stays at column 0 and the rail band sits at the far right; the
    /// panel seam flips to the band's left (content-facing) edge.
    Right,
}

impl TabBarPlacement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// Whether this placement is a vertical rail (Left/Right) rather than the
    /// horizontal top strip.
    pub fn is_rail(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }

    /// The rail-side label this placement maps to: `Right` -> "right", every
    /// other placement (including the vestigial `Top`) -> "left". The settings
    /// surface presents rail SIDE as left|right only; `Top` is retained for
    /// back-compat parsing and folds to the left rail.
    pub fn rail_side_str(self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Top | Self::Left => "left",
        }
    }

    /// The placement actually honored by the current render path. All three
    /// placements now render (F4-P2 landed the `Right` rail), so this is an
    /// identity. It is retained as the single choke point the render/reserve
    /// paths read, so a future placement that needs a fallback has one seam to
    /// add it — callers keep going through `effective()` rather than the raw
    /// setting.
    pub fn effective(self) -> Self {
        self
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "top" | "horizontal" | "bar" => Some(Self::Top),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

/// Workspace-rail visibility and side (design doc ODP-2). The rail lists
/// WORKSPACES (never tabs — tabs are the top bar, always). `Auto` (default)
/// reveals the rail only once a second workspace exists, so a single-workspace
/// launch is a zero-chrome change from a top-only tab bar. `Always` pins it even
/// with one workspace. `Left`/`Right` pin it AND force the side. For
/// `Auto`/`Always` the side is inherited from [`TabBarPlacement`] so a former
/// vertical-tab user keeps their side: `left`/`right` map through, `top`
/// defaults the rail to the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceRail {
    /// Appear only with two or more workspaces (the default; single-workspace =
    /// no rail).
    #[default]
    Auto,
    /// Always visible, even with a single workspace; side inherited from
    /// `tab_bar_placement`.
    Always,
    /// Always visible, pinned to the left side (explicit override).
    Left,
    /// Always visible, pinned to the right side (explicit override).
    Right,
}

impl WorkspaceRail {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// Whether the rail stays pinned regardless of workspace count. `Auto` is the
    /// only mode that hides at a single workspace.
    pub fn always_visible(self) -> bool {
        !matches!(self, Self::Auto)
    }

    /// The side this value forces, or `None` to inherit `tab_bar_placement`.
    pub fn forced_side(self) -> Option<TabBarPlacement> {
        match self {
            Self::Left => Some(TabBarPlacement::Left),
            Self::Right => Some(TabBarPlacement::Right),
            Self::Auto | Self::Always => None,
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "auto" | "default" => Some(Self::Auto),
            "always" | "on" | "pinned" => Some(Self::Always),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }
}

/// ControlPersist window for the reuse master (ODP-9 Tier 2, `remote_persist`).
/// The reuse path keeps an authenticated `ssh` master alive after the last tab
/// to a host closes, so a daily-driver host is authenticated roughly once per
/// boot rather than once per tab. This knob sets how long that master lingers.
///
/// `Min10` (the default) maps to `ControlPersist=600` — byte-identical to the
/// historical fixed 10-minute window, so default behavior is unchanged. `Off`
/// maps to `ControlPersist=no`, tearing the master down with its last
/// connection (the pre-persist security posture). The remaining variants extend
/// the window. A per-host `Persist` line in `hosts.conf` overrides this globally
/// and additionally accepts any raw `ssh` ControlPersist value. Unix-only: the
/// reuse control options are compiled out on a Windows client, so this value is
/// never emitted there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemotePersist {
    /// The shared master dies with its last connection (`ControlPersist=no`).
    Off,
    /// Keep the master alive 10 minutes past the last tab (default; the historical
    /// fixed window, so the emitted argv is byte-identical to before the knob).
    #[default]
    Min10,
    /// Keep the master alive 30 minutes past the last tab.
    Min30,
    /// Keep the master alive 1 hour past the last tab.
    Hour1,
    /// Keep the master alive 2 hours past the last tab.
    Hour2,
}

impl RemotePersist {
    /// The persisted / config token for this value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Min10 => "10m",
            Self::Min30 => "30m",
            Self::Hour1 => "1h",
            Self::Hour2 => "2h",
        }
    }

    /// The `ControlPersist=` argument value (Unix reuse only): `no` for `Off`,
    /// otherwise the window in whole seconds. `Min10` -> `600`, matching the
    /// historical fixed window so the default argv is byte-identical.
    pub fn control_persist_value(self) -> &'static str {
        match self {
            Self::Off => "no",
            Self::Min10 => "600",
            Self::Min30 => "1800",
            Self::Hour1 => "3600",
            Self::Hour2 => "7200",
        }
    }

    /// Parse a config/override token. Accepts the canonical forms plus common
    /// synonyms (`0`/`no`/`none` = off; bare seconds for the presets), so a hand
    /// -written config or a per-host `Persist` value resolves without surprise.
    /// Unrecognized values yield `None` (the caller warns and keeps the default).
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "0" | "no" | "none" | "false" => Some(Self::Off),
            "10m" | "600" | "600s" => Some(Self::Min10),
            "30m" | "1800" | "1800s" => Some(Self::Min30),
            "1h" | "60m" | "3600" | "3600s" => Some(Self::Hour1),
            "2h" | "120m" | "7200" | "7200s" => Some(Self::Hour2),
            _ => None,
        }
    }
}

/// Vertical tab-rail width mode (F4-P4): `Auto` sizes the rail to the longest
/// tab title (clamped to `[MIN_TAB_RAIL_WIDTH, tab_rail_max_width]`); `Manual`
/// pins an operator-chosen width in cells (clamped to `[MIN_TAB_RAIL_WIDTH,
/// MAX_TAB_RAIL_WIDTH]`). `Auto` is the new default — the rail behaves like a
/// Finder column, growing to fit titles and truncating with an ellipsis past
/// the cap. A `Manual` width is what the seam drag and a numeric config value
/// produce; the double-click-seam gesture resets back to `Auto`.
///
/// Persisted as `auto` or a plain integer, so an existing numeric config
/// (`tab_rail_width = 20`) round-trips as `Manual(20)` — old configs keep their
/// exact behavior (F4-P4 migration rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabRailWidth {
    /// Auto-size to the longest tab title, clamped to the auto max (default).
    #[default]
    Auto,
    /// A fixed rail width in cells (already clamped to the widget bounds).
    Manual(u16),
}

impl TabRailWidth {
    /// The config/`to_edit_values` string form: `"auto"` or the column count.
    pub fn as_config_string(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Manual(cols) => cols.to_string(),
        }
    }

    /// `Some(cols)` for a manual width, `None` for auto — the payload the width
    /// resolver clamps in manual mode.
    pub fn manual_cols(self) -> Option<u16> {
        match self {
            Self::Auto => None,
            Self::Manual(cols) => Some(cols),
        }
    }
}

/// Top tab-bar height mode: `Auto` is the classic single text row (the default);
/// `Manual(rows)` pins an operator-chosen band height in text rows (clamped to
/// `[MIN_TAB_BAR_ROWS, MAX_TAB_BAR_ROWS]`). A taller band is chrome around the
/// one row of labels, which are centered vertically in it. Mirrors
/// [`TabRailWidth`] one axis over: the draggable bottom seam and a numeric config
/// value produce a `Manual` height, and the double-click-seam gesture resets to
/// `Auto`. Persisted as `auto` or a plain integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabBarHeight {
    /// The classic one-text-row bar (default).
    #[default]
    Auto,
    /// A fixed bar height in text rows (clamped to the widget bounds on resolve).
    Manual(u16),
}

impl TabBarHeight {
    /// The config/`to_edit_values` string form: `"auto"` or the row count.
    pub fn as_config_string(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Manual(rows) => rows.to_string(),
        }
    }

    /// The resolved band height in text rows: `Auto` is the default one row; a
    /// `Manual` height clamps to `[MIN_TAB_BAR_ROWS, MAX_TAB_BAR_ROWS]`. Never
    /// zero, so the reservation math always keeps at least one row of labels.
    pub fn resolved_rows(self) -> usize {
        match self {
            Self::Auto => DEFAULT_TAB_BAR_ROWS as usize,
            Self::Manual(rows) => {
                (rows as usize).clamp(MIN_TAB_BAR_ROWS as usize, MAX_TAB_BAR_ROWS as usize)
            }
        }
    }
}

/// How the terminal responds when the host writes BEL (`0x07`). Presentation-
/// only — the core merely latches that a bell was requested (see
/// [`crate::core::Terminal::take_bell`]); this setting decides what the native
/// layer does with that signal. OdyTTY has no audio backend, so there is no
/// audible mode; the bell is conveyed visually and/or via window urgency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BellMode {
    /// Drain and ignore. The pre-bell-fix behavior, kept as an explicit opt-out.
    Off,
    /// Brief readability-safe screen flash on every bell, focused or not.
    Visual,
    /// Request window user-attention (effective when unfocused); no flash. The
    /// do-no-harm default: a finished long-running job pings the taskbar, but a
    /// focused shell never flashes on tab-completion bells.
    #[default]
    Urgent,
    /// Both the visual flash and the window-urgency request.
    All,
}

impl BellMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Visual => "visual",
            Self::Urgent => "urgent",
            Self::All => "all",
        }
    }

    /// Whether a bell should paint the visual flash.
    pub fn wants_visual(self) -> bool {
        matches!(self, Self::Visual | Self::All)
    }

    /// Whether a bell should request window user-attention.
    pub fn wants_urgent(self) -> bool {
        matches!(self, Self::Urgent | Self::All)
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "none" | "disabled" => Some(Self::Off),
            "visual" | "flash" => Some(Self::Visual),
            "urgent" | "attention" => Some(Self::Urgent),
            "all" | "both" => Some(Self::All),
            _ => None,
        }
    }
}

/// Presentation policy for completion and terminal notification hints. BEL is
/// deliberately separate and continues to use [`BellMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationMode {
    Off,
    #[default]
    InApp,
    Attention,
    Desktop,
}

impl NotificationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::InApp => "in-app",
            Self::Attention => "attention",
            Self::Desktop => "desktop",
        }
    }

    pub fn shows_in_app(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn wants_attention(self) -> bool {
        matches!(self, Self::Attention)
    }

    pub fn wants_desktop(self) -> bool {
        matches!(self, Self::Desktop)
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "none" | "disabled" => Some(Self::Off),
            "inapp" | "badge" | "badges" => Some(Self::InApp),
            "attention" | "urgent" => Some(Self::Attention),
            "desktop" | "native" | "notification" => Some(Self::Desktop),
            _ => None,
        }
    }
}

/// What typing `exit` (or Ctrl-D EOF on a live shell) does when it would close a
/// whole workspace. Governs ONLY the shell-exit path -- the rail close button
/// and the close-tab / close-workspace / close-pane keybinds keep their
/// per-surface meaning in both modes. Presentation-independent behavior toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellExitCloses {
    /// A shell exit that empties its workspace closes just that workspace (the
    /// historical cascade). Closing the last workspace still exits the app.
    #[default]
    Workspace,
    /// A shell exit that would close a workspace quits OdyTTY instead, even when
    /// other workspaces exist -- pairs with layout restore so the whole set
    /// reopens next launch. Exits with sibling tabs/panes still close only the
    /// tab/pane; only the workspace-closing case escalates to an app quit.
    App,
}

impl ShellExitCloses {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::App => "app",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "workspace" | "workspaceonly" | "closeworkspace" => Some(Self::Workspace),
            "app" | "application" | "quit" | "closeapp" => Some(Self::App),
            _ => None,
        }
    }
}

/// Typed runtime settings used by the native prototype.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: Theme,
    /// The RAW `theme` config string the user set (a built-in name, a user
    /// theme name, or a theme file path), or `None` when unset. Kept distinct
    /// from [`Settings::theme`] the way [`Settings::explicit_font_path`] is kept
    /// distinct from the resolved font: a theme loaded from a FILE projects to
    /// the `&'static` placeholder name `"custom"` ([`crate::theme::ThemeSpec::to_theme`]),
    /// which resolves to no built-in and no file on a later re-parse. Persisting
    /// that placeholder as the writeback baseline made every settings edit fail
    /// for custom-theme users (the fallback warning was promoted to a hard
    /// error). Preserving the original config string here lets the writeback and
    /// panel display round-trip the real value so the file re-reads instead.
    pub theme_config: Option<String>,
    pub visual: VisualEffect,
    /// The EFFECTIVE regular-face path the renderer loads: either the explicit
    /// `font` key, or — when only `font_family` is set — the regular face
    /// resolved from that family. Internal/derived; not the raw config value.
    pub font_path: Option<PathBuf>,
    /// The RAW explicit `font` config key (an advanced one-file override), or
    /// `None` when unset. Kept distinct from [`Settings::font_path`] so the UI
    /// and writeback reflect what the user actually set: picking a `font_family`
    /// populates `font_path` with the resolved face but must NOT make the
    /// advanced `font` row look set (RC4). This is the value the `font` row
    /// displays and the writeback persists for the `font` key.
    pub explicit_font_path: Option<PathBuf>,
    pub font_family: Option<String>,
    /// Optional weight-variant suffix appended to [`Settings::font_family`] to
    /// select a lighter or heavier base face (RV7), e.g. `"Light"`, `"Medium"`,
    /// `"SemiBold"`. Empty (the default) uses the family's regular face exactly
    /// as before — the off path loads the identical face and is byte-identical.
    /// Distinct from the SGR bold attribute: bold/italic discovery always uses
    /// the plain `font_family`, so SGR bold stays visually distinct from the
    /// chosen base weight. Real faces only — a missing weight face warns and
    /// falls back to the regular face (never synthetic emboldening/thinning).
    pub font_weight: String,
    pub font_size_px: f32,
    pub text_gamma: f32,
    /// Stem-darkening strength in `0.0..=1.0` (RV5). `0.0` disables the
    /// raster-time coverage boost and is pixel-identical to before.
    pub stem_darken: f32,
    /// Minimum fg/bg WCAG contrast floor in `1.0..=21.0` (RV1). `1.0`
    /// disables enforcement and is pixel-identical to before.
    pub min_contrast: f32,
    pub focus_dim: f32,
    /// Inactive-pane dimming amount in `0.0..=1.0`. `0.0` (the default) disables
    /// it and is byte-identical to the pre-feature multi-pane renderer; higher
    /// values dim the non-focused panes of a multi-pane tab so the focused pane
    /// stands out. The focused pane is never dimmed and single-pane tabs are
    /// never affected.
    pub inactive_pane_dim: f32,
    pub render_quality: RenderQuality,
    /// ID3/U5 background treatment (`off` default ⇒ pixel-identical plain path).
    pub background_treatment: BackgroundTreatment,
    /// Optional PNG image file path for the `image` background treatment. `None`
    /// (the default) means no image — the `image` treatment then behaves exactly
    /// like `off`. A missing / unreadable / non-PNG file warns and falls back
    /// gracefully (no image, no crash).
    pub background_image: Option<PathBuf>,
    /// CPU box-blur radius (logical pixels) applied to `background_image` once at
    /// load time. `0` (the default) leaves the image sharp. Blur is skipped with
    /// a warning if the image exceeds 4096×4096px.
    pub background_blur_radius: u32,
    /// Explicit readability-scrim override in `0.0..=1.0` for the background
    /// image. `None` (the default) auto-computes the scrim from the image's
    /// worst-case luminance + the theme so the RV1 floor is guaranteed; an
    /// explicit value lets an expert dial it back.
    pub background_image_scrim: Option<f32>,
    /// Cell background opacity in `0.0..=1.0`. `1.0` (the default) keeps cells
    /// fully opaque — the background image shows only in the window padding, and
    /// the cell-vertex output is byte-identical to before. Values `< 1.0` make
    /// cells translucent so the image shows through behind text; the RV1 floor
    /// stays safe at any opacity via the readability scrim.
    pub cell_bg_opacity: f32,
    /// Colored-background opacity floor in `0.0..=1.0`: the minimum window-alpha
    /// contribution for cells whose resolved background differs from the theme
    /// default (powerline segments, button chips, status blocks), so they stay
    /// strong as window opacity drops instead of washing out with the empty
    /// backdrop. `0.0` disables the floor (colored cells match today exactly);
    /// an opaque window is byte-identical at any value. Never weakens a cell.
    pub colored_bg_opacity: f32,
    /// Selection-highlight opacity in `0.0..=1.0`, independent of window opacity,
    /// theme colours, and the min-contrast floor. `1.0` (the default) keeps the
    /// selection fully opaque — byte-identical to before in the opaque-window
    /// path, and the documented "fully opaque" behaviour under window
    /// transparency. Values `< 1.0` make the selection translucent so a busy or
    /// transparent backdrop shows through behind it; the RV1 floor still holds
    /// foreground legibility over the effective composited fill.
    pub selection_opacity: f32,
    /// Text-brightness lift in `1.0..=1.5`: glyph foregrounds are lifted toward
    /// white in linear space with a soft knee, for legibility over a busy
    /// backdrop at low window opacity. `1.0` (the default) is exact identity.
    /// Applied after the min-contrast floor; color emoji are exempt.
    pub text_brightness: f32,
    /// Logical pixels of inset between the window edge and terminal grid. `0.0`
    /// preserves the historical edge-to-edge geometry exactly.
    pub window_padding_px: f32,
    pub bloom: bool,
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub bloom_radius: f32,
    /// Cohesive retro/phosphor preset. When on, effective bloom/CRT values are
    /// promoted to a stronger tuned profile without overwriting the individual
    /// knobs; `render_quality=plain` still forces every post-process off.
    pub retro: bool,
    pub crt: bool,
    pub crt_scanline_intensity: f32,
    pub crt_scanline_period: f32,
    pub crt_vignette_strength: f32,
    /// CRT screen curvature (`CRT_CURVATURE`, 0.0–0.12). `0.0` (default) is
    /// flat; higher values barrel-distort the composited frame toward the
    /// screen edges. Inert on the plain renderer profile and forced `0.0`
    /// there; the retro preset overrides it to a subtle curve via
    /// [`Self::effective_crt_curvature`].
    pub crt_curvature: f32,
    pub subpixel: SubpixelMode,
    /// Line-height multiplier baked into the glyph cell (LINEHEIGHT). `1.0`
    /// (default) adds zero leading and is pixel-identical to before; higher
    /// values grow the cell box and add symmetric vertical breathing room.
    pub line_height: f32,
    /// Box-drawing stroke-thickness multiplier (BOXTHICK). `1.0` (default)
    /// reproduces the historical geometric box-drawing weights byte-identically;
    /// other values scale the rule thickness.
    pub box_thickness: f32,
    /// Presentation shaping overlays: ASCII `calt`+`liga`, curated operator
    /// allowlist, and Arabic joining forms in logical LTR cell order (not
    /// bidi). Enabled by default; logical cells, cursor coordinates, copy, and
    /// selection remain unchanged. Optional `ss01`/`ss02` ride separate
    /// settings (off by default).
    pub ligatures: bool,
    /// Optional OpenType stylistic set `ss01`. Off by default; applies only
    /// while [`Self::ligatures`] is on. No other `ssXX` tags are exposed.
    pub ligature_ss01: bool,
    /// Optional OpenType stylistic set `ss02`. Off by default; applies only
    /// while [`Self::ligatures`] is on. No other `ssXX` tags are exposed.
    pub ligature_ss02: bool,
    /// Whether Kitty graphics may read file, temporary-file, and POSIX
    /// shared-memory transports named by terminal output. Off by default;
    /// direct and chunked-inline graphics remain available.
    pub kitty_named_transports: bool,
    pub key_bindings: Vec<KeyBindingOverride>,
    /// Multiplexer prefix chord (§7). `Some(Ctrl-b)` by default — the single
    /// new globally-captured key that opens the transient pane-command mode.
    /// `None` (config value `off`/`none`/`disabled`) disables the prefix model
    /// entirely, making the input path byte-identical to the pre-§7 build.
    pub pane_prefix: Option<KeyChord>,
    /// Default cursor shape applied at power-on (DECSCUSR can override).
    pub cursor_style: CursorStyle,
    /// Default cursor blink policy applied at power-on (DECSCUSR can override).
    pub cursor_blink: CursorBlink,
    /// Whether the cursor eases its opacity across the blink toggle (ID1). On
    /// by default; the off path holds alpha at `1.0` and hard-hides on the blink
    /// off-phase. Purely presentational.
    pub cursor_easing: bool,
    /// Whether the cursor draws one shape-aware analytic aura behind its glyph,
    /// matching Block, Bar, or Underline geometry (ID1). On by default; the
    /// off path emits no aura geometry. Purely presentational; never affects
    /// cell semantics or the logical cursor position.
    pub cursor_glow: bool,
    /// Cursor glow strength on a normalized `0.0..=1.0` scale, independent of
    /// the whole-scene HDR `bloom_intensity`. `0.0` emits no aura even while
    /// `cursor_glow` is on; the default reproduces the calibrated restrained
    /// peak; `1.0` is stronger but bounded so nearby text stays readable and
    /// translucent backgrounds never receive an excessive alpha lift. Purely
    /// presentational; hot-reloadable.
    pub cursor_glow_intensity: f32,
    /// Whether a short fading after-image trails the cursor along its slide path
    /// (VE4). On by default; the off path emits no trail quads and arms no
    /// extra wake, byte-identical to before. Rides the cursor-slide animation,
    /// so it is visible only while `cursor_motion` is also on; purely
    /// presentational, never affects cell semantics or the logical cursor.
    pub cursor_trail: bool,
    /// Linked response profile for the nearby echo and large-jump follower.
    pub cursor_trail_strength: CursorTrailStrength,
    /// Whether the cursor glides between adjacent positions instead of
    /// teleporting (VE4). On by default; the off path sits at the exact cell
    /// origin with zero offset. Discontinuities always snap. The logical cursor
    /// position is always the destination cell.
    pub cursor_motion: bool,
    /// Master accessibility gate for cursor slide, trail, glow, blink fade, and
    /// new-output fade. When on, the effects use static or instant behavior
    /// without mutating their stored individual settings.
    pub reduced_motion: bool,
    /// Whether OSC 52 clipboard read/query replies are enabled. Off by default
    /// to avoid silent clipboard exfiltration.
    pub osc52_read: bool,
    /// OSC 52 clipboard-write policy. `On` preserves peer-terminal copy
    /// compatibility; the native layer still requires active-session and OS
    /// focus before applying any write.
    pub osc52_write: Osc52WritePolicy,
    /// Whether the renderer synthesizes missing bold/italic faces from the
    /// regular outline (double-strike embolden + shear). On by default; turning
    /// it off makes styled cells render as plain regular glyphs when no real
    /// face is loaded. Purely presentational — never affects cell semantics.
    pub synthetic_styles: bool,
    /// Whether box-drawing, block-element and Powerline glyphs are rendered
    /// geometrically (cell-aligned rectangles/rails/arcs/triangles) instead of
    /// from the font (RV2). On by default; turning it off restores the
    /// byte-identical font path. Purely presentational — never affects cell
    /// semantics.
    pub geometric_boxdraw: bool,
    /// Whether to install a symbol / Nerd-font fallback face for private-use
    /// prompt icons (RV6). On by default so common PUA prompt icons render out
    /// of the box; set to `false` to force the plain missing-glyph path.
    pub symbol_fallback: bool,
    /// Optional explicit symbol / Nerd-font file path. `None` means auto-resolve
    /// a suitable symbol face when [`Settings::symbol_fallback`] is enabled.
    pub symbol_font: Option<PathBuf>,
    /// Per-codepoint-range font-family override (SYMMAP, extends RV6). Routes
    /// specific Unicode ranges to named font families without patching the body
    /// font. The default is an empty map: every lookup returns `None`, so glyph
    /// resolution is byte-identical to the no-override path. First-match-wins.
    pub symbol_map: crate::text::SymbolMap,
    /// Whether semantic theme roles drive native cursor, selection, and search
    /// highlight colors. On by default by operator decision; turning it off
    /// restores the historical foreground cursor, inverse selection, and
    /// black-on-yellow active search treatment.
    pub themed_ui_roles: bool,
    /// Rows of local scrollback advanced per mouse-wheel notch (MOUSE-WHEEL-SPEED).
    /// Default `3.0` is byte-identical to the historical fixed step. Stored as
    /// `f32` to ride the shared numeric-setting model; [`Settings::scroll_wheel_step`]
    /// rounds it to a `usize >= 1` for the wheel path. Affects local viewport
    /// scroll only — never the TUI mouse-reporting path.
    pub scroll_wheel_lines: f32,
    /// Scrollback retention cap in logical lines (SCROLLBACK-CAP). Default
    /// `10000.0`. Bounds steady-state memory so unbounded output cannot OOM the
    /// process. Stored as `f32` to ride the shared numeric-setting model;
    /// [`Settings::scrollback_limit`] rounds it to a `usize` for the core. `0`
    /// means unbounded. Live-reloadable; lowering it trims history immediately.
    pub scrollback_lines: f32,
    /// Drag-edge autoscroll speed profile (MOUSE-AUTOSCROLL-VEL). `Ramp` (the
    /// default) accelerates the autoscroll step with overshoot past the edge
    /// band, capped at [`MAX_AUTOSCROLL_ROWS`]; `Legacy` pins it to a fixed one
    /// row per tick, byte-identical to the pre-feature behavior. Local selection
    /// drag only — never the TUI mouse-reporting path.
    pub scroll_drag_speed: ScrollDragSpeed,
    /// When on, finishing a local selection also writes the CLIPBOARD, not just
    /// the PRIMARY selection (MOUSE-COPYSELECT). Off by default — the off path is
    /// byte-identical to before (PRIMARY + middle-click paste already work).
    pub copy_on_select: bool,
    /// What plain `Ctrl+C` does in the focused terminal (SMART-CTRLC). `Off`
    /// (default) always sends the interrupt byte, byte-identical to before;
    /// `CopyOrInterrupt` copies and clears a local selection when one exists and
    /// otherwise interrupts. Read live at keypress time; no rebuild on change.
    pub smart_ctrl_c: SmartCtrlC,
    /// When on, hovering a bare (non-OSC-8) URL with an allowlisted scheme shows
    /// the hand cursor + Ctrl+hover armed underline and Ctrl+click opens it
    /// through the same argv-only, scheme-allowlisted dispatch as OSC 8
    /// (INTERACTIVE-URLS). On by default (operator decision); off makes the
    /// bare-URL hover scan never run, so the hover path is byte-identical to a
    /// build without the feature. Independent of `interactive_paths`.
    pub interactive_urls: bool,
    /// When on, a double-click-then-drag extends the selection by whole words, a
    /// triple-click-then-drag by whole lines, and Shift+click extends the
    /// current selection (MOUSE-EXTEND). On by default (operator decision); off
    /// restores the historical click-to-finish behavior byte-identically.
    pub selection_drag_extend: bool,
    /// When on, the right-edge scroll indicator is a draggable thumb: a left
    /// press grabs it and the drag scrubs scrollback (MOUSE-SCROLLBAR). On by
    /// default; the thumb only renders while scrolled back, so the grab is inert
    /// at the live tail and the off path leaves press routing byte-identical.
    pub scrollbar_drag: bool,
    /// When on, Ctrl+wheel adjusts the font size (up = larger, down = smaller)
    /// while mouse reporting is off (MOUSE-WHEEL zoom). On by default; it only
    /// fires on the explicit Ctrl+wheel gesture, so a plain wheel — and the
    /// wheel inside a TUI mouse-reporting app — stays byte-identical. Off
    /// returns Ctrl+wheel to plain scrollback movement.
    pub wheel_zoom: bool,
    /// Per-command success/fail gutter (SH2). When on, a thin coloured bar at
    /// the left edge of each finished command's prompt row reads green for an
    /// explicit `exit 0` and red for a non-zero exit, sourced from the OSC 133
    /// command blocks and coloured from the active ANSI palette (so it
    /// daltonises with U4 for free). On by default; the off path draws nothing
    /// and is pixel-identical to a plain margin.
    pub command_status_gutter: bool,
    /// Always show the tab bar, even for a single tab (F4 ODP-7). Off by
    /// default; with one unnamed tab the bar stays hidden and the render path is
    /// byte-identical to today. A lone tab with a custom name shows the bar
    /// regardless of this setting (F4-NF1).
    pub always_show_tab_bar: bool,
    /// Which side the workspace rail sits on when it is shown; tabs always
    /// render on the top bar. `Top` (default) places the rail on the left when
    /// it appears; `Left` and `Right` pin the rail to that side. A former
    /// vertical-tab user keeps their chosen side through this mapping. Default
    /// `Top` keeps the single-workspace view byte-identical to the shipped
    /// top-only bar.
    pub tab_bar_placement: TabBarPlacement,
    /// Workspace-rail visibility/side (ODP-2). `Auto` (default) shows the rail
    /// only with two or more workspaces; a single-workspace session keeps the
    /// top-only tab bar unchanged. See [`WorkspaceRail`].
    pub workspace_rail: WorkspaceRail,
    /// Vertical rail width mode (F4-P1/P4): `Auto` (default) sizes to the longest
    /// tab title; `Manual(cols)` pins a fixed width. Resolved to a `usize` by
    /// [`Settings::rail_width_cols`]. Rail-only; the top bar ignores it.
    pub tab_rail_width: TabRailWidth,
    /// Top tab-bar height mode: `Auto` (default) is one text row; `Manual(rows)`
    /// pins a taller band. Resolved to a `usize` by
    /// [`TabBarHeight::resolved_rows`]. Only affects the top bar.
    pub tab_bar_height: TabBarHeight,
    /// Upper clamp (cells) for the `auto` rail width (F4-P4). Only consulted in
    /// `Auto` mode; a manual width clamps to the absolute widget max instead.
    /// Stored as `f32` for the shared numeric-setting model;
    /// [`Settings::rail_max_width_cols`] rounds it to a `usize`.
    pub tab_rail_max_width: f32,
    /// Inter-slot gap in rows for the vertical rail (F4-P1); the top margin
    /// follows it. Stored as `f32`; [`Settings::rail_slot_gap_rows`] rounds it to
    /// a `usize` in `[0, 3]`.
    pub tab_rail_gap: f32,
    /// Rail slot height in rows (F4-P1): `1` = compact list, `2` = padded/wrapping
    /// default. Stored as `f32`; [`Settings::rail_slot_rows`] rounds and clamps
    /// it to `{1, 2}`.
    pub tab_rail_slot_rows: f32,
    /// Unified tab-panel strength (F4-P1): `0.0` = panel off, `1.0` = strongest,
    /// and `0.8` is the default. Drives both the panel tint lift and the
    /// panel-wash quad alpha. Both axes.
    pub tab_panel_strength: f32,
    /// Tab-panel seam line (F4-P1): one content-boundary hairline on both axes.
    /// Off by default; off removes only the line (the panel stays).
    pub tab_seam: bool,
    /// Rail auto-hide (F4-P1/P3): reveals a floating rail from the configured
    /// edge zone without content reflow. Off by default, rail-only.
    pub tab_rail_autohide: bool,
    /// Rail auto-hide reveal-zone width in **logical** px (F4-P3): how close to
    /// the rail's window edge the pointer summons an auto-hidden rail. Scaled by
    /// the display scale factor at the comparison site so the zone is a
    /// consistent physical size across displays. Stored as `f32` in `[1, 32]`.
    pub tab_rail_reveal_px: f32,
    /// Click-to-position-cursor on the live prompt (SH-CLICK). When on, a plain
    /// left click on the shell prompt line moves the shell's input cursor to the
    /// clicked column by emitting Left/Right cursor keys — the click slice of
    /// OSC 133 `click_events`. On by default and gated on the shell having
    /// advertised `click_events=1`, so a non-integrated shell emits nothing.
    pub sh_click: bool,
    /// Button protocol master gate (docs/buttons.md). When on, programs can
    /// define clickable buttons in their output and clicks report an integer
    /// code back to the program; `ODYTTY_BUTTONS=1` is injected into new
    /// sessions' environment for emitter discovery. On by default; turning it
    /// off parses and discards sequences, prevents click reports, and restores
    /// the byte-identical plain path.
    pub buttons: bool,
    /// Accept the iTerm2 `OSC 1337 ; Button=` spelling. Sub-gate of `buttons`;
    /// inert while the master gate is off.
    pub buttons_iterm_compat: bool,
    /// Honor `scope=sticky` button lifetimes (survive prompt boundaries, live
    /// from scrollback). When off, sticky requests downgrade to the block
    /// lifetime. Sub-gate of `buttons`; inert while the master gate is off.
    pub buttons_sticky: bool,
    /// Automatic OSC 133 shell integration. When on, default local shell spawns
    /// receive OdyTTY's prompt-mark hooks without editing user rc files. On by
    /// default; affects newly spawned shells only.
    pub shell_integration: bool,
    /// Prompt-scoped Kitty keyboard enhancement for integrated bash/zsh shells.
    /// When on (and `shell_integration` is on), the shell pushes keyboard flag
    /// 0x1 (disambiguate only) while the prompt owns the line and pops it before
    /// each command, so modified keys like Ctrl+Enter become distinct CSI-u
    /// sequences the user can bind. Zero effect on programs the shell launches;
    /// fish/PowerShell are unaffected. On by default.
    pub shell_key_enhancement: bool,
    /// Restore the previous workspace/tab/pane shape at launch (WP2, sub-ODP
    /// 8a). Off by default; only a bare `odytty` launch restores, and any CLI
    /// argument suppresses it. Shape-only (names/titles/order/split-tree/cwd),
    /// never grid content or commands. The autosave that feeds restore runs on
    /// the primary instance regardless of this flag.
    pub restore_workspaces: bool,
    /// Whether freshly arrived output rows fade their TEXT in at the live tail
    /// (VE4). On by default; the off path schedules no extra wakes. The fade is
    /// a per-row foreground alpha ramp —
    /// glyphs, decorations, and emoji rise from a visible floor to full
    /// strength while cell backgrounds render exactly as normal from the first
    /// frame (a background veil read as a dark flash on translucent windows).
    /// Only at the live tail; scrollback and resize snap; the cursor's row
    /// never fades. Purely presentational.
    pub new_output_fade: bool,
    /// New-output fade ramp length in milliseconds (VE4). Only acts while
    /// [`Self::new_output_fade`] is on; a longer value makes the fade-in more
    /// perceptible, a shorter one restores the original quick ramp. Clamped to
    /// `[MIN_NEW_OUTPUT_FADE_MS, MAX_NEW_OUTPUT_FADE_MS]`. Purely presentational.
    pub new_output_fade_ms: f32,
    /// Whether a thin themed border is drawn around the grid (ID4). Off by
    /// default; the off path emits no border quads and is byte-identical to
    /// before. The border is painted in the theme `border` role color within the
    /// existing padding band (it never eats cell area), its thickness scaled by
    /// the surface DPI factor, and it tracks the content rect on resize. Purely
    /// presentational; never affects cell semantics.
    pub window_border: bool,
    /// Whether the window keeps its decorations — title bar and borders
    /// (WIN-DECOR). On by default; `true` reproduces the historical
    /// window-attribute chain exactly (pixel-identical startup). When off,
    /// OdyTTY requests a borderless surface both at window creation and live on
    /// a settings change. Effect is environment-dependent: Wayland compositors
    /// remove the title bar reliably, while X11 window managers honor the
    /// request on a best-effort basis — never a hard guarantee.
    pub window_decorations: bool,
    /// Master toggle for whole-window transparency (TRANSPARENCY). Off by
    /// default, so the opaque render path is byte-identical to the historical
    /// presentation. When on (and the display server offers alpha
    /// compositing), the terminal background is drawn at `window_opacity` so
    /// the desktop shows through; text, cursor, selection, and overlays stay
    /// fully opaque. Requires a compositing window manager.
    pub window_transparency: bool,
    /// Window opacity as a percent of full opacity (TRANSPARENCY), applied to
    /// the background only when `window_transparency` is on. Clamped to
    /// `[MIN_WINDOW_OPACITY, MAX_WINDOW_OPACITY]`; 100 is fully opaque.
    pub window_opacity: f32,
    /// Continuous pixel-precise scrollback for high-resolution wheels/touchpads
    /// (SCROLL-FEEL Tier 2). On by default. When on, `PixelDelta` input drives a
    /// continuous sub-row lane tracking physical travel instead of quantizing to
    /// whole notches; it affects only pixel-precise input, so detented wheels
    /// (line deltas) are unchanged whether this is on or off and the at-rest
    /// render path is byte-identical.
    pub pixel_scroll: bool,
    /// Sensitivity multiplier for the continuous pixel-scroll lane. `1.0`
    /// (default) tracks finger travel exactly; higher/lower scroll faster/slower
    /// than the finger. Stored as `f32` to ride the shared numeric-setting model.
    /// Applies only to pixel-precise input, never the detented-wheel path.
    pub scroll_pixel_speed: f32,
    /// Animated scrollback glide between discrete wheel notches (SCROLL-GLIDE).
    /// On by default; a notch moves the integer viewport offset instantly and
    /// the rendered viewport eases toward it with a forward-chase follower that
    /// only moves in the scroll direction (so a notch stream cannot
    /// sawtooth). Only detented wheels use this; high-resolution/touchpad input
    /// uses `pixel_scroll`. At rest the render path is byte-identical.
    pub scroll_glide: bool,
    /// Colour-vision-deficiency palette adaptation mode (U4, Accessibility).
    /// `Off` by default — the off path publishes the authored palette unchanged
    /// and is pixel-identical to before. The deficiency modes daltonise the
    /// palette (16 ANSI + cursor/selection/search roles) so confusable colours
    /// separate, re-floored to stay readable.
    pub cvd_mode: CvdMode,
    /// Strength of the CVD adaptation in `0.0..=1.0` (U4). `1.0` (default) is
    /// the full correction; `0.0` is an exact passthrough. Inert while
    /// [`Settings::cvd_mode`] is `Off`.
    pub cvd_strength: f32,
    /// How the terminal reacts to BEL (`0x07`). Defaults to `Urgent` (window
    /// attention when unfocused, no flash). See [`BellMode`].
    pub bell: BellMode,
    /// Completion/OSC notification presentation. Defaults to bounded in-app
    /// state; native delivery must be explicitly selected.
    pub notifications: NotificationMode,
    /// What typing `exit` does when it would close a whole workspace: close the
    /// single workspace (default) or quit OdyTTY. See [`ShellExitCloses`].
    pub shell_exit_closes: ShellExitCloses,
    /// Whether `ODYTTY_THEME=system` was set: a single-key alias that enables
    /// [`Settings::follow_os_theme`] with default dark/light theme mappings
    /// (OS-THEME alias). `false` (the default) means the authored
    /// [`Settings::theme`] drives presentation as before. When `true`,
    /// [`Settings::follow_os_theme`] is forced on regardless of its parsed
    /// value, and the OS signal maps to [`crate::settings::DEFAULT_OS_THEME_DARK`]
    /// / [`crate::settings::DEFAULT_OS_THEME_LIGHT`] unless the user set an
    /// explicit `os_theme_dark`/`os_theme_light` override. This is a config
    /// alias only — it never extracts the desktop palette.
    pub theme_is_system: bool,
    /// Follow the OS dark/light appearance preference (OS-THEME). Off by
    /// default; while off the OS signal is ignored and the authored
    /// [`Settings::theme`] drives presentation byte-identically to before. When
    /// on, the compositor's color-scheme signal selects between
    /// [`Settings::os_theme_dark`] and [`Settings::os_theme_light`].
    pub follow_os_theme: bool,
    /// Theme name applied when the OS reports a dark color scheme (OS-THEME).
    /// `None` (the default) means a dark signal keeps the authored theme rather
    /// than switching. Resolved against the built-in theme library by name.
    pub os_theme_dark: Option<String>,
    /// Theme name applied when the OS reports a light color scheme (OS-THEME).
    /// `None` (the default) means a light signal keeps the authored theme.
    pub os_theme_light: Option<String>,
    /// Confirm before closing while a foreground job is running (CLOSE-CONFIRM).
    /// On by default; the dialog only appears when a program is actively running
    /// in the terminal, so the common idle-shell close path is unaffected.
    pub confirm_close: bool,
    /// Confirm multiline or control-bearing text before it reaches a child that
    /// has bracketed-paste mode disabled. On by default. The predicate is
    /// cross-platform and examines original text before newline normalization.
    pub warn_on_risky_paste: bool,
    /// Opt-in OpenSSH config host import for the connection manager.
    /// Off by default: OdyTTY's owned hosts list works without touching
    /// `~/.ssh/config`. When on, the data layer reads a caller-supplied
    /// OpenSSH config path read-only and name-only through bounded parsing.
    pub ssh_config_hosts: bool,
    /// Remote OSC 133 shell integration for SSH tabs. On by default: an SSH tab
    /// injects OdyTTY's bash prompt-mark bootstrap on the remote (nothing is
    /// persisted there) so a remote bash session behaves like a local one. Any
    /// failure or a non-bash remote shell degrades to a plain ssh session, and a
    /// per-host `Integration off` opts a single host out. Off globally makes
    /// every SSH tab's argv byte-identical to a plain ssh launch.
    pub remote_integration: bool,
    /// ControlMaster connection reuse for integrated SSH tabs. On by default: an
    /// integrated SSH tab adds ControlMaster/ControlPersist with an OdyTTY-owned
    /// ControlPath so a second tab to the same effective SSH endpoint
    /// multiplexes over the first with no new handshake. A per-host `Reuse off`
    /// opts a single host out. A Windows client emits no control options
    /// (OpenSSH there has no socket multiplexing), so reuse is a silent no-op on
    /// Windows.
    pub remote_reuse: bool,
    /// tmux persistence for integrated SSH tabs. Off by default: when on, an
    /// integrated SSH tab's bootstrap `exec`s `tmux new-session -A -s odytty` so
    /// a dropped-and-reconnected link reattaches the same remote session. The
    /// remote shell degrades to plain bash when the remote has no `tmux`. A
    /// per-host `Tmux on`/`Tmux off` overrides for a single host. Only meaningful
    /// with `remote_integration` on (the bootstrap is the injection point).
    pub remote_tmux: bool,
    /// How long a reuse master lingers after the last tab to a host closes
    /// (ODP-9 Tier 2). `Min10` (default) is the historical fixed 10-minute
    /// window (`ControlPersist=600`), so default behavior is byte-identical;
    /// `Off` tears the master down with its last connection. A per-host `Persist`
    /// line in `hosts.conf` overrides this. Unix-only: never emitted on a Windows
    /// client (no ControlMaster there). Only meaningful with `remote_reuse` on.
    pub remote_persist: RemotePersist,
    /// Image paste-through for remote integrated ssh tabs (F6-i7). `Ask`
    /// (default) prompts before uploading a pasted clipboard image to the remote
    /// host; `Off` disables it. Only engages on a remote integrated tab; a local
    /// or plain-ssh tab's paste path is unaffected.
    pub remote_image_paste: RemoteImagePaste,
    /// Opt-in per-session output recording for the scrubbable replay overlay
    /// (Phase 2). Off by default; while off the PTY pump records nothing and the
    /// plain path is byte-identical. When on, each session keeps a bounded
    /// in-memory ring of recent screen frames the replay overlay can scrub.
    /// Recording is local-only: frames never leave memory (no disk, no network).
    pub session_replay: bool,
    /// Opt-in interactive filesystem paths (Phase 7+). Off by default; while off
    /// the pointer path never scans terminal text for paths and the plain hover
    /// path is byte-identical. When on, hovering a path-looking span that
    /// resolves to a real filesystem entry shows the pointer (hand) cursor.
    /// Detection is local-only (nothing logged, persisted, or sent) and runs on
    /// the focused pane only (v1 bound, shared with OSC 8 hyperlink hover).
    pub interactive_paths: bool,
    /// When interactive paths are enabled, detect basename-like file tokens such
    /// as `carpet1.jpg` and resolve them against the pane cwd. On by default
    /// behind the global `interactive_paths` gate; resolution is still
    /// stat-gated, so non-existent barewords stay inert.
    pub interactive_paths_barewords: bool,
    /// UX-A (Phase 11): show the transient "Ctrl+click to open" discoverability
    /// hint after two plain mis-clicks on a resolved path within a short window.
    /// On by default behind the global `interactive_paths` gate; off silences
    /// only the hint (hand cursor, Ctrl+hover underline, and Ctrl+click open all
    /// still work).
    pub interactive_paths_click_hint: bool,
    /// UX-C (Phase 13): when interactive paths are enabled, Ctrl+clicking an
    /// image path opens the in-OdyTTY viewer by default. On by default behind
    /// the global `interactive_paths` gate; off restores the external opener
    /// behavior for images while keeping the right-click "Open in OdyTTY" item.
    pub interactive_paths_image_inline: bool,
    /// Optional editor override for opening `path:line:col` spans (Phase 8 / C3).
    /// Empty (the default) means "detect from `$EDITOR`/`$VISUAL` via the
    /// invocation matrix". A non-empty value pins the editor: either a known
    /// editor name (keys into the matrix) or an argv template with `{file}`,
    /// `{line}`, `{col}` placeholders. Always tokenized on whitespace and passed
    /// as an argv vector — never evaluated by a shell.
    pub interactive_paths_editor: String,
    pub native_autoclose: Option<Duration>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME,
            theme_config: None,
            visual: DEFAULT_VISUAL,
            font_path: None,
            explicit_font_path: None,
            font_family: Some(crate::text::BUNDLED_FONT_FAMILY.to_owned()),
            font_weight: String::new(),
            font_size_px: DEFAULT_FONT_SIZE_PX,
            text_gamma: DEFAULT_TEXT_GAMMA,
            stem_darken: DEFAULT_STEM_DARKEN,
            min_contrast: DEFAULT_MIN_CONTRAST,
            focus_dim: DEFAULT_FOCUS_DIM,
            inactive_pane_dim: DEFAULT_INACTIVE_PANE_DIM,
            render_quality: RenderQuality::default(),
            background_treatment: BackgroundTreatment::default(),
            background_image: Some(bundled_background_path()),
            background_blur_radius: 0,
            background_image_scrim: Some(DEFAULT_BACKGROUND_IMAGE_SCRIM),
            cell_bg_opacity: DEFAULT_CELL_BG_OPACITY,
            colored_bg_opacity: DEFAULT_COLORED_BG_OPACITY,
            selection_opacity: DEFAULT_SELECTION_OPACITY,
            text_brightness: DEFAULT_TEXT_BRIGHTNESS,
            window_padding_px: DEFAULT_WINDOW_PADDING_PX,
            bloom: DEFAULT_BLOOM,
            bloom_threshold: DEFAULT_BLOOM_THRESHOLD,
            bloom_intensity: DEFAULT_BLOOM_INTENSITY,
            bloom_radius: DEFAULT_BLOOM_RADIUS,
            retro: DEFAULT_RETRO,
            crt: DEFAULT_CRT,
            crt_scanline_intensity: DEFAULT_CRT_SCANLINE_INTENSITY,
            crt_scanline_period: DEFAULT_CRT_SCANLINE_PERIOD,
            crt_vignette_strength: DEFAULT_CRT_VIGNETTE_STRENGTH,
            crt_curvature: DEFAULT_CRT_CURVATURE,
            subpixel: SubpixelMode::Off,
            line_height: DEFAULT_LINE_HEIGHT,
            box_thickness: DEFAULT_BOX_THICKNESS,
            ligatures: DEFAULT_LIGATURES,
            ligature_ss01: DEFAULT_LIGATURE_SS01,
            ligature_ss02: DEFAULT_LIGATURE_SS02,
            kitty_named_transports: false,
            key_bindings: Vec::new(),
            pane_prefix: default_pane_prefix(),
            cursor_style: CursorStyle::Block,
            cursor_blink: CursorBlink::On,
            cursor_easing: DEFAULT_CURSOR_EASING,
            cursor_glow: DEFAULT_CURSOR_GLOW,
            cursor_glow_intensity: DEFAULT_CURSOR_GLOW_INTENSITY,
            cursor_trail: DEFAULT_CURSOR_TRAIL,
            cursor_trail_strength: CursorTrailStrength::default(),
            cursor_motion: DEFAULT_CURSOR_MOTION,
            reduced_motion: DEFAULT_REDUCED_MOTION,
            osc52_read: false,
            osc52_write: Osc52WritePolicy::default(),
            synthetic_styles: true,
            geometric_boxdraw: true,
            symbol_fallback: true,
            symbol_font: None,
            symbol_map: crate::text::SymbolMap::new(),
            themed_ui_roles: true,
            scroll_wheel_lines: DEFAULT_SCROLL_WHEEL_LINES,
            scrollback_lines: DEFAULT_SCROLLBACK_LINES,
            scroll_drag_speed: ScrollDragSpeed::default(),
            copy_on_select: DEFAULT_COPY_ON_SELECT,
            smart_ctrl_c: SmartCtrlC::default(),
            selection_drag_extend: DEFAULT_SELECTION_DRAG_EXTEND,
            scrollbar_drag: DEFAULT_SCROLLBAR_DRAG,
            wheel_zoom: DEFAULT_WHEEL_ZOOM,
            command_status_gutter: DEFAULT_COMMAND_STATUS_GUTTER,
            always_show_tab_bar: DEFAULT_ALWAYS_SHOW_TAB_BAR,
            tab_bar_placement: TabBarPlacement::default(),
            workspace_rail: WorkspaceRail::default(),
            tab_rail_width: TabRailWidth::default(),
            tab_bar_height: TabBarHeight::default(),
            tab_rail_max_width: DEFAULT_TAB_RAIL_MAX_WIDTH,
            tab_rail_gap: DEFAULT_TAB_RAIL_GAP,
            tab_rail_slot_rows: DEFAULT_TAB_RAIL_SLOT_ROWS,
            tab_panel_strength: DEFAULT_TAB_PANEL_STRENGTH,
            tab_seam: DEFAULT_TAB_SEAM,
            tab_rail_autohide: DEFAULT_TAB_RAIL_AUTOHIDE,
            tab_rail_reveal_px: DEFAULT_TAB_RAIL_REVEAL_PX,
            sh_click: DEFAULT_SH_CLICK,
            buttons: DEFAULT_BUTTONS,
            buttons_iterm_compat: DEFAULT_BUTTONS_ITERM_COMPAT,
            buttons_sticky: DEFAULT_BUTTONS_STICKY,
            shell_integration: DEFAULT_SHELL_INTEGRATION,
            shell_key_enhancement: DEFAULT_SHELL_KEY_ENHANCEMENT,
            restore_workspaces: DEFAULT_RESTORE_WORKSPACES,
            new_output_fade: DEFAULT_NEW_OUTPUT_FADE,
            new_output_fade_ms: DEFAULT_NEW_OUTPUT_FADE_MS,
            window_border: DEFAULT_WINDOW_BORDER,
            window_decorations: DEFAULT_WINDOW_DECORATIONS,
            window_transparency: DEFAULT_WINDOW_TRANSPARENCY,
            window_opacity: DEFAULT_WINDOW_OPACITY,
            pixel_scroll: DEFAULT_PIXEL_SCROLL,
            scroll_pixel_speed: DEFAULT_SCROLL_PIXEL_SPEED,
            scroll_glide: DEFAULT_SCROLL_GLIDE,
            cvd_mode: CvdMode::default(),
            cvd_strength: DEFAULT_CVD_STRENGTH,
            bell: BellMode::default(),
            notifications: NotificationMode::default(),
            shell_exit_closes: ShellExitCloses::default(),
            theme_is_system: false,
            follow_os_theme: DEFAULT_FOLLOW_OS_THEME,
            os_theme_dark: None,
            os_theme_light: None,
            confirm_close: DEFAULT_CONFIRM_CLOSE,
            warn_on_risky_paste: DEFAULT_WARN_ON_RISKY_PASTE,
            ssh_config_hosts: DEFAULT_SSH_CONFIG_HOSTS,
            remote_integration: DEFAULT_REMOTE_INTEGRATION,
            remote_reuse: DEFAULT_REMOTE_REUSE,
            remote_tmux: DEFAULT_REMOTE_TMUX,
            remote_persist: RemotePersist::default(),
            remote_image_paste: RemoteImagePaste::default(),
            session_replay: DEFAULT_SESSION_REPLAY,
            interactive_urls: DEFAULT_INTERACTIVE_URLS,
            interactive_paths: DEFAULT_INTERACTIVE_PATHS,
            interactive_paths_barewords: DEFAULT_INTERACTIVE_PATHS_BAREWORDS,
            interactive_paths_click_hint: DEFAULT_INTERACTIVE_PATHS_CLICK_HINT,
            interactive_paths_image_inline: DEFAULT_INTERACTIVE_PATHS_IMAGE_INLINE,
            interactive_paths_editor: String::new(),
            native_autoclose: None,
        }
    }
}

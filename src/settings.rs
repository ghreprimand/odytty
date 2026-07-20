// SPDX-License-Identifier: GPL-3.0-only
//! Runtime settings for the prototype.
//!
//! Settings are sourced from a small config file and environment variables, but
//! the rest of the app consumes this typed struct. That keeps runtime
//! configuration in one place without pushing `std::env` or file reads through
//! renderer and terminal code.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use std::path::Path;

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::{Theme, ThemeSpec, VisualEffect};

mod config;
mod consts;
mod descriptions;
mod fs_read;
mod info;
mod reload;
mod values;
mod writeback;

pub use consts::*;
pub use descriptions::*;
pub use info::{NumericSpec, SettingInfo, SettingKind};
pub use reload::{
    ConfigReloadPoller, SettingsReloadOutcome, SettingsReloader, apply_reloadable_values,
};
pub use writeback::{
    ConfigWritebackError, ConfigWritebackResult, ensure_config_file_exists,
    ensure_config_file_exists_at, write_settings_changes, write_settings_changes_to_path,
};

use self::values::*;
use config::{ConfigValues, env_to_config_key};
pub(crate) use consts::SETTING_ENV_KEYS;

/// Runtime flag mirroring [`Settings::synthetic_styles`], published process-wide
/// so the GPU renderer can read it without threading `Settings` through the
/// `NativeOptions` seam (whose construction literals live in a separate
/// module). Defaults to `true` (synthesis on); the native entry point
/// publishes the resolved setting at startup and the config-reload path
/// republishes it on change. This mirrors the existing process-global pattern
/// used for default cell colors ([`crate::text::set_default_colors`]).
static SYNTHETIC_STYLES_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Publish the synthetic-styles kill switch so the renderer's atlas-build path
/// can gate font synthesis. Called at startup and whenever the config reloads.
pub fn set_synthetic_styles_enabled(enabled: bool) {
    SYNTHETIC_STYLES_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Read the published synthetic-styles flag. `true` means synthesize missing
/// bold/italic faces from the regular outline; `false` forces the atlas mask off
/// so styled cells render as plain regular glyphs.
pub fn synthetic_styles_enabled() -> bool {
    SYNTHETIC_STYLES_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Runtime flag mirroring [`Settings::geometric_boxdraw`], published
/// process-wide so the GPU renderer can apply it to every rebuilt glyph atlas
/// without threading `Settings` through the native options seam. Defaults to
/// `false`, preserving the font-rasterized plain path unless explicitly
/// enabled.
static GEOMETRIC_BOXDRAW_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Publish the geometric box-drawing switch so the renderer's atlas-build path
/// can enable pixel geometry for box/block/Powerline glyphs. Called at startup
/// and whenever the config reloads.
pub fn set_geometric_boxdraw_enabled(enabled: bool) {
    GEOMETRIC_BOXDRAW_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Read the published geometric box-drawing flag. `false` is exact passthrough
/// to font glyph rasterization; `true` enables atlas-owned geometry for covered
/// codepoints.
pub fn geometric_boxdraw_enabled() -> bool {
    GEOMETRIC_BOXDRAW_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Process-wide contextual shaping switch. `false` is the exact scalar renderer
/// path and allocates no shaping cache entries.
static LIGATURES_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(DEFAULT_LIGATURES);

pub fn set_ligatures_enabled(enabled: bool) {
    LIGATURES_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn ligatures_enabled() -> bool {
    LIGATURES_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Runtime flag mirroring [`Settings::symbol_fallback`], published
/// process-wide so the GPU renderer can rebuild the glyph atlas when live
/// settings enable or disable the RV6 symbol / Nerd-font fallback. Defaults to
/// `true`, with a bundled symbols face intended to make PUA prompt icons work
/// out of the box; users can still disable it explicitly.
static SYMBOL_FALLBACK_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Optional explicit symbol / Nerd-font path mirroring [`Settings::symbol_font`].
/// `None` means the renderer falls back to its host font search.
static SYMBOL_FONT_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn set_symbol_fallback_enabled(enabled: bool) {
    SYMBOL_FALLBACK_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

pub fn symbol_fallback_enabled() -> bool {
    SYMBOL_FALLBACK_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_symbol_font_path(path: Option<PathBuf>) {
    let mut slot = SYMBOL_FONT_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = path;
}

pub fn symbol_font_path() -> Option<PathBuf> {
    SYMBOL_FONT_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Live SYMMAP override map (`Settings::symbol_map`), published process-wide so
/// the GPU renderer can resolve override font families and rebuild the glyph
/// atlas when the map changes. The default is empty (the identity / off path).
static SYMBOL_MAP: std::sync::Mutex<Option<crate::text::SymbolMap>> = std::sync::Mutex::new(None);

pub fn set_symbol_map(map: crate::text::SymbolMap) {
    let mut slot = SYMBOL_MAP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(map);
}

pub fn symbol_map() -> crate::text::SymbolMap {
    SYMBOL_MAP
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_default()
}

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

    fn parse(raw: &str) -> Option<Self> {
        match normalize_name(raw).as_str() {
            "on" | "true" | "yes" | "blink" | "blinking" => Some(Self::On),
            "off" | "false" | "no" | "steady" | "solid" => Some(Self::Off),
            "auto" | "default" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// Terminal-local actions that can be rebound without changing PTY input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindableAction {
    Search,
    SettingsPanel,
    ThemePicker,
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
    /// Jump the viewport to the previous shell prompt (OSC 133 boundary).
    JumpPromptPrev,
    /// Jump the viewport to the next shell prompt (OSC 133 boundary).
    JumpPromptNext,
    /// Enter keyboard scrollback selection ("copy") mode.
    CopyMode,
    /// Activate keyboard pattern-select hints (URLs / paths / SHAs).
    Hints,
    /// Clear the current shell input line (IN1). Writes a readline-style
    /// kill-whole-line sequence to the PTY; inert when unbound.
    ClearInput,
    /// Open the in-window command palette. Bound to `Ctrl+Shift+P` by default
    /// (the industry-standard palette chord; v0.3.1 discoverability). This
    /// reclaims `Ctrl+Shift+P` from prompt-jump, whose primary bindings are the
    /// `Ctrl+Shift+Up/Down` arrow chords (unchanged).
    CommandPalette,
    /// Open the output-replay overlay (Phase 2). Bound to `Ctrl+Shift+R` by
    /// default (v0.3.1 discoverability). Replay is presentation-only.
    SessionReplay,
    /// Open the connection-manager overlay (Phase 4). Bound to `Ctrl+Shift+S`
    /// by default (v0.3.1 discoverability). The overlay is presentation-only; it
    /// lists saved hosts and emits a connect request on accept.
    ConnectionManager,
    /// Open the theme builder overlay directly (v0.3.1 discoverability). Bound to
    /// `Ctrl+Shift+B` by default. Previously reachable only via the settings
    /// Themes section; this gives it a first-class action so a chord and a menu
    /// entry can open it without navigating the panel.
    ThemeBuilder,
    /// Open the in-window session-attach summon overlay (Phase 5 / B2). Bound to
    /// `Ctrl+Shift+A` by default. Lists live detached sessions; accepting a row
    /// attaches that session into a new tab. Presentation-only — the overlay
    /// never attaches anything itself, it emits an attach request on accept.
    SessionAttach,
    NewTab,
    /// Launch another top-level OdyTTY window (a fresh process instance). Bound
    /// to `Ctrl+Shift+N` by default (gnome-terminal / kitty convention). The new
    /// window inherits the parent environment; it is a separate process, not a
    /// tab. Spawn failure is logged and dropped — never a crash.
    NewWindow,
    NextTab,
    PrevTab,
    CloseTab,
    /// Open a new local tab in the active pane's working directory — a fresh
    /// shell where the current one is, not a process fork (scrollback and the
    /// running program are not duplicated). Reachable from the tab context menu
    /// and bindable; no default chord.
    DuplicateTab,
    // --- Workspace actions. A workspace groups a set of tabs; these are global
    //     chords (never on the multiplexer prefix), so `is_pane_action` stays
    //     false for them. Creation/rename/close are unbound by default (the rail
    //     `+` slot, context menu, and palette cover them); cycling has default
    //     chords a TUI cannot receive.
    /// Create a fresh workspace (one single-pane tab) and switch to it. Unbound
    /// by default.
    NewWorkspace,
    /// Duplicate the active workspace: open a fresh workspace whose first shell
    /// spawns in the active pane's working directory -- a fresh shell where the
    /// current one is, not a process fork (scrollback and running programs are
    /// not duplicated). Reachable from the workspace context menu and bindable;
    /// default chord `Ctrl+Shift+Alt+D` (the tab->workspace Alt escalation of
    /// Duplicate Tab's `Ctrl+Shift+D`).
    DuplicateWorkspace,
    /// Close the entire active workspace — every tab and pane. Closing the last
    /// workspace exits the app (last-tab-of-last-workspace exit semantics).
    /// Unbound by default.
    CloseWorkspace,
    /// Rename the active workspace in place. Unbound by default.
    RenameWorkspace,
    /// Switch to the next workspace in rail order (wrapping). Bound to
    /// `Ctrl+Shift+PageDown` by default (mnemonically above `Ctrl+PageDown` tab
    /// cycling).
    NextWorkspace,
    /// Switch to the previous workspace in rail order (wrapping). Bound to
    /// `Ctrl+Shift+PageUp` by default.
    PrevWorkspace,
    /// Open the command palette focused on workspace navigation (v1 has no
    /// dedicated picker overlay; the palette lists every workspace). Unbound by
    /// default.
    WorkspacePicker,
    // --- Pane-management actions (§7). These resolve only on the multiplexer
    //     prefix (default `Ctrl-b`), never as bare global chords, so they never
    //     perturb the no-prefix input path. See `BindableAction::is_pane_action`.
    /// Split the focused pane side-by-side (tmux `Ctrl-b %`).
    SplitColumns,
    /// Split the focused pane stacked (tmux `Ctrl-b "`).
    SplitRows,
    /// Move focus to the pane left of the focused one (tmux `Ctrl-b ←`).
    FocusPaneLeft,
    /// Move focus to the pane right of the focused one (tmux `Ctrl-b →`).
    FocusPaneRight,
    /// Move focus to the pane above the focused one (tmux `Ctrl-b ↑`).
    FocusPaneUp,
    /// Move focus to the pane below the focused one (tmux `Ctrl-b ↓`).
    FocusPaneDown,
    /// Cycle focus to the next pane in tree order (tmux `Ctrl-b o`).
    FocusPaneNext,
    /// Close the focused pane (tmux `Ctrl-b x`).
    ClosePane,
    /// Zoom / toggle-fullscreen the focused pane (tmux `Ctrl-b z`).
    ZoomPane,
    /// Reset all split ratios in the active tab to even (tmux `Ctrl-b =`).
    EqualizePanes,
}

impl BindableAction {
    /// Every `BindableAction` variant, in the canonical UI order (core actions,
    /// then overlay actions, then tab actions, then pane actions). This is the
    /// single source of truth the in-app keybinding editor iterates and the
    /// coverage guard checks against, so a new variant cannot be silently
    /// omitted from the editor. Keep it exhaustive — the
    /// `all_bindable_actions_is_exhaustive` test pins it to the enum's size.
    pub const ALL: [Self; 40] = [
        // Core non-tab actions.
        Self::Search,
        Self::SettingsPanel,
        Self::ThemePicker,
        Self::Copy,
        Self::Paste,
        Self::ScrollPageUp,
        Self::ScrollPageDown,
        Self::JumpPromptPrev,
        Self::JumpPromptNext,
        Self::CopyMode,
        Self::Hints,
        Self::ClearInput,
        // Overlay actions (v0.3.1 discoverability family).
        Self::CommandPalette,
        Self::ConnectionManager,
        Self::SessionReplay,
        Self::ThemeBuilder,
        Self::SessionAttach,
        // Tab actions.
        Self::NewTab,
        Self::NewWindow,
        Self::NextTab,
        Self::PrevTab,
        Self::CloseTab,
        Self::DuplicateTab,
        // Workspace actions.
        Self::NewWorkspace,
        Self::DuplicateWorkspace,
        Self::CloseWorkspace,
        Self::RenameWorkspace,
        Self::NextWorkspace,
        Self::PrevWorkspace,
        Self::WorkspacePicker,
        // Pane-management actions (§7).
        Self::SplitColumns,
        Self::SplitRows,
        Self::FocusPaneLeft,
        Self::FocusPaneRight,
        Self::FocusPaneUp,
        Self::FocusPaneDown,
        Self::FocusPaneNext,
        Self::ClosePane,
        Self::ZoomPane,
        Self::EqualizePanes,
    ];

    fn parse(raw: &str) -> Option<Self> {
        match normalize_name(raw).as_str() {
            "search" | "searchtoggle" | "togglesearch" => Some(Self::Search),
            "settings" | "settingspanel" | "togglesettings" | "preferences" | "prefs" => {
                Some(Self::SettingsPanel)
            }
            "theme" | "themes" | "themepicker" | "picktheme" | "choosetheme" => {
                Some(Self::ThemePicker)
            }
            "copy" => Some(Self::Copy),
            "paste" => Some(Self::Paste),
            "scrollup" | "pageup" | "scrollpageup" | "scrollbackpageup" => Some(Self::ScrollPageUp),
            "scrolldown" | "pagedown" | "scrollpagedown" | "scrollbackpagedown" => {
                Some(Self::ScrollPageDown)
            }
            "jumppromptprev" | "promptprev" | "prevprompt" | "jumpprevprompt" => {
                Some(Self::JumpPromptPrev)
            }
            "jumppromptnext" | "promptnext" | "nextprompt" | "jumpnextprompt" => {
                Some(Self::JumpPromptNext)
            }
            "copymode" | "selectmode" => Some(Self::CopyMode),
            "hints" | "hint" | "quickselect" | "patternselect" => Some(Self::Hints),
            "clearinput" | "clearline" | "killline" | "clear" => Some(Self::ClearInput),
            "commandpalette" | "palette" | "cmdpalette" | "fuzzypalette" => {
                Some(Self::CommandPalette)
            }
            "sessionreplay" | "replay" | "outputreplay" | "replayoverlay" => {
                Some(Self::SessionReplay)
            }
            "connectionmanager" | "connections" | "connect" | "sshmanager" | "hosts" => {
                Some(Self::ConnectionManager)
            }
            "themebuilder" | "buildtheme" | "newtheme" | "themeeditor" => Some(Self::ThemeBuilder),
            "sessionattach" | "attach" | "attachsession" | "sessions" | "sessionpicker" => {
                Some(Self::SessionAttach)
            }
            "newtab" | "tabnew" => Some(Self::NewTab),
            "newwindow" | "windownew" => Some(Self::NewWindow),
            "nexttab" | "tabnext" => Some(Self::NextTab),
            "prevtab" | "previoustab" | "tabprev" => Some(Self::PrevTab),
            "closetab" | "tabclose" => Some(Self::CloseTab),
            "duplicatetab" | "tabduplicate" | "duplicate" => Some(Self::DuplicateTab),
            "newworkspace" | "workspacenew" => Some(Self::NewWorkspace),
            "duplicateworkspace" | "workspaceduplicate" => Some(Self::DuplicateWorkspace),
            "closeworkspace" | "workspaceclose" => Some(Self::CloseWorkspace),
            "renameworkspace" | "workspacerename" => Some(Self::RenameWorkspace),
            "nextworkspace" | "workspacenext" => Some(Self::NextWorkspace),
            "prevworkspace" | "previousworkspace" | "workspaceprev" => Some(Self::PrevWorkspace),
            "workspacepicker" | "workspaceswitcher" | "workspaces" | "pickworkspace" => {
                Some(Self::WorkspacePicker)
            }
            "splitcolumns" | "splitsidebyside" | "splitright" => Some(Self::SplitColumns),
            "splitrows" | "splitstacked" | "splitdown" => Some(Self::SplitRows),
            "focuspaneleft" | "paneleft" => Some(Self::FocusPaneLeft),
            "focuspaneright" | "paneright" => Some(Self::FocusPaneRight),
            "focuspaneup" | "paneup" => Some(Self::FocusPaneUp),
            "focuspanedown" | "panedown" => Some(Self::FocusPaneDown),
            "focuspanenext" | "panenext" | "nextpane" => Some(Self::FocusPaneNext),
            "closepane" | "paneclose" => Some(Self::ClosePane),
            "zoompane" | "panezoom" | "togglezoom" => Some(Self::ZoomPane),
            "equalizepanes" | "equalize" | "panesequalize" => Some(Self::EqualizePanes),
            _ => None,
        }
    }

    /// Whether this action is a pane-management action that resolves only on the
    /// multiplexer prefix (§7), never as a bare global chord. The prefix engine
    /// owns these; the flat global binding table excludes them, so they cannot
    /// perturb the no-prefix input path.
    pub fn is_pane_action(self) -> bool {
        matches!(
            self,
            Self::SplitColumns
                | Self::SplitRows
                | Self::FocusPaneLeft
                | Self::FocusPaneRight
                | Self::FocusPaneUp
                | Self::FocusPaneDown
                | Self::FocusPaneNext
                | Self::ClosePane
                | Self::ZoomPane
                | Self::EqualizePanes
        )
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
    base_values: BTreeMap<&'static str, String>,
    values: BTreeMap<&'static str, String>,
    settings: Settings,
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
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

    fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "none" | "disabled" => Some(Self::Off),
            "visual" | "flash" => Some(Self::Visual),
            "urgent" | "attention" => Some(Self::Urgent),
            "all" | "both" => Some(Self::All),
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

    fn parse(value: &str) -> Option<Self> {
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
    /// ASCII programming ligatures via contextual OpenType `calt`. Enabled by
    /// default; logical cells, cursor coordinates, copy, and selection remain
    /// unchanged.
    pub ligatures: bool,
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
    /// daltonises with U4 for free). Off by default — the off path draws nothing
    /// and is pixel-identical to today.
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
    /// ControlPath so a second tab to the same host multiplexes over the first
    /// with no new handshake. A per-host `Reuse off` opts a single host out. A
    /// Windows client emits no control options (OpenSSH there has no socket
    /// multiplexing), so reuse is a silent no-op on Windows.
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
            shell_exit_closes: ShellExitCloses::default(),
            theme_is_system: false,
            follow_os_theme: DEFAULT_FOLLOW_OS_THEME,
            os_theme_dark: None,
            os_theme_light: None,
            confirm_close: DEFAULT_CONFIRM_CLOSE,
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

impl Settings {
    pub fn plain_render_quality(&self) -> bool {
        self.render_quality.is_plain()
    }

    pub fn effective_stem_darken(&self) -> f32 {
        if self.plain_render_quality() {
            0.0
        } else {
            self.stem_darken
        }
    }

    pub fn effective_min_contrast(&self) -> f32 {
        if self.plain_render_quality() {
            1.0
        } else {
            self.min_contrast
        }
    }

    pub fn effective_focus_dim(&self) -> f32 {
        if self.plain_render_quality() {
            0.0
        } else {
            self.focus_dim
        }
    }

    /// Inactive-pane dimming amount, forced to `0.0` on the plain renderer
    /// profile so the fast path stays byte-identical even when the knob is set.
    pub fn effective_inactive_pane_dim(&self) -> f32 {
        if self.plain_render_quality() {
            0.0
        } else {
            self.inactive_pane_dim
        }
    }

    /// The active background treatment (ID3/U5), forced `Off` on the plain
    /// renderer profile so the fast path stays pixel-identical even when the
    /// knob is set.
    pub fn effective_background_treatment(&self) -> BackgroundTreatment {
        if self.plain_render_quality() {
            BackgroundTreatment::Off
        } else {
            self.background_treatment
        }
    }

    pub fn effective_bloom_enabled(&self) -> bool {
        !self.plain_render_quality() && (self.bloom || self.retro)
    }

    pub fn effective_bloom_threshold(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_BLOOM_THRESHOLD
        } else {
            self.bloom_threshold
        }
    }

    pub fn effective_bloom_intensity(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_BLOOM_INTENSITY
        } else {
            self.bloom_intensity
        }
    }

    pub fn effective_bloom_radius(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_BLOOM_RADIUS
        } else {
            self.bloom_radius
        }
    }

    pub fn effective_crt_enabled(&self) -> bool {
        !self.plain_render_quality() && (self.crt || self.retro)
    }

    pub fn effective_crt_scanline_intensity(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_CRT_SCANLINE_INTENSITY
        } else {
            self.crt_scanline_intensity
        }
    }

    pub fn effective_crt_vignette_strength(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_CRT_VIGNETTE_STRENGTH
        } else {
            self.crt_vignette_strength
        }
    }
    pub fn effective_crt_curvature(&self) -> f32 {
        // Curvature is a configuration/environment knob only, flat by default,
        // with no settings-panel control. The retro preset raises the other CRT
        // treatments but deliberately does not force curvature, so it falls
        // through to the configured knob exactly like the plain CRT profile. The
        // direct render path still zeroes it.
        if self.plain_render_quality() {
            0.0
        } else {
            self.crt_curvature
        }
    }

    /// Rows advanced per mouse-wheel notch (MOUSE-WHEEL-SPEED), as a `usize >= 1`.
    /// Rounds the stored `f32` (kept in the shared numeric-setting model) and
    /// floors at 1 so a wheel notch always moves at least one row. The default
    /// `3.0` returns `3`, byte-identical to the historical fixed step.
    pub fn scroll_wheel_step(&self) -> usize {
        (self.scroll_wheel_lines.round() as i64).max(1) as usize
    }

    /// Resolve the vertical rail band width in cells (F4-P4).
    ///
    /// `auto_want_cols` is the width the `Auto` mode wants — the longest tab
    /// title plus the widget's label chrome, measured by the integration layer
    /// (which owns the widget geometry). In `Auto` mode the result is that value
    /// clamped to `[MIN_TAB_RAIL_WIDTH, tab_rail_max_width]`; in `Manual` mode
    /// the stored width is clamped to the absolute widget bounds
    /// `[MIN_TAB_RAIL_WIDTH, MAX_TAB_RAIL_WIDTH]` and `auto_want_cols` is
    /// ignored. Kept pure (no session/widget deref) so the resolution is unit-
    /// tested at the settings seam.
    pub fn rail_width_cols(&self, auto_want_cols: usize) -> usize {
        let min = MIN_TAB_RAIL_WIDTH as usize;
        match self.tab_rail_width {
            TabRailWidth::Manual(cols) => (cols as usize).clamp(min, MAX_TAB_RAIL_WIDTH as usize),
            TabRailWidth::Auto => auto_want_cols.clamp(min, self.rail_max_width_cols()),
        }
    }

    /// The `auto`-mode upper clamp in cells (F4-P4), rounded from the stored
    /// `f32` and clamped to `[MIN_TAB_RAIL_MAX_WIDTH, MAX_TAB_RAIL_MAX_WIDTH]`.
    pub fn rail_max_width_cols(&self) -> usize {
        self.tab_rail_max_width
            .round()
            .clamp(MIN_TAB_RAIL_MAX_WIDTH, MAX_TAB_RAIL_MAX_WIDTH) as usize
    }

    /// Inter-slot gap in rows for the vertical rail (F4-P1), rounded and clamped
    /// to `[MIN_TAB_RAIL_GAP, MAX_TAB_RAIL_GAP]`.
    pub fn rail_slot_gap_rows(&self) -> usize {
        self.tab_rail_gap
            .round()
            .clamp(MIN_TAB_RAIL_GAP, MAX_TAB_RAIL_GAP) as usize
    }

    /// Rail slot height in rows (F4-P1), rounded and clamped to `{1, 2}`.
    pub fn rail_slot_rows(&self) -> usize {
        self.tab_rail_slot_rows
            .round()
            .clamp(MIN_TAB_RAIL_SLOT_ROWS, MAX_TAB_RAIL_SLOT_ROWS) as usize
    }

    /// Scrollback retention cap in logical lines for the core (`0` = unbounded).
    /// Rounds and floors the stored `f32`; a negative or non-finite value (which
    /// the parser already rejects) collapses to `0`.
    pub fn scrollback_limit(&self) -> usize {
        let rounded = self.scrollback_lines.round();
        if rounded.is_finite() && rounded > 0.0 {
            rounded as usize
        } else {
            0
        }
    }

    /// Upper bound on rows advanced per drag-edge autoscroll tick
    /// (MOUSE-AUTOSCROLL-VEL). `Ramp` allows up to [`MAX_AUTOSCROLL_ROWS`] so the
    /// step accelerates with overshoot past the band; `Legacy` returns `1`, which
    /// pins the delta helper to exactly ±1/0 — byte-identical to the pre-feature
    /// fixed one-row-per-tick autoscroll.
    pub fn autoscroll_max_rows(&self) -> usize {
        match self.scroll_drag_speed {
            ScrollDragSpeed::Ramp => MAX_AUTOSCROLL_ROWS,
            ScrollDragSpeed::Legacy => 1,
        }
    }

    /// Load settings from the config file, then overlay the current process
    /// environment. Environment variables always win.
    pub fn from_env() -> Self {
        Self::from_env_and_optional_config(config_file_path())
    }

    fn from_edit_values(values: &BTreeMap<&'static str, String>) -> Result<Self, SettingEditError> {
        let mut warnings = Vec::new();
        // On the overlay-edit path a font-family change that does not resolve is
        // the most actionable failure, so capture the precise reason (not found
        // vs not monospace) and surface it ahead of the generic fallback warning
        // `from_source` also emits for the same condition. The success arm stays
        // byte-identical: `try_resolve_font_family(..).Ok` returns the same
        // `regular` path `resolve_font_family` would have.
        let mut font_family_error: Option<SettingEditError> = None;
        let settings = Self::from_source(
            |key| values.get(key).map(OsString::from),
            |message| warnings.push(message.to_owned()),
            |family| match crate::text::try_resolve_font_family(
                family,
                &crate::text::font_search_dirs(),
            ) {
                Ok(matched) => Some(matched.regular),
                Err(reason) => {
                    font_family_error.get_or_insert_with(|| SettingEditError {
                        key: "font_family",
                        message: font_family_error_message(family, reason),
                    });
                    None
                }
            },
            |value| resolve_theme_file(value, theme_dir_path().as_deref()),
        );
        if let Some(error) = font_family_error {
            return Err(error);
        }
        // C4: THEME resolution/parse warnings must not block the edit. A theme
        // loaded from a file (or one that resolves with a tolerable warning like
        // an unknown key) would otherwise make every OTHER key read-only, since
        // each edit re-parses the whole value map including the theme. These are
        // treated as tolerant here, matching startup/reload semantics (which
        // only print them). All other warnings — an invalid numeric value, an
        // out-of-range knob — stay hard edit errors so the panel still rejects
        // bad input with a precise message. Theme warnings are identified by the
        // `theme <name>: ...` per-file prefix or the THEME_ENV token in the
        // fallback message.
        let promotable = warnings
            .into_iter()
            .find(|message| !(message.starts_with("theme ") || message.contains(THEME_ENV)));
        if let Some(message) = promotable {
            return Err(SettingEditError { key: "", message });
        }
        Ok(settings)
    }

    fn from_env_and_optional_config(config_path: Option<PathBuf>) -> Self {
        let mut warnings = Vec::new();
        let mut suppressed = 0usize;
        let config = config_path
            .as_deref()
            .and_then(|path| {
                let mut warn = fs_read::bounded_warn(&mut warnings, &mut suppressed);
                match ConfigValues::read(path, &mut warn) {
                    Ok(values) => Some(values),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        warn(format!(
                            "could not read config file {}: {error}",
                            path.display()
                        ));
                        None
                    }
                }
            })
            .unwrap_or_default();
        fs_read::note_suppressed(&mut warnings, suppressed);

        for warning in warnings {
            eprintln!("odytty: {warning}");
        }

        Self::from_source(
            |key| std::env::var_os(key).or_else(|| config.get(key).cloned()),
            |message| {
                eprintln!("odytty: {message}");
            },
            |family| {
                crate::text::resolve_font_family(family, &crate::text::font_search_dirs())
                    .map(|m| m.regular)
            },
            |value| resolve_theme_file(value, theme_dir_path().as_deref()),
        )
    }

    fn from_env_snapshot_and_config(
        env_values: &HashMap<&'static str, OsString>,
        config: &ConfigValues,
        mut warn: impl FnMut(String),
    ) -> Self {
        Self::from_source(
            |key| {
                env_values
                    .get(key)
                    .cloned()
                    .or_else(|| config.get(key).cloned())
            },
            |message| warn(message.to_owned()),
            |family| {
                crate::text::resolve_font_family(family, &crate::text::font_search_dirs())
                    .map(|m| m.regular)
            },
            |value| resolve_theme_file(value, theme_dir_path().as_deref()),
        )
    }

    fn from_source(
        mut get: impl FnMut(&str) -> Option<OsString>,
        mut warn: impl FnMut(&str),
        mut resolve_family: impl FnMut(&str) -> Option<PathBuf>,
        mut read_theme: impl FnMut(&str) -> Option<String>,
    ) -> Self {
        // ODYTTY_THEME resolution: a built-in name resolves to its const; any
        // other value is treated as a user theme (a path, or a name found in
        // the user theme dir) loaded via `read_theme` and parsed through the
        // shared `ThemeSpec` path. A missing/garbage value falls back to the
        // configured default with a warning — startup never fails from a bad
        // theme setting.
        // `ODYTTY_THEME=system` is a single-key alias (OS-THEME alias) that
        // enables OS dark/light following with default mappings. It is NOT a
        // built-in theme: the authored `theme` falls back to the configured
        // default, and `theme_is_system` carries the alias intent so
        // [`Settings::resolve_active_theme`] and writeback can honor it. The
        // individual `os_theme_dark`/`os_theme_light` overrides remain
        // available for custom mappings.
        // Workspace-rail geometry accepts two env/config families for each field:
        // the canonical `WORKSPACE_RAIL_*` name (the rail shows workspaces) and
        // the legacy `TAB_RAIL_*` twin, both mapping to the same Settings field.
        // Precedence is deterministic: the canonical `WORKSPACE_RAIL_*` value
        // wins when both are set. The two `get` calls run sequentially (not a
        // nested closure) so the single `FnMut` source is borrowed once at a
        // time. Same on all three platforms; settings are platform-agnostic.
        macro_rules! rail_get {
            ($canonical:expr, $legacy:expr) => {
                match get($canonical) {
                    Some(value) => Some(value),
                    None => get($legacy),
                }
            };
        }
        let mut theme_is_system = false;
        // The raw `theme` config string, preserved verbatim ONLY for a
        // file-loaded theme so the writeback/display round-trip re-reads the
        // file instead of the non-resolving `"custom"` placeholder the runtime
        // `Theme` carries. Built-ins round-trip fine via `Theme::name`, so this
        // stays `None` for them (and for the unset/system/fallback cases).
        let mut theme_config: Option<String> = None;
        let theme = match get(THEME_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            None => DEFAULT_THEME,
            Some(value) => {
                if value.eq_ignore_ascii_case(crate::settings::SYSTEM_THEME_NAME) {
                    theme_is_system = true;
                    DEFAULT_THEME
                } else if let Some(builtin) = Theme::from_name(&value) {
                    builtin
                } else if let Some(contents) = read_theme(&value) {
                    let spec = ThemeSpec::parse(&contents, |message| {
                        warn(&format!("theme {value:?}: {message}"))
                    });
                    // Preserve the exact config string (path or user-theme name)
                    // so `to_edit_values`/`setting_info` never persist the
                    // placeholder `"custom"` name the projected theme carries.
                    theme_config = Some(value);
                    spec.to_theme()
                } else {
                    warn(&format!(
                        "{THEME_ENV}={value:?} is not a built-in theme or a readable theme file; using the default theme"
                    ));
                    DEFAULT_THEME
                }
            }
        };
        let visual = get(VISUAL_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| VisualEffect::from_name_or_default(&value))
            .unwrap_or(DEFAULT_VISUAL);
        // Direct path knob (ODYTTY_FONT) takes precedence over family lookup so
        // an explicit file always wins. ODYTTY_FONT_FAMILY is resolved to a
        // validated monospace path only when no direct path is given; resolution
        // failure falls back to the embedded default (font_path = None) with
        // one warning, so a bad family value never aborts startup.
        let direct_path = get(FONT_ENV).map(PathBuf::from);
        // The raw explicit `font` key, kept verbatim for display/writeback. This
        // is distinct from the effective `font_path` below, which may instead be
        // the regular face resolved from `font_family` (RC4): the advanced `font`
        // row must reflect only what the user explicitly set.
        let explicit_font_path = direct_path.clone();
        let font_family = get(FONT_FAMILY_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| Some(crate::text::BUNDLED_FONT_FAMILY.to_owned()));
        let font_path = if direct_path.is_some() {
            direct_path
        } else if let Some(family) = font_family.as_deref() {
            if crate::text::is_bundled_font_family(family) {
                None
            } else {
                match resolve_family(family) {
                    Some(path) => Some(path),
                    None => {
                        warn(&format!(
                            "{FONT_FAMILY_ENV}={family:?} did not resolve to a monospace font; using the default font"
                        ));
                        None
                    }
                }
            }
        } else {
            None
        };
        let font_weight = get(FONT_WEIGHT_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| parse_font_weight_variant(&value))
            .unwrap_or_default();
        let font_size_px = parse_font_size(get(FONT_SIZE_ENV).as_deref(), &mut warn);
        let text_gamma = parse_text_gamma(get(TEXT_GAMMA_ENV).as_deref(), &mut warn);
        let stem_darken = parse_stem_darken(get(STEM_DARKEN_ENV).as_deref(), &mut warn);
        let min_contrast = parse_min_contrast(get(MIN_CONTRAST_ENV).as_deref(), &mut warn);
        let focus_dim = parse_focus_dim(get(FOCUS_DIM_ENV).as_deref(), &mut warn);
        let inactive_pane_dim =
            parse_inactive_pane_dim(get(INACTIVE_PANE_DIM_ENV).as_deref(), &mut warn);
        let render_quality = parse_render_quality(get(RENDER_QUALITY_ENV).as_deref(), &mut warn);
        let background_treatment =
            parse_background_treatment(get(BACKGROUND_TREATMENT_ENV).as_deref(), &mut warn);
        // `background_image` resolution (v0.6.0):
        //   * unset            -> the bundled default (compiled-in sentinel),
        //   * `none`/`off`/""   -> explicitly no image (the documented off-switch),
        //   * `default`/`bundled` -> the bundled default,
        //   * anything else     -> a real on-disk path.
        let background_image = match get(BACKGROUND_IMAGE_ENV) {
            None => Some(bundled_background_path()),
            Some(raw) => {
                let lossy = raw.to_string_lossy();
                let trimmed = lossy.trim();
                match trimmed.to_ascii_lowercase().as_str() {
                    "" | "none" | "off" | "false" => None,
                    "default" | "bundled" | BUNDLED_BACKGROUND_SENTINEL => {
                        Some(bundled_background_path())
                    }
                    _ => Some(PathBuf::from(trimmed)),
                }
            }
        };
        let background_blur_radius =
            parse_background_blur_radius(get(BACKGROUND_BLUR_RADIUS_ENV).as_deref(), &mut warn);
        // Unset -> the shipped default scrim (paired with the bundled default
        // background); an explicit value (including `auto` -> None) overrides.
        let background_image_scrim = match get(BACKGROUND_IMAGE_SCRIM_ENV) {
            None => Some(DEFAULT_BACKGROUND_IMAGE_SCRIM),
            some => parse_background_image_scrim(some.as_deref(), &mut warn),
        };
        let cell_bg_opacity = parse_cell_bg_opacity(get(CELL_BG_OPACITY_ENV).as_deref(), &mut warn);
        let colored_bg_opacity =
            parse_colored_bg_opacity(get(COLORED_BG_OPACITY_ENV).as_deref(), &mut warn);
        let selection_opacity =
            parse_selection_opacity(get(SELECTION_OPACITY_ENV).as_deref(), &mut warn);
        let text_brightness = parse_text_brightness(get(TEXT_BRIGHTNESS_ENV).as_deref(), &mut warn);
        let window_padding_px = parse_window_padding(get(WINDOW_PADDING_ENV).as_deref(), &mut warn);
        let bloom = parse_bool_setting(
            get(BLOOM_ENV).as_deref(),
            BLOOM_ENV,
            DEFAULT_BLOOM,
            &mut warn,
        );
        let default_bloom_threshold = DEFAULT_BLOOM_THRESHOLD;
        let bloom_threshold = parse_bloom_threshold(
            get(BLOOM_THRESHOLD_ENV).as_deref(),
            default_bloom_threshold,
            &mut warn,
        );
        let bloom_intensity = parse_bloom_intensity(get(BLOOM_INTENSITY_ENV).as_deref(), &mut warn);
        let bloom_radius = parse_bloom_radius(get(BLOOM_RADIUS_ENV).as_deref(), &mut warn);
        let retro = parse_bool_setting(
            get(RETRO_ENV).as_deref(),
            RETRO_ENV,
            DEFAULT_RETRO,
            &mut warn,
        );
        // UX5: the legacy `visual=ambient`/`scanlines` scanline effect is folded
        // into the unified CRT post-process. An ambient visual aliases to
        // `crt=on` ONLY when no explicit CRT setting is present — an explicit
        // `crt=`/`ODYTTY_CRT` always wins over the alias (the alias merely fills
        // the unset case), so a config can never stack two scanline passes.
        let crt_explicit = get(CRT_ENV);
        let crt = if crt_explicit.is_some() {
            parse_bool_setting(crt_explicit.as_deref(), CRT_ENV, DEFAULT_CRT, &mut warn)
        } else {
            DEFAULT_CRT || visual == VisualEffect::Ambient
        };
        let crt_scanline_intensity =
            parse_crt_scanline_intensity(get(CRT_SCANLINE_INTENSITY_ENV).as_deref(), &mut warn);
        let crt_scanline_period =
            parse_crt_scanline_period(get(CRT_SCANLINE_PERIOD_ENV).as_deref(), &mut warn);
        let crt_vignette_strength =
            parse_crt_vignette_strength(get(CRT_VIGNETTE_STRENGTH_ENV).as_deref(), &mut warn);
        let crt_curvature = parse_crt_curvature(get(CRT_CURVATURE_ENV).as_deref(), &mut warn);
        let subpixel = parse_subpixel(get(SUBPIXEL_ENV).as_deref(), &mut warn);
        let line_height = parse_line_height(get(LINE_HEIGHT_ENV).as_deref(), &mut warn);
        let box_thickness = parse_box_thickness(get(BOX_THICKNESS_ENV).as_deref(), &mut warn);
        let ligatures = parse_bool_setting(
            get(LIGATURES_ENV).as_deref(),
            LIGATURES_ENV,
            DEFAULT_LIGATURES,
            &mut warn,
        );
        let kitty_named_transports = parse_bool_setting(
            get(KITTY_NAMED_TRANSPORTS_ENV).as_deref(),
            KITTY_NAMED_TRANSPORTS_ENV,
            false,
            &mut warn,
        );
        let key_bindings = parse_key_bindings(get(KEYBINDS_ENV).as_deref(), &mut warn);
        let pane_prefix = parse_pane_prefix(get(PANE_PREFIX_ENV).as_deref(), &mut warn);
        let cursor_style = parse_cursor_style_setting(get(CURSOR_STYLE_ENV).as_deref(), &mut warn);
        let cursor_blink = parse_cursor_blink_setting(get(CURSOR_BLINK_ENV).as_deref(), &mut warn);
        let cursor_easing = parse_bool_setting(
            get(CURSOR_EASING_ENV).as_deref(),
            CURSOR_EASING_ENV,
            DEFAULT_CURSOR_EASING,
            &mut warn,
        );
        let cursor_glow = parse_bool_setting(
            get(CURSOR_GLOW_ENV).as_deref(),
            CURSOR_GLOW_ENV,
            DEFAULT_CURSOR_GLOW,
            &mut warn,
        );
        let cursor_glow_intensity =
            parse_cursor_glow_intensity(get(CURSOR_GLOW_INTENSITY_ENV).as_deref(), &mut warn);
        let cursor_trail = parse_bool_setting(
            get(CURSOR_TRAIL_ENV).as_deref(),
            CURSOR_TRAIL_ENV,
            DEFAULT_CURSOR_TRAIL,
            &mut warn,
        );
        let raw_cursor_trail_strength = get(CURSOR_TRAIL_STRENGTH_ENV);
        let cursor_trail_strength = raw_cursor_trail_strength
            .as_deref()
            .and_then(|raw| raw.to_str())
            .and_then(CursorTrailStrength::parse)
            .unwrap_or_else(|| {
                if let Some(raw) = raw_cursor_trail_strength.as_ref() {
                    warn(&format!(
                        "{CURSOR_TRAIL_STRENGTH_ENV}={raw:?} is not a valid cursor-trail strength; using balanced"
                    ));
                }
                CursorTrailStrength::default()
            });
        let cursor_motion = parse_bool_setting(
            get(CURSOR_MOTION_ENV).as_deref(),
            CURSOR_MOTION_ENV,
            DEFAULT_CURSOR_MOTION,
            &mut warn,
        );
        let reduced_motion = parse_bool_setting(
            get(REDUCED_MOTION_ENV).as_deref(),
            REDUCED_MOTION_ENV,
            DEFAULT_REDUCED_MOTION,
            &mut warn,
        );
        let osc52_read = parse_bool_setting(
            get(OSC52_READ_ENV).as_deref(),
            OSC52_READ_ENV,
            false,
            &mut warn,
        );
        let osc52_write = parse_osc52_write(get(OSC52_WRITE_ENV).as_deref(), &mut warn);
        let synthetic_styles = parse_bool_setting(
            get(SYNTHETIC_STYLES_ENV).as_deref(),
            SYNTHETIC_STYLES_ENV,
            true,
            &mut warn,
        );
        let geometric_boxdraw = parse_bool_setting(
            get(GEOMETRIC_BOXDRAW_ENV).as_deref(),
            GEOMETRIC_BOXDRAW_ENV,
            true,
            &mut warn,
        );
        let symbol_fallback = parse_bool_setting(
            get(SYMBOL_FALLBACK_ENV).as_deref(),
            SYMBOL_FALLBACK_ENV,
            true,
            &mut warn,
        );
        let symbol_font = get(SYMBOL_FONT_ENV).and_then(parse_symbol_font_path);
        let symbol_map = get(SYMBOL_MAP_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|raw| parse_symbol_map(&raw, &mut warn))
            .unwrap_or_default();
        let themed_ui_roles = parse_bool_setting(
            get(THEMED_UI_ROLES_ENV).as_deref(),
            THEMED_UI_ROLES_ENV,
            true,
            &mut warn,
        );
        let scroll_wheel_lines =
            parse_scroll_wheel_lines(get(SCROLL_WHEEL_LINES_ENV).as_deref(), &mut warn);
        let scrollback_lines =
            parse_scrollback_lines(get(SCROLLBACK_LINES_ENV).as_deref(), &mut warn);
        let scroll_drag_speed =
            parse_scroll_drag_speed(get(SCROLL_DRAG_SPEED_ENV).as_deref(), &mut warn);
        let pixel_scroll = parse_bool_setting(
            get(PIXEL_SCROLL_ENV).as_deref(),
            PIXEL_SCROLL_ENV,
            DEFAULT_PIXEL_SCROLL,
            &mut warn,
        );
        let scroll_pixel_speed =
            parse_scroll_pixel_speed(get(SCROLL_PIXEL_SPEED_ENV).as_deref(), &mut warn);
        let scroll_glide = parse_bool_setting(
            get(SCROLL_GLIDE_ENV).as_deref(),
            SCROLL_GLIDE_ENV,
            DEFAULT_SCROLL_GLIDE,
            &mut warn,
        );
        let copy_on_select = parse_bool_setting(
            get(COPY_ON_SELECT_ENV).as_deref(),
            COPY_ON_SELECT_ENV,
            DEFAULT_COPY_ON_SELECT,
            &mut warn,
        );
        let smart_ctrl_c = parse_smart_ctrl_c(get(SMART_CTRL_C_ENV).as_deref(), &mut warn);
        let selection_drag_extend = parse_bool_setting(
            get(SELECTION_DRAG_EXTEND_ENV).as_deref(),
            SELECTION_DRAG_EXTEND_ENV,
            DEFAULT_SELECTION_DRAG_EXTEND,
            &mut warn,
        );
        let scrollbar_drag = parse_bool_setting(
            get(SCROLLBAR_DRAG_ENV).as_deref(),
            SCROLLBAR_DRAG_ENV,
            DEFAULT_SCROLLBAR_DRAG,
            &mut warn,
        );
        let wheel_zoom = parse_bool_setting(
            get(WHEEL_ZOOM_ENV).as_deref(),
            WHEEL_ZOOM_ENV,
            DEFAULT_WHEEL_ZOOM,
            &mut warn,
        );
        let command_status_gutter = parse_bool_setting(
            get(COMMAND_STATUS_GUTTER_ENV).as_deref(),
            COMMAND_STATUS_GUTTER_ENV,
            DEFAULT_COMMAND_STATUS_GUTTER,
            &mut warn,
        );
        let always_show_tab_bar = parse_bool_setting(
            get(ALWAYS_SHOW_TAB_BAR_ENV).as_deref(),
            ALWAYS_SHOW_TAB_BAR_ENV,
            DEFAULT_ALWAYS_SHOW_TAB_BAR,
            &mut warn,
        );
        // Rail side and visibility are separate settings. The side-of-record is
        // the `tab_bar_placement` field (kept so `workspace_rail_side()` is
        // unchanged); the canonical `ODYTTY_WORKSPACE_RAIL_SIDE` (left|right)
        // wins over the legacy `ODYTTY_TAB_BAR_PLACEMENT` (top|left|right,
        // top->left) when both are set. A legacy `workspace_rail=left|right`
        // still parses: it folds to visibility Always and supplies the side when
        // the canonical side key is absent. Precedence (side): canonical
        // WORKSPACE_RAIL_SIDE > legacy workspace_rail=left|right > tab_bar_placement.
        let workspace_rail_side_raw = get(WORKSPACE_RAIL_SIDE_ENV);
        let mut workspace_rail =
            parse_workspace_rail(get(WORKSPACE_RAIL_ENV).as_deref(), &mut warn);
        let folded_side = workspace_rail.forced_side();
        if folded_side.is_some() {
            workspace_rail = WorkspaceRail::Always;
        }
        let tab_bar_placement = if workspace_rail_side_raw.is_some() {
            parse_workspace_rail_side(workspace_rail_side_raw.as_deref(), &mut warn)
        } else if let Some(side) = folded_side {
            side
        } else {
            parse_tab_bar_placement(get(TAB_BAR_PLACEMENT_ENV).as_deref(), &mut warn)
        };
        let tab_rail_width = parse_tab_rail_width(
            rail_get!(WORKSPACE_RAIL_WIDTH_ENV, TAB_RAIL_WIDTH_ENV).as_deref(),
            &mut warn,
        );
        let tab_bar_height = parse_tab_bar_height(get(TAB_BAR_HEIGHT_ENV).as_deref(), &mut warn);
        let tab_rail_max_width = parse_tab_rail_max_width(
            rail_get!(WORKSPACE_RAIL_MAX_WIDTH_ENV, TAB_RAIL_MAX_WIDTH_ENV).as_deref(),
            &mut warn,
        );
        let tab_rail_gap = parse_tab_rail_gap(
            rail_get!(WORKSPACE_RAIL_GAP_ENV, TAB_RAIL_GAP_ENV).as_deref(),
            &mut warn,
        );
        let tab_rail_slot_rows = parse_tab_rail_slot_rows(
            rail_get!(WORKSPACE_RAIL_SLOT_ROWS_ENV, TAB_RAIL_SLOT_ROWS_ENV).as_deref(),
            &mut warn,
        );
        let tab_panel_strength =
            parse_tab_panel_strength(get(TAB_PANEL_STRENGTH_ENV).as_deref(), &mut warn);
        let tab_seam = parse_bool_setting(
            get(TAB_SEAM_ENV).as_deref(),
            TAB_SEAM_ENV,
            DEFAULT_TAB_SEAM,
            &mut warn,
        );
        let tab_rail_autohide = parse_bool_setting(
            rail_get!(WORKSPACE_RAIL_AUTOHIDE_ENV, TAB_RAIL_AUTOHIDE_ENV).as_deref(),
            TAB_RAIL_AUTOHIDE_ENV,
            DEFAULT_TAB_RAIL_AUTOHIDE,
            &mut warn,
        );
        let tab_rail_reveal_px = parse_tab_rail_reveal_px(
            rail_get!(WORKSPACE_RAIL_REVEAL_PX_ENV, TAB_RAIL_REVEAL_PX_ENV).as_deref(),
            &mut warn,
        );
        let sh_click = parse_bool_setting(
            get(SH_CLICK_ENV).as_deref(),
            SH_CLICK_ENV,
            DEFAULT_SH_CLICK,
            &mut warn,
        );
        let buttons = parse_bool_setting(
            get(BUTTONS_ENV).as_deref(),
            BUTTONS_ENV,
            DEFAULT_BUTTONS,
            &mut warn,
        );
        let buttons_iterm_compat = parse_bool_setting(
            get(BUTTONS_ITERM_COMPAT_ENV).as_deref(),
            BUTTONS_ITERM_COMPAT_ENV,
            DEFAULT_BUTTONS_ITERM_COMPAT,
            &mut warn,
        );
        let buttons_sticky = parse_bool_setting(
            get(BUTTONS_STICKY_ENV).as_deref(),
            BUTTONS_STICKY_ENV,
            DEFAULT_BUTTONS_STICKY,
            &mut warn,
        );
        let shell_integration = parse_bool_setting(
            get(SHELL_INTEGRATION_ENV).as_deref(),
            SHELL_INTEGRATION_ENV,
            DEFAULT_SHELL_INTEGRATION,
            &mut warn,
        );
        let shell_key_enhancement = parse_bool_setting(
            get(SHELL_KEY_ENHANCEMENT_ENV).as_deref(),
            SHELL_KEY_ENHANCEMENT_ENV,
            DEFAULT_SHELL_KEY_ENHANCEMENT,
            &mut warn,
        );
        let restore_workspaces = parse_bool_setting(
            get(RESTORE_WORKSPACES_ENV).as_deref(),
            RESTORE_WORKSPACES_ENV,
            DEFAULT_RESTORE_WORKSPACES,
            &mut warn,
        );
        let new_output_fade = parse_bool_setting(
            get(NEW_OUTPUT_FADE_ENV).as_deref(),
            NEW_OUTPUT_FADE_ENV,
            DEFAULT_NEW_OUTPUT_FADE,
            &mut warn,
        );
        let new_output_fade_ms =
            parse_new_output_fade_ms(get(NEW_OUTPUT_FADE_MS_ENV).as_deref(), &mut warn);
        let window_border = parse_bool_setting(
            get(WINDOW_BORDER_ENV).as_deref(),
            WINDOW_BORDER_ENV,
            DEFAULT_WINDOW_BORDER,
            &mut warn,
        );
        let window_decorations = parse_bool_setting(
            get(WINDOW_DECORATIONS_ENV).as_deref(),
            WINDOW_DECORATIONS_ENV,
            DEFAULT_WINDOW_DECORATIONS,
            &mut warn,
        );
        let window_transparency = parse_bool_setting(
            get(WINDOW_TRANSPARENCY_ENV).as_deref(),
            WINDOW_TRANSPARENCY_ENV,
            DEFAULT_WINDOW_TRANSPARENCY,
            &mut warn,
        );
        let window_opacity = parse_window_opacity(get(WINDOW_OPACITY_ENV).as_deref(), &mut warn);
        let cvd_mode = parse_cvd_mode(get(CVD_MODE_ENV).as_deref(), &mut warn);
        let cvd_strength = parse_cvd_strength(get(CVD_STRENGTH_ENV).as_deref(), &mut warn);
        let bell = parse_bell(get(BELL_ENV).as_deref(), &mut warn);
        let shell_exit_closes =
            parse_shell_exit_closes(get(SHELL_EXIT_CLOSES_ENV).as_deref(), &mut warn);
        // `theme = system` forces OS following on regardless of the explicit
        // `follow_os_theme` value, so the alias is self-sufficient. The explicit
        // setting still wins for display/writeback of that specific key.
        let explicit_follow_os_theme = parse_bool_setting(
            get(FOLLOW_OS_THEME_ENV).as_deref(),
            FOLLOW_OS_THEME_ENV,
            DEFAULT_FOLLOW_OS_THEME,
            &mut warn,
        );
        let follow_os_theme = theme_is_system || explicit_follow_os_theme;
        // OS-THEME dark/light theme names are stored verbatim (trimmed, empty =
        // unset) and resolved to a built-in theme lazily when the OS signal
        // applies, so an unknown name warns at apply time, not parse time.
        let os_theme_dark = get(OS_THEME_DARK_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let os_theme_light = get(OS_THEME_LIGHT_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let confirm_close = parse_bool_setting(
            get(CONFIRM_CLOSE_ENV).as_deref(),
            CONFIRM_CLOSE_ENV,
            DEFAULT_CONFIRM_CLOSE,
            &mut warn,
        );
        let ssh_config_hosts = parse_bool_setting(
            get(SSH_CONFIG_HOSTS_ENV).as_deref(),
            SSH_CONFIG_HOSTS_ENV,
            DEFAULT_SSH_CONFIG_HOSTS,
            &mut warn,
        );
        let remote_integration = parse_bool_setting(
            get(REMOTE_INTEGRATION_ENV).as_deref(),
            REMOTE_INTEGRATION_ENV,
            DEFAULT_REMOTE_INTEGRATION,
            &mut warn,
        );
        let remote_reuse = parse_bool_setting(
            get(REMOTE_REUSE_ENV).as_deref(),
            REMOTE_REUSE_ENV,
            DEFAULT_REMOTE_REUSE,
            &mut warn,
        );
        let remote_tmux = parse_bool_setting(
            get(REMOTE_TMUX_ENV).as_deref(),
            REMOTE_TMUX_ENV,
            DEFAULT_REMOTE_TMUX,
            &mut warn,
        );
        let remote_persist = parse_remote_persist(get(REMOTE_PERSIST_ENV).as_deref(), &mut warn);
        let remote_image_paste =
            parse_remote_image_paste(get(REMOTE_IMAGE_PASTE_ENV).as_deref(), &mut warn);
        let session_replay = parse_bool_setting(
            get(SESSION_REPLAY_ENV).as_deref(),
            SESSION_REPLAY_ENV,
            DEFAULT_SESSION_REPLAY,
            &mut warn,
        );
        let interactive_urls = parse_bool_setting(
            get(INTERACTIVE_URLS_ENV).as_deref(),
            INTERACTIVE_URLS_ENV,
            DEFAULT_INTERACTIVE_URLS,
            &mut warn,
        );
        let interactive_paths = parse_bool_setting(
            get(INTERACTIVE_PATHS_ENV).as_deref(),
            INTERACTIVE_PATHS_ENV,
            DEFAULT_INTERACTIVE_PATHS,
            &mut warn,
        );
        let interactive_paths_barewords = parse_bool_setting(
            get(INTERACTIVE_PATHS_BAREWORDS_ENV).as_deref(),
            INTERACTIVE_PATHS_BAREWORDS_ENV,
            DEFAULT_INTERACTIVE_PATHS_BAREWORDS,
            &mut warn,
        );
        let interactive_paths_click_hint = parse_bool_setting(
            get(INTERACTIVE_PATHS_CLICK_HINT_ENV).as_deref(),
            INTERACTIVE_PATHS_CLICK_HINT_ENV,
            DEFAULT_INTERACTIVE_PATHS_CLICK_HINT,
            &mut warn,
        );
        let interactive_paths_image_inline = parse_bool_setting(
            get(INTERACTIVE_PATHS_IMAGE_INLINE_ENV).as_deref(),
            INTERACTIVE_PATHS_IMAGE_INLINE_ENV,
            DEFAULT_INTERACTIVE_PATHS_IMAGE_INLINE,
            &mut warn,
        );
        // Trim surrounding whitespace; empty (the default) means "use $EDITOR".
        // No validation here — the editor spec is tokenized + matched at open
        // time (a path/template with internal spaces is preserved as written).
        let interactive_paths_editor = get(INTERACTIVE_PATHS_EDITOR_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_owned())
            .unwrap_or_default();
        let native_autoclose = parse_autoclose(get(NATIVE_AUTOCLOSE_ENV).as_deref());

        Self {
            theme,
            theme_config,
            visual,
            font_path,
            explicit_font_path,
            font_family,
            font_weight,
            font_size_px,
            text_gamma,
            stem_darken,
            min_contrast,
            focus_dim,
            inactive_pane_dim,
            render_quality,
            background_treatment,
            background_image,
            background_blur_radius,
            background_image_scrim,
            cell_bg_opacity,
            colored_bg_opacity,
            selection_opacity,
            text_brightness,
            window_padding_px,
            bloom,
            bloom_threshold,
            bloom_intensity,
            bloom_radius,
            retro,
            crt,
            crt_scanline_intensity,
            crt_scanline_period,
            crt_vignette_strength,
            crt_curvature,
            subpixel,
            line_height,
            box_thickness,
            ligatures,
            kitty_named_transports,
            key_bindings,
            pane_prefix,
            cursor_style,
            cursor_blink,
            cursor_easing,
            cursor_glow,
            cursor_glow_intensity,
            cursor_trail,
            cursor_trail_strength,
            cursor_motion,
            reduced_motion,
            osc52_read,
            osc52_write,
            synthetic_styles,
            geometric_boxdraw,
            symbol_fallback,
            symbol_font,
            symbol_map,
            themed_ui_roles,
            scroll_wheel_lines,
            scrollback_lines,
            scroll_drag_speed,
            copy_on_select,
            smart_ctrl_c,
            selection_drag_extend,
            scrollbar_drag,
            wheel_zoom,
            command_status_gutter,
            always_show_tab_bar,
            tab_bar_placement,
            workspace_rail,
            tab_rail_width,
            tab_bar_height,
            tab_rail_max_width,
            tab_rail_gap,
            tab_rail_slot_rows,
            tab_panel_strength,
            tab_seam,
            tab_rail_autohide,
            tab_rail_reveal_px,
            sh_click,
            buttons,
            buttons_iterm_compat,
            buttons_sticky,
            shell_integration,
            shell_key_enhancement,
            restore_workspaces,
            new_output_fade,
            new_output_fade_ms,
            window_border,
            window_decorations,
            window_transparency,
            window_opacity,
            pixel_scroll,
            scroll_pixel_speed,
            scroll_glide,
            cvd_mode,
            cvd_strength,
            bell,
            shell_exit_closes,
            theme_is_system,
            follow_os_theme,
            os_theme_dark,
            os_theme_light,
            confirm_close,
            ssh_config_hosts,
            remote_integration,
            remote_reuse,
            remote_tmux,
            remote_persist,
            remote_image_paste,
            session_replay,
            interactive_urls,
            interactive_paths,
            interactive_paths_barewords,
            interactive_paths_click_hint,
            interactive_paths_image_inline,
            interactive_paths_editor,
            native_autoclose,
        }
    }
}

impl Settings {
    /// The `theme` value as it should round-trip to config and display in the
    /// panel: the `system` alias token when the OS-following alias is active,
    /// else the raw `theme` config string the user set (a file path or user
    /// theme name, preserved so a file-loaded theme re-reads its file rather
    /// than the non-resolving `"custom"` placeholder), else the resolved
    /// theme's own name. Shared by `to_edit_values` and `setting_info` so the
    /// writeback baseline and the displayed value never diverge (C4).
    pub(crate) fn theme_config_value(&self) -> String {
        if self.theme_is_system {
            crate::settings::SYSTEM_THEME_NAME.to_owned()
        } else if let Some(cfg) = self.theme_config.as_deref() {
            cfg.to_owned()
        } else {
            self.theme.name.to_owned()
        }
    }

    fn to_edit_values(&self) -> BTreeMap<&'static str, String> {
        let mut values = BTreeMap::new();
        // When the `system` alias is active, write back the alias token (not the
        // internal fallback theme name) so the config round-trips as `theme =
        // system` and the user's intent is preserved.
        values.insert(THEME_ENV, self.theme_config_value());
        values.insert(VISUAL_ENV, self.visual.as_str().to_owned());
        // The `font` config key reflects the RAW explicit override, never the
        // face resolved from `font_family` (RC4): a family pick must not make the
        // writeback baseline carry a `font` value.
        if let Some(path) = self.explicit_font_path.as_ref() {
            values.insert(FONT_ENV, path.display().to_string());
        }
        if let Some(family) = self.font_family.as_ref() {
            values.insert(FONT_FAMILY_ENV, family.clone());
        }
        if !self.font_weight.is_empty() {
            values.insert(FONT_WEIGHT_ENV, self.font_weight.clone());
        }
        values.insert(FONT_SIZE_ENV, format_float(self.font_size_px));
        values.insert(TEXT_GAMMA_ENV, format_float(self.text_gamma));
        values.insert(STEM_DARKEN_ENV, format_float(self.stem_darken));
        values.insert(MIN_CONTRAST_ENV, format_float(self.min_contrast));
        values.insert(FOCUS_DIM_ENV, format_float(self.focus_dim));
        values.insert(INACTIVE_PANE_DIM_ENV, format_float(self.inactive_pane_dim));
        values.insert(RENDER_QUALITY_ENV, self.render_quality.as_str().to_owned());
        values.insert(
            BACKGROUND_TREATMENT_ENV,
            self.background_treatment.as_str().to_owned(),
        );
        if let Some(path) = self.background_image.as_ref() {
            // Render the bundled-default sentinel as the friendly opt-in token so
            // the written config round-trips and never exposes the raw marker.
            let rendered = if is_bundled_background(path) {
                BUNDLED_BACKGROUND_TOKEN.to_owned()
            } else {
                path.display().to_string()
            };
            values.insert(BACKGROUND_IMAGE_ENV, rendered);
        }
        values.insert(
            BACKGROUND_BLUR_RADIUS_ENV,
            self.background_blur_radius.to_string(),
        );
        if let Some(scrim) = self.background_image_scrim {
            values.insert(BACKGROUND_IMAGE_SCRIM_ENV, format_float(scrim));
        }
        values.insert(CELL_BG_OPACITY_ENV, format_float(self.cell_bg_opacity));
        values.insert(
            COLORED_BG_OPACITY_ENV,
            format_float(self.colored_bg_opacity),
        );
        values.insert(SELECTION_OPACITY_ENV, format_float(self.selection_opacity));
        values.insert(TEXT_BRIGHTNESS_ENV, format_float(self.text_brightness));
        values.insert(WINDOW_PADDING_ENV, format_float(self.window_padding_px));
        values.insert(BLOOM_ENV, bool_display(self.bloom).to_owned());
        values.insert(BLOOM_THRESHOLD_ENV, format_float(self.bloom_threshold));
        values.insert(BLOOM_INTENSITY_ENV, format_float(self.bloom_intensity));
        values.insert(BLOOM_RADIUS_ENV, format_float(self.bloom_radius));
        values.insert(RETRO_ENV, bool_display(self.retro).to_owned());
        values.insert(CRT_ENV, bool_display(self.crt).to_owned());
        values.insert(
            CRT_SCANLINE_INTENSITY_ENV,
            format_float(self.crt_scanline_intensity),
        );
        values.insert(
            CRT_SCANLINE_PERIOD_ENV,
            format_float(self.crt_scanline_period),
        );
        values.insert(
            CRT_VIGNETTE_STRENGTH_ENV,
            format_float(self.crt_vignette_strength),
        );
        values.insert(CRT_CURVATURE_ENV, format_float(self.crt_curvature));
        values.insert(SUBPIXEL_ENV, subpixel_display(self.subpixel).to_owned());
        values.insert(LINE_HEIGHT_ENV, format_float(self.line_height));
        values.insert(BOX_THICKNESS_ENV, format_float(self.box_thickness));
        values.insert(LIGATURES_ENV, bool_display(self.ligatures).to_owned());
        values.insert(
            KITTY_NAMED_TRANSPORTS_ENV,
            bool_display(self.kitty_named_transports).to_owned(),
        );
        values.insert(KEYBINDS_ENV, key_bindings_edit_value(&self.key_bindings));
        values.insert(PANE_PREFIX_ENV, pane_prefix_display(self.pane_prefix));
        values.insert(
            CURSOR_STYLE_ENV,
            cursor_style_display(self.cursor_style).to_owned(),
        );
        values.insert(CURSOR_BLINK_ENV, self.cursor_blink.as_str().to_owned());
        values.insert(
            CURSOR_EASING_ENV,
            bool_display(self.cursor_easing).to_owned(),
        );
        values.insert(CURSOR_GLOW_ENV, bool_display(self.cursor_glow).to_owned());
        values.insert(
            CURSOR_GLOW_INTENSITY_ENV,
            format_float(self.cursor_glow_intensity),
        );
        values.insert(CURSOR_TRAIL_ENV, bool_display(self.cursor_trail).to_owned());
        values.insert(
            CURSOR_TRAIL_STRENGTH_ENV,
            self.cursor_trail_strength.as_str().to_owned(),
        );
        values.insert(
            CURSOR_MOTION_ENV,
            bool_display(self.cursor_motion).to_owned(),
        );
        values.insert(
            REDUCED_MOTION_ENV,
            bool_display(self.reduced_motion).to_owned(),
        );
        values.insert(OSC52_READ_ENV, bool_display(self.osc52_read).to_owned());
        values.insert(OSC52_WRITE_ENV, self.osc52_write.as_str().to_owned());
        values.insert(
            SYNTHETIC_STYLES_ENV,
            bool_display(self.synthetic_styles).to_owned(),
        );
        values.insert(
            GEOMETRIC_BOXDRAW_ENV,
            bool_display(self.geometric_boxdraw).to_owned(),
        );
        values.insert(
            SYMBOL_FALLBACK_ENV,
            bool_display(self.symbol_fallback).to_owned(),
        );
        if let Some(path) = self.symbol_font.as_ref() {
            values.insert(SYMBOL_FONT_ENV, path.display().to_string());
        }
        if !self.symbol_map.is_empty() {
            values.insert(SYMBOL_MAP_ENV, format_symbol_map(&self.symbol_map));
        }
        values.insert(
            THEMED_UI_ROLES_ENV,
            bool_display(self.themed_ui_roles).to_owned(),
        );
        values.insert(
            SCROLL_WHEEL_LINES_ENV,
            format_float(self.scroll_wheel_lines),
        );
        values.insert(SCROLLBACK_LINES_ENV, format_float(self.scrollback_lines));
        values.insert(
            SCROLL_DRAG_SPEED_ENV,
            self.scroll_drag_speed.as_str().to_owned(),
        );
        values.insert(
            COPY_ON_SELECT_ENV,
            bool_display(self.copy_on_select).to_owned(),
        );
        values.insert(SMART_CTRL_C_ENV, self.smart_ctrl_c.as_str().to_owned());
        values.insert(
            SELECTION_DRAG_EXTEND_ENV,
            bool_display(self.selection_drag_extend).to_owned(),
        );
        values.insert(
            SCROLLBAR_DRAG_ENV,
            bool_display(self.scrollbar_drag).to_owned(),
        );
        values.insert(WHEEL_ZOOM_ENV, bool_display(self.wheel_zoom).to_owned());
        values.insert(
            COMMAND_STATUS_GUTTER_ENV,
            bool_display(self.command_status_gutter).to_owned(),
        );
        values.insert(
            ALWAYS_SHOW_TAB_BAR_ENV,
            bool_display(self.always_show_tab_bar).to_owned(),
        );
        values.insert(
            TAB_BAR_PLACEMENT_ENV,
            self.tab_bar_placement.as_str().to_owned(),
        );
        values.insert(WORKSPACE_RAIL_ENV, self.workspace_rail.as_str().to_owned());
        values.insert(TAB_RAIL_WIDTH_ENV, self.tab_rail_width.as_config_string());
        values.insert(TAB_BAR_HEIGHT_ENV, self.tab_bar_height.as_config_string());
        values.insert(
            TAB_RAIL_MAX_WIDTH_ENV,
            format_float(self.tab_rail_max_width),
        );
        values.insert(TAB_RAIL_GAP_ENV, format_float(self.tab_rail_gap));
        values.insert(
            TAB_RAIL_SLOT_ROWS_ENV,
            format_float(self.tab_rail_slot_rows),
        );
        values.insert(
            TAB_PANEL_STRENGTH_ENV,
            format_float(self.tab_panel_strength),
        );
        values.insert(TAB_SEAM_ENV, bool_display(self.tab_seam).to_owned());
        values.insert(
            TAB_RAIL_AUTOHIDE_ENV,
            bool_display(self.tab_rail_autohide).to_owned(),
        );
        values.insert(
            TAB_RAIL_REVEAL_PX_ENV,
            format_float(self.tab_rail_reveal_px),
        );
        values.insert(SH_CLICK_ENV, bool_display(self.sh_click).to_owned());
        values.insert(BUTTONS_ENV, bool_display(self.buttons).to_owned());
        values.insert(
            BUTTONS_ITERM_COMPAT_ENV,
            bool_display(self.buttons_iterm_compat).to_owned(),
        );
        values.insert(
            BUTTONS_STICKY_ENV,
            bool_display(self.buttons_sticky).to_owned(),
        );
        values.insert(
            SHELL_INTEGRATION_ENV,
            bool_display(self.shell_integration).to_owned(),
        );
        values.insert(
            SHELL_KEY_ENHANCEMENT_ENV,
            bool_display(self.shell_key_enhancement).to_owned(),
        );
        values.insert(
            RESTORE_WORKSPACES_ENV,
            bool_display(self.restore_workspaces).to_owned(),
        );
        values.insert(
            NEW_OUTPUT_FADE_ENV,
            bool_display(self.new_output_fade).to_owned(),
        );
        values.insert(
            NEW_OUTPUT_FADE_MS_ENV,
            format_float(self.new_output_fade_ms),
        );
        values.insert(
            WINDOW_BORDER_ENV,
            bool_display(self.window_border).to_owned(),
        );
        values.insert(
            WINDOW_DECORATIONS_ENV,
            bool_display(self.window_decorations).to_owned(),
        );
        values.insert(
            WINDOW_TRANSPARENCY_ENV,
            bool_display(self.window_transparency).to_owned(),
        );
        values.insert(WINDOW_OPACITY_ENV, format_float(self.window_opacity));
        values.insert(PIXEL_SCROLL_ENV, bool_display(self.pixel_scroll).to_owned());
        values.insert(
            SCROLL_PIXEL_SPEED_ENV,
            format_float(self.scroll_pixel_speed),
        );
        values.insert(SCROLL_GLIDE_ENV, bool_display(self.scroll_glide).to_owned());
        values.insert(CVD_MODE_ENV, self.cvd_mode.as_str().to_owned());
        values.insert(CVD_STRENGTH_ENV, format_float(self.cvd_strength));
        values.insert(BELL_ENV, self.bell.as_str().to_owned());
        values.insert(
            SHELL_EXIT_CLOSES_ENV,
            self.shell_exit_closes.as_str().to_owned(),
        );
        values.insert(
            FOLLOW_OS_THEME_ENV,
            bool_display(self.follow_os_theme).to_owned(),
        );
        if let Some(name) = self.os_theme_dark.as_ref() {
            values.insert(OS_THEME_DARK_ENV, name.clone());
        }
        if let Some(name) = self.os_theme_light.as_ref() {
            values.insert(OS_THEME_LIGHT_ENV, name.clone());
        }
        values.insert(
            CONFIRM_CLOSE_ENV,
            bool_display(self.confirm_close).to_owned(),
        );
        values.insert(
            SSH_CONFIG_HOSTS_ENV,
            bool_display(self.ssh_config_hosts).to_owned(),
        );
        values.insert(
            REMOTE_INTEGRATION_ENV,
            bool_display(self.remote_integration).to_owned(),
        );
        values.insert(REMOTE_REUSE_ENV, bool_display(self.remote_reuse).to_owned());
        values.insert(REMOTE_TMUX_ENV, bool_display(self.remote_tmux).to_owned());
        values.insert(REMOTE_PERSIST_ENV, self.remote_persist.as_str().to_owned());
        values.insert(
            REMOTE_IMAGE_PASTE_ENV,
            self.remote_image_paste.as_str().to_owned(),
        );
        values.insert(
            SESSION_REPLAY_ENV,
            bool_display(self.session_replay).to_owned(),
        );
        values.insert(
            INTERACTIVE_URLS_ENV,
            bool_display(self.interactive_urls).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_ENV,
            bool_display(self.interactive_paths).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_BAREWORDS_ENV,
            bool_display(self.interactive_paths_barewords).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_CLICK_HINT_ENV,
            bool_display(self.interactive_paths_click_hint).to_owned(),
        );
        values.insert(
            INTERACTIVE_PATHS_IMAGE_INLINE_ENV,
            bool_display(self.interactive_paths_image_inline).to_owned(),
        );
        if !self.interactive_paths_editor.is_empty() {
            values.insert(
                INTERACTIVE_PATHS_EDITOR_ENV,
                self.interactive_paths_editor.clone(),
            );
        }
        if let Some(duration) = self.native_autoclose {
            values.insert(NATIVE_AUTOCLOSE_ENV, duration.as_millis().to_string());
        }
        values
    }
}

impl SettingsEditOverlay {
    pub fn new(settings: &Settings) -> Self {
        let values = settings.to_edit_values();
        Self {
            base_values: values.clone(),
            values,
            settings: settings.clone(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn changes(&self) -> Vec<SettingEdit> {
        self.base_values
            .keys()
            .chain(self.values.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|env| self.base_values.get(env) != self.values.get(env))
            .filter_map(|env| setting_key_for_env(env).map(|key| (key, env)))
            .map(|(key, env)| SettingEdit {
                key,
                env,
                value: self.values.get(env).cloned().unwrap_or_default(),
            })
            .collect()
    }

    pub fn changed_count(&self) -> usize {
        self.changes().len()
    }

    pub fn mark_saved(&mut self) {
        self.base_values = self.values.clone();
    }

    /// Adopt externally-applied settings as the clean baseline, while replaying
    /// any pending panel-owned edits on top.
    pub fn rebase_onto(&mut self, new: &Settings) {
        let mut pending: Vec<(&'static str, Option<String>)> = Vec::new();
        for env in self
            .base_values
            .keys()
            .chain(self.values.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            if self.base_values.get(env) != self.values.get(env) {
                pending.push((env, self.values.get(env).cloned()));
            }
        }

        let nv = new.to_edit_values();
        self.base_values = nv.clone();
        self.values = nv;

        for (env, val) in pending {
            match val {
                Some(value) => {
                    self.values.insert(env, value);
                }
                None => {
                    self.values.remove(env);
                }
            }
        }

        // Pending edits were already parsed when accepted; fall back to the
        // incoming snapshot rather than leaving a half-applied settings view.
        self.settings = Settings::from_edit_values(&self.values).unwrap_or_else(|_| new.clone());
    }

    pub fn apply_raw(
        &mut self,
        key: &'static str,
        raw: &str,
    ) -> Result<Option<Settings>, SettingEditError> {
        let Some(info) = self
            .settings
            .setting_info()
            .into_iter()
            .find(|info| info.key == key)
        else {
            return Err(SettingEditError {
                key,
                message: "Unknown setting row.".to_owned(),
            });
        };
        if !info.reloadable {
            return Err(SettingEditError {
                key,
                message: "This setting is startup-only and cannot be edited live.".to_owned(),
            });
        }

        let mut values = self.values.clone();
        let trimmed = raw.trim();
        if clears_setting(key, trimmed) {
            values.remove(info.env);
        } else {
            values.insert(info.env, trimmed.to_owned());
        }

        let candidate = Settings::from_edit_values(&values).map_err(|mut error| {
            error.key = key;
            error
        })?;
        let canonical = candidate.to_edit_values();
        if let Some(value) = canonical.get(info.env) {
            values.insert(info.env, value.clone());
        } else {
            values.remove(info.env);
        }
        let candidate = Settings::from_edit_values(&values).map_err(|mut error| {
            error.key = key;
            error
        })?;
        if candidate == self.settings {
            self.values = values;
            return Ok(None);
        }

        self.values = values;
        self.settings = candidate.clone();
        Ok(Some(candidate))
    }
}

fn clears_setting(key: &str, value: &str) -> bool {
    (value.is_empty() || (key == "symbol_font" && value.eq_ignore_ascii_case("auto")))
        && matches!(
            key,
            "font" | "font_family" | "symbol_font" | "native_autoclose_ms"
        )
}

/// User-facing overlay message for a failed `font_family` edit, naming the
/// family and the precise reason. The current font is kept because the edit is
/// rejected (the loader is never switched to the embedded probe list here).
fn font_family_error_message(family: &str, reason: crate::text::FontResolveError) -> String {
    use crate::text::FontResolveError;
    match reason {
        FontResolveError::NotFound => {
            format!("Font family \"{family}\" not found. Keeping the current font.")
        }
        FontResolveError::NotMonospace => {
            format!("Font family \"{family}\" is not monospace. Keeping the current font.")
        }
    }
}

fn setting_key_for_env(env: &str) -> Option<&'static str> {
    env_to_config_key(env)
}

fn key_bindings_edit_value(bindings: &[KeyBindingOverride]) -> String {
    bindings
        .iter()
        .map(format_key_binding)
        .collect::<Vec<_>>()
        .join(";")
}

/// Serialize key-binding overrides to the `keybinds=` config value (KB-REMAP
/// persistence). Public wrapper over the internal serializer so the native
/// remap UI writes the EXACT string the parser round-trips — never a reinvented
/// format. An empty slice yields an empty string (clears the setting).
pub fn key_bindings_config_value(overrides: &[KeyBindingOverride]) -> String {
    key_bindings_edit_value(overrides)
}

/// Display string for a single chord (KB-REMAP UI). Public wrapper over the
/// internal formatter so the on-screen label and the persisted config value
/// agree byte-for-byte (e.g. `ctrl+shift+f`).
pub fn format_key_chord(chord: KeyChord) -> String {
    format_chord(chord)
}

/// Canonical display name for a bindable action (KB-REMAP UI). The single
/// authority shared with the settings-panel `keybinds` options list, so the
/// remap menu and the config tokens never drift.
pub fn bindable_action_display_name(action: BindableAction) -> &'static str {
    bindable_action_name(action)
}

/// Parse a `ODYTTY_SYMBOL_MAP` string into a [`crate::text::SymbolMap`] (SYMMAP).
///
/// Grammar: semicolon-separated rules, each `U+XXXX[-U+YYYY]=FontFamilyName`.
/// A single codepoint (`U+XXXX=Name`) is treated as the range `XXXX..=XXXX`.
/// Hex is case-insensitive; the `U+`/`u+` prefix is required on each codepoint.
/// The font name is everything after the first `=`, trimmed, and may contain
/// spaces. Malformed entries (no `=`, empty font, bad codepoint, degenerate
/// range) are warned and skipped — a bad rule never aborts startup, and an
/// empty result is the identity (no override). First-match-wins preserves the
/// written order.
fn parse_symbol_map(raw: &str, warn: &mut impl FnMut(&str)) -> crate::text::SymbolMap {
    let mut map = crate::text::SymbolMap::new();
    let parse_cp = |s: &str| -> Option<u32> {
        let hex = s.strip_prefix("U+").or_else(|| s.strip_prefix("u+"))?;
        u32::from_str_radix(hex, 16).ok()
    };
    for entry in raw.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((range_part, font_part)) = entry.split_once('=') else {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has no '=' separator; skipping"
            ));
            continue;
        };
        let range_part = range_part.trim();
        let font_name = font_part.trim();
        if font_name.is_empty() {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has an empty font name; skipping"
            ));
            continue;
        }
        let (start_str, end_str) = match range_part.split_once('-') {
            Some((s, e)) => (s.trim(), e.trim()),
            None => (range_part, range_part),
        };
        let (Some(start), Some(end)) = (parse_cp(start_str), parse_cp(end_str)) else {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has an invalid codepoint range (expected U+XXXX[-U+YYYY]); skipping"
            ));
            continue;
        };
        if !map.push(start, end, font_name) {
            warn(&format!(
                "{SYMBOL_MAP_ENV}: rule {entry:?} has start > end; skipping"
            ));
        }
    }
    map
}

/// Serialize a [`crate::text::SymbolMap`] back to its `ODYTTY_SYMBOL_MAP` config
/// string (the inverse of [`parse_symbol_map`]). Each rule renders as
/// `U+XXXX-U+YYYY=Font` (or `U+XXXX=Font` when start == end), joined by `; `, so
/// the persisted value round-trips through the parser byte-for-byte.
fn format_symbol_map(map: &crate::text::SymbolMap) -> String {
    map.rules()
        .iter()
        .map(|rule| {
            let (start, end) = rule.bounds();
            if start == end {
                format!("U+{start:04X}={}", rule.font())
            } else {
                format!("U+{start:04X}-U+{end:04X}={}", rule.font())
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn parse_symbol_font_path(raw: OsString) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    match raw.into_string() {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        }
        Err(value) => Some(PathBuf::from(value)),
    }
}

/// Normalize an `ODYTTY_FONT_WEIGHT` value (RV7). Trims surrounding whitespace
/// and treats `regular`/`normal` (case-insensitive) as the identity case,
/// returning an empty string so the effective font query is the plain
/// `font_family` exactly as before. Any other token is preserved verbatim
/// (e.g. `Light`, `SemiBold`) to be appended to the family at face resolution.
fn parse_font_weight_variant(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("regular")
        || trimmed.eq_ignore_ascii_case("normal")
    {
        String::new()
    } else {
        trimmed.to_string()
    }
}

pub fn config_file_path() -> Option<PathBuf> {
    config_base_dir_from_env(
        std::env::var_os("APPDATA"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .map(|dir| dir.join(CONFIG_FILE_NAME))
}

/// Resolve the OdyTTY config directory (`<base>/odytty`) from the relevant
/// environment values, following the platform base rules: on Windows
/// `%APPDATA%\\odytty` when APPDATA is set (falling through when it is not),
/// then `$XDG_CONFIG_HOME/odytty`, then `$HOME/.config/odytty`. Pure and
/// testable; the public wrappers pass the live process env and append the
/// file/dir leaf. `None` when nothing resolves.
fn config_base_dir_from_env(
    appdata: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let non_empty = |value: OsString| (!value.is_empty()).then(|| PathBuf::from(value));

    #[cfg(windows)]
    if let Some(base) = appdata.and_then(non_empty) {
        return Some(base.join(CONFIG_DIR_NAME));
    }
    #[cfg(not(windows))]
    let _ = &appdata;

    if let Some(base) = xdg_config_home.and_then(non_empty) {
        return Some(base.join(CONFIG_DIR_NAME));
    }

    home.and_then(non_empty)
        .map(|home| home.join(".config").join(CONFIG_DIR_NAME))
}

/// Resolved user theme directory (`<config-dir>/odytty/themes`), mirroring
/// [`config_file_path`]'s base-directory rules. `ODYTTY_THEME` values that are
/// not built-in names are looked up here (by `<name>.theme` or `<name>`).
pub fn theme_dir_path() -> Option<PathBuf> {
    config_base_dir_from_env(
        std::env::var_os("APPDATA"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .map(|dir| dir.join(THEME_DIR_NAME))
}

/// Read a user theme file for an `ODYTTY_THEME` value that is not a built-in
/// name. Resolution order:
///
/// 1. A path-like value (contains a separator or ends in `.theme`) is read
///    directly.
/// 2. Otherwise the value is looked up in `theme_dir` as `<value>.theme` and
///    then `<value>`.
///
/// Returns the file contents, or `None` when nothing resolves (caller falls
/// back to plain). All IO errors are swallowed into `None` — a bad theme value
/// must never abort startup.
fn resolve_theme_file(value: &str, theme_dir: Option<&Path>) -> Option<String> {
    let looks_like_path = value.contains('/') || value.ends_with(".theme");
    if looks_like_path && let Ok(contents) = fs_read::read_capped(Path::new(value)) {
        return Some(contents);
    }
    let dir = theme_dir?;
    let named = dir.join(format!("{value}.theme"));
    if let Ok(contents) = fs_read::read_capped(&named) {
        return Some(contents);
    }
    fs_read::read_capped(&dir.join(value)).ok()
}

pub(super) fn normalize_name(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests;

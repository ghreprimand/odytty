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
    ConfigWritebackError, ConfigWritebackResult, write_settings_changes,
    write_settings_changes_to_path,
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

/// Runtime flag mirroring [`Settings::symbol_fallback`], published
/// process-wide so the GPU renderer can rebuild the glyph atlas when live
/// settings enable or disable the RV6 symbol / Nerd-font fallback. Defaults to
/// `false`, preserving the missing-glyph path unless explicitly enabled.
static SYMBOL_FALLBACK_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

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

/// Default cursor blink policy (`ODYTTY_CURSOR_BLINK`). This is the host default
/// applied at power-on and after DECSCUSR 0 / RIS / DECSTR; an application's
/// DECSCUSR can still override it at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorBlink {
    /// Cursor blinks by default.
    On,
    /// Cursor is steady by default.
    Off,
    /// Conventional terminal default. Currently resolves to blinking; reserved
    /// to later follow a system/app preference.
    #[default]
    Auto,
}

impl CursorBlink {
    /// Resolve the policy to a concrete default blink flag for the core.
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
}

impl BindableAction {
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
    base_values: BTreeMap<&'static str, String>,
    values: BTreeMap<&'static str, String>,
    settings: Settings,
}

/// Master render-quality profile. `Balanced` is the current renderer behavior;
/// `Plain` derives a hard fast path by neutralizing optional visual work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderQuality {
    Plain,
    Balanced,
    High,
}

impl Default for RenderQuality {
    fn default() -> Self {
        Self::Balanced
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
    /// No treatment — background drawn unchanged (default).
    #[default]
    Off,
    /// Vertical gradient: darkens toward the bottom rows.
    Gradient,
    /// Radial vignette: darkens toward the edges and corners.
    Vignette,
}

impl BackgroundTreatment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Gradient => "gradient",
            Self::Vignette => "vignette",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match normalize_name(value).as_str() {
            "off" | "none" | "false" | "plain" => Some(Self::Off),
            "gradient" | "linear" => Some(Self::Gradient),
            "vignette" | "radial" => Some(Self::Vignette),
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

/// Typed runtime settings used by the native prototype.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: Theme,
    pub visual: VisualEffect,
    pub font_path: Option<PathBuf>,
    pub font_family: Option<String>,
    pub font_size_px: f32,
    pub text_gamma: f32,
    /// Stem-darkening strength in `0.0..=1.0` (RV5). `0.0` (default) disables
    /// the raster-time coverage boost and is pixel-identical to before.
    pub stem_darken: f32,
    /// Minimum fg/bg WCAG contrast floor in `1.0..=21.0` (RV1). `1.0` (default)
    /// disables enforcement and is pixel-identical to before.
    pub min_contrast: f32,
    pub focus_dim: f32,
    pub render_quality: RenderQuality,
    /// ID3/U5 background treatment (`off` default ⇒ pixel-identical plain path).
    pub background_treatment: BackgroundTreatment,
    /// Logical pixels of inset between the window edge and terminal grid. `0.0`
    /// preserves the historical edge-to-edge geometry exactly.
    pub window_padding_px: f32,
    pub bloom: bool,
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub bloom_radius: f32,
    pub crt: bool,
    pub crt_scanline_intensity: f32,
    pub crt_scanline_period: f32,
    pub crt_vignette_strength: f32,
    pub subpixel: SubpixelMode,
    /// Line-height multiplier baked into the glyph cell (LINEHEIGHT). `1.0`
    /// (default) adds zero leading and is pixel-identical to before; higher
    /// values grow the cell box and add symmetric vertical breathing room.
    pub line_height: f32,
    /// Box-drawing stroke-thickness multiplier (BOXTHICK). `1.0` (default)
    /// reproduces the historical geometric box-drawing weights byte-identically;
    /// other values scale the rule thickness.
    pub box_thickness: f32,
    pub key_bindings: Vec<KeyBindingOverride>,
    /// Default cursor shape applied at power-on (DECSCUSR can override).
    pub cursor_style: CursorStyle,
    /// Default cursor blink policy applied at power-on (DECSCUSR can override).
    pub cursor_blink: CursorBlink,
    /// Whether the cursor eases its opacity across the blink toggle (ID1). Off
    /// by default; the off path holds alpha at `1.0` and hard-hides on the blink
    /// off-phase, byte-identical to before. Purely presentational.
    pub cursor_easing: bool,
    /// Whether the cursor draws a soft concentric halo behind the block (ID1).
    /// Off by default; the off path emits no glow quads, byte-identical to
    /// before. Purely presentational; never affects cell semantics or the
    /// logical cursor position.
    pub cursor_glow: bool,
    /// Whether the cursor glides between adjacent positions instead of
    /// teleporting (VE4). Off by default; the off path sits at the exact cell
    /// origin (zero offset), byte-identical to before. Discontinuities always
    /// snap. The logical cursor position is always the destination cell.
    pub cursor_motion: bool,
    /// Whether OSC 52 clipboard read/query replies are enabled. Off by default
    /// to avoid silent clipboard exfiltration.
    pub osc52_read: bool,
    /// Whether the renderer synthesizes missing bold/italic faces from the
    /// regular outline (double-strike embolden + shear). On by default; turning
    /// it off makes styled cells render as plain regular glyphs when no real
    /// face is loaded. Purely presentational — never affects cell semantics.
    pub synthetic_styles: bool,
    /// Whether box-drawing, block-element and Powerline glyphs are rendered
    /// geometrically (cell-aligned rectangles/rails/arcs/triangles) instead of
    /// from the font (RV2). Off by default; the font path is byte-identical to
    /// before. Purely presentational — never affects cell semantics.
    pub geometric_boxdraw: bool,
    /// Whether to install a symbol / Nerd-font fallback face for private-use
    /// prompt icons (RV6). Off by default so the plain missing-glyph path is
    /// byte-identical unless explicitly enabled.
    pub symbol_fallback: bool,
    /// Optional explicit symbol / Nerd-font file path. `None` means auto-resolve
    /// a suitable symbol face when [`Settings::symbol_fallback`] is enabled.
    pub symbol_font: Option<PathBuf>,
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
    /// Click-to-position-cursor on the live prompt (SH-CLICK). When on, a plain
    /// left click on the shell prompt line moves the shell's input cursor to the
    /// clicked column by emitting Left/Right cursor keys — the click slice of
    /// OSC 133 `click_events`. Off by default and additionally gated on the
    /// shell having advertised `click_events=1`, so the off path (and a
    /// non-integrated shell) emits nothing and is byte-identical to today.
    pub sh_click: bool,
    /// Whether freshly arrived output rows fade in at the live tail (VE4). Off
    /// by default; the off path emits no fade quads, schedules no extra wakes,
    /// and is byte-identical to before. The fade is a background-color overlay
    /// quad that decays to transparent, so the underlying content is always
    /// fully rendered and never drops below the RV1 floor mid-fade. Only at the
    /// live tail; scrollback and resize snap. Purely presentational.
    pub new_output_fade: bool,
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
    pub native_autoclose: Option<Duration>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::PLAIN,
            visual: VisualEffect::Off,
            font_path: None,
            font_family: None,
            font_size_px: DEFAULT_FONT_SIZE_PX,
            text_gamma: DEFAULT_TEXT_GAMMA,
            stem_darken: DEFAULT_STEM_DARKEN,
            min_contrast: DEFAULT_MIN_CONTRAST,
            focus_dim: DEFAULT_FOCUS_DIM,
            render_quality: RenderQuality::default(),
            background_treatment: BackgroundTreatment::default(),
            window_padding_px: DEFAULT_WINDOW_PADDING_PX,
            bloom: DEFAULT_BLOOM,
            bloom_threshold: default_bloom_threshold_for_theme(Theme::PLAIN),
            bloom_intensity: DEFAULT_BLOOM_INTENSITY,
            bloom_radius: DEFAULT_BLOOM_RADIUS,
            crt: DEFAULT_CRT,
            crt_scanline_intensity: DEFAULT_CRT_SCANLINE_INTENSITY,
            crt_scanline_period: DEFAULT_CRT_SCANLINE_PERIOD,
            crt_vignette_strength: DEFAULT_CRT_VIGNETTE_STRENGTH,
            subpixel: SubpixelMode::Off,
            line_height: DEFAULT_LINE_HEIGHT,
            box_thickness: DEFAULT_BOX_THICKNESS,
            key_bindings: Vec::new(),
            cursor_style: CursorStyle::Block,
            cursor_blink: CursorBlink::Auto,
            cursor_easing: DEFAULT_CURSOR_EASING,
            cursor_glow: DEFAULT_CURSOR_GLOW,
            cursor_motion: DEFAULT_CURSOR_MOTION,
            osc52_read: false,
            synthetic_styles: true,
            geometric_boxdraw: false,
            symbol_fallback: false,
            symbol_font: None,
            themed_ui_roles: true,
            scroll_wheel_lines: DEFAULT_SCROLL_WHEEL_LINES,
            scroll_drag_speed: ScrollDragSpeed::default(),
            copy_on_select: DEFAULT_COPY_ON_SELECT,
            selection_drag_extend: DEFAULT_SELECTION_DRAG_EXTEND,
            scrollbar_drag: DEFAULT_SCROLLBAR_DRAG,
            wheel_zoom: DEFAULT_WHEEL_ZOOM,
            command_status_gutter: DEFAULT_COMMAND_STATUS_GUTTER,
            sh_click: DEFAULT_SH_CLICK,
            new_output_fade: DEFAULT_NEW_OUTPUT_FADE,
            cvd_mode: CvdMode::default(),
            cvd_strength: DEFAULT_CVD_STRENGTH,
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
        !self.plain_render_quality() && self.bloom
    }

    pub fn effective_crt_enabled(&self) -> bool {
        // UX5: CRT is normally suppressed by the plain render-quality fast path.
        // The retired `visual=ambient` scanline path was NEVER plain-gated, so
        // when it aliases into CRT (see `from_source`) we preserve that
        // back-compat by bypassing the plain gate specifically for the ambient
        // alias. An explicit `crt=` config under a plain profile still obeys the
        // plain gate; only the legacy ambient route is exempt.
        let ambient_alias = self.visual == VisualEffect::Ambient;
        (!self.plain_render_quality() || ambient_alias) && self.crt
    }

    /// Rows advanced per mouse-wheel notch (MOUSE-WHEEL-SPEED), as a `usize >= 1`.
    /// Rounds the stored `f32` (kept in the shared numeric-setting model) and
    /// floors at 1 so a wheel notch always moves at least one row. The default
    /// `3.0` returns `3`, byte-identical to the historical fixed step.
    pub fn scroll_wheel_step(&self) -> usize {
        (self.scroll_wheel_lines.round() as i64).max(1) as usize
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
        if let Some(message) = warnings.into_iter().next() {
            return Err(SettingEditError { key: "", message });
        }
        Ok(settings)
    }

    fn from_env_and_optional_config(config_path: Option<PathBuf>) -> Self {
        let mut warnings = Vec::new();
        let config = config_path
            .as_deref()
            .and_then(
                |path| match ConfigValues::read(path, |message| warnings.push(message)) {
                    Ok(values) => Some(values),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        warnings.push(format!(
                            "could not read config file {}: {error}",
                            path.display()
                        ));
                        None
                    }
                },
            )
            .unwrap_or_default();

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
        // shared `ThemeSpec` path. A missing/garbage value falls back to plain
        // with a warning — startup never fails from a bad theme setting.
        let theme = match get(THEME_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            None => Theme::PLAIN,
            Some(value) => {
                if let Some(builtin) = Theme::from_name(&value) {
                    builtin
                } else if let Some(contents) = read_theme(&value) {
                    let spec = ThemeSpec::parse(&contents, |message| {
                        warn(&format!("theme {value:?}: {message}"))
                    });
                    spec.to_theme()
                } else {
                    warn(&format!(
                        "{THEME_ENV}={value:?} is not a built-in theme or a readable theme file; using plain"
                    ));
                    Theme::PLAIN
                }
            }
        };
        let visual = get(VISUAL_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| VisualEffect::from_name_or_default(&value))
            .unwrap_or(VisualEffect::Off);
        // Direct path knob (ODYTTY_FONT) takes precedence over family lookup so
        // an explicit file always wins. ODYTTY_FONT_FAMILY is resolved to a
        // validated monospace path only when no direct path is given; resolution
        // failure falls back to the embedded probe list (font_path = None) with
        // one warning, so a bad family value never aborts startup.
        let direct_path = get(FONT_ENV).map(PathBuf::from);
        let font_family = get(FONT_FAMILY_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let font_path = if direct_path.is_some() {
            direct_path
        } else if let Some(family) = font_family.as_deref() {
            match resolve_family(family) {
                Some(path) => Some(path),
                None => {
                    warn(&format!(
                        "{FONT_FAMILY_ENV}={family:?} did not resolve to a monospace font; using the default font"
                    ));
                    None
                }
            }
        } else {
            None
        };
        let font_size_px = parse_font_size(get(FONT_SIZE_ENV).as_deref(), &mut warn);
        let text_gamma = parse_text_gamma(get(TEXT_GAMMA_ENV).as_deref(), &mut warn);
        let stem_darken = parse_stem_darken(get(STEM_DARKEN_ENV).as_deref(), &mut warn);
        let min_contrast = parse_min_contrast(get(MIN_CONTRAST_ENV).as_deref(), &mut warn);
        let focus_dim = parse_focus_dim(get(FOCUS_DIM_ENV).as_deref(), &mut warn);
        let render_quality = parse_render_quality(get(RENDER_QUALITY_ENV).as_deref(), &mut warn);
        let background_treatment =
            parse_background_treatment(get(BACKGROUND_TREATMENT_ENV).as_deref(), &mut warn);
        let window_padding_px = parse_window_padding(get(WINDOW_PADDING_ENV).as_deref(), &mut warn);
        let bloom = parse_bool_setting(get(BLOOM_ENV).as_deref(), BLOOM_ENV, false, &mut warn);
        let default_bloom_threshold = default_bloom_threshold_for_theme(theme);
        let bloom_threshold = parse_bloom_threshold(
            get(BLOOM_THRESHOLD_ENV).as_deref(),
            default_bloom_threshold,
            &mut warn,
        );
        let bloom_intensity = parse_bloom_intensity(get(BLOOM_INTENSITY_ENV).as_deref(), &mut warn);
        let bloom_radius = parse_bloom_radius(get(BLOOM_RADIUS_ENV).as_deref(), &mut warn);
        // UX5: the legacy `visual=ambient`/`scanlines` scanline effect is folded
        // into the unified CRT post-process. An ambient visual aliases to
        // `crt=on` ONLY when no explicit CRT setting is present — an explicit
        // `crt=`/`ODYTTY_CRT` always wins over the alias (the alias merely fills
        // the unset case), so a config can never stack two scanline passes.
        let crt_explicit = get(CRT_ENV);
        let crt = if crt_explicit.is_some() {
            parse_bool_setting(crt_explicit.as_deref(), CRT_ENV, false, &mut warn)
        } else {
            visual == VisualEffect::Ambient
        };
        let crt_scanline_intensity =
            parse_crt_scanline_intensity(get(CRT_SCANLINE_INTENSITY_ENV).as_deref(), &mut warn);
        let crt_scanline_period =
            parse_crt_scanline_period(get(CRT_SCANLINE_PERIOD_ENV).as_deref(), &mut warn);
        let crt_vignette_strength =
            parse_crt_vignette_strength(get(CRT_VIGNETTE_STRENGTH_ENV).as_deref(), &mut warn);
        let subpixel = parse_subpixel(get(SUBPIXEL_ENV).as_deref(), &mut warn);
        let line_height = parse_line_height(get(LINE_HEIGHT_ENV).as_deref(), &mut warn);
        let box_thickness = parse_box_thickness(get(BOX_THICKNESS_ENV).as_deref(), &mut warn);
        let key_bindings = parse_key_bindings(get(KEYBINDS_ENV).as_deref(), &mut warn);
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
        let cursor_motion = parse_bool_setting(
            get(CURSOR_MOTION_ENV).as_deref(),
            CURSOR_MOTION_ENV,
            DEFAULT_CURSOR_MOTION,
            &mut warn,
        );
        let osc52_read = parse_bool_setting(
            get(OSC52_READ_ENV).as_deref(),
            OSC52_READ_ENV,
            false,
            &mut warn,
        );
        let synthetic_styles = parse_bool_setting(
            get(SYNTHETIC_STYLES_ENV).as_deref(),
            SYNTHETIC_STYLES_ENV,
            true,
            &mut warn,
        );
        let geometric_boxdraw = parse_bool_setting(
            get(GEOMETRIC_BOXDRAW_ENV).as_deref(),
            GEOMETRIC_BOXDRAW_ENV,
            false,
            &mut warn,
        );
        let symbol_fallback = parse_bool_setting(
            get(SYMBOL_FALLBACK_ENV).as_deref(),
            SYMBOL_FALLBACK_ENV,
            false,
            &mut warn,
        );
        let symbol_font = get(SYMBOL_FONT_ENV).and_then(parse_symbol_font_path);
        let themed_ui_roles = parse_bool_setting(
            get(THEMED_UI_ROLES_ENV).as_deref(),
            THEMED_UI_ROLES_ENV,
            true,
            &mut warn,
        );
        let scroll_wheel_lines =
            parse_scroll_wheel_lines(get(SCROLL_WHEEL_LINES_ENV).as_deref(), &mut warn);
        let scroll_drag_speed =
            parse_scroll_drag_speed(get(SCROLL_DRAG_SPEED_ENV).as_deref(), &mut warn);
        let copy_on_select = parse_bool_setting(
            get(COPY_ON_SELECT_ENV).as_deref(),
            COPY_ON_SELECT_ENV,
            DEFAULT_COPY_ON_SELECT,
            &mut warn,
        );
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
        let sh_click = parse_bool_setting(
            get(SH_CLICK_ENV).as_deref(),
            SH_CLICK_ENV,
            DEFAULT_SH_CLICK,
            &mut warn,
        );
        let new_output_fade = parse_bool_setting(
            get(NEW_OUTPUT_FADE_ENV).as_deref(),
            NEW_OUTPUT_FADE_ENV,
            DEFAULT_NEW_OUTPUT_FADE,
            &mut warn,
        );
        let cvd_mode = parse_cvd_mode(get(CVD_MODE_ENV).as_deref(), &mut warn);
        let cvd_strength = parse_cvd_strength(get(CVD_STRENGTH_ENV).as_deref(), &mut warn);
        let native_autoclose = parse_autoclose(get(NATIVE_AUTOCLOSE_ENV).as_deref());

        Self {
            theme,
            visual,
            font_path,
            font_family,
            font_size_px,
            text_gamma,
            stem_darken,
            min_contrast,
            focus_dim,
            render_quality,
            background_treatment,
            window_padding_px,
            bloom,
            bloom_threshold,
            bloom_intensity,
            bloom_radius,
            crt,
            crt_scanline_intensity,
            crt_scanline_period,
            crt_vignette_strength,
            subpixel,
            line_height,
            box_thickness,
            key_bindings,
            cursor_style,
            cursor_blink,
            cursor_easing,
            cursor_glow,
            cursor_motion,
            osc52_read,
            synthetic_styles,
            geometric_boxdraw,
            symbol_fallback,
            symbol_font,
            themed_ui_roles,
            scroll_wheel_lines,
            scroll_drag_speed,
            copy_on_select,
            selection_drag_extend,
            scrollbar_drag,
            wheel_zoom,
            command_status_gutter,
            sh_click,
            new_output_fade,
            cvd_mode,
            cvd_strength,
            native_autoclose,
        }
    }
}

impl Settings {
    fn to_edit_values(&self) -> BTreeMap<&'static str, String> {
        let mut values = BTreeMap::new();
        values.insert(THEME_ENV, self.theme.name.to_owned());
        values.insert(VISUAL_ENV, self.visual.as_str().to_owned());
        if let Some(path) = self.font_path.as_ref() {
            values.insert(FONT_ENV, path.display().to_string());
        }
        if let Some(family) = self.font_family.as_ref() {
            values.insert(FONT_FAMILY_ENV, family.clone());
        }
        values.insert(FONT_SIZE_ENV, format_float(self.font_size_px));
        values.insert(TEXT_GAMMA_ENV, format_float(self.text_gamma));
        values.insert(STEM_DARKEN_ENV, format_float(self.stem_darken));
        values.insert(MIN_CONTRAST_ENV, format_float(self.min_contrast));
        values.insert(FOCUS_DIM_ENV, format_float(self.focus_dim));
        values.insert(RENDER_QUALITY_ENV, self.render_quality.as_str().to_owned());
        values.insert(
            BACKGROUND_TREATMENT_ENV,
            self.background_treatment.as_str().to_owned(),
        );
        values.insert(WINDOW_PADDING_ENV, format_float(self.window_padding_px));
        values.insert(BLOOM_ENV, bool_display(self.bloom).to_owned());
        values.insert(BLOOM_THRESHOLD_ENV, format_float(self.bloom_threshold));
        values.insert(BLOOM_INTENSITY_ENV, format_float(self.bloom_intensity));
        values.insert(BLOOM_RADIUS_ENV, format_float(self.bloom_radius));
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
        values.insert(SUBPIXEL_ENV, subpixel_display(self.subpixel).to_owned());
        values.insert(LINE_HEIGHT_ENV, format_float(self.line_height));
        values.insert(BOX_THICKNESS_ENV, format_float(self.box_thickness));
        values.insert(KEYBINDS_ENV, key_bindings_edit_value(&self.key_bindings));
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
            CURSOR_MOTION_ENV,
            bool_display(self.cursor_motion).to_owned(),
        );
        values.insert(OSC52_READ_ENV, bool_display(self.osc52_read).to_owned());
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
        values.insert(
            THEMED_UI_ROLES_ENV,
            bool_display(self.themed_ui_roles).to_owned(),
        );
        values.insert(
            SCROLL_WHEEL_LINES_ENV,
            format_float(self.scroll_wheel_lines),
        );
        values.insert(
            SCROLL_DRAG_SPEED_ENV,
            self.scroll_drag_speed.as_str().to_owned(),
        );
        values.insert(
            COPY_ON_SELECT_ENV,
            bool_display(self.copy_on_select).to_owned(),
        );
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
        values.insert(SH_CLICK_ENV, bool_display(self.sh_click).to_owned());
        values.insert(
            NEW_OUTPUT_FADE_ENV,
            bool_display(self.new_output_fade).to_owned(),
        );
        values.insert(CVD_MODE_ENV, self.cvd_mode.as_str().to_owned());
        values.insert(CVD_STRENGTH_ENV, format_float(self.cvd_strength));
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

pub fn config_file_path() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        return Some(base.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME));
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .map(|home| {
            home.join(".config")
                .join(CONFIG_DIR_NAME)
                .join(CONFIG_FILE_NAME)
        })
}

/// Resolved user theme directory (`<config-dir>/odytty/themes`), mirroring
/// [`config_file_path`]'s base-directory rules. `ODYTTY_THEME` values that are
/// not built-in names are looked up here (by `<name>.theme` or `<name>`).
pub fn theme_dir_path() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        return Some(base.join(CONFIG_DIR_NAME).join(THEME_DIR_NAME));
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .map(|home| {
            home.join(".config")
                .join(CONFIG_DIR_NAME)
                .join(THEME_DIR_NAME)
        })
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
    if looks_like_path {
        if let Ok(contents) = std::fs::read_to_string(Path::new(value)) {
            return Some(contents);
        }
    }
    let dir = theme_dir?;
    let named = dir.join(format!("{value}.theme"));
    if let Ok(contents) = std::fs::read_to_string(&named) {
        return Some(contents);
    }
    std::fs::read_to_string(dir.join(value)).ok()
}

pub(super) fn normalize_name(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests;

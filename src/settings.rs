//! Runtime settings for the prototype.
//!
//! Settings are sourced from a small config file and environment variables, but
//! the rest of the app consumes this typed struct. That keeps runtime
//! configuration in one place without pushing `std::env` or file reads through
//! renderer and terminal code.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use std::path::Path;

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::{Theme, ThemeSpec, VisualEffect};

mod config;
mod reload;
mod writeback;

pub use reload::{
    ConfigReloadPoller, SettingsReloadOutcome, SettingsReloader, apply_reloadable_values,
};
pub use writeback::{
    ConfigWritebackError, ConfigWritebackResult, write_settings_changes,
    write_settings_changes_to_path,
};

use config::{ConfigValues, env_to_config_key};

pub const THEME_ENV: &str = "ODYTTY_THEME";
pub const VISUAL_ENV: &str = "ODYTTY_VISUAL";
pub const FONT_ENV: &str = "ODYTTY_FONT";
pub const FONT_FAMILY_ENV: &str = "ODYTTY_FONT_FAMILY";
pub const FONT_SIZE_ENV: &str = "ODYTTY_FONT_SIZE";
pub const TEXT_GAMMA_ENV: &str = "ODYTTY_TEXT_GAMMA";
pub const STEM_DARKEN_ENV: &str = "ODYTTY_STEM_DARKEN";
pub const MIN_CONTRAST_ENV: &str = "ODYTTY_MIN_CONTRAST";
pub const SUBPIXEL_ENV: &str = "ODYTTY_SUBPIXEL";
pub const KEYBINDS_ENV: &str = "ODYTTY_KEYBINDS";
pub const CURSOR_STYLE_ENV: &str = "ODYTTY_CURSOR_STYLE";
pub const CURSOR_BLINK_ENV: &str = "ODYTTY_CURSOR_BLINK";
pub const OSC52_READ_ENV: &str = "ODYTTY_OSC52_READ";
pub const SYNTHETIC_STYLES_ENV: &str = "ODYTTY_SYNTHETIC_STYLES";
pub const NATIVE_AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";
pub const CONFIG_FILE_NAME: &str = "odytty.conf";
pub const CONFIG_DIR_NAME: &str = "odytty";
/// Subdirectory of the config dir where user theme files (`*.theme`) live.
pub const THEME_DIR_NAME: &str = "themes";
pub const CONFIG_RELOAD_INTERVAL: Duration = Duration::from_secs(1);

const SETTING_ENV_KEYS: &[&str] = &[
    THEME_ENV,
    VISUAL_ENV,
    FONT_ENV,
    FONT_FAMILY_ENV,
    FONT_SIZE_ENV,
    TEXT_GAMMA_ENV,
    STEM_DARKEN_ENV,
    MIN_CONTRAST_ENV,
    SUBPIXEL_ENV,
    KEYBINDS_ENV,
    CURSOR_STYLE_ENV,
    CURSOR_BLINK_ENV,
    OSC52_READ_ENV,
    SYNTHETIC_STYLES_ENV,
    NATIVE_AUTOCLOSE_ENV,
];

/// Runtime flag mirroring [`Settings::synthetic_styles`], published process-wide
/// so the GPU renderer can read it without threading `Settings` through the
/// `NativeOptions` seam (whose construction literals live in another worker's
/// fenced files). Defaults to `true` (synthesis on); the native entry point
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

fn parse_cursor_style(raw: &str) -> Option<CursorStyle> {
    match normalize_name(raw).as_str() {
        "block" => Some(CursorStyle::Block),
        "underline" | "under" => Some(CursorStyle::Underline),
        "bar" | "ibeam" | "beam" | "vertical" => Some(CursorStyle::Bar),
        _ => None,
    }
}

pub const DEFAULT_FONT_SIZE_PX: f32 = 14.0;
pub const MIN_FONT_SIZE_PX: f32 = 6.0;
pub const MAX_FONT_SIZE_PX: f32 = 72.0;
pub const DEFAULT_TEXT_GAMMA: f32 = 1.4;
pub const MIN_TEXT_GAMMA: f32 = 0.5;
pub const MAX_TEXT_GAMMA: f32 = 3.0;

/// Stem-darkening strength (`ODYTTY_STEM_DARKEN`): a coverage boost applied at
/// glyph raster time so light-on-dark body text holds weight at small sizes
/// (RV5). `0.0` disables it and is pixel-identical to the pre-feature renderer;
/// `1.0` is the strongest boost. Defaults to off pending a perceptual eyeball
/// pass — see the audit findings for the recommended enable value.
pub const DEFAULT_STEM_DARKEN: f32 = 0.0;
pub const MIN_STEM_DARKEN: f32 = 0.0;
pub const MAX_STEM_DARKEN: f32 = 1.0;

/// Human-readable help for the stem-darken knob, destined for the in-app
/// settings panel (UX2). Establishes the convention that every new knob ships
/// with a concise description, its accepted values, and its default.
pub const STEM_DARKEN_DESC: &str = "Stem darkening: boosts glyph coverage so light-on-dark text holds weight at \
     small sizes. Accepts 0.0–1.0; 0.0 is off (identical to no boost), 1.0 is \
     strongest. Default 0.0.";

/// Minimum fg/bg contrast floor (`ODYTTY_MIN_CONTRAST`): a configurable WCAG
/// contrast ratio that every cell's foreground is lifted to meet, so no app can
/// render illegibly low-contrast text (RV1). `1.0` disables the floor and is
/// pixel-identical to the pre-feature renderer; higher values enforce more
/// contrast (4.5 is WCAG AA for body text, 7.0 is AAA). The lift moves only
/// perceptual lightness, preserving hue.
pub const DEFAULT_MIN_CONTRAST: f32 = 1.0;
pub const MIN_MIN_CONTRAST: f32 = 1.0;
pub const MAX_MIN_CONTRAST: f32 = 21.0;

/// Human-readable help for the minimum-contrast knob, shown in the in-app
/// settings panel (UX2). Follows the every-knob-carries-a-description convention.
pub const MIN_CONTRAST_DESC: &str = "Minimum contrast: lifts foreground text so its WCAG contrast against the \
     background meets at least this ratio, keeping low-contrast apps legible. \
     Accepts 1.0–21.0; 1.0 is off (no change), 4.5 is the WCAG AA body-text \
     threshold, 7.0 is AAA. Hue is preserved. Default 1.0.";

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

/// Broad setting type hint for the read-only UX2-a panel and later editors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Bool,
    Enum,
    Number,
    String,
    Path,
    List,
}

/// Static/dynamic metadata for one settings row in stable display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingInfo {
    pub group: &'static str,
    pub key: &'static str,
    pub env: &'static str,
    pub name: &'static str,
    pub value: String,
    pub description: &'static str,
    pub kind: SettingKind,
    pub range: Option<&'static str>,
    pub options: &'static [&'static str],
    pub reloadable: bool,
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
    pub subpixel: SubpixelMode,
    pub key_bindings: Vec<KeyBindingOverride>,
    /// Default cursor shape applied at power-on (DECSCUSR can override).
    pub cursor_style: CursorStyle,
    /// Default cursor blink policy applied at power-on (DECSCUSR can override).
    pub cursor_blink: CursorBlink,
    /// Whether OSC 52 clipboard read/query replies are enabled. Off by default
    /// to avoid silent clipboard exfiltration.
    pub osc52_read: bool,
    /// Whether the renderer synthesizes missing bold/italic faces from the
    /// regular outline (double-strike embolden + shear). On by default; turning
    /// it off makes styled cells render as plain regular glyphs when no real
    /// face is loaded. Purely presentational — never affects cell semantics.
    pub synthetic_styles: bool,
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
            subpixel: SubpixelMode::Off,
            key_bindings: Vec::new(),
            cursor_style: CursorStyle::Block,
            cursor_blink: CursorBlink::Auto,
            osc52_read: false,
            synthetic_styles: true,
            native_autoclose: None,
        }
    }
}

impl Settings {
    /// Stable read-only inventory for the in-app settings panel.
    ///
    /// This intentionally mirrors every field on [`Settings`]. UX2-b can attach
    /// editors and persistence to the same rows; UX2-a only displays them.
    pub fn setting_info(&self) -> Vec<SettingInfo> {
        vec![
            SettingInfo {
                group: "Theme",
                key: "theme",
                env: THEME_ENV,
                name: "Theme",
                value: self.theme.name.to_owned(),
                description: "Full appearance profile: default colors, ANSI palette, semantic role colors, and optional user theme files.",
                kind: SettingKind::Enum,
                range: None,
                options: &[
                    "plain",
                    "odyssey",
                    "odyssey-noir",
                    "user theme name",
                    "theme file path",
                ],
                reloadable: true,
            },
            SettingInfo {
                group: "Theme",
                key: "visual",
                env: VISUAL_ENV,
                name: "Visual effect",
                value: self.visual.as_str().to_owned(),
                description: "Optional presentation-only visual treatment. Off is the plain renderer and remains the safest fast path.",
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "ambient"],
                reloadable: true,
            },
            SettingInfo {
                group: "Font",
                key: "font",
                env: FONT_ENV,
                name: "Font file",
                value: self
                    .font_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "default monospace probe list".to_owned()),
                description: "Explicit font file path. Takes precedence over font_family and falls back safely when unreadable.",
                kind: SettingKind::Path,
                range: None,
                options: &[],
                reloadable: true,
            },
            SettingInfo {
                group: "Font",
                key: "font_family",
                env: FONT_FAMILY_ENV,
                name: "Font family",
                value: self
                    .font_family
                    .clone()
                    .unwrap_or_else(|| "unset".to_owned()),
                description: "System font family lookup for the regular monospace face. Ignored when an explicit font file is set.",
                kind: SettingKind::String,
                range: None,
                options: &[],
                reloadable: true,
            },
            SettingInfo {
                group: "Font",
                key: "font_size",
                env: FONT_SIZE_ENV,
                name: "Font size",
                value: format_float(self.font_size_px),
                description: "Native font size in pixels. Rebuilds the glyph atlas, cell metrics, terminal grid, and PTY window size.",
                kind: SettingKind::Number,
                range: Some("6.0..=72.0 px"),
                options: &[],
                reloadable: true,
            },
            SettingInfo {
                group: "Rendering",
                key: "text_gamma",
                env: TEXT_GAMMA_ENV,
                name: "Text gamma",
                value: format_float(self.text_gamma),
                description: "Glyph coverage gamma applied in the shader for text weight and contrast.",
                kind: SettingKind::Number,
                range: Some("0.5..=3.0"),
                options: &[],
                reloadable: true,
            },
            SettingInfo {
                group: "Rendering",
                key: "stem_darken",
                env: STEM_DARKEN_ENV,
                name: "Stem darkening",
                value: format_float(self.stem_darken),
                description: STEM_DARKEN_DESC,
                kind: SettingKind::Number,
                range: Some("0.0..=1.0"),
                options: &[],
                reloadable: true,
            },
            SettingInfo {
                group: "Rendering",
                key: "min_contrast",
                env: MIN_CONTRAST_ENV,
                name: "Minimum contrast",
                value: format_float(self.min_contrast),
                description: MIN_CONTRAST_DESC,
                kind: SettingKind::Number,
                range: Some("1.0..=21.0"),
                options: &[],
                reloadable: true,
            },
            SettingInfo {
                group: "Rendering",
                key: "subpixel",
                env: SUBPIXEL_ENV,
                name: "Subpixel AA",
                value: subpixel_display(self.subpixel).to_owned(),
                description: "Optional RGB/BGR subpixel text coverage. Unsupported adapters fall back to grayscale.",
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "rgb", "bgr"],
                reloadable: true,
            },
            SettingInfo {
                group: "Rendering",
                key: "synthetic_styles",
                env: SYNTHETIC_STYLES_ENV,
                name: "Synthetic styles",
                value: bool_display(self.synthetic_styles).to_owned(),
                description: "Synthesizes missing bold and italic faces from the regular font when real style faces are unavailable.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
            },
            SettingInfo {
                group: "Cursor",
                key: "cursor_style",
                env: CURSOR_STYLE_ENV,
                name: "Cursor style",
                value: cursor_style_display(self.cursor_style).to_owned(),
                description: "Host default cursor shape used at startup and after terminal reset. Applications can still override it.",
                kind: SettingKind::Enum,
                range: None,
                options: &["block", "underline", "bar"],
                reloadable: true,
            },
            SettingInfo {
                group: "Cursor",
                key: "cursor_blink",
                env: CURSOR_BLINK_ENV,
                name: "Cursor blink",
                value: self.cursor_blink.as_str().to_owned(),
                description: "Host default cursor blink policy. Auto currently resolves to blinking and is reserved for future system preference support.",
                kind: SettingKind::Enum,
                range: None,
                options: &["auto", "on", "off"],
                reloadable: true,
            },
            SettingInfo {
                group: "Input",
                key: "keybinds",
                env: KEYBINDS_ENV,
                name: "Key bindings",
                value: key_bindings_display(&self.key_bindings),
                description: "Terminal-local shortcut overrides for search, settings, theme picker, copy, paste, and scrollback actions. PTY key encoding is unchanged.",
                kind: SettingKind::List,
                range: None,
                options: &[
                    "search",
                    "settings",
                    "theme-picker",
                    "copy",
                    "paste",
                    "scroll-up",
                    "scroll-down",
                ],
                reloadable: true,
            },
            SettingInfo {
                group: "Clipboard",
                key: "osc52_read",
                env: OSC52_READ_ENV,
                name: "OSC 52 read",
                value: bool_display(self.osc52_read).to_owned(),
                description: "Allows terminal applications to query local clipboard contents through OSC 52 replies. Off by default for safety.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
            },
            SettingInfo {
                group: "Development",
                key: "native_autoclose_ms",
                env: NATIVE_AUTOCLOSE_ENV,
                name: "Native autoclose",
                value: self
                    .native_autoclose
                    .map(|duration| format!("{} ms", duration.as_millis()))
                    .unwrap_or_else(|| "unset".to_owned()),
                description: "Smoke-test helper that closes the native window after a startup delay. It is startup-only, not live-reloadable.",
                kind: SettingKind::Number,
                range: Some("positive milliseconds"),
                options: &[],
                reloadable: false,
            },
        ]
    }

    /// Load settings from the config file, then overlay the current process
    /// environment. Environment variables always win.
    pub fn from_env() -> Self {
        Self::from_env_and_optional_config(config_file_path())
    }

    fn from_edit_values(values: &BTreeMap<&'static str, String>) -> Result<Self, SettingEditError> {
        let mut warnings = Vec::new();
        let settings = Self::from_source(
            |key| values.get(key).map(OsString::from),
            |message| warnings.push(message.to_owned()),
            |family| {
                crate::text::resolve_font_family(family, &crate::text::font_search_dirs())
                    .map(|m| m.regular)
            },
            |value| resolve_theme_file(value, theme_dir_path().as_deref()),
        );
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
        let subpixel = parse_subpixel(get(SUBPIXEL_ENV).as_deref(), &mut warn);
        let key_bindings = parse_key_bindings(get(KEYBINDS_ENV).as_deref(), &mut warn);
        let cursor_style = parse_cursor_style_setting(get(CURSOR_STYLE_ENV).as_deref(), &mut warn);
        let cursor_blink = parse_cursor_blink_setting(get(CURSOR_BLINK_ENV).as_deref(), &mut warn);
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
            subpixel,
            key_bindings,
            cursor_style,
            cursor_blink,
            osc52_read,
            synthetic_styles,
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
        values.insert(SUBPIXEL_ENV, subpixel_display(self.subpixel).to_owned());
        values.insert(KEYBINDS_ENV, key_bindings_edit_value(&self.key_bindings));
        values.insert(
            CURSOR_STYLE_ENV,
            cursor_style_display(self.cursor_style).to_owned(),
        );
        values.insert(CURSOR_BLINK_ENV, self.cursor_blink.as_str().to_owned());
        values.insert(OSC52_READ_ENV, bool_display(self.osc52_read).to_owned());
        values.insert(
            SYNTHETIC_STYLES_ENV,
            bool_display(self.synthetic_styles).to_owned(),
        );
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
    value.is_empty() && matches!(key, "font" | "font_family" | "native_autoclose_ms")
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

/// Parse `ODYTTY_CURSOR_STYLE`, falling back to the default block shape with one
/// warning on an unrecognized value. Empty/unset is the silent default.
fn parse_cursor_style_setting(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> CursorStyle {
    let Some(raw) = raw else {
        return CursorStyle::Block;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return CursorStyle::Block;
    }
    match parse_cursor_style(trimmed) {
        Some(style) => style,
        None => {
            warn(&format!(
                "{CURSOR_STYLE_ENV}={trimmed:?} is not block|underline|bar; using block"
            ));
            CursorStyle::Block
        }
    }
}

/// Parse `ODYTTY_CURSOR_BLINK`, falling back to the default `auto` policy with
/// one warning on an unrecognized value. Empty/unset is the silent default.
fn parse_cursor_blink_setting(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> CursorBlink {
    let Some(raw) = raw else {
        return CursorBlink::Auto;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return CursorBlink::Auto;
    }
    match CursorBlink::parse(trimmed) {
        Some(policy) => policy,
        None => {
            warn(&format!(
                "{CURSOR_BLINK_ENV}={trimmed:?} is not on|off|auto; using auto"
            ));
            CursorBlink::Auto
        }
    }
}

fn parse_font_size(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_FONT_SIZE_PX;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_FONT_SIZE_PX;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{FONT_SIZE_ENV}={trimmed:?} is not a valid pixel size; using {DEFAULT_FONT_SIZE_PX}"
            ));
            return DEFAULT_FONT_SIZE_PX;
        }
    };

    parsed.clamp(MIN_FONT_SIZE_PX, MAX_FONT_SIZE_PX)
}

fn parse_text_gamma(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_TEXT_GAMMA;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_TEXT_GAMMA;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{TEXT_GAMMA_ENV}={trimmed:?} is not a valid gamma value; using {DEFAULT_TEXT_GAMMA}"
            ));
            return DEFAULT_TEXT_GAMMA;
        }
    };

    parsed.clamp(MIN_TEXT_GAMMA, MAX_TEXT_GAMMA)
}

fn parse_stem_darken(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_STEM_DARKEN;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_STEM_DARKEN;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{STEM_DARKEN_ENV}={trimmed:?} is not a valid stem-darken strength; using {DEFAULT_STEM_DARKEN}"
            ));
            return DEFAULT_STEM_DARKEN;
        }
    };

    parsed.clamp(MIN_STEM_DARKEN, MAX_STEM_DARKEN)
}

fn parse_min_contrast(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> f32 {
    let Some(raw) = raw else {
        return DEFAULT_MIN_CONTRAST;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_MIN_CONTRAST;
    }

    let parsed = match trimmed.parse::<f32>() {
        Ok(value) if value.is_finite() => value,
        _ => {
            warn(&format!(
                "{MIN_CONTRAST_ENV}={trimmed:?} is not a valid contrast ratio; using {DEFAULT_MIN_CONTRAST}"
            ));
            return DEFAULT_MIN_CONTRAST;
        }
    };

    parsed.clamp(MIN_MIN_CONTRAST, MAX_MIN_CONTRAST)
}

fn parse_subpixel(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> SubpixelMode {
    let Some(raw) = raw else {
        return SubpixelMode::Off;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return SubpixelMode::Off;
    }

    match normalize_name(trimmed).as_str() {
        "off" | "none" | "false" | "0" => SubpixelMode::Off,
        "rgb" => SubpixelMode::Rgb,
        "bgr" => SubpixelMode::Bgr,
        _ => {
            warn(&format!(
                "{SUBPIXEL_ENV}={trimmed:?} is not off|rgb|bgr; using off"
            ));
            SubpixelMode::Off
        }
    }
}

fn parse_autoclose(raw: Option<&OsStr>) -> Option<Duration> {
    let raw = raw?;
    let ms: u64 = raw.to_string_lossy().trim().parse().ok()?;
    (ms > 0).then_some(Duration::from_millis(ms))
}

fn format_float(value: f32) -> String {
    let formatted = format!("{value:.2}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn bool_display(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn subpixel_display(value: SubpixelMode) -> &'static str {
    match value {
        SubpixelMode::Off => "off",
        SubpixelMode::Rgb => "rgb",
        SubpixelMode::Bgr => "bgr",
    }
}

fn cursor_style_display(value: CursorStyle) -> &'static str {
    match value {
        CursorStyle::Block => "block",
        CursorStyle::Underline => "underline",
        CursorStyle::Bar => "bar",
    }
}

fn key_bindings_display(bindings: &[KeyBindingOverride]) -> String {
    if bindings.is_empty() {
        return "default key bindings".to_owned();
    }

    bindings
        .iter()
        .map(format_key_binding)
        .collect::<Vec<_>>()
        .join("; ")
}

fn format_key_binding(binding: &KeyBindingOverride) -> String {
    format!(
        "{}={}",
        format_chord(binding.chord),
        bindable_action_name(binding.action)
    )
}

fn format_chord(chord: KeyChord) -> String {
    let mut parts = Vec::new();
    if chord.modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if chord.modifiers.shift {
        parts.push("shift".to_owned());
    }
    if chord.modifiers.alt {
        parts.push("alt".to_owned());
    }
    if chord.modifiers.super_key {
        parts.push("super".to_owned());
    }
    parts.push(format_key(chord.key));
    parts.join("+")
}

fn format_key(key: KeyBindingKey) -> String {
    match key {
        KeyBindingKey::Character(',') => "comma".to_owned(),
        KeyBindingKey::Character(ch) => ch.to_string(),
        KeyBindingKey::Named(named) => match named {
            KeyBindingNamedKey::Enter => "enter".to_owned(),
            KeyBindingNamedKey::Backspace => "backspace".to_owned(),
            KeyBindingNamedKey::Escape => "esc".to_owned(),
            KeyBindingNamedKey::Tab => "tab".to_owned(),
            KeyBindingNamedKey::Space => "space".to_owned(),
            KeyBindingNamedKey::PageUp => "pageup".to_owned(),
            KeyBindingNamedKey::PageDown => "pagedown".to_owned(),
            KeyBindingNamedKey::Home => "home".to_owned(),
            KeyBindingNamedKey::End => "end".to_owned(),
            KeyBindingNamedKey::Delete => "delete".to_owned(),
            KeyBindingNamedKey::Insert => "insert".to_owned(),
            KeyBindingNamedKey::ArrowUp => "up".to_owned(),
            KeyBindingNamedKey::ArrowDown => "down".to_owned(),
            KeyBindingNamedKey::ArrowLeft => "left".to_owned(),
            KeyBindingNamedKey::ArrowRight => "right".to_owned(),
            KeyBindingNamedKey::F(number) => format!("f{number}"),
        },
    }
}

fn bindable_action_name(action: BindableAction) -> &'static str {
    match action {
        BindableAction::Search => "search",
        BindableAction::SettingsPanel => "settings",
        BindableAction::ThemePicker => "theme-picker",
        BindableAction::Copy => "copy",
        BindableAction::Paste => "paste",
        BindableAction::ScrollPageUp => "scroll-up",
        BindableAction::ScrollPageDown => "scroll-down",
    }
}

fn parse_bool_setting(
    raw: Option<&OsStr>,
    env: &str,
    default: bool,
    warn: &mut impl FnMut(&str),
) -> bool {
    let Some(raw) = raw else {
        return default;
    };
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return default;
    }
    match normalize_name(trimmed).as_str() {
        "1" | "true" | "yes" | "on" | "enabled" | "enable" => true,
        "0" | "false" | "no" | "off" | "disabled" | "disable" => false,
        _ => {
            warn(&format!("{env}={trimmed:?} is not on|off; using {default}"));
            default
        }
    }
}

fn parse_key_bindings(raw: Option<&OsStr>, warn: &mut impl FnMut(&str)) -> Vec<KeyBindingOverride> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let value = raw.to_string_lossy();
    value
        .split([',', ';'])
        .filter_map(|entry| parse_key_binding_entry(entry, warn))
        .collect()
}

fn parse_key_binding_entry(entry: &str, warn: &mut impl FnMut(&str)) -> Option<KeyBindingOverride> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return None;
    }
    let Some((chord_raw, action_raw)) = trimmed.split_once('=') else {
        warn(&format!(
            "{KEYBINDS_ENV} entry {trimmed:?} is missing '='; skipping"
        ));
        return None;
    };
    let Some(chord) = parse_key_chord(chord_raw.trim()) else {
        warn(&format!(
            "{KEYBINDS_ENV} entry {trimmed:?} has an invalid key chord; skipping"
        ));
        return None;
    };
    let Some(action) = BindableAction::parse(action_raw.trim()) else {
        warn(&format!(
            "{KEYBINDS_ENV} entry {trimmed:?} has an unknown action; skipping"
        ));
        return None;
    };
    Some(KeyBindingOverride { chord, action })
}

fn parse_key_chord(raw: &str) -> Option<KeyChord> {
    let mut modifiers = KeyBindingModifiers::default();
    let mut key = None;
    for token in raw
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        match normalize_name(token).as_str() {
            "ctrl" | "control" => modifiers.ctrl = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.alt = true,
            "super" | "meta" | "cmd" | "command" | "win" | "windows" => {
                modifiers.super_key = true;
            }
            _ if key.is_none() => key = parse_key_binding_key(token),
            _ => return None,
        }
    }
    Some(KeyChord {
        modifiers,
        key: key?,
    })
}

fn parse_key_binding_key(raw: &str) -> Option<KeyBindingKey> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.len() == 1 {
        let ch = lower.chars().next()?;
        if ch.is_ascii_graphic() && ch != '+' && ch != '=' {
            return Some(KeyBindingKey::Character(ch));
        }
    }

    let named = match normalize_name(trimmed).as_str() {
        "comma" => return Some(KeyBindingKey::Character(',')),
        "enter" | "return" => KeyBindingNamedKey::Enter,
        "backspace" | "bksp" => KeyBindingNamedKey::Backspace,
        "esc" | "escape" => KeyBindingNamedKey::Escape,
        "tab" => KeyBindingNamedKey::Tab,
        "space" | "spacebar" => KeyBindingNamedKey::Space,
        "pageup" | "pgup" => KeyBindingNamedKey::PageUp,
        "pagedown" | "pgdn" => KeyBindingNamedKey::PageDown,
        "home" => KeyBindingNamedKey::Home,
        "end" => KeyBindingNamedKey::End,
        "delete" | "del" => KeyBindingNamedKey::Delete,
        "insert" | "ins" => KeyBindingNamedKey::Insert,
        "up" | "arrowup" => KeyBindingNamedKey::ArrowUp,
        "down" | "arrowdown" => KeyBindingNamedKey::ArrowDown,
        "left" | "arrowleft" => KeyBindingNamedKey::ArrowLeft,
        "right" | "arrowright" => KeyBindingNamedKey::ArrowRight,
        f_key if f_key.starts_with('f') => {
            let number = f_key[1..].parse::<u8>().ok()?;
            if (1..=24).contains(&number) {
                KeyBindingNamedKey::F(number)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(KeyBindingKey::Named(named))
}

pub(super) fn normalize_name(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests;

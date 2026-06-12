//! Runtime settings for the prototype.
//!
//! Settings are sourced from a small config file and environment variables, but
//! the rest of the app consumes this typed struct. That keeps runtime
//! configuration in one place without pushing `std::env` or file reads through
//! renderer and terminal code.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::{Theme, VisualEffect};

mod config;
mod reload;

pub use reload::{
    ConfigReloadPoller, SettingsReloadOutcome, SettingsReloader, apply_reloadable_values,
};

use config::ConfigValues;

pub const THEME_ENV: &str = "ODYTTY_THEME";
pub const VISUAL_ENV: &str = "ODYTTY_VISUAL";
pub const FONT_ENV: &str = "ODYTTY_FONT";
pub const FONT_FAMILY_ENV: &str = "ODYTTY_FONT_FAMILY";
pub const FONT_SIZE_ENV: &str = "ODYTTY_FONT_SIZE";
pub const TEXT_GAMMA_ENV: &str = "ODYTTY_TEXT_GAMMA";
pub const SUBPIXEL_ENV: &str = "ODYTTY_SUBPIXEL";
pub const KEYBINDS_ENV: &str = "ODYTTY_KEYBINDS";
pub const CURSOR_STYLE_ENV: &str = "ODYTTY_CURSOR_STYLE";
pub const CURSOR_BLINK_ENV: &str = "ODYTTY_CURSOR_BLINK";
pub const OSC52_READ_ENV: &str = "ODYTTY_OSC52_READ";
pub const SYNTHETIC_STYLES_ENV: &str = "ODYTTY_SYNTHETIC_STYLES";
pub const NATIVE_AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";
pub const CONFIG_FILE_NAME: &str = "odytty.conf";
pub const CONFIG_DIR_NAME: &str = "odytty";
pub const CONFIG_RELOAD_INTERVAL: Duration = Duration::from_secs(1);

const SETTING_ENV_KEYS: &[&str] = &[
    THEME_ENV,
    VISUAL_ENV,
    FONT_ENV,
    FONT_FAMILY_ENV,
    FONT_SIZE_ENV,
    TEXT_GAMMA_ENV,
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

/// Terminal-local actions that can be rebound without changing PTY input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindableAction {
    Search,
    Copy,
    Paste,
    ScrollPageUp,
    ScrollPageDown,
}

impl BindableAction {
    fn parse(raw: &str) -> Option<Self> {
        match normalize_name(raw).as_str() {
            "search" | "searchtoggle" | "togglesearch" => Some(Self::Search),
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

/// Typed runtime settings used by the native prototype.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: Theme,
    pub visual: VisualEffect,
    pub font_path: Option<PathBuf>,
    pub font_family: Option<String>,
    pub font_size_px: f32,
    pub text_gamma: f32,
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
    /// Load settings from the config file, then overlay the current process
    /// environment. Environment variables always win.
    pub fn from_env() -> Self {
        Self::from_env_and_optional_config(config_file_path())
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
        )
    }

    fn from_source(
        mut get: impl FnMut(&str) -> Option<OsString>,
        mut warn: impl FnMut(&str),
        mut resolve_family: impl FnMut(&str) -> Option<PathBuf>,
    ) -> Self {
        let theme = get(THEME_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| Theme::from_name_or_default(&value))
            .unwrap_or(Theme::PLAIN);
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
        if ch.is_ascii_alphanumeric() {
            return Some(KeyBindingKey::Character(ch));
        }
    }

    let named = match normalize_name(trimmed).as_str() {
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

//! Runtime settings for the prototype.
//!
//! Today settings are sourced from environment variables, but the rest of the
//! app consumes this typed struct. That keeps runtime configuration in one place
//! so a config file can replace or augment the environment source later without
//! pushing `std::env` reads through renderer and terminal code.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::{Theme, VisualEffect};

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
pub const NATIVE_AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";

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
            native_autoclose: None,
        }
    }
}

impl Settings {
    /// Load settings from the current process environment.
    pub fn from_env() -> Self {
        Self::from_source(
            |key| std::env::var_os(key),
            |message| {
                eprintln!("odytty: {message}");
            },
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
            native_autoclose,
        }
    }
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

fn normalize_name(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !matches!(ch, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_from<const N: usize>(values: [(&str, &str); N]) -> (Settings, Vec<String>) {
        // Default stub resolver: no family resolves. Family-resolution tests use
        // `settings_from_resolving` to inject a deterministic resolver.
        settings_from_resolving(values, |_| None)
    }

    fn settings_from_resolving<const N: usize>(
        values: [(&str, &str); N],
        resolve_family: impl FnMut(&str) -> Option<PathBuf>,
    ) -> (Settings, Vec<String>) {
        let mut warnings = Vec::new();
        let settings = Settings::from_source(
            |key| {
                values
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(value))
            },
            |message| warnings.push(message.to_owned()),
            resolve_family,
        );
        (settings, warnings)
    }

    #[test]
    fn defaults_are_stable_without_env() {
        let (settings, warnings) = settings_from([]);

        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn valid_values_resolve_to_typed_settings() {
        let (settings, warnings) = settings_from([
            (THEME_ENV, "odyssey"),
            (VISUAL_ENV, "ambient"),
            (FONT_ENV, "/tmp/ody.ttf"),
            (FONT_SIZE_ENV, "18.5"),
            (TEXT_GAMMA_ENV, "1.25"),
            (SUBPIXEL_ENV, "rgb"),
            (NATIVE_AUTOCLOSE_ENV, "750"),
        ]);

        assert_eq!(settings.theme, Theme::ODYSSEY);
        assert_eq!(settings.visual, VisualEffect::Ambient);
        assert_eq!(settings.font_path, Some(PathBuf::from("/tmp/ody.ttf")));
        assert_eq!(settings.font_size_px, 18.5);
        assert_eq!(settings.text_gamma, 1.25);
        assert_eq!(settings.subpixel, SubpixelMode::Rgb);
        assert_eq!(settings.native_autoclose, Some(Duration::from_millis(750)));
        assert!(warnings.is_empty());
    }

    #[test]
    fn empty_font_size_falls_back_without_warning() {
        let (settings, warnings) = settings_from([(FONT_SIZE_ENV, "  ")]);

        assert_eq!(settings.font_size_px, DEFAULT_FONT_SIZE_PX);
        assert!(warnings.is_empty());
    }

    #[test]
    fn garbage_font_size_falls_back_with_one_warning() {
        let (settings, warnings) = settings_from([(FONT_SIZE_ENV, "huge")]);

        assert_eq!(settings.font_size_px, DEFAULT_FONT_SIZE_PX);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(FONT_SIZE_ENV));
    }

    #[test]
    fn font_size_clamps_to_sane_range() {
        let (small, small_warnings) = settings_from([(FONT_SIZE_ENV, "2")]);
        let (large, large_warnings) = settings_from([(FONT_SIZE_ENV, "900")]);

        assert_eq!(small.font_size_px, MIN_FONT_SIZE_PX);
        assert_eq!(large.font_size_px, MAX_FONT_SIZE_PX);
        assert!(small_warnings.is_empty());
        assert!(large_warnings.is_empty());
    }

    #[test]
    fn empty_text_gamma_falls_back_without_warning() {
        let (settings, warnings) = settings_from([(TEXT_GAMMA_ENV, "  ")]);

        assert_eq!(settings.text_gamma, DEFAULT_TEXT_GAMMA);
        assert!(warnings.is_empty());
    }

    #[test]
    fn garbage_text_gamma_falls_back_with_one_warning() {
        let (settings, warnings) = settings_from([(TEXT_GAMMA_ENV, "bright")]);

        assert_eq!(settings.text_gamma, DEFAULT_TEXT_GAMMA);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(TEXT_GAMMA_ENV));
    }

    #[test]
    fn text_gamma_clamps_to_sane_range() {
        let (small, small_warnings) = settings_from([(TEXT_GAMMA_ENV, "0.1")]);
        let (large, large_warnings) = settings_from([(TEXT_GAMMA_ENV, "9")]);

        assert_eq!(small.text_gamma, MIN_TEXT_GAMMA);
        assert_eq!(large.text_gamma, MAX_TEXT_GAMMA);
        assert!(small_warnings.is_empty());
        assert!(large_warnings.is_empty());
    }

    #[test]
    fn subpixel_defaults_off_and_parses_orders() {
        let (default, default_warnings) = settings_from([]);
        let (rgb, rgb_warnings) = settings_from([(SUBPIXEL_ENV, " RGB ")]);
        let (bgr, bgr_warnings) = settings_from([(SUBPIXEL_ENV, "bgr")]);
        let (off, off_warnings) = settings_from([(SUBPIXEL_ENV, "none")]);

        assert_eq!(default.subpixel, SubpixelMode::Off);
        assert_eq!(rgb.subpixel, SubpixelMode::Rgb);
        assert_eq!(bgr.subpixel, SubpixelMode::Bgr);
        assert_eq!(off.subpixel, SubpixelMode::Off);
        assert!(default_warnings.is_empty());
        assert!(rgb_warnings.is_empty());
        assert!(bgr_warnings.is_empty());
        assert!(off_warnings.is_empty());
    }

    #[test]
    fn garbage_subpixel_falls_back_with_one_warning() {
        let (settings, warnings) = settings_from([(SUBPIXEL_ENV, "pentile")]);

        assert_eq!(settings.subpixel, SubpixelMode::Off);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(SUBPIXEL_ENV));
    }

    #[test]
    fn font_family_is_parsed_and_trimmed() {
        let (settings, warnings) =
            settings_from_resolving([(FONT_FAMILY_ENV, "  JetBrains Mono  ")], |family| {
                assert_eq!(family, "JetBrains Mono");
                Some(PathBuf::from("/fonts/JetBrainsMono-Regular.ttf"))
            });
        assert_eq!(settings.font_family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(
            settings.font_path,
            Some(PathBuf::from("/fonts/JetBrainsMono-Regular.ttf"))
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn direct_font_path_wins_over_family() {
        let mut resolver_called = false;
        let (settings, warnings) = settings_from_resolving(
            [
                (FONT_ENV, "/tmp/explicit.ttf"),
                (FONT_FAMILY_ENV, "Some Family"),
            ],
            |_| {
                resolver_called = true;
                Some(PathBuf::from("/fonts/resolved.ttf"))
            },
        );
        // Explicit path takes precedence; the family resolver is never consulted.
        assert!(
            !resolver_called,
            "direct path must short-circuit resolution"
        );
        assert_eq!(settings.font_path, Some(PathBuf::from("/tmp/explicit.ttf")));
        // The raw family string is still recorded for introspection.
        assert_eq!(settings.font_family.as_deref(), Some("Some Family"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn unresolvable_family_falls_back_with_one_warning() {
        let (settings, warnings) =
            settings_from_resolving([(FONT_FAMILY_ENV, "No Such Mono")], |_| None);
        // Falls back to the embedded probe list (None) rather than failing.
        assert_eq!(settings.font_path, None);
        assert_eq!(settings.font_family.as_deref(), Some("No Such Mono"));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(FONT_FAMILY_ENV));
    }

    #[test]
    fn empty_font_family_is_ignored() {
        let (settings, warnings) = settings_from([(FONT_FAMILY_ENV, "   ")]);
        assert_eq!(settings.font_family, None);
        assert_eq!(settings.font_path, None);
        assert!(warnings.is_empty());
    }

    #[test]
    fn key_bindings_parse_valid_entries_case_insensitively() {
        let (settings, warnings) = settings_from([(
            KEYBINDS_ENV,
            "ctrl+shift+y=copy; SUPER+F=search, Shift+PageDown=scroll-down",
        )]);

        assert_eq!(settings.key_bindings.len(), 3);
        assert_eq!(
            settings.key_bindings[0],
            KeyBindingOverride {
                chord: KeyChord {
                    modifiers: KeyBindingModifiers {
                        ctrl: true,
                        shift: true,
                        alt: false,
                        super_key: false,
                    },
                    key: KeyBindingKey::Character('y'),
                },
                action: BindableAction::Copy,
            }
        );
        assert_eq!(
            settings.key_bindings[1],
            KeyBindingOverride {
                chord: KeyChord {
                    modifiers: KeyBindingModifiers {
                        ctrl: false,
                        shift: false,
                        alt: false,
                        super_key: true,
                    },
                    key: KeyBindingKey::Character('f'),
                },
                action: BindableAction::Search,
            }
        );
        assert_eq!(
            settings.key_bindings[2].chord.key,
            KeyBindingKey::Named(KeyBindingNamedKey::PageDown)
        );
        assert_eq!(
            settings.key_bindings[2].action,
            BindableAction::ScrollPageDown
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn key_bindings_skip_bad_entries_with_warnings() {
        let (settings, warnings) = settings_from([(
            KEYBINDS_ENV,
            "ctrl+shift=copy,ctrl+shift+f=nope,ctrl+x+z=paste,alt+space=paste",
        )]);

        assert_eq!(settings.key_bindings.len(), 1);
        assert_eq!(
            settings.key_bindings[0].chord.key,
            KeyBindingKey::Named(KeyBindingNamedKey::Space)
        );
        assert_eq!(settings.key_bindings[0].action, BindableAction::Paste);
        assert_eq!(warnings.len(), 3);
        assert!(
            warnings
                .iter()
                .all(|warning| warning.contains(KEYBINDS_ENV))
        );
    }

    #[test]
    fn empty_key_bindings_are_ignored_without_warning() {
        let (settings, warnings) = settings_from([(KEYBINDS_ENV, " , ; ")]);

        assert!(settings.key_bindings.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn duplicate_key_binding_entries_preserve_input_order() {
        let (settings, warnings) =
            settings_from([(KEYBINDS_ENV, "ctrl+shift+y=copy,ctrl+shift+y=paste")]);

        assert_eq!(settings.key_bindings.len(), 2);
        assert_eq!(settings.key_bindings[0].action, BindableAction::Copy);
        assert_eq!(settings.key_bindings[1].action, BindableAction::Paste);
        assert!(warnings.is_empty());
    }

    #[test]
    fn cursor_defaults_without_env() {
        let (settings, warnings) = settings_from([]);
        assert_eq!(settings.cursor_style, CursorStyle::Block);
        assert_eq!(settings.cursor_blink, CursorBlink::Auto);
        assert!(settings.cursor_blink.enabled());
        assert!(warnings.is_empty());
    }

    #[test]
    fn cursor_style_and_blink_parse_case_insensitively() {
        let (settings, warnings) =
            settings_from([(CURSOR_STYLE_ENV, "  Bar  "), (CURSOR_BLINK_ENV, "Off")]);
        assert_eq!(settings.cursor_style, CursorStyle::Bar);
        assert_eq!(settings.cursor_blink, CursorBlink::Off);
        assert!(!settings.cursor_blink.enabled());
        assert!(warnings.is_empty());

        let (underline, _) = settings_from([(CURSOR_STYLE_ENV, "underline")]);
        assert_eq!(underline.cursor_style, CursorStyle::Underline);
        let (on, _) = settings_from([(CURSOR_BLINK_ENV, "on")]);
        assert_eq!(on.cursor_blink, CursorBlink::On);
    }

    #[test]
    fn garbage_cursor_style_falls_back_with_one_warning() {
        let (settings, warnings) = settings_from([(CURSOR_STYLE_ENV, "diamond")]);
        assert_eq!(settings.cursor_style, CursorStyle::Block);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(CURSOR_STYLE_ENV));
    }

    #[test]
    fn garbage_cursor_blink_falls_back_with_one_warning() {
        let (settings, warnings) = settings_from([(CURSOR_BLINK_ENV, "sometimes")]);
        assert_eq!(settings.cursor_blink, CursorBlink::Auto);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(CURSOR_BLINK_ENV));
    }

    #[test]
    fn empty_cursor_settings_are_silent_defaults() {
        let (settings, warnings) =
            settings_from([(CURSOR_STYLE_ENV, "  "), (CURSOR_BLINK_ENV, "")]);
        assert_eq!(settings.cursor_style, CursorStyle::Block);
        assert_eq!(settings.cursor_blink, CursorBlink::Auto);
        assert!(warnings.is_empty());
    }
}

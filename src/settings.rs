//! Runtime settings for the prototype.
//!
//! Today settings are sourced from environment variables, but the rest of the
//! app consumes this typed struct. That keeps runtime configuration in one place
//! so a config file can replace or augment the environment source later without
//! pushing `std::env` reads through renderer and terminal code.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use crate::theme::{Theme, VisualEffect};

pub const THEME_ENV: &str = "ODYTTY_THEME";
pub const VISUAL_ENV: &str = "ODYTTY_VISUAL";
pub const FONT_ENV: &str = "ODYTTY_FONT";
pub const FONT_SIZE_ENV: &str = "ODYTTY_FONT_SIZE";
pub const TEXT_GAMMA_ENV: &str = "ODYTTY_TEXT_GAMMA";
pub const NATIVE_AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";

pub const DEFAULT_FONT_SIZE_PX: f32 = 14.0;
pub const MIN_FONT_SIZE_PX: f32 = 6.0;
pub const MAX_FONT_SIZE_PX: f32 = 72.0;
pub const DEFAULT_TEXT_GAMMA: f32 = 1.4;
pub const MIN_TEXT_GAMMA: f32 = 0.5;
pub const MAX_TEXT_GAMMA: f32 = 3.0;

/// Typed runtime settings used by the native prototype.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: Theme,
    pub visual: VisualEffect,
    pub font_path: Option<PathBuf>,
    pub font_size_px: f32,
    pub text_gamma: f32,
    pub native_autoclose: Option<Duration>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::PLAIN,
            visual: VisualEffect::Off,
            font_path: None,
            font_size_px: DEFAULT_FONT_SIZE_PX,
            text_gamma: DEFAULT_TEXT_GAMMA,
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
        )
    }

    fn from_source(
        mut get: impl FnMut(&str) -> Option<OsString>,
        mut warn: impl FnMut(&str),
    ) -> Self {
        let theme = get(THEME_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| Theme::from_name_or_default(&value))
            .unwrap_or(Theme::PLAIN);
        let visual = get(VISUAL_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| VisualEffect::from_name_or_default(&value))
            .unwrap_or(VisualEffect::Off);
        let font_path = get(FONT_ENV).map(PathBuf::from);
        let font_size_px = parse_font_size(get(FONT_SIZE_ENV).as_deref(), &mut warn);
        let text_gamma = parse_text_gamma(get(TEXT_GAMMA_ENV).as_deref(), &mut warn);
        let native_autoclose = parse_autoclose(get(NATIVE_AUTOCLOSE_ENV).as_deref());

        Self {
            theme,
            visual,
            font_path,
            font_size_px,
            text_gamma,
            native_autoclose,
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

fn parse_autoclose(raw: Option<&OsStr>) -> Option<Duration> {
    let raw = raw?;
    let ms: u64 = raw.to_string_lossy().trim().parse().ok()?;
    (ms > 0).then_some(Duration::from_millis(ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_from<const N: usize>(values: [(&str, &str); N]) -> (Settings, Vec<String>) {
        let mut warnings = Vec::new();
        let settings = Settings::from_source(
            |key| {
                values
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(value))
            },
            |message| warnings.push(message.to_owned()),
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
            (NATIVE_AUTOCLOSE_ENV, "750"),
        ]);

        assert_eq!(settings.theme, Theme::ODYSSEY);
        assert_eq!(settings.visual, VisualEffect::Ambient);
        assert_eq!(settings.font_path, Some(PathBuf::from("/tmp/ody.ttf")));
        assert_eq!(settings.font_size_px, 18.5);
        assert_eq!(settings.text_gamma, 1.25);
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
}

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use super::{
    CURSOR_BLINK_ENV, CURSOR_STYLE_ENV, FONT_ENV, FONT_FAMILY_ENV, FONT_SIZE_ENV,
    GEOMETRIC_BOXDRAW_ENV, KEYBINDS_ENV, MIN_CONTRAST_ENV, NATIVE_AUTOCLOSE_ENV, OSC52_READ_ENV,
    STEM_DARKEN_ENV, SUBPIXEL_ENV, SYNTHETIC_STYLES_ENV, TEXT_GAMMA_ENV, THEME_ENV, VISUAL_ENV,
    normalize_name,
};
#[derive(Debug, Clone, Default)]
pub(super) struct ConfigValues {
    values: HashMap<&'static str, OsString>,
}

impl ConfigValues {
    pub(super) fn read(path: &Path, mut warn: impl FnMut(String)) -> io::Result<Self> {
        let contents = fs::read_to_string(path)?;
        Ok(Self::parse(&contents, |message| {
            warn(format!("{}: {message}", path.display()));
        }))
    }

    pub(super) fn parse(contents: &str, mut warn: impl FnMut(String)) -> Self {
        let mut values = HashMap::new();
        for (line_index, line) in contents.lines().enumerate() {
            let line_number = line_index + 1;
            let trimmed = line
                .split_once('#')
                .map(|(before_comment, _)| before_comment)
                .unwrap_or(line)
                .trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((key_raw, value_raw)) = trimmed.split_once('=') else {
                warn(format!(
                    "line {line_number}: expected key = value; skipping"
                ));
                continue;
            };
            let key = key_raw.trim();
            if key.is_empty() {
                warn(format!("line {line_number}: empty key; skipping"));
                continue;
            }
            let Some(env_key) = config_key_to_env(key) else {
                warn(format!("line {line_number}: unknown key {key:?}; skipping"));
                continue;
            };
            values.insert(env_key, OsString::from(value_raw.trim()));
        }
        Self { values }
    }

    pub(super) fn get(&self, key: &str) -> Option<&OsString> {
        self.values.get(key)
    }
}

pub(super) fn config_key_to_env(key: &str) -> Option<&'static str> {
    match normalize_name(key).as_str() {
        "theme" => Some(THEME_ENV),
        "visual" => Some(VISUAL_ENV),
        "font" => Some(FONT_ENV),
        "fontfamily" => Some(FONT_FAMILY_ENV),
        "fontsize" => Some(FONT_SIZE_ENV),
        "textgamma" => Some(TEXT_GAMMA_ENV),
        "stemdarken" => Some(STEM_DARKEN_ENV),
        "mincontrast" => Some(MIN_CONTRAST_ENV),
        "geometricboxdraw" | "boxdraw" => Some(GEOMETRIC_BOXDRAW_ENV),
        "subpixel" => Some(SUBPIXEL_ENV),
        "keybinds" | "keybindings" => Some(KEYBINDS_ENV),
        "cursorstyle" => Some(CURSOR_STYLE_ENV),
        "cursorblink" => Some(CURSOR_BLINK_ENV),
        "osc52read" | "allowosc52read" | "clipboardread" => Some(OSC52_READ_ENV),
        "syntheticstyles" | "synthstyles" | "syntheticfonts" => Some(SYNTHETIC_STYLES_ENV),
        "nativeautoclosems" => Some(NATIVE_AUTOCLOSE_ENV),
        _ => None,
    }
}

pub(super) fn env_to_config_key(env: &str) -> Option<&'static str> {
    match env {
        THEME_ENV => Some("theme"),
        VISUAL_ENV => Some("visual"),
        FONT_ENV => Some("font"),
        FONT_FAMILY_ENV => Some("font_family"),
        FONT_SIZE_ENV => Some("font_size"),
        TEXT_GAMMA_ENV => Some("text_gamma"),
        STEM_DARKEN_ENV => Some("stem_darken"),
        MIN_CONTRAST_ENV => Some("min_contrast"),
        GEOMETRIC_BOXDRAW_ENV => Some("geometric_boxdraw"),
        SUBPIXEL_ENV => Some("subpixel"),
        KEYBINDS_ENV => Some("keybinds"),
        CURSOR_STYLE_ENV => Some("cursor_style"),
        CURSOR_BLINK_ENV => Some("cursor_blink"),
        OSC52_READ_ENV => Some("osc52_read"),
        SYNTHETIC_STYLES_ENV => Some("synthetic_styles"),
        NATIVE_AUTOCLOSE_ENV => Some("native_autoclose_ms"),
        _ => None,
    }
}

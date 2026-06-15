// SPDX-License-Identifier: GPL-3.0-only
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use super::{
    BLOOM_ENV, BLOOM_INTENSITY_ENV, BLOOM_RADIUS_ENV, BLOOM_THRESHOLD_ENV, COPY_ON_SELECT_ENV,
    CRT_ENV, CRT_SCANLINE_INTENSITY_ENV, CRT_SCANLINE_PERIOD_ENV, CRT_VIGNETTE_STRENGTH_ENV,
    CURSOR_BLINK_ENV, CURSOR_STYLE_ENV, FOCUS_DIM_ENV, FONT_ENV, FONT_FAMILY_ENV, FONT_SIZE_ENV,
    GEOMETRIC_BOXDRAW_ENV, KEYBINDS_ENV, MIN_CONTRAST_ENV, NATIVE_AUTOCLOSE_ENV, OSC52_READ_ENV,
    RENDER_QUALITY_ENV, SCROLL_DRAG_SPEED_ENV, SCROLL_WHEEL_LINES_ENV, SCROLLBAR_DRAG_ENV,
    SELECTION_DRAG_EXTEND_ENV, STEM_DARKEN_ENV, SUBPIXEL_ENV, SYMBOL_FALLBACK_ENV, SYMBOL_FONT_ENV,
    SYNTHETIC_STYLES_ENV, TEXT_GAMMA_ENV, THEME_ENV, THEMED_UI_ROLES_ENV, VISUAL_ENV,
    WINDOW_PADDING_ENV, normalize_name,
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
        "bloom" => Some(BLOOM_ENV),
        "bloomthreshold" => Some(BLOOM_THRESHOLD_ENV),
        "bloomintensity" => Some(BLOOM_INTENSITY_ENV),
        "bloomradius" => Some(BLOOM_RADIUS_ENV),
        "crt" | "retro" => Some(CRT_ENV),
        "crtscanlineintensity" | "crtscanlines" => Some(CRT_SCANLINE_INTENSITY_ENV),
        "crtscanlineperiod" | "crtscanlinedensity" => Some(CRT_SCANLINE_PERIOD_ENV),
        "crtvignettestrength" | "crtvignette" => Some(CRT_VIGNETTE_STRENGTH_ENV),
        "font" => Some(FONT_ENV),
        "fontfamily" => Some(FONT_FAMILY_ENV),
        "fontsize" => Some(FONT_SIZE_ENV),
        "textgamma" => Some(TEXT_GAMMA_ENV),
        "stemdarken" => Some(STEM_DARKEN_ENV),
        "mincontrast" => Some(MIN_CONTRAST_ENV),
        "focusdim" | "unfocuseddim" => Some(FOCUS_DIM_ENV),
        "renderquality" | "quality" | "rendermode" => Some(RENDER_QUALITY_ENV),
        "windowpadding" | "padding" | "windowpaddingpx" => Some(WINDOW_PADDING_ENV),
        "geometricboxdraw" | "boxdraw" => Some(GEOMETRIC_BOXDRAW_ENV),
        "symbolfallback" | "symbols" | "nerdfont" => Some(SYMBOL_FALLBACK_ENV),
        "symbolfont" | "nerdfontpath" | "symbolfontpath" => Some(SYMBOL_FONT_ENV),
        "themeduiroles" | "themedroles" | "uiroles" => Some(THEMED_UI_ROLES_ENV),
        "subpixel" => Some(SUBPIXEL_ENV),
        "keybinds" | "keybindings" => Some(KEYBINDS_ENV),
        "cursorstyle" => Some(CURSOR_STYLE_ENV),
        "cursorblink" => Some(CURSOR_BLINK_ENV),
        "osc52read" | "allowosc52read" | "clipboardread" => Some(OSC52_READ_ENV),
        "syntheticstyles" | "synthstyles" | "syntheticfonts" => Some(SYNTHETIC_STYLES_ENV),
        "scrollwheellines" | "wheellines" | "scrollspeed" | "scrollwheelspeed" => {
            Some(SCROLL_WHEEL_LINES_ENV)
        }
        "scrolldragspeed" | "dragscrollspeed" | "autoscrollspeed" | "dragautoscroll" => {
            Some(SCROLL_DRAG_SPEED_ENV)
        }
        "copyonselect" | "selecttoclipboard" => Some(COPY_ON_SELECT_ENV),
        "selectiondragextend" | "dragextend" | "dragextendselection" => {
            Some(SELECTION_DRAG_EXTEND_ENV)
        }
        "scrollbardrag" | "draggablescrollbar" | "scrollthumbdrag" => Some(SCROLLBAR_DRAG_ENV),
        "nativeautoclosems" => Some(NATIVE_AUTOCLOSE_ENV),
        _ => None,
    }
}

pub(super) fn env_to_config_key(env: &str) -> Option<&'static str> {
    match env {
        THEME_ENV => Some("theme"),
        VISUAL_ENV => Some("visual"),
        BLOOM_ENV => Some("bloom"),
        BLOOM_THRESHOLD_ENV => Some("bloom_threshold"),
        BLOOM_INTENSITY_ENV => Some("bloom_intensity"),
        BLOOM_RADIUS_ENV => Some("bloom_radius"),
        CRT_ENV => Some("crt"),
        CRT_SCANLINE_INTENSITY_ENV => Some("crt_scanline_intensity"),
        CRT_SCANLINE_PERIOD_ENV => Some("crt_scanline_period"),
        CRT_VIGNETTE_STRENGTH_ENV => Some("crt_vignette_strength"),
        FONT_ENV => Some("font"),
        FONT_FAMILY_ENV => Some("font_family"),
        FONT_SIZE_ENV => Some("font_size"),
        TEXT_GAMMA_ENV => Some("text_gamma"),
        STEM_DARKEN_ENV => Some("stem_darken"),
        MIN_CONTRAST_ENV => Some("min_contrast"),
        FOCUS_DIM_ENV => Some("focus_dim"),
        RENDER_QUALITY_ENV => Some("render_quality"),
        WINDOW_PADDING_ENV => Some("window_padding"),
        GEOMETRIC_BOXDRAW_ENV => Some("geometric_boxdraw"),
        SYMBOL_FALLBACK_ENV => Some("symbol_fallback"),
        SYMBOL_FONT_ENV => Some("symbol_font"),
        THEMED_UI_ROLES_ENV => Some("themed_ui_roles"),
        SUBPIXEL_ENV => Some("subpixel"),
        KEYBINDS_ENV => Some("keybinds"),
        CURSOR_STYLE_ENV => Some("cursor_style"),
        CURSOR_BLINK_ENV => Some("cursor_blink"),
        OSC52_READ_ENV => Some("osc52_read"),
        SYNTHETIC_STYLES_ENV => Some("synthetic_styles"),
        SCROLL_WHEEL_LINES_ENV => Some("scroll_wheel_lines"),
        SCROLL_DRAG_SPEED_ENV => Some("scroll_drag_speed"),
        COPY_ON_SELECT_ENV => Some("copy_on_select"),
        SELECTION_DRAG_EXTEND_ENV => Some("selection_drag_extend"),
        SCROLLBAR_DRAG_ENV => Some("scrollbar_drag"),
        NATIVE_AUTOCLOSE_ENV => Some("native_autoclose_ms"),
        _ => None,
    }
}

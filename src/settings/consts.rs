// SPDX-License-Identifier: GPL-3.0-only
use std::time::Duration;

use crate::theme::Theme;

pub const THEME_ENV: &str = "ODYTTY_THEME";
pub const VISUAL_ENV: &str = "ODYTTY_VISUAL";
pub const FONT_ENV: &str = "ODYTTY_FONT";
pub const FONT_FAMILY_ENV: &str = "ODYTTY_FONT_FAMILY";
pub const FONT_SIZE_ENV: &str = "ODYTTY_FONT_SIZE";
pub const TEXT_GAMMA_ENV: &str = "ODYTTY_TEXT_GAMMA";
pub const STEM_DARKEN_ENV: &str = "ODYTTY_STEM_DARKEN";
pub const MIN_CONTRAST_ENV: &str = "ODYTTY_MIN_CONTRAST";
pub const FOCUS_DIM_ENV: &str = "ODYTTY_FOCUS_DIM";
pub const RENDER_QUALITY_ENV: &str = "ODYTTY_RENDER_QUALITY";
pub const WINDOW_PADDING_ENV: &str = "ODYTTY_WINDOW_PADDING";
pub const BLOOM_ENV: &str = "ODYTTY_BLOOM";
pub const BLOOM_THRESHOLD_ENV: &str = "ODYTTY_BLOOM_THRESHOLD";
pub const BLOOM_INTENSITY_ENV: &str = "ODYTTY_BLOOM_INTENSITY";
pub const BLOOM_RADIUS_ENV: &str = "ODYTTY_BLOOM_RADIUS";
pub const CRT_ENV: &str = "ODYTTY_CRT";
pub const CRT_SCANLINE_INTENSITY_ENV: &str = "ODYTTY_CRT_SCANLINE_INTENSITY";
pub const CRT_SCANLINE_PERIOD_ENV: &str = "ODYTTY_CRT_SCANLINE_PERIOD";
pub const CRT_VIGNETTE_STRENGTH_ENV: &str = "ODYTTY_CRT_VIGNETTE_STRENGTH";
pub const SUBPIXEL_ENV: &str = "ODYTTY_SUBPIXEL";
pub const KEYBINDS_ENV: &str = "ODYTTY_KEYBINDS";
pub const CURSOR_STYLE_ENV: &str = "ODYTTY_CURSOR_STYLE";
pub const CURSOR_BLINK_ENV: &str = "ODYTTY_CURSOR_BLINK";
pub const OSC52_READ_ENV: &str = "ODYTTY_OSC52_READ";
pub const SYNTHETIC_STYLES_ENV: &str = "ODYTTY_SYNTHETIC_STYLES";
pub const GEOMETRIC_BOXDRAW_ENV: &str = "ODYTTY_GEOMETRIC_BOXDRAW";
pub const SYMBOL_FALLBACK_ENV: &str = "ODYTTY_SYMBOL_FALLBACK";
pub const SYMBOL_FONT_ENV: &str = "ODYTTY_SYMBOL_FONT";
pub const THEMED_UI_ROLES_ENV: &str = "ODYTTY_THEMED_UI_ROLES";
pub const NATIVE_AUTOCLOSE_ENV: &str = "ODYTTY_NATIVE_AUTOCLOSE_MS";
pub const CONFIG_FILE_NAME: &str = "odytty.conf";
pub const CONFIG_DIR_NAME: &str = "odytty";
/// Subdirectory of the config dir where user theme files (`*.theme`) live.
pub const THEME_DIR_NAME: &str = "themes";
pub const CONFIG_RELOAD_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) const SETTING_ENV_KEYS: &[&str] = &[
    THEME_ENV,
    VISUAL_ENV,
    FONT_ENV,
    FONT_FAMILY_ENV,
    FONT_SIZE_ENV,
    TEXT_GAMMA_ENV,
    STEM_DARKEN_ENV,
    MIN_CONTRAST_ENV,
    FOCUS_DIM_ENV,
    RENDER_QUALITY_ENV,
    WINDOW_PADDING_ENV,
    BLOOM_ENV,
    BLOOM_THRESHOLD_ENV,
    BLOOM_INTENSITY_ENV,
    BLOOM_RADIUS_ENV,
    CRT_ENV,
    CRT_SCANLINE_INTENSITY_ENV,
    CRT_SCANLINE_PERIOD_ENV,
    CRT_VIGNETTE_STRENGTH_ENV,
    SUBPIXEL_ENV,
    KEYBINDS_ENV,
    CURSOR_STYLE_ENV,
    CURSOR_BLINK_ENV,
    OSC52_READ_ENV,
    SYNTHETIC_STYLES_ENV,
    GEOMETRIC_BOXDRAW_ENV,
    SYMBOL_FALLBACK_ENV,
    SYMBOL_FONT_ENV,
    THEMED_UI_ROLES_ENV,
    NATIVE_AUTOCLOSE_ENV,
];

pub const DEFAULT_FONT_SIZE_PX: f32 = 14.0;
pub const MIN_FONT_SIZE_PX: f32 = 6.0;
pub const MAX_FONT_SIZE_PX: f32 = 72.0;
pub const DEFAULT_TEXT_GAMMA: f32 = 1.4;
pub const MIN_TEXT_GAMMA: f32 = 0.5;
pub const MAX_TEXT_GAMMA: f32 = 3.0;

/// Stem-darkening strength (`ODYTTY_STEM_DARKEN`): a coverage boost applied at
/// glyph raster time so light-on-dark body text holds weight at small sizes
/// (RV5). `0.0` disables it and is pixel-identical to the pre-feature renderer;
/// `1.0` is the strongest boost. Ships default-on at a deliberately conservative
/// `0.2` -- perceptibly crisper stems without looking bold. Setting `0.0` is the
/// opt-out and fully restores the classic, pre-feature raster.
pub const DEFAULT_STEM_DARKEN: f32 = 0.2;
pub const MIN_STEM_DARKEN: f32 = 0.0;
pub const MAX_STEM_DARKEN: f32 = 1.0;

/// Minimum fg/bg contrast floor (`ODYTTY_MIN_CONTRAST`): a configurable WCAG
/// contrast ratio that every cell's foreground is lifted to meet, so no app can
/// render illegibly low-contrast text (RV1). `1.0` disables the floor and is
/// pixel-identical to the pre-feature renderer; higher values enforce more
/// contrast (4.5 is WCAG AA for body text, 7.0 is AAA). The lift moves only
/// perceptual lightness, preserving hue.
pub const DEFAULT_MIN_CONTRAST: f32 = 1.0;
pub const MIN_MIN_CONTRAST: f32 = 1.0;
pub const MAX_MIN_CONTRAST: f32 = 21.0;

/// Focus dimming amount (`ODYTTY_FOCUS_DIM`): how much the whole grid (both text
/// and background) recedes perceptually while the window is unfocused (ID2). The
/// dim runs at color-resolution time before the RV1 minimum-contrast floor, so
/// legibility is preserved by construction. `0.0` disables it and is
/// pixel-identical to the pre-feature renderer; higher values dim further. The
/// focused window is never dimmed regardless of this value, so focused frames
/// stay byte-identical to today.
pub const DEFAULT_FOCUS_DIM: f32 = 0.0;
pub const MIN_FOCUS_DIM: f32 = 0.0;
pub const MAX_FOCUS_DIM: f32 = 1.0;

/// Window padding (`ODYTTY_WINDOW_PADDING`): logical pixels of inset on every
/// window edge before the terminal grid begins. `0.0` restores the historical
/// exact edge-to-edge layout; the non-zero default gives text breathing room.
pub const DEFAULT_WINDOW_PADDING_PX: f32 = 8.0;
pub const MIN_WINDOW_PADDING_PX: f32 = 0.0;
pub const MAX_WINDOW_PADDING_PX: f32 = 64.0;

pub const DEFAULT_BLOOM: bool = false;
pub const BLOOM_THRESHOLD_MARGIN: f32 = 0.12;
pub const MIN_BLOOM_THRESHOLD: f32 = 0.70;
pub const MAX_BLOOM_THRESHOLD: f32 = 1.25;
pub const DEFAULT_BLOOM_INTENSITY: f32 = 0.4;
pub const MIN_BLOOM_INTENSITY: f32 = 0.0;
pub const MAX_BLOOM_INTENSITY: f32 = 1.0;
pub const DEFAULT_BLOOM_RADIUS: f32 = 3.0;
pub const MIN_BLOOM_RADIUS: f32 = 0.5;
pub const MAX_BLOOM_RADIUS: f32 = 8.0;

pub fn default_bloom_threshold_for_theme(theme: Theme) -> f32 {
    (crate::theme::relative_luminance(theme.foreground) as f32 + BLOOM_THRESHOLD_MARGIN)
        .clamp(MIN_BLOOM_THRESHOLD, MAX_BLOOM_THRESHOLD)
}

pub const DEFAULT_CRT: bool = false;
pub const DEFAULT_CRT_SCANLINE_INTENSITY: f32 = 0.08;
pub const MIN_CRT_SCANLINE_INTENSITY: f32 = 0.0;
pub const MAX_CRT_SCANLINE_INTENSITY: f32 = 0.18;
pub const DEFAULT_CRT_SCANLINE_PERIOD: f32 = 3.0;
pub const MIN_CRT_SCANLINE_PERIOD: f32 = 2.0;
pub const MAX_CRT_SCANLINE_PERIOD: f32 = 12.0;
pub const DEFAULT_CRT_VIGNETTE_STRENGTH: f32 = 0.10;
pub const MIN_CRT_VIGNETTE_STRENGTH: f32 = 0.0;
pub const MAX_CRT_VIGNETTE_STRENGTH: f32 = 0.16;

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
pub const LINE_HEIGHT_ENV: &str = "ODYTTY_LINE_HEIGHT";
pub const BOX_THICKNESS_ENV: &str = "ODYTTY_BOX_THICKNESS";
pub const KEYBINDS_ENV: &str = "ODYTTY_KEYBINDS";
pub const CURSOR_STYLE_ENV: &str = "ODYTTY_CURSOR_STYLE";
pub const CURSOR_BLINK_ENV: &str = "ODYTTY_CURSOR_BLINK";
pub const CURSOR_EASING_ENV: &str = "ODYTTY_CURSOR_EASING";
pub const CURSOR_MOTION_ENV: &str = "ODYTTY_CURSOR_MOTION";
pub const OSC52_READ_ENV: &str = "ODYTTY_OSC52_READ";
pub const SYNTHETIC_STYLES_ENV: &str = "ODYTTY_SYNTHETIC_STYLES";
pub const GEOMETRIC_BOXDRAW_ENV: &str = "ODYTTY_GEOMETRIC_BOXDRAW";
pub const SYMBOL_FALLBACK_ENV: &str = "ODYTTY_SYMBOL_FALLBACK";
pub const SYMBOL_FONT_ENV: &str = "ODYTTY_SYMBOL_FONT";
pub const THEMED_UI_ROLES_ENV: &str = "ODYTTY_THEMED_UI_ROLES";
pub const SCROLL_WHEEL_LINES_ENV: &str = "ODYTTY_SCROLL_WHEEL_LINES";
pub const SCROLL_DRAG_SPEED_ENV: &str = "ODYTTY_SCROLL_DRAG_SPEED";
pub const COPY_ON_SELECT_ENV: &str = "ODYTTY_COPY_ON_SELECT";
pub const SELECTION_DRAG_EXTEND_ENV: &str = "ODYTTY_SELECTION_DRAG_EXTEND";
pub const SCROLLBAR_DRAG_ENV: &str = "ODYTTY_SCROLLBAR_DRAG";
pub const WHEEL_ZOOM_ENV: &str = "ODYTTY_WHEEL_ZOOM";
pub const COMMAND_STATUS_GUTTER_ENV: &str = "ODYTTY_COMMAND_STATUS_GUTTER";
pub const CVD_MODE_ENV: &str = "ODYTTY_CVD_MODE";
pub const CVD_STRENGTH_ENV: &str = "ODYTTY_CVD_STRENGTH";
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
    LINE_HEIGHT_ENV,
    BOX_THICKNESS_ENV,
    KEYBINDS_ENV,
    CURSOR_STYLE_ENV,
    CURSOR_BLINK_ENV,
    CURSOR_EASING_ENV,
    CURSOR_MOTION_ENV,
    OSC52_READ_ENV,
    SYNTHETIC_STYLES_ENV,
    GEOMETRIC_BOXDRAW_ENV,
    SYMBOL_FALLBACK_ENV,
    SYMBOL_FONT_ENV,
    THEMED_UI_ROLES_ENV,
    SCROLL_WHEEL_LINES_ENV,
    SCROLL_DRAG_SPEED_ENV,
    COPY_ON_SELECT_ENV,
    SELECTION_DRAG_EXTEND_ENV,
    SCROLLBAR_DRAG_ENV,
    WHEEL_ZOOM_ENV,
    CVD_MODE_ENV,
    CVD_STRENGTH_ENV,
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

/// Line-height multiplier (`ODYTTY_LINE_HEIGHT`, LINEHEIGHT): extra vertical
/// leading baked into each glyph cell, expressed as a multiple of the natural
/// font cell height. `1.0` (default) adds zero leading and is byte-identical to
/// the pre-feature atlas: cell height, baseline and glyph coverage are all
/// unchanged. Values above `1.0` grow the cell box and shift the baseline down
/// by the top half of the added rows, so glyphs keep their exact shape and only
/// gain breathing room above and below. The leading is clamped so it can never
/// exceed one extra cell height.
pub const DEFAULT_LINE_HEIGHT: f32 = 1.0;
pub const MIN_LINE_HEIGHT: f32 = 1.0;
pub const MAX_LINE_HEIGHT: f32 = 2.0;

/// Box-drawing stroke-thickness multiplier (`ODYTTY_BOX_THICKNESS`, BOXTHICK):
/// scales the geometric box-drawing / Powerline line weight relative to the
/// DPI-derived default. `1.0` (default) reproduces the historical stroke widths
/// byte-identically (multiplying the light weight by `1.0` is exact in `f32`);
/// values below `1.0` draw thinner rules and above `1.0` draw heavier rules.
/// Only affects the renderer's own geometric box-drawing path — inert when
/// geometric box-drawing is off or when a font supplies the glyphs.
pub const DEFAULT_BOX_THICKNESS: f32 = 1.0;
pub const MIN_BOX_THICKNESS: f32 = 0.5;
pub const MAX_BOX_THICKNESS: f32 = 3.0;

/// Mouse-wheel scroll multiplier (`ODYTTY_SCROLL_WHEEL_LINES`, MOUSE-WHEEL-SPEED):
/// rows of local scrollback advanced per wheel notch. The default `3.0` is
/// byte-identical to the historical hardcoded `WHEEL_STEP_LINES`. Stored as `f32`
/// to ride the shared numeric-setting model (slider / keyboard step / range
/// label); the wheel path rounds it to a `usize >= 1`. Local viewport scroll
/// only — when TUI mouse reporting is active the wheel still reports unchanged,
/// and continuous (touchpad pixel) deltas are never multiplied.
pub const DEFAULT_SCROLL_WHEEL_LINES: f32 = 3.0;
pub const MIN_SCROLL_WHEEL_LINES: f32 = 1.0;
pub const MAX_SCROLL_WHEEL_LINES: f32 = 10.0;

/// Upper bound on the rows the drag-edge autoscroll advances per ~80 ms tick
/// when the velocity ramp is active (`ODYTTY_SCROLL_DRAG_SPEED=ramp`,
/// MOUSE-AUTOSCROLL-VEL). The ramp grows one extra row per cell-height the
/// pointer is dragged past the edge band and is clamped to this cap so it can
/// never scroll uncontrollably fast. The `legacy` mode pins the step to exactly
/// one row per tick, which is byte-identical to the pre-feature behavior.
pub const MAX_AUTOSCROLL_ROWS: usize = 8;

/// Copy-on-select (`ODYTTY_COPY_ON_SELECT`, MOUSE-COPYSELECT): when on, finishing
/// a local selection also writes the CLIPBOARD (in addition to the PRIMARY
/// selection it always writes). Off by default — PRIMARY and middle-click paste
/// already work regardless, so the off path is byte-identical to before.
pub const DEFAULT_COPY_ON_SELECT: bool = false;

/// Cursor blink-fade easing (`ODYTTY_CURSOR_EASING`, ID1): when on, the cursor
/// eases its opacity in and out across the blink toggle instead of hard
/// on/off-switching. Off by default — while off the cursor renders its alpha at
/// a constant `1.0` and the blink off-phase hides the cursor outright, so the
/// render path is byte-identical to before. Purely presentational; never
/// affects cell semantics or the logical cursor position.
pub const DEFAULT_CURSOR_EASING: bool = false;

/// Cursor slide motion (`ODYTTY_CURSOR_MOTION`, VE4): when on, the cursor glides
/// a short sub-cell interpolation between adjacent steady-state positions
/// instead of teleporting. Off by default — while off the cursor sits at its
/// exact cell origin (zero offset) and the render path is byte-identical to
/// before. Discontinuities (first frame, resize/reflow, scrollback, large jump,
/// unfocused) always snap rather than slide. Purely presentational; the logical
/// cursor position is always the destination cell, so selection/clipboard and
/// TUI semantics are unaffected.
pub const DEFAULT_CURSOR_MOTION: bool = false;

/// Drag-to-extend selection (`ODYTTY_SELECTION_DRAG_EXTEND`, MOUSE-EXTEND): when
/// on, a double-click-then-drag grows the selection by whole words, a
/// triple-click-then-drag by whole lines, and Shift+click extends the current
/// selection to the click. On by default (operator decision) — it only gives
/// meaning to gestures that did nothing before. Off restores the historical
/// behavior where a double/triple-click finalizes and the follow-on drag does
/// not extend. Local selection only; never affects TUI mouse reporting.
pub const DEFAULT_SELECTION_DRAG_EXTEND: bool = true;

/// Draggable scroll thumb (`ODYTTY_SCROLLBAR_DRAG`, MOUSE-SCROLLBAR): when on, a
/// left press on the right-edge scroll indicator grabs it as a thumb and the
/// drag scrubs through scrollback. On by default — the thumb only renders while
/// scrolled back into history, so the grab is inert at the live tail and the
/// off path (and the live-tail path) leave press routing byte-identical. Local
/// only; never affects TUI mouse reporting (a press off the thumb still reports
/// as before).
pub const DEFAULT_SCROLLBAR_DRAG: bool = true;

/// Ctrl+wheel font-size zoom (`ODYTTY_WHEEL_ZOOM`, MOUSE-WHEEL): when on,
/// Ctrl+wheel up grows the font and Ctrl+wheel down shrinks it, within
/// [`MIN_FONT_SIZE_PX`]..[`MAX_FONT_SIZE_PX`]. On by default — it only fires on
/// the explicit Ctrl+wheel gesture while mouse reporting is off, so a plain
/// wheel (and the wheel inside a TUI mouse-reporting app) is byte-identical.
/// Off restores Ctrl+wheel to plain scrollback movement.
pub const DEFAULT_WHEEL_ZOOM: bool = true;

/// Per-command success/fail gutter (`ODYTTY_COMMAND_STATUS_GUTTER`, SH2): when
/// on, a thin coloured bar at the left edge of each finished command's prompt
/// row reads green for an explicit `exit 0` and red for a non-zero exit, sourced
/// from the OSC 133 command blocks and coloured from the active ANSI palette.
/// Off by default — while off the gutter draws nothing and the render path is
/// pixel-identical to today. With shell integration absent no command marks
/// exist, so the gutter is empty regardless of the setting.
pub const DEFAULT_COMMAND_STATUS_GUTTER: bool = false;

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

/// Colour-vision-deficiency adaptation strength (`ODYTTY_CVD_STRENGTH`, U4): how
/// strongly the palette is daltonised toward separability for the selected
/// [`crate::settings::CvdMode`]. `1.0` (default) is the full correction; `0.0`
/// is an exact passthrough. Inert while the mode is `off` — the off mode is the
/// primary pixel-identical guarantee, and `0.0` strength is a second net. The
/// adaptation is palette-scope (the 16 ANSI colours plus the cursor/selection/
/// search roles), re-floored to stay readable; app truecolour is not remapped.
pub const DEFAULT_CVD_STRENGTH: f32 = 1.0;
pub const MIN_CVD_STRENGTH: f32 = 0.0;
pub const MAX_CVD_STRENGTH: f32 = 1.0;

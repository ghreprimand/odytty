// SPDX-License-Identifier: GPL-3.0-only
use super::*;

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

/// Structured numeric bounds for a [`SettingKind::Number`] row (UX4-P2): the
/// authoritative `(min, max, step)` the slider widget and keyboard step share,
/// plus an optional display `unit` (e.g. `"px"`). `min`/`max` mirror the same
/// constants the parser clamps to, so the slider, the keyboard step, the derived
/// range label, and the live-apply clamp are one source of truth and cannot
/// drift. `unit` exists only so the derived range label keeps its suffix
/// losslessly; the `{min, max, step}` core is exactly the modeled spec.
///
/// `f32` fields mean this (and therefore [`SettingInfo`]) cannot derive `Eq`;
/// `PartialEq` is sufficient for every consumer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericSpec {
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub unit: &'static str,
}

impl NumericSpec {
    /// Position of `value` along the track as a `0.0..=1.0` fraction (clamped),
    /// used to place the slider thumb. A degenerate `min == max` maps to `0.0`.
    pub fn fraction_of(&self, value: f32) -> f32 {
        let span = self.max - self.min;
        if span.abs() < f32::EPSILON {
            return 0.0;
        }
        ((value - self.min) / span).clamp(0.0, 1.0)
    }

    /// Value at a `0.0..=1.0` track fraction (clamped), snapped to `step` and
    /// clamped back into `[min, max]`. The live-apply parser clamps again, so
    /// this only needs to be approximately in-range.
    pub fn value_at_fraction(&self, fraction: f32) -> f32 {
        let raw = self.min + fraction.clamp(0.0, 1.0) * (self.max - self.min);
        self.snap(raw)
    }

    /// Snap a raw value to the nearest `step` from `min`, clamped to `[min, max]`.
    pub fn snap(&self, value: f32) -> f32 {
        let snapped = if self.step > f32::EPSILON {
            self.min + ((value - self.min) / self.step).round() * self.step
        } else {
            value
        };
        snapped.clamp(self.min, self.max)
    }

    /// Stable character budget reserved for the slider's numeric readout: the
    /// wider of the two bound labels plus room for the `" *"` changed marker, so
    /// the track geometry does not shift as the live value (or its marker)
    /// changes during a drag.
    pub fn readout_width(&self) -> usize {
        let lo = format_float(self.min).chars().count();
        let hi = format_float(self.max).chars().count();
        lo.max(hi) + 2
    }
}

/// Static/dynamic metadata for one settings row in stable display order.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingInfo {
    pub group: &'static str,
    pub key: &'static str,
    pub env: &'static str,
    pub name: &'static str,
    pub value: String,
    pub description: &'static str,
    pub kind: SettingKind,
    /// Human-readable allowed-range hint. For [`SettingKind::Number`] rows with
    /// a [`NumericSpec`] this is derived from the spec (UX4-P2, Q4) so it can
    /// never drift from the clamp bounds; other rows carry a literal hint or
    /// `None`.
    pub range: Option<String>,
    /// Structured numeric bounds for slider/step/clamp (UX4-P2). `Some` for
    /// bounded, live-editable [`SettingKind::Number`] rows; `None` otherwise.
    pub numeric: Option<NumericSpec>,
    pub options: &'static [&'static str],
    pub reloadable: bool,
}

impl Settings {
    /// Stable read-only inventory for the in-app settings panel.
    ///
    /// This intentionally mirrors every field on [`Settings`]. UX2-b can attach
    /// editors and persistence to the same rows; UX2-a only displays them.
    pub fn setting_info(&self) -> Vec<SettingInfo> {
        let mut rows = vec![
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
                numeric: None,
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
                numeric: None,
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
                numeric: None,
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
                numeric: None,
            },
            SettingInfo {
                group: "Font",
                key: "font_size",
                env: FONT_SIZE_ENV,
                name: "Font size",
                value: format_float(self.font_size_px),
                description: "Native font size in pixels. Rebuilds the glyph atlas, cell metrics, terminal grid, and PTY window size.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_FONT_SIZE_PX,
                    max: MAX_FONT_SIZE_PX,
                    step: 1.0,
                    unit: "px",
                }),
            },
            SettingInfo {
                group: "Rendering",
                key: "text_gamma",
                env: TEXT_GAMMA_ENV,
                name: "Text gamma",
                value: format_float(self.text_gamma),
                description: "Glyph coverage gamma applied in the shader for text weight and contrast.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_TEXT_GAMMA,
                    max: MAX_TEXT_GAMMA,
                    step: 0.1,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Rendering",
                key: "stem_darken",
                env: STEM_DARKEN_ENV,
                name: "Stem darkening",
                value: format_float(self.stem_darken),
                description: STEM_DARKEN_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_STEM_DARKEN,
                    max: MAX_STEM_DARKEN,
                    step: 0.05,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Rendering",
                key: "min_contrast",
                env: MIN_CONTRAST_ENV,
                name: "Minimum contrast",
                value: format_float(self.min_contrast),
                description: MIN_CONTRAST_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_MIN_CONTRAST,
                    max: MAX_MIN_CONTRAST,
                    step: 1.0,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Rendering",
                key: "focus_dim",
                env: FOCUS_DIM_ENV,
                name: "Focus dimming",
                value: format_float(self.focus_dim),
                description: FOCUS_DIM_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_FOCUS_DIM,
                    max: MAX_FOCUS_DIM,
                    step: 0.05,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Rendering",
                key: "render_quality",
                env: RENDER_QUALITY_ENV,
                name: "Render quality",
                value: self.render_quality.as_str().to_owned(),
                description: RENDER_QUALITY_DESC,
                kind: SettingKind::Enum,
                range: None,
                options: &["plain", "balanced", "high"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "window_padding",
                env: WINDOW_PADDING_ENV,
                name: "Window padding",
                value: format_float(self.window_padding_px),
                description: WINDOW_PADDING_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_WINDOW_PADDING_PX,
                    max: MAX_WINDOW_PADDING_PX,
                    step: 1.0,
                    unit: "px",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "bloom",
                env: BLOOM_ENV,
                name: "Bloom",
                value: bool_display(self.bloom).to_owned(),
                description: BLOOM_DESC,
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Post-process",
                key: "bloom_threshold",
                env: BLOOM_THRESHOLD_ENV,
                name: "Bloom threshold",
                value: format_float(self.bloom_threshold),
                description: BLOOM_THRESHOLD_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_BLOOM_THRESHOLD,
                    max: MAX_BLOOM_THRESHOLD,
                    step: 0.05,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "bloom_intensity",
                env: BLOOM_INTENSITY_ENV,
                name: "Bloom intensity",
                value: format_float(self.bloom_intensity),
                description: BLOOM_INTENSITY_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_BLOOM_INTENSITY,
                    max: MAX_BLOOM_INTENSITY,
                    step: 0.05,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "bloom_radius",
                env: BLOOM_RADIUS_ENV,
                name: "Bloom radius",
                value: format_float(self.bloom_radius),
                description: BLOOM_RADIUS_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_BLOOM_RADIUS,
                    max: MAX_BLOOM_RADIUS,
                    step: 0.5,
                    unit: "px",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "crt",
                env: CRT_ENV,
                name: "CRT profile",
                value: bool_display(self.crt).to_owned(),
                description: CRT_DESC,
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Post-process",
                key: "crt_scanline_intensity",
                env: CRT_SCANLINE_INTENSITY_ENV,
                name: "CRT scanlines",
                value: format_float(self.crt_scanline_intensity),
                description: CRT_SCANLINE_INTENSITY_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_CRT_SCANLINE_INTENSITY,
                    max: MAX_CRT_SCANLINE_INTENSITY,
                    step: 0.01,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "crt_scanline_period",
                env: CRT_SCANLINE_PERIOD_ENV,
                name: "CRT scanline period",
                value: format_float(self.crt_scanline_period),
                description: CRT_SCANLINE_PERIOD_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_CRT_SCANLINE_PERIOD,
                    max: MAX_CRT_SCANLINE_PERIOD,
                    step: 0.5,
                    unit: "px",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "crt_vignette_strength",
                env: CRT_VIGNETTE_STRENGTH_ENV,
                name: "CRT vignette",
                value: format_float(self.crt_vignette_strength),
                description: CRT_VIGNETTE_STRENGTH_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_CRT_VIGNETTE_STRENGTH,
                    max: MAX_CRT_VIGNETTE_STRENGTH,
                    step: 0.01,
                    unit: "",
                }),
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
                numeric: None,
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
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "geometric_boxdraw",
                env: GEOMETRIC_BOXDRAW_ENV,
                name: "Geometric box-drawing",
                value: bool_display(self.geometric_boxdraw).to_owned(),
                description: GEOMETRIC_BOXDRAW_DESC,
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "symbol_fallback",
                env: SYMBOL_FALLBACK_ENV,
                name: "Symbol fallback",
                value: bool_display(self.symbol_fallback).to_owned(),
                description: SYMBOL_FALLBACK_DESC,
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "symbol_font",
                env: SYMBOL_FONT_ENV,
                name: "Symbol font",
                value: self
                    .symbol_font
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "auto".to_owned()),
                description: SYMBOL_FONT_DESC,
                kind: SettingKind::Path,
                range: None,
                options: &[],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Theme",
                key: "themed_ui_roles",
                env: THEMED_UI_ROLES_ENV,
                name: "Themed UI roles",
                value: bool_display(self.themed_ui_roles).to_owned(),
                description: "Uses theme cursor, selection, and search semantic colors in native UI overlays. Off restores the legacy foreground cursor, inverse selection, and black-on-yellow active search match.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
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
                numeric: None,
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
                numeric: None,
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
                numeric: None,
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
                numeric: None,
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
                range: Some("positive milliseconds".to_owned()),
                options: &[],
                reloadable: false,
                numeric: None,
            },
        ];
        for row in &mut rows {
            if row.range.is_none()
                && let Some(spec) = row.numeric
            {
                row.range = Some(numeric_range_label(spec));
            }
        }
        rows
    }
}

/// Derive the human-readable range hint for a numeric row from its
/// [`NumericSpec`] (UX4-P2, Q4), keeping the optional unit suffix so the
/// display string can never drift from the clamp bounds.
fn numeric_range_label(spec: NumericSpec) -> String {
    let lo = format_bound(spec.min);
    let hi = format_bound(spec.max);
    if spec.unit.is_empty() {
        format!("{lo}..={hi}")
    } else {
        format!("{lo}..={hi} {}", spec.unit)
    }
}

/// Format a numeric bound for the range hint: two decimals, then trailing
/// zeros trimmed while always keeping at least one decimal place (so `6.0`
/// stays `6.0` and `0.18` stays `0.18`).
fn format_bound(value: f32) -> String {
    let mut s = format!("{value:.2}");
    while s.ends_with('0') && !s.ends_with(".0") {
        s.pop();
    }
    s
}

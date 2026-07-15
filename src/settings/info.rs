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
/// authoritative `(min, max, step)` shared by the settings stepper, keyboard
/// step, and any fraction-based test helpers, plus an optional display `unit`
/// (e.g. `"px"`). `min`/`max` mirror the same constants the parser clamps to,
/// so the UI controls, keyboard step, derived range label, and live-apply clamp
/// are one source of truth and cannot drift. `unit` exists only so the derived
/// range label keeps its suffix losslessly; the `{min, max, step}` core is
/// exactly the modeled spec.
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

    /// Stable character budget reserved for the numeric readout: the
    /// wider of the two bound labels plus room for the `" *"` changed marker, so
    /// row geometry does not shift as the live value (or its marker) changes.
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
                // When the `system` alias is active the displayed value is the
                // alias token, matching the config writeback, so the panel
                // reads "system" instead of the internal fallback theme name.
                value: if self.theme_is_system {
                    crate::settings::SYSTEM_THEME_NAME.to_owned()
                } else {
                    self.theme.name.to_owned()
                },
                description: "Full appearance profile: default colors, ANSI palette, semantic role colors, and optional user theme files.",
                kind: SettingKind::Enum,
                range: None,
                options: &[
                    "system",
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
                key: "follow_os_theme",
                env: FOLLOW_OS_THEME_ENV,
                name: "Follow OS dark/light theme",
                value: bool_display(self.follow_os_theme).to_owned(),
                description: "When on, switches between the dark and light themes below based on the desktop's color-scheme preference. The compositor delivers this live on Wayland; on X11 set ODYTTY_APPEARANCE=dark|light to seed it. Off by default — the OS signal is ignored and the theme above drives presentation. A direction whose theme name is unset keeps the authored theme.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Theme",
                key: "os_theme_dark",
                env: OS_THEME_DARK_ENV,
                name: "Dark OS theme",
                value: self
                    .os_theme_dark
                    .clone()
                    .unwrap_or_else(|| "unset".to_owned()),
                description: "Theme applied when Follow OS theme is on and the desktop reports a dark color scheme. Resolved by name against the built-in theme library; unset keeps the authored theme on a dark signal.",
                kind: SettingKind::String,
                range: None,
                options: &[],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Theme",
                key: "os_theme_light",
                env: OS_THEME_LIGHT_ENV,
                name: "Light OS theme",
                value: self
                    .os_theme_light
                    .clone()
                    .unwrap_or_else(|| "unset".to_owned()),
                description: "Theme applied when Follow OS theme is on and the desktop reports a light color scheme. Resolved by name against the built-in theme library; unset keeps the authored theme on a light signal.",
                kind: SettingKind::String,
                range: None,
                options: &[],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Post-process",
                key: "visual",
                env: VISUAL_ENV,
                name: "Ambient visual effect",
                value: self.visual.as_str().to_owned(),
                description: "Ambient scanline look, produced by the unified CRT post-process: ambient turns on CRT scanlines when no explicit crt setting is present, and an explicit crt setting always wins. Requires a GPU adapter with filterable 16-bit float support; falls back to no effect otherwise. Off keeps the renderer plain and is the fastest, most compatible option.",
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
                name: "Font file (advanced)",
                // Display the RAW explicit `font` key, never the effective
                // `font_path` (RC4): picking a `font_family` resolves a regular
                // face INTO `font_path`, but the advanced row must stay empty
                // because the user never set an explicit file. An UNSET explicit
                // font carries an EMPTY value, not a human sentence — this `value`
                // is what the path-picker seeds and what the writeback compares;
                // empty is treated as a clear (apply_raw("font", "")), so an
                // untouched font emits no edit. The default hint lives in the
                // description below, not in the value.
                value: self
                    .explicit_font_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                description: "Advanced: an explicit path to one font file, used instead of the system font lookup. Leave empty (the default: the probed monospace font, or the Font family setting) unless you want to force a specific file. When set, it takes precedence over Font family and falls back safely if the file is missing or unreadable.",
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
                key: "font_weight",
                env: FONT_WEIGHT_ENV,
                name: "Font weight variant",
                value: if self.font_weight.is_empty() {
                    "regular".to_owned()
                } else {
                    self.font_weight.clone()
                },
                description: "Weight-variant suffix for the base face (e.g. Light, Medium, SemiBold). Empty uses the family's regular face. Bold still renders distinctly. Unknown variants fall back to regular with a warning.",
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
                description: "Gamma curve applied to glyph coverage when drawing text. Increasing it makes strokes appear heavier and darker; decreasing it makes them lighter and thinner.",
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
                group: "Panes",
                key: "inactive_pane_dim",
                env: INACTIVE_PANE_DIM_ENV,
                name: "Inactive-pane dimming",
                value: format_float(self.inactive_pane_dim),
                description: INACTIVE_PANE_DIM_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_INACTIVE_PANE_DIM,
                    max: MAX_INACTIVE_PANE_DIM,
                    step: 0.05,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Rendering",
                key: "render_quality",
                env: RENDER_QUALITY_ENV,
                name: "Renderer profile",
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
                group: "Rendering",
                key: "window_border",
                env: WINDOW_BORDER_ENV,
                name: "Window border",
                value: bool_display(self.window_border).to_owned(),
                description: "When on, a thin border in the theme border color frames the terminal grid. Drawn inside the window padding band so it never covers cell text, scaled to the display DPI, and tracks the content on resize. Off by default. Purely visual.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "window_decorations",
                env: WINDOW_DECORATIONS_ENV,
                name: "Window decorations",
                value: bool_display(self.window_decorations).to_owned(),
                description: "When on, the window keeps its title bar and borders; off requests a borderless surface. On by default. Applies at startup and live. Effect depends on the environment: Wayland compositors remove the title bar reliably; X11 window managers honor it on a best-effort basis.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "window_transparency",
                env: WINDOW_TRANSPARENCY_ENV,
                name: "Window transparency",
                value: bool_display(self.window_transparency).to_owned(),
                description: "When on, the terminal background is drawn at the Window opacity below so the desktop shows through; text, cursor, selection, and every overlay (menus, pickers, settings) stay fully opaque. Off by default — the opaque render path is unchanged. Requires a compositing window manager (Wayland natively; X11 needs a compositor; Windows uses DWM). Where no alpha compositing is offered the toggle has no visible effect.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "window_opacity",
                env: WINDOW_OPACITY_ENV,
                name: "Window opacity",
                value: format_float(self.window_opacity),
                description: "Background opacity as a percent when Window transparency is on: 100 is fully opaque, lower values let more of the desktop through behind the terminal. Only the background scales — text and overlays never fade. Adjust with the stepper or arrow keys.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_WINDOW_OPACITY,
                    max: MAX_WINDOW_OPACITY,
                    step: 5.0,
                    unit: "%",
                }),
            },
            SettingInfo {
                group: "Tabs",
                key: "always_show_tab_bar",
                env: ALWAYS_SHOW_TAB_BAR_ENV,
                name: "Always show tab bar",
                value: bool_display(self.always_show_tab_bar).to_owned(),
                description: "When on, the tab bar stays visible even with a single tab. Off by default, so one unnamed tab shows no bar. A single tab you have renamed always shows the bar regardless, so a named workflow tab is never hidden. Applies live.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Workspace rail",
                key: "tab_bar_placement",
                env: TAB_BAR_PLACEMENT_ENV,
                name: "Rail side",
                value: self.tab_bar_placement.rail_side_str().to_owned(),
                description: "Which side of the terminal content the workspace rail sits on: left (default) or right. Tabs always render on the top bar. Applies live.",
                kind: SettingKind::Enum,
                range: None,
                options: &["left", "right"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Workspace rail",
                key: "workspace_rail",
                env: WORKSPACE_RAIL_ENV,
                name: "Rail visibility",
                value: self.workspace_rail.as_str().to_owned(),
                description: "When the workspace rail is shown: auto (default, appears once a second workspace exists) or always (pinned even with a single workspace). The side is set by \"Rail side\". Applies live.",
                kind: SettingKind::Enum,
                range: None,
                options: &["auto", "always"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Tabs",
                key: "tab_bar_height",
                env: TAB_BAR_HEIGHT_ENV,
                name: "Tab bar height",
                value: self.tab_bar_height.as_config_string(),
                description: "Top tab bar height: \"auto\" is one text row, or a fixed row count for a taller band with the labels centered vertically. Adjust with the stepper or arrow keys; stepping below the minimum returns to auto. Drag the tab bar's bottom edge to set a manual height; double-click it to return to auto. Top bar only. Applies live.",
                kind: SettingKind::Number,
                range: Some(format!(
                    "auto or {}-{} rows",
                    MIN_TAB_BAR_ROWS as usize, MAX_TAB_BAR_ROWS as usize
                )),
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_TAB_BAR_ROWS,
                    max: MAX_TAB_BAR_ROWS,
                    step: 1.0,
                    unit: "rows",
                }),
            },
            SettingInfo {
                group: "Workspace rail",
                key: "tab_rail_width",
                env: TAB_RAIL_WIDTH_ENV,
                name: "Rail width",
                value: self.tab_rail_width.as_config_string(),
                description: "Vertical workspace rail width: \"auto\" sizes to the longest workspace name (up to the rail max width), or a fixed cell count. Drag the rail's inner edge to set a manual width; double-click it to return to auto. Rail placements only. Applies live.",
                kind: SettingKind::String,
                range: Some(format!(
                    "auto or {}-{} cells",
                    MIN_TAB_RAIL_WIDTH as usize, MAX_TAB_RAIL_WIDTH as usize
                )),
                options: &[],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Workspace rail",
                key: "tab_rail_max_width",
                env: TAB_RAIL_MAX_WIDTH_ENV,
                name: "Rail max width",
                value: format_float(self.tab_rail_max_width),
                description: "Widest the auto-sized workspace rail grows to fit long workspace names before truncating them with an ellipsis. Only used when the rail width is \"auto\". Rail placements only. Applies live.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_TAB_RAIL_MAX_WIDTH,
                    max: MAX_TAB_RAIL_MAX_WIDTH,
                    step: 1.0,
                    unit: "cells",
                }),
            },
            SettingInfo {
                group: "Workspace rail",
                key: "tab_rail_gap",
                env: TAB_RAIL_GAP_ENV,
                name: "Rail slot gap",
                value: format_float(self.tab_rail_gap),
                description: "Rows of empty space between adjacent workspace rail slots (the top margin before the first slot follows it). Rail placements only. Applies live.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_TAB_RAIL_GAP,
                    max: MAX_TAB_RAIL_GAP,
                    step: 1.0,
                    unit: "rows",
                }),
            },
            SettingInfo {
                group: "Workspace rail",
                key: "tab_rail_slot_rows",
                env: TAB_RAIL_SLOT_ROWS_ENV,
                name: "Rail slot height",
                value: format_float(self.tab_rail_slot_rows),
                description: "Rows each workspace rail slot occupies: 1 for a compact single-row list (labels truncate), 2 for the padded default that can wrap a long workspace name across two rows. Rail placements only. Applies live.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_TAB_RAIL_SLOT_ROWS,
                    max: MAX_TAB_RAIL_SLOT_ROWS,
                    step: 1.0,
                    unit: "rows",
                }),
            },
            SettingInfo {
                group: "Panel",
                key: "tab_panel_strength",
                env: TAB_PANEL_STRENGTH_ENV,
                name: "Tab panel strength",
                value: format_float(self.tab_panel_strength),
                description: "Strength of the translucent panel behind the tab bar/rail. 0 turns the panel off (bare labels over the wallpaper); higher values mute the wallpaper more so tab labels read against a quiet surface. Both placements. Applies live.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_TAB_PANEL_STRENGTH,
                    max: MAX_TAB_PANEL_STRENGTH,
                    step: 0.1,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Panel",
                key: "tab_seam",
                env: TAB_SEAM_ENV,
                name: "Tab panel seam",
                value: bool_display(self.tab_seam).to_owned(),
                description: "When on, a thin hairline separates the tab panel from the terminal content. Off removes the line only; the panel stays. Both placements. Applies live.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Workspace rail",
                key: "tab_rail_autohide",
                env: TAB_RAIL_AUTOHIDE_ENV,
                name: "Rail auto-hide",
                value: bool_display(self.tab_rail_autohide).to_owned(),
                description: "When on, the vertical workspace rail stays hidden until the pointer reaches the window edge, then reveals as a floating overlay (no content reflow). Rail placements only — no effect when tabs are placed on top; use \"Always show tab bar\" for the top bar. Off by default.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Workspace rail",
                key: "tab_rail_reveal_px",
                env: TAB_RAIL_REVEAL_PX_ENV,
                name: "Rail reveal zone",
                value: format_float(self.tab_rail_reveal_px),
                description: "Width in logical pixels of the window-edge zone that reveals an auto-hidden workspace rail on hover (scaled for HiDPI displays). Rail placements only.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_TAB_RAIL_REVEAL_PX,
                    max: MAX_TAB_RAIL_REVEAL_PX,
                    step: 1.0,
                    unit: "px",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "retro",
                env: RETRO_ENV,
                name: "Retro preset",
                value: bool_display(self.retro).to_owned(),
                description: RETRO_DESC,
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
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
                name: "CRT scanline spacing",
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
                group: "Post-process",
                key: "crt_curvature",
                env: CRT_CURVATURE_ENV,
                name: "CRT curvature",
                value: format_float(self.crt_curvature),
                description: CRT_CURVATURE_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_CRT_CURVATURE,
                    max: MAX_CRT_CURVATURE,
                    step: 0.005,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "background_treatment",
                env: BACKGROUND_TREATMENT_ENV,
                name: "Background treatment",
                value: self.background_treatment.as_str().to_owned(),
                description: BACKGROUND_TREATMENT_DESC,
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "gradient", "vignette", "image"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Post-process",
                key: "background_image",
                env: BACKGROUND_IMAGE_ENV,
                name: "Background image",
                value: self
                    .background_image
                    .as_ref()
                    .map(|path| {
                        if crate::settings::is_bundled_background(path) {
                            format!("{} (bundled)", crate::settings::BUNDLED_BACKGROUND_TOKEN)
                        } else {
                            path.display().to_string()
                        }
                    })
                    .unwrap_or_else(|| "none".to_owned()),
                description: BACKGROUND_IMAGE_DESC,
                kind: SettingKind::Path,
                range: None,
                options: &[],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Post-process",
                key: "cell_bg_opacity",
                env: CELL_BG_OPACITY_ENV,
                name: "Wallpaper visibility",
                value: format_float(1.0 - self.cell_bg_opacity),
                description: CELL_BG_OPACITY_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_CELL_BG_OPACITY,
                    max: MAX_CELL_BG_OPACITY,
                    step: 0.05,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "background_blur_radius",
                env: BACKGROUND_BLUR_RADIUS_ENV,
                name: "Background blur radius",
                value: self.background_blur_radius.to_string(),
                description: BACKGROUND_BLUR_RADIUS_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: 0.0,
                    max: MAX_BACKGROUND_BLUR_RADIUS as f32,
                    step: 1.0,
                    unit: "px",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "background_image_scrim",
                env: BACKGROUND_IMAGE_SCRIM_ENV,
                name: "Wallpaper readability",
                value: self
                    .background_image_scrim
                    .map(format_float)
                    .unwrap_or_else(|| "auto".to_owned()),
                description: BACKGROUND_IMAGE_SCRIM_DESC,
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_BACKGROUND_IMAGE_SCRIM,
                    max: MAX_BACKGROUND_IMAGE_SCRIM,
                    step: 0.05,
                    unit: "",
                }),
            },
            SettingInfo {
                group: "Post-process",
                key: "new_output_fade",
                env: NEW_OUTPUT_FADE_ENV,
                name: "New-output fade-in",
                value: bool_display(self.new_output_fade).to_owned(),
                description: "When on, rows of freshly arrived output fade in over a short ramp at the live tail instead of appearing instantly. The fade obscures then reveals each new row, so the text underneath is always fully rendered and stays readable. Off by default; only at the live tail — scrollback and resize snap. Purely visual.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Rendering",
                key: "subpixel",
                env: SUBPIXEL_ENV,
                name: "Subpixel antialiasing",
                value: subpixel_display(self.subpixel).to_owned(),
                description: "Optional RGB or BGR subpixel antialiasing for crisper text on most LCD displays. Off by default. Unsupported adapters fall back to grayscale antialiasing.",
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "rgb", "bgr"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Font",
                key: "line_height",
                env: LINE_HEIGHT_ENV,
                name: "Line height",
                value: format_float(self.line_height),
                description: "Vertical leading multiplier per text row. 1.0 is the natural cell height; higher values add breathing room above and below each line.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_LINE_HEIGHT,
                    max: MAX_LINE_HEIGHT,
                    step: 0.05,
                    unit: "x",
                }),
            },
            SettingInfo {
                group: "Rendering",
                key: "synthetic_styles",
                env: SYNTHETIC_STYLES_ENV,
                name: "Synthesize bold & italic",
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
                key: "box_thickness",
                env: BOX_THICKNESS_ENV,
                name: "Box-drawing thickness",
                value: format_float(self.box_thickness),
                description: "Scales the geometric box-drawing line weight. 1.0 keeps the default stroke; lower is thinner, higher is heavier.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_BOX_THICKNESS,
                    max: MAX_BOX_THICKNESS,
                    step: 0.1,
                    unit: "x",
                }),
            },
            SettingInfo {
                group: "Font",
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
                group: "Font",
                key: "symbol_font",
                env: SYMBOL_FONT_ENV,
                name: "Symbol font file",
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
                group: "Font",
                key: "symbol_map",
                env: SYMBOL_MAP_ENV,
                name: "Symbol map",
                value: if self.symbol_map.is_empty() {
                    "none".to_owned()
                } else {
                    super::format_symbol_map(&self.symbol_map)
                },
                description: "Per-range font override: route Unicode codepoint ranges to named font families (e.g. U+E000-U+F8FF=Symbols Nerd Font; U+2500-U+257F=Fira Code). Semicolon-separated; first match wins. Leaves the body font untouched.",
                kind: SettingKind::String,
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
                description: "Default cursor shape shown at startup and after a terminal reset. Running applications may send their own cursor shape requests to override this.",
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
                description: "Default cursor blink policy at startup. on blinks at a fixed rate; off keeps the cursor steady. auto uses the conventional blinking default (Linux exposes no OS caret-blink preference). An app's DECSCUSR can override any of these at runtime.",
                kind: SettingKind::Enum,
                range: None,
                options: &["auto", "on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Cursor",
                key: "cursor_easing",
                env: CURSOR_EASING_ENV,
                name: "Cursor blink fade",
                value: bool_display(self.cursor_easing).to_owned(),
                description: "When on, the cursor eases its opacity in and out across each blink instead of switching hard on and off. On by default; only acts while the cursor is blinking and the window is focused. Purely visual.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Cursor",
                key: "cursor_glow",
                env: CURSOR_GLOW_ENV,
                name: "Cursor glow",
                value: bool_display(self.cursor_glow).to_owned(),
                description: "When on, the cursor gets a soft halo of three faint concentric rings in the theme foreground color, drawn behind the cursor block. Off by default; the halo is faint enough to keep nearby text readable. Purely visual — never moves the logical cursor.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Cursor",
                key: "cursor_trail",
                env: CURSOR_TRAIL_ENV,
                name: "Cursor trail",
                value: bool_display(self.cursor_trail).to_owned(),
                description: "When on, a short fading after-image trails the cursor as it glides between cells, in the theme cursor color and drawn behind the cursor block. On by default, but only visible while Cursor slide is also on (it trails that motion) and fully decays as the glide settles — Cursor slide is off by default, so enable it to see the trail. Purely visual — never moves the logical cursor.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Cursor",
                key: "cursor_motion",
                env: CURSOR_MOTION_ENV,
                name: "Cursor slide",
                value: bool_display(self.cursor_motion).to_owned(),
                description: "When on, the cursor glides a short distance between adjacent positions instead of jumping. Off by default; large jumps, resizes, scrollback and the first frame snap instantly. Purely visual — the logical cursor is always at the destination cell.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "keybinds",
                env: KEYBINDS_ENV,
                name: "Key bindings",
                value: key_bindings_display(&self.key_bindings),
                description: "Terminal-local shortcut overrides for search, settings, theme picker, command palette, copy, paste, and scrollback actions. PTY key encoding is unchanged unless an override captures that chord.",
                kind: SettingKind::List,
                range: None,
                // D-KBR-2: every BindableAction display name, in the exact
                // order of `BindableAction::ALL` (the same order the in-app
                // key-remap editor lists). Display only — the parser already
                // accepts every action. Pinned to `ALL` by
                // `keybinds_info_options_lists_all_actions` so a new variant
                // fails that test until its token is added here.
                options: &[
                    "search",
                    "settings",
                    "theme-picker",
                    "copy",
                    "paste",
                    "scroll-up",
                    "scroll-down",
                    "jump-prompt-prev",
                    "jump-prompt-next",
                    "copy-mode",
                    "hints",
                    "clear-input",
                    "command-palette",
                    "connection-manager",
                    "session-replay",
                    "theme-builder",
                    "session-attach",
                    "new-tab",
                    "new-window",
                    "next-tab",
                    "prev-tab",
                    "close-tab",
                    "duplicate-tab",
                    "new-workspace",
                    "duplicate-workspace",
                    "close-workspace",
                    "rename-workspace",
                    "next-workspace",
                    "prev-workspace",
                    "workspace-picker",
                    "split-columns",
                    "split-rows",
                    "focus-pane-left",
                    "focus-pane-right",
                    "focus-pane-up",
                    "focus-pane-down",
                    "focus-pane-next",
                    "close-pane",
                    "zoom-pane",
                    "equalize-panes",
                ],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Panes",
                key: "pane_prefix",
                env: PANE_PREFIX_ENV,
                name: "Pane prefix",
                value: pane_prefix_display(self.pane_prefix),
                description: "Multiplexer prefix chord (default ctrl+b) that opens the transient pane-command mode: prefix then % or \" to split, arrows or o to focus, x to close, z to zoom, space or = to equalize. Press the prefix twice to send a literal prefix to the focused pane (for a nested multiplexer). Set to off to disable.",
                kind: SettingKind::String,
                range: None,
                options: &[],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "scroll_wheel_lines",
                env: SCROLL_WHEEL_LINES_ENV,
                name: "Scroll wheel speed",
                value: format_float(self.scroll_wheel_lines),
                description: "Rows of scrollback the mouse wheel advances per notch. Affects local scrolling only; when a full-screen program captures the mouse, the wheel still reports to it unchanged.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_SCROLL_WHEEL_LINES,
                    max: MAX_SCROLL_WHEEL_LINES,
                    step: 1.0,
                    unit: "lines",
                }),
            },
            SettingInfo {
                group: "Input",
                key: "scrollback_lines",
                env: SCROLLBACK_LINES_ENV,
                name: "Scrollback limit",
                value: format_float(self.scrollback_lines),
                description: "Maximum lines of history kept in scrollback before the oldest are dropped. Caps memory use so a program printing endless output cannot exhaust RAM. Set to 0 for unlimited history (use with care).",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_SCROLLBACK_LINES,
                    max: MAX_SCROLLBACK_LINES,
                    step: 1000.0,
                    unit: "lines",
                }),
            },
            SettingInfo {
                group: "Input",
                key: "selection_drag_extend",
                env: SELECTION_DRAG_EXTEND_ENV,
                name: "Drag to extend selection",
                value: bool_display(self.selection_drag_extend).to_owned(),
                description: "When on, double-click then drag extends the selection by whole words, triple-click then drag by whole lines, and Shift+click extends the current selection. On by default; turn off to restore click-to-finish selection.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "scroll_drag_speed",
                env: SCROLL_DRAG_SPEED_ENV,
                name: "Drag autoscroll speed",
                value: self.scroll_drag_speed.as_str().to_owned(),
                description: SCROLL_DRAG_SPEED_DESC,
                kind: SettingKind::Enum,
                range: None,
                options: &["ramp", "legacy"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "pixel_scroll",
                env: PIXEL_SCROLL_ENV,
                name: "Pixel-precise scrolling",
                value: bool_display(self.pixel_scroll).to_owned(),
                description: "When on, high-resolution wheels and touchpads scroll the viewport by a continuous sub-row amount that tracks physical finger travel, instead of quantizing to whole notches. On by default; affects only pixel-precise input, so classic detented wheels are unchanged.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "scroll_pixel_speed",
                env: SCROLL_PIXEL_SPEED_ENV,
                name: "Pixel scroll speed",
                value: format_float(self.scroll_pixel_speed),
                description: "Sensitivity multiplier for pixel-precise (touchpad / hi-res wheel) scrolling. 1.0 tracks finger travel exactly; higher scrolls faster than the finger, lower slower. Applies only to pixel-precise input, never to detented wheels (see Scroll wheel speed for those).",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_SCROLL_PIXEL_SPEED,
                    max: MAX_SCROLL_PIXEL_SPEED,
                    step: 0.25,
                    unit: "x",
                }),
            },
            SettingInfo {
                group: "Input",
                key: "scroll_glide",
                env: SCROLL_GLIDE_ENV,
                name: "Animated scroll glide",
                value: bool_display(self.scroll_glide).to_owned(),
                description: "When on, a wheel notch still moves the viewport instantly, but the rendered view eases toward the new position over a few frames for a smoother glide. On by default; affects only detented wheels (high-resolution wheels and touchpads use Pixel-precise scrolling instead).",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "scrollbar_drag",
                env: SCROLLBAR_DRAG_ENV,
                name: "Draggable scrollbar",
                value: bool_display(self.scrollbar_drag).to_owned(),
                description: "When on, grab the right-edge scroll indicator and drag to scrub through scrollback. The thumb only appears while scrolled back into history. On by default; turn off to keep the indicator display-only.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "wheel_zoom",
                env: WHEEL_ZOOM_ENV,
                name: "Ctrl+wheel font zoom",
                value: bool_display(self.wheel_zoom).to_owned(),
                description: "When on, hold Ctrl and scroll the wheel to grow or shrink the font size. On by default; it only acts while a full-screen program is not capturing the mouse, so the plain wheel still scrolls. Turn off to use Ctrl+wheel for scrollback instead.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "command_status_gutter",
                env: COMMAND_STATUS_GUTTER_ENV,
                name: "Command status gutter",
                value: bool_display(self.command_status_gutter).to_owned(),
                description: "When on, draws a thin colored bar in the left margin beside each finished command: green for success, red for a non-zero exit. Requires a shell that emits command-boundary marks. Off by default; the plain left margin stays empty.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "sh_click",
                env: SH_CLICK_ENV,
                name: "Click to position cursor",
                value: bool_display(self.sh_click).to_owned(),
                description: "When on, a plain left click in the typed command line moves the input cursor to the clicked spot, including across soft-wrapped lines. Requires a shell that advertises click support via its command-boundary marks; a drag still selects and Shift+click still extends a selection. On by default; without an integrated shell it does nothing.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "shell_integration",
                env: SHELL_INTEGRATION_ENV,
                name: "Shell integration",
                value: bool_display(self.shell_integration).to_owned(),
                description: "When on, new local shell tabs receive OdyTTY's OSC 133 prompt hooks at spawn. This enables prompt-aware editing and navigation without modifying shell rc files. Off by default; existing shells are unchanged until restarted.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "confirm_close",
                env: CONFIRM_CLOSE_ENV,
                name: "Confirm close when running",
                value: bool_display(self.confirm_close).to_owned(),
                description: "When on, asks for confirmation before closing the window if a program is still running in the terminal. On by default — the prompt only appears when a foreground job is active, so closing an idle shell still exits immediately. Off closes on request unconditionally.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "bell",
                env: BELL_ENV,
                name: "Bell",
                value: self.bell.as_str().to_owned(),
                description: "How the terminal reacts when a program rings the bell (BEL). Off ignores it. Visual flashes the screen briefly. Urgent (default) requests window attention when unfocused without flashing. All does both. There is no audible bell.",
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "visual", "urgent", "all"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "interactive_urls",
                env: INTERACTIVE_URLS_ENV,
                name: "Clickable URLs",
                value: bool_display(self.interactive_urls).to_owned(),
                description: "On by default. When on, a bare http(s):// (or other allowlisted-scheme) URL that a program printed gets a hand cursor on hover and a Ctrl+hover underline; Ctrl+click opens it in your browser. URLs are never auto-opened, never run through a shell, and only allowlisted schemes (http, https, file, mailto) open. Detection is local-only and scans only the hovered row of the focused pane. Off makes the URL scan never run (byte-identical hover). Independent of Interactive file paths; explicit OSC 8 hyperlinks are handled separately.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "interactive_paths",
                env: INTERACTIVE_PATHS_ENV,
                name: "Interactive file paths",
                value: bool_display(self.interactive_paths).to_owned(),
                description: "Off by default, so the pointer path never scans terminal text and the plain hover path is byte-identical. When on, hovering a path-looking span that resolves to a real file or directory shows the pointer (hand) cursor. Detection is local-only (nothing is logged, persisted, or sent) and the single filesystem check happens only on a hovered candidate. Hover works on the focused pane only.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "interactive_paths_barewords",
                env: INTERACTIVE_PATHS_BAREWORDS_ENV,
                name: "Bare filename paths",
                value: bool_display(self.interactive_paths_barewords).to_owned(),
                description: "When interactive file paths are on, also consider basename-like tokens with extensions (for example carpet1.jpg) as path candidates. On by default behind the global interactive_paths gate; candidates still resolve through the filesystem check against the pane cwd, so non-existent words stay inert.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "interactive_paths_click_hint",
                env: INTERACTIVE_PATHS_CLICK_HINT_ENV,
                name: "Click-to-open hint",
                value: bool_display(self.interactive_paths_click_hint).to_owned(),
                description: "When interactive file paths are on, show a transient bottom-left \"Ctrl+click to open\" hint after two plain mis-clicks on a resolved path land within a short window. On by default behind the global interactive_paths gate. Off silences only the hint; the hand cursor, the Ctrl+hover underline, and Ctrl+click open all still work.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "interactive_paths_image_inline",
                env: INTERACTIVE_PATHS_IMAGE_INLINE_ENV,
                name: "Inline image paths",
                value: bool_display(self.interactive_paths_image_inline).to_owned(),
                description: "When interactive file paths are on, Ctrl+clicking a resolved image path opens the in-OdyTTY viewer by default. On by default behind the global interactive_paths gate. Off restores the external opener for images; the right-click Open in OdyTTY action remains available.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Input",
                key: "interactive_paths_editor",
                env: INTERACTIVE_PATHS_EDITOR_ENV,
                name: "Interactive path editor",
                value: if self.interactive_paths_editor.is_empty() {
                    "default".to_owned()
                } else {
                    self.interactive_paths_editor.clone()
                },
                description: "Editor used to open a path with a line/column suffix (path:line:col). Empty (default) detects the editor from $EDITOR/$VISUAL. Set a known editor name (vim, nvim, code, emacs, helix, sublime, nano, micro) or an argv template with {file}, {line}, {col} placeholders. Always passed as an argv vector, never run through a shell.",
                kind: SettingKind::String,
                range: None,
                options: &[],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Connections",
                key: "ssh_config_hosts",
                env: SSH_CONFIG_HOSTS_ENV,
                name: "Import OpenSSH host names",
                value: bool_display(self.ssh_config_hosts).to_owned(),
                description: "Opt-in source for the future connection manager. Off by default, so OdyTTY never reads OpenSSH config. When on, OdyTTY reads the caller-resolved ~/.ssh/config path read-only and name-only through a bounded parser; key directives and credentials are not surfaced.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Connections",
                key: "remote_integration",
                env: REMOTE_INTEGRATION_ENV,
                name: "Remote shell integration",
                value: bool_display(self.remote_integration).to_owned(),
                description: "Inject OdyTTY's OSC 133 shell integration on SSH tabs so a remote bash session gains the same prompt and input boundaries as a local one. Nothing is persisted on the remote; any failure or a non-bash remote shell degrades to a plain ssh session. On by default. A per-host 'Integration off' in hosts.conf opts a single host out; off globally makes every SSH tab's launch byte-identical to a plain ssh.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Connections",
                key: "remote_reuse",
                env: REMOTE_REUSE_ENV,
                name: "Reuse SSH connections",
                value: bool_display(self.remote_reuse).to_owned(),
                description: "Multiplex integrated SSH tabs over a shared connection with ControlMaster/ControlPersist and an OdyTTY-owned control socket, so a second tab to the same host connects with no new handshake. On by default; if the shared master is gone the tab degrades to a normal fresh connect. A per-host 'Reuse off' in hosts.conf opts a single host out. No effect on a Windows client, where OpenSSH has no connection multiplexing.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Connections",
                key: "remote_tmux",
                env: REMOTE_TMUX_ENV,
                name: "Persist SSH sessions with tmux",
                value: bool_display(self.remote_tmux).to_owned(),
                description: "Wrap an integrated SSH tab's remote shell in a persistent tmux session ('tmux new-session -A -s odytty'), so a dropped-and-reconnected link reattaches the same remote session with its running programs and scrollback intact. Off by default; the remote shell degrades to plain bash when the remote has no tmux, so enabling it never breaks a session. A per-host 'Tmux on'/'Tmux off' in hosts.conf overrides for a single host. Only takes effect with remote integration on.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Connections",
                key: "remote_persist",
                env: REMOTE_PERSIST_ENV,
                name: "Keep SSH connection warm",
                value: self.remote_persist.as_str().to_owned(),
                description: "How long a reused SSH master connection stays authenticated after the last tab to a host closes, so a daily-driver host is authenticated roughly once per boot rather than once per tab. Default 10m keeps the historical 10-minute window (behavior unchanged); off tears the master down with its last connection. A per-host 'Persist' line in hosts.conf overrides this and accepts any ssh ControlPersist value. Only takes effect with connection reuse on; no effect on a Windows client, where OpenSSH has no connection multiplexing.",
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "10m", "30m", "1h", "2h"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Connections",
                key: "remote_image_paste",
                env: REMOTE_IMAGE_PASTE_ENV,
                name: "Paste clipboard images to remote",
                value: self.remote_image_paste.as_str().to_owned(),
                description: "Offer to upload a clipboard image pasted into a remote integrated SSH tab. 'ask' (default) prompts before every upload, showing the encoded size and the target host, so image bytes never leave the machine on a keystroke without confirmation; 'off' disables it, so an image paste there does nothing. Only engages on a remote integrated tab; a local or plain-ssh tab's paste is unaffected. Uploads land 0600 in the remote /tmp under an unguessable name and are cleaned up best-effort on tab close.",
                kind: SettingKind::Enum,
                range: None,
                options: &["ask", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Sessions",
                key: "session_replay",
                env: SESSION_REPLAY_ENV,
                name: "Record output for replay",
                value: bool_display(self.session_replay).to_owned(),
                description: "Opt-in per-session output recording for the scrubbable replay overlay. Off by default, so the PTY pump records nothing and the plain path is byte-identical. When on, each session keeps a bounded in-memory ring of recent screen frames the replay overlay can scrub. Recording is local-only: frames never leave memory (no disk, no network).",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Sessions",
                key: "restore_workspaces",
                env: RESTORE_WORKSPACES_ENV,
                name: "Restore workspaces at launch",
                value: bool_display(self.restore_workspaces).to_owned(),
                description: "When on, launching odytty with no arguments reopens the previous workspace/tab/pane layout \u{2014} workspace names, tab titles and order, and each pane's split tree at its captured directory, each with a fresh shell. Never restores terminal output, scrollback, or commands (shape only). Off by default; any command-line argument starts fresh for that launch. The layout autosave runs regardless, so a snapshot is ready the moment this is turned on.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Sessions",
                key: "shell_exit_closes",
                env: SHELL_EXIT_CLOSES_ENV,
                name: "Typing exit closes",
                value: self.shell_exit_closes.as_str().to_owned(),
                description: "What typing exit (or Ctrl-D on a live shell) does when it would close a whole workspace. Workspace (default) closes just that workspace, exactly as before; closing the last workspace still quits. Application quits OdyTTY instead whenever a shell exit would close a workspace, even if other workspaces are open \u{2014} it pairs with Restore workspaces so the same set reopens next launch. Either way the rail close button and the close-tab / close-workspace / close-pane keybinds still close a single surface, and exiting a shell that has sibling tabs or panes still closes only that tab or pane.",
                kind: SettingKind::Enum,
                range: None,
                options: &["workspace", "app"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Clipboard",
                key: "osc52_read",
                env: OSC52_READ_ENV,
                name: "Allow clipboard read (OSC 52)",
                value: bool_display(self.osc52_read).to_owned(),
                description: "Allows terminal applications to query local clipboard contents through OSC 52 replies. Off by default for safety; clipboard writes are separate.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Clipboard",
                key: "copy_on_select",
                env: COPY_ON_SELECT_ENV,
                name: "Copy selection to clipboard",
                value: bool_display(self.copy_on_select).to_owned(),
                description: "When on, finishing a mouse selection also copies it to the clipboard. Off by default — the primary selection and middle-click paste work either way.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Clipboard",
                key: "smart_ctrl_c",
                env: SMART_CTRL_C_ENV,
                name: "Smart Ctrl+C (copy or interrupt)",
                value: self.smart_ctrl_c.as_str().to_owned(),
                description: "What plain Ctrl+C does. Copy-or-interrupt (default): when text is selected, Ctrl+C copies it and clears the selection; with nothing selected, Ctrl+C still sends the interrupt. Off always sends the interrupt signal (^C), as a terminal normally does. To still interrupt while text is selected, press Esc first (or Ctrl+C twice). Ctrl+Shift+V pastes, and Ctrl+Shift+C always copies, regardless of this setting.",
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "copy-or-interrupt"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Accessibility",
                key: "reduced_motion",
                env: REDUCED_MOTION_ENV,
                name: "Reduce motion",
                value: bool_display(self.reduced_motion).to_owned(),
                description: "When on, cursor slide, trail, glow, blink fade, and new-output fade use static or instant behavior. Their individual settings remain saved unchanged. This explicit setting behaves the same on Windows, macOS, and Linux; OS reduced-motion preference discovery is not yet available.",
                kind: SettingKind::Bool,
                range: None,
                options: &["on", "off"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Accessibility",
                key: "cvd_mode",
                env: CVD_MODE_ENV,
                name: "Color-blind adaptation",
                value: self.cvd_mode.as_str().to_owned(),
                description: "Adapts the palette so colors that a color-vision deficiency confuses become distinguishable, while staying readable. Off by default. Protan and deutan target red-green confusion, tritan targets blue-yellow.",
                kind: SettingKind::Enum,
                range: None,
                options: &["off", "protan", "deutan", "tritan"],
                reloadable: true,
                numeric: None,
            },
            SettingInfo {
                group: "Accessibility",
                key: "cvd_strength",
                env: CVD_STRENGTH_ENV,
                name: "Color-blind adaptation strength",
                value: format_float(self.cvd_strength),
                description: "How strongly the palette is shifted toward separability for the selected color-blind mode. 1.0 is the full correction, 0.0 is no change. Has no effect while the mode is off.",
                kind: SettingKind::Number,
                range: None,
                options: &[],
                reloadable: true,
                numeric: Some(NumericSpec {
                    min: MIN_CVD_STRENGTH,
                    max: MAX_CVD_STRENGTH,
                    step: 0.1,
                    unit: "",
                }),
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
        rows.sort_by_key(|row| setting_group_rank(row.group));
        for row in &mut rows {
            if row.range.is_none()
                && let Some(spec) = row.numeric
            {
                row.range = Some(numeric_range_label(spec));
            }
        }
        rows
    }

    /// Return only the human-readable `value` string for a single setting key,
    /// mirroring the per-field derivation in [`Self::setting_info`]. Used by the
    /// settings panel to update one row in place after a live edit instead of
    /// rebuilding the full [`SettingInfo`] table.
    /// Returns `None` for an unknown key so callers can fall back to a full
    /// rebuild if the inventory changes shape.
    pub fn display_value_for_key(&self, key: &str) -> Option<String> {
        let value = match key {
            "theme" => self.theme.name.to_owned(),
            "follow_os_theme" => bool_display(self.follow_os_theme).to_owned(),
            "os_theme_dark" => self
                .os_theme_dark
                .clone()
                .unwrap_or_else(|| "unset".to_owned()),
            "os_theme_light" => self
                .os_theme_light
                .clone()
                .unwrap_or_else(|| "unset".to_owned()),
            "visual" => self.visual.as_str().to_owned(),
            "font" => self
                .explicit_font_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            "font_family" => self
                .font_family
                .clone()
                .unwrap_or_else(|| "unset".to_owned()),
            "font_weight" => {
                if self.font_weight.is_empty() {
                    "regular".to_owned()
                } else {
                    self.font_weight.clone()
                }
            }
            "font_size" => format_float(self.font_size_px),
            "text_gamma" => format_float(self.text_gamma),
            "stem_darken" => format_float(self.stem_darken),
            "min_contrast" => format_float(self.min_contrast),
            "focus_dim" => format_float(self.focus_dim),
            "inactive_pane_dim" => format_float(self.inactive_pane_dim),
            "render_quality" => self.render_quality.as_str().to_owned(),
            "window_padding" => format_float(self.window_padding_px),
            "window_border" => bool_display(self.window_border).to_owned(),
            "window_decorations" => bool_display(self.window_decorations).to_owned(),
            "window_transparency" => bool_display(self.window_transparency).to_owned(),
            "window_opacity" => format_float(self.window_opacity),
            "retro" => bool_display(self.retro).to_owned(),
            "crt" => bool_display(self.crt).to_owned(),
            "bloom" => bool_display(self.bloom).to_owned(),
            "bloom_threshold" => format_float(self.bloom_threshold),
            "bloom_intensity" => format_float(self.bloom_intensity),
            "bloom_radius" => format_float(self.bloom_radius),
            "crt_scanline_intensity" => format_float(self.crt_scanline_intensity),
            "crt_scanline_period" => format_float(self.crt_scanline_period),
            "crt_vignette_strength" => format_float(self.crt_vignette_strength),
            "crt_curvature" => format_float(self.crt_curvature),
            "background_treatment" => self.background_treatment.as_str().to_owned(),
            "background_image" => self
                .background_image
                .as_ref()
                .map(|path| {
                    if crate::settings::is_bundled_background(path) {
                        format!("{} (bundled)", crate::settings::BUNDLED_BACKGROUND_TOKEN)
                    } else {
                        path.display().to_string()
                    }
                })
                .unwrap_or_else(|| "none".to_owned()),
            "cell_bg_opacity" => format_float(1.0 - self.cell_bg_opacity),
            "background_blur_radius" => self.background_blur_radius.to_string(),
            "background_image_scrim" => self
                .background_image_scrim
                .map(format_float)
                .unwrap_or_else(|| "auto".to_owned()),
            "new_output_fade" => bool_display(self.new_output_fade).to_owned(),
            "reduced_motion" => bool_display(self.reduced_motion).to_owned(),
            "subpixel" => subpixel_display(self.subpixel).to_owned(),
            "line_height" => format_float(self.line_height),
            "synthetic_styles" => bool_display(self.synthetic_styles).to_owned(),
            "geometric_boxdraw" => bool_display(self.geometric_boxdraw).to_owned(),
            "box_thickness" => format_float(self.box_thickness),
            "symbol_fallback" => bool_display(self.symbol_fallback).to_owned(),
            "symbol_font" => self
                .symbol_font
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "auto".to_owned()),
            "symbol_map" => {
                if self.symbol_map.is_empty() {
                    "none".to_owned()
                } else {
                    super::format_symbol_map(&self.symbol_map)
                }
            }
            "themed_ui_roles" => bool_display(self.themed_ui_roles).to_owned(),
            "cursor_style" => cursor_style_display(self.cursor_style).to_owned(),
            "cursor_blink" => self.cursor_blink.as_str().to_owned(),
            "cursor_easing" => bool_display(self.cursor_easing).to_owned(),
            "cursor_glow" => bool_display(self.cursor_glow).to_owned(),
            "cursor_trail" => bool_display(self.cursor_trail).to_owned(),
            "cursor_motion" => bool_display(self.cursor_motion).to_owned(),
            "keybinds" => key_bindings_display(&self.key_bindings),
            "pane_prefix" => pane_prefix_display(self.pane_prefix),
            "scroll_wheel_lines" => format_float(self.scroll_wheel_lines),
            "scrollback_lines" => format_float(self.scrollback_lines),
            "selection_drag_extend" => bool_display(self.selection_drag_extend).to_owned(),
            "scroll_drag_speed" => self.scroll_drag_speed.as_str().to_owned(),
            "pixel_scroll" => bool_display(self.pixel_scroll).to_owned(),
            "scroll_pixel_speed" => format_float(self.scroll_pixel_speed),
            "scroll_glide" => bool_display(self.scroll_glide).to_owned(),
            "scrollbar_drag" => bool_display(self.scrollbar_drag).to_owned(),
            "wheel_zoom" => bool_display(self.wheel_zoom).to_owned(),
            "command_status_gutter" => bool_display(self.command_status_gutter).to_owned(),
            "always_show_tab_bar" => bool_display(self.always_show_tab_bar).to_owned(),
            "tab_bar_placement" => self.tab_bar_placement.rail_side_str().to_owned(),
            "workspace_rail" => self.workspace_rail.as_str().to_owned(),
            "tab_bar_height" => self.tab_bar_height.as_config_string(),
            "tab_rail_width" => self.tab_rail_width.as_config_string(),
            "tab_rail_max_width" => format_float(self.tab_rail_max_width),
            "tab_rail_gap" => format_float(self.tab_rail_gap),
            "tab_rail_slot_rows" => format_float(self.tab_rail_slot_rows),
            "tab_panel_strength" => format_float(self.tab_panel_strength),
            "tab_seam" => bool_display(self.tab_seam).to_owned(),
            "tab_rail_autohide" => bool_display(self.tab_rail_autohide).to_owned(),
            "tab_rail_reveal_px" => format_float(self.tab_rail_reveal_px),
            "sh_click" => bool_display(self.sh_click).to_owned(),
            "shell_integration" => bool_display(self.shell_integration).to_owned(),
            "confirm_close" => bool_display(self.confirm_close).to_owned(),
            "ssh_config_hosts" => bool_display(self.ssh_config_hosts).to_owned(),
            "remote_integration" => bool_display(self.remote_integration).to_owned(),
            "remote_reuse" => bool_display(self.remote_reuse).to_owned(),
            "remote_tmux" => bool_display(self.remote_tmux).to_owned(),
            "remote_persist" => self.remote_persist.as_str().to_owned(),
            "remote_image_paste" => self.remote_image_paste.as_str().to_owned(),
            "session_replay" => bool_display(self.session_replay).to_owned(),
            "restore_workspaces" => bool_display(self.restore_workspaces).to_owned(),
            "shell_exit_closes" => self.shell_exit_closes.as_str().to_owned(),
            "interactive_urls" => bool_display(self.interactive_urls).to_owned(),
            "interactive_paths" => bool_display(self.interactive_paths).to_owned(),
            "interactive_paths_barewords" => {
                bool_display(self.interactive_paths_barewords).to_owned()
            }
            "interactive_paths_click_hint" => {
                bool_display(self.interactive_paths_click_hint).to_owned()
            }
            "interactive_paths_image_inline" => {
                bool_display(self.interactive_paths_image_inline).to_owned()
            }
            "interactive_paths_editor" => {
                if self.interactive_paths_editor.is_empty() {
                    "default".to_owned()
                } else {
                    self.interactive_paths_editor.clone()
                }
            }
            "osc52_read" => bool_display(self.osc52_read).to_owned(),
            "copy_on_select" => bool_display(self.copy_on_select).to_owned(),
            "smart_ctrl_c" => self.smart_ctrl_c.as_str().to_owned(),
            "cvd_mode" => self.cvd_mode.as_str().to_owned(),
            "cvd_strength" => format_float(self.cvd_strength),
            "bell" => self.bell.as_str().to_owned(),
            "native_autoclose_ms" => self
                .native_autoclose
                .map(|duration| format!("{} ms", duration.as_millis()))
                .unwrap_or_else(|| "unset".to_owned()),
            _ => return None,
        };
        Some(value)
    }
}

fn setting_group_rank(group: &str) -> usize {
    match group {
        "Theme" => 0,
        "Font" => 1,
        "Rendering" => 2,
        // The four "Layout" groups sort right after `Rendering`, matching that
        // section's position (4th in `SECTIONS`): Tabs, then Workspace rail, then
        // Panel, then Panes, so the rows read top-to-bottom in that order.
        "Tabs" => 3,
        "Workspace rail" => 4,
        "Panel" => 5,
        "Panes" => 6,
        "Post-process" => 7,
        "Cursor" => 8,
        "Input" => 9,
        "Connections" => 10,
        "Sessions" => 11,
        "Clipboard" => 12,
        "Accessibility" => 13,
        "Development" => 14,
        _ => 99,
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

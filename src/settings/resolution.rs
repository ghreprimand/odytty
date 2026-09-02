// SPDX-License-Identifier: GPL-3.0-only
//! Effective-value policy and config/environment resolution orchestration.

use super::*;

impl Settings {
    pub fn plain_render_quality(&self) -> bool {
        self.render_quality.is_plain()
    }

    pub fn effective_stem_darken(&self) -> f32 {
        if self.plain_render_quality() {
            0.0
        } else {
            self.stem_darken
        }
    }

    pub fn effective_min_contrast(&self) -> f32 {
        if self.plain_render_quality() {
            1.0
        } else {
            self.min_contrast
        }
    }

    pub fn effective_focus_dim(&self) -> f32 {
        if self.plain_render_quality() {
            0.0
        } else {
            self.focus_dim
        }
    }

    /// Inactive-pane dimming amount, forced to `0.0` on the plain renderer
    /// profile so the fast path stays byte-identical even when the knob is set.
    pub fn effective_inactive_pane_dim(&self) -> f32 {
        if self.plain_render_quality() {
            0.0
        } else {
            self.inactive_pane_dim
        }
    }

    /// The active background treatment (ID3/U5), forced `Off` on the plain
    /// renderer profile so the fast path stays pixel-identical even when the
    /// knob is set.
    pub fn effective_background_treatment(&self) -> BackgroundTreatment {
        if self.plain_render_quality() {
            BackgroundTreatment::Off
        } else {
            self.background_treatment
        }
    }

    pub fn effective_bloom_enabled(&self) -> bool {
        !self.plain_render_quality() && (self.bloom || self.retro)
    }

    pub fn effective_bloom_threshold(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_BLOOM_THRESHOLD
        } else {
            self.bloom_threshold
        }
    }

    pub fn effective_bloom_intensity(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_BLOOM_INTENSITY
        } else {
            self.bloom_intensity
        }
    }

    pub fn effective_bloom_radius(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_BLOOM_RADIUS
        } else {
            self.bloom_radius
        }
    }

    pub fn effective_crt_enabled(&self) -> bool {
        !self.plain_render_quality() && (self.crt || self.retro)
    }

    pub fn effective_crt_scanline_intensity(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_CRT_SCANLINE_INTENSITY
        } else {
            self.crt_scanline_intensity
        }
    }

    pub fn effective_crt_vignette_strength(&self) -> f32 {
        if self.retro && !self.plain_render_quality() {
            RETRO_CRT_VIGNETTE_STRENGTH
        } else {
            self.crt_vignette_strength
        }
    }
    pub fn effective_crt_curvature(&self) -> f32 {
        // Curvature is a configuration/environment knob only, flat by default,
        // with no settings-panel control. The retro preset raises the other CRT
        // treatments but deliberately does not force curvature, so it falls
        // through to the configured knob exactly like the plain CRT profile. The
        // direct render path still zeroes it.
        if self.plain_render_quality() {
            0.0
        } else {
            self.crt_curvature
        }
    }

    /// Rows advanced per mouse-wheel notch (MOUSE-WHEEL-SPEED), as a `usize >= 1`.
    /// Rounds the stored `f32` (kept in the shared numeric-setting model) and
    /// floors at 1 so a wheel notch always moves at least one row. The default
    /// `3.0` returns `3`, byte-identical to the historical fixed step.
    pub fn scroll_wheel_step(&self) -> usize {
        (self.scroll_wheel_lines.round() as i64).max(1) as usize
    }

    /// Resolve the vertical rail band width in cells (F4-P4).
    ///
    /// `auto_want_cols` is the width the `Auto` mode wants — the longest tab
    /// title plus the widget's label chrome, measured by the integration layer
    /// (which owns the widget geometry). In `Auto` mode the result is that value
    /// clamped to `[MIN_TAB_RAIL_WIDTH, tab_rail_max_width]`; in `Manual` mode
    /// the stored width is clamped to the absolute widget bounds
    /// `[MIN_TAB_RAIL_WIDTH, MAX_TAB_RAIL_WIDTH]` and `auto_want_cols` is
    /// ignored. Kept pure (no session/widget deref) so the resolution is unit-
    /// tested at the settings seam.
    pub fn rail_width_cols(&self, auto_want_cols: usize) -> usize {
        let min = MIN_TAB_RAIL_WIDTH as usize;
        match self.tab_rail_width {
            TabRailWidth::Manual(cols) => (cols as usize).clamp(min, MAX_TAB_RAIL_WIDTH as usize),
            TabRailWidth::Auto => auto_want_cols.clamp(min, self.rail_max_width_cols()),
        }
    }

    /// The `auto`-mode upper clamp in cells (F4-P4), rounded from the stored
    /// `f32` and clamped to `[MIN_TAB_RAIL_MAX_WIDTH, MAX_TAB_RAIL_MAX_WIDTH]`.
    pub fn rail_max_width_cols(&self) -> usize {
        self.tab_rail_max_width
            .round()
            .clamp(MIN_TAB_RAIL_MAX_WIDTH, MAX_TAB_RAIL_MAX_WIDTH) as usize
    }

    /// Inter-slot gap in rows for the vertical rail (F4-P1), rounded and clamped
    /// to `[MIN_TAB_RAIL_GAP, MAX_TAB_RAIL_GAP]`.
    pub fn rail_slot_gap_rows(&self) -> usize {
        self.tab_rail_gap
            .round()
            .clamp(MIN_TAB_RAIL_GAP, MAX_TAB_RAIL_GAP) as usize
    }

    /// Rail slot height in rows (F4-P1), rounded and clamped to `{1, 2}`.
    pub fn rail_slot_rows(&self) -> usize {
        self.tab_rail_slot_rows
            .round()
            .clamp(MIN_TAB_RAIL_SLOT_ROWS, MAX_TAB_RAIL_SLOT_ROWS) as usize
    }

    /// Scrollback retention cap in logical lines for the core (`0` = unbounded).
    /// Rounds and floors the stored `f32`; a negative or non-finite value (which
    /// the parser already rejects) collapses to `0`.
    pub fn scrollback_limit(&self) -> usize {
        let rounded = self.scrollback_lines.round();
        if rounded.is_finite() && rounded > 0.0 {
            rounded as usize
        } else {
            0
        }
    }

    /// Upper bound on rows advanced per drag-edge autoscroll tick
    /// (MOUSE-AUTOSCROLL-VEL). `Ramp` allows up to [`MAX_AUTOSCROLL_ROWS`] so the
    /// step accelerates with overshoot past the band; `Legacy` returns `1`, which
    /// pins the delta helper to exactly ±1/0 — byte-identical to the pre-feature
    /// fixed one-row-per-tick autoscroll.
    pub fn autoscroll_max_rows(&self) -> usize {
        match self.scroll_drag_speed {
            ScrollDragSpeed::Ramp => MAX_AUTOSCROLL_ROWS,
            ScrollDragSpeed::Legacy => 1,
        }
    }

    /// Load settings from the config file, then overlay the current process
    /// environment. Environment variables always win.
    pub fn from_env() -> Self {
        Self::from_env_and_optional_config(config_file_path())
    }

    pub(super) fn from_edit_values(
        values: &BTreeMap<&'static str, String>,
    ) -> Result<Self, SettingEditError> {
        let mut warnings = Vec::new();
        // On the overlay-edit path a font-family change that does not resolve is
        // the most actionable failure, so capture the precise reason (not found
        // vs not monospace) and surface it ahead of the generic fallback warning
        // `from_source` also emits for the same condition. The success arm stays
        // byte-identical: `try_resolve_font_family(..).Ok` returns the same
        // `regular` path `resolve_font_family` would have.
        let mut font_family_error: Option<SettingEditError> = None;
        let settings = Self::from_source(
            |key| values.get(key).map(OsString::from),
            |message| warnings.push(message.to_owned()),
            |family| match crate::text::try_resolve_font_family(
                family,
                &crate::text::font_search_dirs(),
            ) {
                Ok(matched) => Some(matched.regular),
                Err(reason) => {
                    font_family_error.get_or_insert_with(|| SettingEditError {
                        key: "font_family",
                        message: font_family_error_message(family, reason),
                    });
                    None
                }
            },
            |value| resolve_theme_file(value, theme_dir_path().as_deref()),
        );
        if let Some(error) = font_family_error {
            return Err(error);
        }
        // C4: THEME resolution/parse warnings must not block the edit. A theme
        // loaded from a file (or one that resolves with a tolerable warning like
        // an unknown key) would otherwise make every OTHER key read-only, since
        // each edit re-parses the whole value map including the theme. These are
        // treated as tolerant here, matching startup/reload semantics (which
        // only print them). All other warnings — an invalid numeric value, an
        // out-of-range knob — stay hard edit errors so the panel still rejects
        // bad input with a precise message. Theme warnings are identified by the
        // `theme <name>: ...` per-file prefix or the THEME_ENV token in the
        // fallback message.
        let promotable = warnings
            .into_iter()
            .find(|message| !(message.starts_with("theme ") || message.contains(THEME_ENV)));
        if let Some(message) = promotable {
            return Err(SettingEditError { key: "", message });
        }
        Ok(settings)
    }

    fn from_env_and_optional_config(config_path: Option<PathBuf>) -> Self {
        let mut warnings = Vec::new();
        let mut suppressed = 0usize;
        let config = config_path
            .as_deref()
            .and_then(|path| {
                let mut warn = fs_read::bounded_warn(&mut warnings, &mut suppressed);
                match ConfigValues::read(path, &mut warn) {
                    Ok(values) => Some(values),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        warn(format!(
                            "could not read config file {}: {error}",
                            path.display()
                        ));
                        None
                    }
                }
            })
            .unwrap_or_default();
        fs_read::note_suppressed(&mut warnings, suppressed);

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

    pub(super) fn from_env_snapshot_and_config(
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

    pub(crate) fn from_source(
        mut get: impl FnMut(&str) -> Option<OsString>,
        mut warn: impl FnMut(&str),
        mut resolve_family: impl FnMut(&str) -> Option<PathBuf>,
        mut read_theme: impl FnMut(&str) -> Option<String>,
    ) -> Self {
        // ODYTTY_THEME resolution: a built-in name resolves to its const; any
        // other value is treated as a user theme (a path, or a name found in
        // the user theme dir) loaded via `read_theme` and parsed through the
        // shared `ThemeSpec` path. A missing/garbage value falls back to the
        // configured default with a warning — startup never fails from a bad
        // theme setting.
        // `ODYTTY_THEME=system` is a single-key alias (OS-THEME alias) that
        // enables OS dark/light following with default mappings. It is NOT a
        // built-in theme: the authored `theme` falls back to the configured
        // default, and `theme_is_system` carries the alias intent so
        // [`Settings::resolve_active_theme`] and writeback can honor it. The
        // individual `os_theme_dark`/`os_theme_light` overrides remain
        // available for custom mappings.
        // Workspace-rail geometry accepts two env/config families for each field:
        // the canonical `WORKSPACE_RAIL_*` name (the rail shows workspaces) and
        // the legacy `TAB_RAIL_*` twin, both mapping to the same Settings field.
        // Precedence is deterministic: the canonical `WORKSPACE_RAIL_*` value
        // wins when both are set. The two `get` calls run sequentially (not a
        // nested closure) so the single `FnMut` source is borrowed once at a
        // time. Same on all three platforms; settings are platform-agnostic.
        macro_rules! rail_get {
            ($canonical:expr, $legacy:expr) => {
                match get($canonical) {
                    Some(value) => Some(value),
                    None => get($legacy),
                }
            };
        }
        let mut theme_is_system = false;
        // The raw `theme` config string, preserved verbatim ONLY for a
        // file-loaded theme so the writeback/display round-trip re-reads the
        // file instead of the non-resolving `"custom"` placeholder the runtime
        // `Theme` carries. Built-ins round-trip fine via `Theme::name`, so this
        // stays `None` for them (and for the unset/system/fallback cases).
        let mut theme_config: Option<String> = None;
        let theme = match get(THEME_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            None => DEFAULT_THEME,
            Some(value) => {
                if value.eq_ignore_ascii_case(crate::settings::SYSTEM_THEME_NAME) {
                    theme_is_system = true;
                    DEFAULT_THEME
                } else if let Some(builtin) = Theme::from_name(&value) {
                    builtin
                } else if let Some(contents) = read_theme(&value) {
                    let spec = ThemeSpec::parse(&contents, |message| {
                        warn(&format!("theme {value:?}: {message}"))
                    });
                    // Preserve the exact config string (path or user-theme name)
                    // so `to_edit_values`/`setting_info` never persist the
                    // placeholder `"custom"` name the projected theme carries.
                    theme_config = Some(value);
                    spec.to_theme()
                } else {
                    warn(&format!(
                        "{THEME_ENV}={value:?} is not a built-in theme or a readable theme file; using the default theme"
                    ));
                    DEFAULT_THEME
                }
            }
        };
        let visual = get(VISUAL_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| VisualEffect::from_name_or_default(&value))
            .unwrap_or(DEFAULT_VISUAL);
        // Direct path knob (ODYTTY_FONT) takes precedence over family lookup so
        // an explicit file always wins. ODYTTY_FONT_FAMILY is resolved to a
        // validated monospace path only when no direct path is given; resolution
        // failure falls back to the embedded default (font_path = None) with
        // one warning, so a bad family value never aborts startup.
        let direct_path = get(FONT_ENV).map(PathBuf::from);
        // The raw explicit `font` key, kept verbatim for display/writeback. This
        // is distinct from the effective `font_path` below, which may instead be
        // the regular face resolved from `font_family` (RC4): the advanced `font`
        // row must reflect only what the user explicitly set.
        let explicit_font_path = direct_path.clone();
        let font_family = get(FONT_FAMILY_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| Some(crate::text::BUNDLED_FONT_FAMILY.to_owned()));
        let font_path = if direct_path.is_some() {
            direct_path
        } else if let Some(family) = font_family.as_deref() {
            if crate::text::is_bundled_font_family(family) {
                None
            } else {
                match resolve_family(family) {
                    Some(path) => Some(path),
                    None => {
                        warn(&format!(
                            "{FONT_FAMILY_ENV}={family:?} did not resolve to a monospace font; using the default font"
                        ));
                        None
                    }
                }
            }
        } else {
            None
        };
        let font_weight = get(FONT_WEIGHT_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| parse_font_weight_variant(&value))
            .unwrap_or_default();
        let font_size_px = parse_font_size(get(FONT_SIZE_ENV).as_deref(), &mut warn);
        let text_gamma = parse_text_gamma(get(TEXT_GAMMA_ENV).as_deref(), &mut warn);
        let stem_darken = parse_stem_darken(get(STEM_DARKEN_ENV).as_deref(), &mut warn);
        let min_contrast = parse_min_contrast(get(MIN_CONTRAST_ENV).as_deref(), &mut warn);
        let focus_dim = parse_focus_dim(get(FOCUS_DIM_ENV).as_deref(), &mut warn);
        let inactive_pane_dim =
            parse_inactive_pane_dim(get(INACTIVE_PANE_DIM_ENV).as_deref(), &mut warn);
        let render_quality = parse_render_quality(get(RENDER_QUALITY_ENV).as_deref(), &mut warn);
        let background_treatment =
            parse_background_treatment(get(BACKGROUND_TREATMENT_ENV).as_deref(), &mut warn);
        // `background_image` resolution (v0.6.0):
        //   * unset            -> the bundled default (compiled-in sentinel),
        //   * `none`/`off`/""   -> explicitly no image (the documented off-switch),
        //   * `default`/`bundled` -> the bundled default,
        //   * anything else     -> a real on-disk path.
        let background_image = match get(BACKGROUND_IMAGE_ENV) {
            None => Some(bundled_background_path()),
            Some(raw) => {
                let lossy = raw.to_string_lossy();
                let trimmed = lossy.trim();
                match trimmed.to_ascii_lowercase().as_str() {
                    "" | "none" | "off" | "false" => None,
                    "default" | "bundled" | BUNDLED_BACKGROUND_SENTINEL => {
                        Some(bundled_background_path())
                    }
                    _ => Some(PathBuf::from(trimmed)),
                }
            }
        };
        let background_blur_radius =
            parse_background_blur_radius(get(BACKGROUND_BLUR_RADIUS_ENV).as_deref(), &mut warn);
        // Unset -> the shipped default scrim (paired with the bundled default
        // background); an explicit value (including `auto` -> None) overrides.
        let background_image_scrim = match get(BACKGROUND_IMAGE_SCRIM_ENV) {
            None => Some(DEFAULT_BACKGROUND_IMAGE_SCRIM),
            some => parse_background_image_scrim(some.as_deref(), &mut warn),
        };
        let cell_bg_opacity = parse_cell_bg_opacity(get(CELL_BG_OPACITY_ENV).as_deref(), &mut warn);
        let colored_bg_opacity =
            parse_colored_bg_opacity(get(COLORED_BG_OPACITY_ENV).as_deref(), &mut warn);
        let selection_opacity =
            parse_selection_opacity(get(SELECTION_OPACITY_ENV).as_deref(), &mut warn);
        let text_brightness = parse_text_brightness(get(TEXT_BRIGHTNESS_ENV).as_deref(), &mut warn);
        let window_padding_px = parse_window_padding(get(WINDOW_PADDING_ENV).as_deref(), &mut warn);
        let bloom = parse_bool_setting(
            get(BLOOM_ENV).as_deref(),
            BLOOM_ENV,
            DEFAULT_BLOOM,
            &mut warn,
        );
        let default_bloom_threshold = DEFAULT_BLOOM_THRESHOLD;
        let bloom_threshold = parse_bloom_threshold(
            get(BLOOM_THRESHOLD_ENV).as_deref(),
            default_bloom_threshold,
            &mut warn,
        );
        let bloom_intensity = parse_bloom_intensity(get(BLOOM_INTENSITY_ENV).as_deref(), &mut warn);
        let bloom_radius = parse_bloom_radius(get(BLOOM_RADIUS_ENV).as_deref(), &mut warn);
        let retro = parse_bool_setting(
            get(RETRO_ENV).as_deref(),
            RETRO_ENV,
            DEFAULT_RETRO,
            &mut warn,
        );
        // UX5: the legacy `visual=ambient`/`scanlines` scanline effect is folded
        // into the unified CRT post-process. An ambient visual aliases to
        // `crt=on` ONLY when no explicit CRT setting is present — an explicit
        // `crt=`/`ODYTTY_CRT` always wins over the alias (the alias merely fills
        // the unset case), so a config can never stack two scanline passes.
        let crt_explicit = get(CRT_ENV);
        let crt = if crt_explicit.is_some() {
            parse_bool_setting(crt_explicit.as_deref(), CRT_ENV, DEFAULT_CRT, &mut warn)
        } else {
            DEFAULT_CRT || visual == VisualEffect::Ambient
        };
        let crt_scanline_intensity =
            parse_crt_scanline_intensity(get(CRT_SCANLINE_INTENSITY_ENV).as_deref(), &mut warn);
        let crt_scanline_period =
            parse_crt_scanline_period(get(CRT_SCANLINE_PERIOD_ENV).as_deref(), &mut warn);
        let crt_vignette_strength =
            parse_crt_vignette_strength(get(CRT_VIGNETTE_STRENGTH_ENV).as_deref(), &mut warn);
        let crt_curvature = parse_crt_curvature(get(CRT_CURVATURE_ENV).as_deref(), &mut warn);
        let subpixel = parse_subpixel(get(SUBPIXEL_ENV).as_deref(), &mut warn);
        let line_height = parse_line_height(get(LINE_HEIGHT_ENV).as_deref(), &mut warn);
        let box_thickness = parse_box_thickness(get(BOX_THICKNESS_ENV).as_deref(), &mut warn);
        let ligatures = parse_bool_setting(
            get(LIGATURES_ENV).as_deref(),
            LIGATURES_ENV,
            DEFAULT_LIGATURES,
            &mut warn,
        );
        let ligature_ss01 = parse_bool_setting(
            get(LIGATURE_SS01_ENV).as_deref(),
            LIGATURE_SS01_ENV,
            DEFAULT_LIGATURE_SS01,
            &mut warn,
        );
        let ligature_ss02 = parse_bool_setting(
            get(LIGATURE_SS02_ENV).as_deref(),
            LIGATURE_SS02_ENV,
            DEFAULT_LIGATURE_SS02,
            &mut warn,
        );
        let kitty_named_transports = parse_bool_setting(
            get(KITTY_NAMED_TRANSPORTS_ENV).as_deref(),
            KITTY_NAMED_TRANSPORTS_ENV,
            false,
            &mut warn,
        );
        let key_bindings = parse_key_bindings(get(KEYBINDS_ENV).as_deref(), &mut warn);
        let pane_prefix = parse_pane_prefix(get(PANE_PREFIX_ENV).as_deref(), &mut warn);
        let cursor_style = parse_cursor_style_setting(get(CURSOR_STYLE_ENV).as_deref(), &mut warn);
        let cursor_blink = parse_cursor_blink_setting(get(CURSOR_BLINK_ENV).as_deref(), &mut warn);
        let cursor_easing = parse_bool_setting(
            get(CURSOR_EASING_ENV).as_deref(),
            CURSOR_EASING_ENV,
            DEFAULT_CURSOR_EASING,
            &mut warn,
        );
        let cursor_glow = parse_bool_setting(
            get(CURSOR_GLOW_ENV).as_deref(),
            CURSOR_GLOW_ENV,
            DEFAULT_CURSOR_GLOW,
            &mut warn,
        );
        let cursor_glow_intensity =
            parse_cursor_glow_intensity(get(CURSOR_GLOW_INTENSITY_ENV).as_deref(), &mut warn);
        let cursor_trail = parse_bool_setting(
            get(CURSOR_TRAIL_ENV).as_deref(),
            CURSOR_TRAIL_ENV,
            DEFAULT_CURSOR_TRAIL,
            &mut warn,
        );
        let raw_cursor_trail_strength = get(CURSOR_TRAIL_STRENGTH_ENV);
        let cursor_trail_strength = raw_cursor_trail_strength
            .as_deref()
            .and_then(|raw| raw.to_str())
            .and_then(CursorTrailStrength::parse)
            .unwrap_or_else(|| {
                if let Some(raw) = raw_cursor_trail_strength.as_ref() {
                    warn(&format!(
                        "{CURSOR_TRAIL_STRENGTH_ENV}={raw:?} is not a valid cursor-trail strength; using balanced"
                    ));
                }
                CursorTrailStrength::default()
            });
        let cursor_motion = parse_bool_setting(
            get(CURSOR_MOTION_ENV).as_deref(),
            CURSOR_MOTION_ENV,
            DEFAULT_CURSOR_MOTION,
            &mut warn,
        );
        let reduced_motion = parse_bool_setting(
            get(REDUCED_MOTION_ENV).as_deref(),
            REDUCED_MOTION_ENV,
            DEFAULT_REDUCED_MOTION,
            &mut warn,
        );
        let osc52_read = parse_bool_setting(
            get(OSC52_READ_ENV).as_deref(),
            OSC52_READ_ENV,
            false,
            &mut warn,
        );
        let osc52_write = parse_osc52_write(get(OSC52_WRITE_ENV).as_deref(), &mut warn);
        let synthetic_styles = parse_bool_setting(
            get(SYNTHETIC_STYLES_ENV).as_deref(),
            SYNTHETIC_STYLES_ENV,
            true,
            &mut warn,
        );
        let geometric_boxdraw = parse_bool_setting(
            get(GEOMETRIC_BOXDRAW_ENV).as_deref(),
            GEOMETRIC_BOXDRAW_ENV,
            true,
            &mut warn,
        );
        let symbol_fallback = parse_bool_setting(
            get(SYMBOL_FALLBACK_ENV).as_deref(),
            SYMBOL_FALLBACK_ENV,
            true,
            &mut warn,
        );
        let symbol_font = get(SYMBOL_FONT_ENV).and_then(parse_symbol_font_path);
        let symbol_map = get(SYMBOL_MAP_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|raw| parse_symbol_map(&raw, &mut warn))
            .unwrap_or_default();
        let themed_ui_roles = parse_bool_setting(
            get(THEMED_UI_ROLES_ENV).as_deref(),
            THEMED_UI_ROLES_ENV,
            true,
            &mut warn,
        );
        let scroll_wheel_lines =
            parse_scroll_wheel_lines(get(SCROLL_WHEEL_LINES_ENV).as_deref(), &mut warn);
        let scrollback_lines =
            parse_scrollback_lines(get(SCROLLBACK_LINES_ENV).as_deref(), &mut warn);
        let scroll_drag_speed =
            parse_scroll_drag_speed(get(SCROLL_DRAG_SPEED_ENV).as_deref(), &mut warn);
        let pixel_scroll = parse_bool_setting(
            get(PIXEL_SCROLL_ENV).as_deref(),
            PIXEL_SCROLL_ENV,
            DEFAULT_PIXEL_SCROLL,
            &mut warn,
        );
        let scroll_pixel_speed =
            parse_scroll_pixel_speed(get(SCROLL_PIXEL_SPEED_ENV).as_deref(), &mut warn);
        let scroll_glide = parse_bool_setting(
            get(SCROLL_GLIDE_ENV).as_deref(),
            SCROLL_GLIDE_ENV,
            DEFAULT_SCROLL_GLIDE,
            &mut warn,
        );
        let copy_on_select = parse_bool_setting(
            get(COPY_ON_SELECT_ENV).as_deref(),
            COPY_ON_SELECT_ENV,
            DEFAULT_COPY_ON_SELECT,
            &mut warn,
        );
        let smart_ctrl_c = parse_smart_ctrl_c(get(SMART_CTRL_C_ENV).as_deref(), &mut warn);
        let selection_drag_extend = parse_bool_setting(
            get(SELECTION_DRAG_EXTEND_ENV).as_deref(),
            SELECTION_DRAG_EXTEND_ENV,
            DEFAULT_SELECTION_DRAG_EXTEND,
            &mut warn,
        );
        let scrollbar_drag = parse_bool_setting(
            get(SCROLLBAR_DRAG_ENV).as_deref(),
            SCROLLBAR_DRAG_ENV,
            DEFAULT_SCROLLBAR_DRAG,
            &mut warn,
        );
        let wheel_zoom = parse_bool_setting(
            get(WHEEL_ZOOM_ENV).as_deref(),
            WHEEL_ZOOM_ENV,
            DEFAULT_WHEEL_ZOOM,
            &mut warn,
        );
        let command_status_gutter = parse_bool_setting(
            get(COMMAND_STATUS_GUTTER_ENV).as_deref(),
            COMMAND_STATUS_GUTTER_ENV,
            DEFAULT_COMMAND_STATUS_GUTTER,
            &mut warn,
        );
        let always_show_tab_bar = parse_bool_setting(
            get(ALWAYS_SHOW_TAB_BAR_ENV).as_deref(),
            ALWAYS_SHOW_TAB_BAR_ENV,
            DEFAULT_ALWAYS_SHOW_TAB_BAR,
            &mut warn,
        );
        // Rail side and visibility are separate settings. The side-of-record is
        // the `tab_bar_placement` field (kept so `workspace_rail_side()` is
        // unchanged); the canonical `ODYTTY_WORKSPACE_RAIL_SIDE` (left|right)
        // wins over the legacy `ODYTTY_TAB_BAR_PLACEMENT` (top|left|right,
        // top->left) when both are set. A legacy `workspace_rail=left|right`
        // still parses: it folds to visibility Always and supplies the side when
        // the canonical side key is absent. Precedence (side): canonical
        // WORKSPACE_RAIL_SIDE > legacy workspace_rail=left|right > tab_bar_placement.
        let workspace_rail_side_raw = get(WORKSPACE_RAIL_SIDE_ENV);
        let mut workspace_rail =
            parse_workspace_rail(get(WORKSPACE_RAIL_ENV).as_deref(), &mut warn);
        let folded_side = workspace_rail.forced_side();
        if folded_side.is_some() {
            workspace_rail = WorkspaceRail::Always;
        }
        let tab_bar_placement = if workspace_rail_side_raw.is_some() {
            parse_workspace_rail_side(workspace_rail_side_raw.as_deref(), &mut warn)
        } else if let Some(side) = folded_side {
            side
        } else {
            parse_tab_bar_placement(get(TAB_BAR_PLACEMENT_ENV).as_deref(), &mut warn)
        };
        let tab_rail_width = parse_tab_rail_width(
            rail_get!(WORKSPACE_RAIL_WIDTH_ENV, TAB_RAIL_WIDTH_ENV).as_deref(),
            &mut warn,
        );
        let tab_bar_height = parse_tab_bar_height(get(TAB_BAR_HEIGHT_ENV).as_deref(), &mut warn);
        let tab_rail_max_width = parse_tab_rail_max_width(
            rail_get!(WORKSPACE_RAIL_MAX_WIDTH_ENV, TAB_RAIL_MAX_WIDTH_ENV).as_deref(),
            &mut warn,
        );
        let tab_rail_gap = parse_tab_rail_gap(
            rail_get!(WORKSPACE_RAIL_GAP_ENV, TAB_RAIL_GAP_ENV).as_deref(),
            &mut warn,
        );
        let tab_rail_slot_rows = parse_tab_rail_slot_rows(
            rail_get!(WORKSPACE_RAIL_SLOT_ROWS_ENV, TAB_RAIL_SLOT_ROWS_ENV).as_deref(),
            &mut warn,
        );
        let tab_panel_strength =
            parse_tab_panel_strength(get(TAB_PANEL_STRENGTH_ENV).as_deref(), &mut warn);
        let tab_seam = parse_bool_setting(
            get(TAB_SEAM_ENV).as_deref(),
            TAB_SEAM_ENV,
            DEFAULT_TAB_SEAM,
            &mut warn,
        );
        let tab_rail_autohide = parse_bool_setting(
            rail_get!(WORKSPACE_RAIL_AUTOHIDE_ENV, TAB_RAIL_AUTOHIDE_ENV).as_deref(),
            TAB_RAIL_AUTOHIDE_ENV,
            DEFAULT_TAB_RAIL_AUTOHIDE,
            &mut warn,
        );
        let tab_rail_reveal_px = parse_tab_rail_reveal_px(
            rail_get!(WORKSPACE_RAIL_REVEAL_PX_ENV, TAB_RAIL_REVEAL_PX_ENV).as_deref(),
            &mut warn,
        );
        let sh_click = parse_bool_setting(
            get(SH_CLICK_ENV).as_deref(),
            SH_CLICK_ENV,
            DEFAULT_SH_CLICK,
            &mut warn,
        );
        let buttons = parse_bool_setting(
            get(BUTTONS_ENV).as_deref(),
            BUTTONS_ENV,
            DEFAULT_BUTTONS,
            &mut warn,
        );
        let buttons_iterm_compat = parse_bool_setting(
            get(BUTTONS_ITERM_COMPAT_ENV).as_deref(),
            BUTTONS_ITERM_COMPAT_ENV,
            DEFAULT_BUTTONS_ITERM_COMPAT,
            &mut warn,
        );
        let buttons_sticky = parse_bool_setting(
            get(BUTTONS_STICKY_ENV).as_deref(),
            BUTTONS_STICKY_ENV,
            DEFAULT_BUTTONS_STICKY,
            &mut warn,
        );
        let shell_integration = parse_bool_setting(
            get(SHELL_INTEGRATION_ENV).as_deref(),
            SHELL_INTEGRATION_ENV,
            DEFAULT_SHELL_INTEGRATION,
            &mut warn,
        );
        let shell_key_enhancement = parse_bool_setting(
            get(SHELL_KEY_ENHANCEMENT_ENV).as_deref(),
            SHELL_KEY_ENHANCEMENT_ENV,
            DEFAULT_SHELL_KEY_ENHANCEMENT,
            &mut warn,
        );
        let restore_workspaces = parse_bool_setting(
            get(RESTORE_WORKSPACES_ENV).as_deref(),
            RESTORE_WORKSPACES_ENV,
            DEFAULT_RESTORE_WORKSPACES,
            &mut warn,
        );
        let profile_auto_switch = parse_bool_setting(
            get(PROFILE_AUTO_SWITCH_ENV).as_deref(),
            PROFILE_AUTO_SWITCH_ENV,
            DEFAULT_PROFILE_AUTO_SWITCH,
            &mut warn,
        );
        let default_launch_profile = get(DEFAULT_LAUNCH_PROFILE_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let new_output_fade = parse_bool_setting(
            get(NEW_OUTPUT_FADE_ENV).as_deref(),
            NEW_OUTPUT_FADE_ENV,
            DEFAULT_NEW_OUTPUT_FADE,
            &mut warn,
        );
        let new_output_fade_ms =
            parse_new_output_fade_ms(get(NEW_OUTPUT_FADE_MS_ENV).as_deref(), &mut warn);
        let window_border = parse_bool_setting(
            get(WINDOW_BORDER_ENV).as_deref(),
            WINDOW_BORDER_ENV,
            DEFAULT_WINDOW_BORDER,
            &mut warn,
        );
        let window_decorations = parse_bool_setting(
            get(WINDOW_DECORATIONS_ENV).as_deref(),
            WINDOW_DECORATIONS_ENV,
            DEFAULT_WINDOW_DECORATIONS,
            &mut warn,
        );
        let window_transparency = parse_bool_setting(
            get(WINDOW_TRANSPARENCY_ENV).as_deref(),
            WINDOW_TRANSPARENCY_ENV,
            DEFAULT_WINDOW_TRANSPARENCY,
            &mut warn,
        );
        let window_opacity = parse_window_opacity(get(WINDOW_OPACITY_ENV).as_deref(), &mut warn);
        let cvd_mode = parse_cvd_mode(get(CVD_MODE_ENV).as_deref(), &mut warn);
        let cvd_strength = parse_cvd_strength(get(CVD_STRENGTH_ENV).as_deref(), &mut warn);
        let bell = parse_bell(get(BELL_ENV).as_deref(), &mut warn);
        let notifications = parse_notifications(get(NOTIFICATIONS_ENV).as_deref(), &mut warn);
        let shell_exit_closes =
            parse_shell_exit_closes(get(SHELL_EXIT_CLOSES_ENV).as_deref(), &mut warn);
        // `theme = system` forces OS following on regardless of the explicit
        // `follow_os_theme` value, so the alias is self-sufficient. The explicit
        // setting still wins for display/writeback of that specific key.
        let explicit_follow_os_theme = parse_bool_setting(
            get(FOLLOW_OS_THEME_ENV).as_deref(),
            FOLLOW_OS_THEME_ENV,
            DEFAULT_FOLLOW_OS_THEME,
            &mut warn,
        );
        let follow_os_theme = theme_is_system || explicit_follow_os_theme;
        let follow_external_palette = parse_bool_setting(
            get(FOLLOW_EXTERNAL_PALETTE_ENV).as_deref(),
            FOLLOW_EXTERNAL_PALETTE_ENV,
            DEFAULT_FOLLOW_EXTERNAL_PALETTE,
            &mut warn,
        );
        let external_palette_provider = match get(EXTERNAL_PALETTE_PROVIDER_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            Some(raw) => match crate::external_palette::ExternalPaletteProvider::parse(&raw) {
                Some(provider) => provider,
                None => {
                    warn(&format!(
                        "{EXTERNAL_PALETTE_PROVIDER_ENV}={raw:?} is not a known provider; using odytty"
                    ));
                    crate::external_palette::ExternalPaletteProvider::OdyttyAnsi
                }
            },
            None => crate::external_palette::ExternalPaletteProvider::OdyttyAnsi,
        };
        let external_palette_path = get(EXTERNAL_PALETTE_PATH_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        // OS-THEME dark/light theme names are stored verbatim (trimmed, empty =
        // unset) and resolved to a built-in theme lazily when the OS signal
        // applies, so an unknown name warns at apply time, not parse time.
        let os_theme_dark = get(OS_THEME_DARK_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let os_theme_light = get(OS_THEME_LIGHT_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let confirm_close = parse_bool_setting(
            get(CONFIRM_CLOSE_ENV).as_deref(),
            CONFIRM_CLOSE_ENV,
            DEFAULT_CONFIRM_CLOSE,
            &mut warn,
        );
        let warn_on_risky_paste = parse_bool_setting(
            get(WARN_ON_RISKY_PASTE_ENV).as_deref(),
            WARN_ON_RISKY_PASTE_ENV,
            DEFAULT_WARN_ON_RISKY_PASTE,
            &mut warn,
        );
        let ssh_config_hosts = parse_bool_setting(
            get(SSH_CONFIG_HOSTS_ENV).as_deref(),
            SSH_CONFIG_HOSTS_ENV,
            DEFAULT_SSH_CONFIG_HOSTS,
            &mut warn,
        );
        let remote_integration = parse_bool_setting(
            get(REMOTE_INTEGRATION_ENV).as_deref(),
            REMOTE_INTEGRATION_ENV,
            DEFAULT_REMOTE_INTEGRATION,
            &mut warn,
        );
        let remote_reuse = parse_bool_setting(
            get(REMOTE_REUSE_ENV).as_deref(),
            REMOTE_REUSE_ENV,
            DEFAULT_REMOTE_REUSE,
            &mut warn,
        );
        let remote_tmux = parse_bool_setting(
            get(REMOTE_TMUX_ENV).as_deref(),
            REMOTE_TMUX_ENV,
            DEFAULT_REMOTE_TMUX,
            &mut warn,
        );
        let remote_persist = parse_remote_persist(get(REMOTE_PERSIST_ENV).as_deref(), &mut warn);
        let remote_image_paste =
            parse_remote_image_paste(get(REMOTE_IMAGE_PASTE_ENV).as_deref(), &mut warn);
        let session_replay = parse_bool_setting(
            get(SESSION_REPLAY_ENV).as_deref(),
            SESSION_REPLAY_ENV,
            DEFAULT_SESSION_REPLAY,
            &mut warn,
        );
        let navigator_preview = parse_bool_setting(
            get(NAVIGATOR_PREVIEW_ENV).as_deref(),
            NAVIGATOR_PREVIEW_ENV,
            DEFAULT_NAVIGATOR_PREVIEW,
            &mut warn,
        );
        let interactive_urls = parse_bool_setting(
            get(INTERACTIVE_URLS_ENV).as_deref(),
            INTERACTIVE_URLS_ENV,
            DEFAULT_INTERACTIVE_URLS,
            &mut warn,
        );
        let interactive_paths = parse_bool_setting(
            get(INTERACTIVE_PATHS_ENV).as_deref(),
            INTERACTIVE_PATHS_ENV,
            DEFAULT_INTERACTIVE_PATHS,
            &mut warn,
        );
        let interactive_paths_barewords = parse_bool_setting(
            get(INTERACTIVE_PATHS_BAREWORDS_ENV).as_deref(),
            INTERACTIVE_PATHS_BAREWORDS_ENV,
            DEFAULT_INTERACTIVE_PATHS_BAREWORDS,
            &mut warn,
        );
        let interactive_paths_click_hint = parse_bool_setting(
            get(INTERACTIVE_PATHS_CLICK_HINT_ENV).as_deref(),
            INTERACTIVE_PATHS_CLICK_HINT_ENV,
            DEFAULT_INTERACTIVE_PATHS_CLICK_HINT,
            &mut warn,
        );
        let interactive_paths_image_inline = parse_bool_setting(
            get(INTERACTIVE_PATHS_IMAGE_INLINE_ENV).as_deref(),
            INTERACTIVE_PATHS_IMAGE_INLINE_ENV,
            DEFAULT_INTERACTIVE_PATHS_IMAGE_INLINE,
            &mut warn,
        );
        // Trim surrounding whitespace; empty (the default) means "use $EDITOR".
        // No validation here — the editor spec is tokenized + matched at open
        // time (a path/template with internal spaces is preserved as written).
        let interactive_paths_editor = get(INTERACTIVE_PATHS_EDITOR_ENV)
            .and_then(|value| value.into_string().ok())
            .map(|value| value.trim().to_owned())
            .unwrap_or_default();
        let native_autoclose = parse_autoclose(get(NATIVE_AUTOCLOSE_ENV).as_deref());

        Self {
            theme,
            theme_config,
            visual,
            font_path,
            explicit_font_path,
            font_family,
            font_weight,
            font_size_px,
            text_gamma,
            stem_darken,
            min_contrast,
            focus_dim,
            inactive_pane_dim,
            render_quality,
            background_treatment,
            background_image,
            background_blur_radius,
            background_image_scrim,
            cell_bg_opacity,
            colored_bg_opacity,
            selection_opacity,
            text_brightness,
            window_padding_px,
            bloom,
            bloom_threshold,
            bloom_intensity,
            bloom_radius,
            retro,
            crt,
            crt_scanline_intensity,
            crt_scanline_period,
            crt_vignette_strength,
            crt_curvature,
            subpixel,
            line_height,
            box_thickness,
            ligatures,
            ligature_ss01,
            ligature_ss02,
            kitty_named_transports,
            key_bindings,
            pane_prefix,
            cursor_style,
            cursor_blink,
            cursor_easing,
            cursor_glow,
            cursor_glow_intensity,
            cursor_trail,
            cursor_trail_strength,
            cursor_motion,
            reduced_motion,
            osc52_read,
            osc52_write,
            synthetic_styles,
            geometric_boxdraw,
            symbol_fallback,
            symbol_font,
            symbol_map,
            themed_ui_roles,
            scroll_wheel_lines,
            scrollback_lines,
            scroll_drag_speed,
            copy_on_select,
            smart_ctrl_c,
            selection_drag_extend,
            scrollbar_drag,
            wheel_zoom,
            command_status_gutter,
            always_show_tab_bar,
            tab_bar_placement,
            workspace_rail,
            tab_rail_width,
            tab_bar_height,
            tab_rail_max_width,
            tab_rail_gap,
            tab_rail_slot_rows,
            tab_panel_strength,
            tab_seam,
            tab_rail_autohide,
            tab_rail_reveal_px,
            sh_click,
            buttons,
            buttons_iterm_compat,
            buttons_sticky,
            shell_integration,
            shell_key_enhancement,
            restore_workspaces,
            profile_auto_switch,
            default_launch_profile,
            new_output_fade,
            new_output_fade_ms,
            window_border,
            window_decorations,
            window_transparency,
            window_opacity,
            pixel_scroll,
            scroll_pixel_speed,
            scroll_glide,
            cvd_mode,
            cvd_strength,
            bell,
            notifications,
            shell_exit_closes,
            theme_is_system,
            follow_os_theme,
            follow_external_palette,
            external_palette_provider,
            external_palette_path,
            os_theme_dark,
            os_theme_light,
            confirm_close,
            warn_on_risky_paste,
            ssh_config_hosts,
            remote_integration,
            remote_reuse,
            remote_tmux,
            remote_persist,
            remote_image_paste,
            session_replay,
            navigator_preview,
            interactive_urls,
            interactive_paths,
            interactive_paths_barewords,
            interactive_paths_click_hint,
            interactive_paths_image_inline,
            interactive_paths_editor,
            native_autoclose,
        }
    }
}

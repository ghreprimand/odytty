// SPDX-License-Identifier: GPL-3.0-only

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
        |_| None,
    );
    (settings, warnings)
}

fn settings_from_config_and_env<const N: usize>(
    config_contents: &str,
    env_values: [(&str, &str); N],
) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let config = ConfigValues::parse(config_contents, |message| warnings.push(message));
    let settings = Settings::from_source(
        |key| {
            env_values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
                .or_else(|| config.get(key).cloned())
        },
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    (settings, warnings)
}

fn env_map<const N: usize>(values: [(&'static str, &str); N]) -> HashMap<&'static str, OsString> {
    values
        .into_iter()
        .map(|(key, value)| (key, OsString::from(value)))
        .collect()
}

#[test]
fn defaults_are_stable_without_env() {
    let (settings, warnings) = settings_from([]);

    assert_eq!(settings, Settings::default());
    assert_eq!(settings.theme, DEFAULT_THEME);
    assert_eq!(settings.visual, DEFAULT_VISUAL);
    assert_eq!(
        settings.font_family.as_deref(),
        Some(crate::text::BUNDLED_FONT_FAMILY)
    );
    assert_eq!(settings.font_path, None);
    assert!(settings.bloom);
    assert_eq!(settings.bloom_threshold, DEFAULT_BLOOM_THRESHOLD);
    assert_eq!(settings.bloom_intensity, DEFAULT_BLOOM_INTENSITY);
    assert_eq!(settings.bloom_radius, DEFAULT_BLOOM_RADIUS);
    assert!(!settings.retro);
    assert!(settings.crt);
    assert_eq!(
        settings.crt_scanline_intensity,
        DEFAULT_CRT_SCANLINE_INTENSITY
    );
    assert_eq!(settings.crt_scanline_period, DEFAULT_CRT_SCANLINE_PERIOD);
    assert_eq!(settings.text_gamma, DEFAULT_TEXT_GAMMA);
    assert_eq!(settings.stem_darken, DEFAULT_STEM_DARKEN);
    assert_eq!(settings.min_contrast, DEFAULT_MIN_CONTRAST);
    assert_eq!(settings.focus_dim, DEFAULT_FOCUS_DIM);
    assert_eq!(settings.window_padding_px, DEFAULT_WINDOW_PADDING_PX);
    assert_eq!(settings.subpixel, SubpixelMode::Off);
    assert_eq!(settings.cell_bg_opacity, DEFAULT_CELL_BG_OPACITY);
    assert!(settings.ligatures, "programming ligatures default on");
    assert_eq!(settings.cursor_style, CursorStyle::Block);
    assert!(settings.cursor_glow, "cursor glow defaults on");
    assert!(settings.cursor_motion, "cursor motion defaults on");
    assert!(settings.cursor_trail, "the default motion trail is enabled");
    assert_eq!(
        settings.cursor_trail_strength,
        CursorTrailStrength::Balanced,
        "the linked trail profile defaults to balanced"
    );
    assert!(!settings.reduced_motion, "motion is allowed by default");
    assert!(warnings.is_empty());
}

#[test]
fn background_scrim_auto_is_valid_and_clears_override() {
    let (settings, warnings) = settings_from([(BACKGROUND_IMAGE_SCRIM_ENV, "auto")]);

    assert_eq!(settings.background_image_scrim, None);
    assert!(warnings.is_empty());
}

#[test]
fn setting_info_covers_every_field_with_descriptions() {
    let settings = Settings::default();
    let info = settings.setting_info();
    let keys = info.iter().map(|row| row.key).collect::<Vec<_>>();

    assert_eq!(
        keys,
        vec![
            "theme",
            "follow_os_theme",
            "os_theme_dark",
            "os_theme_light",
            "themed_ui_roles",
            "font",
            "font_family",
            "font_weight",
            "font_size",
            "line_height",
            "symbol_fallback",
            "symbol_font",
            "symbol_map",
            "ligatures",
            "kitty_named_transports",
            "text_gamma",
            "text_brightness",
            "stem_darken",
            "min_contrast",
            "focus_dim",
            "render_quality",
            "window_padding",
            "window_border",
            "window_decorations",
            "window_transparency",
            "window_opacity",
            "selection_opacity",
            "colored_bg_opacity",
            "subpixel",
            "synthetic_styles",
            "geometric_boxdraw",
            "box_thickness",
            "always_show_tab_bar",
            "tab_bar_height",
            "tab_bar_placement",
            "workspace_rail",
            "tab_rail_width",
            "tab_rail_max_width",
            "tab_rail_gap",
            "tab_rail_slot_rows",
            "tab_rail_autohide",
            "tab_rail_reveal_px",
            "tab_panel_strength",
            "tab_seam",
            "inactive_pane_dim",
            "pane_prefix",
            "visual",
            "retro",
            "crt",
            "bloom",
            "bloom_threshold",
            "bloom_intensity",
            "bloom_radius",
            "crt_scanline_intensity",
            "crt_scanline_period",
            "crt_vignette_strength",
            "background_treatment",
            "background_image",
            "cell_bg_opacity",
            "background_blur_radius",
            "background_image_scrim",
            "new_output_fade",
            "new_output_fade_ms",
            "cursor_style",
            "cursor_blink",
            "cursor_easing",
            "cursor_glow",
            "cursor_glow_intensity",
            "cursor_trail",
            "cursor_trail_strength",
            "cursor_motion",
            "keybinds",
            "scroll_wheel_lines",
            "scrollback_lines",
            "selection_drag_extend",
            "scroll_drag_speed",
            "pixel_scroll",
            "scroll_pixel_speed",
            "scroll_glide",
            "scrollbar_drag",
            "wheel_zoom",
            "confirm_close",
            "bell",
            "interactive_urls",
            "interactive_paths",
            "interactive_paths_barewords",
            "interactive_paths_click_hint",
            "interactive_paths_image_inline",
            "interactive_paths_editor",
            "sh_click",
            "command_status_gutter",
            "buttons",
            "buttons_iterm_compat",
            "buttons_sticky",
            "shell_integration",
            "shell_key_enhancement",
            "ssh_config_hosts",
            "remote_integration",
            "remote_reuse",
            "remote_tmux",
            "remote_persist",
            "remote_image_paste",
            "session_replay",
            "restore_workspaces",
            "shell_exit_closes",
            "osc52_write",
            "osc52_read",
            "copy_on_select",
            "smart_ctrl_c",
            "reduced_motion",
            "cvd_mode",
            "cvd_strength",
            "native_autoclose_ms",
        ]
    );
    assert!(info.iter().all(|row| !row.description.trim().is_empty()));
    // Every row carries a value EXCEPT an unset optional Path (e.g. the explicit
    // `font` file): an unset Path must surface an empty value, not a human
    // sentence, so the writeback never persists a placeholder as a real path
    // (FONT-SAVE-CORRECTNESS BUG 1). Non-Path rows still all carry a value.
    assert!(
        info.iter()
            .all(|row| { !row.value.trim().is_empty() || matches!(row.kind, SettingKind::Path) })
    );
    assert!(
        info.iter()
            .any(|row| row.key == "stem_darken" && row.range.as_deref() == Some("0.0..=1.0"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "min_contrast" && row.range.as_deref() == Some("1.0..=21.0"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "focus_dim" && row.range.as_deref() == Some("0.0..=1.0"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "render_quality" && row.options == ["plain", "balanced", "high"])
    );
    assert!(info
        .iter()
        .any(|row| row.key == "window_padding" && row.range.as_deref() == Some("0.0..=64.0 px")));
    assert!(
        info.iter()
            .any(|row| row.key == "bloom" && row.options == ["on", "off"])
    );
    assert!(
        info.iter()
            .any(|row| row.key == "bloom_threshold" && row.range.as_deref() == Some("0.7..=1.25"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "bloom_intensity" && row.range.as_deref() == Some("0.0..=1.0"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "bloom_radius" && row.range.as_deref() == Some("0.5..=8.0 px"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "retro" && row.options == ["on", "off"])
    );
    assert!(
        info.iter()
            .any(|row| row.key == "crt" && row.options == ["on", "off"])
    );
    assert!(info.iter().any(
        |row| row.key == "crt_scanline_intensity" && row.range.as_deref() == Some("0.0..=0.35")
    ));
    assert!(info.iter().any(
        |row| row.key == "crt_scanline_period" && row.range.as_deref() == Some("2.0..=12.0 px")
    ));
    assert!(info.iter().any(
        |row| row.key == "crt_vignette_strength" && row.range.as_deref() == Some("0.0..=0.45")
    ));
    // crt_curvature is a config/env-only knob with no settings-panel row; the
    // setting still parses and clamps (see crt_curvature_* tests below).
    assert!(
        !info.iter().any(|row| row.key == "crt_curvature"),
        "crt_curvature must not appear as a settings-panel row"
    );
    assert!(info.iter().any(|row| row.key == "background_treatment"
        && row.options == ["off", "gradient", "vignette", "image"]));
}

#[test]
fn cursor_glow_intensity_defaults_parses_and_clamps() {
    // Absent env → default.
    let (settings, warnings) = settings_from([]);
    assert_eq!(
        settings.cursor_glow_intensity,
        DEFAULT_CURSOR_GLOW_INTENSITY
    );
    assert!(warnings.is_empty());

    // A valid in-range value parses.
    let (settings, _) = settings_from([(CURSOR_GLOW_INTENSITY_ENV, "0.75")]);
    assert_eq!(settings.cursor_glow_intensity, 0.75);

    // Out-of-range values clamp to the normalized bounds.
    let (settings, _) = settings_from([(CURSOR_GLOW_INTENSITY_ENV, "5")]);
    assert_eq!(settings.cursor_glow_intensity, MAX_CURSOR_GLOW_INTENSITY);
    let (settings, _) = settings_from([(CURSOR_GLOW_INTENSITY_ENV, "-3")]);
    assert_eq!(settings.cursor_glow_intensity, MIN_CURSOR_GLOW_INTENSITY);

    // Garbage warns and falls back to the default rather than aborting.
    let (settings, warnings) = settings_from([(CURSOR_GLOW_INTENSITY_ENV, "bright")]);
    assert_eq!(
        settings.cursor_glow_intensity,
        DEFAULT_CURSOR_GLOW_INTENSITY
    );
    assert!(warnings.iter().any(|w| w.contains("cursor glow intensity")));
}

#[test]
fn cursor_glow_intensity_round_trips_through_edit_values() {
    // The panel edit-value writeback surfaces and re-parses the intensity, so a
    // slider change survives a Save + reload without drifting.
    let settings = Settings {
        cursor_glow_intensity: 0.4,
        ..Settings::default()
    };
    let info = settings.setting_info();
    let value = info
        .iter()
        .find(|row| row.key == "cursor_glow_intensity")
        .map(|row| row.value.as_str())
        .unwrap();
    assert_eq!(value, "0.4");

    let (reparsed, _) = settings_from([(CURSOR_GLOW_INTENSITY_ENV, value)]);
    assert_eq!(reparsed.cursor_glow_intensity, 0.4);
}

#[test]
fn new_output_fade_ms_defaults_parses_and_clamps() {
    // Absent env → default (chosen so the ramp is perceptible).
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.new_output_fade_ms, DEFAULT_NEW_OUTPUT_FADE_MS);
    assert_eq!(settings.new_output_fade_ms, 250.0);
    assert!(warnings.is_empty());

    // A valid in-range value parses.
    let (settings, _) = settings_from([(NEW_OUTPUT_FADE_MS_ENV, "400")]);
    assert_eq!(settings.new_output_fade_ms, 400.0);

    // Out-of-range values clamp to the bounds at both ends.
    let (settings, _) = settings_from([(NEW_OUTPUT_FADE_MS_ENV, "5000")]);
    assert_eq!(settings.new_output_fade_ms, MAX_NEW_OUTPUT_FADE_MS);
    let (settings, _) = settings_from([(NEW_OUTPUT_FADE_MS_ENV, "0")]);
    assert_eq!(settings.new_output_fade_ms, MIN_NEW_OUTPUT_FADE_MS);

    // Garbage warns and falls back to the default rather than aborting.
    let (settings, warnings) = settings_from([(NEW_OUTPUT_FADE_MS_ENV, "slow")]);
    assert_eq!(settings.new_output_fade_ms, DEFAULT_NEW_OUTPUT_FADE_MS);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("new-output fade duration"))
    );
}

#[test]
fn new_output_fade_defaults_on_and_parses_off() {
    // Ships on out of the box now that the ramp is text-only and never darkens
    // a row.
    let (settings, warnings) = settings_from([]);
    assert!(settings.new_output_fade, "on by default");
    assert_eq!(settings.new_output_fade, DEFAULT_NEW_OUTPUT_FADE);
    assert!(warnings.is_empty());

    // An explicit off is still honoured.
    let (off, _) = settings_from([(NEW_OUTPUT_FADE_ENV, "off")]);
    assert!(!off.new_output_fade);
    let (on, _) = settings_from([(NEW_OUTPUT_FADE_ENV, "on")]);
    assert!(on.new_output_fade);
}

#[test]
fn new_output_fade_ms_round_trips_through_config_key_and_edit_values() {
    // Config alias maps to the env key and back.
    assert_eq!(
        config_key_to_env("new_output_fade_ms"),
        Some(NEW_OUTPUT_FADE_MS_ENV)
    );
    assert_eq!(
        env_to_config_key(NEW_OUTPUT_FADE_MS_ENV),
        Some("new_output_fade_ms")
    );

    // The panel edit-value writeback surfaces and re-parses the duration, so a
    // slider change survives a Save + reload without drifting.
    let settings = Settings {
        new_output_fade_ms: 325.0,
        ..Settings::default()
    };
    let info = settings.setting_info();
    let value = info
        .iter()
        .find(|row| row.key == "new_output_fade_ms")
        .map(|row| row.value.as_str())
        .unwrap();
    assert_eq!(value, "325");

    let (reparsed, _) = settings_from([(NEW_OUTPUT_FADE_MS_ENV, value)]);
    assert_eq!(reparsed.new_output_fade_ms, 325.0);
}

#[test]
fn setting_info_formats_current_values_for_display() {
    let settings = Settings {
        theme: Theme::ODYSSEY,
        font_family: Some("JetBrains Mono".to_owned()),
        font_size_px: 18.0,
        bloom: true,
        bloom_threshold: 0.9,
        bloom_intensity: 0.35,
        bloom_radius: 4.5,
        render_quality: RenderQuality::High,
        window_padding_px: 12.0,
        crt: true,
        crt_scanline_intensity: 0.12,
        crt_scanline_period: 4.0,
        crt_vignette_strength: 0.14,
        symbol_font: Some(PathBuf::from("/tmp/symbols.otf")),
        cursor_blink: CursorBlink::Off,
        cursor_trail_strength: CursorTrailStrength::Expressive,
        osc52_read: true,
        ..Settings::default()
    };
    let info = settings.setting_info();
    let value = |key| {
        info.iter()
            .find(|row| row.key == key)
            .map(|row| row.value.as_str())
            .unwrap()
    };

    assert_eq!(value("theme"), "odyssey");
    assert_eq!(value("font_family"), "JetBrains Mono");
    assert_eq!(value("font_size"), "18");
    assert_eq!(value("bloom"), "on");
    assert_eq!(value("bloom_threshold"), "0.9");
    assert_eq!(value("bloom_intensity"), "0.35");
    assert_eq!(value("bloom_radius"), "4.5");
    assert_eq!(value("render_quality"), "high");
    assert_eq!(value("window_padding"), "12");
    assert_eq!(value("crt"), "on");
    assert_eq!(value("crt_scanline_intensity"), "0.12");
    assert_eq!(value("crt_scanline_period"), "4");
    assert_eq!(value("crt_vignette_strength"), "0.14");
    assert_eq!(value("symbol_font"), "/tmp/symbols.otf");
    assert_eq!(value("cursor_blink"), "off");
    assert_eq!(value("cursor_trail_strength"), "expressive");
    assert_eq!(value("osc52_read"), "on");
}

#[test]
fn config_parser_accepts_comments_whitespace_and_duplicate_last_wins() {
    let (settings, warnings) = settings_from_config_and_env(
        r#"
            # OdyTTY config
            theme = odyssey
            font_size = 17
            font_size = 19
            subpixel = bgr # inline comment
            cursor_blink = off
        "#,
        [],
    );

    assert_eq!(settings.theme, Theme::ODYSSEY);
    assert_eq!(settings.font_size_px, 19.0);
    assert_eq!(settings.subpixel, SubpixelMode::Bgr);
    assert_eq!(settings.cursor_blink, CursorBlink::Off);
    assert!(warnings.is_empty());
}

#[test]
fn config_parser_warns_and_skips_bad_lines_but_keeps_good_values() {
    let (settings, warnings) = settings_from_config_and_env(
        r#"
            font_size = 16
            no separator
            unknown_key = value
            = value
            text_gamma = bright
        "#,
        [],
    );

    assert_eq!(settings.font_size_px, 16.0);
    assert_eq!(settings.text_gamma, DEFAULT_TEXT_GAMMA);
    assert_eq!(warnings.len(), 4);
    assert!(warnings[0].contains("expected key = value"));
    assert!(warnings[1].contains("unknown key"));
    assert!(warnings[2].contains("empty key"));
    assert!(warnings[3].contains(TEXT_GAMMA_ENV));
}

#[test]
fn line_height_and_box_thickness_parse_clamp_and_default() {
    // Valid values round-trip through the config keys.
    let (settings, warnings) = settings_from_config_and_env(
        r#"
            line_height = 1.4
            box_thickness = 1.5
        "#,
        [],
    );
    assert_eq!(settings.line_height, 1.4);
    assert_eq!(settings.box_thickness, 1.5);
    assert!(warnings.is_empty());

    // Out-of-range values clamp to the documented bounds.
    let (clamped, _) = settings_from_config_and_env(
        r#"
            line_height = 9.0
            box_thickness = 0.01
        "#,
        [],
    );
    assert_eq!(clamped.line_height, MAX_LINE_HEIGHT);
    assert_eq!(clamped.box_thickness, MIN_BOX_THICKNESS);

    // Unset keys keep the byte-identical defaults; bad values warn and default.
    let (defaults, warnings) = settings_from_config_and_env(
        r#"
            line_height = wide
        "#,
        [],
    );
    assert_eq!(defaults.line_height, DEFAULT_LINE_HEIGHT);
    assert_eq!(defaults.box_thickness, DEFAULT_BOX_THICKNESS);
    assert_eq!(defaults.line_height, 1.0);
    assert_eq!(defaults.box_thickness, 1.0);
    assert!(warnings.iter().any(|w| w.contains(LINE_HEIGHT_ENV)));
}

#[test]
fn env_values_override_config_values() {
    let (settings, warnings) = settings_from_config_and_env(
        r#"
            font_size = 16
            text_gamma = 1.0
            subpixel = rgb
            cursor_style = underline
        "#,
        [
            (FONT_SIZE_ENV, "21"),
            (SUBPIXEL_ENV, "off"),
            (CURSOR_STYLE_ENV, "bar"),
        ],
    );

    assert_eq!(settings.font_size_px, 21.0);
    assert_eq!(settings.text_gamma, 1.0);
    assert_eq!(settings.subpixel, SubpixelMode::Off);
    assert_eq!(settings.cursor_style, CursorStyle::Bar);
    assert!(warnings.is_empty());
}

#[test]
fn config_values_use_the_same_parse_and_clamp_rules_as_env() {
    let (settings, warnings) = settings_from_config_and_env(
        r#"
            font_size = 900
            text_gamma = 0.1
            stem_darken = 0.4
            render_quality = high
            window_padding = 99
            bloom = on
            bloom_threshold = 9
            bloom_intensity = 2
            bloom_radius = 99
            retro = on
            crt = on
            crt_scanline_intensity = 9
            crt_scanline_period = 99
            crt_vignette_strength = 9
            keybinds = ctrl+shift+y=copy;alt+space=paste
            cursor_blink = steady
            native_autoclose_ms = 600
        "#,
        [],
    );

    assert_eq!(settings.font_size_px, MAX_FONT_SIZE_PX);
    assert_eq!(settings.text_gamma, MIN_TEXT_GAMMA);
    assert_eq!(settings.stem_darken, 0.4);
    assert_eq!(settings.render_quality, RenderQuality::High);
    assert_eq!(settings.window_padding_px, MAX_WINDOW_PADDING_PX);
    assert!(settings.bloom);
    assert_eq!(settings.bloom_threshold, MAX_BLOOM_THRESHOLD);
    assert_eq!(settings.bloom_intensity, MAX_BLOOM_INTENSITY);
    assert_eq!(settings.bloom_radius, MAX_BLOOM_RADIUS);
    assert!(settings.retro);
    assert!(settings.crt);
    assert_eq!(settings.crt_scanline_intensity, MAX_CRT_SCANLINE_INTENSITY);
    assert_eq!(settings.crt_scanline_period, MAX_CRT_SCANLINE_PERIOD);
    assert_eq!(settings.crt_vignette_strength, MAX_CRT_VIGNETTE_STRENGTH);
    assert_eq!(settings.key_bindings.len(), 2);
    assert_eq!(settings.cursor_blink, CursorBlink::Off);
    assert_eq!(settings.native_autoclose, Some(Duration::from_millis(600)));
    assert!(warnings.is_empty());
}

#[test]
fn missing_config_file_is_a_nonfatal_not_found() {
    let mut warnings = Vec::new();
    let path = std::env::temp_dir().join(format!(
        "odytty-missing-config-{}-cf1.conf",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    let result = ConfigValues::read(&path, |message| warnings.push(message));

    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    assert!(warnings.is_empty());
}

#[test]
fn config_reload_poller_uses_injected_time_and_fingerprint_changes() {
    let path = PathBuf::from("/tmp/odytty-test.conf");
    let t0 = Instant::now();
    let old = ConfigFileFingerprint {
        modified: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
        len: 4,
    };
    let new = ConfigFileFingerprint {
        modified: SystemTime::UNIX_EPOCH + Duration::from_secs(11),
        len: 4,
    };
    let mut poller = ConfigReloadPoller {
        path: Some(path),
        interval: CONFIG_RELOAD_INTERVAL,
        next_poll: t0 + CONFIG_RELOAD_INTERVAL,
        last_seen: Some(old),
    };

    assert_eq!(
        poller
            .poll_with(t0 + Duration::from_millis(500), || Ok(Some(new)))
            .unwrap(),
        ConfigPollEvent::Unchanged
    );
    assert_eq!(poller.last_seen, Some(old));
    assert_eq!(
        poller
            .poll_with(t0 + CONFIG_RELOAD_INTERVAL, || Ok(Some(new)))
            .unwrap(),
        ConfigPollEvent::Changed
    );
    assert_eq!(poller.last_seen, Some(new));
    assert_eq!(
        poller
            .poll_with(t0 + CONFIG_RELOAD_INTERVAL * 2, || Ok(None))
            .unwrap(),
        ConfigPollEvent::Deleted
    );
    assert_eq!(poller.last_seen, None);
}

#[test]
fn config_reload_preserves_startup_env_precedence() {
    let dir = std::env::temp_dir().join(format!("odytty-cf2-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reload.conf");
    fs::write(&path, "font_size = 16\ntheme = plain\n").unwrap();
    let t0 = Instant::now();
    let mut reloader =
        SettingsReloader::new(Some(path.clone()), env_map([(FONT_SIZE_ENV, "21")]), t0);

    fs::write(&path, "font_size = 32\ntheme = odyssey\n").unwrap();
    let outcome = reloader.poll(t0 + CONFIG_RELOAD_INTERVAL);
    let SettingsReloadOutcome::Reloaded { settings, warnings } = outcome else {
        panic!("expected reload, got {outcome:?}");
    };
    assert!(warnings.is_empty(), "a clean rewrite carries no warnings");
    assert_eq!(settings.font_size_px, 21.0);
    assert_eq!(settings.theme, Theme::ODYSSEY);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir(dir);
}

#[test]
fn config_reload_applies_usable_values_and_surfaces_warnings() {
    // F13: a rewrite with an out-of-range value must not discard the whole
    // reload. The bad value falls back (matching the startup path) and the
    // warning is surfaced rather than blocking the live reload.
    let dir = std::env::temp_dir().join(format!("odytty-cf2-bad-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reload.conf");
    fs::write(&path, "font_size = 16\n").unwrap();
    let t0 = Instant::now();
    let mut reloader = SettingsReloader::new(Some(path.clone()), HashMap::new(), t0);

    fs::write(&path, "font_size = massive\n").unwrap();
    let outcome = reloader.poll(t0 + CONFIG_RELOAD_INTERVAL);
    let SettingsReloadOutcome::Reloaded { settings, warnings } = outcome else {
        panic!("expected reload with warnings, got {outcome:?}");
    };
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(FONT_SIZE_ENV));
    // The unusable value fell back to the default rather than discarding.
    assert_eq!(settings.font_size_px, Settings::default().font_size_px);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir(dir);
}

#[test]
fn config_reload_applies_a_real_edit_despite_an_unknown_key() {
    // F13 canonical case: an unknown/future/typo'd key must not block a live
    // edit to a real setting. The real value reloads; the unknown key is a
    // surfaced-but-non-fatal notice.
    let dir = std::env::temp_dir().join(format!("odytty-cf2-unknown-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reload.conf");
    fs::write(&path, "font_size = 16\n").unwrap();
    let t0 = Instant::now();
    let mut reloader = SettingsReloader::new(Some(path.clone()), HashMap::new(), t0);

    fs::write(&path, "font_size = 28\nnot_a_real_key = whatever\n").unwrap();
    let outcome = reloader.poll(t0 + CONFIG_RELOAD_INTERVAL);
    let SettingsReloadOutcome::Reloaded { settings, warnings } = outcome else {
        panic!("expected reload despite an unknown key, got {outcome:?}");
    };
    assert_eq!(settings.font_size_px, 28.0, "the real edit is applied live");
    assert!(
        warnings.iter().any(|w| w.contains("not_a_real_key")),
        "the unknown key is surfaced as a non-fatal notice"
    );

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir(dir);
}

#[test]
fn writeback_preserves_comments_unknown_keys_and_roundtrips_edits() {
    let dir = temp_test_dir("writeback-roundtrip");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(CONFIG_FILE_NAME);
    let original = "\
# hand-written header

unknown_future = keep-me
font_size = 16 # inline survives
font = fonts/OldMono.ttf
theme = plain
";
    fs::write(&path, original).unwrap();
    let (base, _warnings) = settings_from_config_and_env(original, []);
    assert_eq!(base.font_size_px, 16.0);
    assert_eq!(base.font_path, Some(PathBuf::from("fonts/OldMono.ttf")));
    assert_eq!(base.theme, Theme::PLAIN);

    let mut edits = SettingsEditOverlay::new(&base);
    edits.apply_raw("font_size", "22").unwrap();
    edits.apply_raw("theme", "odyssey").unwrap();
    edits.apply_raw("font", "").unwrap();
    let result = write_settings_changes_to_path(&path, &edits.changes()).unwrap();
    assert_eq!(result.changed, 3);

    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.contains("# hand-written header"));
    assert!(saved.contains("\n\nunknown_future = keep-me"));
    assert!(saved.contains("# inline survives"));
    assert!(saved.contains("font_size = 22 # inline survives"));
    assert!(saved.contains("# disabled by OdyTTY settings panel: font = fonts/OldMono.ttf"));
    assert!(!saved.contains("/home/"));

    let (reloaded, _warnings) = settings_from_config_and_env(&saved, []);
    assert_eq!(reloaded.font_size_px, 22.0);
    assert_eq!(reloaded.theme, Theme::ODYSSEY);
    assert_eq!(reloaded.font_path, None);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir(dir);
}

#[test]
fn writeback_creates_missing_config_atomically_in_injected_dir() {
    let dir = temp_test_dir("writeback-create");
    let path = dir.join("nested").join(CONFIG_FILE_NAME);
    let changes = [SettingEdit {
        key: "visual",
        env: VISUAL_ENV,
        value: "ambient".to_owned(),
    }];

    let result = write_settings_changes_to_path(&path, &changes).unwrap();
    assert_eq!(result.path, path);
    assert_eq!(result.changed, 1);

    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.starts_with("# OdyTTY settings panel\n"));
    assert!(saved.contains("visual = ambient\n"));
    assert!(!saved.contains("/home/"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(dir);
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

// UX5 — legacy `visual=ambient` is folded into the unified CRT post-process.
// These guard the alias precedence + the plain-gate bypass so the retired
// scanline look survives a back-compat config and never double-applies.

#[test]
fn ux5_ambient_visual_aliases_to_crt_when_unset() {
    // TRAP 1 BACK-COMPAT: `visual=ambient` with no explicit CRT setting routes
    // the scanline look through CRT (crt=on). `scanlines` is an ambient alias.
    let (settings, _) = settings_from([(VISUAL_ENV, "ambient")]);
    assert_eq!(settings.visual, VisualEffect::Ambient);
    assert!(
        settings.crt,
        "ambient must alias to crt=on when crt is unset"
    );

    let (settings, _) = settings_from([(VISUAL_ENV, "scanlines")]);
    assert!(
        settings.crt,
        "scanlines is an ambient alias and also aliases crt"
    );
}

#[test]
fn ux5_explicit_crt_always_wins_over_ambient_alias() {
    // TRAP 4 NO-DOUBLE-APPLY: an explicit `crt=` always overrides the alias, in
    // BOTH precedence directions, so a config can never stack two scanline
    // passes (the alias only fills the unset case).
    let (settings, _) = settings_from([(VISUAL_ENV, "ambient"), (CRT_ENV, "off")]);
    assert!(
        !settings.crt,
        "explicit crt=off must win over the ambient alias"
    );

    let (settings, _) = settings_from([(VISUAL_ENV, "off"), (CRT_ENV, "on")]);
    assert!(
        settings.crt,
        "explicit crt=on stands without any visual alias"
    );
}

#[test]
fn retro_preset_is_a_separate_one_switch_profile() {
    let (settings, warnings) = settings_from([
        (RETRO_ENV, "on"),
        (BLOOM_ENV, "off"),
        (CRT_ENV, "off"),
        (BLOOM_THRESHOLD_ENV, "1.10"),
        (BLOOM_INTENSITY_ENV, "0.10"),
        (BLOOM_RADIUS_ENV, "1.0"),
        (CRT_SCANLINE_INTENSITY_ENV, "0.02"),
        (CRT_VIGNETTE_STRENGTH_ENV, "0.03"),
    ]);

    assert!(settings.retro);
    assert!(settings.effective_bloom_enabled());
    assert!(settings.effective_crt_enabled());
    assert_eq!(settings.effective_bloom_threshold(), RETRO_BLOOM_THRESHOLD);
    assert_eq!(settings.effective_bloom_intensity(), RETRO_BLOOM_INTENSITY);
    assert_eq!(settings.effective_bloom_radius(), RETRO_BLOOM_RADIUS);
    assert_eq!(
        settings.effective_crt_scanline_intensity(),
        RETRO_CRT_SCANLINE_INTENSITY
    );
    assert_eq!(
        settings.effective_crt_vignette_strength(),
        RETRO_CRT_VIGNETTE_STRENGTH
    );
    assert_eq!(settings.bloom_threshold, 1.10);
    assert_eq!(settings.bloom_intensity, 0.10);
    assert_eq!(settings.bloom_radius, 1.0);
    assert_eq!(settings.crt_scanline_intensity, 0.02);
    assert_eq!(settings.crt_vignette_strength, 0.03);
    assert!(warnings.is_empty());
}

#[test]
fn plain_render_quality_bypasses_retro_preset() {
    let (settings, warnings) = settings_from([(RETRO_ENV, "on"), (RENDER_QUALITY_ENV, "plain")]);

    assert!(settings.retro);
    assert!(!settings.effective_bloom_enabled());
    assert!(!settings.effective_crt_enabled());
    assert!(warnings.is_empty());
}

#[test]
fn retro_config_key_no_longer_aliases_crt_only() {
    let (settings, warnings) = settings_from_config_and_env("retro = on\ncrt = off\n", []);

    assert!(settings.retro);
    assert!(!settings.crt);
    assert!(settings.effective_crt_enabled());
    assert!(warnings.is_empty());
}

#[test]
fn ux5_no_ambient_still_uses_crt_default() {
    // CRT is now a default profile choice, independent of the legacy
    // visual=ambient alias. Explicit crt=off remains the opt-out.
    let (settings, _) = settings_from([]);
    assert_eq!(settings.visual, DEFAULT_VISUAL);
    assert!(settings.crt, "fresh install defaults to CRT on");
}

#[test]
fn render_quality_plain_suppresses_ambient_crt() {
    // The plain profile is the explicit fast/unstyled escape hatch.
    let settings = Settings {
        render_quality: RenderQuality::Plain,
        visual: VisualEffect::Ambient,
        crt: true,
        ..Settings::default()
    };
    assert!(
        !settings.effective_crt_enabled(),
        "plain render quality suppresses CRT even when ambient is active"
    );

    // An explicit crt under a plain profile obeys the same gate.
    let settings = Settings {
        render_quality: RenderQuality::Plain,
        visual: VisualEffect::Off,
        crt: true,
        ..Settings::default()
    };
    assert!(
        !settings.effective_crt_enabled(),
        "non-ambient crt still obeys the plain gate"
    );
}

fn temp_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("odytty-{name}-{}", std::process::id()))
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
fn stem_darken_defaults_to_visible_boost() {
    // Fresh installs use a visible boost; explicit 0.0 remains the opt-out.
    // The opt-out is an explicit `0.0`, exercised by the clamp/parse tests below.
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.stem_darken, DEFAULT_STEM_DARKEN);
    assert_eq!(settings.stem_darken, 0.7);
    assert!(warnings.is_empty());
}

#[test]
fn stem_darken_parses_a_valid_value() {
    let (settings, warnings) = settings_from([(STEM_DARKEN_ENV, "0.5")]);
    assert_eq!(settings.stem_darken, 0.5);
    assert!(warnings.is_empty());
}

#[test]
fn empty_stem_darken_falls_back_without_warning() {
    let (settings, warnings) = settings_from([(STEM_DARKEN_ENV, "  ")]);
    assert_eq!(settings.stem_darken, DEFAULT_STEM_DARKEN);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_stem_darken_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(STEM_DARKEN_ENV, "heavy")]);
    assert_eq!(settings.stem_darken, DEFAULT_STEM_DARKEN);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(STEM_DARKEN_ENV));
}

#[test]
fn stem_darken_clamps_to_unit_range() {
    let (small, small_warnings) = settings_from([(STEM_DARKEN_ENV, "-1")]);
    let (large, large_warnings) = settings_from([(STEM_DARKEN_ENV, "5")]);

    assert_eq!(small.stem_darken, MIN_STEM_DARKEN);
    assert_eq!(large.stem_darken, MAX_STEM_DARKEN);
    assert!(small_warnings.is_empty());
    assert!(large_warnings.is_empty());
}

#[test]
fn min_contrast_defaults_to_strong_floor() {
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.min_contrast, DEFAULT_MIN_CONTRAST);
    assert_eq!(settings.min_contrast, 17.0);
    assert!(warnings.is_empty());
}

#[test]
fn min_contrast_parses_a_valid_value() {
    let (settings, warnings) = settings_from([(MIN_CONTRAST_ENV, "4.5")]);
    assert_eq!(settings.min_contrast, 4.5);
    assert!(warnings.is_empty());
}

#[test]
fn empty_min_contrast_falls_back_without_warning() {
    let (settings, warnings) = settings_from([(MIN_CONTRAST_ENV, "  ")]);
    assert_eq!(settings.min_contrast, DEFAULT_MIN_CONTRAST);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_min_contrast_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(MIN_CONTRAST_ENV, "high")]);
    assert_eq!(settings.min_contrast, DEFAULT_MIN_CONTRAST);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(MIN_CONTRAST_ENV));
}

#[test]
fn min_contrast_clamps_to_supported_range() {
    let (small, small_warnings) = settings_from([(MIN_CONTRAST_ENV, "0.2")]);
    let (large, large_warnings) = settings_from([(MIN_CONTRAST_ENV, "40")]);
    assert_eq!(small.min_contrast, MIN_MIN_CONTRAST);
    assert_eq!(large.min_contrast, MAX_MIN_CONTRAST);
    assert!(small_warnings.is_empty());
    assert!(large_warnings.is_empty());
}

#[test]
fn focus_dim_defaults_to_off() {
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.focus_dim, DEFAULT_FOCUS_DIM);
    assert_eq!(settings.focus_dim, 0.0);
    assert!(warnings.is_empty());
}

#[test]
fn focus_dim_parses_a_valid_value() {
    let (settings, warnings) = settings_from([(FOCUS_DIM_ENV, "0.25")]);
    assert_eq!(settings.focus_dim, 0.25);
    assert!(warnings.is_empty());
}

#[test]
fn empty_focus_dim_falls_back_without_warning() {
    let (settings, warnings) = settings_from([(FOCUS_DIM_ENV, "  ")]);
    assert_eq!(settings.focus_dim, DEFAULT_FOCUS_DIM);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_focus_dim_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(FOCUS_DIM_ENV, "lots")]);
    assert_eq!(settings.focus_dim, DEFAULT_FOCUS_DIM);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(FOCUS_DIM_ENV));
}

#[test]
fn focus_dim_clamps_to_unit_range() {
    let (small, small_warnings) = settings_from([(FOCUS_DIM_ENV, "-0.5")]);
    let (large, large_warnings) = settings_from([(FOCUS_DIM_ENV, "4")]);
    assert_eq!(small.focus_dim, MIN_FOCUS_DIM);
    assert_eq!(large.focus_dim, MAX_FOCUS_DIM);
    assert!(small_warnings.is_empty());
    assert!(large_warnings.is_empty());
}

#[test]
fn focus_dim_round_trips_through_config_key_mapping() {
    assert_eq!(config_key_to_env("focus_dim"), Some(FOCUS_DIM_ENV));
    assert_eq!(config_key_to_env("unfocuseddim"), Some(FOCUS_DIM_ENV));
    assert_eq!(env_to_config_key(FOCUS_DIM_ENV), Some("focus_dim"));
}

#[test]
fn pixel_scroll_round_trips_through_config_key_mapping() {
    assert_eq!(config_key_to_env("pixel_scroll"), Some(PIXEL_SCROLL_ENV));
    assert_eq!(config_key_to_env("pixelscroll"), Some(PIXEL_SCROLL_ENV));
    assert_eq!(env_to_config_key(PIXEL_SCROLL_ENV), Some("pixel_scroll"));
    assert_eq!(
        config_key_to_env("scroll_pixel_speed"),
        Some(SCROLL_PIXEL_SPEED_ENV)
    );
    assert_eq!(
        env_to_config_key(SCROLL_PIXEL_SPEED_ENV),
        Some("scroll_pixel_speed")
    );
}

#[test]
fn scroll_glide_round_trips_and_defaults_on() {
    // The discrete-wheel glide knob maps both ways and is on by default (tuned
    // interactive feel). Config parsing is generic, so an explicit value
    // overrides the default without any per-field code.
    assert_eq!(config_key_to_env("scroll_glide"), Some(SCROLL_GLIDE_ENV));
    assert_eq!(config_key_to_env("glidescroll"), Some(SCROLL_GLIDE_ENV));
    assert_eq!(env_to_config_key(SCROLL_GLIDE_ENV), Some("scroll_glide"));

    let (settings, warnings) = settings_from_config_and_env("", []);
    assert!(settings.scroll_glide, "scroll_glide is on by default");
    assert!(warnings.is_empty());

    // An explicit off overrides the default (no forced value).
    let (settings, warnings) = settings_from_config_and_env(
        "scroll_glide = off
",
        [],
    );
    assert!(
        !settings.scroll_glide,
        "an explicit scroll_glide = off overrides the on default"
    );
    assert!(warnings.is_empty());
}

#[test]
fn pixel_scroll_defaults_on_and_speed_is_identity() {
    let (settings, warnings) = settings_from_config_and_env("", []);
    assert!(
        settings.pixel_scroll,
        "continuous pixel scroll is on by default"
    );
    assert_eq!(
        settings.scroll_pixel_speed, 1.0,
        "1:1 physical tracking default"
    );
    assert!(warnings.is_empty());
}

#[test]
fn stale_smooth_scroll_config_key_loads_clean_without_disabling_pixel_scroll() {
    // The removed `smooth_scroll` knob is no longer recognized. An existing
    // config that still carries it must load cleanly (unknown key → warn+skip,
    // never a hard error) and must NOT silently disable the independent
    // continuous pixel-scroll lane, which keeps its on-by-default.
    let (settings, warnings) = settings_from_config_and_env(
        r#"
            smooth_scroll = on
            font_size = 15
        "#,
        [],
    );
    assert_eq!(
        settings.font_size_px, 15.0,
        "the rest of the config still applies"
    );
    assert!(
        settings.pixel_scroll,
        "pixel_scroll stays on despite the stale key"
    );
    assert_eq!(warnings.len(), 1, "only the stale key warns");
    assert!(warnings[0].contains("unknown key"));
}

#[test]
fn inactive_pane_dim_defaults_to_off() {
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.inactive_pane_dim, DEFAULT_INACTIVE_PANE_DIM);
    assert_eq!(settings.inactive_pane_dim, 0.0);
    assert!(warnings.is_empty());
}

#[test]
fn inactive_pane_dim_parses_a_valid_value() {
    let (settings, warnings) = settings_from([(INACTIVE_PANE_DIM_ENV, "0.2")]);
    assert_eq!(settings.inactive_pane_dim, 0.2);
    assert!(warnings.is_empty());
}

#[test]
fn empty_inactive_pane_dim_falls_back_without_warning() {
    let (settings, warnings) = settings_from([(INACTIVE_PANE_DIM_ENV, "  ")]);
    assert_eq!(settings.inactive_pane_dim, DEFAULT_INACTIVE_PANE_DIM);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_inactive_pane_dim_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(INACTIVE_PANE_DIM_ENV, "lots")]);
    assert_eq!(settings.inactive_pane_dim, DEFAULT_INACTIVE_PANE_DIM);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(INACTIVE_PANE_DIM_ENV));
}

#[test]
fn inactive_pane_dim_clamps_to_unit_range() {
    let (small, small_warnings) = settings_from([(INACTIVE_PANE_DIM_ENV, "-0.5")]);
    let (large, large_warnings) = settings_from([(INACTIVE_PANE_DIM_ENV, "4")]);
    assert_eq!(small.inactive_pane_dim, MIN_INACTIVE_PANE_DIM);
    assert_eq!(large.inactive_pane_dim, MAX_INACTIVE_PANE_DIM);
    assert!(small_warnings.is_empty());
    assert!(large_warnings.is_empty());
}

#[test]
fn inactive_pane_dim_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("inactive_pane_dim"),
        Some(INACTIVE_PANE_DIM_ENV)
    );
    assert_eq!(config_key_to_env("panedim"), Some(INACTIVE_PANE_DIM_ENV));
    assert_eq!(
        env_to_config_key(INACTIVE_PANE_DIM_ENV),
        Some("inactive_pane_dim")
    );
}

#[test]
fn inactive_pane_dim_forced_off_on_plain_profile() {
    let settings = Settings {
        render_quality: RenderQuality::Plain,
        inactive_pane_dim: 0.3,
        ..Settings::default()
    };
    assert_eq!(settings.effective_inactive_pane_dim(), 0.0);

    let balanced = Settings {
        render_quality: RenderQuality::Balanced,
        inactive_pane_dim: 0.3,
        ..Settings::default()
    };
    assert_eq!(balanced.effective_inactive_pane_dim(), 0.3);
}

#[test]
fn render_quality_defaults_to_high() {
    // v0.6.0: the shipped identity defaults to the High render profile.
    let (settings, warnings) = settings_from([]);

    assert_eq!(settings.render_quality, RenderQuality::High);
    assert_eq!(settings.render_quality.as_str(), "high");
    assert!(!settings.plain_render_quality());
    assert!(warnings.is_empty());
}

#[test]
fn render_quality_parses_known_values_and_aliases() {
    for (raw, expected) in [
        ("plain", RenderQuality::Plain),
        ("fast", RenderQuality::Plain),
        ("balanced", RenderQuality::Balanced),
        ("default", RenderQuality::Balanced),
        ("high", RenderQuality::High),
    ] {
        let (settings, warnings) = settings_from([(RENDER_QUALITY_ENV, raw)]);
        assert_eq!(settings.render_quality, expected, "raw={raw}");
        assert!(warnings.is_empty(), "raw={raw}: {warnings:?}");
    }
}

#[test]
fn garbage_render_quality_falls_back_with_warning() {
    let (settings, warnings) = settings_from([(RENDER_QUALITY_ENV, "cinematic")]);

    assert_eq!(settings.render_quality, RenderQuality::High);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(RENDER_QUALITY_ENV));
}

#[test]
fn render_quality_plain_derives_hard_plain_effective_values() {
    let settings = Settings {
        render_quality: RenderQuality::Plain,
        stem_darken: 0.7,
        min_contrast: 4.5,
        focus_dim: 0.3,
        bloom: true,
        crt: true,
        ..Settings::default()
    };

    assert_eq!(settings.effective_stem_darken(), 0.0);
    assert_eq!(settings.effective_min_contrast(), 1.0);
    assert_eq!(settings.effective_focus_dim(), 0.0);
    assert!(!settings.effective_bloom_enabled());
    assert!(!settings.effective_crt_enabled());
}

#[test]
fn render_quality_balanced_preserves_effective_values() {
    let settings = Settings {
        render_quality: RenderQuality::Balanced,
        stem_darken: 0.7,
        min_contrast: 4.5,
        focus_dim: 0.3,
        bloom: true,
        crt: true,
        ..Settings::default()
    };

    assert_eq!(settings.effective_stem_darken(), 0.7);
    assert_eq!(settings.effective_min_contrast(), 4.5);
    assert_eq!(settings.effective_focus_dim(), 0.3);
    assert!(settings.effective_bloom_enabled());
    assert!(settings.effective_crt_enabled());
}

#[test]
fn render_quality_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("render_quality"),
        Some(RENDER_QUALITY_ENV)
    );
    assert_eq!(config_key_to_env("quality"), Some(RENDER_QUALITY_ENV));
    assert_eq!(
        env_to_config_key(RENDER_QUALITY_ENV),
        Some("render_quality")
    );
    assert_eq!(
        config_key_to_env("window_padding"),
        Some(WINDOW_PADDING_ENV)
    );
    assert_eq!(config_key_to_env("padding"), Some(WINDOW_PADDING_ENV));
    assert_eq!(
        env_to_config_key(WINDOW_PADDING_ENV),
        Some("window_padding")
    );
}

#[test]
fn window_padding_defaults_parses_zero_and_clamps() {
    let (default_settings, warnings) = settings_from([]);
    assert_eq!(
        default_settings.window_padding_px,
        DEFAULT_WINDOW_PADDING_PX
    );
    assert!(warnings.is_empty());

    let (zero, warnings) = settings_from([(WINDOW_PADDING_ENV, "0")]);
    assert_eq!(zero.window_padding_px, 0.0);
    assert!(warnings.is_empty());

    let (clamped, warnings) = settings_from([(WINDOW_PADDING_ENV, "999")]);
    assert_eq!(clamped.window_padding_px, MAX_WINDOW_PADDING_PX);
    assert!(warnings.is_empty());
}

#[test]
fn bloom_defaults_to_on_with_fixed_threshold() {
    let (settings, warnings) = settings_from([]);
    assert!(settings.bloom);
    assert_eq!(settings.bloom_threshold, DEFAULT_BLOOM_THRESHOLD);
    assert_eq!(settings.bloom_intensity, DEFAULT_BLOOM_INTENSITY);
    assert_eq!(settings.bloom_radius, DEFAULT_BLOOM_RADIUS);
    assert!(warnings.is_empty());
}

#[test]
fn bloom_parses_valid_values() {
    let (settings, warnings) = settings_from([
        (BLOOM_ENV, "on"),
        (BLOOM_THRESHOLD_ENV, "0.95"),
        (BLOOM_INTENSITY_ENV, "0.25"),
        (BLOOM_RADIUS_ENV, "4.5"),
    ]);

    assert!(settings.bloom);
    assert_eq!(settings.bloom_threshold, 0.95);
    assert_eq!(settings.bloom_intensity, 0.25);
    assert_eq!(settings.bloom_radius, 4.5);
    assert!(warnings.is_empty());
}

#[test]
fn bloom_auto_threshold_uses_fixed_default() {
    let (settings, warnings) =
        settings_from([(THEME_ENV, "odyssey"), (BLOOM_THRESHOLD_ENV, "auto")]);

    assert_eq!(settings.bloom_threshold, DEFAULT_BLOOM_THRESHOLD);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_bloom_numbers_fall_back_with_warnings() {
    let (settings, warnings) = settings_from([
        (BLOOM_THRESHOLD_ENV, "bright"),
        (BLOOM_INTENSITY_ENV, "strong"),
        (BLOOM_RADIUS_ENV, "wide"),
    ]);

    assert_eq!(settings.bloom_threshold, DEFAULT_BLOOM_THRESHOLD);
    assert_eq!(settings.bloom_intensity, DEFAULT_BLOOM_INTENSITY);
    assert_eq!(settings.bloom_radius, DEFAULT_BLOOM_RADIUS);
    assert_eq!(warnings.len(), 3);
    assert!(warnings[0].contains(BLOOM_THRESHOLD_ENV));
    assert!(warnings[1].contains(BLOOM_INTENSITY_ENV));
    assert!(warnings[2].contains(BLOOM_RADIUS_ENV));
}

#[test]
fn bloom_values_clamp_to_supported_ranges() {
    let (small, small_warnings) = settings_from([
        (BLOOM_THRESHOLD_ENV, "0.1"),
        (BLOOM_INTENSITY_ENV, "-1"),
        (BLOOM_RADIUS_ENV, "0.1"),
    ]);
    let (large, large_warnings) = settings_from([
        (BLOOM_THRESHOLD_ENV, "9"),
        (BLOOM_INTENSITY_ENV, "9"),
        (BLOOM_RADIUS_ENV, "99"),
    ]);

    assert_eq!(small.bloom_threshold, MIN_BLOOM_THRESHOLD);
    assert_eq!(small.bloom_intensity, MIN_BLOOM_INTENSITY);
    assert_eq!(small.bloom_radius, MIN_BLOOM_RADIUS);
    assert_eq!(large.bloom_threshold, MAX_BLOOM_THRESHOLD);
    assert_eq!(large.bloom_intensity, MAX_BLOOM_INTENSITY);
    assert_eq!(large.bloom_radius, MAX_BLOOM_RADIUS);
    assert!(small_warnings.is_empty());
    assert!(large_warnings.is_empty());
}

#[test]
fn bloom_round_trips_through_config_key_mapping() {
    assert_eq!(config_key_to_env("bloom"), Some(BLOOM_ENV));
    assert_eq!(
        config_key_to_env("bloom_threshold"),
        Some(BLOOM_THRESHOLD_ENV)
    );
    assert_eq!(
        config_key_to_env("bloom_intensity"),
        Some(BLOOM_INTENSITY_ENV)
    );
    assert_eq!(config_key_to_env("bloom_radius"), Some(BLOOM_RADIUS_ENV));
    assert_eq!(env_to_config_key(BLOOM_ENV), Some("bloom"));
    assert_eq!(
        env_to_config_key(BLOOM_THRESHOLD_ENV),
        Some("bloom_threshold")
    );
    assert_eq!(
        env_to_config_key(BLOOM_INTENSITY_ENV),
        Some("bloom_intensity")
    );
    assert_eq!(env_to_config_key(BLOOM_RADIUS_ENV), Some("bloom_radius"));
}

#[test]
fn crt_defaults_to_on_with_bounded_defaults() {
    let (settings, warnings) = settings_from([]);
    assert!(settings.crt);
    assert_eq!(
        settings.crt_scanline_intensity,
        DEFAULT_CRT_SCANLINE_INTENSITY
    );
    assert_eq!(settings.crt_scanline_period, DEFAULT_CRT_SCANLINE_PERIOD);
    assert_eq!(
        settings.crt_vignette_strength,
        DEFAULT_CRT_VIGNETTE_STRENGTH
    );
    assert!(warnings.is_empty());
}

#[test]
fn crt_parses_valid_values() {
    let (settings, warnings) = settings_from([
        (CRT_ENV, "on"),
        (CRT_SCANLINE_INTENSITY_ENV, "0.12"),
        (CRT_SCANLINE_PERIOD_ENV, "4.5"),
        (CRT_VIGNETTE_STRENGTH_ENV, "0.14"),
    ]);

    assert!(settings.crt);
    assert_eq!(settings.crt_scanline_intensity, 0.12);
    assert_eq!(settings.crt_scanline_period, 4.5);
    assert_eq!(settings.crt_vignette_strength, 0.14);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_crt_numbers_fall_back_with_warnings() {
    let (settings, warnings) = settings_from([
        (CRT_SCANLINE_INTENSITY_ENV, "strong"),
        (CRT_SCANLINE_PERIOD_ENV, "dense"),
        (CRT_VIGNETTE_STRENGTH_ENV, "dark"),
    ]);

    assert_eq!(
        settings.crt_scanline_intensity,
        DEFAULT_CRT_SCANLINE_INTENSITY
    );
    assert_eq!(settings.crt_scanline_period, DEFAULT_CRT_SCANLINE_PERIOD);
    assert_eq!(
        settings.crt_vignette_strength,
        DEFAULT_CRT_VIGNETTE_STRENGTH
    );
    assert_eq!(warnings.len(), 3);
    assert!(warnings[0].contains(CRT_SCANLINE_INTENSITY_ENV));
    assert!(warnings[1].contains(CRT_SCANLINE_PERIOD_ENV));
    assert!(warnings[2].contains(CRT_VIGNETTE_STRENGTH_ENV));
}

#[test]
fn crt_values_clamp_to_supported_ranges() {
    let (small, small_warnings) = settings_from([
        (CRT_SCANLINE_INTENSITY_ENV, "-1"),
        (CRT_SCANLINE_PERIOD_ENV, "0.5"),
        (CRT_VIGNETTE_STRENGTH_ENV, "-1"),
    ]);
    let (large, large_warnings) = settings_from([
        (CRT_SCANLINE_INTENSITY_ENV, "9"),
        (CRT_SCANLINE_PERIOD_ENV, "99"),
        (CRT_VIGNETTE_STRENGTH_ENV, "9"),
    ]);

    assert_eq!(small.crt_scanline_intensity, MIN_CRT_SCANLINE_INTENSITY);
    assert_eq!(small.crt_scanline_period, MIN_CRT_SCANLINE_PERIOD);
    assert_eq!(small.crt_vignette_strength, MIN_CRT_VIGNETTE_STRENGTH);
    assert_eq!(large.crt_scanline_intensity, MAX_CRT_SCANLINE_INTENSITY);
    assert_eq!(large.crt_scanline_period, MAX_CRT_SCANLINE_PERIOD);
    assert_eq!(large.crt_vignette_strength, MAX_CRT_VIGNETTE_STRENGTH);
    assert!(small_warnings.is_empty());
    assert!(large_warnings.is_empty());
}

#[test]
fn crt_round_trips_through_config_key_mapping() {
    assert_eq!(config_key_to_env("crt"), Some(CRT_ENV));
    assert_eq!(
        config_key_to_env("crt_scanline_intensity"),
        Some(CRT_SCANLINE_INTENSITY_ENV)
    );
    assert_eq!(
        config_key_to_env("crt_scanline_period"),
        Some(CRT_SCANLINE_PERIOD_ENV)
    );
    assert_eq!(
        config_key_to_env("crt_vignette_strength"),
        Some(CRT_VIGNETTE_STRENGTH_ENV)
    );
    assert_eq!(env_to_config_key(CRT_ENV), Some("crt"));
    assert_eq!(
        env_to_config_key(CRT_SCANLINE_INTENSITY_ENV),
        Some("crt_scanline_intensity")
    );
    assert_eq!(
        env_to_config_key(CRT_SCANLINE_PERIOD_ENV),
        Some("crt_scanline_period")
    );
    assert_eq!(
        env_to_config_key(CRT_VIGNETTE_STRENGTH_ENV),
        Some("crt_vignette_strength")
    );
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
        settings_from_resolving([(FONT_FAMILY_ENV, "  Test Mono  ")], |family| {
            assert_eq!(family, "Test Mono");
            Some(PathBuf::from("/fonts/TestMono-Regular.ttf"))
        });
    assert_eq!(settings.font_family.as_deref(), Some("Test Mono"));
    assert_eq!(
        settings.font_path,
        Some(PathBuf::from("/fonts/TestMono-Regular.ttf"))
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
    // Falls back to the embedded default path (None) rather than failing.
    assert_eq!(settings.font_path, None);
    assert_eq!(settings.font_family.as_deref(), Some("No Such Mono"));
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(FONT_FAMILY_ENV));
}

#[test]
fn bundled_font_family_uses_embedded_default_without_host_resolution() {
    let mut resolver_called = false;
    let (settings, warnings) = settings_from_resolving(
        [(FONT_FAMILY_ENV, crate::text::BUNDLED_FONT_FAMILY)],
        |_| {
            resolver_called = true;
            None
        },
    );

    assert!(
        !resolver_called,
        "bundled family must not depend on host fonts"
    );
    assert_eq!(
        settings.font_family.as_deref(),
        Some(crate::text::BUNDLED_FONT_FAMILY)
    );
    assert_eq!(settings.font_path, None);
    assert!(warnings.is_empty());
}

#[test]
fn empty_font_family_is_ignored() {
    let (settings, warnings) = settings_from([(FONT_FAMILY_ENV, "   ")]);
    assert_eq!(
        settings.font_family.as_deref(),
        Some(crate::text::BUNDLED_FONT_FAMILY)
    );
    assert_eq!(settings.font_path, None);
    assert!(warnings.is_empty());
}

#[test]
fn overlay_edit_to_missing_font_family_reports_clear_error() {
    // The overlay-edit path (`from_edit_values`) must reject an unresolvable
    // family with a precise, family-named, user-facing message instead of
    // silently keeping the old font. A clearly bogus name is "not found" on any
    // host. Default settings have no direct font path, so the family is consulted.
    let mut values = Settings::default().to_edit_values();
    values.insert(FONT_FAMILY_ENV, "ZzzNoSuchFamily12345".to_owned());

    let error = Settings::from_edit_values(&values).expect_err("missing family must error");
    assert_eq!(error.key, "font_family");
    assert!(
        error.message.contains("ZzzNoSuchFamily12345"),
        "names the family: {}",
        error.message
    );
    assert!(
        error.message.contains("not found"),
        "states the reason: {}",
        error.message
    );
}

#[test]
fn cursor_defaults_without_env() {
    // v0.9.0 ships a blinking block cursor by default.
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.cursor_style, CursorStyle::Block);
    assert_eq!(settings.cursor_blink, CursorBlink::On);
    assert!(settings.cursor_blink.enabled());
    assert!(warnings.is_empty());
}

#[test]
fn cursor_blink_auto_resolves_to_the_conventional_blinking_default() {
    // HELP1: `Auto` is honest about Linux exposing no OS caret-blink preference
    // — it resolves to the conventional blinking default (the historical VT
    // power-on state), the same concrete blink flag `On` produces, while `Off`
    // is the explicit steady override. No phantom "OS preference" is consulted.
    assert!(CursorBlink::Auto.enabled());
    assert_eq!(CursorBlink::Auto.enabled(), CursorBlink::On.enabled());
    assert!(!CursorBlink::Off.enabled());
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
    assert!(warnings[0].contains("using block"));
}

#[test]
fn garbage_cursor_blink_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(CURSOR_BLINK_ENV, "sometimes")]);
    assert_eq!(settings.cursor_blink, CursorBlink::On);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(CURSOR_BLINK_ENV));
}

#[test]
fn osc52_read_defaults_off_and_parses_explicit_opt_in() {
    let (settings, warnings) = settings_from([]);
    assert!(!settings.osc52_read);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(OSC52_READ_ENV, "on")]);
    assert!(settings.osc52_read);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("osc52_read = yes", []);
    assert!(settings.osc52_read);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_osc52_read_falls_back_off_with_warning() {
    let (settings, warnings) = settings_from([(OSC52_READ_ENV, "maybe")]);
    assert!(!settings.osc52_read);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(OSC52_READ_ENV));
}

#[test]
fn synthetic_styles_defaults_on_and_parses_off() {
    let (settings, warnings) = settings_from([]);
    assert!(settings.synthetic_styles);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(SYNTHETIC_STYLES_ENV, "off")]);
    assert!(!settings.synthetic_styles);
    assert!(warnings.is_empty());

    // Config-file aliases map onto the same setting.
    let (settings, warnings) = settings_from_config_and_env("synthetic_styles = no", []);
    assert!(!settings.synthetic_styles);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("synthstyles = off", []);
    assert!(!settings.synthetic_styles);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_synthetic_styles_falls_back_on_with_warning() {
    let (settings, warnings) = settings_from([(SYNTHETIC_STYLES_ENV, "maybe")]);
    assert!(settings.synthetic_styles);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(SYNTHETIC_STYLES_ENV));
}

#[test]
fn geometric_boxdraw_defaults_on_and_parses_off() {
    // v0.6.0: geometric box-drawing ships on by default.
    let (settings, warnings) = settings_from([]);
    assert!(settings.geometric_boxdraw);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(GEOMETRIC_BOXDRAW_ENV, "off")]);
    assert!(!settings.geometric_boxdraw);
    assert!(warnings.is_empty());

    // Config-file form maps onto the same setting.
    let (settings, warnings) = settings_from_config_and_env("geometric_boxdraw = false", []);
    assert!(!settings.geometric_boxdraw);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_geometric_boxdraw_falls_back_on_with_warning() {
    // Falls back to the (now on) default with one warning.
    let (settings, warnings) = settings_from([(GEOMETRIC_BOXDRAW_ENV, "sometimes")]);
    assert!(settings.geometric_boxdraw);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(GEOMETRIC_BOXDRAW_ENV));
}

#[test]
fn background_treatment_defaults_to_image_and_color_opts_out() {
    use crate::settings::BackgroundTreatment;

    // v0.6.0: the shipped default treatment is the bundled image.
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.background_treatment, BackgroundTreatment::Image);
    assert!(settings.background_image.is_some(), "bundled default set");
    assert!(warnings.is_empty());

    // The documented off-switch: `background_treatment = color` draws the theme
    // background only (no image), with no warning.
    let (settings, warnings) = settings_from([(BACKGROUND_TREATMENT_ENV, "color")]);
    assert_eq!(settings.background_treatment, BackgroundTreatment::Off);
    assert!(
        warnings.is_empty(),
        "`color` is a recognized alias: {warnings:?}"
    );

    // The other documented off-switch: `background_image = none`.
    let (settings, _) = settings_from([(BACKGROUND_IMAGE_ENV, "none")]);
    assert!(settings.background_image.is_none());
}

#[test]
fn symbol_fallback_defaults_on_and_parses_off() {
    let (settings, warnings) = settings_from([]);
    assert!(settings.symbol_fallback);
    assert!(settings.symbol_font.is_none());
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(SYMBOL_FALLBACK_ENV, "off")]);
    assert!(!settings.symbol_fallback);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("symbol_fallback = false", []);
    assert!(!settings.symbol_fallback);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("nerdfont = no", []);
    assert!(!settings.symbol_fallback);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_symbol_fallback_falls_back_on_with_warning() {
    let (settings, warnings) = settings_from([(SYMBOL_FALLBACK_ENV, "sometimes")]);
    assert!(settings.symbol_fallback);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(SYMBOL_FALLBACK_ENV));
}

#[test]
fn symbol_font_parses_path_and_auto_clears_it() {
    let (settings, warnings) = settings_from([(SYMBOL_FONT_ENV, "/tmp/Symbols Nerd Font.otf")]);
    assert_eq!(
        settings.symbol_font,
        Some(PathBuf::from("/tmp/Symbols Nerd Font.otf"))
    );
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("symbol_font = auto", []);
    assert!(settings.symbol_font.is_none());
    assert!(warnings.is_empty());

    let (settings, warnings) =
        settings_from_config_and_env("symbolfontpath = /tmp/symbols.ttf", []);
    assert_eq!(
        settings.symbol_font,
        Some(PathBuf::from("/tmp/symbols.ttf"))
    );
    assert!(warnings.is_empty());
}

#[test]
fn symbol_fallback_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("symbol_fallback"),
        Some(SYMBOL_FALLBACK_ENV)
    );
    assert_eq!(config_key_to_env("nerdfont"), Some(SYMBOL_FALLBACK_ENV));
    assert_eq!(
        env_to_config_key(SYMBOL_FALLBACK_ENV),
        Some("symbol_fallback")
    );
    assert_eq!(config_key_to_env("symbol_font"), Some(SYMBOL_FONT_ENV));
    assert_eq!(config_key_to_env("nerdfontpath"), Some(SYMBOL_FONT_ENV));
    assert_eq!(env_to_config_key(SYMBOL_FONT_ENV), Some("symbol_font"));

    let settings = Settings {
        symbol_fallback: true,
        symbol_font: Some(PathBuf::from("/tmp/symbols.otf")),
        ..Settings::default()
    };
    assert_eq!(
        settings.to_edit_values().get(SYMBOL_FALLBACK_ENV),
        Some(&"on".to_owned())
    );
    assert_eq!(
        settings.to_edit_values().get(SYMBOL_FONT_ENV),
        Some(&"/tmp/symbols.otf".to_owned())
    );
}

#[test]
fn symbol_map_defaults_empty_and_parses_ranges() {
    // SYMMAP: default is empty (identity); a well-formed spec parses to ordered
    // first-match rules; malformed entries warn and are skipped without aborting.
    let (settings, warnings) = settings_from([]);
    assert!(settings.symbol_map.is_empty());
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(
        SYMBOL_MAP_ENV,
        "U+2500-U+257F=Fira Code; U+E000-U+F8FF=Symbols Nerd Font; U+2600=Weather",
    )]);
    assert_eq!(settings.symbol_map.len(), 3);
    assert_eq!(
        settings.symbol_map.lookup_char('\u{2500}'),
        Some("Fira Code")
    );
    assert_eq!(
        settings.symbol_map.lookup_char('\u{257F}'),
        Some("Fira Code")
    );
    assert_eq!(
        settings.symbol_map.lookup_char('\u{E000}'),
        Some("Symbols Nerd Font")
    );
    // Single codepoint U+2600 parses as the inclusive range 2600..=2600.
    assert_eq!(settings.symbol_map.lookup_char('\u{2600}'), Some("Weather"));
    assert_eq!(settings.symbol_map.lookup_char('\u{2601}'), None);
    // An unmapped codepoint outside every range is identity (None).
    assert_eq!(settings.symbol_map.lookup_char('A'), None);
    assert!(warnings.is_empty());

    // Malformed entries (no '=', empty font, bad codepoint, degenerate range)
    // warn and are skipped; a valid neighbor still lands.
    let (settings, warnings) = settings_from([(
        SYMBOL_MAP_ENV,
        "garbage; U+ZZZZ=Bad; U+30=; U+50-U+40=Reversed; U+41=Good",
    )]);
    assert_eq!(settings.symbol_map.len(), 1);
    assert_eq!(settings.symbol_map.lookup_char('A'), Some("Good"));
    assert_eq!(warnings.len(), 4);
    assert!(warnings.iter().all(|w| w.contains(SYMBOL_MAP_ENV)));
}

#[test]
fn symbol_map_round_trips_through_config_and_edit_values() {
    // Config-key aliases map both ways.
    assert_eq!(config_key_to_env("symbol_map"), Some(SYMBOL_MAP_ENV));
    assert_eq!(config_key_to_env("codepointmap"), Some(SYMBOL_MAP_ENV));
    assert_eq!(env_to_config_key(SYMBOL_MAP_ENV), Some("symbol_map"));

    // A config-sourced map parses identically to the env path.
    let (settings, warnings) = settings_from_config_and_env(
        "symbol_map = U+E000-U+F8FF=Symbols Nerd Font; U+2500=Fira Code",
        [],
    );
    assert_eq!(settings.symbol_map.len(), 2);
    assert!(warnings.is_empty());

    // to_edit_values serializes back to a parseable spec (range + single forms);
    // re-parsing reproduces the same map (round-trip stability).
    let serialized = settings
        .to_edit_values()
        .get(SYMBOL_MAP_ENV)
        .cloned()
        .expect("non-empty map is serialized");
    assert!(serialized.contains("U+E000-U+F8FF=Symbols Nerd Font"));
    assert!(serialized.contains("U+2500=Fira Code"));
    let (reparsed, warnings) = settings_from([(SYMBOL_MAP_ENV, serialized.as_str())]);
    assert_eq!(reparsed.symbol_map, settings.symbol_map);
    assert!(warnings.is_empty());

    // An empty map is omitted from the edit values (nothing to persist).
    let empty = Settings::default();
    assert!(!empty.to_edit_values().contains_key(SYMBOL_MAP_ENV));
}

#[test]
fn os_theme_round_trips_through_config_key_mapping() {
    // OS-THEME: the follow knob and the dark/light theme names map both ways.
    assert_eq!(
        config_key_to_env("follow_os_theme"),
        Some(FOLLOW_OS_THEME_ENV)
    );
    assert_eq!(config_key_to_env("autotheme"), Some(FOLLOW_OS_THEME_ENV));
    assert_eq!(
        env_to_config_key(FOLLOW_OS_THEME_ENV),
        Some("follow_os_theme")
    );
    assert_eq!(config_key_to_env("os_theme_dark"), Some(OS_THEME_DARK_ENV));
    assert_eq!(config_key_to_env("darktheme"), Some(OS_THEME_DARK_ENV));
    assert_eq!(env_to_config_key(OS_THEME_DARK_ENV), Some("os_theme_dark"));
    assert_eq!(
        config_key_to_env("os_theme_light"),
        Some(OS_THEME_LIGHT_ENV)
    );
    assert_eq!(
        env_to_config_key(OS_THEME_LIGHT_ENV),
        Some("os_theme_light")
    );

    let (settings, warnings) = settings_from([
        (FOLLOW_OS_THEME_ENV, "true"),
        (OS_THEME_DARK_ENV, "odyssey-noir"),
        (OS_THEME_LIGHT_ENV, "plain"),
    ]);
    assert!(settings.follow_os_theme);
    assert_eq!(settings.os_theme_dark.as_deref(), Some("odyssey-noir"));
    assert_eq!(settings.os_theme_light.as_deref(), Some("plain"));
    assert!(warnings.is_empty());

    // The edit-values surface re-emits the live values for writeback.
    let values = settings.to_edit_values();
    assert_eq!(values.get(FOLLOW_OS_THEME_ENV), Some(&"on".to_owned()));
    assert_eq!(
        values.get(OS_THEME_DARK_ENV),
        Some(&"odyssey-noir".to_owned())
    );
    assert_eq!(values.get(OS_THEME_LIGHT_ENV), Some(&"plain".to_owned()));

    // Defaults: following off, both directions unset (no edit-value emitted).
    let defaults = Settings::default();
    assert!(!defaults.follow_os_theme);
    assert!(defaults.os_theme_dark.is_none());
    assert!(defaults.os_theme_light.is_none());
    let default_values = defaults.to_edit_values();
    assert_eq!(
        default_values.get(FOLLOW_OS_THEME_ENV),
        Some(&"off".to_owned())
    );
    assert!(!default_values.contains_key(OS_THEME_DARK_ENV));
    assert!(!default_values.contains_key(OS_THEME_LIGHT_ENV));
}

#[test]
fn confirm_close_round_trips_through_config_key_mapping() {
    // CLOSE-CONFIRM: the toggle maps both ways and defaults ON.
    assert_eq!(config_key_to_env("confirm_close"), Some(CONFIRM_CLOSE_ENV));
    assert_eq!(config_key_to_env("closeconfirm"), Some(CONFIRM_CLOSE_ENV));
    assert_eq!(env_to_config_key(CONFIRM_CLOSE_ENV), Some("confirm_close"));

    assert!(Settings::default().confirm_close);

    let (settings, warnings) = settings_from([(CONFIRM_CLOSE_ENV, "off")]);
    assert!(!settings.confirm_close);
    assert!(warnings.is_empty());
    assert_eq!(
        settings.to_edit_values().get(CONFIRM_CLOSE_ENV),
        Some(&"off".to_owned())
    );
}

#[test]
fn ssh_config_hosts_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("ssh_config_hosts"),
        Some(SSH_CONFIG_HOSTS_ENV)
    );
    assert_eq!(config_key_to_env("sshhosts"), Some(SSH_CONFIG_HOSTS_ENV));
    assert_eq!(
        env_to_config_key(SSH_CONFIG_HOSTS_ENV),
        Some("ssh_config_hosts")
    );
    assert!(!Settings::default().ssh_config_hosts);

    let (settings, warnings) = settings_from([(SSH_CONFIG_HOSTS_ENV, "on")]);
    assert!(settings.ssh_config_hosts);
    assert!(warnings.is_empty());
    assert_eq!(
        settings.to_edit_values().get(SSH_CONFIG_HOSTS_ENV),
        Some(&"on".to_owned())
    );
}

#[test]
fn remote_integration_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("remote_integration"),
        Some(REMOTE_INTEGRATION_ENV)
    );
    assert_eq!(
        config_key_to_env("sshintegration"),
        Some(REMOTE_INTEGRATION_ENV)
    );
    assert_eq!(
        env_to_config_key(REMOTE_INTEGRATION_ENV),
        Some("remote_integration")
    );
    // On by default: the safe plain-ssh fallback makes default-on safe.
    assert!(Settings::default().remote_integration);

    let (settings, warnings) = settings_from([(REMOTE_INTEGRATION_ENV, "off")]);
    assert!(!settings.remote_integration);
    assert!(warnings.is_empty());
    assert_eq!(
        settings.to_edit_values().get(REMOTE_INTEGRATION_ENV),
        Some(&"off".to_owned())
    );
}

#[test]
fn remote_reuse_round_trips_through_config_key_mapping() {
    assert_eq!(config_key_to_env("remote_reuse"), Some(REMOTE_REUSE_ENV));
    assert_eq!(config_key_to_env("controlmaster"), Some(REMOTE_REUSE_ENV));
    assert_eq!(env_to_config_key(REMOTE_REUSE_ENV), Some("remote_reuse"));
    // On by default: a lost master degrades to a fresh connect, so default-on
    // is safe.
    assert!(Settings::default().remote_reuse);

    let (settings, warnings) = settings_from([(REMOTE_REUSE_ENV, "off")]);
    assert!(!settings.remote_reuse);
    assert!(warnings.is_empty());
    assert_eq!(
        settings.to_edit_values().get(REMOTE_REUSE_ENV),
        Some(&"off".to_owned())
    );
}

#[test]
fn remote_persist_round_trips_and_defaults_to_ten_minutes() {
    // ODP-9 Tier 2: the config key resolves both directions.
    assert_eq!(
        config_key_to_env("remote_persist"),
        Some(REMOTE_PERSIST_ENV)
    );
    assert_eq!(
        config_key_to_env("controlpersist"),
        Some(REMOTE_PERSIST_ENV)
    );
    assert_eq!(
        env_to_config_key(REMOTE_PERSIST_ENV),
        Some("remote_persist")
    );

    // Default is 10m, which resolves to the historical 600-second window so the
    // emitted argv is unchanged by default.
    assert_eq!(Settings::default().remote_persist, RemotePersist::Min10);
    assert_eq!(
        Settings::default().remote_persist.control_persist_value(),
        "600"
    );
    assert_eq!(RemotePersist::Off.control_persist_value(), "no");

    // A config value round-trips through the enum and back to its token.
    let (settings, warnings) = settings_from([(REMOTE_PERSIST_ENV, "2h")]);
    assert_eq!(settings.remote_persist, RemotePersist::Hour2);
    assert!(warnings.is_empty());
    assert_eq!(
        settings.to_edit_values().get(REMOTE_PERSIST_ENV),
        Some(&"2h".to_owned())
    );

    // An unrecognized value warns and keeps the default.
    let (settings, warnings) = settings_from([(REMOTE_PERSIST_ENV, "banana")]);
    assert_eq!(settings.remote_persist, RemotePersist::Min10);
    assert!(!warnings.is_empty());
}

#[test]
fn display_value_for_key_matches_setting_info_for_every_key() {
    // The single-key display-value derivation must stay byte-identical to the
    // full `setting_info()` table for every key, so the in-place panel update
    // can never drift from a full rebuild.
    let settings = Settings {
        theme: Theme::ODYSSEY,
        font_family: Some("JetBrains Mono".to_owned()),
        font_size_px: 18.0,
        bloom: true,
        bloom_threshold: 0.9,
        bloom_intensity: 0.35,
        bloom_radius: 4.5,
        render_quality: RenderQuality::High,
        window_padding_px: 12.0,
        crt: true,
        crt_scanline_intensity: 0.12,
        crt_scanline_period: 4.0,
        crt_vignette_strength: 0.14,
        symbol_font: Some(PathBuf::from("fixtures/symbols.otf")),
        cursor_blink: CursorBlink::Off,
        osc52_read: true,
        ..Settings::default()
    };
    let info = settings.setting_info();
    for row in &info {
        let single = settings
            .display_value_for_key(row.key)
            .unwrap_or_else(|| panic!("display_value_for_key missing key {}", row.key));
        assert_eq!(
            single, row.value,
            "display_value_for_key drift on key {}",
            row.key
        );
    }
}

#[test]
fn crt_curvature_defaults_off_and_parses_valid_values() {
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.crt_curvature, DEFAULT_CRT_CURVATURE);
    assert_eq!(settings.effective_crt_curvature(), 0.0);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(CRT_CURVATURE_ENV, "0.08")]);
    assert_eq!(settings.crt_curvature, 0.08);
    assert!(settings.crt);
    // crt is on (ambient default) but not retro, so the knob wins.
    assert_eq!(settings.effective_crt_curvature(), 0.08);
    assert!(warnings.is_empty());
}

#[test]
fn crt_curvature_clamps_to_supported_range() {
    let (small, small_warnings) = settings_from([(CRT_CURVATURE_ENV, "-1")]);
    let (large, large_warnings) = settings_from([(CRT_CURVATURE_ENV, "9")]);
    assert_eq!(small.crt_curvature, MIN_CRT_CURVATURE);
    assert_eq!(large.crt_curvature, MAX_CRT_CURVATURE);
    assert!(small_warnings.is_empty());
    assert!(large_warnings.is_empty());
}

#[test]
fn crt_curvature_garbage_falls_back_with_warning() {
    let (settings, warnings) = settings_from([(CRT_CURVATURE_ENV, "warped")]);
    assert_eq!(settings.crt_curvature, DEFAULT_CRT_CURVATURE);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(CRT_CURVATURE_ENV));
}

#[test]
fn crt_curvature_forced_off_on_plain_render_quality() {
    let (settings, _) = settings_from([
        (RENDER_QUALITY_ENV, "plain"),
        (CRT_ENV, "on"),
        (CRT_CURVATURE_ENV, "0.1"),
    ]);
    assert!(settings.plain_render_quality());
    assert_eq!(settings.crt_curvature, 0.1);
    // Plain path always exposes flat sampling, regardless of the knob.
    assert_eq!(settings.effective_crt_curvature(), 0.0);
}

#[test]
fn crt_curvature_retro_preset_overrides_to_subtle_curve() {
    // Explicit knob is set lower than the retro override; retro must win.
    let (settings, warnings) = settings_from([
        (RETRO_ENV, "on"),
        (CRT_ENV, "off"),
        (CRT_CURVATURE_ENV, "0.01"),
    ]);
    assert!(settings.retro);
    assert!(settings.effective_crt_enabled());
    assert_eq!(settings.crt_curvature, 0.01);
    assert_eq!(settings.effective_crt_curvature(), RETRO_CRT_CURVATURE);
    assert!(warnings.is_empty());
}

#[test]
fn crt_curvature_round_trips_through_config_key_mapping() {
    assert_eq!(config_key_to_env("crt_curvature"), Some(CRT_CURVATURE_ENV));
    assert_eq!(config_key_to_env("curvature"), Some(CRT_CURVATURE_ENV));
    assert_eq!(env_to_config_key(CRT_CURVATURE_ENV), Some("crt_curvature"));
}

#[test]
fn window_transparency_defaults_on_and_parses_on_off() {
    let (default_settings, warnings) = settings_from([]);
    assert!(default_settings.window_transparency);
    assert!(warnings.is_empty());

    let (on, warnings) = settings_from([(WINDOW_TRANSPARENCY_ENV, "on")]);
    assert!(on.window_transparency);
    assert!(warnings.is_empty());

    let (off, warnings) = settings_from([(WINDOW_TRANSPARENCY_ENV, "off")]);
    assert!(!off.window_transparency);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_window_transparency_falls_back_to_default_with_warning() {
    // Unparseable input falls back to the compiled default, which is now on.
    let (settings, warnings) = settings_from([(WINDOW_TRANSPARENCY_ENV, "maybe")]);
    assert!(settings.window_transparency);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(WINDOW_TRANSPARENCY_ENV));
}

#[test]
fn window_opacity_defaults_parses_and_clamps() {
    let (default_settings, warnings) = settings_from([]);
    assert_eq!(default_settings.window_opacity, DEFAULT_WINDOW_OPACITY);
    assert!(warnings.is_empty());

    let (parsed, warnings) = settings_from([(WINDOW_OPACITY_ENV, "70")]);
    assert_eq!(parsed.window_opacity, 70.0);
    assert!(warnings.is_empty());

    // Below the floor and above the ceiling both clamp into range.
    let (low, warnings) = settings_from([(WINDOW_OPACITY_ENV, "5")]);
    assert_eq!(low.window_opacity, MIN_WINDOW_OPACITY);
    assert!(warnings.is_empty());

    // The 20% floor is pinned by literal, not only symbolically: an exact
    // floor value survives unchanged, and a value below it clamps up to 20.
    let (at_floor, warnings) = settings_from([(WINDOW_OPACITY_ENV, "20")]);
    assert_eq!(at_floor.window_opacity, 20.0);
    assert!(warnings.is_empty());
    let (below, warnings) = settings_from([(WINDOW_OPACITY_ENV, "10")]);
    assert_eq!(below.window_opacity, 20.0);
    assert!(warnings.is_empty());

    let (high, warnings) = settings_from([(WINDOW_OPACITY_ENV, "150")]);
    assert_eq!(high.window_opacity, MAX_WINDOW_OPACITY);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_window_opacity_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(WINDOW_OPACITY_ENV, "translucent")]);
    assert_eq!(settings.window_opacity, DEFAULT_WINDOW_OPACITY);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(WINDOW_OPACITY_ENV));
}

#[test]
fn window_transparency_and_opacity_round_trip_through_config_keys() {
    assert_eq!(
        config_key_to_env("window_transparency"),
        Some(WINDOW_TRANSPARENCY_ENV)
    );
    assert_eq!(
        config_key_to_env("transparency"),
        Some(WINDOW_TRANSPARENCY_ENV)
    );
    assert_eq!(
        env_to_config_key(WINDOW_TRANSPARENCY_ENV),
        Some("window_transparency")
    );
    assert_eq!(
        config_key_to_env("window_opacity"),
        Some(WINDOW_OPACITY_ENV)
    );
    assert_eq!(config_key_to_env("opacity"), Some(WINDOW_OPACITY_ENV));
    assert_eq!(
        env_to_config_key(WINDOW_OPACITY_ENV),
        Some("window_opacity")
    );
}

#[test]
fn window_opacity_row_is_a_bounded_percent_stepper() {
    let settings = Settings::default();
    let info = settings.setting_info();
    let row = info
        .iter()
        .find(|r| r.key == "window_opacity")
        .expect("window_opacity row present");
    assert_eq!(row.kind, SettingKind::Number);
    assert_eq!(row.group, "Rendering");
    let spec = row.numeric.expect("window_opacity has a numeric spec");
    assert_eq!(spec.min, MIN_WINDOW_OPACITY);
    assert_eq!(spec.max, MAX_WINDOW_OPACITY);
    assert_eq!(spec.step, 5.0);
    assert_eq!(spec.unit, "%");

    let toggle = info
        .iter()
        .find(|r| r.key == "window_transparency")
        .expect("window_transparency row present");
    assert_eq!(toggle.kind, SettingKind::Bool);
    assert_eq!(toggle.group, "Rendering");
}

#[test]
fn selection_opacity_defaults_parses_and_clamps() {
    let (default_settings, warnings) = settings_from([]);
    assert_eq!(
        default_settings.selection_opacity,
        DEFAULT_SELECTION_OPACITY
    );
    assert_eq!(
        DEFAULT_SELECTION_OPACITY, 0.6,
        "default is a translucent tint (0.6), not fully opaque"
    );
    assert!(warnings.is_empty());

    let (parsed, warnings) = settings_from([(SELECTION_OPACITY_ENV, "0.5")]);
    assert_eq!(parsed.selection_opacity, 0.5);
    assert!(warnings.is_empty());

    // Below the floor and above the ceiling both clamp into [0,1].
    let (low, warnings) = settings_from([(SELECTION_OPACITY_ENV, "-0.3")]);
    assert_eq!(low.selection_opacity, MIN_SELECTION_OPACITY);
    assert!(warnings.is_empty());
    let (high, warnings) = settings_from([(SELECTION_OPACITY_ENV, "2.0")]);
    assert_eq!(high.selection_opacity, MAX_SELECTION_OPACITY);
    assert!(warnings.is_empty());

    // Empty/whitespace falls back to the default without warning.
    let (blank, warnings) = settings_from([(SELECTION_OPACITY_ENV, "  ")]);
    assert_eq!(blank.selection_opacity, DEFAULT_SELECTION_OPACITY);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_selection_opacity_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(SELECTION_OPACITY_ENV, "faint")]);
    assert_eq!(settings.selection_opacity, DEFAULT_SELECTION_OPACITY);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(SELECTION_OPACITY_ENV));
}

#[test]
fn selection_opacity_round_trips_through_config_keys() {
    assert_eq!(
        config_key_to_env("selection_opacity"),
        Some(SELECTION_OPACITY_ENV)
    );
    assert_eq!(
        config_key_to_env("selectionopacity"),
        Some(SELECTION_OPACITY_ENV)
    );
    assert_eq!(
        config_key_to_env("selectionalpha"),
        Some(SELECTION_OPACITY_ENV)
    );
    assert_eq!(
        env_to_config_key(SELECTION_OPACITY_ENV),
        Some("selection_opacity")
    );
}

#[test]
fn selection_opacity_row_is_a_bounded_unit_stepper() {
    let settings = Settings::default();
    let info = settings.setting_info();
    let row = info
        .iter()
        .find(|r| r.key == "selection_opacity")
        .expect("selection_opacity row present");
    assert_eq!(row.kind, SettingKind::Number);
    assert_eq!(row.group, "Rendering");
    assert!(row.reloadable, "selection opacity applies live");
    let spec = row.numeric.expect("selection_opacity has a numeric spec");
    assert_eq!(spec.min, MIN_SELECTION_OPACITY);
    assert_eq!(spec.max, MAX_SELECTION_OPACITY);
    assert_eq!(spec.step, 0.05);
    assert_eq!(spec.unit, "");
}

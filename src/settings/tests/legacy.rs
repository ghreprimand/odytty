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
            "text_gamma",
            "stem_darken",
            "min_contrast",
            "focus_dim",
            "render_quality",
            "window_padding",
            "window_border",
            "window_decorations",
            "subpixel",
            "synthetic_styles",
            "geometric_boxdraw",
            "box_thickness",
            "visual",
            "bloom",
            "bloom_threshold",
            "bloom_intensity",
            "bloom_radius",
            "crt",
            "crt_scanline_intensity",
            "crt_scanline_period",
            "crt_vignette_strength",
            "background_treatment",
            "background_image",
            "cell_bg_opacity",
            "background_blur_radius",
            "background_image_scrim",
            "new_output_fade",
            "cursor_style",
            "cursor_blink",
            "cursor_easing",
            "cursor_glow",
            "cursor_trail",
            "cursor_motion",
            "keybinds",
            "scroll_wheel_lines",
            "selection_drag_extend",
            "scroll_drag_speed",
            "smooth_scroll",
            "scrollbar_drag",
            "wheel_zoom",
            "command_status_gutter",
            "sh_click",
            "confirm_close",
            "osc52_read",
            "copy_on_select",
            "cvd_mode",
            "cvd_strength",
            "native_autoclose_ms",
        ]
    );
    assert!(info.iter().all(|row| !row.description.trim().is_empty()));
    assert!(info.iter().all(|row| !row.value.trim().is_empty()));
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
            .any(|row| row.key == "crt" && row.options == ["on", "off"])
    );
    assert!(info.iter().any(
        |row| row.key == "crt_scanline_intensity" && row.range.as_deref() == Some("0.0..=0.18")
    ));
    assert!(info.iter().any(
        |row| row.key == "crt_scanline_period" && row.range.as_deref() == Some("2.0..=12.0 px")
    ));
    assert!(info.iter().any(
        |row| row.key == "crt_vignette_strength" && row.range.as_deref() == Some("0.0..=0.16")
    ));
    assert!(info.iter().any(|row| row.key == "background_treatment"
        && row.options == ["off", "gradient", "vignette", "image"]));
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
    let SettingsReloadOutcome::Reloaded(settings) = outcome else {
        panic!("expected reload, got {outcome:?}");
    };
    assert_eq!(settings.font_size_px, 21.0);
    assert_eq!(settings.theme, Theme::ODYSSEY);

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir(dir);
}

#[test]
fn config_reload_rejects_bad_rewrite_without_candidate_settings() {
    let dir = std::env::temp_dir().join(format!("odytty-cf2-bad-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reload.conf");
    fs::write(&path, "font_size = 16\n").unwrap();
    let t0 = Instant::now();
    let mut reloader = SettingsReloader::new(Some(path.clone()), HashMap::new(), t0);

    fs::write(&path, "font_size = massive\n").unwrap();
    let outcome = reloader.poll(t0 + CONFIG_RELOAD_INTERVAL);
    let SettingsReloadOutcome::Invalid { warnings } = outcome else {
        panic!("expected invalid rewrite, got {outcome:?}");
    };
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(FONT_SIZE_ENV));

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
fn ux5_no_ambient_leaves_crt_off_by_default() {
    // The default (no visual, no crt) keeps CRT off — the alias only fires for
    // ambient. Guards TRAP 2 PLAIN-PATH IDENTITY at the settings layer.
    let (settings, _) = settings_from([]);
    assert_eq!(settings.visual, VisualEffect::Off);
    assert!(!settings.crt, "no ambient + no crt config keeps crt off");
}

#[test]
fn ux5_ambient_alias_bypasses_plain_gate() {
    // The legacy ambient path was never plain-gated, so its CRT alias is exempt
    // from the plain render-quality suppression — preserving back-compat.
    let settings = Settings {
        render_quality: RenderQuality::Plain,
        visual: VisualEffect::Ambient,
        crt: true,
        ..Settings::default()
    };
    assert!(
        settings.effective_crt_enabled(),
        "ambient alias bypasses the plain gate"
    );

    // An explicit (non-ambient) crt under a plain profile still obeys the gate
    // (the bypass is specific to the ambient alias).
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
fn stem_darken_defaults_to_subtle_boost() {
    // RV5 ships default-on: a conservative, perceptibly-crisper boost (not bold).
    // The opt-out is an explicit `0.0`, exercised by the clamp/parse tests below.
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.stem_darken, DEFAULT_STEM_DARKEN);
    assert_eq!(settings.stem_darken, 0.2);
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
fn min_contrast_defaults_to_passthrough() {
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.min_contrast, DEFAULT_MIN_CONTRAST);
    assert_eq!(settings.min_contrast, 1.0);
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
fn render_quality_defaults_to_balanced() {
    let (settings, warnings) = settings_from([]);

    assert_eq!(settings.render_quality, RenderQuality::Balanced);
    assert_eq!(settings.render_quality.as_str(), "balanced");
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

    assert_eq!(settings.render_quality, RenderQuality::Balanced);
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
fn bloom_defaults_to_off_with_theme_derived_threshold() {
    let (settings, warnings) = settings_from([]);
    assert!(!settings.bloom);
    assert_eq!(
        settings.bloom_threshold,
        default_bloom_threshold_for_theme(Theme::PLAIN)
    );
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
fn bloom_auto_threshold_tracks_theme_foreground() {
    let (settings, warnings) =
        settings_from([(THEME_ENV, "odyssey"), (BLOOM_THRESHOLD_ENV, "auto")]);

    assert_eq!(
        settings.bloom_threshold,
        default_bloom_threshold_for_theme(Theme::ODYSSEY)
    );
    assert!(warnings.is_empty());
}

#[test]
fn garbage_bloom_numbers_fall_back_with_warnings() {
    let (settings, warnings) = settings_from([
        (BLOOM_THRESHOLD_ENV, "bright"),
        (BLOOM_INTENSITY_ENV, "strong"),
        (BLOOM_RADIUS_ENV, "wide"),
    ]);

    assert_eq!(
        settings.bloom_threshold,
        default_bloom_threshold_for_theme(Theme::PLAIN)
    );
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
fn crt_defaults_to_off_with_bounded_defaults() {
    let (settings, warnings) = settings_from([]);
    assert!(!settings.crt);
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
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.cursor_style, CursorStyle::Block);
    assert_eq!(settings.cursor_blink, CursorBlink::Auto);
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
}

#[test]
fn garbage_cursor_blink_falls_back_with_one_warning() {
    let (settings, warnings) = settings_from([(CURSOR_BLINK_ENV, "sometimes")]);
    assert_eq!(settings.cursor_blink, CursorBlink::Auto);
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
fn geometric_boxdraw_defaults_off_and_parses_on() {
    let (settings, warnings) = settings_from([]);
    assert!(!settings.geometric_boxdraw);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(GEOMETRIC_BOXDRAW_ENV, "on")]);
    assert!(settings.geometric_boxdraw);
    assert!(warnings.is_empty());

    // Config-file form maps onto the same setting.
    let (settings, warnings) = settings_from_config_and_env("geometric_boxdraw = true", []);
    assert!(settings.geometric_boxdraw);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_geometric_boxdraw_falls_back_off_with_warning() {
    let (settings, warnings) = settings_from([(GEOMETRIC_BOXDRAW_ENV, "sometimes")]);
    assert!(!settings.geometric_boxdraw);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(GEOMETRIC_BOXDRAW_ENV));
}

#[test]
fn symbol_fallback_defaults_off_and_parses_on() {
    let (settings, warnings) = settings_from([]);
    assert!(!settings.symbol_fallback);
    assert!(settings.symbol_font.is_none());
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(SYMBOL_FALLBACK_ENV, "on")]);
    assert!(settings.symbol_fallback);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("symbol_fallback = true", []);
    assert!(settings.symbol_fallback);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("nerdfont = yes", []);
    assert!(settings.symbol_fallback);
    assert!(warnings.is_empty());
}

#[test]
fn garbage_symbol_fallback_falls_back_off_with_warning() {
    let (settings, warnings) = settings_from([(SYMBOL_FALLBACK_ENV, "sometimes")]);
    assert!(!settings.symbol_fallback);
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
    assert!(empty.to_edit_values().get(SYMBOL_MAP_ENV).is_none());
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
    assert!(default_values.get(OS_THEME_DARK_ENV).is_none());
    assert!(default_values.get(OS_THEME_LIGHT_ENV).is_none());
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

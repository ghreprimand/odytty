use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::Theme;

use super::config::{config_key_to_env, env_to_config_key};
use super::reload::{ConfigFileFingerprint, ConfigPollEvent};
use super::*;

static RELOAD_GLOBAL_TEST_LOCK: Mutex<()> = Mutex::new(());

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
            "visual",
            "font",
            "font_family",
            "font_size",
            "text_gamma",
            "stem_darken",
            "min_contrast",
            "focus_dim",
            "subpixel",
            "synthetic_styles",
            "geometric_boxdraw",
            "symbol_fallback",
            "symbol_font",
            "themed_ui_roles",
            "cursor_style",
            "cursor_blink",
            "keybinds",
            "osc52_read",
            "native_autoclose_ms",
        ]
    );
    assert!(info.iter().all(|row| !row.description.trim().is_empty()));
    assert!(info.iter().all(|row| !row.value.trim().is_empty()));
    assert!(
        info.iter()
            .any(|row| row.key == "stem_darken" && row.range == Some("0.0..=1.0"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "min_contrast" && row.range == Some("1.0..=21.0"))
    );
    assert!(
        info.iter()
            .any(|row| row.key == "focus_dim" && row.range == Some("0.0..=1.0"))
    );
}

#[test]
fn setting_info_formats_current_values_for_display() {
    let settings = Settings {
        theme: Theme::ODYSSEY,
        font_family: Some("JetBrains Mono".to_owned()),
        font_size_px: 18.0,
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
            keybinds = ctrl+shift+y=copy;alt+space=paste
            cursor_blink = steady
            native_autoclose_ms = 600
        "#,
        [],
    );

    assert_eq!(settings.font_size_px, MAX_FONT_SIZE_PX);
    assert_eq!(settings.text_gamma, MIN_TEXT_GAMMA);
    assert_eq!(settings.stem_darken, 0.4);
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
fn apply_reloadable_values_ignores_native_autoclose_ms() {
    let _guard = RELOAD_GLOBAL_TEST_LOCK.lock().unwrap();
    let mut current = Settings {
        theme: Theme::PLAIN,
        native_autoclose: Some(Duration::from_millis(500)),
        ..Settings::default()
    };
    let reloaded = Settings {
        theme: Theme::ODYSSEY,
        native_autoclose: Some(Duration::from_millis(9000)),
        ..Settings::default()
    };

    assert!(apply_reloadable_values(&mut current, reloaded));
    assert_eq!(current.theme, Theme::ODYSSEY);
    assert_eq!(current.native_autoclose, Some(Duration::from_millis(500)));
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
fn key_bindings_parse_valid_entries_case_insensitively() {
    let (settings, warnings) = settings_from([(
        KEYBINDS_ENV,
        "ctrl+shift+y=copy; SUPER+F=search, Shift+PageDown=scroll-down;ctrl+shift+comma=settings;ctrl+shift+t=theme-picker",
    )]);

    assert_eq!(settings.key_bindings.len(), 5);
    assert_eq!(
        settings.key_bindings[0],
        KeyBindingOverride {
            chord: KeyChord {
                modifiers: KeyBindingModifiers {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    super_key: false,
                },
                key: KeyBindingKey::Character('y'),
            },
            action: BindableAction::Copy,
        }
    );
    assert_eq!(
        settings.key_bindings[1],
        KeyBindingOverride {
            chord: KeyChord {
                modifiers: KeyBindingModifiers {
                    ctrl: false,
                    shift: false,
                    alt: false,
                    super_key: true,
                },
                key: KeyBindingKey::Character('f'),
            },
            action: BindableAction::Search,
        }
    );
    assert_eq!(
        settings.key_bindings[2].chord.key,
        KeyBindingKey::Named(KeyBindingNamedKey::PageDown)
    );
    assert_eq!(
        settings.key_bindings[2].action,
        BindableAction::ScrollPageDown
    );
    assert_eq!(
        settings.key_bindings[3].chord.key,
        KeyBindingKey::Character(',')
    );
    assert_eq!(
        settings.key_bindings[3].action,
        BindableAction::SettingsPanel
    );
    assert_eq!(
        settings.key_bindings[4].chord.key,
        KeyBindingKey::Character('t')
    );
    assert_eq!(settings.key_bindings[4].action, BindableAction::ThemePicker);
    assert!(warnings.is_empty());
}

#[test]
fn key_bindings_skip_bad_entries_with_warnings() {
    let (settings, warnings) = settings_from([(
        KEYBINDS_ENV,
        "ctrl+shift=copy,ctrl+shift+f=nope,ctrl+x+z=paste,alt+space=paste",
    )]);

    assert_eq!(settings.key_bindings.len(), 1);
    assert_eq!(
        settings.key_bindings[0].chord.key,
        KeyBindingKey::Named(KeyBindingNamedKey::Space)
    );
    assert_eq!(settings.key_bindings[0].action, BindableAction::Paste);
    assert_eq!(warnings.len(), 3);
    assert!(
        warnings
            .iter()
            .all(|warning| warning.contains(KEYBINDS_ENV))
    );
}

#[test]
fn empty_key_bindings_are_ignored_without_warning() {
    let (settings, warnings) = settings_from([(KEYBINDS_ENV, " , ; ")]);

    assert!(settings.key_bindings.is_empty());
    assert!(warnings.is_empty());
}

#[test]
fn duplicate_key_binding_entries_preserve_input_order() {
    let (settings, warnings) =
        settings_from([(KEYBINDS_ENV, "ctrl+shift+y=copy,ctrl+shift+y=paste")]);

    assert_eq!(settings.key_bindings.len(), 2);
    assert_eq!(settings.key_bindings[0].action, BindableAction::Copy);
    assert_eq!(settings.key_bindings[1].action, BindableAction::Paste);
    assert!(warnings.is_empty());
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
fn themed_ui_roles_defaults_on_and_parses_off() {
    let (settings, warnings) = settings_from([]);
    assert!(settings.themed_ui_roles);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from([(THEMED_UI_ROLES_ENV, "off")]);
    assert!(!settings.themed_ui_roles);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("themed_ui_roles = false", []);
    assert!(!settings.themed_ui_roles);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("themedroles = no", []);
    assert!(!settings.themed_ui_roles);
    assert!(warnings.is_empty());

    let (settings, warnings) = settings_from_config_and_env("uiroles = 0", []);
    assert!(!settings.themed_ui_roles);
    assert!(warnings.is_empty());
}

#[test]
fn themed_ui_roles_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("themed_ui_roles"),
        Some(THEMED_UI_ROLES_ENV)
    );
    assert_eq!(
        env_to_config_key(THEMED_UI_ROLES_ENV),
        Some("themed_ui_roles")
    );

    let settings = Settings {
        themed_ui_roles: false,
        ..Settings::default()
    };
    assert_eq!(
        settings.to_edit_values().get(THEMED_UI_ROLES_ENV),
        Some(&"off".to_owned())
    );
}

#[test]
fn garbage_themed_ui_roles_falls_back_on_with_warning() {
    let (settings, warnings) = settings_from([(THEMED_UI_ROLES_ENV, "maybe")]);
    assert!(settings.themed_ui_roles);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(THEMED_UI_ROLES_ENV));
}

#[test]
fn apply_reloadable_values_publishes_synthetic_styles_global() {
    let _guard = RELOAD_GLOBAL_TEST_LOCK.lock().unwrap();
    // The kill switch is reloadable: applying a reload that flips it updates the
    // Settings and republishes the process-wide flag the renderer reads on its
    // next atlas-build. Restore the default afterward so the shared global does
    // not leak into other tests.
    let restore = synthetic_styles_enabled();

    set_synthetic_styles_enabled(true);
    let mut current = Settings {
        synthetic_styles: true,
        ..Settings::default()
    };
    let reloaded_off = Settings {
        synthetic_styles: false,
        ..Settings::default()
    };
    assert!(apply_reloadable_values(&mut current, reloaded_off));
    assert!(!current.synthetic_styles);
    assert!(!synthetic_styles_enabled());

    let reloaded_on = Settings {
        synthetic_styles: true,
        ..Settings::default()
    };
    assert!(apply_reloadable_values(&mut current, reloaded_on));
    assert!(current.synthetic_styles);
    assert!(synthetic_styles_enabled());

    set_synthetic_styles_enabled(restore);
}

#[test]
fn settings_edit_overlay_tracks_edit_revert_and_clear_diff() {
    let base = Settings {
        font_path: Some(PathBuf::from("/tmp/font-a.ttf")),
        ..Settings::default()
    };
    let mut edits = SettingsEditOverlay::new(&base);

    let changed = edits
        .apply_raw("font_size", "20")
        .expect("valid font size edit");
    assert_eq!(changed.unwrap().font_size_px, 20.0);
    assert_eq!(
        edits
            .changes()
            .iter()
            .map(|change| (change.key, change.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("font_size", "20")]
    );

    let changed = edits
        .apply_raw("font_size", "14")
        .expect("valid font size revert");
    assert_eq!(changed.unwrap().font_size_px, 14.0);
    assert!(edits.changes().is_empty());

    let changed = edits.apply_raw("font", "").expect("valid font clear");
    assert!(changed.unwrap().font_path.is_none());
    assert_eq!(
        edits
            .changes()
            .iter()
            .map(|change| (change.key, change.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("font", "")]
    );
}

#[test]
fn empty_cursor_settings_are_silent_defaults() {
    let (settings, warnings) = settings_from([(CURSOR_STYLE_ENV, "  "), (CURSOR_BLINK_ENV, "")]);
    assert_eq!(settings.cursor_style, CursorStyle::Block);
    assert_eq!(settings.cursor_blink, CursorBlink::Auto);
    assert!(warnings.is_empty());
}

// --- ODYTTY_THEME file resolution (TH2) ---------------------------------

/// Build settings with an injected `read_theme` resolver (no real filesystem).
fn settings_from_theme(
    theme_value: &str,
    read_theme: impl FnMut(&str) -> Option<String>,
) -> (Settings, Vec<String>) {
    let value = OsString::from(theme_value);
    let mut warnings = Vec::new();
    let settings = Settings::from_source(
        |key| (key == THEME_ENV).then(|| value.clone()),
        |message| warnings.push(message.to_owned()),
        |_| None,
        read_theme,
    );
    (settings, warnings)
}

#[test]
fn builtin_theme_name_does_not_consult_theme_files() {
    // A built-in name resolves to its const without touching `read_theme`.
    let (settings, warnings) = settings_from_theme("odyssey", |_| {
        panic!("read_theme must not be called for built-ins")
    });
    assert_eq!(settings.theme, Theme::ODYSSEY);
    assert!(warnings.is_empty());
}

#[test]
fn user_theme_file_contents_resolve_through_spec() {
    let theme_file = "name = Mine\nbackground = #010203\ncolor1 = #112233\ncursor = #445566\n";
    let (settings, warnings) = settings_from_theme("mine", |value| {
        assert_eq!(value, "mine");
        Some(theme_file.to_string())
    });
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    assert_eq!(settings.theme.background, (0x01, 0x02, 0x03));
    assert_eq!(settings.theme.palette[1], (0x11, 0x22, 0x33));
    assert_eq!(settings.theme.cursor, (0x44, 0x55, 0x66));
    // A user theme projects to the static placeholder name.
    assert_eq!(settings.theme.name, "custom");
    // Unspecified slots keep the plain baseline.
    assert_eq!(settings.theme.foreground, Theme::PLAIN.foreground);
}

#[test]
fn unresolvable_theme_value_falls_back_to_plain_with_warning() {
    let (settings, warnings) = settings_from_theme("does-not-exist", |_| None);
    assert_eq!(settings.theme, Theme::PLAIN);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(THEME_ENV));
    assert!(warnings[0].contains("does-not-exist"));
}

#[test]
fn malformed_theme_file_warns_but_still_loads_valid_lines() {
    // A bad line inside the theme file warns but never aborts; valid lines
    // around it still apply, and the result is a usable theme (never a crash).
    let theme_file = "background = #010203\nbroken line without equals\ncolor2 = nothex\n";
    let (settings, warnings) = settings_from_theme("partial", |_| Some(theme_file.to_string()));
    assert_eq!(settings.theme.background, (0x01, 0x02, 0x03));
    assert_eq!(settings.theme.palette[2], Theme::PLAIN.palette[2]);
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().all(|w| w.contains("theme \"partial\"")));
}

#[test]
fn resolve_theme_file_reads_path_like_values() {
    let dir = std::env::temp_dir().join(format!("odytty-th2-path-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("custom.theme");
    fs::write(&path, "background = #0a0b0c\n").unwrap();

    // A `.theme` path is read directly (theme_dir irrelevant).
    let contents = resolve_theme_file(path.to_str().unwrap(), None);
    assert_eq!(contents.as_deref(), Some("background = #0a0b0c\n"));

    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir(&dir);
}

#[test]
fn resolve_theme_file_looks_up_names_in_theme_dir() {
    let dir = std::env::temp_dir().join(format!("odytty-th2-dir-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("solar.theme"), "background = #112233\n").unwrap();

    // A bare name resolves to `<dir>/<name>.theme`.
    let contents = resolve_theme_file("solar", Some(dir.as_path()));
    assert_eq!(contents.as_deref(), Some("background = #112233\n"));
    // An unknown name resolves to nothing.
    assert_eq!(resolve_theme_file("missing", Some(dir.as_path())), None);

    let _ = fs::remove_file(dir.join("solar.theme"));
    let _ = fs::remove_dir(&dir);
}

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::Theme;

use super::reload::{ConfigFileFingerprint, ConfigPollEvent};
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
            keybinds = ctrl+shift+y=copy;alt+space=paste
            cursor_blink = steady
            native_autoclose_ms = 600
        "#,
        [],
    );

    assert_eq!(settings.font_size_px, MAX_FONT_SIZE_PX);
    assert_eq!(settings.text_gamma, MIN_TEXT_GAMMA);
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
        "ctrl+shift+y=copy; SUPER+F=search, Shift+PageDown=scroll-down",
    )]);

    assert_eq!(settings.key_bindings.len(), 3);
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
fn apply_reloadable_values_publishes_synthetic_styles_global() {
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
fn empty_cursor_settings_are_silent_defaults() {
    let (settings, warnings) = settings_from([(CURSOR_STYLE_ENV, "  "), (CURSOR_BLINK_ENV, "")]);
    assert_eq!(settings.cursor_style, CursorStyle::Block);
    assert_eq!(settings.cursor_blink, CursorBlink::Auto);
    assert!(warnings.is_empty());
}

// SPDX-License-Identifier: GPL-3.0-only
//! Focused tests for settings overlay editing, themed UI roles, theme file
//! resolution, and the `apply_reloadable_values` path — kept out of the large
//! `legacy` module so that file stays under the source-size cap.

use super::*;

/// Build a `Settings` from a flat env-style key/value list, collecting any
/// warnings. Mirrors the `legacy` module's private helper; kept local so this
/// submodule is self-contained.
fn settings_from<const N: usize>(values: [(&str, &str); N]) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let settings = Settings::from_source(
        |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
        },
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    (settings, warnings)
}

/// Build a `Settings` from a config-file string plus a flat env-style list,
/// collecting any warnings. Mirrors the `legacy` module's private helper.
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
fn custom_theme_file_keeps_settings_editable() {
    // C4 regression: a theme loaded from a FILE projects to the placeholder
    // runtime name "custom", which resolves to no built-in and no file on a
    // later re-parse. Persisting that placeholder as the writeback baseline made
    // the whole settings panel read-only for custom-theme users — the fallback
    // warning was promoted to a hard error on every edit. Preserving the raw
    // theme-config string keeps the writeback round-tripping the real file path,
    // and decoupling non-fatal parse warnings from edit validation keeps every
    // other key editable.
    let path = std::env::temp_dir().join(format!(
        "odytty-c4-{}-{}.theme",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    // The unknown key makes the file resolve WITH a tolerable parse warning,
    // so this also covers that a theme warning does not block other edits.
    std::fs::write(
        &path,
        "name = My Personal Theme\nbackground = #010101\nunknown_key = 1\n",
    )
    .expect("write temp theme file");
    let path_str = path.to_string_lossy().into_owned();

    // Load settings with the file-path theme active. A path-like value is read
    // directly by the real resolver, so this exercises the true file path.
    let mut values = Settings::default().to_edit_values();
    values.insert(THEME_ENV, path_str.clone());
    let base = Settings::from_edit_values(&values).expect("file theme loads");
    assert_eq!(
        base.theme.name, "custom",
        "a file-loaded theme carries the placeholder name"
    );
    assert_eq!(
        base.theme_config.as_deref(),
        Some(path_str.as_str()),
        "the raw config path is preserved for round-trip"
    );
    // The writeback baseline round-trips the file path, never the placeholder.
    assert_eq!(
        base.to_edit_values().get(THEME_ENV).map(String::as_str),
        Some(path_str.as_str()),
        "to_edit_values persists the real path, not \"custom\""
    );

    // Editing an unrelated key must succeed (previously rejected outright) and
    // preserve the custom theme.
    let mut overlay = SettingsEditOverlay::new(&base);
    let changed = overlay
        .apply_raw("font_size", "24")
        .expect("editing font_size must not be blocked by a custom theme");
    assert!(changed.is_some(), "the edit changed settings");
    let after = overlay.settings();
    assert_eq!(after.font_size_px, 24.0, "the unrelated edit applied");
    assert_eq!(
        after.theme.name, "custom",
        "the custom theme is preserved across the edit"
    );
    assert_eq!(
        after.theme_config.as_deref(),
        Some(path_str.as_str()),
        "the theme file path still round-trips after the edit"
    );

    let _ = std::fs::remove_file(&path);
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
    // The `font` config key tracks the RAW explicit override
    // (`explicit_font_path`), not the effective `font_path` that family
    // resolution may populate (RC4). Seed the explicit field so the clear-diff
    // exercises the real `font`-key writeback path.
    let base = Settings {
        explicit_font_path: Some(PathBuf::from("/tmp/font-a.ttf")),
        font_path: Some(PathBuf::from("/tmp/font-a.ttf")),
        ..Settings::default()
    };
    let mut edits = SettingsEditOverlay::new(&base);

    let changed = edits
        .apply_raw("font_size", "24")
        .expect("valid font size edit");
    assert_eq!(changed.unwrap().font_size_px, 24.0);
    assert_eq!(
        edits
            .changes()
            .iter()
            .map(|change| (change.key, change.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("font_size", "24")]
    );

    let default_font_size = crate::settings::DEFAULT_FONT_SIZE_PX;
    let changed = edits
        .apply_raw("font_size", &default_font_size.to_string())
        .expect("valid font size revert");
    assert_eq!(changed.unwrap().font_size_px, default_font_size);
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
    assert_eq!(settings.cursor_blink, CursorBlink::On);
    assert!(warnings.is_empty());
}

// --- ODYTTY_THEME file resolution (TH2) ---------------------------------

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
fn unresolvable_theme_value_falls_back_to_default_with_warning() {
    let (settings, warnings) = settings_from_theme("does-not-exist", |_| None);
    assert_eq!(settings.theme, DEFAULT_THEME);
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

#[test]
fn rebase_onto_adopts_external_theme_and_preserves_dirty_edit() {
    let base = Settings {
        theme: Theme::PLAIN,
        ..Settings::default()
    };
    let mut edits = SettingsEditOverlay::new(&base);

    edits
        .apply_raw("font_size", "24")
        .expect("valid font size edit");
    assert_eq!(edits.changes().len(), 1);
    assert_eq!(edits.settings().theme, Theme::PLAIN);

    edits.rebase_onto(&Settings {
        theme: Theme::ODYSSEY,
        ..Settings::default()
    });

    assert_eq!(
        edits
            .changes()
            .iter()
            .map(|c| (c.key, c.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("font_size", "24")],
        "pending dirty edit must survive rebase"
    );
    assert_eq!(edits.settings().theme, Theme::ODYSSEY);
    assert_eq!(edits.changed_count(), 1, "theme must not count as dirty");
}

#[test]
fn rebase_onto_then_panel_commit_does_not_revert_theme() {
    let base = Settings {
        theme: Theme::PLAIN,
        ..Settings::default()
    };
    let mut edits = SettingsEditOverlay::new(&base);

    edits.rebase_onto(&Settings {
        theme: Theme::ODYSSEY_NOIR,
        ..Settings::default()
    });
    assert_eq!(edits.settings().theme, Theme::ODYSSEY_NOIR);
    assert!(edits.changes().is_empty(), "clean rebase, no dirty edits");

    let applied = edits
        .apply_raw("font_size", "16")
        .expect("valid font size edit");
    assert_eq!(applied.unwrap().font_size_px, 16.0);

    assert_eq!(
        edits.settings().theme,
        Theme::ODYSSEY_NOIR,
        "panel commit must not revert the externally-applied theme"
    );
    assert_eq!(
        edits.changes().iter().map(|c| c.key).collect::<Vec<_>>(),
        vec!["font_size"]
    );
}

#[test]
fn rebase_then_mark_saved_keeps_theme() {
    let base = Settings {
        theme: Theme::PLAIN,
        ..Settings::default()
    };
    let mut edits = SettingsEditOverlay::new(&base);
    edits.rebase_onto(&Settings {
        theme: Theme::ODYSSEY,
        ..Settings::default()
    });
    edits.apply_raw("font_size", "18").expect("valid edit");
    assert_eq!(edits.changed_count(), 1);

    edits.mark_saved();

    assert_eq!(edits.settings().theme, Theme::ODYSSEY);
    assert!(edits.changes().is_empty(), "save clears dirty edits");
}

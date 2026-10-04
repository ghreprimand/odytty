// SPDX-License-Identifier: GPL-3.0-only
//! Alternate-zero (`font_zero`) setting coverage: off by default, parsed from
//! env and `odytty.conf`, a reloadable Fonts row, published on reload, and
//! persisted under its canonical key.

use super::*;

fn settings_from_config(config_contents: &str) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let config = ConfigValues::parse(config_contents, |message| warnings.push(message));
    let settings = Settings::from_source(
        |key| config.get(key).cloned(),
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    (settings, warnings)
}

#[test]
fn font_zero_defaults_off_and_parses_env_config_and_aliases() {
    let (default, warnings) = settings_from_config("");
    assert!(!default.font_zero);
    assert!(warnings.is_empty());

    for line in ["font_zero = on", "zero = on", "alternate_zero = true"] {
        let (settings, warnings) = settings_from_config(line);
        assert!(settings.font_zero, "{line}");
        assert!(warnings.is_empty(), "{line}: {warnings:?}");
    }

    let from_env = Settings::from_source(
        |key| (key == FONT_ZERO_ENV).then(|| OsString::from("on")),
        |_| {},
        |_| None,
        |_| None,
    );
    assert!(from_env.font_zero);

    let (invalid, warnings) = settings_from_config("font_zero = slashed");
    assert!(!invalid.font_zero, "an invalid value keeps the default");
    assert!(!warnings.is_empty(), "an invalid value warns");
}

#[test]
fn font_zero_maps_to_one_canonical_config_key() {
    assert_eq!(config_key_to_env("font_zero"), Some(FONT_ZERO_ENV));
    assert_eq!(env_to_config_key(FONT_ZERO_ENV), Some("font_zero"));
}

#[test]
fn font_zero_has_a_reloadable_fonts_row() {
    let row = Settings::default()
        .setting_info()
        .into_iter()
        .find(|row| row.key == "font_zero")
        .expect("font_zero panel row");
    assert_eq!(row.group, "Font");
    assert_eq!(row.env, FONT_ZERO_ENV);
    assert_eq!(row.value, "off");
    assert!(row.reloadable);
    assert!(matches!(row.kind, SettingKind::Bool));
    assert!(row.description.contains("Off by default"));
}

#[test]
fn reload_publishes_font_zero_without_changing_other_settings() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let mut current = Settings::default();
    let mut reloaded = current.clone();
    reloaded.font_zero = true;
    assert!(apply_reloadable_values(&mut current, reloaded));
    assert!(current.font_zero);
    assert!(font_zero_enabled());
    let expected = Settings {
        font_zero: true,
        ..Settings::default()
    };
    assert_eq!(current, expected, "only font_zero moved");
}

#[test]
fn font_zero_writes_back_under_its_canonical_key() {
    let dir =
        std::env::temp_dir().join(format!("odytty-font-zero-writeback-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(CONFIG_FILE_NAME);
    fs::write(&path, "# kept\n").unwrap();
    let mut edits = SettingsEditOverlay::new(&Settings::default());
    edits.apply_raw("font_zero", "on").unwrap();
    let result = write_settings_changes_to_path(&path, &edits.changes()).unwrap();
    assert_eq!(result.changed, 1);
    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.contains("font_zero = on"), "{saved}");
    let (reloaded, warnings) = settings_from_config(&saved);
    assert!(reloaded.font_zero);
    assert!(warnings.is_empty());
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir(dir);
}

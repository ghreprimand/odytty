// SPDX-License-Identifier: GPL-3.0-only
//! Programming-ligature setting coverage.

use super::*;

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
fn ligatures_default_off_and_parse_env_and_config() {
    let (default, warnings) = settings_from([]);
    assert!(!default.ligatures);
    assert!(warnings.is_empty());

    let (from_env, warnings) = settings_from([(LIGATURES_ENV, "on")]);
    assert!(from_env.ligatures);
    assert!(warnings.is_empty());

    let (from_config, warnings) = settings_from_config("ligatures = true");
    assert!(from_config.ligatures);
    assert!(warnings.is_empty());
}

#[test]
fn ligatures_have_a_reloadable_panel_row() {
    let row = Settings::default()
        .setting_info()
        .into_iter()
        .find(|row| row.key == "ligatures")
        .expect("ligatures panel row");
    assert_eq!(row.env, LIGATURES_ENV);
    assert_eq!(row.value, "off");
    assert!(row.reloadable);
    assert!(row.description.contains("ASCII"));
}

#[test]
fn reload_publishes_ligature_switch_without_changing_other_settings() {
    let _guard = RELOAD_GLOBAL_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let restore = ligatures_enabled();
    let mut current = Settings::default();
    let mut reloaded = current.clone();
    reloaded.ligatures = true;
    assert!(apply_reloadable_values(&mut current, reloaded));
    assert!(current.ligatures);
    assert!(ligatures_enabled());
    set_ligatures_enabled(restore);
}

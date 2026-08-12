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
fn ligatures_default_on_and_explicit_env_and_config_opt_out() {
    let (default, warnings) = settings_from([]);
    assert!(default.ligatures);
    assert!(warnings.is_empty());

    let (from_env, warnings) = settings_from([(LIGATURES_ENV, "off")]);
    assert!(!from_env.ligatures);
    assert!(warnings.is_empty());

    let (from_config, warnings) = settings_from_config("ligatures = off");
    assert!(!from_config.ligatures);
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
    assert_eq!(row.value, "on");
    assert!(row.reloadable);
    assert!(row.description.contains("calt") || row.description.contains("liga"));
}

#[test]
fn ss01_and_ss02_default_off_and_reload_through_config() {
    let (default, warnings) = settings_from([]);
    assert!(!default.ligature_ss01);
    assert!(!default.ligature_ss02);
    assert!(warnings.is_empty());

    let (from_env, warnings) =
        settings_from([(LIGATURE_SS01_ENV, "on"), (LIGATURE_SS02_ENV, "on")]);
    assert!(from_env.ligature_ss01);
    assert!(from_env.ligature_ss02);
    assert!(warnings.is_empty());

    let (from_config, warnings) = settings_from_config("ss01 = on\nss02 = on");
    assert!(from_config.ligature_ss01);
    assert!(from_config.ligature_ss02);
    assert!(warnings.is_empty());
}

#[test]
fn ss01_ss02_have_reloadable_panel_rows() {
    let info = Settings::default().setting_info();
    for key in ["ss01", "ss02"] {
        let row = info
            .iter()
            .find(|row| row.key == key)
            .unwrap_or_else(|| panic!("{key} panel row"));
        assert!(row.reloadable);
        assert_eq!(row.value, "off");
        assert!(row.description.contains("Off by default"));
    }
}

#[test]
fn reload_publishes_ss01_ss02_without_changing_other_settings() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let mut current = Settings::default();
    let mut reloaded = current.clone();
    reloaded.ligature_ss01 = true;
    reloaded.ligature_ss02 = true;
    assert!(apply_reloadable_values(&mut current, reloaded));
    assert!(current.ligature_ss01);
    assert!(current.ligature_ss02);
    assert!(ligature_ss01_enabled());
    assert!(ligature_ss02_enabled());
}

#[test]
fn reload_publishes_ligature_switch_without_changing_other_settings() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let mut current = Settings::default();
    let mut reloaded = current.clone();
    reloaded.ligatures = false;
    assert!(apply_reloadable_values(&mut current, reloaded));
    assert!(!current.ligatures);
    assert!(!ligatures_enabled());
}

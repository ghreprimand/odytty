// SPDX-License-Identifier: GPL-3.0-only
//! Kitty named-transport policy setting coverage.

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

#[test]
fn kitty_named_transports_default_off_with_explicit_env_and_config_opt_in() {
    let (default, warnings) = settings_from([]);
    assert!(!default.kitty_named_transports);
    assert!(warnings.is_empty());

    let (from_env, warnings) = settings_from([(KITTY_NAMED_TRANSPORTS_ENV, "on")]);
    assert!(from_env.kitty_named_transports);
    assert!(warnings.is_empty());

    let mut warnings = Vec::new();
    let config = ConfigValues::parse("kitty_named_transports = on", |message| {
        warnings.push(message)
    });
    let from_config = Settings::from_source(
        |key| config.get(key).cloned(),
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    assert!(from_config.kitty_named_transports);
    assert!(warnings.is_empty());
}

#[test]
fn kitty_named_transports_have_a_reloadable_panel_row() {
    let row = Settings::default()
        .setting_info()
        .into_iter()
        .find(|row| row.key == "kitty_named_transports")
        .expect("Kitty named-transport panel row");
    assert_eq!(row.env, KITTY_NAMED_TRANSPORTS_ENV);
    assert_eq!(row.value, "off");
    assert_eq!(row.options, &["on", "off"]);
    assert!(row.reloadable);
    assert!(row.description.contains("plain SSH"));
}

#[test]
fn kitty_named_transport_policy_reloads_without_changing_other_settings() {
    let _guard = RELOAD_GLOBAL_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut current = Settings::default();
    let mut reloaded = current.clone();
    reloaded.kitty_named_transports = true;
    assert!(apply_reloadable_values(&mut current, reloaded));
    assert!(current.kitty_named_transports);
}

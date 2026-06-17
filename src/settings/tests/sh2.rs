// SPDX-License-Identifier: GPL-3.0-only
//! Focused tests for the SH2 `command_status_gutter` setting, kept out of the
//! large `legacy` module so that file stays under the source-size cap.

use super::*;

/// Build a `Settings` from a flat env-style key/value list, collecting any
/// warnings. Mirrors the sibling submodules' private helper; kept local so this
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

#[test]
fn command_status_gutter_defaults_off() {
    let (settings, warnings) = settings_from([]);
    assert!(!settings.command_status_gutter);
    assert!(warnings.is_empty());
}

#[test]
fn command_status_gutter_parses_on_and_off() {
    let (on, _) = settings_from([(COMMAND_STATUS_GUTTER_ENV, "on")]);
    assert!(on.command_status_gutter);

    let (off, _) = settings_from([(COMMAND_STATUS_GUTTER_ENV, "off")]);
    assert!(!off.command_status_gutter);
}

#[test]
fn command_status_gutter_round_trips_through_config_key() {
    // The config-file alias maps to the env key and back, so a config-file entry
    // is honoured and the overlay write-back stays consistent.
    assert_eq!(
        config_key_to_env("command_status_gutter"),
        Some(COMMAND_STATUS_GUTTER_ENV)
    );
    assert_eq!(
        env_to_config_key(COMMAND_STATUS_GUTTER_ENV),
        Some("command_status_gutter")
    );
}

#[test]
fn command_status_gutter_is_persisted_in_edit_values() {
    // The overlay write path includes the gutter so toggling an unrelated
    // setting and saving never silently drops a user's gutter choice.
    let settings = Settings {
        command_status_gutter: true,
        ..Settings::default()
    };
    assert_eq!(
        settings.to_edit_values().get(COMMAND_STATUS_GUTTER_ENV),
        Some(&"on".to_owned())
    );
}

#[test]
fn command_status_gutter_is_live_reloadable() {
    let _guard = RELOAD_GLOBAL_TEST_LOCK.lock().unwrap();
    let mut current = Settings::default();
    assert!(!current.command_status_gutter);

    let reloaded = Settings {
        command_status_gutter: true,
        ..Settings::default()
    };
    assert!(apply_reloadable_values(&mut current, reloaded));
    assert!(
        current.command_status_gutter,
        "a live reload flips the gutter on"
    );
}

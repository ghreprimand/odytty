// SPDX-License-Identifier: GPL-3.0-only
//! Local automation endpoint setting coverage.

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
fn automation_endpoint_defaults_off_and_parses_opt_in() {
    let (default, warnings) = settings_from([]);
    assert!(!default.automation_endpoint);
    assert!(warnings.is_empty());

    let (enabled, warnings) = settings_from([(AUTOMATION_ENDPOINT_ENV, "on")]);
    assert!(enabled.automation_endpoint);
    assert!(warnings.is_empty());
}

#[test]
fn automation_endpoint_round_trips_config_panel_and_reload() {
    assert_eq!(
        config_key_to_env("automation_endpoint"),
        Some(AUTOMATION_ENDPOINT_ENV)
    );
    assert_eq!(
        env_to_config_key(AUTOMATION_ENDPOINT_ENV),
        Some("automation_endpoint")
    );

    let mut warnings = Vec::new();
    let config = ConfigValues::parse("automation_endpoint = on", |message| warnings.push(message));
    let enabled = Settings::from_source(
        |key| config.get(key).cloned(),
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    assert!(enabled.automation_endpoint);
    assert!(warnings.is_empty());
    assert_eq!(
        enabled.to_edit_values().get(AUTOMATION_ENDPOINT_ENV),
        Some(&"on".to_owned())
    );

    let row = enabled
        .setting_info()
        .into_iter()
        .find(|row| row.key == "automation_endpoint")
        .expect("automation endpoint panel row");
    assert_eq!(row.value, "on");
    assert!(row.reloadable);

    let mut current = Settings::default();
    assert!(apply_reloadable_values(&mut current, enabled));
    assert!(current.automation_endpoint);
}

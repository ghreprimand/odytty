// SPDX-License-Identifier: GPL-3.0-only
//! OSC 52 clipboard-write policy setting coverage.

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
fn osc52_write_defaults_ask_and_parses_every_policy() {
    let (default, warnings) = settings_from([]);
    assert_eq!(default.osc52_write, Osc52WritePolicy::Ask);
    assert!(warnings.is_empty());

    for (raw, expected) in [
        ("off", Osc52WritePolicy::Off),
        ("ask", Osc52WritePolicy::Ask),
        ("on", Osc52WritePolicy::On),
    ] {
        let (settings, warnings) = settings_from([(OSC52_WRITE_ENV, raw)]);
        assert_eq!(settings.osc52_write, expected);
        assert!(warnings.is_empty());
    }
}

#[test]
fn osc52_write_config_panel_and_reload_are_tri_state() {
    let _render_globals = crate::test_lock::render_globals_lock();
    let mut warnings = Vec::new();
    // Use a non-default value (`on`) so the reload is a real change over the
    // `ask` default and `apply_reloadable_values` reports it.
    let config = ConfigValues::parse("osc52_write = on", |message| warnings.push(message));
    let reloaded = Settings::from_source(
        |key| config.get(key).cloned(),
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    assert_eq!(reloaded.osc52_write, Osc52WritePolicy::On);
    assert!(warnings.is_empty());

    let row = reloaded
        .setting_info()
        .into_iter()
        .find(|row| row.key == "osc52_write")
        .expect("OSC 52 write panel row");
    assert_eq!(row.env, OSC52_WRITE_ENV);
    assert_eq!(row.value, "on");
    assert_eq!(row.options, &["off", "ask", "on"]);
    assert!(row.reloadable);

    let mut current = Settings::default();
    assert!(apply_reloadable_values(&mut current, reloaded));
    assert_eq!(current.osc52_write, Osc52WritePolicy::On);
    assert!(
        !current.osc52_read,
        "write policy is independent from reads"
    );
}

#[test]
fn invalid_osc52_write_policy_warns_and_falls_back_ask() {
    let (settings, warnings) = settings_from([(OSC52_WRITE_ENV, "sometimes")]);
    assert_eq!(settings.osc52_write, Osc52WritePolicy::Ask);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(OSC52_WRITE_ENV));
}

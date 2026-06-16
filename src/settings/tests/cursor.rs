// SPDX-License-Identifier: GPL-3.0-only
//! Focused tests for cursor presentation settings (the ID1 `cursor_glow` knob
//! and its config aliases), kept out of the large `legacy` module so that file
//! stays under the source-size cap.

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

/// Build a `Settings` from a config-file body (so config-key aliases resolve
/// through the normalizer) plus an optional env overlay.
fn settings_from_config<const N: usize>(
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

#[test]
fn cursor_glow_defaults_off() {
    let (settings, warnings) = settings_from([]);
    assert!(!settings.cursor_glow, "cursor glow is off by default");
    assert!(warnings.is_empty());
}

#[test]
fn cursor_glow_parses_on_via_env() {
    let (on, warnings) = settings_from([(CURSOR_GLOW_ENV, "on")]);
    assert!(on.cursor_glow);
    assert!(warnings.is_empty());

    let (off, _) = settings_from([(CURSOR_GLOW_ENV, "off")]);
    assert!(!off.cursor_glow);
}

#[test]
fn cursor_glow_round_trips_through_config_key_and_aliases() {
    let (settings, warnings) = settings_from_config("cursor_glow = true", []);
    assert!(settings.cursor_glow);
    assert!(warnings.is_empty());

    // The aliases authored in config.rs resolve through the key normalizer.
    for alias in ["cursorglow", "cursorhalo", "cursorbloom"] {
        let (settings, warnings) = settings_from_config(&format!("{alias} = yes"), []);
        assert!(settings.cursor_glow, "alias `{alias}` must enable the glow");
        assert!(warnings.is_empty(), "alias `{alias}` must not warn");
    }
}

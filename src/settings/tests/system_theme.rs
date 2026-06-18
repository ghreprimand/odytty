// SPDX-License-Identifier: GPL-3.0-only
//! Focused tests for the `theme = system` alias (OS-THEME alias): parse,
//! writeback round-trip, interaction with explicit OS-theme overrides, and
//! default dark/light resolution behavior.

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
fn system_alias_enables_follow_and_sets_flag() {
    let (settings, warnings) = settings_from([(THEME_ENV, SYSTEM_THEME_NAME)]);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert!(settings.theme_is_system, "theme_is_system must be set");
    assert!(
        settings.follow_os_theme,
        "follow_os_theme must be forced on by the alias"
    );
}

#[test]
fn system_alias_is_case_insensitive() {
    let (settings, _) = settings_from([(THEME_ENV, "System")]);
    assert!(settings.theme_is_system);
    assert!(settings.follow_os_theme);
}

#[test]
fn plain_theme_does_not_set_the_alias() {
    let (settings, _) = settings_from([(THEME_ENV, "plain")]);
    assert!(!settings.theme_is_system);
    assert!(!settings.follow_os_theme);
}

#[test]
fn unset_theme_is_not_system() {
    let (settings, _) = settings_from([]);
    assert!(!settings.theme_is_system);
}

#[test]
fn system_alias_writeback_round_trips() {
    let (settings, _) = settings_from([(THEME_ENV, SYSTEM_THEME_NAME)]);
    let values = settings.to_edit_values();
    assert_eq!(
        values.get(THEME_ENV).map(String::as_str),
        Some(SYSTEM_THEME_NAME),
        "writeback must preserve the system token, not the internal fallback"
    );
}

#[test]
fn system_alias_respects_explicit_os_dark_override() {
    let (settings, _) = settings_from([
        (THEME_ENV, SYSTEM_THEME_NAME),
        (OS_THEME_DARK_ENV, "monokai"),
    ]);
    assert!(settings.theme_is_system);
    assert_eq!(
        settings.os_theme_dark.as_deref(),
        Some("monokai"),
        "explicit override must be preserved verbatim"
    );
    assert!(
        settings.os_theme_light.is_none(),
        "light direction stays unset when only dark was overridden"
    );
}

#[test]
fn system_alias_keeps_explicit_follow_off_display_value() {
    // The alias forces follow_os_theme on internally, but the explicit
    // follow_os_theme setting remains a separate, user-editable row. Setting
    // the alias must not alter the authored theme name.
    let (settings, _) = settings_from([(THEME_ENV, SYSTEM_THEME_NAME)]);
    assert_eq!(
        settings.theme, DEFAULT_THEME,
        "authored theme stays the default under the alias"
    );
}

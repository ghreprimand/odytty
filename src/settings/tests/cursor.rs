// SPDX-License-Identifier: GPL-3.0-only
//! Focused tests for cursor presentation and reduced-motion settings, kept out
//! of the large `legacy` module so that file stays under the source-size cap.

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

#[test]
fn reduced_motion_defaults_parses_persists_and_has_a_panel_row() {
    let (default, warnings) = settings_from([]);
    assert!(!default.reduced_motion, "reduced motion is off by default");
    assert!(warnings.is_empty());

    let (from_env, warnings) = settings_from([(REDUCED_MOTION_ENV, "on")]);
    assert!(from_env.reduced_motion);
    assert!(warnings.is_empty());

    let (from_config, warnings) = settings_from_config("reduced_motion = yes", []);
    assert!(from_config.reduced_motion);
    assert!(warnings.is_empty());
    assert_eq!(
        from_config.to_edit_values().get(REDUCED_MOTION_ENV),
        Some(&"on".to_owned())
    );

    let row = from_config
        .setting_info()
        .into_iter()
        .find(|row| row.key == "reduced_motion")
        .expect("reduced-motion panel row");
    assert_eq!(row.group, "Accessibility");
    assert_eq!(row.env, REDUCED_MOTION_ENV);
    assert_eq!(row.value, "on");
}

#[test]
fn reduced_motion_live_reload_preserves_individual_preferences() {
    let _guard = RELOAD_GLOBAL_TEST_LOCK.lock().unwrap();
    let mut current = Settings {
        cursor_easing: true,
        cursor_glow: true,
        cursor_trail: true,
        cursor_motion: true,
        new_output_fade: true,
        ..Settings::default()
    };
    let reloaded = Settings {
        reduced_motion: true,
        ..current.clone()
    };

    assert!(apply_reloadable_values(&mut current, reloaded));
    assert!(current.reduced_motion);
    assert!(current.cursor_easing);
    assert!(current.cursor_glow);
    assert!(current.cursor_trail);
    assert!(current.cursor_motion);
    assert!(current.new_output_fade);
}

#[test]
fn cursor_trail_strength_parses_config_env_and_edit_round_trip() {
    for (raw, expected) in [
        ("subtle", CursorTrailStrength::Subtle),
        ("balanced", CursorTrailStrength::Balanced),
        ("expressive", CursorTrailStrength::Expressive),
    ] {
        let (from_config, warnings) =
            settings_from_config(&format!("cursor_trail_strength = {raw}"), []);
        assert!(warnings.is_empty());
        assert_eq!(from_config.cursor_trail_strength, expected);
        assert_eq!(
            from_config.to_edit_values().get(CURSOR_TRAIL_STRENGTH_ENV),
            Some(&raw.to_owned())
        );

        let (from_env, warnings) = settings_from_config(
            "cursor_trail_strength = subtle",
            [(CURSOR_TRAIL_STRENGTH_ENV, raw)],
        );
        assert!(warnings.is_empty());
        assert_eq!(from_env.cursor_trail_strength, expected);
    }

    let (fallback, warnings) = settings_from_config("cursor_trail_strength = loud", []);
    assert_eq!(
        fallback.cursor_trail_strength,
        CursorTrailStrength::Balanced
    );
    assert_eq!(warnings.len(), 1);
}

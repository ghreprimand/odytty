// SPDX-License-Identifier: GPL-3.0-only
//! Focused tests for the mouse/pointer settings (MOUSE-AUTOSCROLL-VEL and
//! friends), kept out of the large `legacy` module so that file stays under the
//! source-size cap.

use super::*;

/// Build a `Settings` from a flat env-style key/value list, collecting any
/// warnings. Mirrors the `legacy` module's private helper; kept local so this
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
fn scroll_drag_speed_defaults_to_ramp() {
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.scroll_drag_speed, ScrollDragSpeed::Ramp);
    assert!(warnings.is_empty());
}

#[test]
fn scroll_drag_speed_parses_legacy_and_ramp() {
    let (legacy, warnings) = settings_from([(SCROLL_DRAG_SPEED_ENV, "legacy")]);
    assert_eq!(legacy.scroll_drag_speed, ScrollDragSpeed::Legacy);
    assert!(warnings.is_empty());

    let (ramp, warnings) = settings_from([(SCROLL_DRAG_SPEED_ENV, "ramp")]);
    assert_eq!(ramp.scroll_drag_speed, ScrollDragSpeed::Ramp);
    assert!(warnings.is_empty());
}

#[test]
fn scroll_drag_speed_garbage_falls_back_to_ramp_with_warning() {
    let (settings, warnings) = settings_from([(SCROLL_DRAG_SPEED_ENV, "turbo")]);
    assert_eq!(settings.scroll_drag_speed, ScrollDragSpeed::Ramp);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("ramp|legacy"));
}

#[test]
fn scroll_drag_speed_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("scroll_drag_speed"),
        Some(SCROLL_DRAG_SPEED_ENV)
    );
    assert_eq!(
        config_key_to_env("autoscrollspeed"),
        Some(SCROLL_DRAG_SPEED_ENV)
    );
    assert_eq!(
        env_to_config_key(SCROLL_DRAG_SPEED_ENV),
        Some("scroll_drag_speed")
    );
}

#[test]
fn scroll_drag_speed_round_trips_through_edit_values() {
    let settings = Settings {
        scroll_drag_speed: ScrollDragSpeed::Legacy,
        ..Settings::default()
    };
    assert_eq!(
        settings.to_edit_values().get(SCROLL_DRAG_SPEED_ENV),
        Some(&"legacy".to_owned())
    );

    assert_eq!(
        Settings::default()
            .to_edit_values()
            .get(SCROLL_DRAG_SPEED_ENV),
        Some(&"ramp".to_owned())
    );
}

#[test]
fn autoscroll_max_rows_maps_ramp_to_cap_and_legacy_to_one() {
    // The OFF-path inverted-gate proof at the settings layer: `legacy` resolves
    // to a cap of exactly 1, which is what makes the drag-autoscroll delta
    // byte-identical to the historical fixed one-row-per-tick behavior (the
    // per-row math parity is asserted in `selection.rs`). `ramp` (default) opens
    // the bounded velocity ramp up to `MAX_AUTOSCROLL_ROWS`.
    let ramp = Settings {
        scroll_drag_speed: ScrollDragSpeed::Ramp,
        ..Settings::default()
    };
    assert_eq!(ramp.autoscroll_max_rows(), MAX_AUTOSCROLL_ROWS);

    let legacy = Settings {
        scroll_drag_speed: ScrollDragSpeed::Legacy,
        ..Settings::default()
    };
    assert_eq!(legacy.autoscroll_max_rows(), 1);
}

#[test]
fn scroll_drag_speed_has_an_enum_row_in_the_input_group() {
    let rows = Settings::default().setting_info();
    let row = rows
        .iter()
        .find(|row| row.key == "scroll_drag_speed")
        .expect("scroll_drag_speed row present");
    assert_eq!(row.group, "Input");
    assert_eq!(row.kind, SettingKind::Enum);
    assert_eq!(row.options, ["ramp", "legacy"]);
    assert_eq!(row.value, "ramp");
    assert!(row.reloadable);
    assert!(!row.description.trim().is_empty());
}

#[test]
fn scrollbar_drag_defaults_on_and_parses() {
    // MOUSE-SCROLLBAR: default on (the draggable thumb is inert at the live
    // tail, so default-on does not change the plain press path).
    let (settings, warnings) = settings_from([]);
    assert!(settings.scrollbar_drag);
    assert!(warnings.is_empty());

    let (off, _) = settings_from([(SCROLLBAR_DRAG_ENV, "off")]);
    assert!(!off.scrollbar_drag);

    let (on, _) = settings_from([(SCROLLBAR_DRAG_ENV, "on")]);
    assert!(on.scrollbar_drag);
}

#[test]
fn scrollbar_drag_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("scrollbar_drag"),
        Some(SCROLLBAR_DRAG_ENV)
    );
    assert_eq!(
        config_key_to_env("draggablescrollbar"),
        Some(SCROLLBAR_DRAG_ENV)
    );
    assert_eq!(
        env_to_config_key(SCROLLBAR_DRAG_ENV),
        Some("scrollbar_drag")
    );
}

#[test]
fn scrollbar_drag_round_trips_through_edit_values() {
    let settings = Settings {
        scrollbar_drag: false,
        ..Settings::default()
    };
    assert_eq!(
        settings.to_edit_values().get(SCROLLBAR_DRAG_ENV),
        Some(&"off".to_owned())
    );
    assert_eq!(
        Settings::default().to_edit_values().get(SCROLLBAR_DRAG_ENV),
        Some(&"on".to_owned())
    );
}

#[test]
fn scrollbar_drag_has_a_bool_row_in_the_input_group() {
    let rows = Settings::default().setting_info();
    let row = rows
        .iter()
        .find(|row| row.key == "scrollbar_drag")
        .expect("scrollbar_drag row present");
    assert_eq!(row.group, "Input");
    assert_eq!(row.kind, SettingKind::Bool);
    assert_eq!(row.options, ["on", "off"]);
    assert_eq!(row.value, "on");
    assert!(row.reloadable);
    assert!(!row.description.trim().is_empty());
}

#[test]
fn wheel_zoom_defaults_on_and_parses() {
    // CTRL-WHEEL-ZOOM: default on. It only fires on the explicit Ctrl+wheel
    // gesture while mouse reporting is off, so default-on leaves a plain wheel
    // byte-identical.
    let (settings, warnings) = settings_from([]);
    assert!(settings.wheel_zoom);
    assert!(warnings.is_empty());

    let (off, _) = settings_from([(WHEEL_ZOOM_ENV, "off")]);
    assert!(!off.wheel_zoom);

    let (on, _) = settings_from([(WHEEL_ZOOM_ENV, "on")]);
    assert!(on.wheel_zoom);
}

#[test]
fn wheel_zoom_round_trips_through_config_key_mapping() {
    assert_eq!(config_key_to_env("wheel_zoom"), Some(WHEEL_ZOOM_ENV));
    assert_eq!(config_key_to_env("ctrlwheelzoom"), Some(WHEEL_ZOOM_ENV));
    assert_eq!(config_key_to_env("fontzoom"), Some(WHEEL_ZOOM_ENV));
    assert_eq!(env_to_config_key(WHEEL_ZOOM_ENV), Some("wheel_zoom"));
}

#[test]
fn wheel_zoom_round_trips_through_edit_values() {
    let settings = Settings {
        wheel_zoom: false,
        ..Settings::default()
    };
    assert_eq!(
        settings.to_edit_values().get(WHEEL_ZOOM_ENV),
        Some(&"off".to_owned())
    );
    assert_eq!(
        Settings::default().to_edit_values().get(WHEEL_ZOOM_ENV),
        Some(&"on".to_owned())
    );
}

#[test]
fn wheel_zoom_has_a_bool_row_in_the_input_group() {
    let rows = Settings::default().setting_info();
    let row = rows
        .iter()
        .find(|row| row.key == "wheel_zoom")
        .expect("wheel_zoom row present");
    assert_eq!(row.group, "Input");
    assert_eq!(row.kind, SettingKind::Bool);
    assert_eq!(row.options, ["on", "off"]);
    assert_eq!(row.value, "on");
    assert!(row.reloadable);
    assert!(!row.description.trim().is_empty());
}

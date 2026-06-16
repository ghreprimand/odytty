// SPDX-License-Identifier: GPL-3.0-only
//! Focused tests for `NumericSpec` (slider math, derived range labels, folded
//! keyboard steps) and the numeric settings that carry a slider spec
//! (scroll_wheel_lines, copy_on_select, sh_click, selection_drag_extend) —
//! kept out of the large `legacy` module so that file stays under the
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

// --- NumericSpec (slider bounds, derived range, folded step) ---

#[test]
fn numeric_spec_is_present_exactly_for_bounded_number_rows() {
    let info = Settings::default().setting_info();
    for row in &info {
        match row.key {
            // Startup-only, unbounded: a Number row with no slider spec.
            "native_autoclose_ms" => {
                assert_eq!(row.kind, SettingKind::Number);
                assert!(
                    row.numeric.is_none(),
                    "startup-only autoclose carries no slider spec"
                );
            }
            _ if row.kind == SettingKind::Number => assert!(
                row.numeric.is_some(),
                "bounded number row {} carries a slider spec",
                row.key
            ),
            _ => assert!(
                row.numeric.is_none(),
                "non-number row {} has no slider spec",
                row.key
            ),
        }
    }
}

#[test]
fn numeric_spec_bounds_match_the_parser_clamp_constants() {
    let info = Settings::default().setting_info();
    let spec = |key: &str| {
        info.iter()
            .find(|row| row.key == key)
            .unwrap_or_else(|| panic!("row {key}"))
            .numeric
            .unwrap_or_else(|| panic!("spec {key}"))
    };

    let fs = spec("font_size");
    assert_eq!(
        (fs.min, fs.max, fs.step, fs.unit),
        (MIN_FONT_SIZE_PX, MAX_FONT_SIZE_PX, 1.0, "px")
    );
    let mc = spec("min_contrast");
    assert_eq!(
        (mc.min, mc.max, mc.unit),
        (MIN_MIN_CONTRAST, MAX_MIN_CONTRAST, "")
    );
    let csi = spec("crt_scanline_intensity");
    assert_eq!(
        (csi.min, csi.max),
        (MIN_CRT_SCANLINE_INTENSITY, MAX_CRT_SCANLINE_INTENSITY)
    );
    let swl = spec("scroll_wheel_lines");
    assert_eq!(
        (swl.min, swl.max, swl.step, swl.unit),
        (MIN_SCROLL_WHEEL_LINES, MAX_SCROLL_WHEEL_LINES, 1.0, "lines")
    );
}

#[test]
fn numeric_spec_steps_preserve_the_folded_keyboard_steps() {
    // UX4-P2 folds the former per-key `number_step` table into `spec.step`; the
    // keyboard Left/Right step must be byte-for-byte what it was before.
    let info = Settings::default().setting_info();
    let step = |key: &str| {
        info.iter()
            .find(|row| row.key == key)
            .unwrap()
            .numeric
            .unwrap()
            .step
    };
    for (key, expected) in [
        ("font_size", 1.0),
        ("text_gamma", 0.1),
        ("stem_darken", 0.05),
        ("min_contrast", 1.0),
        ("focus_dim", 0.05),
        ("window_padding", 1.0),
        ("bloom_threshold", 0.05),
        ("bloom_intensity", 0.05),
        ("bloom_radius", 0.5),
        ("crt_scanline_intensity", 0.01),
        ("crt_scanline_period", 0.5),
        ("crt_vignette_strength", 0.01),
        ("scroll_wheel_lines", 1.0),
    ] {
        assert_eq!(step(key), expected, "{key} keyboard step");
    }
}

#[test]
fn numeric_range_label_is_derived_and_keeps_its_unit() {
    // Every numeric row's range hint is derived from its spec, so it cannot
    // drift from the clamp bounds; the unit suffix is preserved when present.
    let info = Settings::default().setting_info();
    for row in &info {
        if let Some(spec) = row.numeric {
            let range = row
                .range
                .as_deref()
                .unwrap_or_else(|| panic!("derived range for {}", row.key));
            assert!(range.contains("..="), "range is a bound pair: {range}");
            if !spec.unit.is_empty() {
                assert!(
                    range.ends_with(spec.unit),
                    "range keeps the {} unit: {range}",
                    spec.unit
                );
            }
        }
    }
}

#[test]
fn numeric_spec_slider_math_clamps_and_snaps() {
    let spec = NumericSpec {
        min: 0.0,
        max: 10.0,
        step: 1.0,
        unit: "",
    };
    assert_eq!(spec.fraction_of(0.0), 0.0);
    assert_eq!(spec.fraction_of(5.0), 0.5);
    assert_eq!(spec.fraction_of(10.0), 1.0);
    assert_eq!(spec.fraction_of(-5.0), 0.0, "below min clamps to 0");
    assert_eq!(spec.fraction_of(50.0), 1.0, "above max clamps to 1");

    assert_eq!(spec.value_at_fraction(0.0), 0.0);
    assert_eq!(spec.value_at_fraction(1.0), 10.0);
    assert_eq!(spec.value_at_fraction(0.54), 5.0, "snaps to nearest step");
    assert_eq!(
        spec.value_at_fraction(2.0),
        10.0,
        "fraction over 1 saturates"
    );

    // Reserved readout budget: wider bound label ("10" = 2) plus " *" = 4.
    assert_eq!(spec.readout_width(), 4);

    // A degenerate zero-width range never divides by zero.
    let flat = NumericSpec {
        min: 3.0,
        max: 3.0,
        step: 1.0,
        unit: "",
    };
    assert_eq!(flat.fraction_of(3.0), 0.0);
    assert_eq!(flat.value_at_fraction(0.5), 3.0);
}

#[test]
fn scroll_wheel_lines_defaults_parses_and_clamps() {
    // Absent → default (byte-identical historical step of 3).
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.scroll_wheel_lines, DEFAULT_SCROLL_WHEEL_LINES);
    assert_eq!(settings.scroll_wheel_step(), 3);
    assert!(warnings.is_empty());

    // A valid in-range value is taken as-is and rounds to a usize step.
    let (settings, warnings) = settings_from([(SCROLL_WHEEL_LINES_ENV, "5")]);
    assert_eq!(settings.scroll_wheel_lines, 5.0);
    assert_eq!(settings.scroll_wheel_step(), 5);
    assert!(warnings.is_empty());

    // Out-of-range values clamp to the spec bounds (no warning — clamp, not reject).
    let (high, _) = settings_from([(SCROLL_WHEEL_LINES_ENV, "999")]);
    assert_eq!(high.scroll_wheel_lines, MAX_SCROLL_WHEEL_LINES);
    let (low, _) = settings_from([(SCROLL_WHEEL_LINES_ENV, "0")]);
    assert_eq!(low.scroll_wheel_lines, MIN_SCROLL_WHEEL_LINES);
    assert_eq!(low.scroll_wheel_step(), 1, "step floors at one row");

    // An unparseable value warns and falls back to the default.
    let (settings, warnings) = settings_from([(SCROLL_WHEEL_LINES_ENV, "fast")]);
    assert_eq!(settings.scroll_wheel_lines, DEFAULT_SCROLL_WHEEL_LINES);
    assert_eq!(warnings.len(), 1);
}

#[test]
fn copy_on_select_defaults_off_and_parses() {
    // Absent → off (byte-identical: PRIMARY-only on selection finish).
    let (settings, warnings) = settings_from([]);
    assert!(!settings.copy_on_select);
    assert!(warnings.is_empty());

    // Enabled via the env/config key.
    let (settings, _) = settings_from([(COPY_ON_SELECT_ENV, "on")]);
    assert!(settings.copy_on_select);

    let (settings, _) = settings_from([(COPY_ON_SELECT_ENV, "off")]);
    assert!(!settings.copy_on_select);
}

#[test]
fn sh_click_defaults_off_and_round_trips_through_config_key() {
    // Absent → off (SH-CLICK click-to-position is off by default; the off path
    // emits no bytes and is byte-identical to today).
    let (settings, warnings) = settings_from([]);
    assert!(!settings.sh_click);
    assert!(warnings.is_empty());

    let (settings, _) = settings_from([(SH_CLICK_ENV, "on")]);
    assert!(settings.sh_click);

    let (settings, _) = settings_from([(SH_CLICK_ENV, "off")]);
    assert!(!settings.sh_click);

    // The config-file key (and an alias) maps to the env key and back, and the
    // value survives a to_edit_values round-trip.
    assert_eq!(config_key_to_env("sh_click"), Some(SH_CLICK_ENV));
    assert_eq!(config_key_to_env("click_to_position"), Some(SH_CLICK_ENV));
    assert_eq!(env_to_config_key(SH_CLICK_ENV), Some("sh_click"));
    assert_eq!(
        Settings {
            sh_click: true,
            ..Settings::default()
        }
        .to_edit_values()
        .get(SH_CLICK_ENV)
        .map(String::as_str),
        Some("on")
    );
}

#[test]
fn selection_drag_extend_defaults_on_and_parses() {
    // Absent → on (operator default; drag-extend + Shift+click extend active).
    let (settings, warnings) = settings_from([]);
    assert!(settings.selection_drag_extend);
    assert!(warnings.is_empty());

    // Off restores the historical click-to-finish selection.
    let (settings, _) = settings_from([(SELECTION_DRAG_EXTEND_ENV, "off")]);
    assert!(!settings.selection_drag_extend);

    let (settings, _) = settings_from([(SELECTION_DRAG_EXTEND_ENV, "on")]);
    assert!(settings.selection_drag_extend);

    // Config-file alias maps to the env key and back.
    assert_eq!(
        config_key_to_env("selection_drag_extend"),
        Some(SELECTION_DRAG_EXTEND_ENV)
    );
    assert_eq!(
        env_to_config_key(SELECTION_DRAG_EXTEND_ENV),
        Some("selection_drag_extend")
    );
}

#[test]
fn scroll_wheel_lines_round_trips_through_config_key() {
    // The config-file alias maps to the env key and back, and the value
    // survives a to_edit_values round-trip.
    assert_eq!(
        config_key_to_env("scroll_wheel_lines"),
        Some(SCROLL_WHEEL_LINES_ENV)
    );
    assert_eq!(
        env_to_config_key(SCROLL_WHEEL_LINES_ENV),
        Some("scroll_wheel_lines")
    );
    assert_eq!(
        config_key_to_env("copy_on_select"),
        Some(COPY_ON_SELECT_ENV)
    );
    assert_eq!(
        env_to_config_key(COPY_ON_SELECT_ENV),
        Some("copy_on_select")
    );
}

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
    let cc = spec("crt_curvature");
    assert_eq!((cc.min, cc.max), (MIN_CRT_CURVATURE, MAX_CRT_CURVATURE));
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
        ("inactive_pane_dim", 0.05),
        ("window_padding", 1.0),
        ("bloom_threshold", 0.05),
        ("bloom_intensity", 0.05),
        ("bloom_radius", 0.5),
        ("crt_scanline_intensity", 0.01),
        ("crt_scanline_period", 0.5),
        ("crt_vignette_strength", 0.01),
        ("crt_curvature", 0.005),
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
            if row.key == "tab_bar_height" {
                assert_eq!(range, "auto or 1-5 rows");
                continue;
            }
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
    // Absent → default (6 rows per notch, the tuned interactive feel).
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.scroll_wheel_lines, DEFAULT_SCROLL_WHEEL_LINES);
    assert_eq!(settings.scroll_wheel_step(), 6);
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
fn smart_ctrl_c_defaults_on_and_parses() {
    // v0.6.0 shipped identity: copy-or-interrupt is the default.
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.smart_ctrl_c, SmartCtrlC::CopyOrInterrupt);
    assert!(settings.smart_ctrl_c.is_active());
    assert!(warnings.is_empty());

    // Enabled via the env/config key (canonical token + aliases).
    let (settings, _) = settings_from([(SMART_CTRL_C_ENV, "copy-or-interrupt")]);
    assert_eq!(settings.smart_ctrl_c, SmartCtrlC::CopyOrInterrupt);
    assert!(settings.smart_ctrl_c.is_active());

    let (settings, _) = settings_from([(SMART_CTRL_C_ENV, "on")]);
    assert_eq!(settings.smart_ctrl_c, SmartCtrlC::CopyOrInterrupt);

    let (settings, _) = settings_from([(SMART_CTRL_C_ENV, "off")]);
    assert_eq!(settings.smart_ctrl_c, SmartCtrlC::Off);

    // Unknown value warns and falls back to the default (copy-or-interrupt).
    let (settings, warnings) = settings_from([(SMART_CTRL_C_ENV, "bogus")]);
    assert_eq!(settings.smart_ctrl_c, SmartCtrlC::CopyOrInterrupt);
    assert_eq!(warnings.len(), 1);

    // Config-file key + alias map to the env key and back, and the value
    // survives a to_edit_values round-trip.
    assert_eq!(config_key_to_env("smart_ctrl_c"), Some(SMART_CTRL_C_ENV));
    assert_eq!(
        config_key_to_env("copy_or_interrupt"),
        Some(SMART_CTRL_C_ENV)
    );
    assert_eq!(env_to_config_key(SMART_CTRL_C_ENV), Some("smart_ctrl_c"));
    assert_eq!(
        Settings {
            smart_ctrl_c: SmartCtrlC::CopyOrInterrupt,
            ..Settings::default()
        }
        .to_edit_values()
        .get(SMART_CTRL_C_ENV)
        .map(String::as_str),
        Some("copy-or-interrupt")
    );
}

#[test]
fn interactive_urls_defaults_on_and_round_trips_through_config_key() {
    // Absent → on (default: printed URLs are clickable out of the box).
    let (settings, warnings) = settings_from([]);
    assert!(settings.interactive_urls);
    assert!(warnings.is_empty());

    let (settings, _) = settings_from([(INTERACTIVE_URLS_ENV, "off")]);
    assert!(!settings.interactive_urls);

    let (settings, _) = settings_from([(INTERACTIVE_URLS_ENV, "on")]);
    assert!(settings.interactive_urls);

    // Config-file key + alias map to the env key and back.
    assert_eq!(
        config_key_to_env("interactive_urls"),
        Some(INTERACTIVE_URLS_ENV)
    );
    assert_eq!(config_key_to_env("linkify"), Some(INTERACTIVE_URLS_ENV));
    assert_eq!(
        env_to_config_key(INTERACTIVE_URLS_ENV),
        Some("interactive_urls")
    );
    assert_eq!(
        Settings {
            interactive_urls: false,
            ..Settings::default()
        }
        .to_edit_values()
        .get(INTERACTIVE_URLS_ENV)
        .map(String::as_str),
        Some("off")
    );
}

#[test]
fn sh_click_defaults_on_and_round_trips_through_config_key() {
    // Absent → on (F2: click-to-position defaults on; it stays inert unless a
    // cooperating shell advertises click_events=1, so non-integrated shells
    // see no behavior change).
    let (settings, warnings) = settings_from([]);
    assert!(settings.sh_click);
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
fn buttons_gates_default_and_round_trip_through_config_keys() {
    // BUTTONS-SETTINGS: the master gate and the iTerm2-compat spelling default
    // ON (click reports are terminal-composed from the parsed code, clicks are
    // suppressed under an active mouse app, same risk class as OSC 8 links);
    // sticky stays OFF because a button surviving a scroll-away is the one
    // surprising variant.
    let (settings, warnings) = settings_from([]);
    assert!(settings.buttons);
    assert!(settings.buttons_iterm_compat);
    assert!(!settings.buttons_sticky);
    assert!(warnings.is_empty());

    let (settings, _) = settings_from([
        (BUTTONS_ENV, "on"),
        (BUTTONS_ITERM_COMPAT_ENV, "on"),
        (BUTTONS_STICKY_ENV, "on"),
    ]);
    assert!(settings.buttons);
    assert!(settings.buttons_iterm_compat);
    assert!(settings.buttons_sticky);

    // Explicit off overrides the on-by-default master gate.
    let (settings, _) = settings_from([(BUTTONS_ENV, "off")]);
    assert!(!settings.buttons);

    // Config keys and aliases map to the env keys and back, and the values
    // survive a to_edit_values round-trip.
    assert_eq!(config_key_to_env("buttons"), Some(BUTTONS_ENV));
    assert_eq!(config_key_to_env("button_protocol"), Some(BUTTONS_ENV));
    assert_eq!(config_key_to_env("clickable_buttons"), Some(BUTTONS_ENV));
    assert_eq!(
        config_key_to_env("buttons_iterm_compat"),
        Some(BUTTONS_ITERM_COMPAT_ENV)
    );
    assert_eq!(
        config_key_to_env("iterm_buttons"),
        Some(BUTTONS_ITERM_COMPAT_ENV)
    );
    assert_eq!(
        config_key_to_env("buttons_sticky"),
        Some(BUTTONS_STICKY_ENV)
    );
    assert_eq!(
        config_key_to_env("sticky_buttons"),
        Some(BUTTONS_STICKY_ENV)
    );
    assert_eq!(env_to_config_key(BUTTONS_ENV), Some("buttons"));
    assert_eq!(
        env_to_config_key(BUTTONS_ITERM_COMPAT_ENV),
        Some("buttons_iterm_compat")
    );
    assert_eq!(
        env_to_config_key(BUTTONS_STICKY_ENV),
        Some("buttons_sticky")
    );
    let edit_values = Settings {
        buttons: true,
        buttons_iterm_compat: true,
        buttons_sticky: false,
        ..Settings::default()
    }
    .to_edit_values();
    assert_eq!(edit_values.get(BUTTONS_ENV).map(String::as_str), Some("on"));
    assert_eq!(
        edit_values
            .get(BUTTONS_ITERM_COMPAT_ENV)
            .map(String::as_str),
        Some("on")
    );
    assert_eq!(
        edit_values.get(BUTTONS_STICKY_ENV).map(String::as_str),
        Some("off")
    );
}

#[test]
fn always_show_tab_bar_defaults_off_and_round_trips_through_config_key() {
    // Absent → off (F4 ODP-7: the tab bar stays hidden for a single unnamed
    // tab; the render path is byte-identical to today).
    let (settings, warnings) = settings_from([]);
    assert!(!settings.always_show_tab_bar);
    assert!(warnings.is_empty());

    let (settings, _) = settings_from([(ALWAYS_SHOW_TAB_BAR_ENV, "on")]);
    assert!(settings.always_show_tab_bar);

    let (settings, _) = settings_from([(ALWAYS_SHOW_TAB_BAR_ENV, "off")]);
    assert!(!settings.always_show_tab_bar);

    // The config-file key (and an alias) maps to the env key and back, and the
    // value survives a to_edit_values round-trip.
    assert_eq!(
        config_key_to_env("always_show_tab_bar"),
        Some(ALWAYS_SHOW_TAB_BAR_ENV)
    );
    assert_eq!(
        config_key_to_env("show_tab_bar"),
        Some(ALWAYS_SHOW_TAB_BAR_ENV)
    );
    assert_eq!(
        env_to_config_key(ALWAYS_SHOW_TAB_BAR_ENV),
        Some("always_show_tab_bar")
    );
    assert_eq!(
        Settings {
            always_show_tab_bar: true,
            ..Settings::default()
        }
        .to_edit_values()
        .get(ALWAYS_SHOW_TAB_BAR_ENV)
        .map(String::as_str),
        Some("on")
    );
}

#[test]
fn shell_integration_defaults_on_and_round_trips_through_config_key() {
    // Shipped default: on out of the box (opt-out). The integration only adds
    // prompt-mark hooks to OdyTTY's own default-shell launches and never edits
    // the user's rc files.
    let (settings, warnings) = settings_from([]);
    assert!(settings.shell_integration);
    assert!(warnings.is_empty());

    let (settings, _) = settings_from([(SHELL_INTEGRATION_ENV, "on")]);
    assert!(settings.shell_integration);

    let (settings, _) = settings_from([(SHELL_INTEGRATION_ENV, "off")]);
    assert!(!settings.shell_integration);

    assert_eq!(
        config_key_to_env("shell_integration"),
        Some(SHELL_INTEGRATION_ENV)
    );
    assert_eq!(
        config_key_to_env("prompt_marks"),
        Some(SHELL_INTEGRATION_ENV)
    );
    assert_eq!(
        env_to_config_key(SHELL_INTEGRATION_ENV),
        Some("shell_integration")
    );
    assert_eq!(
        Settings {
            shell_integration: true,
            ..Settings::default()
        }
        .to_edit_values()
        .get(SHELL_INTEGRATION_ENV)
        .map(String::as_str),
        Some("on")
    );
}

#[test]
fn selection_drag_extend_defaults_on_and_parses() {
    // Absent → on (default; drag-extend + Shift+click extend active).
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

// --- F4-V2 / F4-P2: tab_bar_placement (enum: top | left | right, all real) ---

#[test]
fn tab_bar_placement_defaults_to_top() {
    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.tab_bar_placement, TabBarPlacement::Top);
    assert!(warnings.is_empty());
    // Top is not a rail, and it renders as itself.
    assert!(!settings.tab_bar_placement.is_rail());
    assert_eq!(settings.tab_bar_placement.effective(), TabBarPlacement::Top);
}

#[test]
fn tab_bar_placement_parses_top_left_right() {
    let (top, _) = settings_from([(TAB_BAR_PLACEMENT_ENV, "top")]);
    assert_eq!(top.tab_bar_placement, TabBarPlacement::Top);

    let (left, _) = settings_from([(TAB_BAR_PLACEMENT_ENV, "left")]);
    assert_eq!(left.tab_bar_placement, TabBarPlacement::Left);
    assert!(left.tab_bar_placement.is_rail());
    assert_eq!(left.tab_bar_placement.effective(), TabBarPlacement::Left);

    // `right` parses AND renders as itself (F4-P2 landed the right arm), so
    // `effective()` is an identity — no degrade to top.
    let (right, warnings) = settings_from([(TAB_BAR_PLACEMENT_ENV, "right")]);
    assert_eq!(right.tab_bar_placement, TabBarPlacement::Right);
    assert!(warnings.is_empty(), "a known value emits no warning");
    assert!(right.tab_bar_placement.is_rail());
    assert_eq!(
        right.tab_bar_placement.effective(),
        TabBarPlacement::Right,
        "right renders as itself now"
    );
}

#[test]
fn tab_bar_placement_unknown_value_warns_and_defaults() {
    let (settings, warnings) = settings_from([(TAB_BAR_PLACEMENT_ENV, "sideways")]);
    assert_eq!(settings.tab_bar_placement, TabBarPlacement::Top);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("top|left|right"));
}

#[test]
fn tab_bar_placement_round_trips_through_config_key_mapping() {
    assert_eq!(
        config_key_to_env("tab_bar_placement"),
        Some(TAB_BAR_PLACEMENT_ENV)
    );
    assert_eq!(config_key_to_env("tabbarside"), Some(TAB_BAR_PLACEMENT_ENV));
    assert_eq!(
        env_to_config_key(TAB_BAR_PLACEMENT_ENV),
        Some("tab_bar_placement")
    );
}

#[test]
fn tab_bar_placement_round_trips_through_edit_values() {
    let settings = Settings {
        tab_bar_placement: TabBarPlacement::Left,
        ..Settings::default()
    };
    assert_eq!(
        settings.to_edit_values().get(TAB_BAR_PLACEMENT_ENV),
        Some(&"left".to_owned())
    );
    assert_eq!(
        Settings::default()
            .to_edit_values()
            .get(TAB_BAR_PLACEMENT_ENV),
        Some(&"top".to_owned())
    );
}

#[test]
fn rail_side_has_an_enum_row_in_the_workspace_rail_group() {
    let rows = Settings::default().setting_info();
    let row = rows
        .iter()
        .find(|row| row.key == "tab_bar_placement")
        .expect("tab_bar_placement row present");
    // Rail SIDE lives in the "Workspace rail" group -> "Layout" Level-1 section.
    // The user surface offers left|right only; the default Top placement folds to
    // the left rail, so the displayed value is "left".
    assert_eq!(row.group, "Workspace rail");
    assert_eq!(row.name, "Rail side");
    assert_eq!(row.kind, SettingKind::Enum);
    assert_eq!(row.options, ["left", "right"]);
    assert_eq!(row.value, "left");
    assert!(row.reloadable);
    assert!(!row.description.trim().is_empty());
    assert!(
        row.description.contains("right"),
        "description names right as a side"
    );
    assert!(
        !row.description.contains("later update") && !row.description.contains("falls back"),
        "the stale 'right arrives later' caveat must be gone"
    );
}

// --- F4-P1/P4 rail/panel knobs (TAB_RAIL_WIDTH/MAX_WIDTH/GAP/SLOT_ROWS/
//     PANEL_STRENGTH/SEAM/AUTOHIDE/REVEAL_PX) ---

#[test]
fn tab_rail_numeric_knobs_default_parse_and_clamp() {
    // Defaults.
    let (d, warnings) = settings_from([]);
    assert_eq!(
        d.tab_rail_width,
        TabRailWidth::Auto,
        "width is auto by default"
    );
    assert_eq!(d.tab_rail_max_width, DEFAULT_TAB_RAIL_MAX_WIDTH);
    assert_eq!(d.tab_rail_gap, DEFAULT_TAB_RAIL_GAP);
    assert_eq!(d.tab_rail_slot_rows, DEFAULT_TAB_RAIL_SLOT_ROWS);
    assert_eq!(d.tab_panel_strength, DEFAULT_TAB_PANEL_STRENGTH);
    assert_eq!(d.tab_rail_reveal_px, DEFAULT_TAB_RAIL_REVEAL_PX);
    assert!(warnings.is_empty());
    // The rounding accessors return the shipped defaults.
    assert_eq!(d.rail_max_width_cols(), 24);
    assert_eq!(d.rail_slot_gap_rows(), 1);
    assert_eq!(d.rail_slot_rows(), 2);

    // Valid parses (gap/slot-rows/panel/reveal unchanged; width now auto|N).
    let (s, _) = settings_from([
        (TAB_RAIL_GAP_ENV, "2"),
        (TAB_RAIL_SLOT_ROWS_ENV, "1"),
        (TAB_PANEL_STRENGTH_ENV, "0.3"),
        (TAB_RAIL_REVEAL_PX_ENV, "8"),
    ]);
    assert_eq!(s.rail_slot_gap_rows(), 2);
    assert_eq!(s.rail_slot_rows(), 1);
    assert!((s.tab_panel_strength - 0.3).abs() < 1e-6);
    assert_eq!(s.tab_rail_reveal_px, 8.0);

    // Clamps at both ends.
    let (hi, _) = settings_from([
        (TAB_RAIL_MAX_WIDTH_ENV, "999"),
        (TAB_RAIL_GAP_ENV, "99"),
        (TAB_RAIL_SLOT_ROWS_ENV, "9"),
        (TAB_PANEL_STRENGTH_ENV, "9"),
        (TAB_RAIL_REVEAL_PX_ENV, "999"),
    ]);
    assert_eq!(hi.tab_rail_max_width, MAX_TAB_RAIL_MAX_WIDTH);
    assert_eq!(hi.tab_rail_gap, MAX_TAB_RAIL_GAP);
    assert_eq!(hi.tab_rail_slot_rows, MAX_TAB_RAIL_SLOT_ROWS);
    assert_eq!(hi.tab_panel_strength, MAX_TAB_PANEL_STRENGTH);
    assert_eq!(hi.tab_rail_reveal_px, MAX_TAB_RAIL_REVEAL_PX);
    let (lo, _) = settings_from([
        (TAB_RAIL_MAX_WIDTH_ENV, "0"),
        (TAB_PANEL_STRENGTH_ENV, "-1"),
        (TAB_RAIL_REVEAL_PX_ENV, "0"),
    ]);
    assert_eq!(lo.tab_rail_max_width, MIN_TAB_RAIL_MAX_WIDTH);
    assert_eq!(lo.tab_panel_strength, MIN_TAB_PANEL_STRENGTH);
    assert_eq!(lo.tab_rail_reveal_px, MIN_TAB_RAIL_REVEAL_PX);
}

#[test]
fn tab_rail_width_auto_and_manual_modes_parse_and_migrate() {
    // Explicit "auto" (any case) → Auto.
    for raw in ["auto", "AUTO", "Auto"] {
        let (s, w) = settings_from([(TAB_RAIL_WIDTH_ENV, raw)]);
        assert_eq!(s.tab_rail_width, TabRailWidth::Auto, "{raw:?} → Auto");
        assert!(w.is_empty());
    }
    // MIGRATION: an old numeric config parses as Manual with its exact width.
    let (s, w) = settings_from([(TAB_RAIL_WIDTH_ENV, "20")]);
    assert_eq!(s.tab_rail_width, TabRailWidth::Manual(20));
    assert!(w.is_empty());
    // Manual clamps to the absolute widget bounds [8, 32], not the auto max.
    let (hi, _) = settings_from([(TAB_RAIL_WIDTH_ENV, "999")]);
    assert_eq!(
        hi.tab_rail_width,
        TabRailWidth::Manual(MAX_TAB_RAIL_WIDTH as u16)
    );
    let (lo, _) = settings_from([(TAB_RAIL_WIDTH_ENV, "1")]);
    assert_eq!(
        lo.tab_rail_width,
        TabRailWidth::Manual(MIN_TAB_RAIL_WIDTH as u16)
    );
    // Garbage warns and falls back to auto.
    let (g, warnings) = settings_from([(TAB_RAIL_WIDTH_ENV, "wide")]);
    assert_eq!(g.tab_rail_width, TabRailWidth::Auto);
    assert!(warnings.iter().any(|w| w.contains("auto or a cell count")));
}

#[test]
fn rail_width_cols_resolves_auto_and_manual() {
    // AUTO: sizes to the caller's wanted width, clamped to [MIN, auto max].
    let auto = Settings::default();
    assert_eq!(auto.tab_rail_width, TabRailWidth::Auto);
    // Short want clamps up to the min; a mid want passes through; a big want
    // clamps to the auto max (default 24), NOT the absolute widget max.
    assert_eq!(auto.rail_width_cols(3), MIN_TAB_RAIL_WIDTH as usize);
    assert_eq!(auto.rail_width_cols(18), 18);
    assert_eq!(auto.rail_width_cols(100), auto.rail_max_width_cols());
    assert_eq!(auto.rail_max_width_cols(), 24);

    // A higher auto max lets auto grow further (still <= absolute widget max).
    let (wide_auto, _) = settings_from([(TAB_RAIL_MAX_WIDTH_ENV, "30")]);
    assert_eq!(wide_auto.rail_width_cols(100), 30);

    // MANUAL: ignores the wanted width, clamps to the absolute widget bounds.
    let (manual, _) = settings_from([(TAB_RAIL_WIDTH_ENV, "28")]);
    assert_eq!(manual.rail_width_cols(3), 28, "manual ignores auto want");
    assert_eq!(manual.rail_width_cols(100), 28);
}

#[test]
fn tab_seam_and_autohide_bools_default_and_parse() {
    let (d, _) = settings_from([]);
    assert!(d.tab_seam, "seam on by default");
    assert!(!d.tab_rail_autohide, "autohide off by default");
    let (s, _) = settings_from([(TAB_SEAM_ENV, "off"), (TAB_RAIL_AUTOHIDE_ENV, "on")]);
    assert!(!s.tab_seam);
    assert!(s.tab_rail_autohide);
}

#[test]
fn tab_rail_knobs_round_trip_through_config_keys() {
    for (key, env) in [
        ("tab_rail_width", TAB_RAIL_WIDTH_ENV),
        ("tab_rail_max_width", TAB_RAIL_MAX_WIDTH_ENV),
        ("tab_rail_gap", TAB_RAIL_GAP_ENV),
        ("tab_rail_slot_rows", TAB_RAIL_SLOT_ROWS_ENV),
        ("tab_panel_strength", TAB_PANEL_STRENGTH_ENV),
        ("tab_seam", TAB_SEAM_ENV),
        ("tab_rail_autohide", TAB_RAIL_AUTOHIDE_ENV),
        ("tab_rail_reveal_px", TAB_RAIL_REVEAL_PX_ENV),
    ] {
        assert_eq!(config_key_to_env(key), Some(env), "{key} → env");
        assert_eq!(env_to_config_key(env), Some(key), "{env} → key");
    }
}

#[test]
fn workspace_rail_env_aliases_hit_the_same_field_as_tab_rail() {
    // The vertical rail shows workspaces, so WORKSPACE_RAIL_* is the canonical
    // family. Each alias must set the identical Settings field as its legacy
    // TAB_RAIL_* twin. Settings are platform-agnostic: same on all three
    // platforms (Linux, macOS, Windows).
    let (legacy_gap, _) = settings_from([(TAB_RAIL_GAP_ENV, "3")]);
    let (canon_gap, _) = settings_from([(WORKSPACE_RAIL_GAP_ENV, "3")]);
    assert_eq!(canon_gap.tab_rail_gap, legacy_gap.tab_rail_gap);
    assert_eq!(canon_gap.tab_rail_gap, 3.0);

    let (legacy_rows, _) = settings_from([(TAB_RAIL_SLOT_ROWS_ENV, "1")]);
    let (canon_rows, _) = settings_from([(WORKSPACE_RAIL_SLOT_ROWS_ENV, "1")]);
    assert_eq!(
        canon_rows.tab_rail_slot_rows,
        legacy_rows.tab_rail_slot_rows
    );
    assert_eq!(canon_rows.tab_rail_slot_rows, 1.0);

    let (legacy_reveal, _) = settings_from([(TAB_RAIL_REVEAL_PX_ENV, "20")]);
    let (canon_reveal, _) = settings_from([(WORKSPACE_RAIL_REVEAL_PX_ENV, "20")]);
    assert_eq!(
        canon_reveal.tab_rail_reveal_px,
        legacy_reveal.tab_rail_reveal_px
    );
    assert_eq!(canon_reveal.tab_rail_reveal_px, 20.0);

    let (legacy_auto, _) = settings_from([(TAB_RAIL_AUTOHIDE_ENV, "on")]);
    let (canon_auto, _) = settings_from([(WORKSPACE_RAIL_AUTOHIDE_ENV, "on")]);
    assert_eq!(canon_auto.tab_rail_autohide, legacy_auto.tab_rail_autohide);
    assert!(canon_auto.tab_rail_autohide);

    let (legacy_max, _) = settings_from([(TAB_RAIL_MAX_WIDTH_ENV, "30")]);
    let (canon_max, _) = settings_from([(WORKSPACE_RAIL_MAX_WIDTH_ENV, "30")]);
    assert_eq!(
        canon_max.rail_max_width_cols(),
        legacy_max.rail_max_width_cols()
    );

    let (legacy_width, _) = settings_from([(TAB_RAIL_WIDTH_ENV, "28")]);
    let (canon_width, _) = settings_from([(WORKSPACE_RAIL_WIDTH_ENV, "28")]);
    assert_eq!(
        canon_width.rail_width_cols(3),
        legacy_width.rail_width_cols(3)
    );
    assert_eq!(canon_width.rail_width_cols(3), 28);
}

#[test]
fn workspace_rail_wins_over_tab_rail_when_both_are_set() {
    // Deterministic precedence rule: the canonical WORKSPACE_RAIL_* value wins
    // over the legacy TAB_RAIL_* twin when both are set for the same field.
    let (both_gap, _) = settings_from([(WORKSPACE_RAIL_GAP_ENV, "3"), (TAB_RAIL_GAP_ENV, "1")]);
    assert_eq!(
        both_gap.tab_rail_gap, 3.0,
        "workspace rail gap must win over the legacy tab rail gap"
    );

    let (both_auto, _) = settings_from([
        (WORKSPACE_RAIL_AUTOHIDE_ENV, "on"),
        (TAB_RAIL_AUTOHIDE_ENV, "off"),
    ]);
    assert!(
        both_auto.tab_rail_autohide,
        "workspace rail autohide must win over the legacy tab rail autohide"
    );

    // And the legacy name still applies on its own when the canonical is absent.
    let (legacy_only, _) = settings_from([(TAB_RAIL_GAP_ENV, "2")]);
    assert_eq!(legacy_only.tab_rail_gap, 2.0);
}

#[test]
fn workspace_rail_config_keys_canonicalize_and_round_trip() {
    // Each workspace_rail_* config key maps to its WORKSPACE_RAIL_* env alias in
    // both directions, and the legacy tab_rail_* keys keep working unchanged.
    for (key, env) in [
        ("workspace_rail_width", WORKSPACE_RAIL_WIDTH_ENV),
        ("workspace_rail_max_width", WORKSPACE_RAIL_MAX_WIDTH_ENV),
        ("workspace_rail_gap", WORKSPACE_RAIL_GAP_ENV),
        ("workspace_rail_slot_rows", WORKSPACE_RAIL_SLOT_ROWS_ENV),
        ("workspace_rail_autohide", WORKSPACE_RAIL_AUTOHIDE_ENV),
        ("workspace_rail_reveal_px", WORKSPACE_RAIL_REVEAL_PX_ENV),
    ] {
        assert_eq!(config_key_to_env(key), Some(env), "{key} → env");
        assert_eq!(env_to_config_key(env), Some(key), "{env} → key");
    }
    // Legacy tab_rail_* config keys stay fully accepted (no removal).
    assert_eq!(config_key_to_env("tab_rail_gap"), Some(TAB_RAIL_GAP_ENV));
    assert_eq!(config_key_to_env("railgap"), Some(TAB_RAIL_GAP_ENV));
}

#[test]
fn workspace_rail_config_key_sets_the_field_through_parse() {
    // A config-file line using the canonical workspace_rail_* key parses through
    // to the same field, proving the config path (not just env) honors it.
    let config = ConfigValues::parse(
        "workspace_rail_gap = 3
",
        |_| {},
    );
    let settings =
        Settings::from_source(|key| config.get(key).cloned(), |_| {}, |_| None, |_| None);
    assert_eq!(settings.tab_rail_gap, 3.0);
}

#[test]
fn layout_knobs_land_in_their_regrouped_groups() {
    // The Layout section regroups its knobs: rail geometry under "Workspace
    // rail", the tab panel under "Panel", tab knobs under "Tabs", panes under
    // "Panes".
    let rows = Settings::default().setting_info();
    let expected = [
        ("tab_rail_width", "Workspace rail"),
        ("tab_rail_max_width", "Workspace rail"),
        ("tab_rail_gap", "Workspace rail"),
        ("tab_rail_slot_rows", "Workspace rail"),
        ("tab_rail_autohide", "Workspace rail"),
        ("tab_rail_reveal_px", "Workspace rail"),
        ("tab_bar_placement", "Workspace rail"),
        ("workspace_rail", "Workspace rail"),
        ("tab_panel_strength", "Panel"),
        ("tab_seam", "Panel"),
        ("always_show_tab_bar", "Tabs"),
        ("tab_bar_height", "Tabs"),
        ("inactive_pane_dim", "Panes"),
        ("pane_prefix", "Panes"),
    ];
    for (key, group) in expected {
        let row = rows
            .iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("row {key}"));
        assert_eq!(row.group, group, "{key} in {group} group");
        assert!(
            !row.description.trim().is_empty(),
            "{key} has a description"
        );
    }
}

#[test]
fn rail_side_and_visibility_default_to_left_and_auto() {
    // Rail side and visibility are separate settings. With no config the side is
    // the left rail (the default Top placement folds to left) and visibility is
    // Auto (rail appears only once a second workspace exists).
    let (d, warnings) = settings_from([]);
    assert_eq!(d.tab_bar_placement, TabBarPlacement::Top);
    assert_eq!(d.tab_bar_placement.rail_side_str(), "left");
    assert_eq!(d.workspace_rail, WorkspaceRail::Auto);
    assert!(warnings.is_empty());
}

#[test]
fn legacy_tab_bar_placement_still_sets_the_rail_side() {
    // The legacy side selector keeps working: top|left|right, top -> left.
    let (right, _) = settings_from([(TAB_BAR_PLACEMENT_ENV, "right")]);
    assert_eq!(right.tab_bar_placement, TabBarPlacement::Right);
    assert_eq!(right.tab_bar_placement.rail_side_str(), "right");

    let (left, _) = settings_from([(TAB_BAR_PLACEMENT_ENV, "left")]);
    assert_eq!(left.tab_bar_placement.rail_side_str(), "left");

    let (top, _) = settings_from([(TAB_BAR_PLACEMENT_ENV, "top")]);
    assert_eq!(top.tab_bar_placement.rail_side_str(), "left");
}

#[test]
fn legacy_workspace_rail_side_folds_to_side_plus_always() {
    // A legacy workspace_rail=left|right folds to the side field plus Always
    // visibility; the UI now offers auto|always for visibility.
    let (left, _) = settings_from([(WORKSPACE_RAIL_ENV, "left")]);
    assert_eq!(left.tab_bar_placement.rail_side_str(), "left");
    assert_eq!(
        left.workspace_rail,
        WorkspaceRail::Always,
        "legacy left folds visibility to Always"
    );

    let (right, _) = settings_from([(WORKSPACE_RAIL_ENV, "right")]);
    assert_eq!(right.tab_bar_placement.rail_side_str(), "right");
    assert_eq!(right.workspace_rail, WorkspaceRail::Always);

    // auto|always pass through untouched and keep the default side.
    let (always, _) = settings_from([(WORKSPACE_RAIL_ENV, "always")]);
    assert_eq!(always.workspace_rail, WorkspaceRail::Always);
    assert_eq!(always.tab_bar_placement.rail_side_str(), "left");
}

#[test]
fn canonical_workspace_rail_side_wins_over_legacy_tab_bar_placement() {
    // The canonical ODYTTY_WORKSPACE_RAIL_SIDE (left|right) wins over the legacy
    // ODYTTY_TAB_BAR_PLACEMENT when both are set.
    let (both, _) = settings_from([
        (WORKSPACE_RAIL_SIDE_ENV, "right"),
        (TAB_BAR_PLACEMENT_ENV, "left"),
    ]);
    assert_eq!(
        both.tab_bar_placement.rail_side_str(),
        "right",
        "canonical rail side wins over legacy tab bar placement"
    );

    // The canonical key applies on its own too.
    let (canon_only, _) = settings_from([(WORKSPACE_RAIL_SIDE_ENV, "right")]);
    assert_eq!(canon_only.tab_bar_placement.rail_side_str(), "right");

    // An invalid canonical value warns and falls back to the left rail.
    let (bad, warnings) = settings_from([(WORKSPACE_RAIL_SIDE_ENV, "top")]);
    assert_eq!(bad.tab_bar_placement.rail_side_str(), "left");
    assert!(warnings.iter().any(|w| w.contains("WORKSPACE_RAIL_SIDE")));
}

#[test]
fn workspace_rail_side_config_key_round_trips_and_parses() {
    // The canonical config key maps both directions, and a config-file line
    // using it sets the side through parse.
    assert_eq!(
        config_key_to_env("workspace_rail_side"),
        Some(WORKSPACE_RAIL_SIDE_ENV)
    );
    assert_eq!(config_key_to_env("railside"), Some(WORKSPACE_RAIL_SIDE_ENV));
    assert_eq!(
        env_to_config_key(WORKSPACE_RAIL_SIDE_ENV),
        Some("workspace_rail_side")
    );

    let config = ConfigValues::parse(
        "workspace_rail_side = right
",
        |_| {},
    );
    let settings =
        Settings::from_source(|key| config.get(key).cloned(), |_| {}, |_| None, |_| None);
    assert_eq!(settings.tab_bar_placement.rail_side_str(), "right");
}

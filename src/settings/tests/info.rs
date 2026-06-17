// SPDX-License-Identifier: GPL-3.0-only
use super::*;

fn row<'a>(rows: &'a [SettingInfo], key: &str) -> &'a SettingInfo {
    rows.iter()
        .find(|row| row.key == key)
        .unwrap_or_else(|| panic!("missing setting row {key}"))
}

#[test]
fn setting_info_groups_are_contiguous_and_ordered_for_the_panel() {
    let rows = Settings::default().setting_info();
    let groups = rows.iter().fold(Vec::<&str>::new(), |mut groups, row| {
        if groups.last().copied() != Some(row.group) {
            groups.push(row.group);
        }
        groups
    });

    assert_eq!(
        groups,
        vec![
            "Theme",
            "Font",
            "Rendering",
            "Post-process",
            "Cursor",
            "Input",
            "Clipboard",
            "Accessibility",
            "Development",
        ]
    );

    for group in &groups {
        let first = rows.iter().position(|row| row.group == *group).unwrap();
        let last = rows.iter().rposition(|row| row.group == *group).unwrap();
        assert!(
            rows[first..=last].iter().all(|row| row.group == *group),
            "{group} rows are contiguous"
        );
    }
}

#[test]
fn ux4_p3_labels_are_specific_without_renaming_config_keys() {
    let rows = Settings::default().setting_info();

    let osc52 = row(&rows, "osc52_read");
    assert_eq!(osc52.name, "Allow clipboard read (OSC 52)");
    assert_eq!(osc52.env, OSC52_READ_ENV);

    let copy_on_select = row(&rows, "copy_on_select");
    assert_eq!(copy_on_select.name, "Copy selection to clipboard");
    assert_eq!(copy_on_select.env, COPY_ON_SELECT_ENV);

    let visual = row(&rows, "visual");
    assert_eq!(visual.group, "Post-process");
    assert_eq!(visual.name, "Ambient visual effect");
    assert_eq!(visual.env, VISUAL_ENV);
}

#[test]
fn keybinds_info_options_lists_all_actions() {
    // D-KBR-2 / R7: the `keybinds` row's options[] must enumerate every
    // BindableAction (it was stale at 7 of 12). Pinned to the
    // `bindable_action_display_name` authority so the list and the parser tokens
    // can never drift, and so adding a BindableAction variant fails here until
    // its display token is added to the row.
    let rows = Settings::default().setting_info();
    let keybinds = row(&rows, "keybinds");
    let expected = [
        BindableAction::Search,
        BindableAction::SettingsPanel,
        BindableAction::ThemePicker,
        BindableAction::Copy,
        BindableAction::Paste,
        BindableAction::ScrollPageUp,
        BindableAction::ScrollPageDown,
        BindableAction::JumpPromptPrev,
        BindableAction::JumpPromptNext,
        BindableAction::CopyMode,
        BindableAction::Hints,
        BindableAction::ClearInput,
    ];
    let expected_names: Vec<&'static str> = expected
        .iter()
        .map(|action| bindable_action_display_name(*action))
        .collect();
    assert_eq!(keybinds.options, expected_names.as_slice());
    assert_eq!(keybinds.options.len(), 12, "all 12 actions selectable");
}

#[test]
fn help1_cryptic_settings_have_actionable_descriptions() {
    let rows = Settings::default().setting_info();

    let symbol_font = row(&rows, "symbol_font");
    assert_eq!(symbol_font.name, "Symbol font file");
    assert!(symbol_font.description.contains(".ttf/.otf path"));
    assert!(symbol_font.description.contains("symbol fallback"));
    assert!(
        symbol_font
            .description
            .contains("automatic symbol-font search")
    );

    let render_quality = row(&rows, "render_quality");
    assert_eq!(render_quality.name, "Renderer profile");
    assert!(
        render_quality
            .description
            .contains("balanced is the default")
    );
    assert!(
        render_quality
            .description
            .contains("plain is the hard fast path")
    );
    assert!(render_quality.description.contains("high is reserved"));

    let scanline_period = row(&rows, "crt_scanline_period");
    assert_eq!(scanline_period.name, "CRT scanline spacing");
    assert!(scanline_period.description.contains("physical pixels"));
    assert!(scanline_period.description.contains("Smaller values"));
    assert!(scanline_period.description.contains("2.0"));
    assert_eq!(scanline_period.range.as_deref(), Some("2.0..=12.0 px"));

    // General sweep: raw-abbreviation and raw-key names replaced with plain English.
    let subpixel = row(&rows, "subpixel");
    assert_eq!(subpixel.name, "Subpixel antialiasing");
    assert!(subpixel.description.contains("RGB or BGR"));
    assert!(subpixel.description.contains("LCD"));

    let synthetic_styles = row(&rows, "synthetic_styles");
    assert_eq!(synthetic_styles.name, "Synthesize bold & italic");
    assert!(synthetic_styles.description.contains("bold and italic"));
}

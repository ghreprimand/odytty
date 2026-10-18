// SPDX-License-Identifier: GPL-3.0-only
//! Paragraph direction (`bidi_direction`) setting coverage: left to right by
//! default, parsed from env and `odytty.conf`, an Enum row beside
//! `bidi_reorder`, and persisted under its canonical key.

use super::*;

fn settings_from_config(config_contents: &str) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let config = ConfigValues::parse(config_contents, |message| warnings.push(message));
    let settings = Settings::from_source(
        |key| config.get(key).cloned(),
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    (settings, warnings)
}

#[test]
fn bidi_direction_defaults_ltr_and_parses_env_config_and_aliases() {
    let (default, warnings) = settings_from_config("");
    assert_eq!(default.bidi_direction, BidiDirection::LeftToRight);
    assert!(warnings.is_empty());
    for (line, expected) in [
        ("bidi_direction = rtl", BidiDirection::RightToLeft),
        ("bidi_direction = AUTO", BidiDirection::Auto),
        (
            "paragraph_direction = right-to-left",
            BidiDirection::RightToLeft,
        ),
        ("bidi_direction = ltr", BidiDirection::LeftToRight),
    ] {
        let (settings, warnings) = settings_from_config(line);
        assert_eq!(settings.bidi_direction, expected, "{line}");
        assert!(warnings.is_empty(), "{line}: {warnings:?}");
    }
    let from_env = Settings::from_source(
        |key| (key == BIDI_DIRECTION_ENV).then(|| OsString::from("auto")),
        |_| {},
        |_| None,
        |_| None,
    );
    assert_eq!(from_env.bidi_direction, BidiDirection::Auto);
    let (invalid, warnings) = settings_from_config("bidi_direction = sideways");
    assert_eq!(invalid.bidi_direction, BidiDirection::LeftToRight);
    assert!(!warnings.is_empty(), "an invalid value warns");
}

#[test]
fn bidi_direction_maps_to_one_canonical_config_key_and_round_trips() {
    assert_eq!(
        config_key_to_env("bidi_direction"),
        Some(BIDI_DIRECTION_ENV)
    );
    assert_eq!(
        env_to_config_key(BIDI_DIRECTION_ENV),
        Some("bidi_direction")
    );
    for direction in [
        BidiDirection::LeftToRight,
        BidiDirection::RightToLeft,
        BidiDirection::Auto,
    ] {
        assert_eq!(BidiDirection::parse(direction.as_str()), Some(direction));
    }
}

#[test]
fn bidi_direction_has_a_reloadable_rendering_row() {
    let row = Settings::default()
        .setting_info()
        .into_iter()
        .find(|row| row.key == "bidi_direction")
        .expect("bidi_direction panel row");
    assert_eq!(row.group, "Rendering");
    assert_eq!(row.env, BIDI_DIRECTION_ENV);
    assert_eq!(row.value, "ltr");
    assert!(row.reloadable);
    assert!(matches!(row.kind, SettingKind::Enum));
    assert_eq!(row.options, &["ltr", "rtl", "auto"]);
}

/// A live change made outside the editor and not saved becomes an unsaved
/// edit over the saved baseline; the editor's own pending edit of another key
/// stays.
#[test]
fn adopt_unsaved_keeps_the_saved_baseline_and_pending_edits() {
    let base = Settings::default();
    let mut edits = SettingsEditOverlay::new(&base);
    edits
        .apply_raw("font_size", "24")
        .expect("a pending panel edit");
    let live = Settings {
        bidi_reorder: true,
        bidi_direction: BidiDirection::RightToLeft,
        font_size_px: 24.0,
        ..base.clone()
    };
    edits.adopt_unsaved(&live);
    assert!(edits.settings().bidi_reorder);
    assert_eq!(edits.settings().bidi_direction, BidiDirection::RightToLeft);
    assert_eq!(edits.settings().font_size_px, 24.0);
    let mut keys: Vec<_> = edits.changes().into_iter().map(|edit| edit.key).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["bidi_direction", "bidi_reorder", "font_size"]);

    // Adopting the same live settings again changes nothing.
    edits.adopt_unsaved(&live);
    assert_eq!(edits.changed_count(), 3);
    // Saving makes them the baseline.
    edits.mark_saved();
    assert_eq!(edits.changed_count(), 0);
}

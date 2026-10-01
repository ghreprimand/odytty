// SPDX-License-Identifier: GPL-3.0-only
//! East Asian Ambiguous width: narrow by default, wide only when asked.

use std::ffi::OsString;

use crate::profiles::LaunchProfile;
use crate::settings::{AMBIGUOUS_WIDTH_ENV, AmbiguousWidth, Settings};

use super::*;

#[test]
fn section_sign_is_narrow_until_the_flag_is_on() {
    assert_eq!(crate::core::char_display_width('\u{a7}', false), 1);
    assert_eq!(crate::core::char_display_width('\u{a7}', true), 2);
    assert_eq!(crate::core::char_display_width('A', false), 1);
    assert_eq!(crate::core::char_display_width('A', true), 1);
    assert_eq!(crate::core::char_display_width('\u{4e00}', false), 2);
    assert_eq!(crate::core::char_display_width('\u{4e00}', true), 2);
    assert_eq!(crate::core::char_display_width('\u{0301}', false), 0);
    assert_eq!(crate::core::char_display_width('\u{0301}', true), 0);
}

#[test]
fn toggling_wide_reflows_the_cursor_and_keeps_the_character() {
    let mut terminal = Terminal::new(20, 2);
    terminal.advance("\u{a7}".as_bytes());
    assert_eq!(terminal.screen().cursor().column, 1);
    assert!(terminal.screen().output_since_last_resize());
    let before = terminal.snapshot();
    assert_eq!(before.cells[0].ch, '\u{a7}');
    assert!(!before.cells[1].wide_continuation);

    terminal.set_ambiguous_wide(true);
    assert_eq!(terminal.screen().dimensions(), Dimensions::new(20, 2));
    assert_eq!(terminal.screen().cursor().column, 2);
    assert!(terminal.screen().output_since_last_resize());
    let wide = terminal.snapshot();
    assert_eq!(wide.cells[0].ch, '\u{a7}');
    assert!(wide.cells[1].wide_continuation);

    terminal.set_ambiguous_wide(false);
    assert_eq!(terminal.screen().cursor().column, 1);
    let narrow = terminal.snapshot();
    assert_eq!(narrow.cells[0].ch, '\u{a7}');
    assert!(!narrow.cells[1].wide_continuation);
}

#[test]
fn profile_wide_beats_a_narrow_global_and_a_missing_field_keeps_global() {
    assert_eq!(
        AmbiguousWidth::from_profile(Some("wide"), AmbiguousWidth::Narrow),
        AmbiguousWidth::Wide
    );
    assert_eq!(
        AmbiguousWidth::from_profile(Some("narrow"), AmbiguousWidth::Wide),
        AmbiguousWidth::Narrow
    );
    assert_eq!(
        AmbiguousWidth::from_profile(None, AmbiguousWidth::Narrow),
        AmbiguousWidth::Narrow
    );
    assert_eq!(
        AmbiguousWidth::from_profile(Some("unknown"), AmbiguousWidth::Wide),
        AmbiguousWidth::Wide
    );

    let missing =
        LaunchProfile::parse_json(r#"{"schema_version":1,"name":"plain"}"#, Some("plain"))
            .expect("profile");
    assert_eq!(missing.appearance.ambiguous_width, None);
    assert_eq!(
        AmbiguousWidth::from_profile(
            missing.appearance.ambiguous_width.as_deref(),
            AmbiguousWidth::Narrow
        ),
        AmbiguousWidth::Narrow
    );

    let wide = LaunchProfile::parse_json(
        r#"{"schema_version":1,"name":"wide","appearance":{"ambiguous_width":"wide"}}"#,
        Some("wide"),
    )
    .expect("profile");
    assert_eq!(
        AmbiguousWidth::from_profile(
            wide.appearance.ambiguous_width.as_deref(),
            AmbiguousWidth::Narrow
        ),
        AmbiguousWidth::Wide
    );

    let (settings, warnings) = settings_from([]);
    assert_eq!(settings.ambiguous_width, AmbiguousWidth::Narrow);
    assert!(warnings.is_empty());
    let (settings, _) = settings_from([(AMBIGUOUS_WIDTH_ENV, "wide")]);
    assert_eq!(settings.ambiguous_width, AmbiguousWidth::Wide);
    let (settings, warnings) = settings_from([(AMBIGUOUS_WIDTH_ENV, "banana")]);
    assert_eq!(settings.ambiguous_width, AmbiguousWidth::Narrow);
    assert_eq!(warnings.len(), 1);
}

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

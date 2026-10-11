// SPDX-License-Identifier: GPL-3.0-only
//! Quick-terminal size, shortcut, and profile values are validated when the
//! setting is read, not first when the host uses them.

use super::*;

fn quick(key: &'static str, value: &str) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let settings = Settings::from_source(
        |name| (name == key).then(|| OsString::from(value)),
        |message| warnings.push(message.to_owned()),
        |_| None,
        |_| None,
    );
    (settings, warnings)
}

#[test]
fn quick_terminal_pixel_sizes_are_capped() {
    let (settings, warnings) = quick(QUICK_TERMINAL_COVERAGE_ENV, "16384px");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(settings.quick_terminal_coverage, "16384px");
    let (settings, warnings) = quick(QUICK_TERMINAL_SPAN_ENV, "99999999px");
    assert_eq!(warnings.len(), 1, "an absurd pixel size warns");
    assert_eq!(settings.quick_terminal_span, DEFAULT_QUICK_TERMINAL_SPAN);
    // A bare number stays a pixel size, as documented.
    let (settings, warnings) = quick(QUICK_TERMINAL_COVERAGE_ENV, "600");
    assert!(warnings.is_empty());
    assert_eq!(settings.quick_terminal_coverage, "600px");
}

#[test]
fn quick_terminal_shortcut_refuses_control_and_overlong_text() {
    let (settings, warnings) = quick(QUICK_TERMINAL_SHORTCUT_ENV, "ctrl+shift+grave");
    assert!(warnings.is_empty());
    assert_eq!(settings.quick_terminal_shortcut, "ctrl+shift+grave");
    for bad in ["F1\u{1b}[2J".to_owned(), "x".repeat(65)] {
        let (settings, warnings) = quick(QUICK_TERMINAL_SHORTCUT_ENV, &bad);
        assert_eq!(warnings.len(), 1, "{bad:?}");
        assert_eq!(
            settings.quick_terminal_shortcut,
            DEFAULT_QUICK_TERMINAL_SHORTCUT
        );
    }
}

#[test]
fn quick_terminal_profile_must_be_a_valid_profile_name() {
    let (settings, warnings) = quick(QUICK_TERMINAL_PROFILE_ENV, " dropdown ");
    assert!(warnings.is_empty());
    assert_eq!(settings.quick_terminal_profile, "dropdown");
    let (settings, warnings) = quick(QUICK_TERMINAL_PROFILE_ENV, "../escape");
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        settings.quick_terminal_profile,
        DEFAULT_QUICK_TERMINAL_PROFILE
    );
}

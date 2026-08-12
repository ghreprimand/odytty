// SPDX-License-Identifier: GPL-3.0-only
//! THEME-CAPTURE: "Create theme from current colors".
//!
//! The flow snapshots the focused pane's *effective* dynamic-color state — what
//! the pane is actually displaying — into a theme draft and hands it to the
//! theme editor. These tests pin the two properties that make it trustworthy:
//!
//! 1. The captured spec matches the live state, with live `OSC 4`/`10`/`11`/`12`
//!    overrides winning over the theme-seeded values and the theme-seeded values
//!    used wherever no override exists.
//! 2. Nothing changes by default: capturing is inert until invoked, and
//!    invoking it neither repaints the pane nor alters the applied theme.
//!
//! Platform-neutral: the capture reads terminal state and does pure color
//! arithmetic, with no filesystem, process, or window-system involvement, so it
//! behaves identically on Linux, macOS, and Windows.

use super::*;
use crate::palette_catalog::{DEFAULT_PALETTE_ACTIONS, PaletteAction};
use crate::theme::{Srgb, Theme};

/// A theme whose colors are nothing like the defaults, so "captured from the
/// theme" is unmistakable from "left at `DynamicColors::default()`".
fn distinctive_theme() -> Theme {
    let mut theme = Theme::PLAIN;
    theme.name = "plain";
    theme.foreground = (0x11, 0x22, 0x33);
    theme.background = (0x44, 0x55, 0x66);
    theme.cursor = (0x77, 0x88, 0x99);
    theme.palette = std::array::from_fn(|index| (0xA0 + index as u8, 0x10, 0x20));
    theme
}

fn app_with_theme(theme: Theme) -> App {
    let settings = Settings {
        theme,
        ..Default::default()
    };
    let (app, _terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(40, 8),
        settings,
    );
    app
}

// ---------------------------------------------------------------------------
// (1) The captured spec matches the live dynamic-color state
// ---------------------------------------------------------------------------

#[test]
fn capture_takes_theme_seeded_colors_when_no_override_exists() {
    let _guard = crate::test_lock::render_globals_lock();
    let theme = distinctive_theme();
    let app = app_with_theme(theme);

    let spec = app.captured_theme_spec_for_test();
    assert_eq!(spec.foreground, theme.foreground);
    assert_eq!(spec.background, theme.background);
    assert_eq!(spec.cursor, theme.cursor);
    assert_eq!(spec.palette, theme.palette);
}

#[test]
fn live_default_color_overrides_win_over_the_theme() {
    let _guard = crate::test_lock::render_globals_lock();
    let theme = distinctive_theme();
    let mut app = app_with_theme(theme);

    // OSC 10 / 11 / 12: foreground, background, cursor.
    app.advance_primary_terminal_for_test(b"\x1b]10;rgb:aa/bb/cc\x07");
    app.advance_primary_terminal_for_test(b"\x1b]11;rgb:01/02/03\x07");
    app.advance_primary_terminal_for_test(b"\x1b]12;rgb:0f/0e/0d\x07");

    let spec = app.captured_theme_spec_for_test();
    assert_eq!(spec.foreground, (0xAA, 0xBB, 0xCC));
    assert_eq!(spec.background, (0x01, 0x02, 0x03));
    assert_eq!(spec.cursor, (0x0F, 0x0E, 0x0D));
    // `clear` follows the captured background, not the theme's.
    assert_eq!(spec.clear, (0x01, 0x02, 0x03));
    // Un-overridden palette slots still come from the theme.
    assert_eq!(spec.palette, theme.palette);
}

#[test]
fn live_palette_overrides_win_per_slot() {
    let _guard = crate::test_lock::render_globals_lock();
    let theme = distinctive_theme();
    let mut app = app_with_theme(theme);

    // OSC 4: override two slots, leave the other fourteen theme-seeded. This is
    // the precedence rule the whole feature rests on — a partial override must
    // produce a partially-overridden capture, never all-or-nothing.
    app.advance_primary_terminal_for_test(b"\x1b]4;1;rgb:ff/00/00\x07");
    app.advance_primary_terminal_for_test(b"\x1b]4;9;rgb:00/ff/00\x07");

    let spec = app.captured_theme_spec_for_test();
    assert_eq!(spec.palette[1], (0xFF, 0x00, 0x00), "OSC 4 override wins");
    assert_eq!(spec.palette[9], (0x00, 0xFF, 0x00), "OSC 4 override wins");
    for index in [0usize, 2, 3, 8, 10, 15] {
        assert_eq!(
            spec.palette[index], theme.palette[index],
            "slot {index} had no override and must stay theme-seeded"
        );
    }
}

#[test]
fn resetting_an_override_returns_the_capture_to_the_theme_value() {
    let _guard = crate::test_lock::render_globals_lock();
    let theme = distinctive_theme();
    let mut app = app_with_theme(theme);

    app.advance_primary_terminal_for_test(b"\x1b]11;rgb:01/02/03\x07");
    assert_eq!(
        app.captured_theme_spec_for_test().background,
        (0x01, 0x02, 0x03)
    );

    // OSC 111 resets the default background; the capture must follow the live
    // state back, not latch the override.
    app.advance_primary_terminal_for_test(b"\x1b]111\x07");
    assert_eq!(
        app.captured_theme_spec_for_test().background,
        theme.background
    );
}

#[test]
fn captured_draft_reaches_the_theme_editor() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = app_with_theme(distinctive_theme());
    app.advance_primary_terminal_for_test(b"\x1b]11;rgb:07/08/09\x07");

    let expected = app.captured_theme_spec_for_test();
    app.open_theme_capture_for_test();

    let draft = app
        .theme_builder_draft_for_test()
        .expect("the capture flow opens the theme editor");
    assert_eq!(draft.background, expected.background);
    assert_eq!(draft.foreground, expected.foreground);
    assert_eq!(draft.palette, expected.palette);
    assert_eq!(draft.selection, expected.selection);
    assert_eq!(draft.border, expected.border);
    assert_eq!(draft.inactive, expected.inactive);
    assert_eq!(draft.search, expected.search);
}

#[test]
fn capture_is_reachable_from_the_command_palette() {
    // The row must exist in the default catalog and resolve to the flow's
    // action, or the feature is undiscoverable.
    assert!(
        DEFAULT_PALETTE_ACTIONS.contains(&PaletteAction::CreateThemeFromColors),
        "the capture row is missing from the default palette catalog"
    );
    assert_eq!(
        PaletteAction::from_id("theme-from-colors"),
        Some(PaletteAction::CreateThemeFromColors)
    );
    assert_eq!(
        PaletteAction::CreateThemeFromColors.label(),
        "Create Theme From Current Colors"
    );
}

// ---------------------------------------------------------------------------
// (2) No default behavior changes
// ---------------------------------------------------------------------------

#[test]
fn capture_does_not_change_the_applied_theme_or_the_pane() {
    let _guard = crate::test_lock::render_globals_lock();
    let theme = distinctive_theme();
    let mut app = app_with_theme(theme);
    app.advance_primary_terminal_for_test(b"\x1b]11;rgb:07/08/09\x07");

    let before: Srgb = app.active_theme_for_test().background;
    let live_before = app.captured_theme_spec_for_test();

    app.open_theme_capture_for_test();

    assert_eq!(
        app.active_theme_for_test().background,
        before,
        "capturing must not apply anything to the live theme"
    );
    assert_eq!(
        app.captured_theme_spec_for_test(),
        live_before,
        "capturing must not disturb the pane's dynamic-color state"
    );
}

#[test]
fn nothing_captures_until_the_flow_is_invoked() {
    let _guard = crate::test_lock::render_globals_lock();
    let mut app = app_with_theme(distinctive_theme());
    app.advance_primary_terminal_for_test(b"\x1b]11;rgb:07/08/09\x07");

    // No overlay is open, so there is no draft: the feature is entirely
    // user-invoked and adds no passive work to the frame path.
    assert!(!app.overlay_open_for_test());
    assert!(app.theme_builder_draft_for_test().is_none());
}

#[test]
fn the_ordinary_theme_editor_still_opens_on_a_clone_of_the_active_theme() {
    // The pre-existing entry point must be unaffected by the new one: opening
    // the editor normally still clones the applied theme, ignoring live
    // overrides.
    let _guard = crate::test_lock::render_globals_lock();
    let theme = distinctive_theme();
    let mut app = app_with_theme(theme);
    app.advance_primary_terminal_for_test(b"\x1b]11;rgb:07/08/09\x07");

    app.open_theme_builder_for_test();
    let draft = app
        .theme_builder_draft_for_test()
        .expect("the editor is open");
    assert_eq!(
        draft.background, theme.background,
        "the clone path must not pick up the live OSC 11 override"
    );
}

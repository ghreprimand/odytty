// SPDX-License-Identifier: GPL-3.0-only
//! OS-THEME native-wiring integration: following the desktop dark/light
//! appearance preference selects between a configured dark and light theme
//! through the real override path, while the off path (and an unset/unknown
//! direction) leaves the authored theme in place — byte-identical to before the
//! feature existed. These drive a real `App` over a one-shot PTY (skipped when
//! none is available); no GPU is needed since the active theme is observable
//! before any surface exists.

use super::*;
use winit::window::Theme as WinitTheme;

const COLS: usize = 40;
const ROWS: usize = 8;

/// Build an `App` over a one-shot PTY whose authored theme is `theme`. Returns
/// `None` when no PTY is available.
fn build_app(theme: Theme) -> Option<App> {
    let dims = Dimensions::new(COLS, ROWS);
    let settings = Settings {
        theme,
        ..Default::default()
    };
    let (app, _terminal) = headless_app_with(NativeOptions::default(), dims, settings);
    Some(app)
}

#[test]
fn following_off_keeps_the_authored_theme_on_any_os_signal() {
    let authored = Theme::ODYSSEY;
    let Some(mut app) = build_app(authored) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // The pure resolver returns the authored theme exactly while following is
    // off — the byte-identical off-path guarantee.
    assert_eq!(app.resolve_active_theme_for_test(), authored);

    // Even with a dark/light pair configured AND an OS signal present, following
    // off ignores all of it and the active theme stays the authored one.
    let resolved = app.apply_os_theme_for_test(
        false,
        Some("odyssey-noir"),
        Some("plain"),
        Some(WinitTheme::Dark),
    );
    assert_eq!(resolved, authored);
    assert_eq!(app.active_theme_for_test(), authored);
}

#[test]
fn following_on_switches_between_dark_and_light_themes() {
    let authored = Theme::ODYSSEY;
    let Some(mut app) = build_app(authored) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Dark signal selects the configured dark theme.
    let dark = app.apply_os_theme_for_test(
        true,
        Some("odyssey-noir"),
        Some("plain"),
        Some(WinitTheme::Dark),
    );
    assert_eq!(dark, Theme::ODYSSEY_NOIR);
    assert_eq!(app.active_theme_for_test(), Theme::ODYSSEY_NOIR);

    // Light signal selects the configured light theme.
    let light = app.apply_os_theme_for_test(
        true,
        Some("odyssey-noir"),
        Some("plain"),
        Some(WinitTheme::Light),
    );
    assert_eq!(light, Theme::PLAIN);
    assert_eq!(app.active_theme_for_test(), Theme::PLAIN);
}

#[test]
fn unset_or_unknown_direction_falls_back_to_the_authored_theme() {
    let authored = Theme::ODYSSEY;
    let Some(mut app) = build_app(authored) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Dark direction unset → a dark signal keeps the authored theme (no guess).
    let resolved = app.apply_os_theme_for_test(true, None, Some("plain"), Some(WinitTheme::Dark));
    assert_eq!(resolved, authored);

    // Unknown theme name → fall back to authored, never crash.
    let resolved = app.apply_os_theme_for_test(
        true,
        Some("no-such-theme-xyz"),
        Some("plain"),
        Some(WinitTheme::Dark),
    );
    assert_eq!(resolved, authored);

    // No OS signal (X11 with no env seed) → authored theme regardless of pair.
    let resolved = app.apply_os_theme_for_test(true, Some("odyssey-noir"), Some("plain"), None);
    assert_eq!(resolved, authored);
}

#[test]
fn defaults_are_off_path_identity() {
    // Following off, both directions unset — the feature is fully inert by
    // default. (Config-key round-tripping is covered in the settings tests.)
    let defaults = Settings::default();
    assert!(!defaults.follow_os_theme);
    assert!(defaults.os_theme_dark.is_none());
    assert!(defaults.os_theme_light.is_none());
}

/// Round-3 acceptance regression (item 4/7): a profile tab's theme is session
/// state that survives the global sweeps (settings write, OS-appearance flip)
/// and drives the window chrome while the pane is active. Toggling
/// `follow_os_theme` on then off must not flatten the profile tab.
#[test]
fn profile_theme_survives_sweeps_and_drives_chrome() {
    let authored = Theme::ODYSSEY;
    let Some(mut app) = build_app(authored) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    let dracula = Theme::from_name("dracula").expect("dracula builtin");

    // The active (only) session is a plain tab: chrome is the global theme.
    assert_eq!(app.chrome_theme_for_test(), authored);

    // Stamp it as a profile tab with theme dracula (what a profile launch does).
    app.set_active_profile_theme_for_test(Some(dracula));
    assert_eq!(app.active_profile_theme_for_test(), Some(dracula));
    assert_eq!(
        app.chrome_theme_for_test(),
        dracula,
        "an active profile tab presents its profile theme on the chrome"
    );

    // A settings-write sweep must not flatten the profile tab.
    app.apply_model_state_to_all_sessions_for_test();
    assert_eq!(app.active_profile_theme_for_test(), Some(dracula));
    assert_eq!(app.chrome_theme_for_test(), dracula);

    // Toggling follow_os on then off (the reported item-7 sequence) sweeps the
    // sessions twice; the profile tab keeps dracula throughout.
    let _ = app.apply_os_theme_for_test(
        true,
        Some("odyssey-noir"),
        Some("plain"),
        Some(WinitTheme::Dark),
    );
    assert_eq!(app.active_profile_theme_for_test(), Some(dracula));
    assert_eq!(app.chrome_theme_for_test(), dracula);
    let _ = app.apply_os_theme_for_test(
        false,
        Some("odyssey-noir"),
        Some("plain"),
        Some(WinitTheme::Dark),
    );
    assert_eq!(
        app.chrome_theme_for_test(),
        dracula,
        "toggling follow_os off must not flatten the profile tab to the global theme"
    );
}

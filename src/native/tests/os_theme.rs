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
    let session = PtySession::spawn_shell_command(dims, "sleep 1").ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let mut settings = Settings::default();
    settings.theme = theme;
    let app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
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

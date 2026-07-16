// SPDX-License-Identifier: GPL-3.0-only
//! U4 CVD native-wiring integration: a live `cvd_mode` change through the real
//! `apply_settings` chokepoint updates the effective theme that feeds every
//! renderer publish, while the off path leaves it byte-identical to the
//! authored theme (the pixel-identical guarantee at the wiring level). These
//! drive a real `App` over a one-shot PTY (skipped when none is available, as in
//! CI sandboxes), exercising the production apply path — no GPU needed, since
//! the effective theme is observable before any surface exists.

use super::*;
use crate::settings::CvdMode;

const COLS: usize = 40;
const ROWS: usize = 8;

/// A confusable theme: ANSI red (1) and green (2) a deutan viewer collapses,
/// over a dark background, so adaptation has something to move.
fn confusable_theme() -> Theme {
    let mut theme = Theme::PLAIN;
    theme.background = (0x10, 0x10, 0x10);
    theme.foreground = (0xE0, 0xE0, 0xE0);
    theme.palette[1] = (0xC0, 0x30, 0x30);
    theme.palette[2] = (0x30, 0xA0, 0x30);
    theme
}

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
fn default_off_publishes_the_authored_theme_unchanged() {
    let theme = confusable_theme();
    let Some(app) = build_app(theme) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // cvd_mode defaults Off: the effective theme equals the authored one, so the
    // renderer sees byte-identical colors (the plain path is preserved).
    assert_eq!(
        app.effective_theme_for_test(),
        theme,
        "with CVD off the published theme is the authored theme, bitwise"
    );
}

#[test]
fn enabling_a_mode_through_apply_settings_adapts_the_published_theme() {
    // `apply_cvd_for_test` drives the real reload seam, which republishes the
    // process-global render state (default colors / palette / contrast floor).
    // Serialize against the other render-globals tests so a parallel run can't
    // interleave a global write under another test's read.
    let _guard = crate::test_lock::render_globals_lock();
    let theme = confusable_theme();
    let Some(mut app) = build_app(theme) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    assert_eq!(app.effective_theme_for_test(), theme);

    // Turn deutan adaptation on through the real apply_settings path.
    app.apply_cvd_for_test(CvdMode::Deutan, 1.0);
    let adapted = app.effective_theme_for_test();
    assert_ne!(
        adapted, theme,
        "enabling deutan must adapt the published theme"
    );
    assert!(
        adapted.palette[1] != theme.palette[1] || adapted.palette[2] != theme.palette[2],
        "the confusable red/green must move so a deutan viewer can separate them"
    );

    // Turning it back off restores the authored theme exactly (the off path is
    // an exact return to the plain colors, not a near-miss).
    app.apply_cvd_for_test(CvdMode::Off, 1.0);
    assert_eq!(
        app.effective_theme_for_test(),
        theme,
        "returning to off must republish the authored theme bitwise"
    );
}

#[test]
fn zero_strength_with_a_mode_set_is_an_exact_passthrough() {
    // Serialize against the other render-globals tests: `apply_cvd_for_test`
    // republishes the process-global render state through the reload seam.
    let _guard = crate::test_lock::render_globals_lock();
    let theme = confusable_theme();
    let Some(mut app) = build_app(theme) else {
        eprintln!("skipping: no PTY available");
        return;
    };
    // A mode is selected but strength 0 → the second pixel-identical net: the
    // published theme is the authored one, bypassing the palette re-floor.
    app.apply_cvd_for_test(CvdMode::Deutan, 0.0);
    assert_eq!(
        app.effective_theme_for_test(),
        theme,
        "strength 0 publishes the authored theme unchanged even with a mode set"
    );
}

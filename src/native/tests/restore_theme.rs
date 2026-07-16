// SPDX-License-Identifier: GPL-3.0-only
//! RESTORE-THEME: sessions spawned by snapshot restore-on-launch or layout
//! append must render in the CURRENT theme, not the `DynamicColors::default()`
//! palette.
//!
//! Live-created sessions receive the theme through `initialize_session_with`
//! right after spawn. Restore and append spawn terminals inside the session
//! arena without routing through that path, so before the fix they kept the
//! default palette and rendered every `Color::Default` / `Color::Indexed`
//! surface — most visibly context menus and overlays, which paint in the
//! terminal palette — in the wrong colors. Two workspaces in one window then
//! diverged even though every setting is app-global. The App now seeds every
//! session's model state after restore and after append.
//!
//! Cross-platform: the restore/append spawn path and the seeding sweep are
//! identical on Linux, macOS, and Windows; these tests are platform-neutral
//! (both run headlessly through the model-state sweep and the layout-append
//! seam — no off-main-thread winit `EventLoop` is needed, so CI exercises the
//! seed rather than skipping it).

use super::super::session::RestoreReport;
use super::*;
use crate::core::RgbColor;
use crate::theme::Theme;

// A theme whose default colors are nothing like `DynamicColors::default()`
// (0xCCCCCC on 0x0B0C10), so "seeded from the theme" is unmistakable from
// "left at the default palette".
const THEME_FG: RgbColor = RgbColor {
    red: 0x11,
    green: 0x22,
    blue: 0x33,
};
const THEME_BG: RgbColor = RgbColor {
    red: 0x44,
    green: 0x55,
    blue: 0x66,
};
const DEFAULT_FG: RgbColor = RgbColor {
    red: 0xCC,
    green: 0xCC,
    blue: 0xCC,
};
const DEFAULT_BG: RgbColor = RgbColor {
    red: 0x0B,
    green: 0x0C,
    blue: 0x10,
};

fn distinctive_theme() -> Theme {
    let mut theme = Theme::PLAIN;
    theme.foreground = (THEME_FG.red, THEME_FG.green, THEME_FG.blue);
    theme.background = (THEME_BG.red, THEME_BG.green, THEME_BG.blue);
    theme
}

/// A proxy-less `App` whose authored theme is `theme`. Its one session is built
/// from a bare `Terminal::new` — i.e. UNSEEDED, exactly like a session spawned
/// by restore/append before the app seeds it. `None` when no PTY is available.
fn unseeded_app(theme: Theme) -> Option<App> {
    let dims = Dimensions::new(40, 8);
    let settings = Settings {
        theme,
        ..Default::default()
    };
    let (app, _terminal) = headless_app_with(NativeOptions::default(), dims, settings);
    Some(app)
}

/// The core guarantee: a session whose terminal was built without the theme
/// (the restore/append shape) starts in the DEFAULT palette, and the app's
/// model-state sweep re-seeds it to the theme. Fail-before: without the sweep
/// there is no path that lifts a snapshot-spawned session out of the default
/// palette, so its menus render in the wrong colors.
#[test]
fn model_state_sweep_seeds_an_unseeded_session_with_the_theme() {
    let _guard = crate::test_lock::render_globals_lock();
    let Some(mut app) = unseeded_app(distinctive_theme()) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Restore/append shape: the freshly-built terminal carries the default
    // palette, NOT the theme — this is precisely the divergence.
    assert_eq!(
        app.session_dynamic_colors_for_test(0),
        Some((DEFAULT_FG, DEFAULT_BG)),
        "an unseeded session starts in the default palette (the bug's source)"
    );

    // The sweep restore-on-launch and layout append run.
    app.apply_model_state_to_all_sessions_for_test();

    assert_eq!(
        app.session_dynamic_colors_for_test(0),
        Some((THEME_FG, THEME_BG)),
        "after the sweep the session renders in the theme, not the default palette"
    );
}

/// The production layout-append path seeds the sessions it spawns. A layout
/// appended into a themed window must render its new session in that theme, not
/// the default palette — otherwise its menus/overlays diverge from the live
/// workspaces beside it.
///
/// Runs headlessly through the layout-append seam (no event-loop proxy), so the
/// seed is EXERCISED wherever the suite runs — including CI and macOS, where a
/// real winit `EventLoop` cannot be built and the proxy-backed path would return
/// early and assert nothing. The pre-append state is made NON-pristine (the
/// initial workspace is renamed) so the append lands BESIDE the live workspace
/// at arena index 1 rather than REPLACING the pristine one: pristine-consume
/// reaps the lone pristine workspace's session, which would otherwise leave a
/// single session at index 0 and make the index-1 assertion meaningless.
#[test]
fn appended_layout_session_is_seeded_with_the_theme() {
    let _guard = crate::test_lock::render_globals_lock();
    let Some(mut app) = unseeded_app(distinctive_theme()) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Make the current state non-pristine so the append lands beside the live
    // workspace (index 1) instead of consuming the pristine one.
    app.rename_workspace_for_test(0, "live");

    // Capture the current shape and append it through the headless append path.
    let snapshot = app.capture_shape_for_test();
    let report = app.append_snapshot_headless_for_test(&snapshot);
    assert!(
        matches!(report, RestoreReport::Restored { .. }),
        "the layout must append beside the live workspace (not skip or replace)"
    );

    // The append landed BESIDE, so the arena now holds a second session at
    // index 1 — the newly-spawned appended one (arena order is workspace ->
    // tab -> leaf, so index 1 is deterministic, not HashMap order).
    let appended = app
        .session_dynamic_colors_for_test(1)
        .expect("the appended session exists at arena index 1");
    assert_eq!(
        appended,
        (THEME_FG, THEME_BG),
        "the appended session renders in the theme, not the default palette"
    );
}

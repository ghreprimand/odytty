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
//! (the proxy-backed one is skipped where no PTY / off-main-thread winit
//! `EventLoop` is available, as elsewhere in the suite).

#[cfg(not(target_os = "macos"))]
use super::super::pty::UserEvent;
#[cfg(not(target_os = "macos"))]
use super::super::session::{RestoreReport, Session, SessionToken, WorkspaceSet};
use super::*;
use crate::core::RgbColor;
use crate::theme::Theme;
#[cfg(not(target_os = "macos"))]
use winit::event_loop::EventLoop;
#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "windows")]
use winit::platform::windows::EventLoopBuilderExtWindows;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

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
    let session = spawn_test_pause_shell(dims).ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let settings = Settings {
        theme,
        ..Default::default()
    };
    Some(App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    ))
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

#[cfg(not(target_os = "macos"))]
fn proxy_app(theme: Theme) -> Option<(App, EventLoop<UserEvent>)> {
    let dims = Dimensions::new(40, 8);
    let session = spawn_test_pause_shell(dims).ok()?;
    let writer: PtyWriter = Arc::new(Mutex::new(session.take_writer().ok()?));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
    }
    #[cfg(target_os = "windows")]
    {
        EventLoopBuilderExtWindows::with_any_thread(&mut builder, true);
    }
    let event_loop = builder.build().ok()?;
    let proxy = event_loop.create_proxy();
    let sessions = WorkspaceSet::new(
        Session::new(SessionToken(0), terminal, writer, pty, None),
        Some(proxy),
    );
    let settings = Settings {
        theme,
        ..Default::default()
    };
    let app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, event_loop))
}

/// The production layout-append path seeds the sessions it spawns. A layout
/// instantiated into a themed window must render its new session in that theme,
/// not the default palette — otherwise its menus/overlays diverge from the live
/// workspaces beside it.
#[cfg(not(target_os = "macos"))]
#[test]
fn appended_layout_session_is_seeded_with_the_theme() {
    let _guard = crate::test_lock::render_globals_lock();
    let Some((mut app, _event_loop)) = proxy_app(distinctive_theme()) else {
        eprintln!("skipping: no PTY available");
        return;
    };

    // Capture the current one-workspace shape, then append it as a new
    // workspace through the production append path.
    let snapshot = app.capture_shape_for_test();
    let report = app.append_snapshot_for_test(&snapshot);
    assert!(
        matches!(report, RestoreReport::Restored { .. }),
        "the layout must append (not skip)"
    );

    // The appended workspace's session is the newly-spawned one at the tail of
    // the arena; it must carry the theme, not the default palette.
    let appended = app
        .session_dynamic_colors_for_test(1)
        .expect("an appended session exists");
    assert_eq!(
        appended,
        (THEME_FG, THEME_BG),
        "the appended session renders in the theme, not the default palette"
    );
}

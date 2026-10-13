// SPDX-License-Identifier: GPL-3.0-only
//! `ambiguous_width = wide` reaches every session at creation, not only after
//! a config reload or workspace restore. Each case asserts the terminal model's
//! own policy flag, which decides how wide an East Asian Ambiguous character
//! (for example U+00A1, U+00B1 or U+2605) is laid out. Cases that spawn a second
//! session use the discarding waker, so they really spawn on every host,
//! including one with no display.

use super::super::session::{Session, SessionToken, WorkspaceSet};
use super::*;
use crate::settings::{AmbiguousWidth, Settings};

fn wide_settings() -> Settings {
    Settings {
        ambiguous_width: AmbiguousWidth::Wide,
        ..Settings::default()
    }
}

/// An App over a launch session seeded by the production launch seeding, with
/// a workspace set that can spawn further sessions.
fn app_with(settings: Settings) -> App {
    let dims = Dimensions::new(80, 24);
    let mut model = Terminal::new(dims.columns, dims.rows);
    crate::native::seed_launch_session_model(&mut model, &settings);
    let terminal = Arc::new(Mutex::new(model));
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let headless = Arc::new(crate::native::session::HeadlessSession::new(dims));
    let sessions = WorkspaceSet::new(
        Session::new_headless(SessionToken(0), terminal, writer, headless),
        Some(crate::native::event_waker::EventWaker::discard()),
    );
    let mut app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.on_window_focus_changed_for_test(true);
    app
}

#[test]
fn the_launch_session_is_wide_when_configured_and_narrow_otherwise() {
    // The shared launch seeding is what `run_native` and the headless harness
    // both call, so this is the first session of a real launch.
    let (wide, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        wide_settings(),
    );
    assert_eq!(wide.session_ambiguous_wide_for_test(), [true]);
    let (narrow, _) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    assert_eq!(narrow.session_ambiguous_wide_for_test(), [false]);
}

#[test]
fn a_wide_launch_session_lays_out_an_ambiguous_character_in_two_cells() {
    let (app, terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        wide_settings(),
    );
    terminal
        .lock()
        .expect("terminal")
        .advance("\u{00B1}x".as_bytes());
    let terminal = terminal.lock().expect("terminal");
    assert_eq!(terminal.screen().cursor().column, 3);
    assert_eq!(terminal.screen().cell(0, 2).unwrap().ch, 'x');
    drop(terminal);
    drop(app);
}

#[test]
fn greek_and_cyrillic_letters_stay_one_cell_with_wide_on() {
    // They are Neutral in the width table used, not East Asian Ambiguous, so
    // `wide` leaves them alone while the genuinely ambiguous U+00B1 widens.
    let (_app, terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        wide_settings(),
    );
    let mut terminal = terminal.lock().expect("terminal");
    terminal.advance("\u{03A9}\u{0416}".as_bytes());
    assert_eq!(terminal.screen().cursor().column, 2);
}

#[test]
fn a_new_tab_is_wide_when_configured() {
    let mut app = app_with(wide_settings());
    app.new_tab_for_test();
    assert_eq!(app.session_ambiguous_wide_for_test(), [true, true]);
}

#[test]
fn a_new_tab_stays_narrow_by_default() {
    let mut app = app_with(Settings::default());
    app.new_tab_for_test();
    assert_eq!(app.session_ambiguous_wide_for_test(), [false, false]);
}

#[test]
fn a_split_pane_is_wide_when_configured() {
    let mut app = app_with(wide_settings());
    app.split_active_columns_for_test();
    assert_eq!(app.session_ambiguous_wide_for_test(), [true, true]);
}

#[test]
fn a_new_workspace_is_wide_when_configured() {
    let mut app = app_with(wide_settings());
    app.new_workspace_for_test();
    assert_eq!(app.session_ambiguous_wide_for_test(), [true, true]);
}

fn write_wide_profile(base: &std::path::Path, name: &str, width: &str) {
    let dir = crate::profiles::profiles_dir_from_env(
        Some(base.as_os_str().to_owned()),
        None,
        Some(base.as_os_str().to_owned()),
    )
    .expect("profile dir resolves");
    std::fs::create_dir_all(&dir).expect("profile dir");
    let mut profile = crate::profiles::LaunchProfile::new(name).expect("profile");
    profile.appearance.ambiguous_width = Some(width.to_owned());
    crate::profiles::write_profile_file(&dir.join(format!("{name}.profile.json")), &profile)
        .expect("write profile");
}

#[test]
fn a_profile_tab_takes_its_profile_width_over_the_global_one() {
    let base = crate::test_dirs::fresh_temp_dir("otaw");
    write_wide_profile(&base, "cjk", "wide");
    write_wide_profile(&base, "latin", "narrow");
    // Global narrow, profile wide: the profile tab is wide, the launch tab not.
    let result = config_env::with_config_base(&base, false, || {
        let mut app = app_with(Settings::default());
        app.new_tab_with_profile_for_test("cjk");
        app.session_ambiguous_wide_for_test()
    });
    // Global wide, profile narrow: the profile tab is narrow.
    let reverse = config_env::with_config_base(&base, false, || {
        let mut app = app_with(wide_settings());
        app.new_tab_with_profile_for_test("latin");
        app.session_ambiguous_wide_for_test()
    });
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(result, [false, true]);
    assert_eq!(reverse, [true, false]);
}

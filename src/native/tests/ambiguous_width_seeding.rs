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
pub(super) fn app_with(settings: Settings) -> App {
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

#[test]
fn a_settings_sweep_between_reads_keeps_a_mark_on_its_base() {
    // The sweep repeats each session's width on any settings apply. Between a
    // base and its mark arriving in separate reads, that must not end the
    // owner, under either policy.
    for settings in [Settings::default(), wide_settings()] {
        let mut app = app_with(settings);
        let terminal = app.sessions_mut_for_test().active().terminal.clone();
        terminal.lock().expect("terminal").advance(b"e");
        app.apply_model_state_to_all_sessions_for_test();
        let mut terminal = terminal.lock().expect("terminal");
        terminal.advance("\u{0301}".as_bytes());
        assert_eq!(
            terminal.screen().cell(0, 0).unwrap().combining(),
            &['\u{0301}']
        );
        assert_eq!(terminal.screen().cursor().column, 1);
    }
}

/// A profile `name` whose launch runs `/bin/sh -c script sh reply`, with an
/// optional ambiguous-width override.
#[cfg(unix)]
pub(super) fn write_script_profile(
    base: &std::path::Path,
    name: &str,
    width: Option<&str>,
    script: &str,
    reply: &std::path::Path,
) {
    let dir = crate::profiles::profiles_dir_from_env(
        Some(base.as_os_str().to_owned()),
        None,
        Some(base.as_os_str().to_owned()),
    )
    .expect("profile dir resolves");
    std::fs::create_dir_all(&dir).expect("profile dir");
    let mut profile = crate::profiles::LaunchProfile::new(name).expect("profile");
    profile.appearance.ambiguous_width = width.map(str::to_owned);
    profile.launch.command = Some(crate::profiles::ProfileCommand {
        program: "/bin/sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            script.to_owned(),
            "sh".to_owned(),
            reply.to_str().expect("utf-8 scratch path").to_owned(),
        ],
        preserved: Default::default(),
    });
    crate::profiles::write_profile_file(&dir.join(format!("{name}.profile.json")), &profile)
        .expect("write profile");
}

/// Poll `path` until it holds six bytes, within a finite deadline.
#[cfg(unix)]
fn wait_for_report(path: &std::path::Path) -> Vec<u8> {
    wait_for_reply(path, 6)
}

/// Poll `path` until it holds `len` bytes, within a finite deadline.
#[cfg(unix)]
pub(super) fn wait_for_reply(path: &std::path::Path, len: usize) -> Vec<u8> {
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && bytes.len() >= len
        {
            return bytes;
        }
        assert!(Instant::now() < deadline, "the child recorded no report");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Prints U+00B1 and a cursor report request in one write, raw, then stores
/// the six-byte reply.
#[cfg(unix)]
const FIRST_REPORT_SCRIPT: &str =
    "stty raw -echo; printf '\\302\\261\\033[6n'; dd bs=1 count=6 of=\"$1\" 2>/dev/null";

#[cfg(unix)]
#[test]
fn a_profile_tab_answers_its_first_cursor_report_at_its_own_width() {
    // The pump answers before the App's post-spawn initialization runs: the
    // hook holds the App until the reply is on disk. Both precedence
    // directions: global narrow with a wide profile, global wide with a
    // narrow one.
    for (global, profile_width, expected) in [
        (Settings::default(), "wide", b"\x1b[1;3R"),
        (wide_settings(), "narrow", b"\x1b[1;2R"),
    ] {
        let base = crate::test_dirs::ScratchDir::new("otfr");
        let reply = base.path().join("reply");
        write_script_profile(
            base.path(),
            "probe",
            Some(profile_width),
            FIRST_REPORT_SCRIPT,
            &reply,
        );
        let hook_reply = reply.clone();
        let report = config_env::with_config_base(base.path(), false, || {
            let mut app = app_with(global);
            app.sessions_mut_for_test().pump_started_hook = Some(Box::new(move |_| {
                wait_for_report(&hook_reply);
            }));
            app.new_tab_with_profile_for_test("probe");
            assert_eq!(app.session_ambiguous_wide_for_test().len(), 2);
            wait_for_report(&reply)
        });
        assert_eq!(report, expected, "profile {profile_width}");
    }
}

#[cfg(unix)]
#[test]
fn initialization_between_two_reads_keeps_a_mark_on_its_base() {
    // The child prints a base, waits for one input byte, then prints the
    // mark and a cursor report request. The hook holds the App until the
    // pump has laid out the base, so the App's initialization runs between
    // the two reads. Default narrow, and a wide profile.
    const SCRIPT: &str = "stty raw -echo; printf e; dd bs=1 count=1 of=/dev/null 2>/dev/null; \
        printf '\\314\\201\\033[6n'; dd bs=1 count=6 of=\"$1\" 2>/dev/null";
    for width in [None, Some("wide")] {
        let base = crate::test_dirs::ScratchDir::new("otsc");
        let reply = base.path().join("reply");
        write_script_profile(base.path(), "split", width, SCRIPT, &reply);
        let (report, cell) = config_env::with_config_base(base.path(), false, || {
            let mut app = app_with(Settings::default());
            app.sessions_mut_for_test().pump_started_hook = Some(Box::new(|terminal| {
                let deadline = Instant::now() + std::time::Duration::from_secs(20);
                while crate::native::lock_recover(terminal)
                    .screen()
                    .cursor()
                    .column
                    != 1
                {
                    assert!(Instant::now() < deadline, "the base never arrived");
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            }));
            app.new_tab_with_profile_for_test("split");
            let session = app
                .sessions_mut_for_test()
                .iter()
                .find(|session| session.id != SessionToken(0))
                .expect("the profile tab's session");
            let terminal = session.terminal.clone();
            {
                use std::io::Write as _;
                let mut writer = session.writer.lock().expect("writer");
                writer.write_all(b"x").expect("release the child");
                writer.flush().expect("flush");
            }
            let report = wait_for_report(&reply);
            let cell = terminal
                .lock()
                .expect("terminal")
                .screen()
                .cell(0, 0)
                .unwrap();
            (report, cell)
        });
        assert_eq!(report, b"\x1b[1;2R", "width {width:?}");
        assert_eq!(cell.ch, 'e');
        assert_eq!(cell.combining(), &['\u{0301}'], "width {width:?}");
    }
}

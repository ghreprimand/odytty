// SPDX-License-Identifier: GPL-3.0-only
//! v0.14 Phase A3 F-A3-6: headless trust-boundary tests for live
//! `App::poll_profile_auto_switch`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::Dimensions;
use crate::native::NativeOptions;
use crate::native::test_support::headless_app_with;
use crate::profiles::{LaunchProfile, ProfileSwitchRules, write_profile_file};
use crate::settings::Settings;

fn temp_config_home(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "odytty-a3-autoswitch-{label}-{}-{nanos}",
        std::process::id()
    ))
}

fn with_home<R>(home: &Path, f: impl FnOnce() -> R) -> R {
    // One crate-wide env lock (crate::test_lock) serializes every test that
    // mutates process-global env vars, so a sibling module redirecting the same
    // HOME/XDG base cannot run concurrently. Poison-tolerant by construction.
    let _guard = crate::test_lock::test_env_lock();
    // The live auto-switch poll loads the profile catalog, bumping the
    // process-global load counter the startup-isolation tests assert on. Hold
    // the catalog-count guard too, acquired AFTER the env lock (fixed order,
    // never the reverse) so it cannot deadlock against a sibling.
    let _count_guard = crate::test_lock::catalog_count_lock();
    // APPDATA is redirected too: on Windows the config base resolves from
    // APPDATA before HOME, so leaving it untouched would point the catalog at
    // the real user profile directory instead of the fixture.
    let prev_appdata = std::env::var_os("APPDATA");
    let prev_home = std::env::var_os("HOME");
    let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
    unsafe {
        std::env::set_var("APPDATA", home);
        std::env::set_var("HOME", home);
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    let result = f();
    unsafe {
        match prev_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        match prev_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match prev_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }
    }
    result
}

fn write_switch_profile(profiles_dir: &Path, name: &str, rules: ProfileSwitchRules) -> PathBuf {
    fs::create_dir_all(profiles_dir).expect("profiles dir");
    let path = profiles_dir.join(format!("{name}.profile.json"));
    let mut profile = LaunchProfile::new(name).expect("profile");
    profile.switch = rules;
    write_profile_file(&path, &profile).expect("write profile");
    path
}

/// Profile directory the redirected env resolves to (platform base rules:
/// `%APPDATA%\\odytty` on Windows, `$HOME/.config/odytty` elsewhere).
fn fixture_profiles_dir(home: &Path) -> PathBuf {
    let dir = crate::profiles::profiles_dir_from_env(
        Some(home.as_os_str().to_owned()),
        None,
        Some(home.as_os_str().to_owned()),
    )
    .expect("fixture profile dir resolves");
    fs::create_dir_all(&dir).expect("config dir");
    dir
}

fn osc7_local(path: &str) -> Vec<u8> {
    let mut bytes = b"\x1b]7;".to_vec();
    bytes.extend_from_slice(format!("file://localhost{path}").as_bytes());
    bytes.push(0x07);
    bytes
}

fn app_with_auto_switch() -> crate::native::app::App {
    let settings = Settings {
        profile_auto_switch: true,
        ..Settings::default()
    };
    let (app, _terminal) = headless_app_with(
        NativeOptions::default(),
        Dimensions {
            columns: 80,
            rows: 24,
        },
        settings,
    );
    app
}

#[test]
fn live_poll_local_pane_uses_cwd_and_stamps_launch_profile() {
    let home = temp_config_home("local");
    let profiles_dir = fixture_profiles_dir(&home);
    write_switch_profile(
        &profiles_dir,
        "work",
        ProfileSwitchRules {
            match_hosts: Vec::new(),
            match_directories: vec!["/work/project".to_owned()],
            preserved: Default::default(),
        },
    );

    with_home(&home, || {
        let mut app = app_with_auto_switch();
        app.advance_primary_terminal_for_test(&osc7_local("/work/project/src"));
        app.poll_profile_auto_switch_for_test();
        assert_eq!(
            app.active_launch_profile_for_test().as_deref(),
            Some("work"),
            "directory rule must switch the local pane"
        );
        assert!(
            app.transient_hud_text_for_test()
                .is_some_and(|text| text.contains("Profile work")),
            "switch must disclose the profile and reason"
        );

        app.advance_primary_terminal_for_test(&osc7_local("/work/project/other"));
        app.poll_profile_auto_switch_for_test();
        assert_eq!(
            app.active_launch_profile_for_test().as_deref(),
            Some("work"),
            "repeat cwd events must not flap away from the active matching profile"
        );
    });

    let _ = fs::remove_dir_all(home);
}

#[test]
fn live_poll_remote_pane_uses_ssh_host_not_osc7_cwd() {
    let home = temp_config_home("remote");
    let profiles_dir = fixture_profiles_dir(&home);
    write_switch_profile(
        &profiles_dir,
        "edge",
        ProfileSwitchRules {
            match_hosts: vec!["edge.example".to_owned()],
            match_directories: Vec::new(),
            preserved: Default::default(),
        },
    );
    write_switch_profile(
        &profiles_dir,
        "work",
        ProfileSwitchRules {
            match_hosts: Vec::new(),
            match_directories: vec!["/work/project".to_owned()],
            preserved: Default::default(),
        },
    );

    with_home(&home, || {
        let mut app = app_with_auto_switch();
        app.set_active_remote_destination_for_test(Some("alice@edge.example:22".to_owned()));
        app.advance_primary_terminal_for_test(&osc7_local("/work/project/src"));
        app.poll_profile_auto_switch_for_test();
        assert_eq!(
            app.active_launch_profile_for_test().as_deref(),
            Some("edge"),
            "remote panes must match on trusted ssh host identity, not OSC 7 cwd"
        );

        app.set_active_remote_destination_for_test(None);
        app.advance_primary_terminal_for_test(&osc7_local("/work/project/src"));
        app.poll_profile_auto_switch_for_test();
        assert_eq!(
            app.active_launch_profile_for_test().as_deref(),
            Some("work"),
            "local panes must still honor directory rules from OSC 7 cwd"
        );
    });

    let _ = fs::remove_dir_all(home);
}

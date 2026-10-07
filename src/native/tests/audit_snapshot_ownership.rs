// SPDX-License-Identifier: GPL-3.0-only
//! A profile edit in a secondary App must not request the shared workspace save.

use super::*;
use crate::native::session::{HeadlessSession, Session, SessionToken, WorkspaceSet};
use crate::profiles::{LaunchProfile, profiles_dir_path, write_profile_file};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_config_base() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!("odytty-audit-profile-{nanos}"))
}

fn with_config_base<R>(base: &Path, f: impl FnOnce() -> R) -> R {
    super::config_env::with_config_base(base, true, f)
}

fn app_with_profile_binding(profile_name: &str, settings: Settings) -> App {
    let dimensions = Dimensions::new(80, 24);
    let terminal = Arc::new(Mutex::new(Terminal::new(80, 24)));
    let mut session = Session::new_headless(
        SessionToken(0),
        terminal,
        crate::native::test_support::headless_writer(),
        Arc::new(HeadlessSession::new(dimensions)),
    );
    session.launch_profile = Some(profile_name.to_owned());
    let mut sessions = WorkspaceSet::new(session, None);
    sessions.workspaces[0].launch_profile = Some(profile_name.to_owned());
    App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        settings,
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    )
}

fn broken_config_path(base: &Path) -> PathBuf {
    let parent_file = base.join("not-a-dir");
    fs::write(&parent_file, b"synthetic config parent").expect("write file parent");
    parent_file.join("odytty.conf")
}

/// The uncommented lines of a config file (the settings writer disables a
/// cleared key by commenting it out).
fn active_config_lines(path: &Path) -> String {
    fs::read_to_string(path)
        .expect("read synthetic config")
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn rendered_profile_manager(app: &mut App) -> String {
    app.render_overlay_rows_for_test(120, 32).join("\n")
}

fn primary_autosave_app() -> App {
    let (mut app, _) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_primary_instance_for_test(true);
    app
}

fn advance_cwd(app: &mut App, cwd: &str) {
    app.advance_primary_terminal_for_test(format!("\x1b]7;file://{cwd}\x07").as_bytes());
}

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn renaming_a_bound_profile_in_a_secondary_app_does_not_save_workspace_shape() {
    let base = temporary_config_base();
    with_config_base(&base, || {
        let profiles_dir = profiles_dir_path().expect("synthetic profile directory");
        fs::create_dir_all(&profiles_dir).expect("create synthetic profile directory");
        let old = LaunchProfile::new("audit-old-profile").expect("valid old profile");
        write_profile_file(&profiles_dir.join("audit-old-profile.profile.json"), &old)
            .expect("write old profile");

        let dimensions = Dimensions::new(80, 24);
        let terminal = Arc::new(Mutex::new(Terminal::new(80, 24)));
        let mut session = Session::new_headless(
            SessionToken(0),
            terminal,
            crate::native::test_support::headless_writer(),
            Arc::new(HeadlessSession::new(dimensions)),
        );
        session.launch_profile = Some("audit-old-profile".to_owned());
        let mut sessions = WorkspaceSet::new(session, None);
        sessions.workspaces[0].launch_profile = Some("audit-old-profile".to_owned());
        let mut app = App::new_with_sessions(
            NativeOptions::default(),
            sessions,
            Settings::default(),
            crate::settings::SettingsReloader::for_current_process(Instant::now()),
        );
        app.set_primary_instance_for_test(false);
        assert_eq!(
            app.active_launch_profile_for_test().as_deref(),
            Some("audit-old-profile")
        );
        let writes_before = app.autosave_saves_for_test();

        let renamed = LaunchProfile::new("audit-new-profile").expect("valid renamed profile");
        app.save_overlay_profile_for_test(renamed, Some("audit-old-profile".to_owned()));

        assert_eq!(
            app.autosave_saves_for_test(),
            writes_before,
            "a secondary profile edit must not write the primary workspace snapshot"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn failed_global_default_save_does_not_claim_success() {
    let base = temporary_config_base();
    with_config_base(&base, || {
        let profiles_dir = profiles_dir_path().expect("synthetic profile directory");
        fs::create_dir_all(&profiles_dir).expect("create synthetic profile directory");
        let profile = LaunchProfile::new("audit-set-default").expect("valid profile");
        write_profile_file(
            &profiles_dir.join("audit-set-default.profile.json"),
            &profile,
        )
        .expect("write synthetic profile");

        let (mut app, _) = headless_app_with(
            NativeOptions::default(),
            Dimensions::new(80, 24),
            Settings::default(),
        );
        app.set_config_path_for_test(broken_config_path(&base));
        app.set_global_default_launch_profile_for_test("audit-set-default");

        let rendered = rendered_profile_manager(&mut app);
        assert!(
            rendered.contains("Could not make audit-set-default the global default"),
            "failed config write must show the failure, not success: {rendered:?}"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn failed_rename_default_update_does_not_claim_profile_saved() {
    let base = temporary_config_base();
    with_config_base(&base, || {
        let profiles_dir = profiles_dir_path().expect("synthetic profile directory");
        fs::create_dir_all(&profiles_dir).expect("create synthetic profile directory");
        let old = LaunchProfile::new("audit-rename-old").expect("valid old profile");
        write_profile_file(&profiles_dir.join("audit-rename-old.profile.json"), &old)
            .expect("write old profile");

        let settings = Settings {
            default_launch_profile: Some("audit-rename-old".to_owned()),
            ..Settings::default()
        };
        let mut app = app_with_profile_binding("audit-rename-old", settings);
        app.set_config_path_for_test(broken_config_path(&base));
        let renamed = LaunchProfile::new("audit-rename-new").expect("valid new profile");
        app.save_overlay_profile_for_test(renamed, Some("audit-rename-old".to_owned()));

        let rendered = rendered_profile_manager(&mut app);
        assert!(
            rendered.contains("Profile audit-rename-new was written, but the global default")
                && !rendered.contains("Saved profile audit-rename-new"),
            "failed default update must report the stale global name: {rendered:?}"
        );
        assert!(
            profiles_dir.join("audit-rename-old.profile.json").is_file(),
            "the old profile must be kept while the global default still names it"
        );
        assert_eq!(
            app.workspace_set().active_workspace_launch_profile(),
            Some("audit-rename-old"),
            "bindings must not be retargeted when the default write failed"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn failed_delete_default_update_does_not_claim_profile_deleted_cleanly() {
    let base = temporary_config_base();
    with_config_base(&base, || {
        let profiles_dir = profiles_dir_path().expect("synthetic profile directory");
        fs::create_dir_all(&profiles_dir).expect("create synthetic profile directory");
        let doomed = LaunchProfile::new("audit-delete-default").expect("valid profile");
        write_profile_file(
            &profiles_dir.join("audit-delete-default.profile.json"),
            &doomed,
        )
        .expect("write profile");

        let settings = Settings {
            default_launch_profile: Some("audit-delete-default".to_owned()),
            ..Settings::default()
        };
        let mut app = app_with_profile_binding("audit-delete-default", settings);
        app.set_config_path_for_test(broken_config_path(&base));
        app.delete_overlay_profile_for_test("audit-delete-default");

        let rendered = rendered_profile_manager(&mut app);
        assert!(
            rendered.contains("Profile audit-delete-default was kept")
                && !rendered.contains("Deleted profile audit-delete-default"),
            "failed default clear must not claim a delete: {rendered:?}"
        );
        assert!(
            profiles_dir
                .join("audit-delete-default.profile.json")
                .is_file(),
            "the profile must be kept while the global default still names it"
        );
        assert_eq!(
            app.workspace_set().active_workspace_launch_profile(),
            Some("audit-delete-default"),
            "bindings must survive a delete that did not happen"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn rename_with_undeletable_old_file_moves_default_and_bindings_to_new_name() {
    let base = temporary_config_base();
    with_config_base(&base, || {
        let profiles_dir = profiles_dir_path().expect("synthetic profile directory");
        // A non-empty directory at the old profile path cannot be removed as a
        // file, standing in for an old profile file that refuses deletion.
        let old_path = profiles_dir.join("audit-stuck-old.profile.json");
        fs::create_dir_all(old_path.join("pin")).expect("create undeletable old path");
        let config_path = base.join("odytty.conf");
        fs::write(&config_path, b"default_launch_profile = audit-stuck-old\n")
            .expect("write synthetic config");

        let settings = Settings {
            default_launch_profile: Some("audit-stuck-old".to_owned()),
            ..Settings::default()
        };
        let mut app = app_with_profile_binding("audit-stuck-old", settings);
        app.set_config_path_for_test(config_path.clone());
        let renamed = LaunchProfile::new("audit-stuck-new").expect("valid new profile");
        app.save_overlay_profile_for_test(renamed, Some("audit-stuck-old".to_owned()));

        let rendered = rendered_profile_manager(&mut app);
        assert!(
            rendered.contains("Profile audit-stuck-new was written, but the old profile")
                && !rendered.contains("Saved profile audit-stuck-new"),
            "an undeletable old file must be reported: {rendered:?}"
        );
        assert!(profiles_dir.join("audit-stuck-new.profile.json").is_file());
        let config = active_config_lines(&config_path);
        assert!(
            config.contains("audit-stuck-new") && !config.contains("audit-stuck-old"),
            "the global default must follow the rename: {config:?}"
        );
        assert_eq!(
            app.workspace_set().active_workspace_launch_profile(),
            Some("audit-stuck-new"),
            "bindings must follow the rename once the default moved"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn delete_with_undeletable_file_clears_default_first_and_keeps_bindings() {
    let base = temporary_config_base();
    with_config_base(&base, || {
        let profiles_dir = profiles_dir_path().expect("synthetic profile directory");
        let stuck_path = profiles_dir.join("audit-stuck-delete.profile.json");
        fs::create_dir_all(stuck_path.join("pin")).expect("create undeletable path");
        let config_path = base.join("odytty.conf");
        fs::write(
            &config_path,
            b"default_launch_profile = audit-stuck-delete\n",
        )
        .expect("write synthetic config");

        let settings = Settings {
            default_launch_profile: Some("audit-stuck-delete".to_owned()),
            ..Settings::default()
        };
        let mut app = app_with_profile_binding("audit-stuck-delete", settings);
        app.set_config_path_for_test(config_path.clone());
        app.delete_overlay_profile_for_test("audit-stuck-delete");

        let rendered = rendered_profile_manager(&mut app);
        assert!(
            rendered.contains("The global default is now System Default, but profile")
                && !rendered.contains("Deleted profile audit-stuck-delete"),
            "an undeletable profile must be reported: {rendered:?}"
        );
        let config = active_config_lines(&config_path);
        assert!(
            !config.contains("audit-stuck-delete"),
            "the global default must be cleared before the file removal: {config:?}"
        );
        assert_eq!(
            app.workspace_set().active_workspace_launch_profile(),
            Some("audit-stuck-delete"),
            "bindings to a profile that still exists must be kept"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn cwd_updates_are_captured_without_changing_the_shape_fingerprint() {
    let terminal = Arc::new(Mutex::new(Terminal::new(80, 24)));
    let sessions = WorkspaceSet::new(
        Session::new_headless(
            SessionToken(0),
            Arc::clone(&terminal),
            crate::native::test_support::headless_writer(),
            Arc::new(HeadlessSession::new(Dimensions::new(80, 24))),
        ),
        None,
    );
    let before = sessions.structural_fingerprint();
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b]7;file:///synthetic/cwd-a\x07");
    let cwd = terminal
        .lock()
        .expect("terminal")
        .current_working_directory()
        .map(str::to_owned);
    let snapshot = sessions.capture_shape();
    let captured_cwd = match &snapshot.workspaces[0].tabs[0].layout {
        crate::native::persistence::PaneShape::Leaf { cwd, .. } => cwd.as_deref(),
        crate::native::persistence::PaneShape::Split { .. } => {
            panic!("single-pane fixture captured a split")
        }
    };

    assert_eq!(cwd.as_deref(), Some("/synthetic/cwd-a"));
    assert_eq!(captured_cwd, cwd.as_deref());
    assert_eq!(
        sessions.structural_fingerprint(),
        before,
        "cwd-only changes are currently excluded from shape autosave detection"
    );
}

#[test]
fn cwd_checkpoint_waits_for_settle_and_respects_minimum_write_spacing() {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let mut app = primary_autosave_app();
    app.run_shape_autosave_for_test(start);
    assert_eq!(
        app.autosave_saves_for_test(),
        0,
        "first pass establishes baseline"
    );
    assert_eq!(app.cwd_checkpoint_deadline_for_test(), None);

    advance_cwd(&mut app, "/synthetic/cwd-initial");
    let first_change = start + Duration::from_secs(1);
    app.run_shape_autosave_for_test(first_change);
    let first_deadline = first_change + Duration::from_secs(5);
    assert_eq!(app.cwd_checkpoint_deadline_for_test(), Some(first_deadline));
    app.run_shape_autosave_for_test(first_deadline - Duration::from_nanos(1));
    assert_eq!(
        app.autosave_saves_for_test(),
        0,
        "no write before the settle deadline"
    );
    app.run_shape_autosave_for_test(first_deadline);
    assert_eq!(
        app.autosave_saves_for_test(),
        1,
        "settled cwd is checkpointed once"
    );
    assert_eq!(app.cwd_checkpoint_deadline_for_test(), None);

    let next_deadline = first_deadline + Duration::from_secs(60);
    for index in 1..=20 {
        advance_cwd(&mut app, &format!("/synthetic/cwd-{index}"));
        app.run_shape_autosave_for_test(first_deadline + Duration::from_secs(index));
    }
    assert_eq!(app.cwd_checkpoint_deadline_for_test(), Some(next_deadline));
    app.run_shape_autosave_for_test(next_deadline - Duration::from_nanos(1));
    assert_eq!(
        app.autosave_saves_for_test(),
        1,
        "cwd bursts respect the 60s budget"
    );
    app.run_shape_autosave_for_test(next_deadline);
    assert_eq!(
        app.autosave_saves_for_test(),
        2,
        "one write occurs at the budget edge"
    );

    app.run_shape_autosave_for_test(next_deadline + Duration::from_secs(1));
    assert_eq!(app.cwd_checkpoint_deadline_for_test(), None);
    assert_eq!(
        app.autosave_saves_for_test(),
        2,
        "unchanged cwd does not re-arm"
    );
}

#[test]
fn nonprimary_never_arms_or_writes_a_cwd_checkpoint() {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let (mut app, _) = headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    );
    app.set_primary_instance_for_test(false);
    app.run_shape_autosave_for_test(start);
    advance_cwd(&mut app, "/synthetic/secondary-cwd");
    app.run_shape_autosave_for_test(start + Duration::from_secs(10));
    app.run_shape_autosave_for_test(start + Duration::from_secs(120));

    assert_eq!(app.autosave_saves_for_test(), 0);
    assert_eq!(app.cwd_checkpoint_deadline_for_test(), None);
}

#[test]
fn structural_write_satisfies_pending_cwd_checkpoint() {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let mut app = primary_autosave_app();
    app.run_shape_autosave_for_test(start);
    advance_cwd(&mut app, "/synthetic/cwd-with-structure");
    app.rename_workspace_for_test(0, "synthetic-renamed-workspace");
    let changed = start + Duration::from_secs(1);
    app.run_shape_autosave_for_test(changed);
    assert_eq!(
        app.cwd_checkpoint_deadline_for_test(),
        Some(changed + Duration::from_secs(5))
    );

    app.run_shape_autosave_for_test(changed + Duration::from_millis(1500));
    assert_eq!(
        app.autosave_saves_for_test(),
        1,
        "structural mutation saves once"
    );
    assert_eq!(app.cwd_checkpoint_deadline_for_test(), None);
    app.run_shape_autosave_for_test(changed + Duration::from_secs(10));
    assert_eq!(
        app.autosave_saves_for_test(),
        1,
        "cwd state was included in the structural write"
    );
}

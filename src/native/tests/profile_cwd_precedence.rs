// SPDX-License-Identifier: GPL-3.0-only
//! v0.14 Phase A3 regression: profile starting-directory (working_directory)
//! precedence across the final native launch routes.
//!
//! This module drives the real route resolvers headlessly against an on-disk
//! profile fixture and pins the five contract rules. The inherited pane cwd is
//! a fallback in `resolve_local_tab_launch`, while a restored/captured cwd
//! travels through the dedicated `resolve_for_restored_local_leaf` route so it
//! outranks the profile without masquerading as a CLI override:
//!
//!   1. Explicit UI/workspace/connection-manager selection: a profile that
//!      carries its own working_directory uses that profile cwd over an
//!      inherited pane cwd.
//!   2. A profile without a working_directory inherits the pane cwd.
//!   3. A CLI `--working-directory` override wins over the profile cwd.
//!   4. A restored/captured cwd wins over the profile cwd.
//!   5. A missing profile fails closed (warning plus global fallback), never
//!      manufacturing a launch context.
//!
//! Fixtures are synthetic and public-safe. The config base is redirected to a
//! per-process temp directory so no real profile store is read or written.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::native::app::profile_launch::{
    resolve_for_new_local_tab, resolve_for_restored_local_leaf, resolve_startup_launch,
};
use crate::native::options::NativeOptions;
use crate::profiles::{LaunchProfile, profiles_dir_path, write_profile_file};
use crate::settings::Settings;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_config_base(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "odytty-a3-cwd-{label}-{}-{nanos}",
        std::process::id()
    ))
}

/// Redirect the config base on every platform at once. Windows resolves the
/// profiles directory from `APPDATA`, Unix from `XDG_CONFIG_HOME` (then `HOME`);
/// setting all three to the same base makes the fixture portable, and the test
/// body reads the real `profiles_dir_path()` so it writes wherever production
/// will look.
fn with_config_base<R>(base: &Path, f: impl FnOnce() -> R) -> R {
    // Poison-tolerant: one failing assertion must not cascade-fail every later
    // case in this single-threaded module just because the guard was held on a
    // panic. The lock only serializes the process-global env mutation.
    let _guard = env_lock().lock().unwrap_or_else(PoisonError::into_inner);
    let prev_appdata = std::env::var_os("APPDATA");
    let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let prev_home = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("APPDATA", base);
        std::env::set_var("XDG_CONFIG_HOME", base);
        std::env::set_var("HOME", base);
    }
    let result = f();
    unsafe {
        restore("APPDATA", prev_appdata);
        restore("XDG_CONFIG_HOME", prev_xdg);
        restore("HOME", prev_home);
    }
    result
}

unsafe fn restore(key: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
}

/// Write a synthetic named profile whose only launch field is an optional
/// working_directory. Returns nothing; the catalog is loaded from disk by the
/// route under test.
fn write_launch_profile(name: &str, working_directory: Option<&str>) {
    let dir = profiles_dir_path().expect("profiles dir resolvable under the redirected base");
    fs::create_dir_all(&dir).expect("create profiles dir");
    let mut profile = LaunchProfile::new(name).expect("profile");
    profile.launch.working_directory = working_directory.map(str::to_owned);
    write_profile_file(&dir.join(format!("{name}.profile.json")), &profile).expect("write profile");
}

// ---- Requirement 1: profile cwd over inherited pane cwd ---------------------
//
// `resolve_local_tab_launch` treats `inherited_cwd` (the focused pane's OSC 7
// directory) as a fallback: it is applied only when the precedence resolver
// left no working_directory. An explicitly selected profile that carries its
// own working_directory therefore wins over the inherited pane cwd.
#[test]
fn route_profile_working_directory_wins_over_inherited_pane_cwd() {
    let base = temp_config_base("req1");
    with_config_base(&base, || {
        write_launch_profile("work", Some("/work/project"));
        let inherited = PathBuf::from("/inherited/pane/cwd");
        let effective =
            resolve_for_new_local_tab(&Settings::default(), None, Some(inherited), Some("work"));
        assert_eq!(
            effective.working_directory,
            Some(PathBuf::from("/work/project")),
            "an explicitly selected profile's working_directory must win over the inherited pane cwd"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- Requirement 2: profile without cwd inherits the pane cwd ---------------
#[test]
fn route_profile_without_working_directory_inherits_pane_cwd() {
    let base = temp_config_base("req2");
    with_config_base(&base, || {
        write_launch_profile("plain", None);
        let inherited = PathBuf::from("/inherited/pane/cwd");
        let effective = resolve_for_new_local_tab(
            &Settings::default(),
            None,
            Some(inherited.clone()),
            Some("plain"),
        );
        assert_eq!(
            effective.working_directory,
            Some(inherited),
            "a profile with no working_directory must inherit the pane cwd"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- Requirement 3: CLI --working-directory wins over the profile cwd -------
//
// Startup route (`resolve_startup_launch`): a CLI `--working-directory` sits in
// the CLI precedence layer, above the named-profile layer.
#[test]
fn startup_route_cli_working_directory_wins_over_profile() {
    let base = temp_config_base("req3");
    with_config_base(&base, || {
        write_launch_profile("work", Some("/from/profile"));
        let mut options = NativeOptions::from_settings(&Settings::default());
        options.profile_name = Some("work".to_owned());
        options.working_directory = Some(PathBuf::from("/from/cli"));

        let (_settings, plan, warnings) = resolve_startup_launch(&options, Settings::default());
        let plan = plan.expect("an explicit profile selection yields a launch plan");
        assert_eq!(
            plan.working_directory,
            Some(PathBuf::from("/from/cli")),
            "a CLI --working-directory override must win over the profile working_directory"
        );
        assert!(
            warnings.is_empty(),
            "a present profile plus a CLI cwd produces no resolver warnings, got {warnings:?}"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- Requirement 4: restored/captured cwd wins over the profile cwd ---------
//
// The restoration route (`spawn_restored_local_leaf` -> `resolve_for_restored_
// local_leaf`) carries the captured leaf cwd through `RestoredLaunchOverrides`,
// a dedicated precedence layer that outranks the named profile without
// masquerading as a CLI override. A restored session therefore reopens in its
// captured directory, ahead of the profile's configured starting directory.
#[test]
fn route_restored_captured_cwd_wins_over_profile() {
    let base = temp_config_base("req4");
    with_config_base(&base, || {
        write_launch_profile("work", Some("/work/project"));
        let restored = PathBuf::from("/restored/captured/cwd");
        let effective =
            resolve_for_restored_local_leaf(&Settings::default(), "work", Some(restored.clone()));
        assert_eq!(
            effective.working_directory,
            Some(restored),
            "a restored/captured cwd must win over the profile working_directory"
        );
        assert_eq!(
            effective.profile_name.as_deref(),
            Some("work"),
            "the restored leaf still resolves its named profile for the rest of the context"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- Requirement 5: missing profile fails closed ---------------------------
#[test]
fn route_missing_profile_fails_closed_with_warning() {
    let base = temp_config_base("req5");
    with_config_base(&base, || {
        // No fixture on disk: the named profile does not exist.
        let effective = resolve_for_new_local_tab(
            &Settings::default(),
            None,
            None,
            Some("ghost-profile-does-not-exist"),
        );
        assert!(
            effective
                .warnings
                .iter()
                .any(|warning| warning.contains("missing")),
            "a missing profile must warn, got {:?}",
            effective.warnings
        );
        assert_eq!(
            effective.settings.theme,
            Settings::default().theme,
            "a missing profile must fall back to global settings, not manufacture a context"
        );
        assert!(
            effective.profile_name.as_deref() == Some("ghost-profile-does-not-exist"),
            "the requested name is retained for disclosure even when it fails to resolve"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

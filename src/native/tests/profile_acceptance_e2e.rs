// SPDX-License-Identifier: GPL-3.0-only
//! Acceptance-round e2e profile behavior: env/theme application, missing cwd
//! fallback, default delete/rename, import future-key/password refusal,
//! malformed catalog recovery, and restore of launch_profile.
//!
//! Drives a real `App` with an `EventLoop` proxy so `handle_new_tab_with_profile`
//! can spawn real PTY children. Fixtures redirect the config base so no user
//! profile store is touched. macOS skips proxy-backed cases (off-main-thread
//! EventLoop forbidden by AppKit).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::native::persistence::PaneShape;
use crate::native::session::{Session, SessionToken, WorkspaceSet};
use crate::profiles::{
    LaunchProfile, export_profile_file, load_catalog_from_dir, profiles_dir_path,
    read_profile_file, write_profile_file,
};
use crate::settings::Settings;
use crate::theme::Theme;

use super::*;

fn temp_config_base(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "odytty-accept-e2e-{label}-{}-{nanos}",
        std::process::id()
    ))
}

fn with_config_base<R>(base: &Path, f: impl FnOnce() -> R) -> R {
    let _guard = crate::test_lock::test_env_lock();
    let _count_guard = crate::test_lock::catalog_count_lock();
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
        restore_env("APPDATA", prev_appdata);
        restore_env("XDG_CONFIG_HOME", prev_xdg);
        restore_env("HOME", prev_home);
    }
    result
}

unsafe fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
}

fn profiles_dir() -> PathBuf {
    let dir = profiles_dir_path().expect("profiles dir under redirected base");
    fs::create_dir_all(&dir).expect("create profiles dir");
    dir
}

fn write_env_profile(name: &str, env: &[(&str, &str)], shell: Option<&str>) {
    let dir = profiles_dir();
    let mut profile = LaunchProfile::new(name).expect("profile");
    profile.launch.env = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect::<BTreeMap<_, _>>();
    profile.launch.shell = shell.map(str::to_owned);
    write_profile_file(&dir.join(format!("{name}.profile.json")), &profile).expect("write");
}

fn write_theme_profile(name: &str, theme: &str) {
    let dir = profiles_dir();
    let mut profile = LaunchProfile::new(name).expect("profile");
    profile.appearance.theme = Some(theme.to_owned());
    write_profile_file(&dir.join(format!("{name}.profile.json")), &profile).expect("write");
}

fn write_cwd_profile(name: &str, cwd: &str) {
    let dir = profiles_dir();
    let mut profile = LaunchProfile::new(name).expect("profile");
    profile.launch.working_directory = Some(cwd.to_owned());
    write_profile_file(&dir.join(format!("{name}.profile.json")), &profile).expect("write");
}

fn app_with_proxy() -> Result<App, &'static str> {
    let dims = Dimensions::new(80, 24);
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let headless = Arc::new(crate::native::session::HeadlessSession::new(dims));
    let proxy = event_loop_proxy_for_test()?;
    let sessions = WorkspaceSet::new(
        Session::new_headless(SessionToken(0), terminal, writer, headless),
        Some(proxy),
    );
    let app = App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Ok(app)
}

macro_rules! app_or_skip {
    () => {{
        match app_with_proxy() {
            Ok(app) => app,
            Err(_) => return,
        }
    }};
}

fn wait_for_plain_text(app: &App, session: usize, needle: &str, budget: Duration) -> String {
    let deadline = Instant::now() + budget;
    loop {
        let text = app.session_plain_text_for_test(session).unwrap_or_default();
        if text.contains(needle) || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn rgb_tuple(theme_bg: (u8, u8, u8)) -> crate::core::RgbColor {
    crate::core::RgbColor {
        red: theme_bg.0,
        green: theme_bg.1,
        blue: theme_bg.2,
    }
}

fn collect_leaf_profiles(shape: &PaneShape, out: &mut Vec<Option<String>>) {
    match shape {
        PaneShape::Leaf { launch_profile, .. } => out.push(launch_profile.clone()),
        PaneShape::Split { first, second, .. } => {
            collect_leaf_profiles(first, out);
            collect_leaf_profiles(second, out);
        }
    }
}

// ---- (a) env without shell: DefaultShell must still apply overrides ---------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn profile_env_applies_without_shell_or_command() {
    let base = temp_config_base("env-default");
    with_config_base(&base, || {
        write_env_profile("alpha-env", &[("ODY_TEST", "alpha")], None);
        let mut app = app_or_skip!();
        let before = app.active_workspace_tab_count_for_test();
        app.new_tab_with_profile_for_test("alpha-env");
        assert_eq!(
            app.active_workspace_tab_count_for_test(),
            before + 1,
            "profile tab must open"
        );
        app.write_active_session_for_test(b"echo $ODY_TEST\n");
        let session = app.active_workspace_tab_count_for_test() - 1;
        let text = wait_for_plain_text(&app, session, "alpha", Duration::from_secs(3));
        assert!(
            text.contains("alpha"),
            "DefaultShell spawn must apply profile env; screen={text:?}"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (b) env with explicit shell: pin the working path ----------------------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn profile_env_applies_with_explicit_shell() {
    let base = temp_config_base("env-shell");
    with_config_base(&base, || {
        write_env_profile("alpha-sh", &[("ODY_TEST", "alpha")], Some("/bin/sh"));
        let mut app = app_or_skip!();
        let before = app.active_workspace_tab_count_for_test();
        app.new_tab_with_profile_for_test("alpha-sh");
        assert_eq!(app.active_workspace_tab_count_for_test(), before + 1);
        app.write_active_session_for_test(b"echo $ODY_TEST\n");
        let session = app.active_workspace_tab_count_for_test() - 1;
        let text = wait_for_plain_text(&app, session, "alpha", Duration::from_secs(3));
        assert!(
            text.contains("alpha"),
            "explicit shell spawn must apply profile env; screen={text:?}"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (c) profile theme survives model-state sweep and tab switches ----------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn profile_theme_stays_on_session_across_model_state_and_tab_switch() {
    let base = temp_config_base("theme");
    with_config_base(&base, || {
        write_theme_profile("drac", "dracula");
        let expected = Theme::from_name("dracula").expect("dracula builtin");
        let expected_bg = rgb_tuple(expected.background);

        let mut app = app_or_skip!();
        let plain_idx = 0usize;
        app.new_tab_with_profile_for_test("drac");
        let drac_idx = app.active_workspace_tab_count_for_test() - 1;

        let (_, bg) = app
            .session_dynamic_colors_for_test(drac_idx)
            .expect("dracula session colors");
        assert_eq!(
            bg, expected_bg,
            "spawned profile tab must seed dracula background"
        );
        assert_eq!(
            app.active_profile_theme_for_test()
                .expect("profile theme stamp")
                .background,
            expected.background,
            "active session must carry authored dracula profile_theme"
        );
        assert_eq!(
            app.chrome_theme_for_test().background,
            expected.background,
            "chrome must present the profile theme while the profile tab is active"
        );

        app.apply_model_state_to_all_sessions_for_test();
        let (_, bg_after) = app
            .session_dynamic_colors_for_test(drac_idx)
            .expect("colors after sweep");
        assert_eq!(
            bg_after, expected_bg,
            "model-state sweep must not overwrite a profile session theme"
        );
        assert_eq!(
            app.chrome_theme_for_test().background,
            expected.background,
            "chrome must still present dracula after the model-state sweep"
        );

        app.switch_to_session_for_test(plain_idx);
        let app_theme = app.effective_theme_for_test();
        assert_ne!(
            rgb_tuple(app_theme.background),
            expected_bg,
            "app effective theme stays global while a plain tab is focused"
        );
        assert_eq!(
            app.chrome_theme_for_test().background,
            app_theme.background,
            "plain tab chrome must follow the global effective theme"
        );

        app.switch_to_session_for_test(drac_idx);
        let (_, bg_back) = app
            .session_dynamic_colors_for_test(drac_idx)
            .expect("colors after switch back");
        assert_eq!(
            bg_back, expected_bg,
            "returning to the profile tab must still show dracula"
        );
        assert_eq!(
            app.chrome_theme_for_test().background,
            expected.background,
            "chrome must re-present dracula when switching back to the profile tab"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (d) missing working_directory falls back with a notice -----------------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn missing_profile_cwd_opens_tab_with_warning_not_hard_failure() {
    let base = temp_config_base("cwd-miss");
    with_config_base(&base, || {
        let missing = base.join("no-such-workdir-odytty");
        write_cwd_profile("badcwd", &missing.to_string_lossy());
        let mut app = app_or_skip!();
        let before = app.active_workspace_tab_count_for_test();
        app.new_tab_with_profile_for_test("badcwd");
        assert_eq!(
            app.active_workspace_tab_count_for_test(),
            before + 1,
            "missing cwd must still open a tab"
        );
        let notice = app.open_notice_message_for_test().unwrap_or_default();
        assert!(
            !notice.contains("Could not open a new tab"),
            "must not hard-fail with spawn notice; got {notice:?}"
        );
        assert!(
            !notice.is_empty(),
            "missing cwd must raise a bounded warning notice"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (e) deleting the global default clears the setting ---------------------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn deleting_global_default_profile_clears_setting_and_new_tab_is_plain() {
    let base = temp_config_base("del-default");
    with_config_base(&base, || {
        write_env_profile("doomed", &[("ODY_TEST", "x")], None);
        let mut app = app_or_skip!();
        app.set_global_default_launch_profile_for_test("doomed");
        assert_eq!(
            Settings::from_env().default_launch_profile.as_deref(),
            Some("doomed"),
            "Set as Default must persist default_launch_profile"
        );
        app.delete_overlay_profile_for_test("doomed");
        let reloaded = Settings::from_env();
        assert_eq!(
            reloaded.default_launch_profile, None,
            "deleting the default profile must clear default_launch_profile"
        );
        let before = app.active_workspace_tab_count_for_test();
        app.new_tab_for_test();
        assert_eq!(app.active_workspace_tab_count_for_test(), before + 1);
        assert!(
            app.open_notice_message_for_test().is_none(),
            "plain New Tab after deleting the default must raise no warning"
        );
        assert_eq!(
            app.active_launch_profile_for_test(),
            None,
            "new tab must be unbound from a deleted default"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (f) renaming the default profile updates the setting key ---------------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn renaming_global_default_profile_updates_the_setting_key() {
    let base = temp_config_base("rename-default");
    with_config_base(&base, || {
        write_env_profile("oldname", &[("ODY_TEST", "x")], None);
        let mut app = app_or_skip!();
        app.set_global_default_launch_profile_for_test("oldname");
        let mut renamed = LaunchProfile::new("newname").expect("name");
        renamed
            .launch
            .env
            .insert("ODY_TEST".to_owned(), "x".to_owned());
        app.save_overlay_profile_for_test(renamed, Some("oldname".to_owned()));
        let reloaded = Settings::from_env();
        assert_eq!(
            reloaded.default_launch_profile.as_deref(),
            Some("newname"),
            "renaming the default profile must retarget default_launch_profile"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (g) import round-trip: future_key survives; password refused -----------

#[test]
fn import_round_trip_keeps_future_key_and_refuses_password_env() {
    let base = temp_config_base("import");
    with_config_base(&base, || {
        let dir = profiles_dir();
        let mut alpha = LaunchProfile::new("alpha").expect("alpha");
        alpha.launch.env.insert("SAFE".to_owned(), "one".to_owned());
        let alpha_path = dir.join("alpha.profile.json");
        write_profile_file(&alpha_path, &alpha).expect("write alpha");

        let exported = dir.join("alpha-export.profile.json");
        export_profile_file(&exported, &alpha).expect("export");
        let mut raw = fs::read_to_string(&exported).expect("read export");
        // Inject a top-level future key inside the object.
        let insert_at = raw.rfind('}').expect("object close");
        raw.insert_str(insert_at, ",\"future_key\":true");
        let gamma_src = dir.join("gamma-src.profile.json");
        fs::write(&gamma_src, &raw).expect("write gamma src");

        let mut gamma = read_profile_file(&gamma_src, None).expect("import parse");
        gamma.name = "gamma".to_owned();
        assert!(
            gamma.preserved.contains_key("future_key"),
            "imported document must retain future_key"
        );
        let gamma_path = dir.join("gamma.profile.json");
        write_profile_file(&gamma_path, &gamma).expect("save gamma");
        // Edit+save again (add a harmless display_name) and re-check.
        let mut edited = read_profile_file(&gamma_path, Some("gamma")).expect("reload");
        edited.display_name = Some("Gamma".to_owned());
        write_profile_file(&gamma_path, &edited).expect("re-save");
        let again = read_profile_file(&gamma_path, Some("gamma")).expect("reload after edit");
        assert!(
            again.preserved.contains_key("future_key"),
            "edit+save must keep future_key"
        );
        let bytes = fs::read_to_string(&gamma_path).expect("bytes");
        assert!(
            bytes.contains("future_key"),
            "serialized file must still carry future_key"
        );

        // Password env must refuse with no file written.
        let bad_path = dir.join("secret.profile.json");
        let mut secret = LaunchProfile::new("secret").expect("secret");
        secret
            .launch
            .env
            .insert("password".to_owned(), "nope".to_owned());
        let err = write_profile_file(&bad_path, &secret).expect_err("password env refused");
        assert!(
            !bad_path.exists(),
            "refused password profile must not create a file; err={err}"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (h) malformed profile at startup: others load, bytes unchanged ---------

#[test]
fn malformed_profile_is_listed_with_reason_and_bytes_unchanged() {
    let base = temp_config_base("malformed");
    with_config_base(&base, || {
        let dir = profiles_dir();
        write_env_profile("good", &[("SAFE", "1")], None);
        let bad_path = dir.join("broken.profile.json");
        let truncated = r#"{"schema_version":1,"name":"broken","launch":{"#;
        fs::write(&bad_path, truncated).expect("write truncated");
        let before = fs::read(&bad_path).expect("before bytes");

        let catalog = load_catalog_from_dir(&dir);
        assert!(
            catalog.profiles.contains_key("good"),
            "good profiles must still load"
        );
        assert!(
            !catalog.profiles.contains_key("broken"),
            "malformed profile must not enter the catalog as a valid entry"
        );
        assert!(
            catalog
                .warnings
                .iter()
                .any(|w| w.contains("broken") || w.contains("malformed") || w.contains("parse")),
            "catalog must list a reason for the bad file; warnings={:?}",
            catalog.warnings
        );
        let after = fs::read(&bad_path).expect("after bytes");
        assert_eq!(before, after, "malformed file bytes must stay unchanged");
    });
    let _ = fs::remove_dir_all(&base);
}

// ---- (i) restore preserves launch_profile on profile tabs -------------------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn restore_keeps_launch_profile_on_profile_tabs() {
    let base = temp_config_base("restore");
    with_config_base(&base, || {
        write_env_profile("alpha", &[("ODY_TEST", "alpha")], Some("/bin/sh"));
        let mut app = app_or_skip!();
        app.new_tab_with_profile_for_test("alpha");
        app.new_tab_with_profile_for_test("alpha");
        // One plain tab already exists (headless seed). Snapshot the shape.
        let snapshot = app.capture_shape_for_test();
        let mut leaves = Vec::new();
        for ws in &snapshot.workspaces {
            for tab in &ws.tabs {
                collect_leaf_profiles(&tab.layout, &mut leaves);
            }
        }
        let alpha_count = leaves
            .iter()
            .filter(|p| p.as_deref() == Some("alpha"))
            .count();
        assert_eq!(
            alpha_count, 2,
            "snapshot must record two alpha launch_profile leaves; leaves={leaves:?}"
        );
        assert!(
            leaves.iter().any(|p| p.is_none()),
            "plain leaf must remain unbound; leaves={leaves:?}"
        );

        // Append through the production profile-aware restore path (not the
        // headless theme-seed seam, which intentionally ignores launch_profile).
        let mut restored = app_or_skip!();
        let report = restored.append_snapshot_with_profile_restore_for_test(&snapshot);
        assert!(
            matches!(
                report,
                crate::native::session::RestoreReport::Restored { .. }
            ),
            "append must restore; got {report:?}"
        );
        let stamped: Vec<_> = (0..restored.session_count_for_test())
            .filter_map(|idx| {
                restored.switch_to_session_for_test(idx);
                restored.active_launch_profile_for_test()
            })
            .collect();
        assert_eq!(
            stamped
                .iter()
                .filter(|name| name.as_str() == "alpha")
                .count(),
            2,
            "restored alpha tabs must keep launch_profile; stamped={stamped:?}"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

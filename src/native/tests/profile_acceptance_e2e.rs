// SPDX-License-Identifier: GPL-3.0-only
//! Acceptance-round e2e profile behavior: env/theme application, missing cwd
//! fallback, default delete/rename, import future-key/password refusal,
//! malformed catalog recovery, and restore of launch_profile.
//!
//! Drives a real `App`, with an `EventLoop` proxy where a case needs
//! `handle_new_tab_with_profile` to spawn real PTY children. Fixtures redirect
//! the config base so no user profile store is touched. macOS ignores the
//! proxy-backed cases (AppKit forbids an off-main-thread EventLoop).

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
    super::config_env::with_config_base(base, true, f)
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
    Ok(app_over(Some(event_loop_proxy_for_test()?)))
}

/// An App whose workspace set has no event loop proxy, for cases that never
/// spawn a session. They need no winit loop, so they also run on macOS and on
/// hosts without a display.
fn headless_app() -> App {
    app_over(None)
}

fn app_over(
    proxy: Option<winit::event_loop::EventLoopProxy<crate::native::pty::UserEvent>>,
) -> App {
    let dims = Dimensions::new(80, 24);
    let writer: PtyWriter = crate::native::test_support::headless_writer();
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let headless = Arc::new(crate::native::session::HeadlessSession::new(dims));
    let sessions = WorkspaceSet::new(
        Session::new_headless(SessionToken(0), terminal, writer, headless),
        proxy,
    );
    App::new_with_sessions(
        NativeOptions::default(),
        sessions,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    )
}

macro_rules! app_or_skip {
    () => {{
        match app_with_proxy() {
            Ok(app) => app,
            Err(_) => return,
        }
    }};
}

/// An explicit profile shell present on every supported CI runner. Windows has
/// no `/bin/sh`; a profile naming it fails to spawn and opens no tab.
#[cfg(unix)]
const EXPLICIT_SHELL: &str = "/bin/sh";
#[cfg(windows)]
const EXPLICIT_SHELL: &str = "cmd.exe";

/// The line that prints `ODY_TEST` in [`EXPLICIT_SHELL`], ending in the byte
/// the shell reads as Enter (ConPTY takes a carriage return).
#[cfg(unix)]
const ECHO_IN_EXPLICIT_SHELL: &[u8] = b"echo $ODY_TEST\n";
#[cfg(windows)]
const ECHO_IN_EXPLICIT_SHELL: &[u8] = b"echo %ODY_TEST%\r";

/// The line that prints `ODY_TEST` in the default shell: a POSIX-family login
/// shell on Unix, PowerShell on Windows, where `$ODY_TEST` would name an unset
/// PowerShell variable rather than the environment entry.
#[cfg(unix)]
const ECHO_IN_DEFAULT_SHELL: &[u8] = b"echo $ODY_TEST\n";
#[cfg(windows)]
const ECHO_IN_DEFAULT_SHELL: &[u8] = b"echo $env:ODY_TEST\r";

/// How long a spawned shell may take to print its prompt and then the echoed
/// value. PowerShell on a CI runner starts far slower than a POSIX shell. The
/// waits return as soon as the text appears.
#[cfg(unix)]
const SHELL_BUDGET: Duration = Duration::from_secs(3);
#[cfg(windows)]
const SHELL_BUDGET: Duration = Duration::from_secs(20);

/// An App with an event-loop proxy whose launch geometry is already settled,
/// as it is in a running window by the time a profile tab can be opened. A
/// window that has not drawn its first grid holds new Windows shells (see
/// `crate::pty::spawn_held`), and this fixture never draws one.
fn settled_app_with_proxy() -> Result<App, &'static str> {
    let mut app = app_with_proxy()?;
    app.settle_launch_geometry_for_test();
    Ok(app)
}

/// Wait until the shell in `session` has drawn something (its prompt), so
/// typed input is not raced against shell startup. Returns either way at the
/// deadline; the caller's own assertion decides the outcome.
fn wait_for_shell_output(app: &App, session: usize, budget: Duration) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let text = app
            .tab_plain_text_at_position_for_test(session)
            .unwrap_or_default();
        if !text.trim().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_plain_text(app: &App, session: usize, needle: &str, budget: Duration) -> String {
    let deadline = Instant::now() + budget;
    loop {
        let text = app
            .tab_plain_text_at_position_for_test(session)
            .unwrap_or_default();
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
        let Ok(mut app) = settled_app_with_proxy() else {
            return;
        };
        let before = app.active_workspace_tab_count_for_test();
        app.new_tab_with_profile_for_test("alpha-env");
        assert_eq!(
            app.active_workspace_tab_count_for_test(),
            before + 1,
            "profile tab must open"
        );
        let session = app.active_workspace_tab_count_for_test() - 1;
        wait_for_shell_output(&app, session, SHELL_BUDGET);
        app.write_active_session_for_test(ECHO_IN_DEFAULT_SHELL);
        let text = wait_for_plain_text(&app, session, "alpha", SHELL_BUDGET);
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
        write_env_profile("alpha-sh", &[("ODY_TEST", "alpha")], Some(EXPLICIT_SHELL));
        let Ok(mut app) = settled_app_with_proxy() else {
            return;
        };
        let before = app.active_workspace_tab_count_for_test();
        app.new_tab_with_profile_for_test("alpha-sh");
        assert_eq!(app.active_workspace_tab_count_for_test(), before + 1);
        let session = app.active_workspace_tab_count_for_test() - 1;
        wait_for_shell_output(&app, session, SHELL_BUDGET);
        app.write_active_session_for_test(ECHO_IN_EXPLICIT_SHELL);
        let text = wait_for_plain_text(&app, session, "alpha", SHELL_BUDGET);
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
        let _render_globals = crate::test_lock::render_globals_lock();
        write_theme_profile("drac", "dracula");
        let expected = Theme::from_name("dracula").expect("dracula builtin");
        let expected_bg = rgb_tuple(expected.background);

        let mut app = app_or_skip!();
        let plain_idx = 0usize;
        app.new_tab_with_profile_for_test("drac");
        let drac_idx = app.active_workspace_tab_count_for_test() - 1;

        let (_, bg) = app
            .tab_dynamic_colors_at_position_for_test(drac_idx)
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
            .tab_dynamic_colors_at_position_for_test(drac_idx)
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
            .tab_dynamic_colors_at_position_for_test(drac_idx)
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

// ---- (c2) the GLOBAL DEFAULT profile theme applies on plain New Tab ---------

#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn global_default_profile_theme_applies_on_plain_new_tab() {
    let base = temp_config_base("default-theme");
    with_config_base(&base, || {
        let _render_globals = crate::test_lock::render_globals_lock();
        write_theme_profile("spot", "dracula");
        let expected = Theme::from_name("dracula").expect("dracula builtin");
        let expected_bg = rgb_tuple(expected.background);

        let mut app = app_or_skip!();
        app.set_global_default_launch_profile_for_test("spot");

        // Plain "+" / New Tab with no explicit profile must resolve the saved
        // global default and present its authored theme.
        app.new_tab_for_test();
        let spot_idx = app.active_workspace_tab_count_for_test() - 1;

        assert_eq!(
            app.active_launch_profile_for_test().as_deref(),
            Some("spot"),
            "plain New Tab must bind to the global default profile"
        );
        let (_, bg) = app
            .tab_dynamic_colors_at_position_for_test(spot_idx)
            .expect("default profile session colors");
        assert_eq!(
            bg, expected_bg,
            "plain New Tab must seed the global default profile theme"
        );
        assert_eq!(
            app.chrome_theme_for_test().background,
            expected.background,
            "chrome must present the global default profile theme on plain New Tab"
        );
        // The window chrome (tab bar) must follow the active profile theme, not
        // only the terminal cells: the reported defect was a dracula terminal
        // inside an odyssey-default tab strip.
        assert_eq!(
            app.tab_bar_background_for_test(),
            expected.background,
            "tab bar must paint the default profile theme, not the global theme"
        );
        assert_ne!(
            expected.background,
            app.effective_theme_for_test().background,
            "fixture sanity: profile theme differs from the global effective theme"
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

/// The workspace routes apply the same missing-directory fallback as New Tab:
/// an explicit profile, the global default profile, and a missing directory
/// each open a workspace with a bounded notice instead of a spawn failure.
#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn missing_profile_cwd_opens_workspace_with_warning_not_hard_failure() {
    let base = temp_config_base("ws-cwd-miss");
    with_config_base(&base, || {
        let missing = base.join("no-such-workdir-odytty");
        write_cwd_profile("badcwd", &missing.to_string_lossy());
        for default_route in [false, true] {
            let mut app = app_or_skip!();
            let before = app.workspace_count_for_test();
            if default_route {
                app.set_global_default_launch_profile_for_test("badcwd");
                app.dispatch_workspace_action_for_test(
                    crate::settings::BindableAction::NewWorkspace,
                );
            } else {
                app.new_workspace_with_profile_for_test("badcwd");
            }
            assert_eq!(
                app.workspace_count_for_test(),
                before + 1,
                "default route {default_route}: missing cwd must still open a workspace"
            );
            let notice = app.open_notice_message_for_test().unwrap_or_default();
            assert!(
                notice.contains("working directory does not exist"),
                "default route {default_route}: bounded fallback notice; got {notice:?}"
            );
        }
    });
    let _ = fs::remove_dir_all(&base);
}

/// A workspace opened with a themed profile carries the authored theme as
/// session state, exactly as a profile tab does.
#[cfg_attr(
    target_os = "macos",
    ignore = "harness builds an off-main-thread winit EventLoop; unsupported on macOS"
)]
#[test]
fn profile_workspace_records_the_authored_theme() {
    let base = temp_config_base("ws-theme");
    with_config_base(&base, || {
        write_theme_profile("drac", "dracula");
        let expected = Theme::from_name("dracula").expect("dracula builtin");
        let mut app = app_or_skip!();
        app.new_workspace_with_profile_for_test("drac");
        assert_eq!(
            app.active_profile_theme_for_test()
                .map(|theme| theme.background),
            Some(expected.background),
            "the new workspace's pane carries the authored theme"
        );
        assert_eq!(app.chrome_theme_for_test().background, expected.background);
        app.apply_model_state_to_all_sessions_for_test();
        assert_eq!(
            app.chrome_theme_for_test().background,
            expected.background,
            "a later model sweep keeps the profile theme"
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

#[test]
fn renaming_global_default_profile_updates_the_setting_key() {
    let base = temp_config_base("rename-default");
    with_config_base(&base, || {
        write_env_profile("oldname", &[("ODY_TEST", "x")], None);
        let mut app = headless_app();
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
        write_env_profile("alpha", &[("ODY_TEST", "alpha")], Some(EXPLICIT_SHELL));
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
        let focused = restored.active_session_token_for_test();
        let stamped: Vec<_> = restored
            .all_session_tokens_for_test()
            .into_iter()
            .map(|token| {
                restored
                    .pane_launch_profile_for_test(token)
                    .expect("restored pane exists")
            })
            .collect();
        assert_eq!(
            stamped
                .iter()
                .filter(|name| name.as_deref() == Some("alpha"))
                .count(),
            2,
            "restored alpha tabs must keep launch_profile; stamped={stamped:?}"
        );
        assert!(
            stamped.iter().any(Option::is_none),
            "plain restored panes stay unbound"
        );
        assert_eq!(
            restored.active_session_token_for_test(),
            focused,
            "observing profiles never changes focus"
        );
    });
    let _ = fs::remove_dir_all(&base);
}

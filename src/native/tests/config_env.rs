// SPDX-License-Identifier: GPL-3.0-only
//! Panic-safe config and state isolation, with environment-before-catalog lock order.

use std::ffi::OsString;
use std::path::Path;

const KEYS: [&str; 5] = [
    "HOME",
    "XDG_CONFIG_HOME",
    "APPDATA",
    "LOCALAPPDATA",
    "XDG_STATE_HOME",
];

struct ConfigEnvRestore([Option<OsString>; 5]);

impl ConfigEnvRestore {
    /// The caller holds the shared environment lock until this guard drops.
    fn redirect(base: &Path, redirect_xdg: bool) -> Self {
        let restore = Self(KEYS.map(std::env::var_os));
        // SAFETY: callers hold test_env_lock throughout mutation and restoration.
        unsafe {
            std::env::set_var("HOME", base);
            std::env::set_var("APPDATA", base);
            std::env::set_var("LOCALAPPDATA", base);
            std::env::set_var("XDG_STATE_HOME", base);
            if redirect_xdg {
                std::env::set_var("XDG_CONFIG_HOME", base);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }
        restore
    }
}

impl Drop for ConfigEnvRestore {
    fn drop(&mut self) {
        // SAFETY: this guard drops before its caller releases test_env_lock.
        unsafe {
            for (key, value) in KEYS.into_iter().zip(&self.0) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

pub(in crate::native) fn with_config_base<R>(
    base: &Path,
    redirect_xdg: bool,
    f: impl FnOnce() -> R,
) -> R {
    let _env = crate::test_lock::test_env_lock();
    let _catalog = crate::test_lock::catalog_count_lock();
    let _restore = ConfigEnvRestore::redirect(base, redirect_xdg);
    // Resolved under the lock, so a session this thread spawns writes its
    // shell-integration files inside the redirected base, as production does.
    let _spawn_dir = crate::settings::SpawnConfigDirMark::set(
        crate::settings::config_file_path().and_then(|path| path.parent().map(Path::to_path_buf)),
    );
    f()
}

#[test]
fn config_environment_restores_present_and_absent_values_on_unwind() {
    let _env = crate::test_lock::test_env_lock();
    let _catalog = crate::test_lock::catalog_count_lock();
    let _original = ConfigEnvRestore(KEYS.map(std::env::var_os));
    for present in [false, true] {
        // SAFETY: the shared environment lock covers this fixture's lifetime.
        unsafe {
            for key in KEYS {
                if present {
                    std::env::set_var(key, "synthetic-config-base");
                } else {
                    std::env::remove_var(key);
                }
            }
        }
        let expected = KEYS.map(std::env::var_os);
        for redirect_xdg in [false, true] {
            let panic = std::panic::catch_unwind(|| {
                let _restore = ConfigEnvRestore::redirect(
                    Path::new("synthetic-redirected-base"),
                    redirect_xdg,
                );
                assert_eq!(
                    std::env::var_os("HOME"),
                    Some(OsString::from("synthetic-redirected-base"))
                );
                assert_eq!(
                    std::env::var_os("APPDATA"),
                    Some(OsString::from("synthetic-redirected-base"))
                );
                assert_eq!(
                    std::env::var_os("XDG_CONFIG_HOME"),
                    redirect_xdg.then(|| OsString::from("synthetic-redirected-base"))
                );
                panic!("synthetic fixture failure");
            });
            let payload = panic.expect_err("the fixture must reach its deliberate panic");
            assert_eq!(
                payload.downcast_ref::<&str>(),
                Some(&"synthetic fixture failure")
            );
            assert_eq!(KEYS.map(std::env::var_os), expected);
        }
    }
}

#[test]
fn config_fixture_redirects_every_platform_state_and_layout_directory() {
    let keys = [
        "HOME",
        "XDG_CONFIG_HOME",
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_STATE_HOME",
    ];
    let _env = crate::test_lock::test_env_lock();
    let _catalog = crate::test_lock::catalog_count_lock();
    struct Original([Option<OsString>; 5]);
    impl Drop for Original {
        fn drop(&mut self) {
            // SAFETY: the shared environment lock outlives this guard.
            unsafe {
                for (key, value) in [
                    "HOME",
                    "XDG_CONFIG_HOME",
                    "APPDATA",
                    "LOCALAPPDATA",
                    "XDG_STATE_HOME",
                ]
                .into_iter()
                .zip(&self.0)
                {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }
    let _original = Original(keys.map(std::env::var_os));
    let sentinel = Path::new("synthetic-outside-root");
    let base = Path::new("synthetic-owned-root");
    for redirect_xdg in [false, true] {
        // SAFETY: all mutations and restoration share the environment lock.
        unsafe {
            for key in keys {
                std::env::set_var(key, sentinel);
            }
        }
        {
            let _restore = ConfigEnvRestore::redirect(base, redirect_xdg);
            assert!(crate::logging::state_log_dir().starts_with(base));
            assert!(crate::native::persistence::layouts_dir().starts_with(base));
            assert!(crate::native::persistence::snapshot_path().starts_with(base));
            for key in ["HOME", "APPDATA", "LOCALAPPDATA", "XDG_STATE_HOME"] {
                assert_eq!(
                    std::env::var_os(key),
                    Some(base.as_os_str().to_owned()),
                    "{key}"
                );
            }
        }
        for key in keys {
            assert_eq!(
                std::env::var_os(key),
                Some(sentinel.as_os_str().to_owned()),
                "restored {key}"
            );
        }
    }
}

#[test]
fn listing_saved_layouts_does_not_create_state_directories() {
    let root = crate::test_dirs::fresh_temp_dir("odytty-layout-listing-");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let base = root.join("uncreated-state");
    with_config_base(&base, true, || {
        let layouts = crate::native::persistence::layouts_dir();
        assert!(layouts.starts_with(&base));
        assert!(crate::native::persistence::list_layouts().names.is_empty());
        assert!(crate::native::persistence::list_layout_names().is_empty());
        assert!(!base.exists(), "listing must leave the state tree absent");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let (mut app, _) = super::headless_app_with(
            crate::native::options::NativeOptions::default(),
            super::Dimensions::new(80, 24),
            crate::settings::Settings::default(),
        );
        app.drive_char_with_mods_for_test('p', true, true);
        assert!(
            app.overlay_open_for_test(),
            "the palette opens through its key binding"
        );
        app.drive_named_key_for_test(winit::keyboard::NamedKey::Escape);
        assert!(
            !base.exists(),
            "opening the palette must leave state absent"
        );
    });
}

#[cfg(unix)]
#[test]
fn listing_saved_layouts_preserves_existing_directory_and_file_modes() {
    use std::os::unix::fs::PermissionsExt;
    let root = crate::test_dirs::fresh_temp_dir("odytty-layout-listing-modes-");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    with_config_base(&root, true, || {
        let layouts = crate::native::persistence::layouts_dir();
        std::fs::create_dir_all(&layouts).unwrap();
        let file = layouts.join("sample.json");
        std::fs::write(&file, b"{}").unwrap();
        std::fs::set_permissions(&layouts, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(crate::native::persistence::list_layout_names(), ["sample"]);
        assert!(crate::native::persistence::layout_stamp("sample").is_some());
        let (mut app, _) = super::headless_app_with(
            crate::native::options::NativeOptions::default(),
            super::Dimensions::new(80, 24),
            crate::settings::Settings::default(),
        );
        app.drive_char_with_mods_for_test('p', true, true);
        assert!(app.overlay_open_for_test());
        app.drive_named_key_for_test(winit::keyboard::NamedKey::Escape);
        assert_eq!(
            std::fs::metadata(&layouts).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o644
        );
    });
}

#[cfg(unix)]
#[test]
fn layout_discovery_rejects_a_symlinked_state_leaf() {
    use std::os::unix::fs::symlink;
    let root = crate::test_dirs::fresh_temp_dir("odytty-layout-listing-link-");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    with_config_base(&root.join("config"), true, || {
        let state = crate::logging::state_log_dir();
        let target = root.join("link-target");
        std::fs::create_dir_all(target.join("layouts")).unwrap();
        std::fs::write(target.join("layouts/sample.json"), b"{}").unwrap();
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        symlink(&target, &state).unwrap();
        assert!(crate::native::persistence::list_layout_names().is_empty());
        assert!(crate::native::persistence::layout_stamp("sample").is_none());
        assert!(
            std::fs::symlink_metadata(&state)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    });
}

/// The `--rcfile` wrapper path the Unix injector attached to a Bash command.
#[cfg(unix)]
fn injected_bash_rcfile() -> std::path::PathBuf {
    let mut command = crate::pty::CommandBuilder::new("/bin/bash");
    crate::shell_integration::apply_spawn_integration(&mut command);
    let args = command.args_for_test();
    let at = args
        .iter()
        .position(|arg| arg == "--rcfile")
        .expect("Bash integration attaches an rcfile");
    let rcfile = std::path::PathBuf::from(&args[at + 1]);
    assert!(rcfile.is_file(), "the wrapper is written before the spawn");
    rcfile
}

/// A session spawned inside the config fixture writes its shell-integration
/// wrappers under the redirected config directory, the same layout production
/// derives from the config file path.
#[cfg(unix)]
#[test]
fn shell_integration_inside_the_fixture_writes_under_the_redirected_config_dir() {
    let root = crate::test_dirs::fresh_temp_dir("odytty-spawn-writer-own-");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    for redirect_xdg in [true, false] {
        let base = root.join(if redirect_xdg { "xdg" } else { "home" });
        with_config_base(&base, redirect_xdg, || {
            let config_dir = crate::settings::config_file_path()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .expect("the redirected config path resolves");
            assert!(config_dir.starts_with(&base));
            assert_eq!(
                injected_bash_rcfile(),
                config_dir.join("shell-integration").join("odytty.bash")
            );
        });
    }
}

/// A session spawned on a thread outside the config fixture never writes into
/// the base another test thread has redirected the environment to.
#[cfg(unix)]
#[test]
fn shell_integration_on_another_thread_stays_out_of_a_redirected_base() {
    let root = crate::test_dirs::fresh_temp_dir("odytty-spawn-writer-other-");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let base = root.join("redirected");
    with_config_base(&base, true, || {
        let rcfile = std::thread::spawn(injected_bash_rcfile)
            .join()
            .expect("the spawning thread completes");
        assert!(
            !rcfile.starts_with(&root),
            "another thread's wrapper landed in the redirected base: {rcfile:?}"
        );
        assert!(!base.exists(), "the redirected base stays untouched");
    });
}

/// Outside the config fixture a session's shell-integration wrappers go to a
/// directory owned by the test process, never the config directory the live
/// environment names (the real one, or a relative synthetic base).
#[cfg(unix)]
#[test]
fn shell_integration_outside_the_fixture_stays_out_of_the_environment_config_dir() {
    let _env = crate::test_lock::test_env_lock();
    let ambient =
        crate::settings::config_file_path().and_then(|path| path.parent().map(Path::to_path_buf));
    let rcfile = injected_bash_rcfile();
    if let Some(ambient) = ambient {
        assert!(
            !rcfile.starts_with(&ambient),
            "a test wrote into the environment's config dir: {rcfile:?}"
        );
    }
    assert!(
        rcfile.is_absolute() && rcfile.starts_with(std::env::temp_dir()),
        "the wrapper lives in process-owned scratch: {rcfile:?}"
    );
}

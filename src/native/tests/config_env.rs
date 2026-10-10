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

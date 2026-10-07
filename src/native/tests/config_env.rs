// SPDX-License-Identifier: GPL-3.0-only
//! Panic-safe profile fixture isolation, with environment-before-catalog lock order.

use std::ffi::OsString;
use std::path::Path;

const KEYS: [&str; 3] = ["HOME", "XDG_CONFIG_HOME", "APPDATA"];

struct ConfigEnvRestore([Option<OsString>; 3]);

impl ConfigEnvRestore {
    /// The caller holds the shared environment lock until this guard drops.
    fn redirect(base: &Path, redirect_xdg: bool) -> Self {
        let restore = Self(KEYS.map(std::env::var_os));
        // SAFETY: callers hold test_env_lock throughout mutation and restoration.
        unsafe {
            std::env::set_var("HOME", base);
            std::env::set_var("APPDATA", base);
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

pub(super) fn with_config_base<R>(base: &Path, redirect_xdg: bool, f: impl FnOnce() -> R) -> R {
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

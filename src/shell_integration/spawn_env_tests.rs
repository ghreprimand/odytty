// SPDX-License-Identifier: GPL-3.0-only
//! The zsh and fish wrappers read `ZDOTDIR`, `HOME` and `XDG_DATA_DIRS` at
//! spawn time. A thread that does not hold the shared environment lock must
//! never see the values another test has redirected them to.
use super::tests::temp_integration_dir;
use super::{ShellKind, install::apply_spawn_integration_in_dir};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const KEYS: [&str; 3] = ["ZDOTDIR", "HOME", "XDG_DATA_DIRS"];

/// Sets `KEYS` under `root` for the closure, holding the environment lock,
/// and restores the previous values and removes `root` afterwards.
fn with_redirected_env<R>(root: &Path, f: impl FnOnce() -> R) -> R {
    struct Restore(Vec<(&'static str, Option<OsString>)>, PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            for (key, value) in self.0.drain(..) {
                // SAFETY: the environment lock is still held by the caller.
                unsafe {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&self.1);
        }
    }
    let _env = crate::test_lock::test_env_lock();
    let _restore = Restore(
        KEYS.iter()
            .map(|&key| (key, std::env::var_os(key)))
            .collect(),
        root.to_path_buf(),
    );
    // SAFETY: the shared environment lock is held for the whole window.
    unsafe {
        std::env::set_var("ZDOTDIR", root.join("zdotdir"));
        std::env::set_var("HOME", root.join("home"));
        std::env::set_var("XDG_DATA_DIRS", root.join("data"));
    }
    f()
}

fn injected(kind: ShellKind, program: &str, name: &str) -> crate::pty::CommandBuilder {
    let dir = temp_integration_dir(name);
    let mut command = crate::pty::CommandBuilder::new(program);
    apply_spawn_integration_in_dir(&mut command, kind, &dir);
    let _ = std::fs::remove_dir_all(dir);
    command
}

fn value(command: &crate::pty::CommandBuilder, key: &str) -> OsString {
    command
        .env_value(key)
        .unwrap_or_else(|| panic!("{key} is set"))
        .to_os_string()
}

#[test]
fn zsh_wrapper_on_another_thread_ignores_a_redirected_zdotdir_and_home() {
    let root = temp_integration_dir("spawn-env-zsh-root");
    with_redirected_env(&root, || {
        let command = std::thread::spawn(|| injected(ShellKind::Zsh, "zsh", "spawn-env-zsh"))
            .join()
            .expect("the spawning thread completes");
        let original = PathBuf::from(value(&command, "ODYTTY_ORIGINAL_ZDOTDIR"));
        assert!(!original.starts_with(&root), "{original:?}");
        assert_eq!(original, crate::settings::test_child_home());
        assert_eq!(value(&command, "ODYTTY_ORIGINAL_ZDOTDIR_SET"), "");
    });
}

#[test]
fn zsh_wrapper_on_the_lock_holding_thread_reads_the_live_zdotdir() {
    let root = temp_integration_dir("spawn-env-zsh-live-root");
    with_redirected_env(&root, || {
        let command = injected(ShellKind::Zsh, "zsh", "spawn-env-zsh-live");
        assert_eq!(
            PathBuf::from(value(&command, "ODYTTY_ORIGINAL_ZDOTDIR")),
            root.join("zdotdir")
        );
        assert_eq!(value(&command, "ODYTTY_ORIGINAL_ZDOTDIR_SET"), "1");
    });
}

#[test]
fn fish_wrapper_on_another_thread_ignores_a_redirected_data_dirs() {
    let root = temp_integration_dir("spawn-env-fish-root");
    with_redirected_env(&root, || {
        let command = std::thread::spawn(|| injected(ShellKind::Fish, "fish", "spawn-env-fish"))
            .join()
            .expect("the spawning thread completes");
        let dirs = value(&command, "XDG_DATA_DIRS")
            .to_string_lossy()
            .into_owned();
        assert!(dirs.ends_with(":/usr/local/share:/usr/share"), "{dirs}");
        assert!(!dirs.contains(&*root.to_string_lossy()), "{dirs}");
    });
}

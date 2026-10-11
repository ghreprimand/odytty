// SPDX-License-Identifier: GPL-3.0-only
//! A profile that sets `HOME` reaches the zsh wrapper: with no inherited
//! `ZDOTDIR`, the wrapper sources the dotfiles of the child's `HOME`, so the
//! profile value must be in place before integration records it. Both release
//! spawn paths (the explicit-shell profile command and the default shell) go
//! through the same environment helper. These tests run on a thread that does
//! not hold the environment lock, so no inherited `ZDOTDIR` is visible.
use crate::profiles::{LocalLaunchPlan, LocalSpawnKind, build_local_command};
use crate::pty::CommandBuilder;
use crate::settings::Settings;
use std::collections::BTreeMap;
use std::ffi::OsString;

const PROFILE_HOME: &str = "/odytty-test/profile-home";

fn value(command: &CommandBuilder, key: &str) -> OsString {
    command
        .env_value(key)
        .unwrap_or_else(|| panic!("{key} is set"))
        .to_os_string()
}

fn integrated_settings() -> Settings {
    Settings {
        shell_integration: true,
        ..Settings::default()
    }
}

fn profile_env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn assert_profile_home_is_the_zsh_dotfile_dir(command: &CommandBuilder) {
    assert_eq!(value(command, "ODYTTY_ORIGINAL_ZDOTDIR"), PROFILE_HOME);
    assert_eq!(value(command, "ODYTTY_ORIGINAL_ZDOTDIR_SET"), "");
    assert_eq!(value(command, "HOME"), PROFILE_HOME);
    assert_eq!(
        value(command, "ZDOTDIR"),
        value(command, "ODYTTY_ZSH_WRAPPER_DIR"),
        "integration still owns ZDOTDIR"
    );
}

#[test]
fn profile_shell_command_with_a_home_override_sources_that_home() {
    let plan = LocalLaunchPlan {
        settings: integrated_settings(),
        working_directory: None,
        env: profile_env(&[("HOME", PROFILE_HOME)]),
        spawn: LocalSpawnKind::Shell {
            program: "zsh".to_owned(),
            args: Vec::new(),
        },
    };
    assert_profile_home_is_the_zsh_dotfile_dir(&build_local_command(&plan));
}

#[test]
fn default_shell_with_a_profile_home_override_sources_that_home() {
    let mut command = CommandBuilder::new("zsh");
    command.apply_interactive_shell_env(
        true,
        false,
        false,
        &profile_env(&[("HOME", PROFILE_HOME)]),
    );
    assert_profile_home_is_the_zsh_dotfile_dir(&command);
}

/// Control: a profile `ZDOTDIR` still replaces the wrapper, as before, and a
/// profile without `HOME` leaves the child's own `HOME` as the dotfile dir.
#[test]
fn profile_zdotdir_still_wins_and_no_home_override_keeps_the_child_home() {
    let mut command = CommandBuilder::new("zsh");
    command.apply_interactive_shell_env(
        true,
        false,
        false,
        &profile_env(&[("ZDOTDIR", "/odytty-test/profile-zdotdir")]),
    );
    assert_eq!(value(&command, "ZDOTDIR"), "/odytty-test/profile-zdotdir");
    assert_eq!(
        std::path::PathBuf::from(value(&command, "ODYTTY_ORIGINAL_ZDOTDIR")),
        crate::settings::test_child_home()
    );
}

// SPDX-License-Identifier: GPL-3.0-only
#![allow(unused_imports)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::Theme;

use super::config::{config_key_to_env, env_to_config_key};
use super::reload::{ConfigFileFingerprint, ConfigPollEvent};
use super::*;

/// Serialize tests that call `apply_reloadable_values`, because it republishes
/// process-wide renderer globals.
static RELOAD_GLOBAL_TEST_LOCK: Mutex<()> = Mutex::new(());

mod cursor;
mod info;
mod keybinds;
mod legacy;
mod mouse;
mod numeric;
mod overlay;
mod sh2;
mod system_theme;

/// C14 (structural): every `ODYTTY_*` env-key const declared in
/// `settings/consts.rs` must be listed in [`SETTING_ENV_KEYS`]. That list is
/// the reload snapshot's source of truth (`reload::env_snapshot`), so a key
/// missing from it silently loses its env override on every config reload —
/// exactly what happened to `ODYTTY_COMMAND_STATUS_GUTTER`. The test parses
/// the consts source at compile time, so declaring a new `*_ENV` const
/// without listing it fails here instead of shipping the same bug again.
#[test]
fn setting_env_keys_lists_every_declared_env_const() {
    let source = include_str!("../consts.rs");
    let mut declared = Vec::new();
    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub const ") else {
            continue;
        };
        let Some((name, value_part)) = rest.split_once(':') else {
            continue;
        };
        if !name.trim().ends_with("_ENV") {
            continue;
        }
        let Some(start) = value_part.find('"') else {
            continue;
        };
        let Some(end) = value_part[start + 1..].find('"') else {
            continue;
        };
        declared.push(value_part[start + 1..start + 1 + end].to_owned());
    }

    assert!(
        declared.len() >= 70,
        "the consts parse found only {} env keys — parser broken?",
        declared.len()
    );
    let missing: Vec<&String> = declared
        .iter()
        .filter(|key| !SETTING_ENV_KEYS.contains(&key.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "env-key consts missing from SETTING_ENV_KEYS (their env overrides \
         are silently dropped on config reload): {missing:?}"
    );

    // Inverse direction: the list must not contain strays that no const
    // declares (a typo'd literal would never match the real env var).
    let stray: Vec<&&str> = SETTING_ENV_KEYS
        .iter()
        .filter(|key| !declared.iter().any(|d| d == **key))
        .collect();
    assert!(
        stray.is_empty(),
        "SETTING_ENV_KEYS entries with no declaring const: {stray:?}"
    );
}

#[cfg(not(windows))]
#[test]
fn config_base_dir_resolves_xdg_then_home() {
    // D-12 refactor guard on the unix legs: XDG_CONFIG_HOME wins, else
    // $HOME/.config, else nothing. (APPDATA is ignored off Windows.)
    let via_xdg = config_base_dir_from_env(
        None,
        Some(OsString::from("/xdg")),
        Some(OsString::from("/home/tester")),
    );
    assert_eq!(via_xdg, Some(PathBuf::from("/xdg").join(CONFIG_DIR_NAME)));

    let via_home = config_base_dir_from_env(None, None, Some(OsString::from("/home/tester")));
    assert_eq!(
        via_home,
        Some(
            PathBuf::from("/home/tester")
                .join(".config")
                .join(CONFIG_DIR_NAME)
        )
    );

    // An empty XDG value falls through to HOME.
    let empty_xdg = config_base_dir_from_env(
        None,
        Some(OsString::from("")),
        Some(OsString::from("/home/tester")),
    );
    assert_eq!(
        empty_xdg,
        Some(
            PathBuf::from("/home/tester")
                .join(".config")
                .join(CONFIG_DIR_NAME)
        )
    );

    assert_eq!(config_base_dir_from_env(None, None, None), None);
}

#[cfg(windows)]
#[test]
fn config_base_dir_prefers_appdata_on_windows() {
    // D-12: on Windows the config base resolves under %APPDATA%\odytty; an
    // empty/unset APPDATA falls through to XDG_CONFIG_HOME then HOME. Runs on
    // the windows-latest leg.
    let via_appdata = config_base_dir_from_env(
        Some(OsString::from("C:\\Users\\tester\\AppData\\Roaming")),
        None,
        None,
    )
    .expect("appdata base");
    assert_eq!(
        via_appdata,
        PathBuf::from("C:\\Users\\tester\\AppData\\Roaming").join(CONFIG_DIR_NAME)
    );

    // Empty APPDATA falls through to XDG, then HOME.
    let via_xdg = config_base_dir_from_env(
        Some(OsString::from("")),
        Some(OsString::from("D:\\xdg")),
        None,
    );
    assert_eq!(
        via_xdg,
        Some(PathBuf::from("D:\\xdg").join(CONFIG_DIR_NAME))
    );

    let via_home = config_base_dir_from_env(None, None, Some(OsString::from("C:\\Users\\tester")));
    assert_eq!(
        via_home,
        Some(
            PathBuf::from("C:\\Users\\tester")
                .join(".config")
                .join(CONFIG_DIR_NAME)
        )
    );

    assert_eq!(config_base_dir_from_env(None, None, None), None);
}

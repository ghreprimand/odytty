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

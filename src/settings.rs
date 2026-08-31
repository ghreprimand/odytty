// SPDX-License-Identifier: GPL-3.0-only
//! Runtime settings for the prototype.
//!
//! Settings are sourced from a small config file and environment variables, but
//! the rest of the app consumes this typed struct. That keeps runtime
//! configuration in one place without pushing `std::env` or file reads through
//! renderer and terminal code.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use std::path::Path;

use crate::atlas::SubpixelMode;
use crate::core::CursorStyle;
use crate::theme::{Theme, ThemeSpec, VisualEffect};

mod actions;
mod config;
mod consts;
mod descriptions;
mod editing;
mod fs_read;
mod info;
mod model;
mod parsing;
mod paths;
mod reload;
mod resolution;
mod runtime;
mod serialization;
mod values;
mod writeback;

pub use actions::*;
pub use consts::*;
pub use descriptions::*;
pub use editing::{bindable_action_display_name, format_key_chord, key_bindings_config_value};
pub use info::{NumericSpec, SettingInfo, SettingKind};
pub use model::*;
pub use paths::{config_file_path, theme_dir_path};
pub use reload::{
    ConfigReloadPoller, SettingsReloadOutcome, SettingsReloader, apply_reloadable_values,
};
pub use runtime::*;
pub use writeback::{
    ConfigWritebackError, ConfigWritebackResult, ensure_config_file_exists,
    ensure_config_file_exists_at, write_settings_changes, write_settings_changes_to_path,
};

use self::values::*;
use config::{ConfigValues, env_to_config_key};
pub(crate) use consts::SETTING_ENV_KEYS;
use editing::{font_family_error_message, key_bindings_edit_value};
use parsing::{
    format_symbol_map, parse_font_weight_variant, parse_symbol_font_path, parse_symbol_map,
};
#[cfg(test)]
use paths::config_base_dir_from_env;
use paths::{normalize_name, resolve_theme_file};

#[cfg(test)]
mod tests;

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

mod info;
mod legacy;
mod mouse;

// SPDX-License-Identifier: GPL-3.0-only
//! Bounded limits for named launch profiles.

/// Current on-disk profile document schema version.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

/// Profiles live under `<config-dir>/profiles/`.
pub const PROFILES_DIR_NAME: &str = "profiles";

/// Suffix for one profile file: `<name>.profile.json`.
pub const PROFILE_FILE_SUFFIX: &str = ".profile.json";

/// Maximum bytes read from one profile file (same class as config/theme caps).
pub const MAX_PROFILE_FILE_BYTES: u64 = 1 << 20;

/// Maximum profiles returned from a local directory scan.
pub const MAX_PROFILE_ENTRIES: usize = 256;

/// Maximum characters retained for one profile name; display labels use the field cap.
pub const MAX_PROFILE_NAME_CHARS: usize = 64;

/// Maximum UTF-8 characters retained for one string field inside a profile.
pub const MAX_PROFILE_FIELD_CHARS: usize = 512;

/// Maximum environment override entries inside one profile.
pub const MAX_PROFILE_ENV_ENTRIES: usize = 64;

/// Maximum UTF-8 characters retained for one environment value.
pub const MAX_PROFILE_ENV_VALUE_CHARS: usize = 1024;

/// Maximum command arguments stored in a profile launch command.
pub const MAX_PROFILE_COMMAND_ARGS: usize = 32;

/// Maximum parse warnings retained while loading one profile directory.
pub const MAX_PROFILE_WARNINGS: usize = 100;

/// Maximum host match rules stored on one profile.
pub const MAX_PROFILE_SWITCH_HOSTS: usize = 16;

/// Maximum directory match rules stored on one profile.
pub const MAX_PROFILE_SWITCH_DIRECTORIES: usize = 16;

/// Maximum directory entries examined while loading the profile catalog,
/// counting every entry (other files, directories, malformed profiles), so a
/// directory full of irrelevant or broken entries cannot make a load
/// unbounded. Four times [`MAX_PROFILE_ENTRIES`] leaves room for a normal
/// directory's backups and editor files.
pub const MAX_PROFILE_DIR_SCAN_ENTRIES: usize = 4 * MAX_PROFILE_ENTRIES;

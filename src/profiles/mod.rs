// SPDX-License-Identifier: GPL-3.0-only
//! Named launch profiles: schema, local storage, precedence, and migration.

mod discovery;
mod json;
mod launch;
mod limits;
mod migration;
pub(crate) mod precedence;
mod schema;
mod store;
mod switch;

pub use switch::{
    ProfileSwitchReason, ProfileSwitchSuggestion, normalize_directory_pattern,
    normalize_host_pattern, suggest_profile_switch,
};

pub(crate) use json::{Json, parse as parse_json};

pub use discovery::{DiscoveredShell, ShellKind, discovered_shells, parse_wsl_distro_list};
pub use launch::{LocalLaunchPlan, LocalSpawnKind, build_local_command, spawn_local_plan};
pub use limits::*;
pub use migration::{normalize_workspace_connection_binding, profile_from_connection_host};
pub use precedence::{
    EffectiveLaunch, LaunchCliOverrides, LiveLaunchOverrides, PrecedenceLayer,
    RestoredLaunchOverrides, precedence_chain, validate_named_profile_reference,
};
pub use schema::{
    LaunchProfile, ProfileAppearance, ProfileCommand, ProfileCursor, ProfileEffects, ProfileError,
    ProfileLaunch, ProfileLayout, ProfilePlatform, ProfileSwitchRules, profile_file_name,
    profile_name_from_path, validate_profile_name,
};
pub use store::{
    ProfileCatalog, ProfileStoreError, catalog_load_count_for_test, delete_profile_file,
    export_profile_file, load_catalog_from_dir, profile_path_in_dir, profiles_dir_from_env,
    profiles_dir_path, quarantine_malformed_file, read_profile_file,
    reset_catalog_load_count_for_test, write_profile_file,
};

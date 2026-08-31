// SPDX-License-Identifier: GPL-3.0-only
//! Named launch profiles: schema, local storage, precedence, and migration.

mod json;
mod limits;
mod migration;
pub(crate) mod precedence;
mod schema;
mod store;

pub use limits::*;
pub use migration::{normalize_workspace_connection_binding, profile_from_connection_host};
pub use precedence::{
    EffectiveLaunch, LaunchCliOverrides, LiveLaunchOverrides, PrecedenceLayer,
    RestoredLaunchOverrides, precedence_chain, validate_named_profile_reference,
};
pub use schema::{
    LaunchProfile, ProfileAppearance, ProfileCommand, ProfileCursor, ProfileEffects, ProfileError,
    ProfileLaunch, ProfileLayout, ProfilePlatform, profile_file_name, profile_name_from_path,
    validate_profile_name,
};
pub use store::{
    ProfileCatalog, ProfileStoreError, delete_profile_file, load_catalog_from_dir,
    profile_path_in_dir, profiles_dir_from_env, profiles_dir_path, quarantine_malformed_file,
    read_profile_file, write_profile_file,
};

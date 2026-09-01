// SPDX-License-Identifier: GPL-3.0-only
//! Local profile catalog loading, atomic writes, and malformed-file recovery.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::settings::config_base_dir_from_env;
use crate::settings::fs_read;

use super::limits::*;
use super::schema::{
    LaunchProfile, ProfileError, profile_file_name, profile_name_from_path, validate_profile_name,
};

/// Test-only counter of [`load_catalog_from_dir`] calls. Default launch must
/// leave this at zero; the Profile Manager increments it only when opened.
static CATALOG_LOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

/// In-memory catalog of locally stored named profiles.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileCatalog {
    pub profiles: BTreeMap<String, LaunchProfile>,
    pub warnings: Vec<String>,
}

/// Outcome of reading or writing one profile file.
#[derive(Debug, PartialEq, Eq)]
pub enum ProfileStoreError {
    Io(String),
    Validation(ProfileError),
}

impl std::fmt::Display for ProfileStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Validation(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ProfileStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Validation(error) => Some(error),
            Self::Io(_) => None,
        }
    }
}

impl From<io::Error> for ProfileStoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<ProfileError> for ProfileStoreError {
    fn from(value: ProfileError) -> Self {
        Self::Validation(value)
    }
}

impl ProfileCatalog {
    pub fn get(&self, name: &str) -> Option<&LaunchProfile> {
        self.profiles.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.profiles.values().map(|profile| profile.name.as_str())
    }
}

/// Resolve `<config-dir>/profiles` from the same base rules as settings.
pub fn profiles_dir_from_env(
    appdata: Option<std::ffi::OsString>,
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    config_base_dir_from_env(appdata, xdg_config_home, home).map(|dir| dir.join(PROFILES_DIR_NAME))
}

pub fn profiles_dir_path() -> Option<PathBuf> {
    profiles_dir_from_env(
        std::env::var_os("APPDATA"),
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Load every regular `*.profile.json` file in `dir` without leaving the local
/// filesystem. Missing directories yield an empty catalog; malformed files warn
/// and are skipped so one bad profile cannot block startup.
pub fn load_catalog_from_dir(dir: &Path) -> ProfileCatalog {
    CATALOG_LOAD_COUNT.fetch_add(1, Ordering::Relaxed);
    let mut catalog = ProfileCatalog::default();
    let mut suppressed = 0usize;
    let mut warn = |message: String| {
        if catalog.warnings.len() < MAX_PROFILE_WARNINGS {
            catalog.warnings.push(message);
        } else {
            suppressed += 1;
        }
    };

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return catalog,
        Err(error) => {
            warn(format!(
                "could not read profiles directory {}: {error}",
                dir.display()
            ));
            return catalog;
        }
    };

    for entry in entries.flatten() {
        if catalog.profiles.len() >= MAX_PROFILE_ENTRIES {
            warn(format!(
                "profiles directory {} exceeds {MAX_PROFILE_ENTRIES} entries; remaining files skipped",
                dir.display()
            ));
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = profile_name_from_path(&path) else {
            continue;
        };
        match read_profile_file(&path, Some(name.as_str())) {
            Ok(profile) => {
                if let Some(existing) = catalog.profiles.insert(profile.name.clone(), profile) {
                    warn(format!(
                        "duplicate profile name {:?}; keeping {}",
                        existing.name,
                        path.display()
                    ));
                }
            }
            Err(error) => warn(format!("{}: {error}", path.display())),
        }
    }

    if suppressed > 0 {
        catalog
            .warnings
            .push(format!("{suppressed} further profile warnings suppressed"));
    }
    catalog
}

pub fn read_profile_file(
    path: &Path,
    expected_name: Option<&str>,
) -> Result<LaunchProfile, ProfileStoreError> {
    let contents = fs_read::read_capped_at(path, MAX_PROFILE_FILE_BYTES)?;
    LaunchProfile::parse_json(&contents, expected_name).map_err(ProfileStoreError::Validation)
}

pub fn write_profile_file(path: &Path, profile: &LaunchProfile) -> Result<(), ProfileStoreError> {
    let bytes = profile.validated_serialization()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::state_dir::write_atomic(
        path,
        bytes.as_bytes(),
        crate::state_dir::WriteMode::Sensitive,
    )?;
    Ok(())
}

/// Export a validated profile to a user-chosen destination using the shared
/// export egress (`WriteMode::Export`). Still routes through
/// [`LaunchProfile::validated_serialization`] so secrets and over-limit fields
/// cannot leave the manager.
pub fn export_profile_file(path: &Path, profile: &LaunchProfile) -> Result<(), ProfileStoreError> {
    let bytes = profile.validated_serialization()?;
    crate::state_dir::write_atomic(path, bytes.as_bytes(), crate::state_dir::WriteMode::Export)?;
    Ok(())
}

/// Number of times [`load_catalog_from_dir`] has run in this process. Used by
/// startup-isolation tests to prove the default launch path never enumerates
/// profiles.
pub fn catalog_load_count_for_test() -> usize {
    CATALOG_LOAD_COUNT.load(Ordering::Relaxed)
}

/// Reset [`catalog_load_count_for_test`] between isolation cases.
pub fn reset_catalog_load_count_for_test() {
    CATALOG_LOAD_COUNT.store(0, Ordering::Relaxed);
}

pub fn delete_profile_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn profile_path_in_dir(dir: &Path, name: &str) -> Result<PathBuf, ProfileError> {
    let name = validate_profile_name(name)?;
    Ok(dir.join(profile_file_name(&name)))
}

/// Recover a malformed profile file by renaming it beside the original with a
/// `.bad` suffix. Returns `true` when a rename happened.
pub fn quarantine_malformed_file(path: &Path) -> io::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("profile.profile.json");
    let quarantined = path.with_file_name(format!("{file_name}.bad"));
    match fs::rename(path, &quarantined) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{LaunchProfile, MAX_PROFILE_FIELD_CHARS};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_profiles_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "odytty-profiles-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn atomic_write_and_reload_round_trip() {
        let dir = temp_profiles_dir("write");
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("dev.profile.json");
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile.display_name = Some("Dev".to_owned());
        profile.launch.working_directory = Some("/tmp/project".to_owned());
        write_profile_file(&path, &profile).expect("write");
        let loaded = read_profile_file(&path, Some("dev")).expect("read");
        assert_eq!(loaded, profile);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_file_is_skipped_without_emptying_catalog() {
        // Loads a catalog, bumping the process-global load counter that the
        // startup-isolation tests assert on; hold the catalog-count guard so
        // this load cannot land between a sibling's reset and assertion.
        let _count_guard = crate::test_lock::catalog_count_lock();
        let dir = temp_profiles_dir("malformed");
        fs::create_dir_all(&dir).expect("mkdir");
        let good = dir.join("good.profile.json");
        write_profile_file(&good, &LaunchProfile::new("good").expect("good")).expect("write good");
        fs::write(dir.join("bad.profile.json"), "{ not json").expect("write bad");
        let catalog = load_catalog_from_dir(&dir);
        assert_eq!(catalog.profiles.len(), 1);
        assert!(catalog.profiles.contains_key("good"));
        assert!(!catalog.warnings.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_programmatic_profile_is_not_written() {
        let dir = temp_profiles_dir("reject-write");
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("dev.profile.json");
        let mut profile = LaunchProfile::new("dev").expect("profile");
        profile
            .launch
            .env
            .insert("API_TOKEN".to_owned(), "not-a-real-secret".to_owned());
        assert!(matches!(
            write_profile_file(&path, &profile),
            Err(ProfileStoreError::Validation(ProfileError::RejectedSecret(
                _
            )))
        ));
        assert!(!path.exists());

        let mut over_limit = LaunchProfile::new("dev").expect("profile");
        over_limit.launch.working_directory = Some("x".repeat(MAX_PROFILE_FIELD_CHARS + 1));
        assert!(matches!(
            write_profile_file(&path, &over_limit),
            Err(ProfileStoreError::Validation(ProfileError::LimitExceeded(
                _
            )))
        ));
        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn new_profile_is_owner_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = temp_profiles_dir("private");
        let path = dir.join("dev.profile.json");
        write_profile_file(&path, &LaunchProfile::new("dev").expect("profile")).expect("write");
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(dir);
    }
}

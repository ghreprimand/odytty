// SPDX-License-Identifier: GPL-3.0-only
//! Local profile catalog loading, atomic writes, and malformed-file recovery.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::settings::config_base_dir_from_env;
use crate::settings::fs_read;

use super::catalog_cache;
use super::limits::*;
use super::schema::{LaunchProfile, ProfileError, profile_file_name, validate_profile_name};
use super::switch::unmatchable_switch_patterns;

#[cfg(test)]
thread_local! {
    /// Catalog loads on this test thread, including cache hits. Loads on other
    /// threads cannot change the startup resolver's exact-count assertions.
    static CATALOG_LOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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
///
/// The scan examines at most [`MAX_PROFILE_DIR_SCAN_ENTRIES`] directory
/// entries and keeps at most [`MAX_PROFILE_ENTRIES`] profiles, each with a
/// warning when it truncates. Files load in name order. When the listing
/// matches the previous load's exactly, that parsed catalog is reused (see
/// [`super::catalog_cache`]).
pub fn load_catalog_from_dir(dir: &Path) -> ProfileCatalog {
    #[cfg(test)]
    CATALOG_LOAD_COUNT.with(|count| count.set(count.get() + 1));
    let listing = match catalog_cache::list_profile_dir(dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return ProfileCatalog::default();
        }
        Err(error) => {
            return ProfileCatalog {
                warnings: vec![format!(
                    "could not read profiles directory {}: {error}",
                    dir.display()
                )],
                ..ProfileCatalog::default()
            };
        }
    };
    if let Some(catalog) = catalog_cache::cached(dir, &listing) {
        return catalog;
    }

    let mut catalog = ProfileCatalog::default();
    let mut suppressed = 0usize;
    let mut warn = |message: String| {
        if catalog.warnings.len() < MAX_PROFILE_WARNINGS {
            catalog.warnings.push(message);
        } else {
            suppressed += 1;
        }
    };

    if listing.truncated {
        warn(format!(
            "profiles directory {} has more than {MAX_PROFILE_DIR_SCAN_ENTRIES} entries; \
             only the first {MAX_PROFILE_DIR_SCAN_ENTRIES} were examined",
            dir.display()
        ));
    }
    for candidate in &listing.candidates {
        if catalog.profiles.len() >= MAX_PROFILE_ENTRIES {
            warn(format!(
                "profiles directory {} exceeds {MAX_PROFILE_ENTRIES} entries; remaining files skipped",
                dir.display()
            ));
            break;
        }
        let path = &candidate.path;
        match read_profile_file(path, Some(candidate.name.as_str())) {
            Ok(profile) => {
                for problem in unmatchable_switch_patterns(&profile.name, &profile.switch) {
                    warn(problem);
                }
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
    catalog_cache::store(dir, listing, &catalog);
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
        reject_case_only_collision(parent, path)?;
    }
    crate::state_dir::write_atomic(
        path,
        bytes.as_bytes(),
        crate::state_dir::WriteMode::Sensitive,
    )?;
    Ok(())
}

/// Refuse a save whose file name differs from an existing profile file only by
/// case. On a case-insensitive volume (macOS and Windows defaults) both names
/// are one file, so the save would silently overwrite the other profile while
/// the catalog, which keys by exact name, held both until a restart. A failed
/// or truncated listing refuses the save. At most
/// [`MAX_PROFILE_DIR_SCAN_ENTRIES`] attempted entries are inspected, plus one
/// exhaustion probe. This check does not serialize concurrent directory edits.
fn reject_case_only_collision(dir: &Path, target: &Path) -> Result<(), ProfileStoreError> {
    let Some(target_name) = target.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let entries =
        fs::read_dir(dir).map(|entries| entries.map(|entry| entry.map(|entry| entry.file_name())));
    check_case_only_entries(target_name, entries)
}

fn check_case_only_entries(
    target_name: &str,
    entries: io::Result<impl Iterator<Item = io::Result<std::ffi::OsString>>>,
) -> Result<(), ProfileStoreError> {
    let mut entries = entries?;
    for _ in 0..MAX_PROFILE_DIR_SCAN_ENTRIES {
        let Some(name) = entries.next() else {
            return Ok(());
        };
        let name = name?;
        let Some(name) = name.to_str() else {
            continue;
        };
        if name != target_name && name.eq_ignore_ascii_case(target_name) {
            return Err(ProfileStoreError::Validation(ProfileError::InvalidName(
                format!("{name:?} already exists and differs only by letter case"),
            )));
        }
    }
    if entries.next().is_some() {
        return Err(ProfileStoreError::Io(
            "profile collision scan exceeded its directory entry limit; save refused".to_owned(),
        ));
    }
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

/// Number of [`load_catalog_from_dir`] calls on the current test thread.
/// Startup-isolation tests observe synchronous resolver work; catalog loads
/// from other threads are deliberately excluded.
#[cfg(test)]
pub fn catalog_load_count_for_test() -> usize {
    CATALOG_LOAD_COUNT.with(std::cell::Cell::get)
}

/// Reset [`catalog_load_count_for_test`] on the current test thread.
#[cfg(test)]
pub fn reset_catalog_load_count_for_test() {
    CATALOG_LOAD_COUNT.with(|count| count.set(0));
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

/// Most `.bad` copies kept beside one profile file; see
/// [`quarantine_malformed_file`].
pub(super) const MAX_QUARANTINE_COPIES: usize = 100;

/// Recover a malformed regular profile file beside the original with a `.bad`
/// suffix. Each destination is claimed with a non-replacing hard link before
/// removing the source. Collisions retry `.bad.1`, `.bad.2`, and so on, up to
/// [`MAX_QUARANTINE_COPIES`]. Inspection or link errors leave the source intact;
/// filesystems without hard links refuse quarantine. A source removal failure
/// leaves both names intact and returns an error. Pathname substitution is not
/// serialized. Returns `true` when the source was removed after a claim.
pub fn quarantine_malformed_file(path: &Path) -> io::Result<bool> {
    quarantine_with(
        path,
        |candidate| fs::symlink_metadata(candidate),
        quarantine_claim,
    )
}

fn quarantine_claim(source: &Path, destination: &Path) -> io::Result<()> {
    // Hard-link creation never replaces an existing destination on Unix or
    // Windows. A replacing rename would reopen the inspection/claim race.
    fs::hard_link(source, destination)
}

fn quarantine_with(
    path: &Path,
    mut inspect: impl FnMut(&Path) -> io::Result<fs::Metadata>,
    mut claim: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<bool> {
    match inspect(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("profile.profile.json");
    for attempt in 0..MAX_QUARANTINE_COPIES {
        let candidate = match attempt {
            0 => path.with_file_name(format!("{file_name}.bad")),
            n => path.with_file_name(format!("{file_name}.bad.{n}")),
        };
        match inspect(&candidate) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match claim(path, &candidate) {
            Ok(()) => {
                fs::remove_file(path)?;
                return Ok(true);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many quarantined copies of this profile; remove old .bad files",
    ))
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
    fn collision_scan_accepts_complete_listing_at_bound() {
        let entries = (0..MAX_PROFILE_DIR_SCAN_ENTRIES)
            .map(|n| Ok(std::ffi::OsString::from(format!("other{n}"))));
        assert!(check_case_only_entries("dev.profile.json", Ok(entries)).is_ok());
    }

    #[test]
    fn collision_scan_refuses_unseen_tail() {
        let entries = (0..=MAX_PROFILE_DIR_SCAN_ENTRIES).map(|n| {
            Ok(std::ffi::OsString::from(
                if n == MAX_PROFILE_DIR_SCAN_ENTRIES {
                    "DEV.profile.json".to_owned()
                } else {
                    format!("other{n}")
                },
            ))
        });
        assert!(check_case_only_entries("dev.profile.json", Ok(entries)).is_err());
    }

    #[test]
    fn collision_scan_refuses_iteration_error() {
        let entries = [
            Ok(std::ffi::OsString::from("other")),
            Err(io::Error::other("entry failure")),
        ];
        assert!(check_case_only_entries("dev.profile.json", Ok(entries.into_iter())).is_err());
    }

    #[test]
    fn collision_scan_refuses_listing_error() {
        let entries: io::Result<std::iter::Empty<io::Result<std::ffi::OsString>>> = Err(
            io::Error::new(io::ErrorKind::PermissionDenied, "listing denied"),
        );
        assert!(check_case_only_entries("dev.profile.json", entries).is_err());
    }

    #[test]
    fn quarantine_race_keeps_competing_backup_and_source_when_slots_fill() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-quarantine-race-");
        let path = dir.join("dev.profile.json");
        fs::write(&path, "source text").expect("source");
        let result = quarantine_with(
            &path,
            |p| fs::symlink_metadata(p),
            |source, destination| {
                // Install the first backup after inspection, then occupy every
                // remaining slot. No replacing operation may touch the first copy.
                fs::write(destination, "competing text")?;
                for n in 1..MAX_QUARANTINE_COPIES {
                    fs::write(
                        source.with_file_name(format!("dev.profile.json.bad.{n}")),
                        "kept",
                    )?;
                }
                quarantine_claim(source, destination)
            },
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&path).expect("source retained"),
            "source text"
        );
        assert_eq!(
            fs::read_to_string(dir.join("dev.profile.json.bad")).expect("backup"),
            "competing text"
        );
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn quarantine_inspection_error_preserves_source() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-quarantine-error-");
        let path = dir.join("dev.profile.json");
        fs::write(&path, "source text").expect("source");
        let result = quarantine_with(
            &path,
            |p| {
                if p == path {
                    fs::symlink_metadata(p)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "inspection denied",
                    ))
                }
            },
            quarantine_claim,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&path).expect("source retained"),
            "source text"
        );
        assert!(!dir.join("dev.profile.json.bad").exists());
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn quarantine_uncertain_claim_preserves_source_without_retry() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-quarantine-claim-");
        let path = dir.join("dev.profile.json");
        fs::write(&path, "source text").expect("source");
        let mut attempts = 0;
        let error = quarantine_with(
            &path,
            |p| fs::symlink_metadata(p),
            |_, _| {
                attempts += 1;
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "link unavailable",
                ))
            },
        )
        .expect_err("uncertain claim");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(attempts, 1);
        assert_eq!(
            fs::read_to_string(&path).expect("source retained"),
            "source text"
        );
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn quarantine_source_inspection_failure_is_reported() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-quarantine-source-");
        let path = dir.join("dev.profile.json");
        fs::write(&path, "source text").expect("source");
        let result = quarantine_with(
            &path,
            |_| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "source inspection denied",
                ))
            },
            quarantine_claim,
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&path).expect("source retained"),
            "source text"
        );
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn quarantine_retries_a_competing_claim_without_replacing_it() {
        let dir = crate::test_dirs::fresh_temp_dir("odytty-quarantine-retry-");
        let path = dir.join("dev.profile.json");
        fs::write(&path, "source text").expect("source");
        let mut raced = false;
        assert!(
            quarantine_with(
                &path,
                |p| fs::symlink_metadata(p),
                |source, destination| {
                    if !raced {
                        fs::write(destination, "competing text")?;
                        raced = true;
                    }
                    quarantine_claim(source, destination)
                }
            )
            .expect("retry quarantine")
        );
        assert_eq!(
            fs::read_to_string(dir.join("dev.profile.json.bad")).expect("first"),
            "competing text"
        );
        assert_eq!(
            fs::read_to_string(dir.join("dev.profile.json.bad.1")).expect("second"),
            "source text"
        );
        assert!(!path.exists());
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn catalog_load_count_is_isolated_between_threads() {
        let _count_guard = crate::test_lock::catalog_count_lock();
        let dir = crate::test_dirs::fresh_temp_dir("odytty-profile-count-");
        reset_catalog_load_count_for_test();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let catalog = load_catalog_from_dir(&dir);
                assert!(catalog.profiles.is_empty());
                catalog_load_count_for_test()
            });
            let worker_count = worker.join().expect("catalog worker");
            assert_eq!(
                catalog_load_count_for_test(),
                0,
                "another thread's load is excluded"
            );
            assert_eq!(worker_count, 1, "the worker observes its own load");

            let catalog = load_catalog_from_dir(&dir);
            assert!(catalog.profiles.is_empty());
            assert_eq!(
                catalog_load_count_for_test(),
                1,
                "the caller observes its own load"
            );
            scope
                .spawn(reset_catalog_load_count_for_test)
                .join()
                .expect("counter reset worker");
            assert_eq!(
                catalog_load_count_for_test(),
                1,
                "another thread's reset is excluded"
            );
        });
        fs::remove_dir_all(dir).expect("remove empty fixture directory");
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
    fn a_profile_saved_with_a_byte_order_mark_loads() {
        let dir = temp_profiles_dir("bom");
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("dev.profile.json");
        let profile = LaunchProfile::new("dev").expect("profile");
        let mut bytes = "\u{feff}".as_bytes().to_vec();
        bytes.extend(
            profile
                .validated_serialization()
                .expect("serialize")
                .as_bytes(),
        );
        fs::write(&path, bytes).expect("write");
        assert_eq!(
            read_profile_file(&path, Some("dev")).expect("read"),
            profile
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_file_is_skipped_without_emptying_catalog() {
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

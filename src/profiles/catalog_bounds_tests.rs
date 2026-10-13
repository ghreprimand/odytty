// SPDX-License-Identifier: GPL-3.0-only
//! Profile catalog scan bounds and the verified catalog cache.

use std::fs;
use std::path::{Path, PathBuf};

use super::{
    LaunchProfile, MAX_PROFILE_DIR_SCAN_ENTRIES, MAX_PROFILE_WARNINGS,
    catalog_parse_count_for_test, load_catalog_from_dir, write_profile_file,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        Self(crate::test_dirs::fresh_temp_dir("odytty-profile-scan-"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn suppressed_count(warnings: &[String]) -> usize {
    warnings
        .iter()
        .find_map(|warning| {
            warning
                .strip_suffix(" further profile warnings suppressed")
                .and_then(|count| count.parse().ok())
        })
        .unwrap_or(0)
}

#[test]
fn malformed_and_unrelated_entries_count_toward_the_scan_bound() {
    let _count_guard = crate::test_lock::catalog_count_lock();
    let dir = TempDir::new();
    let malformed = MAX_PROFILE_DIR_SCAN_ENTRIES + 76;
    for index in 0..malformed {
        fs::write(
            dir.path().join(format!("broken{index:04}.profile.json")),
            "{ not json",
        )
        .expect("write malformed profile");
    }
    for index in 0..50 {
        fs::write(dir.path().join(format!("notes{index}.txt")), "x").expect("write other file");
    }

    let catalog = load_catalog_from_dir(dir.path());
    assert!(catalog.profiles.is_empty());
    assert!(
        catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("were examined")),
        "the truncated scan is reported: {:?}",
        catalog.warnings.first()
    );
    // Every warning past the truncation notice is one parsed file; the total
    // never exceeds the examined-entry bound.
    let file_warnings = catalog.warnings.len() - 2 + suppressed_count(&catalog.warnings);
    assert_eq!(catalog.warnings.len(), MAX_PROFILE_WARNINGS + 1);
    assert!(
        file_warnings <= MAX_PROFILE_DIR_SCAN_ENTRIES,
        "{file_warnings} files parsed past the {MAX_PROFILE_DIR_SCAN_ENTRIES}-entry bound"
    );
}

#[test]
fn an_unchanged_directory_is_parsed_once_and_any_change_reloads() {
    let _count_guard = crate::test_lock::catalog_count_lock();
    let dir = TempDir::new();
    let path = dir.path().join("dev.profile.json");
    let mut profile = LaunchProfile::new("dev").expect("profile");
    profile.display_name = Some("First".to_owned());
    write_profile_file(&path, &profile).expect("write");

    let parses = catalog_parse_count_for_test();
    let first = load_catalog_from_dir(dir.path());
    let second = load_catalog_from_dir(dir.path());
    assert_eq!(first, second);
    assert_eq!(
        catalog_parse_count_for_test(),
        parses + 1,
        "an unchanged directory reuses the parsed catalog"
    );

    profile.display_name = Some("Second".to_owned());
    write_profile_file(&path, &profile).expect("rewrite");
    let third = load_catalog_from_dir(dir.path());
    assert_eq!(
        third.get("dev").and_then(|p| p.display_name.as_deref()),
        Some("Second"),
        "a rewritten profile is reloaded"
    );

    write_profile_file(
        &dir.path().join("ops.profile.json"),
        &LaunchProfile::new("ops").expect("ops"),
    )
    .expect("add");
    assert!(load_catalog_from_dir(dir.path()).get("ops").is_some());

    fs::remove_file(dir.path().join("ops.profile.json")).expect("remove");
    assert!(load_catalog_from_dir(dir.path()).get("ops").is_none());
    assert_eq!(catalog_parse_count_for_test(), parses + 4);
}

#[test]
fn an_in_place_edit_of_the_same_length_reloads() {
    let _count_guard = crate::test_lock::catalog_count_lock();
    let dir = TempDir::new();
    let path = dir.path().join("dev.profile.json");
    let mut profile = LaunchProfile::new("dev").expect("profile");
    profile.display_name = Some("AAAA".to_owned());
    write_profile_file(&path, &profile).expect("write");
    assert_eq!(
        load_catalog_from_dir(dir.path())
            .get("dev")
            .and_then(|p| p.display_name.clone()),
        Some("AAAA".to_owned())
    );

    // Move the modification time explicitly so the test never depends on the
    // timestamp resolution of the filesystem it runs on.
    let text = fs::read_to_string(&path).expect("read");
    let first_modified = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .expect("modified time");
    fs::write(&path, text.replace("AAAA", "BBBB")).expect("edit in place");
    fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(first_modified + std::time::Duration::from_secs(5)))
        .expect("advance modified time");
    assert_eq!(
        load_catalog_from_dir(dir.path())
            .get("dev")
            .and_then(|p| p.display_name.clone()),
        Some("BBBB".to_owned())
    );
}

/// Restores a file's saved permissions on drop, including during unwinding, so
/// a failed assertion never leaves a read-only fixture for the directory
/// cleanup to trip over (a read-only file can block deletion on Windows).
struct RestorePermissions {
    path: PathBuf,
    original: fs::Permissions,
}

impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, self.original.clone());
    }
}

/// A read-only toggle changes the Unix mode and change time and the Windows
/// file attributes, so it reloads on every platform even though the size and
/// modification time stay the same.
#[test]
fn a_read_only_toggle_reloads_on_every_platform() {
    let _count_guard = crate::test_lock::catalog_count_lock();
    let dir = TempDir::new();
    let path = dir.path().join("dev.profile.json");
    write_profile_file(&path, &LaunchProfile::new("dev").expect("profile")).expect("write");

    let parses = catalog_parse_count_for_test();
    let _ = load_catalog_from_dir(dir.path());
    let _ = load_catalog_from_dir(dir.path());
    assert_eq!(
        catalog_parse_count_for_test(),
        parses + 1,
        "unchanged: one parse"
    );

    let before = fs::metadata(&path).expect("metadata");
    let original = before.permissions();
    let _restore = RestorePermissions {
        path: path.clone(),
        original: original.clone(),
    };
    let mut read_only = original.clone();
    read_only.set_readonly(true);
    fs::set_permissions(&path, read_only).expect("set read-only");
    let after = fs::metadata(&path).expect("metadata");
    assert_eq!(after.len(), before.len());
    assert_eq!(after.modified().ok(), before.modified().ok());

    assert!(load_catalog_from_dir(dir.path()).get("dev").is_some());
    assert_eq!(
        catalog_parse_count_for_test(),
        parses + 2,
        "an attribute or mode change reloads the catalog"
    );
}

/// A concurrent catalog load must not affect this test's parse count or evict
/// its verified cache entry. The child deliberately does not take the count lock.
#[test]
fn catalog_parse_counts_and_cache_are_isolated_between_test_threads() {
    let _count_guard = crate::test_lock::catalog_count_lock();
    let own = TempDir::new();
    let other = TempDir::new();
    for dir in [own.path(), other.path()] {
        write_profile_file(
            &dir.join("dev.profile.json"),
            &LaunchProfile::new("dev").expect("profile"),
        )
        .expect("write");
    }
    let before = catalog_parse_count_for_test();
    let first = load_catalog_from_dir(own.path());
    assert_eq!(catalog_parse_count_for_test(), before + 1);

    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let before = catalog_parse_count_for_test();
                let first = load_catalog_from_dir(other.path());
                assert_eq!(load_catalog_from_dir(other.path()), first);
                assert_eq!(catalog_parse_count_for_test(), before + 1);
            })
            .join()
            .expect("concurrent catalog load");
    });
    assert_eq!(
        catalog_parse_count_for_test(),
        before + 1,
        "another test thread cannot increment this count"
    );
    assert_eq!(load_catalog_from_dir(own.path()), first);
    assert_eq!(
        catalog_parse_count_for_test(),
        before + 1,
        "another test thread cannot evict this cache entry"
    );
}

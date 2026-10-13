// SPDX-License-Identifier: GPL-3.0-only
//! Bounded profile-directory listing and the verified catalog cache.
//!
//! Every catalog load lists the directory (at most
//! [`MAX_PROFILE_DIR_SCAN_ENTRIES`] entries, whatever they are) and records a
//! stamp of each candidate file: its name, size, and modification time, plus
//! on Unix its inode, change time, and mode, and on Windows its creation time
//! and file attributes. When the directory and every stamp match the previous
//! load, the parsed catalog is reused instead of reading and parsing every
//! file again. Working-directory reports from the shell (one per prompt with
//! profile auto-switch enabled) therefore cost one bounded listing, not a full
//! parse.
//!
//! Unix stamps include inode, change time and mode; detected stamp changes
//! invalidate the cache on the next load. Listing is not a current-disk
//! snapshot: a later write or a coarse timestamp can evade that lookup.
//! Windows stamps lack change time and file index, so an ACL-only change or
//! same-size replacement with unchanged timestamps can remain cached until
//! another detected change or a restart.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use super::limits::MAX_PROFILE_DIR_SCAN_ENTRIES;
use super::schema::profile_name_from_path;
use super::store::ProfileCatalog;

/// The last parsed catalog and the listing it was parsed from.
#[cfg(not(test))]
static CACHE: Mutex<Option<CachedCatalog>> = Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// Isolate test observations and cache eviction from other test threads.
    static CACHE: Mutex<Option<CachedCatalog>> = const { Mutex::new(None) };
    static CATALOG_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn with_cache<T>(operation: impl FnOnce(&Mutex<Option<CachedCatalog>>) -> T) -> T {
    #[cfg(test)]
    {
        CACHE.with(operation)
    }
    #[cfg(not(test))]
    {
        operation(&CACHE)
    }
}

struct CachedCatalog {
    dir: PathBuf,
    listing: Vec<Candidate>,
    truncated: bool,
    catalog: ProfileCatalog,
}

/// One `*.profile.json` regular file found in the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) path: PathBuf,
    pub(super) name: String,
    stamp: FileStamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    unix: (u64, i64, i64, u32),
    /// Creation time (100 ns intervals) and file attributes.
    #[cfg(windows)]
    windows: (u64, u32),
}

impl FileStamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            unix: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                    metadata.mode(),
                )
            },
            #[cfg(windows)]
            windows: {
                use std::os::windows::fs::MetadataExt;
                (metadata.creation_time(), metadata.file_attributes())
            },
        }
    }
}

/// The bounded result of listing a profile directory.
pub(super) struct Listing {
    /// Candidate files sorted by file name, so load order is stable.
    pub(super) candidates: Vec<Candidate>,
    /// More than [`MAX_PROFILE_DIR_SCAN_ENTRIES`] entries were present.
    pub(super) truncated: bool,
}

/// List `dir`, examining at most [`MAX_PROFILE_DIR_SCAN_ENTRIES`] entries of
/// any kind. Only regular files (following a file symlink, as before) named
/// `*.profile.json` become candidates.
pub(super) fn list_profile_dir(dir: &Path) -> io::Result<Listing> {
    let mut candidates = Vec::new();
    let mut truncated = false;
    for (examined, entry) in fs::read_dir(dir)?.enumerate() {
        if examined >= MAX_PROFILE_DIR_SCAN_ENTRIES {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Some(name) = profile_name_from_path(&path) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        candidates.push(Candidate {
            path,
            name,
            stamp: FileStamp::of(&metadata),
        });
    }
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Listing {
        candidates,
        truncated,
    })
}

/// The cached catalog for `dir` when `listing` matches the one it was parsed
/// from.
pub(super) fn cached(dir: &Path, listing: &Listing) -> Option<ProfileCatalog> {
    with_cache(|cache| {
        let cache = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cached = cache.as_ref()?;
        (cached.dir == dir
            && cached.truncated == listing.truncated
            && cached.listing == listing.candidates)
            .then(|| cached.catalog.clone())
    })
}

/// Remember `catalog` as parsed from `listing`.
pub(super) fn store(dir: &Path, listing: Listing, catalog: &ProfileCatalog) {
    #[cfg(test)]
    CATALOG_PARSE_COUNT.with(|count| count.set(count.get() + 1));
    with_cache(|cache| {
        *cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedCatalog {
            dir: dir.to_owned(),
            listing: listing.candidates,
            truncated: listing.truncated,
            catalog: catalog.clone(),
        });
    });
}

/// Catalogs parsed on the current test thread (cache misses).
#[cfg(test)]
pub fn catalog_parse_count_for_test() -> usize {
    CATALOG_PARSE_COUNT.with(std::cell::Cell::get)
}

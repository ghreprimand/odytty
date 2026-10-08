// SPDX-License-Identifier: GPL-3.0-only
//! Font-file discovery: where to look, what counts as a font file, and how
//! family and stem names are normalized for comparison.
//!
//! This layer only finds and names candidate files. Deciding what a file
//! actually contains is [`super::face_meta`]'s job, and choosing between
//! candidates is [`super::resolve`]'s.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One bounded, lazily collected view of a caller-owned set of font roots.
///
/// Native startup passes one inventory through symbol and color-emoji fallback
/// resolution, so those consumers share a single tree walk. Explicit/custom
/// directory callers create their own inventory, keeping their result scoped
/// to that operation rather than freezing host state in a process-global cache.
pub(crate) struct FontFileInventory {
    dirs: Vec<PathBuf>,
    files: OnceLock<Vec<PathBuf>>,
    #[cfg(test)]
    collections: std::cell::Cell<usize>,
}

impl FontFileInventory {
    pub(crate) fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            files: OnceLock::new(),
            #[cfg(test)]
            collections: std::cell::Cell::new(0),
        }
    }

    pub(crate) fn files(&self) -> &[PathBuf] {
        self.files.get_or_init(|| {
            #[cfg(test)]
            self.collections.set(self.collections.get() + 1);
            collect_font_files(&self.dirs)
        })
    }

    #[cfg(test)]
    pub(crate) fn collection_count(&self) -> usize {
        self.collections.get()
    }
}

/// Standard platform font search roots, plus per-user font dirs when available.
/// Only existing directories are returned. Used by settings and native startup
/// font resolution; tests pass explicit dirs instead for hermeticity.
pub fn font_search_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let mut dirs = vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
    ];
    #[cfg(windows)]
    let mut dirs = {
        let mut dirs = Vec::new();
        if let Some(windir) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(windir).join("Fonts"));
        }
        if let Some(local_appdata) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(
                PathBuf::from(local_appdata)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
        dirs
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut dirs = linux_font_roots(
        std::env::var_os("HOME").as_deref().map(Path::new),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("XDG_DATA_DIRS").as_deref(),
    );
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Library/Fonts"));
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// Candidate roots with explicit environment inputs for portable Linux tests.
/// Reversed XDG system order keeps the first configured directory higher
/// priority, because the scan visits the last root first.
#[cfg(not(any(target_os = "macos", windows)))]
fn linux_font_roots(
    home: Option<&Path>,
    data_home: Option<&std::ffi::OsStr>,
    data_dirs: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
        PathBuf::from("/run/current-system/sw/share/fonts"),
        PathBuf::from("/run/current-system/profile/share/fonts"),
    ];
    if let Some(home) = home {
        roots.push(home.join(".nix-profile/share/fonts"));
        roots.push(home.join(".guix-profile/share/fonts"));
    }
    // Environment strings can name many roots; keep discovery's setup bounded
    // as well as its directory walk.
    let mut system: Vec<PathBuf> = data_dirs
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|path| path.is_absolute())
        .take(64)
        .collect();
    if system.is_empty() {
        system = vec![
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share"),
        ];
    }
    for path in system.into_iter().rev().map(|path| path.join("fonts")) {
        // Move an explicitly configured standard root to its XDG priority.
        roots.retain(|root| *root != path);
        roots.push(path);
    }
    let user_data = data_home
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".local/share")));
    if let Some(data) = user_data {
        roots.push(data.join("fonts"));
    }
    if let Some(home) = home {
        roots.push(home.join(".fonts"));
    }
    roots
}

/// Lowercased alphanumeric-only form of a family/stem name, so "DejaVu Sans
/// Mono" and "DejaVuSansMono" compare equal.
pub(super) fn normalize_family(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// `(bold, italic)` flags inferred from a normalized stem.
pub(super) fn variant_flags(normalized_stem: &str) -> (bool, bool) {
    let bold = normalized_stem.contains("bold");
    let italic = normalized_stem.contains("italic") || normalized_stem.contains("oblique");
    (bold, italic)
}

/// Whether a path has a `.ttf`/`.otf`/`.ttc` extension (case-insensitive).
pub(super) fn has_font_ext(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("ttf") | Some("otf") | Some("ttc")
    )
}

/// File stem (name without extension) as a lossy string.
pub(super) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Bounds on one font-directory scan.
#[derive(Debug, Clone, Copy)]
pub(super) struct FontScanLimits {
    /// Maximum directory depth below each font root.
    pub(super) depth: usize,
    /// Maximum font files collected.
    pub(super) files: usize,
    /// Maximum directory entries examined across the whole scan, fonts or not.
    pub(super) entries: usize,
    /// Maximum directories read across the whole scan.
    pub(super) dirs: usize,
}

impl FontScanLimits {
    pub(super) const DEFAULT: Self = Self {
        depth: 6,
        files: 20_000,
        entries: 100_000,
        dirs: 10_000,
    };
}

/// Bounded recursive collection of font files under `dirs`. See
/// [`collect_font_files_bounded`].
pub(super) fn collect_font_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    collect_font_files_bounded(dirs, FontScanLimits::DEFAULT).files
}

/// Font files found by a bounded scan, and whether a bound stopped it early.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct FontScan {
    pub(super) files: Vec<PathBuf>,
    pub(super) truncated: bool,
}

/// Collect font files under `dirs`, bounded in depth, in files kept, in
/// entries examined of any kind, and in directories read (see
/// [`FontScanLimits::DEFAULT`]), so a large tree of non-font files cannot
/// stall startup or font selection. A scan that hits a bound logs one warning with
/// the counts (no paths) and reports `truncated`.
///
/// A symlink counts when it points at a regular font file, the same rule an
/// explicit font path follows; symlinks to directories are not followed, so a
/// link cannot loop the scan or pull in a tree outside the roots. Each
/// directory's admitted entries are visited in name order and the last root is
/// read first (per-user directories come after the system ones). If the entry
/// limit cuts a directory short, its admitted prefix follows filesystem
/// enumeration order and can differ between filesystems. Canonical roots and
/// files are visited only once, so aliases do not consume the same budget twice.
pub(super) fn collect_font_files_bounded(dirs: &[PathBuf], limits: FontScanLimits) -> FontScan {
    let mut scan = FontScan::default();
    let mut examined = 0usize;
    let mut dirs_read = 0usize;
    let mut visited_dirs = HashSet::new();
    let mut visited_files = HashSet::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    'dirs: while let Some((dir, depth)) = stack.pop() {
        let canonical = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if visited_dirs.contains(&canonical) {
            continue;
        }
        if depth > limits.depth {
            scan.truncated = true;
            continue;
        }
        if dirs_read >= limits.dirs {
            scan.truncated = true;
            break;
        }
        dirs_read += 1;
        visited_dirs.insert(canonical);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut listed = Vec::new();
        let mut entry_limit_hit = false;
        for entry in entries {
            if examined >= limits.entries {
                scan.truncated = true;
                entry_limit_hit = true;
                break;
            }
            examined += 1;
            if let Ok(entry) = entry {
                listed.push(entry);
            }
        }
        listed.sort_by_key(std::fs::DirEntry::file_name);
        let mut subdirs = Vec::new();
        for entry in listed {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                subdirs.push((path, depth + 1));
            } else if has_font_ext(&path)
                && (ft.is_file()
                    || (ft.is_symlink()
                        && std::fs::metadata(&path).is_ok_and(|meta| meta.is_file())))
            {
                let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if !visited_files.insert(canonical) {
                    continue;
                }
                if scan.files.len() >= limits.files {
                    scan.truncated = true;
                    break 'dirs;
                }
                scan.files.push(path);
            }
        }
        // Pushed in reverse so the stack pops them in name order.
        stack.extend(subdirs.into_iter().rev());
        if entry_limit_hit {
            break;
        }
    }
    if scan.truncated {
        tracing::warn!(
            files = scan.files.len(),
            examined,
            dirs_read,
            "font discovery stopped at its scan bound; some fonts may be missing"
        );
    }
    scan
}

#[cfg(test)]
#[path = "discovery_root_tests.rs"]
mod root_tests;

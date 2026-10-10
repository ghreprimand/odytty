// SPDX-License-Identifier: GPL-3.0-only
//! Library-side desktop-integration logic for the "Open With…" app picker
//! (C3b). Std-only and free of any windowing/GPU import (the SPEC layering
//! rule), so it is unit-testable on synthetic fixtures with **zero** real
//! filesystem and **zero** real `xdg-mime` invocation.
//!
//! The feature has three pure pieces, all here or in the sibling modules:
//! * [`exec::exec_to_argv`] - the security spine: a `.desktop` `Exec=` string →
//!   an argv vector, never a shell command.
//! * [`parse`] - hand parsers for `.desktop` / `mimeapps.list` /
//!   `mimeinfo.cache`.
//! * [`enumerate_open_with`] - resolves the apps that can open a file, behind
//!   two injectable seams ([`MimeProbe`] + [`DesktopEnv`]) so production wires
//!   the real `xdg-mime` + `std::fs` and tests wire in-memory maps.
//!
//! The production seam implementations live in `native/` (they touch the real
//! process/filesystem); this module stays pure.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

mod code_args;
mod exec;
mod macos_apps;
mod parse;

pub use exec::exec_to_argv;
pub use macos_apps::map_macos_app_paths;

/// Maximum apps offered in the picker, applied after dedup (keeps the overlay
/// compact and the fuzzy ranking bounded regardless of how many handlers a MIME
/// type has). Mirrors the `MAX_RESULTS` discipline of the other list overlays.
pub const MAX_OPEN_WITH: usize = 12;

/// One application that can open the target file, ready for the picker. `name`
/// is the human label (the `.desktop` `Name`, control-char-sanitized by the
/// overlay at render time like session titles); `argv` is the fully-expanded,
/// argv-only command (program + arguments, path already substituted as a single
/// inert element) handed verbatim to the shared `spawn_detached`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    /// The desktop id (`eog.desktop`), used for dedup and as a `Name` fallback.
    pub id: String,
    /// The display label (the entry's `Name`, or the id stem when absent).
    pub name: String,
    /// The fully-expanded argv to launch this app on the target file.
    pub argv: Vec<String>,
}

/// MIME-type detection seam. Production shells `xdg-mime query filetype <abs>`
/// (captured output, the one new spawn shape, behind a single audited helper);
/// tests return a fixed MIME from a map. Implementors must NEVER spawn under the
/// test target.
pub trait MimeProbe {
    /// The MIME type of `abs` (e.g. `image/png`), or `None` when detection fails
    /// (missing `xdg-mime`, non-zero exit, empty output). `None` → empty picker.
    fn query(&self, abs: &str) -> Option<String>;
}

/// Filesystem + XDG-environment seam for the enumeration. Production reads real
/// `std::fs` and the real `XDG_*` env ladder (bounded reads); tests supply an
/// in-memory `HashMap` fs map and synthetic dir lists. Keeping every read behind
/// this trait is what lets the resolution logic be tested with no real fs.
pub trait DesktopEnv {
    /// XDG config dirs in precedence order: `$XDG_CONFIG_HOME` (default
    /// `~/.config`) then each `$XDG_CONFIG_DIRS` (default `/etc/xdg`). Used to
    /// locate `mimeapps.list`.
    fn config_dirs(&self) -> Vec<PathBuf>;
    /// XDG data dirs in precedence order: `$XDG_DATA_HOME` (default
    /// `~/.local/share`) then each `$XDG_DATA_DIRS` (default
    /// `/usr/local/share:/usr/share`). The `applications/` subdir of each holds
    /// the `.desktop` files and `mimeinfo.cache`.
    fn data_dirs(&self) -> Vec<PathBuf>;
    /// Read a file's text, or `None` if it is missing/unreadable. The production
    /// impl bounds the read; tests return map entries. Never panics.
    fn read_file(&self, path: &Path) -> Option<String>;
}

/// Resolve the applications that can open `abs`, best-first, for the "Open With…"
/// picker (C3b §1). Pure aside from the injected seams:
///
/// 1. `mime = probe.query(abs)`; `None` → empty list (graceful).
/// 2. Collect candidate desktop ids in priority order: `mimeapps.list`
///    `[Default Applications]` then `[Added Associations]` across the config
///    ladder, then `mimeinfo.cache` `[MIME Cache]` across the data ladder.
///    Apply `[Removed Associations]` only at their own or lower precedence.
///    Dedup preserving first occurrence.
/// 3. Resolve each id to its `.desktop` file across the data ladder (user dir
///    wins; subdir-prefixed `kde-foo.desktop` → `applications/kde/foo.desktop`).
///    Parse + filter (`Type=Application`, not NoDisplay/Hidden/Terminal, has
///    Exec). Expand `Exec` to argv with [`exec_to_argv`].
/// 4. Cap at [`MAX_OPEN_WITH`].
///
/// Any malformed/missing input is skipped, never an error.
pub fn enumerate_open_with(
    probe: &dyn MimeProbe,
    env: &dyn DesktopEnv,
    abs: &str,
) -> Vec<DesktopApp> {
    let Some(mime) = probe.query(abs).filter(|m| !m.trim().is_empty()) else {
        return Vec::new();
    };
    let mime = mime.trim();

    let config_dirs = env.config_dirs();
    let data_dirs = env.data_dirs();

    // --- Step 2: ordered candidate ids + removed set ------------------------
    let mut removed: HashMap<String, usize> = HashMap::new();
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // mimeapps.list lives in each config dir, and (legacy) in each data dir's
    // applications/ subdir. Read both ladders for associations.
    let mut mimeapps_texts: Vec<(usize, String)> = Vec::new();
    for (rank, dir) in config_dirs.iter().enumerate() {
        if let Some(text) = env.read_file(&dir.join("mimeapps.list")) {
            mimeapps_texts.push((rank, text));
        }
    }
    for (rank, dir) in data_dirs.iter().enumerate() {
        if let Some(text) = env.read_file(&dir.join("applications").join("mimeapps.list")) {
            mimeapps_texts.push((config_dirs.len() + rank, text));
        }
    }

    // Retain the highest-priority removal for each id. A lower-priority file
    // cannot cancel a handler already associated higher in the ladder.
    for (rank, text) in &mimeapps_texts {
        for id in parse::parse_association_list(text, "Removed Associations", mime) {
            removed.entry(id).or_insert(*rank);
        }
    }

    let push_id =
        |id: String, rank: usize, ordered: &mut Vec<String>, seen: &mut HashSet<String>| {
            if removed
                .get(&id)
                .is_some_and(|removed_rank| *removed_rank <= rank)
                || seen.contains(&id)
            {
                return;
            }
            seen.insert(id.clone());
            ordered.push(id);
        };

    // Defaults first (highest priority), then added associations.
    for (rank, text) in &mimeapps_texts {
        for id in parse::parse_association_list(text, "Default Applications", mime) {
            push_id(id, *rank, &mut ordered, &mut seen);
        }
    }
    for (rank, text) in &mimeapps_texts {
        for id in parse::parse_association_list(text, "Added Associations", mime) {
            push_id(id, *rank, &mut ordered, &mut seen);
        }
    }
    // Then the registered handlers from mimeinfo.cache in each applications dir.
    for (rank, dir) in data_dirs.iter().enumerate() {
        let cache = dir.join("applications").join("mimeinfo.cache");
        if let Some(text) = env.read_file(&cache) {
            for id in parse::parse_association_list(&text, "MIME Cache", mime) {
                push_id(id, config_dirs.len() + rank, &mut ordered, &mut seen);
            }
        }
    }

    // --- Step 3: resolve each id to a .desktop, parse, filter, expand -------
    let mut apps: Vec<DesktopApp> = Vec::new();
    for id in ordered {
        let Some(text) = read_desktop_file(env, &data_dirs, &id) else {
            continue;
        };
        let entry = parse::parse_desktop_entry(&text);
        if !entry.is_offerable() {
            continue;
        }
        let Some(exec) = entry.exec.as_deref() else {
            continue;
        };
        // An entry whose field codes sit in a refused context (inside quotes,
        // `%F`/`%U` inside a longer argument, or any code in the program
        // position) or that has no program token is not offered.
        let Some(argv) = exec_to_argv(exec, abs) else {
            continue;
        };
        let name = entry
            .name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| id.trim_end_matches(".desktop").to_owned());
        apps.push(DesktopApp { id, name, argv });
        if apps.len() >= MAX_OPEN_WITH {
            break;
        }
    }
    apps
}

/// Read a desktop id's `.desktop` file across the data ladder (user dir first).
/// Tries the literal `applications/<id>` first, then the subdir form for a
/// dash-prefixed id (`kde-foo.desktop` → `applications/kde/foo.desktop`).
fn read_desktop_file(env: &dyn DesktopEnv, data_dirs: &[PathBuf], id: &str) -> Option<String> {
    // F17: a desktop id is a BARE name (dashes encode subdirectories); it must
    // never resolve outside `applications/`. `Path::join` does not normalize, so
    // an id carrying a separator, a `..` component, or an absolute path (e.g. a
    // hostile `mimeapps.list` entry `image/png=../../../../tmp/evil.desktop`)
    // would otherwise read, and if launched run, an arbitrary out-of-tree
    // file. The original id is rejected here, and every candidate derived from
    // it by dash expansion is admitted only when each of its components is a
    // normal name (see `desktop_relpaths`), so `..-evil.desktop` cannot read
    // `applications/../evil.desktop` either.
    if !is_safe_desktop_id(id) {
        return None;
    }
    for dir in data_dirs {
        let apps = dir.join("applications");
        for rel in desktop_relpaths(id) {
            if let Some(text) = env.read_file(&apps.join(&rel)) {
                return Some(text);
            }
        }
    }
    None
}

/// Whether a desktop id is safe to resolve under `applications/`: a non-empty
/// bare name with no path separator, no `..` component, no NUL, and not
/// absolute. Dashes (which the resolver expands to subdirectories) are fine.
fn is_safe_desktop_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
        && id != ".."
        && id != "."
        && !Path::new(id).is_absolute()
}

/// Most dashes of one id that are expanded as independent path separators. Each
/// dash is either a separator or part of a directory or file name, so an id with
/// `n` dashes has up to `2^n` candidates; beyond this many dashes only the
/// progressive prefix ladder (the first `k` dashes become separators) is tried,
/// which bounds the reads one hostile id can cause.
const MAX_INDEPENDENT_DASHES: usize = 6;

/// Candidate relative paths (under `applications/`) for a desktop id: the
/// literal name first, then the subdirectory forms.
///
/// C15: the freedesktop desktop-entry spec derives a file's id by replacing
/// every path separator under `applications/` with `-`, so resolution must
/// walk the ladder in reverse - `org-gnome-eog.desktop` may live at
/// `org-gnome-eog.desktop`, `org/gnome-eog.desktop`, or `org/gnome/eog.desktop`.
/// A directory or file name may itself contain a dash, so
/// `foo-bar-editor.desktop` may also live at `foo-bar/editor.desktop`.
///
/// Candidates are ordered by how many dashes become separators (none first, so
/// the literal name wins a tie, matching the id-priority convention) and, within
/// one count, by the earliest dashes first. An id with more than
/// [`MAX_INDEPENDENT_DASHES`] dashes gets only the progressive form for each
/// count.
///
/// A derived candidate is produced only when every `/`-separated component is a
/// nonempty normal name: a dash next to `..` or `.` would derive a parent or
/// current-directory component, a leading dash an absolute path, and a doubled
/// or trailing dash an empty component that resolves a different id. A refused
/// form is skipped; later forms are checked independently.
fn desktop_relpaths(id: &str) -> Vec<String> {
    let dashes: Vec<usize> = id.match_indices('-').map(|(at, _)| at).collect();
    let mut out = vec![id.to_owned()];
    let derive = |chosen: &[usize]| {
        let mut candidate = id.to_owned();
        for &at in chosen {
            candidate.replace_range(at..=at, "/");
        }
        candidate
    };
    if dashes.len() <= MAX_INDEPENDENT_DASHES {
        let mut masks: Vec<u32> = (1..(1u32 << dashes.len())).collect();
        // Fewest separators first; the sort is stable over ascending masks, so
        // within a count the earliest dashes come first.
        masks.sort_by_key(|mask| mask.count_ones());
        for mask in masks {
            let chosen: Vec<usize> = (0..dashes.len())
                .filter(|bit| mask & (1 << bit) != 0)
                .map(|bit| dashes[bit])
                .collect();
            let candidate = derive(&chosen);
            if is_contained_relpath(&candidate) {
                out.push(candidate);
            }
        }
    } else {
        for count in 1..=dashes.len() {
            let candidate = derive(&dashes[..count]);
            if !is_contained_relpath(&candidate) {
                break;
            }
            out.push(candidate);
        }
    }
    out
}

/// Whether a derived relative path stays inside `applications/`: every
/// `/`-separated segment is a nonempty name other than `.` or `..` (checked
/// on the text, because `Path::components` silently drops an interior `.`),
/// and the path parses as normal components only (no root or prefix).
fn is_contained_relpath(rel: &str) -> bool {
    rel.split('/')
        .all(|segment| !matches!(segment, "" | "." | ".."))
        && Path::new(rel)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Synthetic MIME probe: a fixed `path → mime` map. NEVER spawns `xdg-mime`.
    struct MapMimeProbe(HashMap<String, String>);
    impl MimeProbe for MapMimeProbe {
        fn query(&self, abs: &str) -> Option<String> {
            self.0.get(abs).cloned()
        }
    }

    /// Synthetic desktop environment: in-memory fs map + fixed dir ladders. NO
    /// real `~/.local/share`, NO real `/usr/share`, no real filesystem at all.
    struct MapEnv {
        config_dirs: Vec<PathBuf>,
        data_dirs: Vec<PathBuf>,
        files: HashMap<PathBuf, String>,
    }
    impl DesktopEnv for MapEnv {
        fn config_dirs(&self) -> Vec<PathBuf> {
            self.config_dirs.clone()
        }
        fn data_dirs(&self) -> Vec<PathBuf> {
            self.data_dirs.clone()
        }
        fn read_file(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }
    }

    fn probe(mime: &str) -> MapMimeProbe {
        let mut m = HashMap::new();
        m.insert("/x/a.png".to_owned(), mime.to_owned());
        MapMimeProbe(m)
    }

    fn desktop(name: &str, exec: &str) -> String {
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n")
    }

    #[test]
    fn empty_when_mime_unknown() {
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![],
            files: HashMap::new(),
        };
        // A probe that knows nothing → empty list, no panic.
        let probe = MapMimeProbe(HashMap::new());
        assert!(enumerate_open_with(&probe, &env, "/x/a.png").is_empty());
    }

    #[test]
    fn resolves_default_then_cache_with_dedup() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/cfg/mimeapps.list"),
            "[Default Applications]\nimage/png=eog.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=eog.desktop;gimp.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/eog.desktop"),
            desktop("Image Viewer", "eog %f"),
        );
        files.insert(
            PathBuf::from("/data/applications/gimp.desktop"),
            desktop("GIMP", "gimp %F"),
        );
        let env = MapEnv {
            config_dirs: vec![PathBuf::from("/cfg")],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        // eog appears in BOTH default and cache → deduped, default order wins.
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Image Viewer");
        assert_eq!(apps[0].argv, vec!["eog".to_owned(), "/x/a.png".to_owned()]);
        assert_eq!(apps[1].name, "GIMP");
    }

    #[test]
    fn desktop_id_path_traversal_is_rejected() {
        // F17: guard predicate - bare names (dashes ok) pass; anything that could
        // escape `applications/` is rejected.
        assert!(is_safe_desktop_id("firefox.desktop"));
        assert!(is_safe_desktop_id("org-gnome-eog.desktop"));
        assert!(!is_safe_desktop_id("../evil.desktop"));
        assert!(!is_safe_desktop_id("../../tmp/evil.desktop"));
        assert!(!is_safe_desktop_id(".."));
        assert!(!is_safe_desktop_id("a/b.desktop"));
        assert!(!is_safe_desktop_id("/etc/passwd"));
        assert!(!is_safe_desktop_id(""));

        // Behavioral: a hostile file planted where a naive `join` with a
        // traversal id would land is NOT read, because the id is rejected before
        // it reaches the data ladder; a legitimate id still resolves.
        let apps = PathBuf::from("/data/applications");
        let mut files = HashMap::new();
        files.insert(
            apps.join("../../tmp/evil.desktop"),
            desktop("Evil", "/bin/evil %f"),
        );
        files.insert(
            apps.join("firefox.desktop"),
            desktop("Firefox", "firefox %U"),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let data_dirs = env.data_dirs();
        assert!(
            read_desktop_file(&env, &data_dirs, "../../tmp/evil.desktop").is_none(),
            "a traversal desktop id must be rejected"
        );
        assert!(
            read_desktop_file(&env, &data_dirs, "firefox.desktop").is_some(),
            "a legitimate desktop id still resolves"
        );
    }

    #[test]
    fn default_beats_cache_ordering() {
        // gimp is the registered cache handler but eog is the user default;
        // the default must rank first.
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/cfg/mimeapps.list"),
            "[Default Applications]\nimage/png=eog.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=gimp.desktop;eog.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/eog.desktop"),
            desktop("EOG", "eog %f"),
        );
        files.insert(
            PathBuf::from("/data/applications/gimp.desktop"),
            desktop("GIMP", "gimp %f"),
        );
        let env = MapEnv {
            config_dirs: vec![PathBuf::from("/cfg")],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps[0].name, "EOG");
        assert_eq!(apps[1].name, "GIMP");
    }

    #[test]
    fn user_data_dir_overrides_system() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=eog.desktop;\n".to_owned(),
        );
        // Same id in both user (/home) and system (/usr) data dirs; the user
        // copy must win.
        files.insert(
            PathBuf::from("/home/applications/eog.desktop"),
            desktop("User EOG", "user-eog %f"),
        );
        files.insert(
            PathBuf::from("/usr/applications/eog.desktop"),
            desktop("System EOG", "system-eog %f"),
        );
        // mimeinfo.cache present in the system dir only is fine; the id resolves
        // against the data ladder (user first).
        files.insert(
            PathBuf::from("/usr/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=eog.desktop;\n".to_owned(),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/home"), PathBuf::from("/usr")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "User EOG");
        assert_eq!(apps[0].argv[0], "user-eog");
    }

    #[test]
    fn removed_associations_subtract() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/cfg/mimeapps.list"),
            "[Added Associations]\nimage/png=eog.desktop;gimp.desktop;\n\
             [Removed Associations]\nimage/png=gimp.desktop;\n"
                .to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/eog.desktop"),
            desktop("EOG", "eog %f"),
        );
        files.insert(
            PathBuf::from("/data/applications/gimp.desktop"),
            desktop("GIMP", "gimp %f"),
        );
        let env = MapEnv {
            config_dirs: vec![PathBuf::from("/cfg")],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "EOG");
    }

    #[test]
    fn subdir_prefixed_id_resolves() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=kde-okular.desktop;\n".to_owned(),
        );
        // The dash-prefixed id maps to the kde/ subdirectory.
        files.insert(
            PathBuf::from("/data/applications/kde/okular.desktop"),
            desktop("Okular", "okular %f"),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Okular");
    }

    /// C15: a multi-dash id resolves through the FULL progressive dash→slash
    /// ladder - `org-gnome-eog.desktop` at `applications/org/gnome/eog.desktop`.
    /// Pre-fix only the first dash split, so the two-level nesting never
    /// resolved.
    #[test]
    fn multi_dash_id_resolves_nested_subdirs() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=org-gnome-eog.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/org/gnome/eog.desktop"),
            desktop("Eye of GNOME", "eog %f"),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Eye of GNOME");
    }

    #[test]
    fn derived_desktop_candidates_admit_only_normal_components() {
        // A dash next to `..` or `.` derives a parent or current-directory
        // component, a leading dash an absolute path, and a doubled or
        // trailing dash an empty component that would resolve a different id;
        // such candidates are never produced, while the literal name and
        // every all-normal form still are.
        assert_eq!(
            desktop_relpaths("..-outside.desktop"),
            vec!["..-outside.desktop"]
        );
        assert_eq!(
            desktop_relpaths("kde-..-..-evil.desktop"),
            vec![
                "kde-..-..-evil.desktop",
                "kde/..-..-evil.desktop",
                "kde-../..-evil.desktop",
                "kde-..-../evil.desktop",
                "kde/..-../evil.desktop"
            ]
        );
        assert_eq!(
            desktop_relpaths("a-.-b.desktop"),
            vec!["a-.-b.desktop", "a/.-b.desktop", "a-./b.desktop"]
        );
        assert_eq!(
            desktop_relpaths("a--b.desktop"),
            vec!["a--b.desktop", "a/-b.desktop", "a-/b.desktop"]
        );
        assert_eq!(desktop_relpaths("-lead.desktop"), vec!["-lead.desktop"]);
        assert_eq!(desktop_relpaths("trail-"), vec!["trail-"]);
        assert_eq!(
            desktop_relpaths("foo-bar-editor.desktop"),
            vec![
                "foo-bar-editor.desktop",
                "foo/bar-editor.desktop",
                "foo-bar/editor.desktop",
                "foo/bar/editor.desktop"
            ]
        );
        for id in [
            "..-outside.desktop",
            "x-..-y.desktop",
            "-lead.desktop",
            "trail-",
            "x-.-y.desktop",
            "x---y.desktop",
        ] {
            for rel in desktop_relpaths(id) {
                assert!(
                    Path::new(&rel)
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                        && rel.split('/').all(|s| !matches!(s, "" | "." | "..")),
                    "{id} derived {rel}"
                );
            }
        }

        // Behavioral: a file planted at the parent-directory candidate is
        // never read.
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/../outside.desktop"),
            desktop("Outside", "outside %f"),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let data_dirs = env.data_dirs();
        assert!(read_desktop_file(&env, &data_dirs, "..-outside.desktop").is_none());
    }

    /// C15: the candidate ladder is literal first, then by how many dashes become
    /// separators - so a literally-installed dash-named file wins over a nested
    /// twin, and a directory name that itself holds a dash is reachable.
    #[test]
    fn desktop_relpaths_ladder_is_progressive() {
        assert_eq!(desktop_relpaths("foo.desktop"), vec!["foo.desktop"]);
        assert_eq!(
            desktop_relpaths("kde-foo.desktop"),
            vec!["kde-foo.desktop", "kde/foo.desktop"]
        );
        assert_eq!(
            desktop_relpaths("org-gnome-eog.desktop"),
            vec![
                "org-gnome-eog.desktop",
                "org/gnome-eog.desktop",
                "org-gnome/eog.desktop",
                "org/gnome/eog.desktop"
            ]
        );
    }

    /// An id with more dashes than [`MAX_INDEPENDENT_DASHES`] cannot multiply
    /// into hundreds of reads: only the progressive prefix forms remain.
    #[test]
    fn desktop_relpaths_bound_independent_dash_choices() {
        let few = format!("{}x.desktop", "a-".repeat(MAX_INDEPENDENT_DASHES));
        assert_eq!(
            desktop_relpaths(&few).len(),
            1 << MAX_INDEPENDENT_DASHES,
            "every subset of dashes is a candidate up to the bound"
        );
        let many = format!("{}x.desktop", "a-".repeat(MAX_INDEPENDENT_DASHES + 1));
        let rels = desktop_relpaths(&many);
        assert_eq!(rels.len(), MAX_INDEPENDENT_DASHES + 2);
        assert_eq!(rels[0], many);
        assert_eq!(rels[1], many.replacen('-', "/", 1));
        assert_eq!(rels[MAX_INDEPENDENT_DASHES + 1], many.replace('-', "/"));
    }

    #[test]
    fn terminal_and_nodisplay_apps_are_filtered() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=good.desktop;term.desktop;hidden.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/good.desktop"),
            desktop("Good", "good %f"),
        );
        files.insert(
            PathBuf::from("/data/applications/term.desktop"),
            "[Desktop Entry]\nType=Application\nName=Term\nExec=t %f\nTerminal=true\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=H\nExec=h %f\nNoDisplay=true\n".to_owned(),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Good");
    }

    #[test]
    fn missing_desktop_file_is_skipped() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=ghost.desktop;real.desktop;\n".to_owned(),
        );
        // ghost.desktop is referenced but absent → skipped, real survives.
        files.insert(
            PathBuf::from("/data/applications/real.desktop"),
            desktop("Real", "real %f"),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Real");
    }

    #[test]
    fn entries_with_refused_field_codes_are_not_offered() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=shell.desktop;list.desktop;plain.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/shell.desktop"),
            desktop("Shell", "sh -c \"eog %f\""),
        );
        files.insert(
            PathBuf::from("/data/applications/list.desktop"),
            desktop("List", "app --files=%F"),
        );
        files.insert(
            PathBuf::from("/data/applications/plain.desktop"),
            desktop("Plain", "\"/opt/Plain App/run\" %f"),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Plain");
        assert_eq!(
            apps[0].argv,
            vec!["/opt/Plain App/run".to_owned(), "/x/a.png".to_owned()]
        );
    }

    #[test]
    fn count_is_capped_at_max() {
        let mut cache = String::from("[MIME Cache]\nimage/png=");
        let mut files = HashMap::new();
        for i in 0..(MAX_OPEN_WITH + 5) {
            let id = format!("app{i}.desktop");
            cache.push_str(&id);
            cache.push(';');
            files.insert(
                PathBuf::from(format!("/data/applications/{id}")),
                desktop(&format!("App {i}"), &format!("app{i} %f")),
            );
        }
        cache.push('\n');
        files.insert(PathBuf::from("/data/applications/mimeinfo.cache"), cache);
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), MAX_OPEN_WITH);
    }

    #[test]
    fn name_falls_back_to_id_stem() {
        let mut files = HashMap::new();
        files.insert(
            PathBuf::from("/data/applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=noname.desktop;\n".to_owned(),
        );
        files.insert(
            PathBuf::from("/data/applications/noname.desktop"),
            "[Desktop Entry]\nType=Application\nExec=nn %f\n".to_owned(),
        );
        let env = MapEnv {
            config_dirs: vec![],
            data_dirs: vec![PathBuf::from("/data")],
            files,
        };
        let apps = enumerate_open_with(&probe("image/png"), &env, "/x/a.png");
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "noname");
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Library-side desktop-integration logic for the "Open With…" app picker
//! (C3b). Std-only and free of any windowing/GPU import (the SPEC layering
//! rule), so it is unit-testable on synthetic fixtures with **zero** real
//! filesystem and **zero** real `xdg-mime` invocation.
//!
//! The feature has three pure pieces, all here or in the sibling modules:
//! * [`exec::exec_to_argv`] — the security spine: a `.desktop` `Exec=` string →
//!   an argv vector, never a shell command.
//! * [`parse`] — hand parsers for `.desktop` / `mimeapps.list` /
//!   `mimeinfo.cache`.
//! * [`enumerate_open_with`] — resolves the apps that can open a file, behind
//!   two injectable seams ([`MimeProbe`] + [`DesktopEnv`]) so production wires
//!   the real `xdg-mime` + `std::fs` and tests wire in-memory maps.
//!
//! The production seam implementations live in `native/` (they touch the real
//! process/filesystem); this module stays pure.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
///    Subtract `[Removed Associations]`. Dedup preserving first occurrence.
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
    let mut removed: HashSet<String> = HashSet::new();
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // mimeapps.list lives in each config dir, and (legacy) in each data dir's
    // applications/ subdir. Read both ladders for associations.
    let mut mimeapps_texts: Vec<String> = Vec::new();
    for dir in &config_dirs {
        if let Some(text) = env.read_file(&dir.join("mimeapps.list")) {
            mimeapps_texts.push(text);
        }
    }
    for dir in &data_dirs {
        if let Some(text) = env.read_file(&dir.join("applications").join("mimeapps.list")) {
            mimeapps_texts.push(text);
        }
    }

    // Removed associations subtract regardless of order, so gather them first.
    for text in &mimeapps_texts {
        for id in parse::parse_association_list(text, "Removed Associations", mime) {
            removed.insert(id);
        }
    }

    let push_id = |id: String, ordered: &mut Vec<String>, seen: &mut HashSet<String>| {
        if removed.contains(&id) || seen.contains(&id) {
            return;
        }
        seen.insert(id.clone());
        ordered.push(id);
    };

    // Defaults first (highest priority), then added associations.
    for text in &mimeapps_texts {
        for id in parse::parse_association_list(text, "Default Applications", mime) {
            push_id(id, &mut ordered, &mut seen);
        }
    }
    for text in &mimeapps_texts {
        for id in parse::parse_association_list(text, "Added Associations", mime) {
            push_id(id, &mut ordered, &mut seen);
        }
    }
    // Then the registered handlers from mimeinfo.cache in each applications dir.
    for dir in &data_dirs {
        let cache = dir.join("applications").join("mimeinfo.cache");
        if let Some(text) = env.read_file(&cache) {
            for id in parse::parse_association_list(&text, "MIME Cache", mime) {
                push_id(id, &mut ordered, &mut seen);
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
        let argv = exec_to_argv(exec, abs);
        if argv.is_empty() {
            continue;
        }
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

/// Candidate relative paths (under `applications/`) for a desktop id: the
/// literal name first, then the progressive dash→slash subdirectory forms.
///
/// C15: the freedesktop desktop-entry spec derives a file's id by replacing
/// every path separator under `applications/` with `-`, so resolution must
/// walk the ladder in reverse — `org-gnome-eog.desktop` may live at
/// `org-gnome-eog.desktop`, `org/gnome-eog.desktop`, or `org/gnome/eog.desktop`.
/// Candidates convert the first `k` dashes to slashes for `k = 0..=n`,
/// shallowest first (the literal name wins a tie, matching the id-priority
/// convention).
fn desktop_relpaths(id: &str) -> Vec<String> {
    let mut out = vec![id.to_owned()];
    let mut candidate = id.to_owned();
    let mut from = 0;
    while let Some(pos) = candidate[from..].find('-') {
        let at = from + pos;
        candidate.replace_range(at..=at, "/");
        from = at + 1;
        out.push(candidate.clone());
    }
    out
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
    /// ladder — `org-gnome-eog.desktop` at `applications/org/gnome/eog.desktop`.
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

    /// C15: the candidate ladder is literal first, then progressively deeper —
    /// so a literally-installed dash-named file wins over a nested twin.
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
                "org/gnome/eog.desktop"
            ]
        );
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

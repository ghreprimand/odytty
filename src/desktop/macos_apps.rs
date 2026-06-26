// SPDX-License-Identifier: GPL-3.0-only
//! Pure mapper: macOS application-bundle paths → [`DesktopApp`] rows for the
//! "Open With…" picker (Phase 17). The macOS FFI in
//! `native::macos_open_with` queries `NSWorkspace` for the applications that
//! can open a file and hands the resulting bundle paths here; this function
//! turns them into the same `DesktopApp { id, name, argv }` shape the Linux
//! `enumerate_open_with` produces, so the picker overlay and `spawn_detached`
//! path stay platform-agnostic.
//!
//! It is deliberately host-agnostic and free of any FFI/`cfg` so it compiles
//! and unit-tests on Linux with synthetic `/Applications/Foo.app` inputs (the
//! v0.4.0 lesson: never let a macOS-only code path go unexercised). On Linux
//! the only production caller is the `cfg(target_os = "macos")` FFI, so the
//! function is dead outside its own tests there — hence the `dead_code` allow.

use super::{DesktopApp, MAX_OPEN_WITH};
use std::collections::HashSet;

/// Map macOS application-bundle paths (LaunchServices order, as returned by
/// `NSWorkspace::URLsForApplicationsToOpenURL`) to picker rows for `file_abs`.
///
/// * Each bundle path (e.g. `/Applications/Preview.app`) becomes one
///   [`DesktopApp`] whose `id` is the bundle path, `name` is the bundle's
///   basename with a trailing `.app` stripped (`Preview`), and `argv` is the
///   inert, argv-only launch command `["open", "-a", <bundle>, <file_abs>]`.
/// * Duplicate bundle paths are dropped, preserving the first (LaunchServices
///   preference) occurrence. Empty path strings are skipped.
/// * The result is capped at [`MAX_OPEN_WITH`], matching the Linux enumeration.
/// * Empty input → empty vec.
///
/// Pure and side-effect-free: no FFI, no filesystem, no spawning.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn map_macos_app_paths(app_paths: Vec<String>, file_abs: &str) -> Vec<DesktopApp> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<DesktopApp> = Vec::new();
    for path in app_paths {
        if path.is_empty() || !seen.insert(path.clone()) {
            continue;
        }
        let name = macos_app_name(&path);
        let argv = vec![
            "open".to_owned(),
            "-a".to_owned(),
            path.clone(),
            file_abs.to_owned(),
        ];
        out.push(DesktopApp {
            id: path,
            name,
            argv,
        });
        if out.len() >= MAX_OPEN_WITH {
            break;
        }
    }
    out
}

/// Human label for a `.app` bundle path: the final path component with a
/// trailing `.app` removed (`/Applications/Preview.app` → `Preview`). Falls
/// back to the basename (or the whole path) when stripping would empty it, so
/// the row always carries a non-empty label.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_app_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let stem = base.strip_suffix(".app").unwrap_or(base);
    if stem.is_empty() {
        base.to_owned()
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_empty() {
        assert!(map_macos_app_paths(Vec::new(), "/home/user/x.png").is_empty());
    }

    #[test]
    fn strips_app_suffix_for_name() {
        let apps = map_macos_app_paths(
            vec![
                "/Applications/Preview.app".to_owned(),
                "/Applications/GIMP.app".to_owned(),
            ],
            "/home/user/x.png",
        );
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Preview");
        assert_eq!(apps[1].name, "GIMP");
    }

    #[test]
    fn argv_is_open_dash_a_bundle_file() {
        let apps = map_macos_app_paths(
            vec!["/Applications/Preview.app".to_owned()],
            "/home/user/x.png",
        );
        assert_eq!(
            apps[0].argv,
            vec![
                "open".to_owned(),
                "-a".to_owned(),
                "/Applications/Preview.app".to_owned(),
                "/home/user/x.png".to_owned(),
            ]
        );
        // id is the bundle path (used for dedup + as a stable identifier).
        assert_eq!(apps[0].id, "/Applications/Preview.app");
    }

    #[test]
    fn duplicate_paths_dedup_preserving_first() {
        let apps = map_macos_app_paths(
            vec![
                "/Applications/Preview.app".to_owned(),
                "/Applications/GIMP.app".to_owned(),
                "/Applications/Preview.app".to_owned(),
            ],
            "/home/user/x.png",
        );
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Preview");
        assert_eq!(apps[1].name, "GIMP");
    }

    #[test]
    fn empty_path_strings_are_skipped() {
        let apps = map_macos_app_paths(
            vec![
                String::new(),
                "/Applications/Preview.app".to_owned(),
                String::new(),
            ],
            "/home/user/x.png",
        );
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Preview");
    }

    #[test]
    fn count_is_capped_at_max() {
        let paths: Vec<String> = (0..(MAX_OPEN_WITH + 3))
            .map(|i| format!("/Applications/App{i}.app"))
            .collect();
        let apps = map_macos_app_paths(paths, "/home/user/x.png");
        assert_eq!(apps.len(), MAX_OPEN_WITH);
        // The cap keeps the first-N (LaunchServices order), so App0 survives.
        assert_eq!(apps[0].name, "App0");
    }

    #[test]
    fn trailing_slash_and_no_suffix_still_label() {
        let apps = map_macos_app_paths(
            vec![
                "/Applications/Xcode.app/".to_owned(),
                "/usr/bin/vim".to_owned(),
            ],
            "/home/user/x.png",
        );
        assert_eq!(apps[0].name, "Xcode");
        // A non-bundle path keeps its basename verbatim (no .app to strip).
        assert_eq!(apps[1].name, "vim");
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! PLATFORM-OPENER — the per-OS file/URI open dispatch seam (P0-1).
//!
//! v0.4.0 hard-coded `xdg-open`/`xdg-mime`, neither of which exists on macOS, so
//! every "open" silently failed there — the headline v0.4.0 breakage. The fix
//! routes EVERY opener through one pure dispatch keyed by an explicit
//! [`OpenerOs`] ENUM rather than scattered `cfg!(target_os = …)` checks.
//!
//! Why an enum parameter and not raw `cfg!`: the root failure was that the macOS
//! argv was never exercised — only the Linux `cfg` arm ran on the Linux CI/dev
//! host. By threading the target OS as a *value*, production picks the host once
//! at the boundary ([`OpenerOs::host`]) while unit tests assert BOTH the Linux
//! and macOS argv on a single CI host. The macOS branch can never again go
//! unexercised.
//!
//! Everything here is argv-only and pure — these functions build a `Vec<String>`
//! and never spawn. The single spawn point stays
//! [`super::interactive_paths::spawn_detached`].

use crate::paths::{FsKind, Resolved};

/// The target operating system for opener dispatch. Production resolves the host
/// via [`OpenerOs::host`]; unit tests construct both variants explicitly so each
/// argv branch is asserted regardless of the runner OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native) enum OpenerOs {
    /// Linux / other free-desktop unixes: the `xdg-utils` family (`xdg-open`,
    /// `xdg-mime`).
    Linux,
    /// macOS: the `open` command (and, for reveal, `open -R`).
    Macos,
}

impl OpenerOs {
    /// The host OS, resolved by the single `cfg!(target_os = "macos")` selector
    /// in the codebase. macOS → [`OpenerOs::Macos`]; everything else →
    /// [`OpenerOs::Linux`] (OdyTTY targets Linux + macOS; any other unix uses
    /// the xdg path, which is the correct default on free-desktop systems).
    pub(in crate::native) fn host() -> Self {
        if cfg!(target_os = "macos") {
            OpenerOs::Macos
        } else {
            OpenerOs::Linux
        }
    }
}

/// Build the argv that opens `target` (an absolute path, or a `file://`/allowed
/// URI) with the system default handler on `os`. Pure — returns the vector; the
/// caller spawns it via [`super::interactive_paths::spawn_detached`].
///
/// * Linux → `["xdg-open", target]`
/// * macOS → `["open", target]`
///
/// `target` is always a single inert argv element — a path/URI containing `;`,
/// `$()`, backticks, or spaces is never shell-interpreted.
pub(in crate::native) fn open_default_argv(os: OpenerOs, target: &str) -> Vec<String> {
    match os {
        OpenerOs::Linux => vec!["xdg-open".to_owned(), target.to_owned()],
        OpenerOs::Macos => vec!["open".to_owned(), target.to_owned()],
    }
}

/// Build the argv that reveals `resolved` in the desktop file manager on `os`.
/// Pure.
///
/// * Linux → `["xdg-open", <parent dir of a file | the dir itself>]` — the file
///   manager opens showing the containing folder (there is no portable
///   "select this file" verb in `xdg-open`).
/// * macOS → `["open", "-R", <abs>]` — `open -R` takes the file ITSELF (not the
///   parent) and reveals it selected in Finder; for a directory it selects the
///   directory in its parent.
pub(in crate::native) fn reveal_argv(os: OpenerOs, resolved: &Resolved) -> Vec<String> {
    match os {
        OpenerOs::Linux => vec!["xdg-open".to_owned(), reveal_parent(resolved)],
        OpenerOs::Macos => vec!["open".to_owned(), "-R".to_owned(), resolved.abs.clone()],
    }
}

/// The Linux reveal target: a file's parent directory (so the file manager
/// opens showing the file's folder), or a directory itself. A root-level file
/// (`/foo`) reveals `/`. Pure. (macOS reveal passes the file itself to
/// `open -R`, so this helper is Linux-only.)
fn reveal_parent(resolved: &Resolved) -> String {
    match resolved.kind {
        FsKind::Dir => resolved.abs.clone(),
        FsKind::File => match resolved.abs.rfind('/') {
            Some(0) => "/".to_owned(),
            Some(idx) => resolved.abs[..idx].to_owned(),
            None => resolved.abs.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    //! Pure argv-construction tests. EVERY case asserts BOTH the Linux and the
    //! macOS argv on one host — that is the whole point of the enum parameter
    //! (the macOS path that v0.4.0 never exercised is now covered on Linux CI).
    //! Synthetic paths only — no real filesystem, no real home paths.
    use super::*;

    fn file(abs: &str) -> Resolved {
        Resolved {
            abs: abs.to_owned(),
            kind: FsKind::File,
            line: None,
            col: None,
        }
    }

    fn dir(abs: &str) -> Resolved {
        Resolved {
            abs: abs.to_owned(),
            kind: FsKind::Dir,
            line: None,
            col: None,
        }
    }

    #[test]
    fn host_resolves_to_a_single_cfg_boundary() {
        // The host selector is the ONLY production `cfg!(target_os)` for opener
        // dispatch. On the Linux CI/dev host it is Linux; on macOS it is Macos.
        let expected = if cfg!(target_os = "macos") {
            OpenerOs::Macos
        } else {
            OpenerOs::Linux
        };
        assert_eq!(OpenerOs::host(), expected);
    }

    #[test]
    fn open_default_argv_both_os_branches() {
        assert_eq!(
            open_default_argv(OpenerOs::Linux, "/proj/a.png"),
            vec!["xdg-open".to_owned(), "/proj/a.png".to_owned()]
        );
        assert_eq!(
            open_default_argv(OpenerOs::Macos, "/proj/a.png"),
            vec!["open".to_owned(), "/proj/a.png".to_owned()]
        );
    }

    #[test]
    fn open_default_argv_keeps_uri_and_spaces_as_one_element() {
        // A file:// URI and a path with spaces each stay a single inert argv
        // element on both OSes.
        let uri = "file:///proj/my dir/a.png";
        assert_eq!(
            open_default_argv(OpenerOs::Linux, uri),
            vec!["xdg-open".to_owned(), uri.to_owned()]
        );
        assert_eq!(
            open_default_argv(OpenerOs::Macos, uri),
            vec!["open".to_owned(), uri.to_owned()]
        );
    }

    #[test]
    fn reveal_argv_file_parent_on_linux_dash_r_on_macos() {
        let r = file("/proj/src/a.rs");
        // Linux reveals the parent directory.
        assert_eq!(
            reveal_argv(OpenerOs::Linux, &r),
            vec!["xdg-open".to_owned(), "/proj/src".to_owned()]
        );
        // macOS reveals the FILE itself with -R.
        assert_eq!(
            reveal_argv(OpenerOs::Macos, &r),
            vec![
                "open".to_owned(),
                "-R".to_owned(),
                "/proj/src/a.rs".to_owned()
            ]
        );
    }

    #[test]
    fn reveal_argv_dir_self_on_linux_dash_r_on_macos() {
        let r = dir("/proj/src");
        assert_eq!(
            reveal_argv(OpenerOs::Linux, &r),
            vec!["xdg-open".to_owned(), "/proj/src".to_owned()]
        );
        assert_eq!(
            reveal_argv(OpenerOs::Macos, &r),
            vec!["open".to_owned(), "-R".to_owned(), "/proj/src".to_owned()]
        );
    }

    #[test]
    fn reveal_argv_root_level_file_reveals_root_on_linux() {
        let r = file("/a.rs");
        assert_eq!(
            reveal_argv(OpenerOs::Linux, &r),
            vec!["xdg-open".to_owned(), "/".to_owned()]
        );
        // macOS still reveals the file itself.
        assert_eq!(
            reveal_argv(OpenerOs::Macos, &r),
            vec!["open".to_owned(), "-R".to_owned(), "/a.rs".to_owned()]
        );
    }
}

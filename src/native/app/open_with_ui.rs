// SPDX-License-Identifier: GPL-3.0-only
//! App-side "Open With…" integration (C3b): the production seams and the open
//! action.
//!
//! The pure enumeration lives in `crate::desktop` behind two injectable seams
//! ([`crate::desktop::MimeProbe`] + [`crate::desktop::DesktopEnv`]). This module
//! supplies the one production implementation of each — [`XdgMimeProbe`] (the
//! single audited captured-output `xdg-mime` spawn) and [`FsDesktopEnv`] (real
//! `std::fs` + the real `XDG_*` env ladder, bounded reads) — and the App method
//! that wires them, enumerates the handler apps, and opens the picker overlay.
//!
//! Layering: the seam *implementations* live here (in `native/`) because they
//! touch the real process/filesystem; `crate::desktop` itself stays pure and
//! GPU/windowing-free. The open action reuses the C3 `spawn_detached` verbatim.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::desktop::{DesktopApp, DesktopEnv, MimeProbe, enumerate_open_with};

use super::*;

/// Maximum bytes read from any single `.desktop` / cache / list file. These are
/// small text files; the cap stops a pathological/hostile file from being slurped
/// whole. A truncated read still parses (a partial entry simply fails the
/// `is_offerable` filter or drops a field) — never an error.
const MAX_DESKTOP_FILE_BYTES: u64 = 256 * 1024;

/// Production MIME probe: the single audited captured-output `xdg-mime` spawn.
/// `xdg-mime query filetype <abs>` is argv-only and read-only; a non-zero exit,
/// missing binary, or empty output yields `None` → an empty picker (graceful).
/// This is the ONLY new spawn shape C3b introduces; the open itself reuses the
/// C3 `spawn_detached`.
pub(in crate::native) struct XdgMimeProbe;

impl MimeProbe for XdgMimeProbe {
    fn query(&self, abs: &str) -> Option<String> {
        let output = Command::new("xdg-mime")
            .args(["query", "filetype", abs])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let mime = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        if mime.is_empty() { None } else { Some(mime) }
    }
}

/// Production desktop environment: the real `XDG_*` ladders and bounded
/// `std::fs` reads. Mirrors the env-var idiom in `palette_overlay.rs`
/// (`filter(non-empty).map(PathBuf)`), with the spec defaults when a variable is
/// unset/empty.
pub(in crate::native) struct FsDesktopEnv;

impl FsDesktopEnv {
    fn home() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }

    /// Split a `:`-separated `XDG_*_DIRS` value into existing-or-not paths, in
    /// order, dropping empty fields.
    fn split_dirs(value: Option<std::ffi::OsString>, fallback: &[&str]) -> Vec<PathBuf> {
        match value
            .filter(|v| !v.is_empty())
            .and_then(|v| v.into_string().ok())
        {
            Some(text) => text
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            None => fallback.iter().map(PathBuf::from).collect(),
        }
    }
}

impl DesktopEnv for FsDesktopEnv {
    fn config_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(value) => dirs.push(PathBuf::from(value)),
            None => {
                if let Some(home) = Self::home() {
                    dirs.push(home.join(".config"));
                }
            }
        }
        dirs.extend(Self::split_dirs(
            std::env::var_os("XDG_CONFIG_DIRS"),
            &["/etc/xdg"],
        ));
        dirs
    }

    fn data_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            Some(value) => dirs.push(PathBuf::from(value)),
            None => {
                if let Some(home) = Self::home() {
                    dirs.push(home.join(".local").join("share"));
                }
            }
        }
        dirs.extend(Self::split_dirs(
            std::env::var_os("XDG_DATA_DIRS"),
            &["/usr/local/share", "/usr/share"],
        ));
        dirs
    }

    fn read_file(&self, path: &Path) -> Option<String> {
        use std::io::Read;
        let file = std::fs::File::open(path).ok()?;
        let mut buf = String::new();
        // Bounded read: a hostile/huge file is truncated rather than slurped.
        file.take(MAX_DESKTOP_FILE_BYTES)
            .read_to_string(&mut buf)
            .ok()?;
        Some(buf)
    }
}

impl App {
    /// Open the "Open With…" app picker for a resolved file (C3b). Enumerates
    /// the handler applications through the production seams (read-only; the
    /// one captured `xdg-mime` spawn plus bounded `std::fs` reads), each row
    /// carrying a pre-built argv-only command, and opens the picker overlay. A
    /// directory never reaches here (the menu item is file-only); a file with no
    /// handlers still opens the overlay with its empty-state hint.
    pub(super) fn open_open_with_overlay(&mut self, resolved: &crate::paths::Resolved) {
        if resolved.kind != crate::paths::FsKind::File {
            return;
        }
        if self.search.is_open() {
            self.close_search(true);
        }
        let apps = self.enumerate_open_with_apps(&resolved.abs);
        self.reset_pointer_state_for_overlay();
        self.overlay.open_open_with(apps);
        self.request_selection_redraw();
    }

    /// Enumerate the apps that can open `abs`, via the production seams. Split
    /// out so a test seam can swap in synthetic probes without spawning
    /// `xdg-mime` or touching the real filesystem.
    fn enumerate_open_with_apps(&self, abs: &str) -> Vec<DesktopApp> {
        enumerate_open_with(&XdgMimeProbe, &FsDesktopEnv, abs)
    }
}

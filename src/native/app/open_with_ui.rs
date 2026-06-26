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

/// Production MIME probe: the platform-aware, single audited captured-output
/// MIME query (P0-1). On Linux it spawns `xdg-mime query filetype <abs>`
/// (argv-only, read-only); a non-zero exit, missing binary, or empty output
/// yields `None` and then falls back to a small built-in magic-byte sniffer.
/// macOS has no `xdg-mime`, so its platform arm is `None` and uses that same
/// fallback. A final `None` on either OS surfaces the empty picker with its
/// visible empty-state hint rather than a silent no-op.
///
/// The OS is held as a value ([`OpenerOs`]) rather than read from `cfg!` inline
/// so BOTH arms are unit-testable on one CI host (the v0.4.0 lesson: never let
/// the macOS branch go unexercised). Production constructs it via
/// [`PlatformMimeProbe::host`].
pub(in crate::native) struct PlatformMimeProbe {
    os: super::platform_opener::OpenerOs,
}

impl PlatformMimeProbe {
    /// The probe for the host OS (the single `cfg!` boundary lives in
    /// [`super::platform_opener::OpenerOs::host`]).
    pub(in crate::native) fn host() -> Self {
        Self {
            os: super::platform_opener::OpenerOs::host(),
        }
    }

    #[cfg(test)]
    fn for_os(os: super::platform_opener::OpenerOs) -> Self {
        Self { os }
    }
}

impl MimeProbe for PlatformMimeProbe {
    fn query(&self, abs: &str) -> Option<String> {
        let platform_mime = match self.os {
            super::platform_opener::OpenerOs::Linux => xdg_mime_query(abs),
            // macOS: no xdg-mime/LaunchServices query wired here yet, so use the
            // fallback below. The empty picker still shows its visible hint when
            // neither path identifies the file.
            super::platform_opener::OpenerOs::Macos => None,
        };
        platform_mime.or_else(|| super::platform_opener::sniff_mime_path(abs))
    }
}

/// The Linux `xdg-mime query filetype <abs>` spawn (captured output, argv-only,
/// read-only). Factored out so the OS dispatch above stays a thin match.
fn xdg_mime_query(abs: &str) -> Option<String> {
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
        #[cfg(target_os = "macos")]
        {
            // macOS has no freedesktop database; ask NSWorkspace directly. The
            // Linux seam-based enumeration would always return empty here.
            crate::native::macos_open_with::enumerate(abs)
        }
        #[cfg(not(target_os = "macos"))]
        {
            enumerate_open_with(&PlatformMimeProbe::host(), &FsDesktopEnv, abs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::MimeProbe;

    /// The macOS MIME arm has no `xdg-mime` spawn. Asserted on the Linux CI host
    /// via the OS-as-value seam so the macOS branch can never go unexercised.
    /// NEVER spawns under the test target (it short-circuits before the Linux
    /// `xdg-mime` spawn).
    #[test]
    fn macos_mime_probe_returns_none_without_spawning() {
        let probe = PlatformMimeProbe::for_os(super::super::platform_opener::OpenerOs::Macos);
        assert_eq!(probe.query("/proj/a.png"), None);
    }

    #[test]
    fn macos_mime_probe_uses_magic_byte_fallback_without_spawning() {
        let probe = PlatformMimeProbe::for_os(super::super::platform_opener::OpenerOs::Macos);
        let path = temp_probe_file("magic.png", &[0x89, b'P', b'N', b'G', 0x0d, 0x0a]);

        let mime = probe.query(path.to_str().expect("utf8 temp path"));

        let _ = std::fs::remove_file(&path);
        assert_eq!(mime, Some("image/png".to_owned()));
    }

    fn temp_probe_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("odytty-open-with-ui-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("write synthetic probe file");
        path
    }
}

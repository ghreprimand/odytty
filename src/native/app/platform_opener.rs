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

/// Maximum bytes read from a file for dependency-free MIME sniffing. Every
/// signature currently checked fits within the first 12 bytes; the slightly
/// larger cap leaves room for future signatures without broadening I/O.
///
/// Linux-only: on macOS, NSWorkspace enumerates the openable apps directly
/// (UTI-aware), so the magic-byte MIME fallback this caps has no caller there.
#[cfg(all(unix, not(target_os = "macos")))]
const MIME_SNIFF_BYTES: u64 = 32;

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
    /// Windows: `explorer <target>` for default-open, `explorer /select,`
    /// for reveal. Like [`OpenerOs::Macos`], this variant is always present (not
    /// `cfg`-gated) so its argv branches are unit-tested on any CI host — the
    /// v0.4.0 lesson that an unexercised platform branch silently rots.
    Windows,
}

impl OpenerOs {
    /// The host OS, resolved by the single `cfg!(target_os)` selector in the
    /// codebase. macOS → [`OpenerOs::Macos`]; Windows → [`OpenerOs::Windows`];
    /// everything else → [`OpenerOs::Linux`] (OdyTTY targets Linux + macOS +
    /// Windows; any other unix uses the xdg path, the correct default on
    /// free-desktop systems).
    pub(in crate::native) fn host() -> Self {
        if cfg!(target_os = "macos") {
            OpenerOs::Macos
        } else if cfg!(target_os = "windows") {
            OpenerOs::Windows
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
/// * Windows → `["explorer", target]`
///
/// `target` is always a single inert argv element — a path/URI containing `;`,
/// `$()`, backticks, or spaces is never shell-interpreted.
///
/// Windows note: the launcher is `explorer.exe`, invoked with `target` as one
/// argv element (`explorer <target>`). Explorer hands the argument to the shell
/// open verb, launching a `file:`/`http`/`https`/`mailto` URI with its default
/// handler and a filesystem path with its associated app — the same effect the
/// old `cmd /C start "" <target>` had, WITHOUT a `cmd.exe` command line. That
/// distinction is a security boundary: `cmd` splits its command line on `&`,
/// `|`, `%VAR%` and other metacharacters, and Rust's argv quoting escapes none
/// of them, so a URI printed by any program (`http://x/&calc.exe&`) reaching
/// `cmd` executed the trailing token. `explorer.exe` takes the target as a
/// single non-shell parameter (dispatched through `CreateProcessW`-style spawn),
/// so no untrusted string ever reaches a `cmd.exe` command line. The argv-vec
/// form (rather than a `ShellExecuteW` FFI call) keeps every branch a pure
/// `Vec<String>` asserted by the cross-host unit tests, matching the module's
/// "argv-only, single spawn point" contract.
pub(in crate::native) fn open_default_argv(os: OpenerOs, target: &str) -> Vec<String> {
    match os {
        OpenerOs::Linux => vec!["xdg-open".to_owned(), target.to_owned()],
        OpenerOs::Macos => vec!["open".to_owned(), target.to_owned()],
        OpenerOs::Windows => vec!["explorer".to_owned(), target.to_owned()],
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
/// * Windows → `["explorer", "/select,", <abs with backslashes>]` — `explorer
///   /select,<path>` opens the containing folder with the entry selected (like
///   macOS `-R`, the entry ITSELF, not its parent). `/select,` is a SINGLE token
///   including its trailing comma, and Explorer only honors it when the path
///   uses backslash separators, so any `/` in the resolved path is converted to
///   `\` for this argv.
pub(in crate::native) fn reveal_argv(os: OpenerOs, resolved: &Resolved) -> Vec<String> {
    match os {
        OpenerOs::Linux => vec!["xdg-open".to_owned(), reveal_parent(resolved)],
        OpenerOs::Macos => vec!["open".to_owned(), "-R".to_owned(), resolved.abs.clone()],
        OpenerOs::Windows => vec![
            "explorer".to_owned(),
            "/select,".to_owned(),
            resolved.abs.replace('/', "\\"),
        ],
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

/// Best-effort MIME fallback from leading magic bytes. This is intentionally
/// dependency-free and conservative: unknown or too-short data returns `None`,
/// letting the platform probe remain authoritative whenever it succeeds.
///
/// Linux-only: macOS resolves openable apps through NSWorkspace (no MIME query
/// or sniff fallback is wired there), so this has no macOS caller.
#[cfg(all(unix, not(target_os = "macos")))]
pub(in crate::native) fn sniff_mime_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.starts_with(b"%PDF") {
        return Some("application/pdf");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        return Some("image/tiff");
    }
    None
}

/// Read a tiny prefix from `abs` and apply [`sniff_mime_bytes`]. I/O failures,
/// directories, and unrecognized data all return `None` so callers can preserve
/// the existing empty-picker behavior when neither the platform nor the fallback
/// can identify the type.
///
/// Linux-only: the sole caller is the Linux `PlatformMimeProbe`, which is itself
/// gated off macOS (NSWorkspace replaces the whole MIME/desktop chain there).
#[cfg(all(unix, not(target_os = "macos")))]
pub(in crate::native) fn sniff_mime_path(abs: &str) -> Option<String> {
    use std::io::Read;

    let file = std::fs::File::open(abs).ok()?;
    let mut bytes = Vec::new();
    file.take(MIME_SNIFF_BYTES).read_to_end(&mut bytes).ok()?;
    sniff_mime_bytes(&bytes).map(str::to_owned)
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
        // dispatch. On the Linux CI/dev host it is Linux; on macOS it is Macos;
        // on Windows it is Windows.
        let expected = if cfg!(target_os = "macos") {
            OpenerOs::Macos
        } else if cfg!(target_os = "windows") {
            OpenerOs::Windows
        } else {
            OpenerOs::Linux
        };
        assert_eq!(OpenerOs::host(), expected);
    }

    #[test]
    fn open_default_argv_all_os_branches() {
        assert_eq!(
            open_default_argv(OpenerOs::Linux, "/proj/a.png"),
            vec!["xdg-open".to_owned(), "/proj/a.png".to_owned()]
        );
        assert_eq!(
            open_default_argv(OpenerOs::Macos, "/proj/a.png"),
            vec!["open".to_owned(), "/proj/a.png".to_owned()]
        );
        // Windows: `explorer <target>` — no `cmd.exe` command line, so a URI
        // with shell metacharacters can never be re-parsed as a command.
        assert_eq!(
            open_default_argv(OpenerOs::Windows, "C:\\proj\\a.png"),
            vec!["explorer".to_owned(), "C:\\proj\\a.png".to_owned()]
        );
    }

    #[test]
    fn open_default_argv_windows_keeps_spaces_and_uri_as_one_element() {
        // A target with spaces stays a single inert argv element passed to
        // `explorer`.
        let spaced = "C:\\my dir\\a.png";
        assert_eq!(
            open_default_argv(OpenerOs::Windows, spaced),
            vec!["explorer".to_owned(), spaced.to_owned()]
        );
    }

    #[test]
    fn open_default_argv_windows_never_routes_a_uri_through_cmd() {
        // C1 regression: a URI carrying `cmd.exe` metacharacters (`&` command
        // chaining, `%VAR%` expansion) must never reach a `cmd.exe` command
        // line. The argv is `["explorer", <uri>]` with the URI kept as one
        // inert element, so there is no `cmd` program and no shell to split on
        // `&` — `calc.exe` is never a separate token.
        let hostile = "http://x/&calc.exe&%USERPROFILE%";
        let argv = open_default_argv(OpenerOs::Windows, hostile);
        assert_eq!(argv, vec!["explorer".to_owned(), hostile.to_owned()]);
        assert_eq!(argv[0], "explorer");
        assert!(
            !argv
                .iter()
                .any(|a| a.eq_ignore_ascii_case("cmd") || a.eq_ignore_ascii_case("cmd.exe")),
            "no cmd program may appear in the Windows open argv"
        );
        // The whole hostile URI is a single argv element — nothing was split.
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[1], hostile);
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
    fn reveal_argv_windows_selects_the_entry_with_backslashes() {
        // A backslash drive path reveals the file itself with `/select,` (one
        // token incl. the comma), backslashes preserved.
        let r = file("C:\\proj\\src\\a.rs");
        assert_eq!(
            reveal_argv(OpenerOs::Windows, &r),
            vec![
                "explorer".to_owned(),
                "/select,".to_owned(),
                "C:\\proj\\src\\a.rs".to_owned()
            ]
        );
        // A resolved path that carries forward slashes is converted to
        // backslashes, which is what Explorer's `/select,` requires.
        let fwd = file("C:/proj/src/a.rs");
        assert_eq!(
            reveal_argv(OpenerOs::Windows, &fwd),
            vec![
                "explorer".to_owned(),
                "/select,".to_owned(),
                "C:\\proj\\src\\a.rs".to_owned()
            ]
        );
        // A directory is also selected (in its parent), not its parent opened.
        let d = dir("C:\\proj\\src");
        assert_eq!(
            reveal_argv(OpenerOs::Windows, &d),
            vec![
                "explorer".to_owned(),
                "/select,".to_owned(),
                "C:\\proj\\src".to_owned()
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

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn sniff_mime_bytes_recognizes_common_magic_headers() {
        assert_eq!(
            sniff_mime_bytes(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a]),
            Some("image/png")
        );
        assert_eq!(
            sniff_mime_bytes(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_mime_bytes(b"GIF89a..."), Some("image/gif"));
        assert_eq!(sniff_mime_bytes(b"%PDF-1.7"), Some("application/pdf"));
        assert_eq!(
            sniff_mime_bytes(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(sniff_mime_bytes(b"BM...."), Some("image/bmp"));
        assert_eq!(sniff_mime_bytes(b"II*\0...."), Some("image/tiff"));
        assert_eq!(sniff_mime_bytes(b"MM\0*...."), Some("image/tiff"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn sniff_mime_bytes_rejects_unknown_or_too_short_data() {
        assert_eq!(sniff_mime_bytes(b""), None);
        assert_eq!(sniff_mime_bytes(b"RIFFshort"), None);
        assert_eq!(sniff_mime_bytes(b"plain text"), None);
    }
}

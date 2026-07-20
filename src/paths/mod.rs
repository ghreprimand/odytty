// SPDX-License-Identifier: GPL-3.0-only
//! PATHS — interactive filesystem-path detection + resolution (the pure spine).
//!
//! This module is the owned, dependency-free core for the "interactive paths"
//! feature: it turns arbitrary terminal text into actionable, **stat-gated**
//! filesystem paths. It is split into two pure layers:
//!
//! * [`detect`] — a hand-rolled scanner that finds path-looking spans in a line
//!   and parses an optional `:line[:col]` suffix. Syntactic only; no I/O.
//! * [`resolve`] (this file) — turns a [`PathSpan`] + the pane's OSC 7 working
//!   directory + the user's home into a canonical absolute path, then
//!   **stat-gates** it through an injectable [`ResolveProbe`] so a span is only
//!   "live" when it maps to a real entry.
//!
//! Purity is structural: `src/paths/` imports **std only** — no `winit`/`wgpu`,
//! no render/settings, no `regex`, no new dependency, and crucially **no
//! `std::fs`**. The single filesystem touch is the caller-supplied probe, which
//! tests replace with a synthetic `HashMap` so no test ever reaches the real
//! filesystem. See `docs/interactive-paths-design.md` for the full design,
//! security argument, open-action dispatch table, and editor invocation matrix.
//!
//! This module (C0/C1) builds the spine only — it is intentionally **not**
//! referenced by any render/input/settings path yet, so it adds zero runtime
//! behavior. Phases 7–9 wire hover/click/menu/viewer onto it.

pub mod detect;
pub mod file_uri;

pub use detect::{
    DetectionOptions, PathSpan, detect_path_candidates_at, detect_paths, detect_paths_with_options,
};

/// What kind of filesystem entry a resolved path points at. The probe reports
/// this; the open-action dispatch (Phase 8) branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    /// A regular file (or anything that is not a directory).
    File,
    /// A directory.
    Dir,
}

/// A path span that resolved to a real filesystem entry. Produced by
/// [`resolve`]; consumed by the (later) open-action dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The canonical absolute path (lexically normalized — `.`/`..`/duplicate
    /// slashes collapsed without touching the filesystem).
    pub abs: String,
    /// Whether the entry is a file or a directory.
    pub kind: FsKind,
    /// Line number carried over from the span's `:line[:col]` suffix.
    pub line: Option<u32>,
    /// Column number carried over from the span's `:line:col` suffix.
    pub col: Option<u32>,
}

/// The stat-gate seam. Production wires this to a `std::fs::symlink_metadata`
/// wrapper; tests inject a synthetic `HashMap<String, FsKind>`. Keeping the only
/// filesystem touch behind this trait is what lets the whole module — and every
/// test — stay off the real filesystem.
pub trait ResolveProbe {
    /// Classify an absolute path, or `None` if it does not exist.
    fn classify(&self, abs_path: &str) -> Option<FsKind>;
}

/// The image extensions the in-terminal viewer (Phase 9 / C4) will *offer* to
/// open. This is the cheap, I/O-free "should we show the menu item" gate: it
/// trusts only the extension. The list MUST match the decoders enabled in
/// `Cargo.toml` exactly — `png`, `jpeg`, `webp` — and no more: offering a format
/// the native decoder cannot read would dead-end at open time. Lowercase; the
/// check is case-insensitive (see [`is_image_path`]). GIF/BMP/TIFF are
/// deliberately absent (their decoders are not enabled).
///
/// Staying in this pure, std-only module keeps the `image` crate out of
/// `src/paths/`; the real format trust is the content-sniff at decode time.
pub const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// Whether `abs` has an extension in [`IMAGE_EXTENSIONS`] (case-insensitive).
/// Pure and std-only — no filesystem touch, no `image` dependency. Used to
/// conditionally show the "Open in OdyTTY" menu item on a resolved image span;
/// the actual decode (with its content-sniff and decode bound) is the real
/// gate. A path with no extension, a trailing dot, or a non-image extension
/// returns `false`.
pub fn is_image_path(abs: &str) -> bool {
    // The extension is the text after the final `.` in the final path segment.
    let name = abs.rsplit('/').next().unwrap_or(abs);
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        // A dotfile like `.png` has no stem before the dot — not an extension.
        return false;
    }
    let ext = ext.to_ascii_lowercase();
    IMAGE_EXTENSIONS.contains(&ext.as_str())
}

/// Resolve a syntactic [`PathSpan`] into a live [`Resolved`] entry, or `None` if
/// it cannot be made absolute (missing cwd/home) or the probe says it does not
/// exist (stat-gate).
///
/// * Absolute (`/…`) → used directly.
/// * Home (`~/…`, `~`) → `~` replaced by `home`; `None` if `home` is unknown.
/// * Relative (`./`, `../`, bare `dir/file`) → joined onto `cwd`; `None` if
///   `cwd` is unknown.
///
/// The joined path is lexically canonicalized (no filesystem access) before the
/// single probe call.
pub fn resolve(
    span: &PathSpan,
    cwd: Option<&str>,
    home: Option<&str>,
    probe: &impl ResolveProbe,
) -> Option<Resolved> {
    let abs = make_absolute(&span.raw, cwd, home)?;
    let canon = lexical_canonicalize(&abs);
    let kind = probe.classify(&canon)?;
    Some(Resolved {
        abs: canon,
        kind,
        line: span.line,
        col: span.col,
    })
}

/// Turn a raw path body into an (un-canonicalized) absolute path string, or
/// `None` when the needed base (cwd/home) is absent.
fn make_absolute(raw: &str, cwd: Option<&str>, home: Option<&str>) -> Option<String> {
    if raw.starts_with('/') {
        return Some(raw.to_owned());
    }
    if raw == "~" {
        return Some(home?.to_owned());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = home?.trim_end_matches('/');
        return Some(format!("{home}/{rest}"));
    }
    // Windows-absolute (`C:\…`, `C:/…`, UNC `\\…`): already rooted — must NOT be
    // joined onto cwd, or a detected `C:\src` would wrongly become
    // `<cwd>/C:\src`. Wired in only on Windows so POSIX resolution stays
    // byte-identical; the predicate is also compiled in test builds so it is
    // unit-tested on a Linux host (see [`is_windows_absolute`]).
    #[cfg(windows)]
    {
        if is_windows_absolute(raw) {
            return Some(raw.to_owned());
        }
    }
    // Relative (`./`, `../`, bare `dir/file`): join onto cwd; canonicalization
    // collapses the `.`/`..` components.
    let cwd = cwd?.trim_end_matches('/');
    Some(format!("{cwd}/{raw}"))
}

/// Whether `raw` is a Windows-absolute path: drive-absolute (`C:\…` / `C:/…`) or
/// UNC (`\\server\share`). Drive-RELATIVE (`C:foo`, no separator after the
/// colon) is intentionally NOT absolute.
///
/// Compiled on Windows (where [`make_absolute`] consults it) and in any test
/// build (so the logic is unit-tested on a Linux host); absent from a Linux/
/// macOS release build so POSIX resolution stays byte-identical. The drive
/// matcher requires EXACTLY ONE ascii-alpha char before the colon (the same
/// URL-collision guard as detection — `https:`/`file:` are never drives).
#[cfg(any(windows, test))]
fn is_windows_absolute(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    raw.starts_with("\\\\")
}

/// Lexically canonicalize an absolute-or-relative path **without touching the
/// filesystem**: collapse `.`, resolve `..` textually, drop duplicate slashes.
/// A leading `/` is preserved; `..` that would escape an absolute root is
/// dropped (matching shell `cd` semantics on a normalized path).
fn lexical_canonicalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                match out.last() {
                    Some(&last) if last != ".." => {
                        out.pop();
                    }
                    _ if absolute => { /* can't escape root: drop */ }
                    _ => out.push(".."),
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_owned()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Synthetic, in-memory filesystem — the ONLY "filesystem" any test touches.
    struct MapProbe(HashMap<String, FsKind>);

    impl MapProbe {
        fn new<const N: usize>(entries: [(&str, FsKind); N]) -> Self {
            MapProbe(entries.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect())
        }
    }

    impl ResolveProbe for MapProbe {
        fn classify(&self, abs_path: &str) -> Option<FsKind> {
            self.0.get(abs_path).copied()
        }
    }

    fn span(raw: &str, line: Option<u32>, col: Option<u32>) -> PathSpan {
        PathSpan {
            start: 0,
            end: raw.len(),
            raw: raw.to_owned(),
            line,
            col,
        }
    }

    #[test]
    fn absolute_path_resolves_when_probe_has_it() {
        // Absolute paths need neither cwd nor home.
        let probe = MapProbe::new([("/proj/src/main.rs", FsKind::File)]);
        let r = resolve(&span("/proj/src/main.rs", None, None), None, None, &probe).unwrap();
        assert_eq!(r.abs, "/proj/src/main.rs");
        assert_eq!(r.kind, FsKind::File);
    }

    #[test]
    fn absolute_path_is_dead_when_probe_lacks_it() {
        let probe = MapProbe::new([]);
        assert!(resolve(&span("/nope/x.rs", None, None), None, None, &probe).is_none());
    }

    #[test]
    fn relative_path_joins_cwd() {
        let probe = MapProbe::new([("/proj/src/main.rs", FsKind::File)]);
        let r = resolve(
            &span("src/main.rs", Some(42), Some(10)),
            Some("/proj"),
            None,
            &probe,
        )
        .unwrap();
        assert_eq!(r.abs, "/proj/src/main.rs");
        assert_eq!((r.line, r.col), (Some(42), Some(10)));
    }

    #[test]
    fn relative_path_without_cwd_is_unresolvable() {
        let probe = MapProbe::new([("/proj/src/main.rs", FsKind::File)]);
        assert!(resolve(&span("src/main.rs", None, None), None, None, &probe).is_none());
    }

    #[test]
    fn dot_and_dotdot_are_canonicalized_textually() {
        let probe = MapProbe::new([("/proj/sibling/x.rs", FsKind::File)]);
        // cwd /proj/src, ../sibling/x.rs → /proj/sibling/x.rs
        let r = resolve(
            &span("../sibling/x.rs", None, None),
            Some("/proj/src"),
            None,
            &probe,
        )
        .unwrap();
        assert_eq!(r.abs, "/proj/sibling/x.rs");
        // ./foo collapses the leading `.`
        let probe2 = MapProbe::new([("/proj/foo.rs", FsKind::File)]);
        let r2 = resolve(&span("./foo.rs", None, None), Some("/proj"), None, &probe2).unwrap();
        assert_eq!(r2.abs, "/proj/foo.rs");
    }

    #[test]
    fn home_expansion_against_synthetic_home() {
        let probe = MapProbe::new([("/home/user/notes/a.txt", FsKind::File)]);
        let r = resolve(
            &span("~/notes/a.txt", None, None),
            None,
            Some("/home/user"),
            &probe,
        )
        .unwrap();
        assert_eq!(r.abs, "/home/user/notes/a.txt");
    }

    #[test]
    fn home_path_without_home_is_unresolvable() {
        let probe = MapProbe::new([("/home/user/notes/a.txt", FsKind::File)]);
        assert!(resolve(&span("~/notes/a.txt", None, None), None, None, &probe).is_none());
    }

    #[test]
    fn bare_tilde_resolves_to_home_dir() {
        let probe = MapProbe::new([("/home/user", FsKind::Dir)]);
        let r = resolve(&span("~", None, None), None, Some("/home/user"), &probe).unwrap();
        assert_eq!(r.abs, "/home/user");
        assert_eq!(r.kind, FsKind::Dir);
    }

    #[test]
    fn file_vs_dir_classification_flows_through() {
        let probe = MapProbe::new([
            ("/proj/src", FsKind::Dir),
            ("/proj/src/main.rs", FsKind::File),
        ]);
        let dir = resolve(&span("/proj/src", None, None), None, None, &probe).unwrap();
        assert_eq!(dir.kind, FsKind::Dir);
        let file = resolve(&span("/proj/src/main.rs", None, None), None, None, &probe).unwrap();
        assert_eq!(file.kind, FsKind::File);
    }

    #[test]
    fn same_span_live_then_dead_across_probes() {
        let s = span("/proj/x.rs", None, None);
        let live = MapProbe::new([("/proj/x.rs", FsKind::File)]);
        let dead = MapProbe::new([]);
        assert!(resolve(&s, None, None, &live).is_some());
        assert!(resolve(&s, None, None, &dead).is_none());
    }

    #[test]
    fn dotdot_cannot_escape_absolute_root() {
        let probe = MapProbe::new([("/x", FsKind::File)]);
        // /../../x normalizes to /x (root is the floor).
        let r = resolve(&span("/../../x", None, None), None, None, &probe).unwrap();
        assert_eq!(r.abs, "/x");
    }

    #[test]
    fn duplicate_slashes_collapse() {
        let probe = MapProbe::new([("/a/b/c", FsKind::File)]);
        let r = resolve(&span("/a//b///c", None, None), None, None, &probe).unwrap();
        assert_eq!(r.abs, "/a/b/c");
    }

    #[test]
    fn is_image_path_matches_enabled_decoders_only() {
        // The enabled decoders (png/jpeg/webp) are offered, case-insensitively.
        for ok in [
            "/proj/a.png",
            "/proj/a.PNG",
            "photo.jpg",
            "photo.JPEG",
            "render.webp",
            "/deep/dir/Screenshot.Png",
        ] {
            assert!(is_image_path(ok), "{ok} should be offered");
        }
        // Disabled / non-image extensions are not offered.
        for no in [
            "/proj/a.gif",  // decoder not enabled
            "/proj/a.bmp",  // decoder not enabled
            "/proj/a.tiff", // decoder not enabled
            "/proj/a.txt",
            "/proj/main.rs",
            "/proj/noext",
            "/proj/.png", // dotfile, no stem
            "/proj/archive.png.gz",
            "png",
        ] {
            assert!(!is_image_path(no), "{no} should NOT be offered");
        }
    }

    #[test]
    fn detect_then_resolve_end_to_end_synthetic() {
        // Detection + resolution composed over synthetic inputs only.
        let line = "error[E0382]: see src/main.rs:42:10 for details";
        let spans = detect_paths(line);
        assert_eq!(spans.len(), 1);
        let probe = MapProbe::new([("/proj/src/main.rs", FsKind::File)]);
        let r = resolve(&spans[0], Some("/proj"), None, &probe).unwrap();
        assert_eq!(r.abs, "/proj/src/main.rs");
        assert_eq!((r.line, r.col), (Some(42), Some(10)));
        assert_eq!(r.kind, FsKind::File);
    }

    #[test]
    fn bareword_detect_then_resolve_lights_up_only_when_probe_has_it() {
        let options = DetectionOptions { barewords: true };
        let spans = detect_paths_with_options("carpet1.jpg", options);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].raw, "carpet1.jpg");

        let live = MapProbe::new([("/proj/carpet1.jpg", FsKind::File)]);
        let dead = MapProbe::new([]);
        let resolved = resolve(&spans[0], Some("/proj"), Some("/home/user"), &live).unwrap();

        assert_eq!(resolved.abs, "/proj/carpet1.jpg");
        assert_eq!(resolved.kind, FsKind::File);
        assert!(resolve(&spans[0], Some("/proj"), Some("/home/user"), &dead).is_none());
    }

    #[test]
    fn windows_drive_cwd_must_not_keep_file_url_leading_slash() {
        let options = DetectionOptions { barewords: true };
        let spans = detect_paths_with_options("a.txt", options);
        assert_eq!(spans.len(), 1);

        let probe = MapProbe::new([("C:/proj/a.txt", FsKind::File)]);
        let resolved = resolve(&spans[0], Some("C:/proj"), None, &probe).unwrap();
        assert_eq!(resolved.abs, "C:/proj/a.txt");

        assert!(resolve(&spans[0], Some("/C:/proj"), None, &probe).is_none());
    }

    #[test]
    fn bareword_detect_then_resolve_keeps_default_mode_byte_identical() {
        assert!(detect_paths("carpet1.jpg").is_empty());
        assert!(detect_paths("README").is_empty());
        assert!(detect_paths("1.2.3").is_empty());
        assert!(detect_paths("example.com").is_empty());
    }

    // --- Windows-absolute predicate (unit-tested on the Linux CI host; consulted
    // by the resolver only under `#[cfg(windows)]`) --------------------------

    #[test]
    fn windows_absolute_predicate_matches_drive_and_unc() {
        assert!(is_windows_absolute("C:\\src\\main.rs"));
        assert!(is_windows_absolute("c:/src/main.rs")); // forward-slash drive
        assert!(is_windows_absolute("Z:\\")); // bare drive root
        assert!(is_windows_absolute("\\\\server\\share")); // UNC
    }

    #[test]
    fn windows_absolute_predicate_rejects_relative_and_schemes() {
        // Drive-RELATIVE (no separator after the colon) is NOT absolute.
        assert!(!is_windows_absolute("C:foo"));
        assert!(!is_windows_absolute("C:10"));
        // Multi-char scheme is not a single-letter drive (URL-collision guard).
        assert!(!is_windows_absolute("https://example.com"));
        // POSIX / bare-relative inputs are not Windows-absolute.
        assert!(!is_windows_absolute("/usr/bin"));
        assert!(!is_windows_absolute("src\\main.rs")); // backslash-relative, not rooted
        assert!(!is_windows_absolute("README"));
    }
}

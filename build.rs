// SPDX-License-Identifier: GPL-3.0-only
//! Build-time provenance for the in-app About panel.
//!
//! Emits several `rustc-env` values the binary reads via `env!`:
//!   ODYTTY_GIT_SHA   commit hash for the About panel, resolved by precedence:
//!                    a validated ODYTTY_BUILD_SHA override (official/package
//!                    CI), else the live git short SHA (+"-dirty" on a dirty
//!                    checkout), else the `git archive` export-subst token from
//!                    the source tarball, else "unavailable" (never "unknown").
//!   ODYTTY_BUILD_DATE UTC date (YYYY-MM-DD), honoring SOURCE_DATE_EPOCH for
//!                    reproducible builds, else the wall clock at build time.
//!   ODYTTY_TARGET    the target triple (from cargo's TARGET env).
//!
//! No external crates: this must build offline (the AUR/Odyssey source builds
//! run with --frozen/--locked and no network). It must ALSO build cleanly from
//! the `git archive` release tarball, which has NO `.git` directory — in that
//! case the live git lookup returns None and the SHA comes from the
//! export-subst token committed as `.git_archival.txt` (see the precedence in
//! `resolve_provenance_sha`).

use std::process::Command;

// Pure SHA-precedence logic, shared verbatim with `tests/provenance.rs` so it
// runs under `cargo test` (a build script's own test module never does).
include!("build_support/provenance.rs");

/// The one file carrying the `git archive` export-subst placeholder. Marked
/// `export-subst` in `.gitattributes`; a plain checkout keeps the literal
/// `$Format:%h$`, the release source tarball carries the substituted hash.
const ARCHIVE_TOKEN_PATH: &str = ".git_archival.txt";

fn main() {
    // Re-run when the commit or working-tree state moves so the embedded SHA /
    // date / dirty flag track reality (see `emit_git_rerun_triggers`).
    emit_git_rerun_triggers();
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // Provenance SHA: validated override -> live git -> git-archive export-subst
    // token -> "unavailable". Re-run when the override env var changes or the
    // token file is re-substituted (a fresh `git archive`).
    println!("cargo:rerun-if-env-changed=ODYTTY_BUILD_SHA");
    println!("cargo:rerun-if-changed={ARCHIVE_TOKEN_PATH}");
    // The included pure-logic file is not auto-tracked once any explicit
    // rerun-if-changed is emitted; watch it so edits there retrigger.
    println!("cargo:rerun-if-changed=build_support/provenance.rs");
    let override_raw = std::env::var("ODYTTY_BUILD_SHA").ok();
    let archive_raw = std::fs::read_to_string(ARCHIVE_TOKEN_PATH).unwrap_or_default();
    let git_sha = resolve_provenance_sha(override_raw.as_deref(), git_short_sha, &archive_raw);
    println!("cargo:rustc-env=ODYTTY_GIT_SHA={git_sha}");

    let build_date = build_date_utc();
    println!("cargo:rustc-env=ODYTTY_BUILD_DATE={build_date}");

    // TARGET is always provided to build scripts by cargo.
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=ODYTTY_TARGET={target}");

    // The rustc the binary was actually built with. Cargo passes the RUSTC path
    // (not its version), so query it. Trims the leading "rustc " for a compact
    // "1.96.0 (ac68faa20 2026-05-25)" string.
    let rustc_version = rustc_version().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ODYTTY_RUSTC_VERSION={rustc_version}");

    embed_windows_icon();
}

/// Emit the `cargo:rerun-if-changed` triggers that keep the embedded provenance
/// (SHA / build date / dirty flag) fresh.
///
/// The naive `rerun-if-changed=.git/HEAD` is NOT enough: `.git/HEAD` only
/// changes on a *branch switch*. A commit on the current branch updates the
/// loose ref file it points at (`.git/refs/heads/<branch>`) or `.git/packed-refs`
/// (when refs are packed) — never `.git/HEAD`. So HEAD alone lets cargo reuse a
/// stale build-script output indefinitely (the operator saw yesterday's SHA on a
/// fresh build). We therefore also watch:
///   - the resolved loose ref file, so a same-branch commit retriggers;
///   - `.git/packed-refs`, so a commit against a packed ref retriggers;
///   - `.git/index`, so staging/unstaging flips the `-dirty` suffix.
///
/// Each extra path is emitted only when it exists, so a missing file can't force
/// an unconditional rerun. `.git/HEAD` stays unconditional (it also carries the
/// commit hash directly in the detached-HEAD case, where there is no ref file to
/// resolve). Missing `.git` entirely (the `git archive` release tarball) simply
/// yields no ref file / index to watch — `git_short_sha` then returns `None` and
/// `resolve_provenance_sha` falls back to the export-subst token or
/// "unavailable". Paths use forward slashes, which cargo accepts on every
/// platform.
fn emit_git_rerun_triggers() {
    let git_dir = std::path::Path::new(".git");

    // Always watch HEAD: it moves on a branch switch and, for a detached HEAD,
    // holds the commit hash itself.
    println!("cargo:rerun-if-changed=.git/HEAD");

    // Watch the ref HEAD points at (loose ref), so a same-branch commit — which
    // updates that file, not HEAD — retriggers the build script.
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD"))
        && let Some(ref_path) = head_ref_path(&head)
    {
        let loose_ref = git_dir.join(&ref_path);
        if loose_ref.exists() {
            // `ref_path` is git's on-disk form (forward slashes); emit it as a
            // relative `.git/` path so cargo hashes the right file.
            println!("cargo:rerun-if-changed=.git/{ref_path}");
        }
    }

    // packed-refs (commit against a packed ref) and index (dirty flag) — watch
    // only when present so a missing file can't force an unconditional rerun.
    for rel in ["packed-refs", "index"] {
        if git_dir.join(rel).exists() {
            println!("cargo:rerun-if-changed=.git/{rel}");
        }
    }
}

/// Parse the contents of `.git/HEAD` and return the ref path it points at,
/// relative to the `.git` directory (e.g. `refs/heads/master`).
///
/// Returns `None` for a detached HEAD (the file holds a raw commit hash, so
/// there is no ref to resolve — watching HEAD itself suffices) or for content
/// that doesn't begin with the `ref:` marker.
fn head_ref_path(head_contents: &str) -> Option<String> {
    let ref_path = head_contents.trim().strip_prefix("ref:")?.trim();
    if ref_path.is_empty() {
        return None;
    }
    Some(ref_path.to_string())
}

/// Embed the application icon into `odytty.exe` as a PE resource (Windows
/// targets only) so Explorer, the taskbar, and Alt-Tab show OdyTTY's icon on the
/// executable itself. The runtime window/title-bar icon is set separately via
/// winit (`src/native/window_icon.rs`).
///
/// CRITICAL: a build script's own `#[cfg(windows)]` reflects the HOST compiling
/// the build script, not the TARGET being built. Gate on the
/// `CARGO_CFG_TARGET_OS` env var cargo sets to the target OS instead, so a
/// Linux/macOS host cross-compiling to Windows still embeds the icon.
///
/// Non-fatal by design: a missing toolchain resource compiler or a transient
/// failure logs a warning and is ignored rather than failing the build — the exe
/// is fully functional without the embedded icon. The `.ico` is committed
/// (`dist/windows/odytty.ico`), so it is present in the `git archive` release
/// tarball too; on the `windows-latest` MSVC runner the bundled `rc.exe`/
/// `llvm-rc` performs the embed.
fn embed_windows_icon() {
    const ICON_PATH: &str = "dist/windows/odytty.ico";
    // Rebuild the resource when the icon art changes.
    println!("cargo:rerun-if-changed={ICON_PATH}");

    let targets_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");

    // `winresource` is a `cfg(windows)` build-dependency, so it only exists in
    // the build graph when the build script itself runs on Windows. The env
    // check restricts the embed to Windows *targets*; the `cfg(windows)` guards
    // the *host* so the call compiles only where the crate is available. A
    // cross-build from a non-Windows host silently skips the embed (the exe
    // still works, just without the embedded file icon); `_ = targets_windows`
    // keeps the binding live on hosts where the `cfg` block is compiled out.
    #[cfg(windows)]
    if targets_windows {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(ICON_PATH);
        if let Err(err) = resource.compile() {
            // Non-fatal: warn and continue with an icon-less exe.
            println!("cargo:warning=odytty: failed to embed Windows icon: {err}");
        }
    }
    let _ = targets_windows;
}

/// `$RUSTC --version`, with the leading "rustc " stripped. Returns `None` if the
/// compiler can't be invoked.
fn rustc_version() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = Command::new(rustc).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8(out.stdout).ok()?.trim().to_string();
    Some(v.strip_prefix("rustc ").unwrap_or(&v).to_string())
}

/// `git rev-parse --short HEAD`, plus a `-dirty` suffix when the working tree
/// has uncommitted changes. Returns `None` if git is missing or this is not a
/// repository (the release-tarball case), so `resolve_provenance_sha` falls back
/// to the export-subst token or "unavailable".
///
/// Guard: only consult git when a `.git` entry exists in the crate root itself
/// (build scripts run with cwd = CARGO_MANIFEST_DIR). A `git archive` tarball has
/// no `.git`, so this returns `None` — even when the tarball is extracted INSIDE
/// another git repo (e.g. `odyssey-build` unpacks into the git-tracked
/// `~/pkgbuilds/` tree). Without this guard, git would walk up to the parent repo
/// and bake in a wrong, misleading SHA.
fn git_short_sha() -> Option<String> {
    if !std::path::Path::new(".git").exists() {
        return None;
    }
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }

    // Dirty check: `git status --porcelain` prints nothing on a clean tree.
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    Some(if dirty { format!("{sha}-dirty") } else { sha })
}

/// UTC build date as `YYYY-MM-DD`. Honors `SOURCE_DATE_EPOCH` (reproducible
/// builds) when set and parseable; otherwise uses the current wall clock.
/// Pure integer date math — no chrono dependency.
fn build_date_utc() -> String {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert a count of days since the Unix epoch to a (year, month, day) UTC
/// civil date. Howard Hinnant's well-known `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::head_ref_path;

    #[test]
    fn symbolic_head_resolves_to_its_ref_path() {
        // The normal on-branch case: HEAD is a symbolic ref with a trailing
        // newline. This is the ref file a same-branch commit updates.
        assert_eq!(
            head_ref_path("ref: refs/heads/master\n").as_deref(),
            Some("refs/heads/master")
        );
        // A slashed branch name is preserved verbatim.
        assert_eq!(
            head_ref_path("ref: refs/heads/feature/tab-bar\n").as_deref(),
            Some("refs/heads/feature/tab-bar")
        );
        // No trailing newline is fine too.
        assert_eq!(
            head_ref_path("ref: refs/heads/main").as_deref(),
            Some("refs/heads/main")
        );
    }

    #[test]
    fn detached_head_hash_has_no_ref_path() {
        // A detached HEAD holds a raw commit hash — no ref to resolve (watching
        // HEAD itself covers it).
        assert_eq!(head_ref_path("9c9fdc0a1b2c3d4e5f\n"), None);
    }

    #[test]
    fn empty_or_malformed_head_is_none() {
        assert_eq!(head_ref_path(""), None);
        assert_eq!(head_ref_path("\n"), None);
        // `ref:` with no path is not a usable ref.
        assert_eq!(head_ref_path("ref:  \n"), None);
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Build-time provenance for the in-app About panel.
//!
//! Emits three `rustc-env` values the binary reads via `env!`:
//!   ODYTTY_GIT_SHA   short commit hash, with "-dirty" suffix on a dirty tree,
//!                    or "unknown" when git/.git is unavailable.
//!   ODYTTY_BUILD_DATE UTC date (YYYY-MM-DD), honoring SOURCE_DATE_EPOCH for
//!                    reproducible builds, else the wall clock at build time.
//!   ODYTTY_TARGET    the target triple (from cargo's TARGET env).
//!
//! No external crates: this must build offline (the AUR/Odyssey source builds
//! run with --frozen/--locked and no network). It must ALSO build cleanly from
//! the `git archive` release tarball, which has NO `.git` directory — in that
//! case the git lookups fail gracefully and the SHA is "unknown".

use std::process::Command;

fn main() {
    // Re-run when HEAD moves so the embedded SHA tracks commits. Guarded: if
    // .git is absent (release tarball), the rerun-if-changed simply points at a
    // path that never changes, which is harmless.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let git_sha = git_short_sha().unwrap_or_else(|| "unknown".to_string());
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
/// repository (the release-tarball case).
///
/// Guard: only consult git when a `.git` entry exists in the crate root itself
/// (build scripts run with cwd = CARGO_MANIFEST_DIR). A `git archive` tarball has
/// no `.git`, so this returns `None` ("unknown") — even when the tarball is
/// extracted INSIDE another git repo (e.g. `odyssey-build` unpacks into the
/// git-tracked `~/pkgbuilds/` tree). Without this guard, git would walk up to the
/// parent repo and bake in a wrong, misleading SHA.
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

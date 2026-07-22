// SPDX-License-Identifier: GPL-3.0-only
//
// Pure commit-SHA provenance resolution shared by `build.rs` and its tests.
//
// This file is `include!`d, not compiled as a crate module: `build.rs`
// includes it to resolve the SHA it embeds for the About panel, and
// `tests/provenance.rs` includes it so the precedence/validation logic runs
// under `cargo test` (a build script's own `#[cfg(test)]` module is never
// executed by the test harness). Plain `//` comments (not `//!`) because the
// include site is not the start of a module. Keep every function here pure and
// dependency-free so both include sites compile it identically.

/// True when `s` is a plausible abbreviated-or-full git object name: 7..=40
/// ASCII hex digits. 7 is git's minimum useful abbreviation; 40 is a full
/// SHA-1 name.
fn is_hex_object_name(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Validate an explicit build-time SHA override (the `ODYTTY_BUILD_SHA` env var
/// that official CI and package builders set to the exact commit).
///
/// Accepts a hex object name (7..=40 hex digits) with an optional `-dirty`
/// suffix, tolerating surrounding whitespace. Returns the trimmed value
/// verbatim (hex core plus any `-dirty`) or `None` when empty or malformed. A
/// malformed override never poisons the embedded SHA: the caller falls through
/// to the next provenance source instead of baking in garbage.
fn validate_override_sha(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let core = trimmed.strip_suffix("-dirty").unwrap_or(trimmed);
    is_hex_object_name(core).then(|| trimmed.to_string())
}

/// Interpret the `git archive` export-subst token read from the placeholder
/// file.
///
/// In a plain checkout the file still holds the literal `$Format:%h$`
/// placeholder (git only substitutes it inside a `git archive` stream), so this
/// returns `None` and the caller falls back to a live git lookup. In the
/// released source tarball git has replaced the placeholder with the
/// abbreviated commit hash, so this returns that hash. Anything that is not a
/// bare hex object name — including the unsubstituted placeholder, which still
/// contains `$` — is rejected.
fn archive_subst_sha(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('$') {
        return None;
    }
    is_hex_object_name(trimmed).then(|| trimmed.to_string())
}

/// Resolve the commit SHA embedded in the About panel, in strict precedence:
///   1. a validated explicit override (`ODYTTY_BUILD_SHA`, official/package CI);
///   2. a live git short SHA (+`-dirty`) from a real checkout;
///   3. the `git archive` export-subst token (source-tarball builds, no `.git`);
///   4. the human-facing fallback `"unavailable"` (never `"unknown"`).
///
/// `git_lookup` is consulted only when there is no valid override, so an
/// override build performs zero git work, and the archive token is read only
/// when neither an override nor a live checkout is available.
fn resolve_provenance_sha(
    override_raw: Option<&str>,
    git_lookup: impl FnOnce() -> Option<String>,
    archive_raw: &str,
) -> String {
    if let Some(sha) = override_raw.and_then(validate_override_sha) {
        return sha;
    }
    if let Some(sha) = git_lookup() {
        return sha;
    }
    if let Some(sha) = archive_subst_sha(archive_raw) {
        return sha;
    }
    "unavailable".to_string()
}

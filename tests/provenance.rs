// SPDX-License-Identifier: GPL-3.0-only
//! Commit-SHA provenance resolution + `git archive` export-subst integration.
//!
//! `build.rs` embeds the About-panel commit SHA via `resolve_provenance_sha`
//! (override -> live git -> git-archive token -> "unavailable"). That logic
//! lives in `build_support/provenance.rs`, which the build script `include!`s;
//! a build script's own `#[cfg(test)]` module never runs under the test
//! harness, so this integration test `include!`s the same file and exercises
//! it directly. The final test drives a real `git archive` extraction with no
//! `.git` present, proving the export-subst path source packages rely on.

// These are used only by the Unix-gated `git archive` integration test; the
// pure-function tests need no imports. Gating them keeps the windows-latest
// leg free of unused-import/dead-code warnings.
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

// Same pure logic the build script compiles. Included (not `mod`-imported)
// because the source is a shared build-support fragment, not a crate module.
include!("../build_support/provenance.rs");

// ---- validate_override_sha -------------------------------------------------

#[test]
fn override_accepts_valid_hex_names_and_dirty_suffix() {
    // Full 40-char SHA (the exact github.sha official CI supplies) is kept
    // verbatim.
    let full = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(validate_override_sha(full).as_deref(), Some(full));
    // A short (abbreviated) name is fine too.
    assert_eq!(validate_override_sha("abc1234").as_deref(), Some("abc1234"));
    // Uppercase hex is accepted.
    assert_eq!(validate_override_sha("ABC1234").as_deref(), Some("ABC1234"));
    // The `-dirty` suffix is preserved.
    assert_eq!(
        validate_override_sha("abc1234-dirty").as_deref(),
        Some("abc1234-dirty")
    );
    // Surrounding whitespace is trimmed but the value is otherwise verbatim.
    assert_eq!(
        validate_override_sha("  abc1234\n").as_deref(),
        Some("abc1234")
    );
}

#[test]
fn override_rejects_malformed_values() {
    assert_eq!(validate_override_sha(""), None);
    assert_eq!(validate_override_sha("   "), None);
    // Too short to be a useful git abbreviation (< 7).
    assert_eq!(validate_override_sha("abc123"), None);
    // Too long to be a SHA-1 name (> 40).
    assert_eq!(validate_override_sha(&"a".repeat(41)), None);
    // Non-hex characters (a branch name, a ref, an injection attempt).
    assert_eq!(validate_override_sha("refs/heads/master"), None);
    assert_eq!(validate_override_sha("nothexbutlong!!"), None);
    assert_eq!(validate_override_sha("abc1234; rm -rf /"), None);
    // The bare `-dirty` marker with no hex core is not a SHA.
    assert_eq!(validate_override_sha("-dirty"), None);
    // The unsubstituted archive placeholder must never validate as an override.
    assert_eq!(validate_override_sha("$Format:%h$"), None);
}

// ---- archive_subst_sha -----------------------------------------------------

#[test]
fn archive_token_substituted_is_accepted() {
    assert_eq!(archive_subst_sha("e4fba50").as_deref(), Some("e4fba50"));
    // Trailing newline from the file is trimmed.
    assert_eq!(archive_subst_sha("e4fba50\n").as_deref(), Some("e4fba50"));
    let full = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(archive_subst_sha(full).as_deref(), Some(full));
}

#[test]
fn archive_token_unsubstituted_or_garbage_is_rejected() {
    // A plain checkout keeps the literal placeholder — it still contains `$`.
    assert_eq!(archive_subst_sha("$Format:%h$"), None);
    assert_eq!(archive_subst_sha("$Format:%h$\n"), None);
    assert_eq!(archive_subst_sha(""), None);
    assert_eq!(archive_subst_sha("   \n"), None);
    // Not a bare hex object name.
    assert_eq!(archive_subst_sha("not-a-sha"), None);
}

// ---- resolve_provenance_sha precedence -------------------------------------

#[test]
fn precedence_prefers_a_valid_override_and_skips_git() {
    let mut git_called = false;
    let sha = resolve_provenance_sha(
        Some("abc1234"),
        || {
            git_called = true;
            Some("deadbee".to_string())
        },
        "e4fba50",
    );
    assert_eq!(sha, "abc1234");
    assert!(!git_called, "a valid override must perform zero git work");
}

#[test]
fn precedence_falls_through_invalid_override_to_git() {
    let sha = resolve_provenance_sha(
        Some("not-a-sha"),
        || Some("deadbee-dirty".to_string()),
        "e4fba50",
    );
    assert_eq!(sha, "deadbee-dirty");
}

#[test]
fn precedence_uses_archive_token_when_no_override_and_no_git() {
    let sha = resolve_provenance_sha(None, || None, "e4fba50");
    assert_eq!(sha, "e4fba50");
}

#[test]
fn precedence_uses_archive_token_when_override_invalid_and_no_git() {
    let sha = resolve_provenance_sha(Some("bogus!!"), || None, "e4fba50");
    assert_eq!(sha, "e4fba50");
}

#[test]
fn precedence_final_fallback_is_unavailable_never_unknown() {
    // No override, no git, unsubstituted archive token (a real checkout with no
    // git binary would land here).
    let sha = resolve_provenance_sha(None, || None, "$Format:%h$");
    assert_eq!(sha, "unavailable");
    assert_ne!(sha, "unknown");
}

// ---- real git archive extraction with no .git ------------------------------

#[test]
#[cfg(unix)]
fn git_archive_substitutes_the_token_and_drops_dot_git() {
    // git + tar are present on the Linux/macOS CI legs and on the Linux dev box;
    // if git cannot even be spawned (a stripped environment), skip rather than
    // fail on a missing precondition.
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping: git not available");
        return;
    }

    let tmp = TempDir::new("odytty-prov");
    let repo = tmp.path();

    // A repository shaped like ours: the export-subst placeholder file plus the
    // matching .gitattributes rule, exactly as committed at the project root.
    fs::write(repo.join(".git_archival.txt"), "$Format:%h$\n").unwrap();
    fs::write(
        repo.join(".gitattributes"),
        ".git_archival.txt export-subst\n",
    )
    .unwrap();
    fs::write(repo.join("README"), "fixture\n").unwrap();

    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "provenance@test.invalid"]);
    git(repo, &["config", "user.name", "provenance-test"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "fixture commit"]);

    // The working-tree copy is still the LITERAL placeholder (git substitutes
    // only inside a `git archive` stream), so the archive path is genuinely
    // exercised — not the checkout that would win here.
    let worktree_token = fs::read_to_string(repo.join(".git_archival.txt")).unwrap();
    assert!(worktree_token.contains("$Format:"));
    assert_eq!(archive_subst_sha(&worktree_token), None);

    let expected = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert!(!expected.is_empty());

    // Produce the release-shaped source archive and extract it into a fresh
    // directory with NO .git — the exact shape AUR/Homebrew build from.
    let archive = repo.join("archive.tar");
    git(
        repo,
        &[
            "archive",
            "--format=tar",
            "--output",
            archive.to_str().unwrap(),
            "HEAD",
        ],
    );
    let extract = repo.join("extract");
    fs::create_dir(&extract).unwrap();
    let tar = Command::new("tar")
        .args(["-xf", archive.to_str().unwrap(), "-C"])
        .arg(&extract)
        .status()
        .expect("run tar");
    assert!(tar.success(), "tar extraction failed");

    // The archive carries no .git — a live git lookup is impossible here.
    assert!(
        !extract.join(".git").exists(),
        "extracted source tarball must not contain a .git directory"
    );

    // The placeholder was substituted to the real short SHA, and the resolver
    // reads it as the archive-fallback provenance.
    let extracted_token = fs::read_to_string(extract.join(".git_archival.txt")).unwrap();
    assert_eq!(extracted_token.trim(), expected);
    assert_eq!(
        archive_subst_sha(&extracted_token).as_deref(),
        Some(expected.as_str())
    );

    // End to end: no override, no git in the extracted tree, substituted token
    // -> the resolver reports the real commit SHA (never "unavailable").
    let resolved = resolve_provenance_sha(None, || None, &extracted_token);
    assert_eq!(resolved, expected);
}

#[cfg(unix)]
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|err| panic!("spawn git {args:?}: {err}"));
    assert!(status.success(), "git {args:?} failed");
}

/// Minimal self-cleaning temp directory (mirrors the helper in `tests/cli.rs`;
/// the integration-test crates are separate, so it cannot be shared).
#[cfg(unix)]
struct TempDir {
    path: PathBuf,
}

#[cfg(unix)]
impl TempDir {
    fn new(prefix: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{seq}", std::process::id()));
        fs::create_dir(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

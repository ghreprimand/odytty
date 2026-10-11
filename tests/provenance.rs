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
#[path = "../src/test_dirs/allocation.rs"]
pub mod scratch_dirs;

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
    // A plain checkout keeps the literal placeholder - it still contains `$`.
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
    // fail on a missing precondition. The skip is a fixed, countable token: the
    // test returning success is not a pass of its assertions.
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!(
            "odytty-test-outcome: unavailable-hardware git test=git_archive_substitutes_the_token_and_drops_dot_git"
        );
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

    git(repo, &["init", "-q", "--template="]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "fixture commit"]);

    // The working-tree copy is still the LITERAL placeholder (git substitutes
    // only inside a `git archive` stream), so the archive path is genuinely
    // exercised - not the checkout that would win here.
    let worktree_token = fs::read_to_string(repo.join(".git_archival.txt")).unwrap();
    assert!(worktree_token.contains("$Format:"));
    assert_eq!(archive_subst_sha(&worktree_token), None);

    let expected = String::from_utf8(
        hermetic_git(repo)
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert!(!expected.is_empty());

    // Produce the release-shaped source archive and extract it into a fresh
    // directory with NO .git - the exact shape AUR/Homebrew build from.
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

    // The archive carries no .git - a live git lookup is impossible here.
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
    let status = hermetic_git(dir)
        .args(args)
        .status()
        .unwrap_or_else(|err| panic!("spawn git {args:?}: {err}"));
    assert!(status.success(), "git {args:?} failed");
}

/// A `git` command that reads nothing from the machine it runs on: the
/// environment is cleared (no inherited `GIT_*` variables), the home and XDG
/// directories are `dir`, the system and global configuration are disabled, and
/// the author and committer are fixed. Signing, hooks, templates, attributes
/// overrides, `core.autocrlf` and the like set in a developer's or runner's git
/// configuration therefore cannot change the archive this test inspects.
#[cfg(unix)]
fn hermetic_git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "provenance-test")
        .env("GIT_AUTHOR_EMAIL", "provenance@test.invalid")
        .env("GIT_COMMITTER_NAME", "provenance-test")
        .env("GIT_COMMITTER_EMAIL", "provenance@test.invalid");
    command
}

/// Environment the parent of the hostile-parent check injects: a process-level
/// configuration injection and the repository-selecting variables a git hook
/// exports. Set on a child process only; this test process is never modified.
#[cfg(unix)]
const HOSTILE_PARENT_ENV: [(&str, &str); 7] = [
    ("GIT_CONFIG_COUNT", "1"),
    ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
    ("GIT_CONFIG_VALUE_0", "true"),
    ("GIT_CONFIG_PARAMETERS", "'core.hooksPath=/hostile'"),
    ("GIT_DIR", "/nonexistent-outer-repository"),
    ("GIT_WORK_TREE", "/nonexistent-outer-worktree"),
    ("GIT_INDEX_FILE", "/nonexistent-outer-index"),
];

/// Variable that turns [`the_hostile_parent_listing`] from a no-op into the
/// child half of the check below.
#[cfg(unix)]
const HOSTILE_CHILD_MARKER: &str = "ODYTTY_PROVENANCE_HOSTILE_CHILD";

/// The fixture repository sees only its own configuration: no system, global or
/// inherited setting reaches `git`, whatever the machine has configured. The
/// check re-runs this test binary as a child whose environment carries
/// [`HOSTILE_PARENT_ENV`], so the cleared environment of [`hermetic_git`] is
/// what keeps the injection out, and a plain `git` in the same child shows that
/// the injection is effective.
#[test]
#[cfg(unix)]
fn the_fixture_git_reads_no_configuration_from_the_machine() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!(
            "odytty-test-outcome: unavailable-hardware git test=the_fixture_git_reads_no_configuration_from_the_machine"
        );
        return;
    }
    let mut child = Command::new(std::env::current_exe().expect("this test binary"));
    child
        .args([
            "--exact",
            "the_hostile_parent_listing",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HOSTILE_CHILD_MARKER, "1");
    for (key, value) in HOSTILE_PARENT_ENV {
        child.env(key, value);
    }
    let output = child.output().expect("run the hostile-parent child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let section = |name: &str| -> String {
        let begin = format!("<<{name}\n");
        let start = stdout
            .find(&begin)
            .unwrap_or_else(|| panic!("no {name} section in:\n{stdout}"))
            + begin.len();
        let end = stdout[start..]
            .find(&format!(">>{name}"))
            .unwrap_or_else(|| panic!("unterminated {name} section"));
        stdout[start..start + end].to_owned()
    };
    // Control: the injected configuration reaches a plain `git`.
    assert!(
        section("plain").contains("commit.gpgsign"),
        "the injection must be visible to an unprotected git"
    );
    // The fixture's git sees only the repository's own file.
    let listing = section("hermetic");
    for line in listing.lines() {
        assert!(
            line.starts_with("file:.git/config\t"),
            "configuration from outside the fixture repository: {line}"
        );
    }
    assert!(!listing.contains("commit.gpgsign"));
    assert!(!listing.contains("hooksPath"));
}

/// Child half of [`the_fixture_git_reads_no_configuration_from_the_machine`]:
/// prints the configuration a plain `git` and the fixture's `git` see. It does
/// nothing unless the parent set [`HOSTILE_CHILD_MARKER`].
#[test]
#[cfg(unix)]
fn the_hostile_parent_listing() {
    if std::env::var_os(HOSTILE_CHILD_MARKER).is_none() {
        return;
    }
    let tmp = TempDir::new("odytty-prov-config");
    let repo = tmp.path();
    git(repo, &["init", "-q", "--template="]);
    let list = |mut command: Command| {
        let output = command
            .args(["config", "--list", "--show-origin"])
            .output()
            .expect("run git config");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let mut plain = Command::new("git");
    plain
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    println!("<<plain\n{}>>plain", list(plain));
    println!("<<hermetic\n{}>>hermetic", list(hermetic_git(repo)));
}

/// Self-cleaning directory using the allocator shared with other test crates.
#[cfg(unix)]
struct TempDir {
    path: PathBuf,
}

#[cfg(unix)]
impl TempDir {
    fn new(prefix: &str) -> Self {
        let path = scratch_dirs::fresh_temp_dir(prefix);
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

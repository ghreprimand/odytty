// SPDX-License-Identifier: GPL-3.0-only
//! INTERACTIVE-PATHS — the production stat-gate probe.
//!
//! The pure path engine (`crate::paths`) is std-only and deliberately never
//! touches the filesystem; its single I/O seam is the [`ResolveProbe`] trait.
//! This module supplies the one production implementation, [`FsResolveProbe`],
//! which lives in `native/` precisely so `src/paths/` stays pure. The probe is
//! a zero-field struct constructed at the hover site (Phase 7) — its only job is
//! to classify an absolute path as a file or directory, or report that it does
//! not exist, via a single `symlink_metadata` call.
//!
//! `symlink_metadata` (not `metadata`) is used so a symlink is classified by the
//! link itself rather than its target — no traversal, no following into a
//! possibly-hostile target, and no surprise on a dangling link. The call only
//! runs when `interactive_paths` is on AND a syntactic path span sits under the
//! pointer, so the default (feature-off) path makes zero `stat` calls.

use crate::paths::{FsKind, ResolveProbe, Resolved};

use super::platform_opener::{OpenerOs, open_default_argv};

/// Production stat-gate: classifies an absolute path via `std::fs`. Only the
/// `cfg(not(test))` hover arm constructs it; under the test target the hover
/// path uses [`MapProbe`] instead, so the struct is (correctly) unused there.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct FsResolveProbe;

impl ResolveProbe for FsResolveProbe {
    fn classify(&self, abs_path: &str) -> Option<FsKind> {
        let meta = std::fs::symlink_metadata(abs_path).ok()?;
        Some(if meta.is_dir() {
            FsKind::Dir
        } else {
            FsKind::File
        })
    }
}

// ---------------------------------------------------------------------------
// C3 — open-action dispatch (argv-only, never a shell string).
//
// These are PURE functions: they build the `argv` vector to open a resolved
// path and return it. The single spawn point ([`spawn_detached`]) is the only
// thing that actually launches a process, so every dispatch path can be
// unit-tested by asserting the vector without ever executing it. No path,
// line, or column is ever interpolated into a shell — a filename containing
// `;`, `$()`, backticks, or spaces is inert because it is a single argv
// element. See `docs/interactive-paths-design.md` §3 (dispatch table) and §4
// (editor matrix).
// ---------------------------------------------------------------------------

/// The decoded image backing the C4 in-terminal viewer overlay. The App keeps
/// it while the `ImageView` overlay is open so a window resize can recompute the
/// centered fit-rect from the same pixels without re-reading or re-decoding the
/// file. Tightly-packed RGBA8, `width`×`height`.
pub(in crate::native) struct ImageOverlayState {
    pub(in crate::native) rgba: Vec<u8>,
    pub(in crate::native) width: u32,
    pub(in crate::native) height: u32,
}

/// The single argv-only spawn point. Routes BOTH the OSC 8 hyperlink open and
/// every interactive-path open through one auditable place: a detached child
/// with null stdio, launched from an explicit `argv` vector — never `sh -c`,
/// never a shell string. The first element is the program; the rest are
/// arguments.
///
/// Returns `Ok(())` when the child was spawned, `Err(..)` when the spawn failed
/// (most commonly a missing opener binary — `xdg-open`/`open` not installed or
/// not on `PATH`, surfaced as `ErrorKind::NotFound`) or the argv was empty. P0-2:
/// the caller uses this to surface a VISIBLE, non-blocking notice on failure
/// instead of the old silent no-op that made a broken opener indistinguishable
/// from "feature off". The success path must NOT fire any notice.
///
/// An empty argv is reported as `ErrorKind::InvalidInput` (defensive — the
/// dispatch functions never return one).
///
/// REAPING (TEST-HANG fix): the spawned child is handed to a small detached
/// reaper thread that blocks in `Child::wait` until the opener exits. Dropping
/// the `Child` handle (the pre-fix behaviour) never waits, so on unix every
/// opener spawn left a ZOMBIE process until the whole app exited — zombies
/// accumulated one per open-click in a long-lived session, and the test suite's
/// `true` spawn showed up as the "leftover `true` child" in a wedged
/// `cargo test` process tree. The reaper thread is cheap (opens are rare,
/// user-initiated events), does not block this call, and does not delay process
/// exit (process teardown never joins detached threads). If the reaper thread
/// itself cannot be spawned we degrade to the old drop-without-wait behaviour
/// rather than failing the open.
#[must_use = "a failed open must surface a visible notice, not be silently dropped"]
pub(crate) fn spawn_detached(argv: &[String]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let Some((program, args)) = argv.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty argv",
        ));
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let _ = std::thread::Builder::new()
        .name("odytty-open-reaper".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

/// Build the argv vector that opens a [`Resolved`] path (design §3 dispatch
/// table). Pure — returns the vector; the caller spawns it via
/// [`spawn_detached`]. `os` selects the platform default opener
/// ([`open_default_argv`]): Linux `xdg-open`, macOS `open`.
///
/// * Directory → the platform default opener on `<abs>` (the desktop file
///   manager).
/// * File without a `:line` suffix → the platform default opener on `<abs>`
///   (the default app).
/// * File with a `:line[:col]` suffix → the editor matrix ([`editor_argv`]),
///   selecting the editor by precedence: the configured `editor_override`
///   (settings `interactive_paths_editor`), else `$EDITOR`/`$VISUAL`, else the
///   platform default opener (position lost — the file still opens).
pub(crate) fn path_open_argv(
    resolved: &Resolved,
    editor_override: &str,
    editor_env: Option<&str>,
    os: OpenerOs,
) -> Vec<String> {
    match resolved.kind {
        FsKind::Dir => open_default_argv(os, &resolved.abs),
        FsKind::File => match resolved.line {
            None => open_default_argv(os, &resolved.abs),
            Some(line) => {
                let override_trimmed = editor_override.trim();
                let spec = if !override_trimmed.is_empty() {
                    Some(override_trimmed)
                } else {
                    editor_env.map(str::trim).filter(|env| !env.is_empty())
                };
                match spec {
                    Some(spec) => editor_argv(spec, &resolved.abs, line, resolved.col, os),
                    // No editor configured or in the environment: open the file
                    // with the default app and accept that the line/col is lost.
                    None => open_default_argv(os, &resolved.abs),
                }
            }
        },
    }
}

/// The `file://<abs>` URI for the "Copy File" menu item. The clipboard is
/// text-only, so "Copy File" copies this URI string; it pastes into desktop
/// file managers as a file reference. Pure.
pub(crate) fn file_uri(abs: &str) -> String {
    format!("file://{abs}")
}

/// The argv vector to open `abs` at `line`(`:col`) with the given editor `spec`
/// (design §4 editor matrix). Pure; never spawns.
///
/// `spec` is either:
/// * an **argv template** containing any of `{file}` / `{line}` / `{col}` — it
///   is whitespace-split into tokens **first**, then each placeholder is
///   substituted inside each token, so a substituted path with spaces stays one
///   argv element; or
/// * an **editor command** (program plus optional args, e.g. `"code --wait"`) —
///   it is whitespace-tokenized (never shell-evaluated), the basename of token 0
///   is matched against the known-editor matrix, and any remaining tokens are
///   carried through as leading arguments before the matrix's position flag and
///   file.
///
/// Unknown editors degrade to `[<spec…>, <abs>]` (open the file, lose the
/// position) rather than guessing a flag that might be read as a filename.
///
/// `os` is only consulted on the defensive empty-spec path (callers always pass
/// a non-empty spec), where it selects the platform default opener.
pub(crate) fn editor_argv(
    spec: &str,
    abs: &str,
    line: u32,
    col: Option<u32>,
    os: OpenerOs,
) -> Vec<String> {
    // Template form: substitute placeholders into pre-split tokens.
    if spec.contains("{file}") || spec.contains("{line}") || spec.contains("{col}") {
        let col_str = col.map(|c| c.to_string()).unwrap_or_default();
        let line_str = line.to_string();
        return spec
            .split_whitespace()
            .map(|token| {
                token
                    .replace("{file}", abs)
                    .replace("{line}", &line_str)
                    .replace("{col}", &col_str)
            })
            .collect();
    }

    // Command form: tokenize (never shell-eval), basename-match token 0.
    let mut tokens = spec.split_whitespace().map(str::to_owned);
    let Some(program) = tokens.next() else {
        // Empty spec: should not happen (callers pass a non-empty spec), but
        // degrade safely to opening the file with the default app.
        return open_default_argv(os, abs);
    };
    let extra: Vec<String> = tokens.collect();
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&program)
        .to_ascii_lowercase();

    // Position strings shared by several matrix rows.
    let file_line_col = match col {
        Some(c) => format!("{abs}:{line}:{c}"),
        None => format!("{abs}:{line}"),
    };

    let mut argv = vec![program.clone()];
    argv.extend(extra);
    match base.as_str() {
        // vim family: `+call cursor(L,C)` honors the column; line-only uses `+L`.
        "vim" | "nvim" | "vi" => {
            match col {
                Some(c) => argv.push(format!("+call cursor({line},{c})")),
                None => argv.push(format!("+{line}")),
            }
            argv.push(abs.to_owned());
        }
        // VS Code: `--goto F:L:C`.
        "code" => {
            argv.push("--goto".to_owned());
            argv.push(file_line_col);
        }
        // Emacs: `+L:C F` (or `+L F`).
        "emacs" | "emacsclient" => {
            match col {
                Some(c) => argv.push(format!("+{line}:{c}")),
                None => argv.push(format!("+{line}")),
            }
            argv.push(abs.to_owned());
        }
        // Helix / Sublime / micro: `F:L:C` positional argument.
        "hx" | "helix" | "subl" | "sublime" | "micro" => {
            argv.push(file_line_col);
        }
        // nano: `+L,C F` (or `+L F`).
        "nano" => {
            match col {
                Some(c) => argv.push(format!("+{line},{c}")),
                None => argv.push(format!("+{line}")),
            }
            argv.push(abs.to_owned());
        }
        // Unknown editor: open the file, drop the position.
        _ => {
            argv.push(abs.to_owned());
        }
    }
    argv
}

/// Synthetic, in-memory stat-gate for native tests — the ONLY "filesystem" any
/// native hover test touches. Mirrors the engine's internal `MapProbe` but is
/// reachable from the `native` test modules so they can inject a fixed fs map
/// instead of reaching the real filesystem.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MapProbe(std::collections::HashMap<String, FsKind>);

#[cfg(test)]
impl MapProbe {
    /// Build a synthetic fs from `(absolute_path, kind)` entries.
    pub(crate) fn new<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (&'static str, FsKind)>,
    {
        MapProbe(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }
}

#[cfg(test)]
impl ResolveProbe for MapProbe {
    fn classify(&self, abs_path: &str) -> Option<FsKind> {
        self.0.get(abs_path).copied()
    }
}

#[cfg(test)]
mod dispatch_tests {
    //! Pure argv-construction tests for C3. Every case asserts the built
    //! `argv` vector; NONE spawns a process (the spawn lives behind
    //! [`spawn_detached`], which is never invoked here). Synthetic paths only —
    //! no real filesystem, no real home paths.
    use super::*;

    // The dispatch/editor matrix is OS-agnostic except for the default-opener
    // fallback; these wrappers pin the Linux branch so each case still asserts a
    // concrete argv. The per-OS opener program itself is covered in
    // `platform_opener::tests` (both branches).
    fn open_argv_lin(r: &Resolved, ovr: &str, env: Option<&str>) -> Vec<String> {
        path_open_argv(r, ovr, env, OpenerOs::Linux)
    }
    fn editor_argv_lin(spec: &str, abs: &str, line: u32, col: Option<u32>) -> Vec<String> {
        editor_argv(spec, abs, line, col, OpenerOs::Linux)
    }

    fn file(abs: &str, line: Option<u32>, col: Option<u32>) -> Resolved {
        Resolved {
            abs: abs.to_owned(),
            kind: FsKind::File,
            line,
            col,
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

    // --- dispatch table (path_open_argv) -----------------------------------

    #[test]
    fn file_without_line_opens_with_xdg_open() {
        let r = file("/proj/src/main.rs", None, None);
        assert_eq!(
            open_argv_lin(&r, "", None),
            vec!["xdg-open".to_owned(), "/proj/src/main.rs".to_owned()]
        );
    }

    #[test]
    fn directory_opens_with_xdg_open() {
        let r = dir("/proj/src");
        assert_eq!(
            open_argv_lin(&r, "", Some("vim")),
            vec!["xdg-open".to_owned(), "/proj/src".to_owned()]
        );
    }

    #[test]
    fn file_with_line_uses_editor_env_when_no_override() {
        let r = file("/proj/src/main.rs", Some(42), Some(10));
        assert_eq!(
            open_argv_lin(&r, "", Some("nvim")),
            vec![
                "nvim".to_owned(),
                "+call cursor(42,10)".to_owned(),
                "/proj/src/main.rs".to_owned()
            ]
        );
    }

    #[test]
    fn override_takes_precedence_over_editor_env() {
        let r = file("/proj/a.rs", Some(7), None);
        // Override = code; env = vim. Override wins.
        assert_eq!(
            open_argv_lin(&r, "code", Some("vim")),
            vec![
                "code".to_owned(),
                "--goto".to_owned(),
                "/proj/a.rs:7".to_owned()
            ]
        );
    }

    #[test]
    fn file_with_line_and_no_editor_falls_back_to_xdg_open() {
        let r = file("/proj/a.rs", Some(7), Some(3));
        // No override, no env → open the file, lose the position.
        assert_eq!(
            open_argv_lin(&r, "", None),
            vec!["xdg-open".to_owned(), "/proj/a.rs".to_owned()]
        );
        // A whitespace-only env is treated as unset.
        assert_eq!(
            open_argv_lin(&r, "   ", Some("  ")),
            vec!["xdg-open".to_owned(), "/proj/a.rs".to_owned()]
        );
    }

    // --- editor matrix (editor_argv) ---------------------------------------

    #[test]
    fn vim_family_uses_call_cursor_with_col() {
        for ed in ["vim", "nvim", "vi"] {
            assert_eq!(
                editor_argv_lin(ed, "/p/f.rs", 12, Some(5)),
                vec![
                    ed.to_owned(),
                    "+call cursor(12,5)".to_owned(),
                    "/p/f.rs".to_owned()
                ]
            );
            assert_eq!(
                editor_argv_lin(ed, "/p/f.rs", 12, None),
                vec![ed.to_owned(), "+12".to_owned(), "/p/f.rs".to_owned()]
            );
        }
    }

    #[test]
    fn vscode_uses_goto() {
        assert_eq!(
            editor_argv_lin("code", "/p/f.rs", 12, Some(5)),
            vec![
                "code".to_owned(),
                "--goto".to_owned(),
                "/p/f.rs:12:5".to_owned()
            ]
        );
        assert_eq!(
            editor_argv_lin("code", "/p/f.rs", 12, None),
            vec![
                "code".to_owned(),
                "--goto".to_owned(),
                "/p/f.rs:12".to_owned()
            ]
        );
    }

    #[test]
    fn emacs_uses_plus_line_col() {
        assert_eq!(
            editor_argv_lin("emacs", "/p/f.rs", 12, Some(5)),
            vec!["emacs".to_owned(), "+12:5".to_owned(), "/p/f.rs".to_owned()]
        );
        assert_eq!(
            editor_argv_lin("emacsclient", "/p/f.rs", 12, None),
            vec![
                "emacsclient".to_owned(),
                "+12".to_owned(),
                "/p/f.rs".to_owned()
            ]
        );
    }

    #[test]
    fn helix_sublime_micro_use_positional() {
        for ed in ["hx", "helix", "subl", "sublime", "micro"] {
            assert_eq!(
                editor_argv_lin(ed, "/p/f.rs", 12, Some(5)),
                vec![ed.to_owned(), "/p/f.rs:12:5".to_owned()]
            );
            assert_eq!(
                editor_argv_lin(ed, "/p/f.rs", 12, None),
                vec![ed.to_owned(), "/p/f.rs:12".to_owned()]
            );
        }
    }

    #[test]
    fn nano_uses_plus_line_comma_col() {
        assert_eq!(
            editor_argv_lin("nano", "/p/f.rs", 12, Some(5)),
            vec!["nano".to_owned(), "+12,5".to_owned(), "/p/f.rs".to_owned()]
        );
        assert_eq!(
            editor_argv_lin("nano", "/p/f.rs", 12, None),
            vec!["nano".to_owned(), "+12".to_owned(), "/p/f.rs".to_owned()]
        );
    }

    #[test]
    fn unknown_editor_opens_file_loses_position() {
        assert_eq!(
            editor_argv_lin("kak", "/p/f.rs", 12, Some(5)),
            vec!["kak".to_owned(), "/p/f.rs".to_owned()]
        );
    }

    #[test]
    fn basename_match_ignores_absolute_program_path() {
        // A full path to the editor still matches the matrix by basename, but
        // the original (full-path) program is kept as argv[0].
        assert_eq!(
            editor_argv_lin("/usr/bin/nvim", "/p/f.rs", 9, None),
            vec![
                "/usr/bin/nvim".to_owned(),
                "+9".to_owned(),
                "/p/f.rs".to_owned()
            ]
        );
    }

    #[test]
    fn editor_with_args_is_tokenized_not_shell_evaluated() {
        // "code --wait" → program=code, leading arg --wait, then --goto.
        assert_eq!(
            editor_argv_lin("code --wait", "/p/f.rs", 3, Some(1)),
            vec![
                "code".to_owned(),
                "--wait".to_owned(),
                "--goto".to_owned(),
                "/p/f.rs:3:1".to_owned()
            ]
        );
        // "emacsclient -nw" → program=emacsclient, leading arg -nw.
        assert_eq!(
            editor_argv_lin("emacsclient -nw", "/p/f.rs", 3, None),
            vec![
                "emacsclient".to_owned(),
                "-nw".to_owned(),
                "+3".to_owned(),
                "/p/f.rs".to_owned()
            ]
        );
    }

    #[test]
    fn template_splits_then_substitutes() {
        assert_eq!(
            editor_argv_lin(
                "myed --line {line} --col {col} {file}",
                "/p/f.rs",
                8,
                Some(2)
            ),
            vec![
                "myed".to_owned(),
                "--line".to_owned(),
                "8".to_owned(),
                "--col".to_owned(),
                "2".to_owned(),
                "/p/f.rs".to_owned()
            ]
        );
    }

    #[test]
    fn template_keeps_path_with_spaces_as_one_element() {
        // The path has a space; because the split happens on the *template*
        // before substitution, the substituted path stays a single argv element.
        assert_eq!(
            editor_argv_lin("code --goto {file}:{line}", "/p/my dir/f.rs", 4, None),
            vec![
                "code".to_owned(),
                "--goto".to_owned(),
                "/p/my dir/f.rs:4".to_owned()
            ]
        );
    }

    #[test]
    fn command_form_keeps_path_with_spaces_as_one_element() {
        // Even in the matrix (non-template) form the file is one argv element.
        let argv = editor_argv_lin("vim", "/p/my dir/f.rs", 4, Some(2));
        assert_eq!(argv.last().unwrap(), "/p/my dir/f.rs");
        assert_eq!(argv.len(), 3);
    }

    #[test]
    fn template_col_placeholder_empty_when_no_col() {
        // {col} with no column resolves to an empty string token.
        assert_eq!(
            editor_argv_lin("ed +{line}:{col} {file}", "/p/f.rs", 5, None),
            vec!["ed".to_owned(), "+5:".to_owned(), "/p/f.rs".to_owned()]
        );
    }

    // --- copy/reveal helpers -----------------------------------------------

    #[test]
    fn file_uri_prefixes_scheme() {
        assert_eq!(file_uri("/proj/a.rs"), "file:///proj/a.rs");
    }

    // Reveal argv (parent dir on Linux, `open -R <file>` on macOS) now lives in
    // `platform_opener::reveal_argv`; its per-OS behaviour is tested there.

    // --- per-OS opener selection -------------------------------------------

    #[test]
    fn path_open_argv_uses_macos_open_on_macos() {
        // The dispatch honours the OS parameter: a plain file and a directory
        // both open with `open` on macOS rather than `xdg-open`.
        let f = file("/proj/src/main.rs", None, None);
        assert_eq!(
            path_open_argv(&f, "", None, OpenerOs::Macos),
            vec!["open".to_owned(), "/proj/src/main.rs".to_owned()]
        );
        let d = dir("/proj/src");
        assert_eq!(
            path_open_argv(&d, "", None, OpenerOs::Macos),
            vec!["open".to_owned(), "/proj/src".to_owned()]
        );
        // The editor fallback (line present, no editor configured) also routes
        // through the macOS opener.
        let fl = file("/proj/a.rs", Some(7), None);
        assert_eq!(
            path_open_argv(&fl, "", None, OpenerOs::Macos),
            vec!["open".to_owned(), "/proj/a.rs".to_owned()]
        );
    }
}

// TEST-HANG regression: `spawn_detached` must not leave zombie children. The
// original code dropped the `Child` handle without ever waiting, so every
// opener spawn (and every test exercising the spawn seam) left a zombie until
// the whole process exited — the "leftover `true` child" seen in the wedged
// `cargo test` process tree. Linux-only: zombie detection reads /proc.
#[cfg(all(test, target_os = "linux"))]
mod spawn_reap_tests {
    use super::spawn_detached;
    use std::time::{Duration, Instant};

    /// True while ANY direct child of this process named `comm` exists in any
    /// state (running or zombie). Reads `/proc/<pid>/stat` for every numeric
    /// /proc entry; comm is parenthesised in field 2, state is field 3 (after
    /// the closing paren, immune to spaces in comm), ppid is field 4.
    fn have_child_named(comm: &str) -> bool {
        let my_pid = std::process::id();
        let needle = format!("({comm})");
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
                continue;
            };
            // stat: "<pid> (<comm>) <state> <ppid> ..."
            let Some(close) = stat.rfind(')') else {
                continue;
            };
            if !stat[..close + 1].ends_with(&needle) {
                continue;
            }
            let mut rest = stat[close + 1..].split_whitespace();
            let _state = rest.next();
            if rest.next() == Some(&my_pid.to_string()) {
                return true;
            }
        }
        false
    }

    /// A spawned opener child is REAPED after it exits — it must not linger as
    /// a zombie until process exit. `true` exits immediately, so within the
    /// deadline the child must disappear from our /proc children entirely.
    /// Fails before the reaper fix: the dropped `Child` is never waited on, so
    /// the zombie persists for the lifetime of the test binary.
    #[test]
    fn spawn_detached_reaps_exited_child() {
        spawn_detached(&["true".to_owned()]).expect("spawn `true`");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !have_child_named("true") {
                return; // reaped — no running or zombie `true` child remains
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!(
            "spawned `true` child was never reaped — still a child of this \
             process (zombie) 10s after spawn_detached returned"
        );
    }
}

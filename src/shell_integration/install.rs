// SPDX-License-Identifier: GPL-3.0-only
//! Spawn-time injection: attaching integration to a shell OdyTTY starts.
//!
//! Windows injects the snippet inline through `-NoExit -Command`. Unix writes
//! wrapper files into OdyTTY's own config directory and points the shell at
//! them. Every failure path leaves the command unchanged, so shell startup
//! never depends on integration plumbing succeeding.

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(any(unix, windows))]
use crate::pty::CommandBuilder;

#[cfg(any(unix, windows))]
use super::detect::ShellKind;
#[cfg(unix)]
use super::scripts::{bash_rcfile, fish_conf, zsh_rcfile};
#[cfg(windows)]
use super::snippets::snippet;

/// Windows spawn-time injection. PowerShell is the only supported family
/// (cmd.exe has no OSC 133 hook surface), so anything else is left unchanged.
/// The generated profile is injected with `-NoExit -Command <snippet>`: the
/// snippet wraps `prompt` and installs the PSReadLine Enter hook, and `-NoExit`
/// keeps the session interactive afterwards. Mirrors the `cfg(unix)` injector's
/// shape -- classify from the program basename, bail on the unsupported case,
/// otherwise attach integration to the command.
#[cfg(windows)]
pub(crate) fn apply_spawn_integration(command: &mut CommandBuilder) {
    let Some(kind) = ShellKind::from_program(command.program()) else {
        return;
    };
    command.arg("-NoExit").arg("-Command").arg(snippet(kind));
}

#[cfg(unix)]
pub(crate) fn apply_spawn_integration(command: &mut CommandBuilder) {
    let Some(kind) = ShellKind::from_program(command.program()) else {
        return;
    };
    let Some(dir) = integration_dir() else {
        return;
    };
    apply_spawn_integration_in_dir(command, kind, &dir);
}

#[cfg(unix)]
pub(super) fn apply_spawn_integration_in_dir(
    command: &mut CommandBuilder,
    kind: ShellKind,
    dir: &Path,
) {
    if let Err(error) = fs::create_dir_all(dir) {
        eprintln!("odytty: shell integration disabled: {error}");
        return;
    }

    match kind {
        ShellKind::Bash => {
            let path = dir.join("odytty.bash");
            if write_if_needed(&path, &bash_rcfile()).is_ok() {
                command.arg("--rcfile").arg(path);
            }
        }
        ShellKind::Zsh => {
            let path = dir.join(".zshrc");
            if write_if_needed(&path, &zsh_rcfile()).is_ok() {
                let original = std::env::var_os("ZDOTDIR")
                    .or_else(|| std::env::var_os("HOME"))
                    .unwrap_or_default();
                command.env("ODYTTY_ORIGINAL_ZDOTDIR", original);
                command.env("ZDOTDIR", dir);
            }
        }
        ShellKind::Fish => {
            let base = dir.join("fish-data");
            let vendor = base.join("fish").join("vendor_conf.d");
            if fs::create_dir_all(&vendor).is_ok()
                && write_if_needed(&vendor.join("odytty.fish"), fish_conf()).is_ok()
            {
                let mut data_dirs = base.into_os_string();
                if let Some(existing) = std::env::var_os("XDG_DATA_DIRS")
                    && !existing.is_empty()
                {
                    data_dirs.push(":");
                    data_dirs.push(existing);
                }
                command.env("XDG_DATA_DIRS", data_dirs);
            }
        }
        // PowerShell integration is Windows-only and injected inline via
        // `-NoExit -Command` (no rcfile/profile is written into the config
        // dir), so the Unix file-based injector never receives this kind. The
        // arm exists only to keep the match exhaustive.
        ShellKind::PowerShell => {}
    }
}

#[cfg(unix)]
fn integration_dir() -> Option<PathBuf> {
    crate::settings::config_file_path()?
        .parent()
        .map(|path| path.join("shell-integration"))
}

#[cfg(unix)]
pub(super) fn write_if_needed(path: &Path, contents: &str) -> std::io::Result<()> {
    if already_matches(path, contents)? {
        return Ok(());
    }
    fs::write(path, contents)
}

/// Whether `path` already holds exactly `contents`, without ever reading more
/// than the wrapper OdyTTY is about to write.
///
/// The comparison is bounded by construction rather than by a separate ceiling:
/// a regular file whose length differs from the wrapper cannot be equal to it,
/// so it is rewritten without being read at all. Only a file of exactly the
/// wrapper's length is read back, and then at most one byte past that length so
/// a file that grew between the size check and the read is seen as different
/// instead of being compared against a truncated view of itself. A replaced or
/// generated file in the integration directory therefore cannot turn this
/// comparison into an unbounded allocation.
///
/// A path that is not a regular file is refused outright: writing a wrapper
/// over a directory, device, or FIFO is never correct, and reading a FIFO here
/// would block shell startup. Every other failure (missing file, unreadable
/// contents) reports "does not match" and falls through to the write, which
/// surfaces its own error, so the pre-existing behavior of the ordinary paths
/// is unchanged.
#[cfg(unix)]
fn already_matches(path: &Path, contents: &str) -> std::io::Result<bool> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(false);
    };
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "shell integration path is not a regular file",
        ));
    }
    if metadata.len() != contents.len() as u64 {
        return Ok(false);
    }

    let Ok(file) = fs::File::open(path) else {
        return Ok(false);
    };
    let mut existing = Vec::with_capacity(contents.len());
    if file
        .take(contents.len() as u64 + 1)
        .read_to_end(&mut existing)
        .is_err()
    {
        return Ok(false);
    }
    Ok(existing == contents.as_bytes())
}

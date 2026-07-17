// SPDX-License-Identifier: GPL-3.0-only
//! Pseudo-terminal session abstraction.
//!
//! The platform-neutral contract lives here; the platform backend that
//! implements [`PtySession`] is selected by `#[cfg]`:
//!
//! - [`ForegroundJob`] and [`CommandBuilder`] are pure (`std`-only) and shared
//!   by every backend.
//! - The [`PtySession`] implementation and the `pub(crate)` [`open_pty_pair`]
//!   helper are backend-specific and re-exported here so call sites use the
//!   neutral `crate::pty::…` path regardless of platform.
//!
//! The Unix backend (`unix.rs`) is the POSIX PTY implementation (rustix
//! `openpt`/`grantpt`/`unlockpt`, termios, `setsid`/`TIOCSCTTY`,
//! process-group signalling). Other platforms supply their own backend.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::PtySession;
#[cfg(windows)]
pub use windows::PtySession;
// `open_pty_pair` is `pub(crate)` and exercised only from a `#[cfg(test)]`
// caller (`app.rs` termios round-trip), so a non-test lib build sees the
// re-export as unused. The Unix backend defined it unconditionally before the
// module split with no such warning; the `allow` preserves that exact
// crate-wide availability without gating it out of release builds.
#[cfg(unix)]
#[allow(unused_imports)]
pub(crate) use unix::open_pty_pair;

/// Whether a foreground job — a process group on the controlling terminal other
/// than the spawned shell itself — is currently running.
///
/// This is a *read-only* classification of the PTY master's foreground process
/// group versus the shell's own group. It never reaps, waits on, or otherwise
/// mutates the child; it only inspects kernel-owned terminal state.
///
/// `Unknown` is the deliberate safe default: callers (e.g. a close-confirmation
/// prompt) treat both `None` and `Unknown` as "safe to close, do not prompt",
/// so a dead PTY, an exited child, or a query error never blocks a close and
/// never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForegroundJob {
    /// The shell itself owns the terminal foreground — nothing would be lost on
    /// close.
    None,
    /// A process group other than the shell owns the terminal foreground — a job
    /// is running in the foreground.
    Running,
    /// The foreground could not be determined (PTY closed, child exited, no
    /// foreground group, or the query errored). Treated as "safe to close".
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CommandBuilder {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    current_dir: Option<PathBuf>,
}

impl CommandBuilder {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            current_dir: None,
        }
    }

    /// The resolved program. Inspected by the shell-integration injector to
    /// classify the shell from its basename: the Unix injector and the Windows
    /// (PowerShell) injector both call it, so it is `any(unix, windows)`.
    #[cfg(any(unix, windows))]
    pub(crate) fn program(&self) -> &OsString {
        &self.program
    }

    pub fn arg(&mut self, arg: impl Into<OsString>) -> &mut Self {
        self.args.push(arg.into());
        self
    }

    pub fn env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    // Inspected by the shell-integration injection tests. `args_for_test` is
    // used on BOTH platforms (Unix asserts the rcfile/env args; Windows asserts
    // the PowerShell `-NoExit -Command <snippet>` injection -- there IS Windows
    // spawn-time injection), so it is `cfg(test)`. `env_for_test` is likewise
    // read on both platforms: the Unix rcfile tests assert the
    // ZDOTDIR/XDG_DATA_DIRS wiring, and the shared buttons-discovery test in
    // this module asserts the ODYTTY_BUTTONS injection everywhere.
    #[cfg(test)]
    pub(crate) fn args_for_test(&self) -> &[OsString] {
        &self.args
    }

    #[cfg(test)]
    pub(crate) fn env_for_test(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    /// Apply the standard OdyTTY terminal environment to a spawned child:
    /// `TERM`/`COLORTERM` (capability advertisement, unchanged from before) plus
    /// `TERM_PROGRAM`/`TERM_PROGRAM_VERSION` for terminal self-identification.
    ///
    /// fastfetch's generic version fallback reads `TERM_PROGRAM_VERSION` and
    /// shows it when the detected terminal process name starts with
    /// `TERM_PROGRAM`, so the literal must be exactly `"odytty"` (lowercase) to
    /// match the binary name. The version comes from `CARGO_PKG_VERSION` at
    /// compile time, staying in lockstep with `Cargo.toml` (same source
    /// `main.rs` uses).
    ///
    /// Centralized so every spawn path advertises an identical environment and
    /// the four variables can never drift between call sites.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn apply_terminal_env(&mut self) -> &mut Self {
        self.env("TERM", "xterm-256color");
        self.env("COLORTERM", "truecolor");
        self.env("TERM_PROGRAM", "odytty");
        self.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        self
    }

    /// Button-protocol feature discovery (docs/buttons.md): when the `buttons`
    /// master gate is on, advertise support to the spawned child by setting
    /// `ODYTTY_BUTTONS=1`; when off, set nothing at all, so a program's
    /// `[ -n "$ODYTTY_BUTTONS" ]` guard tracks the gate exactly. Shared by
    /// both platform spawn paths: the Unix backend passes `env` to the child
    /// through fork/exec, and the Windows backend folds the same vec into the
    /// ConPTY environment block, so the advertisement crosses ConPTY like any
    /// other variable. Env-based discovery deliberately does not cross
    /// ssh/nested sessions (a documented limitation; a query escape can come
    /// later if remote discovery is ever needed).
    #[cfg_attr(not(unix), allow(dead_code))]
    fn apply_buttons_discovery_env(&mut self, buttons_enabled: bool) -> &mut Self {
        if buttons_enabled {
            self.env("ODYTTY_BUTTONS", "1");
        }
        self
    }

    pub fn current_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.current_dir = Some(path.into());
        self
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    fn into_command(self) -> Command {
        let mut command = Command::new(self.program);
        command.args(self.args);
        if let Some(path) = self.current_dir {
            command.current_dir(path);
        }
        for (key, value) in self.env {
            command.env(key, value);
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Buttons feature discovery (docs/buttons.md): gate on injects exactly
    /// `ODYTTY_BUTTONS=1`; gate off injects nothing, so a child's
    /// `[ -n "$ODYTTY_BUTTONS" ]` guard tracks the gate. Runs on every
    /// platform: the same `env` vec feeds Unix fork/exec and the Windows
    /// ConPTY environment block, so this asserts the injection at the shared
    /// site both backends consume.
    #[test]
    fn buttons_discovery_env_tracks_the_master_gate() {
        let mut on = CommandBuilder::new("sh");
        on.apply_buttons_discovery_env(true);
        assert!(
            on.env_for_test()
                .iter()
                .any(|(k, v)| k == "ODYTTY_BUTTONS" && v == "1"),
            "gate on must inject ODYTTY_BUTTONS=1"
        );

        let mut off = CommandBuilder::new("sh");
        off.apply_buttons_discovery_env(false);
        assert!(
            off.env_for_test()
                .iter()
                .all(|(k, _)| k != "ODYTTY_BUTTONS"),
            "gate off must not inject ODYTTY_BUTTONS"
        );
        assert!(
            off.env_for_test().is_empty(),
            "gate off must add no environment at all"
        );
    }
}

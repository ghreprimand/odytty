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
    /// Variables to strip from the child's inherited environment at spawn.
    /// Distinct from an empty `env` set: a removal makes the variable UNSET in
    /// the child (so a `[ -n "$VAR" ]`/`set -q VAR` guard reads absent) rather
    /// than present-but-empty. Both backends honor it — the Unix backend maps
    /// each entry to `Command::env_remove`, the Windows backend drops the
    /// matching entry from the ConPTY environment block.
    env_remove: Vec<OsString>,
    current_dir: Option<PathBuf>,
}

impl CommandBuilder {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
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
        let key = key.into();
        // Later call wins: an explicit set defeats an earlier removal of the
        // same key, so the two lists stay disjoint by construction. Without
        // this, the Unix backend (removals applied last) and the Windows
        // backend (overrides appended last) resolved a set+remove collision
        // oppositely, silently dropping an explicit profile `env` value on Linux.
        self.env_remove.retain(|removed| removed != &key);
        self.env.push((key, value.into()));
        self
    }

    /// Strip `key` from the child's inherited environment at spawn (see the
    /// `env_remove` field). Used to stop stale OdyTTY discovery advertisements
    /// (`ODYTTY_SHELL_INTEGRATION`, `ODYTTY_BUTTONS`, `ODYTTY_KEY_ENHANCE`)
    /// inherited from an outer integrated session from leaking into a nested
    /// odytty's child shell.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    fn env_remove(&mut self, key: impl Into<OsString>) -> &mut Self {
        let key = key.into();
        // Later call wins (see `env`): a removal defeats an earlier set of the
        // same key, keeping the two lists disjoint so both backends resolve a
        // set+remove collision identically.
        self.env.retain(|(existing, _)| existing != &key);
        self.env_remove.push(key);
        self
    }

    /// Standard OdyTTY child environment for an interactive shell spawn.
    #[cfg(any(unix, windows))]
    pub(crate) fn apply_standard_interactive_shell_env(
        &mut self,
        settings: &crate::settings::Settings,
    ) -> &mut Self {
        self.apply_terminal_env();
        self.apply_shell_integration_scrub();
        self.apply_buttons_discovery_env(settings.buttons);
        self.apply_key_enhancement_discovery_env(settings.shell_key_enhancement);
        if settings.shell_integration {
            crate::shell_integration::apply_spawn_integration(self);
        }
        self
    }

    /// Terminal advertisement plus profile env overrides for a direct exec spawn.
    #[cfg(any(unix, windows))]
    pub(crate) fn apply_standard_exec_env(
        &mut self,
        extra_env: &std::collections::BTreeMap<String, String>,
    ) -> &mut Self {
        self.apply_terminal_env();
        self.apply_shell_integration_scrub();
        for (key, value) in extra_env {
            self.env(key.clone(), value.clone());
        }
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

    #[cfg(test)]
    pub(crate) fn env_remove_for_test(&self) -> &[OsString] {
        &self.env_remove
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
        } else {
            // Off must actively SCRUB the advertisement, not merely "set
            // nothing": an outer odytty with buttons on exports ODYTTY_BUTTONS=1
            // into this process's environment, which would otherwise pass
            // straight through to the child shell of a nested odytty whose gate
            // is off. Removing it makes a child's `[ -n "$ODYTTY_BUTTONS" ]`
            // guard track this gate exactly regardless of what was inherited.
            self.env_remove("ODYTTY_BUTTONS");
        }
        self
    }

    /// Prompt-scoped key-enhancement discovery (D-b): when the
    /// `shell_key_enhancement` knob is on, advertise it to an integrated
    /// bash/zsh shell by setting `ODYTTY_KEY_ENHANCE=1`; the snippet guards its
    /// prompt-scoped Kitty-keyboard lifecycle on this variable, so without it
    /// Bash's add/remove and zsh's push/pop never fire. When off, set nothing at
    /// all, so the snippet's `[ -n "$ODYTTY_KEY_ENHANCE" ]` guard tracks the
    /// knob exactly.
    /// Independent of the buttons advertisement; only the bash/zsh snippets read
    /// it (fish self-manages the protocol, PowerShell uses the Console API), so
    /// it is inert for every other child. Shared by both platform spawn paths
    /// like the buttons advertisement; harmless on Windows where no snippet
    /// consumes it.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn apply_key_enhancement_discovery_env(&mut self, key_enhancement_enabled: bool) -> &mut Self {
        if key_enhancement_enabled {
            self.env("ODYTTY_KEY_ENHANCE", "1");
        } else {
            // Off scrubs the advertisement for the same nested-launch reason as
            // the buttons gate: a leaked ODYTTY_KEY_ENHANCE=1 from an outer
            // integrated session must not survive into a nested odytty's child
            // shell whose knob is off.
            self.env_remove("ODYTTY_KEY_ENHANCE");
        }
        self
    }

    /// Nested-launch hygiene: an odytty launched from an already-integrated
    /// odytty session inherits `ODYTTY_SHELL_INTEGRATION=1` in its own process
    /// environment and would pass it straight through to the shell it spawns,
    /// whose snippet guard (`[ -z "${ODYTTY_SHELL_INTEGRATION-}" ]`) then skips
    /// the ENTIRE integration body — no OSC 133 marks, no OSC 7, no button
    /// emitters, no key push. Scrubbing the inherited value at spawn is safe:
    /// the snippet re-exports the variable itself when it runs, so the guard's
    /// real job (preventing double-sourcing WITHIN one shell) is preserved
    /// because that export happens inside the shell, not across odytty
    /// processes. Applied on every default-shell spawn, independent of whether
    /// integration is being injected on this spawn. Shared by both platform
    /// backends; on Windows the scrub folds into the ConPTY environment block.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn apply_shell_integration_scrub(&mut self) -> &mut Self {
        self.env_remove("ODYTTY_SHELL_INTEGRATION");
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
        for key in self.env_remove {
            command.env_remove(key);
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F3: a key present in both `env` and `env_remove` must resolve the same
    /// way on both PTY backends. The lists are kept disjoint at push time (later
    /// call wins), so a set after a removal leaves the key only in `env`, and a
    /// removal after a set leaves it only in `env_remove`. Both backends consume
    /// these two lists, so making them disjoint here makes the backends agree.
    #[test]
    fn env_and_env_remove_stay_disjoint_with_later_call_winning() {
        // set then remove: removal wins, key gone from env.
        let mut a = CommandBuilder::new("sh");
        a.env("ODYTTY_BUTTONS", "1");
        a.env_remove("ODYTTY_BUTTONS");
        assert!(
            !a.env_for_test().iter().any(|(k, _)| k == "ODYTTY_BUTTONS"),
            "a later removal must drop the earlier set"
        );
        assert!(
            a.env_remove_for_test()
                .iter()
                .any(|k| k == "ODYTTY_BUTTONS"),
            "the key is removed in the child"
        );

        // remove then set: set wins, key gone from env_remove.
        let mut b = CommandBuilder::new("sh");
        b.env_remove("ODYTTY_BUTTONS");
        b.env("ODYTTY_BUTTONS", "1");
        assert!(
            b.env_for_test()
                .iter()
                .any(|(k, v)| k == "ODYTTY_BUTTONS" && v == "1"),
            "a later explicit set must defeat the earlier removal"
        );
        assert!(
            !b.env_remove_for_test()
                .iter()
                .any(|k| k == "ODYTTY_BUTTONS"),
            "the removal is dropped so the set survives on both backends"
        );
    }

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
            "gate off must set no environment value at all"
        );
        // Off must ACTIVELY scrub the advertisement so a value inherited from an
        // outer integrated odytty never survives into a nested odytty's child.
        assert!(
            off.env_remove_for_test()
                .iter()
                .any(|k| k == "ODYTTY_BUTTONS"),
            "gate off must remove ODYTTY_BUTTONS from the inherited environment"
        );
    }

    /// Prompt-scoped key-enhancement discovery (D-b): the knob on injects
    /// exactly `ODYTTY_KEY_ENHANCE=1`; off injects nothing, so the bash/zsh
    /// snippet's `[ -n "$ODYTTY_KEY_ENHANCE" ]` guard tracks the knob. Shared
    /// injection site for both backends, same as the buttons advertisement.
    #[test]
    fn key_enhancement_discovery_env_tracks_the_knob() {
        let mut on = CommandBuilder::new("sh");
        on.apply_key_enhancement_discovery_env(true);
        assert!(
            on.env_for_test()
                .iter()
                .any(|(k, v)| k == "ODYTTY_KEY_ENHANCE" && v == "1"),
            "knob on must inject ODYTTY_KEY_ENHANCE=1"
        );

        let mut off = CommandBuilder::new("sh");
        off.apply_key_enhancement_discovery_env(false);
        assert!(
            off.env_for_test().is_empty(),
            "knob off must set no environment value at all"
        );
        assert!(
            off.env_remove_for_test()
                .iter()
                .any(|k| k == "ODYTTY_KEY_ENHANCE"),
            "knob off must remove ODYTTY_KEY_ENHANCE from the inherited environment"
        );
    }

    /// Nested-launch hygiene: the scrub always removes ODYTTY_SHELL_INTEGRATION
    /// from the child environment so a value inherited from an outer integrated
    /// odytty cannot make the child shell's snippet guard skip the whole
    /// integration body. The snippet re-exports the variable itself when it
    /// runs, so scrubbing here never disables integration on the fresh shell.
    #[test]
    fn shell_integration_scrub_strips_inherited_marker() {
        let mut command = CommandBuilder::new("sh");
        command.apply_shell_integration_scrub();
        assert!(
            command
                .env_remove_for_test()
                .iter()
                .any(|k| k == "ODYTTY_SHELL_INTEGRATION"),
            "scrub must remove ODYTTY_SHELL_INTEGRATION from the inherited environment"
        );
        assert!(
            command.env_for_test().is_empty(),
            "scrub must not set any environment value"
        );
    }

    /// Product-path assertion (Unix apply site): `into_command` must carry each
    /// scrub through to the built `std::process::Command` as an env REMOVAL
    /// (surfaced by `get_envs` as `(key, None)`), while ordinary sets survive as
    /// `(key, Some(value))`. This is what makes the fresh shell see
    /// ODYTTY_SHELL_INTEGRATION unset even when the odytty process inherited it.
    #[cfg(unix)]
    #[test]
    fn into_command_applies_scrub_as_env_removal() {
        let mut builder = CommandBuilder::new("sh");
        builder.apply_terminal_env();
        builder.apply_shell_integration_scrub();
        builder.apply_buttons_discovery_env(false);

        let command = builder.into_command();
        let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(|v| v.to_os_string())))
            .collect();

        let removed = |name: &str| envs.iter().any(|(k, v)| k == name && v.is_none());
        let set_to = |name: &str, value: &str| {
            envs.iter()
                .any(|(k, v)| k == name && v.as_deref() == Some(std::ffi::OsStr::new(value)))
        };

        assert!(
            removed("ODYTTY_SHELL_INTEGRATION"),
            "scrub must reach the built command as an env removal"
        );
        assert!(
            removed("ODYTTY_BUTTONS"),
            "off buttons gate must reach the built command as an env removal"
        );
        assert!(
            set_to("TERM", "xterm-256color"),
            "ordinary env sets must survive alongside removals"
        );
    }
}

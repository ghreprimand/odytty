// SPDX-License-Identifier: GPL-3.0-only
//! OSC 133 shell-integration snippets and spawn-time injection helpers.
//!
//! The snippets are pure text so the CLI can print them on every platform. The
//! Unix spawn helper writes wrapper files into OdyTTY's config directory and
//! points the detected shell at them; if anything fails, it leaves the command
//! unchanged so shell startup never depends on integration plumbing.
//!
//! The responsibilities are separated so a change to one cannot quietly move
//! another, and the dependency direction runs strictly downwards:
//!
//! - [`detect`] -- which shell this is, from a user-supplied name or a spawned
//!   program basename.
//! - [`snippets`] -- the per-shell snippet text and its selection by kind.
//! - [`scripts`] -- the wrapper bodies generated around those snippets.
//! - [`family`] -- the readout vocabulary and per-family integration posture.
//! - [`install`] -- spawn-time injection and the bounded file writes it needs.
//!
//! Parsing of the OSC 133 marks these snippets emit is not here: that is the
//! terminal's own responsibility and lives in `crate::core::prompt_marks`.

mod detect;
mod family;
mod install;
mod scripts;
mod snippets;

pub use detect::ShellKind;
pub use family::{IntegrationPosture, ShellFamily};
#[cfg(any(unix, windows))]
pub(crate) use install::apply_spawn_integration;
pub use scripts::bash_integration_rc;
pub use snippets::{snippet, snippet_for_shell};

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: GPL-3.0-only
//! Generating the wrapper scripts OdyTTY points a spawned shell at.
//!
//! Each wrapper sources the user's own startup file first and appends the
//! integration snippet, so enabling integration never replaces a user's
//! configuration. The bash body is shared with the remote SSH bootstrap and is
//! therefore `cfg`-agnostic: the local injector and the remote argv builder
//! must produce byte-identical payloads.

use super::snippets::BASH_SNIPPET;
#[cfg(unix)]
use super::snippets::{FISH_SNIPPET, ZSH_SNIPPET};

/// The bash shell-integration rcfile body: source the user's `~/.bashrc`, then
/// append the OSC 133 snippet. This is the single source of truth for the rc
/// content so the local file-based injector and the remote SSH bootstrap
/// (`crate::ssh_connect`) can never drift. It is `cfg`-agnostic on purpose: the
/// remote-injection argv builder is cross-platform and must produce the exact
/// same integration payload whether the client runs on Unix or Windows.
pub fn bash_integration_rc() -> String {
    format!(
        r#"if [ -r "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi

{snippet}
"#,
        snippet = BASH_SNIPPET
    )
}

#[cfg(unix)]
pub(super) fn bash_rcfile() -> String {
    bash_integration_rc()
}

#[cfg(unix)]
pub(super) fn zsh_rcfile() -> String {
    format!(
        r#"if [ -n "${{ODYTTY_ORIGINAL_ZDOTDIR-}}" ] && [ -r "$ODYTTY_ORIGINAL_ZDOTDIR/.zshrc" ]; then
  . "$ODYTTY_ORIGINAL_ZDOTDIR/.zshrc"
elif [ -r "$HOME/.zshrc" ]; then
  . "$HOME/.zshrc"
fi

{snippet}
"#,
        snippet = ZSH_SNIPPET
    )
}

#[cfg(unix)]
pub(super) fn fish_conf() -> &'static str {
    FISH_SNIPPET
}

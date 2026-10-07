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

// Restore the exact authored state, including an explicitly empty ZDOTDIR.
#[cfg(unix)]
const ZSH_RESTORE_DIR: &str = r#"if [ -n "${ODYTTY_ORIGINAL_ZDOTDIR_SET-}" ]; then
  export ZDOTDIR="$ODYTTY_ORIGINAL_ZDOTDIR"
else
  unset ZDOTDIR
fi
"#;

#[cfg(unix)]
pub(super) fn zsh_startup_file(name: &str, redirect: bool) -> String {
    let mut body = format!(
        "{ZSH_RESTORE_DIR}if [ -r \"${{ZDOTDIR-$HOME}}/{name}\" ]; then\n  . \"${{ZDOTDIR-$HOME}}/{name}\"\nfi\n"
    );
    if redirect {
        // Preserve changes made by .zshenv or .zprofile before routing the
        // interactive rc to the integration wrapper. Noninteractive shells
        // retain the native directory and do not need an interactive hook.
        body.push_str(
            r#"if [ "${ZDOTDIR+x}" = x ]; then
  export ODYTTY_ORIGINAL_ZDOTDIR_SET=1
  export ODYTTY_ORIGINAL_ZDOTDIR="$ZDOTDIR"
else
  export ODYTTY_ORIGINAL_ZDOTDIR_SET=
  export ODYTTY_ORIGINAL_ZDOTDIR="$HOME"
fi
case $- in
  *i*) export ZDOTDIR="$ODYTTY_ZSH_WRAPPER_DIR" ;;
esac
"#,
        );
    }
    body
}

#[cfg(unix)]
pub(super) fn zsh_rcfile() -> String {
    format!("{}\n{ZSH_SNIPPET}\n", zsh_startup_file(".zshrc", false))
}

#[cfg(unix)]
pub(super) fn fish_conf() -> &'static str {
    FISH_SNIPPET
}

// SPDX-License-Identifier: GPL-3.0-only
//! OSC 133 shell-integration snippets and spawn-time injection helpers.
//!
//! The snippets are pure text so the CLI can print them on every platform. The
//! Unix spawn helper writes wrapper files into OdyTTY's config directory and
//! points the detected shell at them; if anything fails, it leaves the command
//! unchanged so shell startup never depends on integration plumbing.

#[cfg(unix)]
use std::ffi::OsStr;

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use crate::pty::CommandBuilder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
}

impl ShellKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// Classify a shell from a spawned program's basename. Only the Unix
    /// spawn-time injector calls this, so it is `cfg(unix)`; the cross-platform
    /// CLI snippet path classifies from the user-supplied name via [`parse`].
    #[cfg(unix)]
    fn from_program(program: &OsStr) -> Option<Self> {
        let name = std::path::Path::new(program)
            .file_name()
            .and_then(OsStr::to_str)?
            .trim_start_matches('-')
            .to_ascii_lowercase();
        Self::parse(&name)
    }
}

pub fn snippet_for_shell(shell: &str) -> Result<&'static str, String> {
    let kind = ShellKind::parse(shell)
        .ok_or_else(|| format!("unsupported shell {shell:?}; expected one of: bash, zsh, fish"))?;
    Ok(snippet(kind))
}

pub fn snippet(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Bash => BASH_SNIPPET,
        ShellKind::Zsh => ZSH_SNIPPET,
        ShellKind::Fish => FISH_SNIPPET,
    }
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
fn apply_spawn_integration_in_dir(command: &mut CommandBuilder, kind: ShellKind, dir: &Path) {
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
    }
}

#[cfg(unix)]
fn integration_dir() -> Option<PathBuf> {
    crate::settings::config_file_path()?
        .parent()
        .map(|path| path.join("shell-integration"))
}

#[cfg(unix)]
fn write_if_needed(path: &Path, contents: &str) -> std::io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    fs::write(path, contents)
}

#[cfg(unix)]
fn bash_rcfile() -> String {
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
fn zsh_rcfile() -> String {
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
fn fish_conf() -> &'static str {
    FISH_SNIPPET
}

const BASH_SNIPPET: &str = r#"if [ -z "${ODYTTY_SHELL_INTEGRATION-}" ]; then
  export ODYTTY_SHELL_INTEGRATION=1

  __odytty_prompt_command() {
    local __odytty_status=$?
    if [ -n "${__ODYTTY_COMMAND_STARTED-}" ]; then
      printf '\e]133;D;%s\a' "$__odytty_status"
      unset __ODYTTY_COMMAND_STARTED
    fi
    printf '\e]133;A\a'
  }

  __odytty_debug_trap() {
    case "$BASH_COMMAND" in
      __odytty_prompt_command*|__odytty_debug_trap*|__odytty_append_prompt_command*) return ;;
    esac
    printf '\e]133;C\a'
    __ODYTTY_COMMAND_STARTED=1
  }

  __odytty_append_prompt_command() {
    case ";${PROMPT_COMMAND-};" in
      *";$1;"*) ;;
      ";"|";;" ) PROMPT_COMMAND="$1" ;;
      *) PROMPT_COMMAND="${PROMPT_COMMAND};$1" ;;
    esac
  }

  case "$PS1" in
    *'133;B'*) ;;
    *) PS1="${PS1}"'\[\e]133;B\a\]' ;;
  esac
  __odytty_append_prompt_command __odytty_prompt_command
  trap '__odytty_debug_trap' DEBUG
fi
"#;

const ZSH_SNIPPET: &str = r#"if [ -z "${ODYTTY_SHELL_INTEGRATION:-}" ]; then
  export ODYTTY_SHELL_INTEGRATION=1
  autoload -Uz add-zsh-hook

  __odytty_precmd() {
    local __odytty_status=$?
    if [ -n "${__ODYTTY_COMMAND_STARTED:-}" ]; then
      printf '\e]133;D;%s\a' "$__odytty_status"
      unset __ODYTTY_COMMAND_STARTED
    fi
    printf '\e]133;A\a'
  }

  __odytty_preexec() {
    printf '\e]133;C\a'
    __ODYTTY_COMMAND_STARTED=1
  }

  case "$PS1" in
    *'133;B'*) ;;
    *) PS1="${PS1}%{\e]133;B\a%}" ;;
  esac
  add-zsh-hook precmd __odytty_precmd
  add-zsh-hook preexec __odytty_preexec
fi
"#;

const FISH_SNIPPET: &str = r#"if not set -q ODYTTY_SHELL_INTEGRATION
    set -gx ODYTTY_SHELL_INTEGRATION 1

    if not functions -q __odytty_original_fish_prompt
        functions -c fish_prompt __odytty_original_fish_prompt
    end

    function fish_prompt
        printf '\e]133;A\a'
        __odytty_original_fish_prompt
        printf '\e]133;B\a'
    end

    function __odytty_preexec --on-event fish_preexec
        printf '\e]133;C\a'
    end

    function __odytty_postexec --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
end
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_emit_osc_133_marks() {
        for shell in ["bash", "zsh", "fish"] {
            let snippet = snippet_for_shell(shell).expect("snippet");
            assert!(snippet.contains("\\e]133;A"), "{shell}: missing A");
            assert!(snippet.contains("133;B"), "{shell}: missing B");
            assert!(snippet.contains("133;C"), "{shell}: missing C");
            assert!(snippet.contains("133;D"), "{shell}: missing D");
            assert!(!snippet.trim().is_empty());
        }
    }

    #[test]
    fn unknown_shell_errors_cleanly() {
        let err = snippet_for_shell("cmd").unwrap_err();
        assert!(err.contains("unsupported shell"));
        assert!(err.contains("bash, zsh, fish"));
    }

    #[cfg(unix)]
    #[test]
    fn shell_kind_detects_program_basename() {
        assert_eq!(
            ShellKind::from_program(OsStr::new("/bin/bash")),
            Some(ShellKind::Bash)
        );
        assert_eq!(
            ShellKind::from_program(OsStr::new("-zsh")),
            Some(ShellKind::Zsh)
        );
        assert_eq!(
            ShellKind::from_program(OsStr::new("/usr/bin/fish")),
            Some(ShellKind::Fish)
        );
        assert_eq!(ShellKind::from_program(OsStr::new("/bin/sh")), None);
    }

    #[cfg(unix)]
    fn temp_integration_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "odytty-shell-integration-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[cfg(unix)]
    #[test]
    fn bash_injection_writes_rcfile_and_adds_rcfile_arg() {
        let dir = temp_integration_dir("bash");
        let mut command = CommandBuilder::new("/bin/bash");
        apply_spawn_integration_in_dir(&mut command, ShellKind::Bash, &dir);

        let args = command.args_for_test();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], std::ffi::OsString::from("--rcfile"));
        assert_eq!(args[1], dir.join("odytty.bash").into_os_string());
        let rcfile = fs::read_to_string(dir.join("odytty.bash")).expect("rcfile");
        assert!(rcfile.contains(". \"$HOME/.bashrc\""));
        assert!(rcfile.contains("PROMPT_COMMAND=\"${PROMPT_COMMAND};$1\""));
        assert!(rcfile.contains("\\e]133;A"));
        assert!(rcfile.contains("133;B"));
        assert!(rcfile.contains("133;C"));
        assert!(rcfile.contains("133;D"));
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn zsh_injection_sets_zdotdir_and_sources_user_config_first() {
        let dir = temp_integration_dir("zsh");
        let mut command = CommandBuilder::new("/bin/zsh");
        apply_spawn_integration_in_dir(&mut command, ShellKind::Zsh, &dir);

        assert!(
            command
                .env_for_test()
                .iter()
                .any(|(key, value)| key == "ZDOTDIR" && value == dir.as_os_str())
        );
        let rcfile = fs::read_to_string(dir.join(".zshrc")).expect("zshrc");
        assert!(rcfile.contains("ODYTTY_ORIGINAL_ZDOTDIR"));
        assert!(rcfile.contains("add-zsh-hook precmd __odytty_precmd"));
        assert!(rcfile.contains("add-zsh-hook preexec __odytty_preexec"));
        assert!(rcfile.contains("133;B"));
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn fish_injection_prepends_vendor_conf_data_dir() {
        let dir = temp_integration_dir("fish");
        let mut command = CommandBuilder::new("/usr/bin/fish");
        apply_spawn_integration_in_dir(&mut command, ShellKind::Fish, &dir);

        let data_dir = dir.join("fish-data");
        assert!(command.env_for_test().iter().any(|(key, value)| {
            key == "XDG_DATA_DIRS"
                && value
                    .to_string_lossy()
                    .starts_with(&data_dir.to_string_lossy().to_string())
        }));
        let conf =
            fs::read_to_string(data_dir.join("fish/vendor_conf.d/odytty.fish")).expect("fish conf");
        assert!(conf.contains("functions -c fish_prompt __odytty_original_fish_prompt"));
        assert!(conf.contains("--on-event fish_preexec"));
        assert!(conf.contains("--on-event fish_postexec"));
        assert!(conf.contains("133;D"));
        let _ = fs::remove_dir_all(dir);
    }
}

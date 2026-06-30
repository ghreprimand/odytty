// SPDX-License-Identifier: GPL-3.0-only
//! OSC 133 shell-integration snippets and spawn-time injection helpers.
//!
//! The snippets are pure text so the CLI can print them on every platform. The
//! Unix spawn helper writes wrapper files into OdyTTY's config directory and
//! points the detected shell at them; if anything fails, it leaves the command
//! unchanged so shell startup never depends on integration plumbing.

#[cfg(any(unix, windows))]
use std::ffi::OsStr;

#[cfg(unix)]
use std::fs;
#[cfg(any(unix, windows))]
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(any(unix, windows))]
use crate::pty::CommandBuilder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl ShellKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "powershell" | "pwsh" => Some(Self::PowerShell),
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

    /// Classify a Windows shell from a spawned program's basename. Only the
    /// Windows spawn-time injector calls this, so it is `cfg(windows)`.
    /// PowerShell (`pwsh.exe` / `powershell.exe`) is the only family with an
    /// OSC 133 hook surface; `cmd.exe` is intentionally unsupported.
    #[cfg(windows)]
    fn from_program(program: &OsStr) -> Option<Self> {
        let name = Path::new(program)
            .file_name()
            .and_then(OsStr::to_str)?
            .to_ascii_lowercase();
        match name.as_str() {
            "pwsh.exe" | "pwsh" | "powershell.exe" | "powershell" => Some(Self::PowerShell),
            _ => None,
        }
    }
}

pub fn snippet_for_shell(shell: &str) -> Result<&'static str, String> {
    let kind = ShellKind::parse(shell).ok_or_else(|| {
        format!("unsupported shell {shell:?}; expected one of: bash, zsh, fish, powershell")
    })?;
    Ok(snippet(kind))
}

pub fn snippet(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Bash => BASH_SNIPPET,
        ShellKind::Zsh => ZSH_SNIPPET,
        ShellKind::Fish => FISH_SNIPPET,
        ShellKind::PowerShell => POWERSHELL_SNIPPET,
    }
}

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

// The prompt-start (`133;A`) carries `click_events=1` unconditionally — there is
// deliberately no separate setting gating its emission. Advertising click-events
// only tells the terminal the shell CAN reposition on a click; the actual
// click-to-position action is gated on the consumer side by the `sh_click`
// setting (default off), so emitting the attribute here changes no default
// behavior. A snippet only ever ships when shell integration is enabled (also
// off by default), and threading a settings value through these static const
// snippets would be strictly worse for no behavioral gain. Re-asserting it on
// every prompt (`A`, not just once) keeps it correct across resets.
const BASH_SNIPPET: &str = r#"if [ -z "${ODYTTY_SHELL_INTEGRATION-}" ]; then
  export ODYTTY_SHELL_INTEGRATION=1

  __odytty_prompt_command() {
    local __odytty_status=$?
    if [ -n "${__ODYTTY_COMMAND_STARTED-}" ]; then
      printf '\e]133;D;%s\a' "$__odytty_status"
      unset __ODYTTY_COMMAND_STARTED
    fi
    printf '\e]133;A;click_events=1\a'
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
    printf '\e]133;A;click_events=1\a'
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
        printf '\e]133;A;click_events=1\a'
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

// PowerShell shell-integration profile, injected on Windows with
// `-NoExit -Command`. Windows PowerShell 5.1 lacks the backtick-e escape, so the
// ESC/BEL bytes are built from `[char]27`/`[char]7`. The set-once
// `ODYTTY_SHELL_INTEGRATION` guard mirrors the unix snippets, the wrapped
// `prompt` emits `133;D` (previous command's `$LASTEXITCODE`) then
// `133;A;click_events=1` then the user's prompt then `133;B`, and the PSReadLine
// Enter handler emits `133;C` just before the command runs. `click_events=1`
// matches the unix snippets; the click-to-position action stays consumer-gated
// by `sh_click` (default off). cmd.exe has no equivalent hook surface and is
// deliberately unsupported.
const POWERSHELL_SNIPPET: &str = r##"if (-not $env:ODYTTY_SHELL_INTEGRATION) {
    $env:ODYTTY_SHELL_INTEGRATION = "1"

    if (Test-Path Function:\prompt) {
        $global:__odytty_original_prompt = $function:prompt
    } else {
        $global:__odytty_original_prompt = { "PS $($executionContext.SessionState.Path.CurrentLocation)> " }
    }

    function global:prompt {
        $__odytty_exit = $LASTEXITCODE
        if ($null -eq $__odytty_exit) { $__odytty_exit = 0 }
        $esc = [char]27
        $bel = [char]7
        $out = "$esc]133;D;$__odytty_exit$bel"
        $out += "$esc]133;A;click_events=1$bel"
        $out += & $global:__odytty_original_prompt
        $out += "$esc]133;B$bel"
        $out
    }

    if (Get-Module -ListAvailable -Name PSReadLine) {
        Import-Module PSReadLine -ErrorAction SilentlyContinue
        Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
            [Console]::Write("$([char]27)]133;C$([char]7)")
            [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
        }
    }
}
"##;

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
        // cmd.exe has no OSC 133 hook surface, so it stays unsupported -- its
        // name must not classify, and the error must list only what we ship.
        let err = snippet_for_shell("cmd").unwrap_err();
        assert!(err.contains("unsupported shell"));
        assert!(err.contains("bash, zsh, fish, powershell"));
        assert!(ShellKind::parse("cmd").is_none());
    }

    #[test]
    fn powershell_snippet_emits_all_osc_133_marks() {
        // The PowerShell snippet is generated cross-platform (plain const), so
        // this generator contract is asserted on Linux even though the spawn
        // wiring that injects it only exists on Windows. PowerShell cannot use
        // the ESC shorthand the unix snippets do, so it builds the escape from
        // [char]27; assert on the OSC bodies, not the literal ESC byte.
        let snippet = snippet(ShellKind::PowerShell);

        // Set-once guard so a nested shell / re-source does not double-wrap.
        assert!(
            snippet.contains("ODYTTY_SHELL_INTEGRATION"),
            "missing the set-once integration guard"
        );
        // Prompt-start (A) advertises click-to-position, matching the unix
        // snippets that landed click_events=1.
        assert!(
            snippet.contains("133;A;click_events=1"),
            "missing prompt-start A with click_events=1"
        );
        // Command-start (B) at end of prompt.
        assert!(snippet.contains("133;B"), "missing command-start B");
        // Command-executed (C) on submit.
        assert!(snippet.contains("133;C"), "missing command-executed C");
        // Command-finished (D) carries the previous command's exit status.
        assert!(snippet.contains("133;D"), "missing command-finished D");
        assert!(
            snippet.contains("$LASTEXITCODE"),
            "D marker must report the real exit code"
        );

        // Also reachable through the cross-platform CLI classifier.
        assert_eq!(ShellKind::parse("powershell"), Some(ShellKind::PowerShell));
        assert_eq!(ShellKind::parse("pwsh"), Some(ShellKind::PowerShell));
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

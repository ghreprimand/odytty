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
fn bash_rcfile() -> String {
    bash_integration_rc()
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
// PROMPT_COMMAND coexistence (NF1/NF1-B): a user's pre-existing PROMPT_COMMAND
// helper (git-prompt, powerline, ...) is loaded from `.bashrc` BEFORE this
// snippet, so OdyTTY must not assume it owns PROMPT_COMMAND. Two hazards, both
// fixed here:
//   * exit-status masking (NF1-B) — the user helper runs first and clobbers
//     `$?` before the reporter can read it, so `133;D` would report the
//     helper's status (0), not the real command's. Fixed by PREPENDING
//     `__odytty_status_capture`, which snapshots `$?` at the very start of the
//     PROMPT_COMMAND chain into `__ODYTTY_LAST_STATUS`; the appended reporter
//     reads the snapshot.
//   * phantom OutputStart (NF1) — the DEBUG trap fires before the user helper
//     (a top-level PROMPT_COMMAND command) and would emit a spurious `133;C`.
//     Fixed with a state flag (`__ODYTTY_PROMPT_EXECUTING`, the kitty/ghostty
//     pattern): the capturer arms it, the reporter clears it, and the trap
//     suppresses `133;C` while armed. That excludes ALL PROMPT_COMMAND-internal
//     commands by state — robust against arbitrary user helper names — while
//     the capturer itself, which runs before the flag is armed, stays covered
//     by the internal-name `case` filter.
const BASH_SNIPPET: &str = r#"if [ -z "${ODYTTY_SHELL_INTEGRATION-}" ]; then
  export ODYTTY_SHELL_INTEGRATION=1

  __odytty_status_capture() {
    __ODYTTY_LAST_STATUS=$?
    __ODYTTY_PROMPT_EXECUTING=1
  }

  __odytty_prompt_command() {
    local __odytty_status=${__ODYTTY_LAST_STATUS:-$?}
    if [ -n "${__ODYTTY_COMMAND_STARTED-}" ]; then
      printf '\e]133;D;%s\a' "$__odytty_status"
      unset __ODYTTY_COMMAND_STARTED
    fi
    printf '\e]7;file://%s\a' "${PWD//\%/%25}"
    printf '\e]133;A;click_events=1\a'
    unset __ODYTTY_PROMPT_EXECUTING
  }

  __odytty_debug_trap() {
    if [ -n "${__ODYTTY_PROMPT_EXECUTING-}" ]; then
      return
    fi
    case "$BASH_COMMAND" in
      __odytty_status_capture*|__odytty_prompt_command*|__odytty_debug_trap*|__odytty_append_prompt_command*|__odytty_prepend_prompt_command*) return ;;
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

  __odytty_prepend_prompt_command() {
    case ";${PROMPT_COMMAND-};" in
      *";$1;"*) ;;
      ";"|";;" ) PROMPT_COMMAND="$1" ;;
      *) PROMPT_COMMAND="$1;${PROMPT_COMMAND}" ;;
    esac
  }

  case "$PS1" in
    *'133;B'*) ;;
    *) PS1="${PS1}"'\[\e]133;B\a\]' ;;
  esac
  __odytty_prepend_prompt_command __odytty_status_capture
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
    printf '\e]7;file://%s\a' "${PWD//\%/%25}"
    printf '\e]133;A;click_events=1\a'
  }

  __odytty_preexec() {
    printf '\e]133;C\a'
    __ODYTTY_COMMAND_STARTED=1
  }

  # Edit-region report (B-DESIGN §3.2): publish the authoritative ZLE buffer
  # length + cursor (rune counts; OdyTTY reconciles runes to display cells) on
  # every redraw via the private OSC `133;P;odytty-edit`. Other terminals
  # ignore the unknown 133 subcommand. Hot path per keystroke: parameter
  # expansions and a builtin printf only -- no subshells, no forks. When the
  # buffer contains hard newlines (PS2 continuations, quoted newlines) their
  # rune offsets ride along as `;nl=` so the terminal never mistakes a
  # multi-line buffer for a single-line one.
  __odytty_edit_report() {
    if [[ $BUFFER == *$'\n'* ]]; then
      local -a __odytty_parts=("${(@ps:\n:)BUFFER}")
      local __odytty_nl="" __odytty_acc=0 __odytty_i
      for (( __odytty_i=1; __odytty_i < ${#__odytty_parts[@]}; __odytty_i++ )); do
        (( __odytty_acc += ${#__odytty_parts[__odytty_i]} ))
        __odytty_nl+="${__odytty_nl:+,}${__odytty_acc}"
        (( __odytty_acc += 1 ))
      done
      printf '\e]133;P;odytty-edit;len=%d;cur=%d;nl=%s\a' ${#BUFFER} ${CURSOR} "$__odytty_nl"
    else
      printf '\e]133;P;odytty-edit;len=%d;cur=%d\a' ${#BUFFER} ${CURSOR}
    fi
  }
  # Chain rather than clobber a user's existing pre-redraw widget.
  if (( ${+widgets[zle-line-pre-redraw]} )); then
    zle -A zle-line-pre-redraw __odytty_wrapped_line_pre_redraw
    __odytty_line_pre_redraw() {
      __odytty_edit_report
      zle __odytty_wrapped_line_pre_redraw -- "$@"
    }
  else
    __odytty_line_pre_redraw() { __odytty_edit_report }
  fi
  zle -N zle-line-pre-redraw __odytty_line_pre_redraw

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

    # Edit-region report (B-DESIGN §3.3): publish the buffer length + cursor
    # from the `commandline` builtin (which excludes the autosuggestion and
    # fish_right_prompt by construction) via the private OSC
    # `133;P;odytty-edit`. fish has no per-keystroke redraw hook, so this
    # fires on prompt events only; OdyTTY validates every report against its
    # grid and treats a stale one as no-signal, so a mid-edit report can never
    # back a wrong delete. Builtins only -- no forks.
    function __odytty_edit_report
        set -l __odytty_buf (commandline)
        printf '\e]133;P;odytty-edit;len=%d;cur=%d\a' (string length -- "$__odytty_buf") (commandline --cursor)
    end

    function fish_prompt
        printf '\e]7;file://%s\a' (string replace -a '%' '%25' -- "$PWD")
        printf '\e]133;A;click_events=1\a'
        __odytty_original_fish_prompt
        printf '\e]133;B\a'
        __odytty_edit_report
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
// Enter handler emits `133;C` just before the command runs. The OSC 7 path is
// percent-encoded (`%` -> `%25`) so a directory whose name contains `%` cannot
// produce a malformed escape that the parser would reject, freezing cwd
// tracking. `133;D` is gated on a per-command flag the Enter handler sets, so
// no phantom `CommandEnd{exit:0}` is stamped before the first command (mirrors
// the unix `__ODYTTY_COMMAND_STARTED` guard). The Enter handler emits `133;C`
// only when the buffer parses as complete; on an incomplete multiline
// continuation it inserts a newline (`AddLine`) without a spurious OutputStart.
// `click_events=1` matches the unix snippets; the click-to-position action
// stays consumer-gated by `sh_click` (default off). cmd.exe has no equivalent
// hook surface and is deliberately unsupported.
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
        $p = $PWD.Path -replace '%','%25' -replace '\\','/'
        $out = ""
        if ($global:__odytty_command_started) {
            $out += "$esc]133;D;$__odytty_exit$bel"
            $global:__odytty_command_started = $false
        }
        $out += "$esc]7;file:///$p$bel"
        $out += "$esc]133;A;click_events=1$bel"
        $out += & $global:__odytty_original_prompt
        $out += "$esc]133;B$bel"
        $out
    }

    if (Get-Module -ListAvailable -Name PSReadLine) {
        Import-Module PSReadLine -ErrorAction SilentlyContinue
        Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
            $line = $null
            $cursor = $null
            [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
            $errs = $null
            [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$null, [ref]$errs) | Out-Null
            $incomplete = $false
            foreach ($e in $errs) { if ($e.IncompleteInput) { $incomplete = $true; break } }
            if ($incomplete) {
                [Microsoft.PowerShell.PSConsoleReadLine]::AddLine()
            } else {
                [Console]::Write("$([char]27)]133;C$([char]7)")
                $global:__odytty_command_started = $true
                [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
            }
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
    fn zsh_and_fish_emit_the_private_edit_region_report() {
        // B-DESIGN §3.2/§3.3: the TIER-A edit-region signal rides the private
        // OSC `133;P;odytty-edit`. zsh publishes it per ZLE redraw
        // (zle-line-pre-redraw); fish has no per-keystroke hook, so its report
        // fires on prompt events and the terminal validates freshness.
        for shell in ["zsh", "fish"] {
            let snippet = snippet_for_shell(shell).expect("snippet");
            assert!(
                snippet.contains("133;P;odytty-edit;len=%d;cur=%d"),
                "{shell}: missing the edit-region report"
            );
        }
        let zsh = snippet_for_shell("zsh").expect("zsh");
        assert!(
            zsh.contains("zle-line-pre-redraw"),
            "zsh: report must fire on every ZLE redraw"
        );
        assert!(
            zsh.contains("nl="),
            "zsh: hard newlines must ride along as nl= offsets"
        );
        let fish = snippet_for_shell("fish").expect("fish");
        assert!(
            fish.contains("commandline --cursor"),
            "fish: cursor must come from the commandline builtin"
        );
        // bash/readline has no per-redraw hook: it must NOT claim the TIER-A
        // signal (it would always be stale), staying on the honest
        // RightEdgeUnknown => no-op path (B-DESIGN §3.4).
        let bash = snippet_for_shell("bash").expect("bash");
        assert!(
            !bash.contains("odytty-edit"),
            "bash must not emit the edit-region report"
        );
    }

    #[test]
    fn snippets_emit_osc7_working_directory() {
        for shell in ["bash", "zsh", "fish", "powershell"] {
            let snippet = snippet_for_shell(shell).expect("snippet");
            assert!(
                snippet.contains("]7;file://"),
                "{shell}: missing OSC 7 cwd emission"
            );
        }
    }

    #[test]
    fn bash_snippet_snapshots_exit_status_before_user_prompt_command() {
        // NF1-B (exit-status masking): a user PROMPT_COMMAND helper is loaded
        // from .bashrc BEFORE this snippet, so the reporter must read a status
        // SNAPSHOT taken at the very start of the PROMPT_COMMAND chain
        // (prepended), never `$?` after the user helper has clobbered it.
        let bash = snippet(ShellKind::Bash);
        assert!(
            bash.contains("__odytty_status_capture"),
            "missing the status capturer"
        );
        assert!(
            bash.contains("__ODYTTY_LAST_STATUS=$?"),
            "capturer must snapshot $?"
        );
        assert!(
            bash.contains("__odytty_prepend_prompt_command __odytty_status_capture"),
            "the capturer must be PREPENDED so it runs before any user helper"
        );
        assert!(
            bash.contains("local __odytty_status=${__ODYTTY_LAST_STATUS:-$?}"),
            "the reporter must read the snapshot, not raw $?"
        );
        // Fails-before: the old reporter captured raw `$?` directly, which the
        // user helper had already overwritten.
        assert!(
            !bash.contains("local __odytty_status=$?"),
            "reporter must not capture raw $? after user helpers clobber it"
        );
    }

    #[test]
    fn bash_snippet_guards_debug_trap_with_prompt_executing_flag() {
        // NF1 (phantom 133;C): the DEBUG trap must suppress OutputStart for
        // every command run *inside* PROMPT_COMMAND via a state flag — robust
        // against arbitrary user helper names — not the name-only `case` filter
        // (which cannot enumerate user helpers).
        let bash = snippet(ShellKind::Bash);
        assert!(
            bash.contains("__ODYTTY_PROMPT_EXECUTING=1"),
            "capturer must arm the prompt-phase flag"
        );
        assert!(
            bash.contains("if [ -n \"${__ODYTTY_PROMPT_EXECUTING-}\" ]; then\n      return"),
            "DEBUG trap must return early while the prompt-phase flag is armed"
        );
        assert!(
            bash.contains("unset __ODYTTY_PROMPT_EXECUTING"),
            "reporter must clear the flag so the next real command emits 133;C"
        );
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

    #[test]
    fn snippets_percent_encode_osc7_cwd() {
        // D-2 fails-before/passes-after: every emitter must percent-encode `%`
        // in the reported cwd. The OSC 7 parser treats a `%` not followed by
        // two hex digits as a malformed escape and drops the whole sequence, so
        // a raw `%` in a directory name would silently freeze cwd tracking at
        // the previous value. Encoding `%` -> `%25` at the source makes the
        // wire form a valid escape that round-trips back to the literal `%`.
        let bash = snippet(ShellKind::Bash);
        assert!(
            bash.contains("${PWD//\\%/%25}"),
            "bash must percent-encode % in the OSC 7 cwd"
        );
        let zsh = snippet(ShellKind::Zsh);
        assert!(
            zsh.contains("${PWD//\\%/%25}"),
            "zsh must percent-encode % in the OSC 7 cwd"
        );
        let fish = snippet(ShellKind::Fish);
        assert!(
            fish.contains("string replace -a '%' '%25'"),
            "fish must percent-encode % in the OSC 7 cwd"
        );
        let ps = snippet(ShellKind::PowerShell);
        assert!(
            ps.contains("-replace '%','%25'"),
            "powershell must percent-encode % in the OSC 7 cwd"
        );
    }

    #[test]
    fn powershell_snippet_gates_command_end_and_gates_output_start() {
        // D-3 fails-before/passes-after: `133;D` must be gated on a per-command
        // flag so no phantom `CommandEnd{exit:0}` is stamped before the first
        // command runs (mirrors the unix `__ODYTTY_COMMAND_STARTED` guard). The
        // old snippet emitted `133;D` unconditionally at the top of every
        // prompt, including the very first.
        let ps = snippet(ShellKind::PowerShell);
        assert!(
            ps.contains("if ($global:__odytty_command_started) {"),
            "133;D emission must be conditional on a command-started flag"
        );
        assert!(
            ps.contains("$global:__odytty_command_started = $true"),
            "the Enter handler must set the command-started flag on accept"
        );
        // D-4 fails-before/passes-after: `133;C` (OutputStart) must be emitted
        // only when the buffer parses as complete; an incomplete multiline
        // continuation must insert a newline instead of a spurious OutputStart.
        assert!(
            ps.contains("IncompleteInput"),
            "the Enter handler must detect incomplete (multiline) input"
        );
        assert!(
            ps.contains("AddLine()"),
            "incomplete input must continue the line, not accept it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bash_percent_encodes_osc7_cwd_end_to_end() {
        // D-2 end-to-end on the Linux/macOS legs: run bash with the real
        // snippet, cd into a directory whose name contains `%`, and confirm the
        // emitted OSC 7 payload carries the encoded `%25` form (fails-before:
        // the old snippet emitted the raw `%`, which the parser would drop).
        let Some(bash) = find_bash() else {
            return;
        };
        let base = temp_integration_dir("bash-pct");
        let pct_dir = base.join("50%off");
        fs::create_dir_all(&pct_dir).expect("mkdir");
        let rc = base.join("rc.bash");
        fs::write(&rc, format!("PS1='P\\$ '\n{BASH_SNIPPET}")).expect("write rc");

        let input = format!("cd '{}'\nexit\n", pct_dir.display());
        let out = run_bash_rc(&bash, &rc, &input);
        if !out.contains("\x1b]133;A") {
            // Interactive integration did not engage in this environment.
            let _ = fs::remove_dir_all(&base);
            return;
        }
        assert!(
            out.contains("50%25off"),
            "cwd with % must be percent-encoded in the OSC 7 payload: {out:?}"
        );
        assert!(
            !out.contains("file://") || !out.contains("/50%off\x07"),
            "the raw unencoded % form must not reach the wire: {out:?}"
        );
        let _ = fs::remove_dir_all(base);
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

    /// Locate a `bash` binary for the behavioral OSC-133 tests. Returns `None`
    /// (self-skip) where bash is absent so the tests stay green on minimal
    /// build hosts; where present (Linux/macOS dev + CI legs) they exercise the
    /// real DEBUG-trap / PROMPT_COMMAND interaction faithfully to nf1-repro.md.
    #[cfg(unix)]
    fn find_bash() -> Option<PathBuf> {
        [
            "/bin/bash",
            "/usr/bin/bash",
            "/usr/local/bin/bash",
            "/opt/homebrew/bin/bash",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
    }

    /// Drive an interactive bash with our rcfile, feed `input`, and return raw
    /// stdout (OSC bytes intact). `input` must terminate the session (feed
    /// `exit\n`); stdin EOF after the write is a second guard so the child can
    /// never wedge.
    #[cfg(unix)]
    fn run_bash_rc(bash: &Path, rc: &Path, input: &str) -> String {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let mut child = Command::new(bash)
            .arg("--rcfile")
            .arg(rc)
            .arg("-i")
            .env_remove("ODYTTY_SHELL_INTEGRATION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bash");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
        let output = child.wait_with_output().expect("wait bash");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Build an rcfile that loads a user PROMPT_COMMAND helper BEFORE the real
    /// `BASH_SNIPPET` — the realistic `.bashrc` ordering nf1-repro.md exercises.
    #[cfg(unix)]
    fn write_bash_rc_with_user_helper(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir).expect("dir");
        let rc = dir.join("rc.bash");
        let contents = format!(
            "__user_prompt_helper() {{ : ; }}\n\
             PROMPT_COMMAND='__user_prompt_helper'\n\
             PS1='P\\$ '\n\
             {BASH_SNIPPET}"
        );
        fs::write(&rc, contents).expect("write rc");
        rc
    }

    #[cfg(unix)]
    #[test]
    fn bash_reports_real_exit_status_past_a_user_prompt_command() {
        // NF1-B fails-before/passes-after (faithful to nf1-repro.md §4): with a
        // user PROMPT_COMMAND helper present, running `false` (exit 1) must
        // report 133;D;1, never 133;D;0. Before the prepended capturer, the
        // helper clobbered $? first and the reporter read 0.
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-status");
        let rc = write_bash_rc_with_user_helper(&dir);

        let out = run_bash_rc(&bash, &rc, "false\nexit\n");
        // Environment self-skip: if interactive integration did not engage at
        // all (no prompt-start marker), do not assert on an inert stream.
        if !out.contains("\x1b]133;A") {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            out.contains("\x1b]133;D;1\x07"),
            "failed command must report exit 1: {out:?}"
        );
        assert!(
            !out.contains("\x1b]133;D;0\x07"),
            "must not report success for a failed command: {out:?}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn bash_emits_no_phantom_output_start_before_first_prompt() {
        // NF1 fails-before/passes-after: a user PROMPT_COMMAND helper must not
        // make the DEBUG trap stamp a phantom 133;C before the first prompt's
        // 133;A. Before the prompt-phase flag, the helper's call tripped the
        // trap and the stream led with a stray OutputStart.
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-phantom");
        let rc = write_bash_rc_with_user_helper(&dir);

        let out = run_bash_rc(&bash, &rc, "echo hi\nexit\n");
        let Some(first_a) = out.find("\x1b]133;A") else {
            // Integration did not engage in this environment; self-skip.
            let _ = fs::remove_dir_all(&dir);
            return;
        };
        assert!(
            !out[..first_a].contains("\x1b]133;C"),
            "phantom OutputStart before the first prompt: {out:?}"
        );
        let _ = fs::remove_dir_all(dir);
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

    #[cfg(windows)]
    #[test]
    fn windows_shell_kind_detects_powershell_programs() {
        // D-12: the Windows `from_program` arm classifies the PowerShell family
        // (by basename, case-insensitively) and rejects cmd.exe, which has no
        // OSC 133 hook surface. Runs on the windows-latest leg.
        assert_eq!(
            ShellKind::from_program(OsStr::new("pwsh.exe")),
            Some(ShellKind::PowerShell)
        );
        assert_eq!(
            ShellKind::from_program(OsStr::new("C:\\Program Files\\PowerShell\\7\\pwsh.exe")),
            Some(ShellKind::PowerShell)
        );
        assert_eq!(
            ShellKind::from_program(OsStr::new("powershell.exe")),
            Some(ShellKind::PowerShell)
        );
        assert_eq!(
            ShellKind::from_program(OsStr::new("PowerShell.EXE")),
            Some(ShellKind::PowerShell)
        );
        assert!(ShellKind::from_program(OsStr::new("cmd.exe")).is_none());
        assert!(ShellKind::from_program(OsStr::new("C:\\Windows\\System32\\cmd.exe")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_apply_spawn_integration_injects_powershell_snippet() {
        // D-12: spawning a PowerShell attaches `-NoExit -Command <snippet>` with
        // the profile that installs the OSC 133 hooks. There IS Windows
        // spawn-time injection (the old "no Windows injection" seam comment was
        // stale).
        let mut command = CommandBuilder::new("pwsh.exe");
        apply_spawn_integration(&mut command);
        let args = command.args_for_test();
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], std::ffi::OsString::from("-NoExit"));
        assert_eq!(args[1], std::ffi::OsString::from("-Command"));
        let snippet = args[2].to_string_lossy();
        assert!(snippet.contains("ODYTTY_SHELL_INTEGRATION"));
        assert!(snippet.contains("133;A;click_events=1"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_apply_spawn_integration_skips_cmd() {
        // D-12: cmd.exe is unsupported, so no integration args are attached.
        let mut command = CommandBuilder::new("cmd.exe");
        apply_spawn_integration(&mut command);
        assert!(command.args_for_test().is_empty());
    }
}

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

/// How OdyTTY delivers shell integration to a shell family, for the Shell
/// Integration settings readout. The section renders one row per family from
/// this so the master switch never over-promises: it says exactly what the
/// switch does for each shell rather than implying a uniform capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationPosture {
    /// OdyTTY injects its OSC 133 snippet at spawn (bash/zsh/fish).
    Injected,
    /// Injected only on Windows. `pwsh` on Unix has no injection surface (the
    /// Unix injector has no PowerShell arm by design), so the readout hides the
    /// PowerShell row off-Windows.
    InjectedWindowsOnly,
    /// The shell ships its own integration config; OdyTTY detects it and points
    /// at the native setting but never injects (nushell).
    ConfigureNatively,
}

/// A shell family the Shell Integration section can describe. This is the
/// readout vocabulary and a deliberate superset of the injection-capable
/// [`ShellKind`]: it also names nushell, which OdyTTY recognizes and documents
/// but never injects into (nushell loads its own `$env.config.shell_integration`
/// and there is no clean spawn-time override). Keeping this separate from
/// [`ShellKind`] keeps the injection path total without an empty-snippet arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFamily {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Nushell,
}

impl ShellFamily {
    /// Every family the readout enumerates, in display order.
    pub const ALL: [ShellFamily; 5] = [
        ShellFamily::Bash,
        ShellFamily::Zsh,
        ShellFamily::Fish,
        ShellFamily::PowerShell,
        ShellFamily::Nushell,
    ];

    /// Classify a shell from a user-supplied name or program basename token.
    /// Recognizes the four injected families plus `nu`/`nushell` (detection
    /// only). Leading `-` (login-shell argv) is stripped by the caller side for
    /// [`ShellKind`]; this readout classifier accepts the bare token.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw
            .trim()
            .trim_start_matches('-')
            .to_ascii_lowercase()
            .as_str()
        {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "powershell" | "pwsh" => Some(Self::PowerShell),
            "nu" | "nushell" => Some(Self::Nushell),
            _ => None,
        }
    }

    /// Human-readable name for the readout row.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::PowerShell => "PowerShell",
            Self::Nushell => "nushell",
        }
    }

    /// How the switch reaches this family.
    pub fn posture(self) -> IntegrationPosture {
        match self {
            Self::Bash | Self::Zsh | Self::Fish => IntegrationPosture::Injected,
            Self::PowerShell => IntegrationPosture::InjectedWindowsOnly,
            Self::Nushell => IntegrationPosture::ConfigureNatively,
        }
    }

    /// The injection-capable [`ShellKind`] for this family, or `None` for
    /// nushell (detected and documented, never injected).
    pub fn injectable(self) -> Option<ShellKind> {
        match self {
            Self::Bash => Some(ShellKind::Bash),
            Self::Zsh => Some(ShellKind::Zsh),
            Self::Fish => Some(ShellKind::Fish),
            Self::PowerShell => Some(ShellKind::PowerShell),
            Self::Nushell => None,
        }
    }

    /// One-line readout summary shown in the Shell Integration section / docs.
    /// Honest about what the switch does for this family; nushell points at the
    /// native config rather than promising injection.
    pub fn readout(self) -> &'static str {
        match self {
            Self::Bash => {
                "OSC 133 prompt marks, OSC 7 cwd, click-to-position, button emitters; \
                 optional prompt-scoped key enhancement"
            }
            Self::Zsh => {
                "OSC 133 prompt marks, OSC 7 cwd, per-keystroke edit region, \
                 click-to-position, button emitters; optional prompt-scoped key enhancement"
            }
            Self::Fish => {
                "OSC 133 prompt marks, OSC 7 cwd, edit region, click-to-position, \
                 button emitters; fish 4+ manages the keyboard protocol itself"
            }
            Self::PowerShell => {
                "Windows only: OSC 133 prompt marks, OSC 7 cwd, button emitters; \
                 key bindings use the PSReadLine/Console API, not a VT protocol"
            }
            Self::Nushell => {
                "Configure natively: set $env.config.shell_integration.osc133/osc7/osc2 \
                 and use_kitty_protocol in your nushell config"
            }
        }
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
//   * array-valued PROMPT_COMMAND (Fedora/systemd) — assigning index zero would
//     place both OdyTTY hooks before the remaining array elements. A later
//     prompt helper then trips DEBUG and stamps `C` after `D`/`A`, erasing the
//     completed block's exit before PS1 stamps `B`. Array values are therefore
//     prepended/appended as array elements so the prompt-phase guard spans the
//     entire chain and the reporter is the final helper.
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

  # Bash did not expand PS0 until 4.4 (macOS still ships 3.2). Keep the
  # capability test argument-driven so its boundary is deterministic and the
  # installed decision is made once from BASH_VERSINFO.
  __odytty_bash_supports_ps0() {
    [ "$1" -gt 4 ] || { [ "$1" -eq 4 ] && [ "$2" -ge 4 ]; }
  }
  __ODYTTY_BASH_HAS_PS0=
  if __odytty_bash_supports_ps0 "${BASH_VERSINFO[0]:-0}" "${BASH_VERSINFO[1]:-0}"; then
    __ODYTTY_BASH_HAS_PS0=1
  fi

  # Percent-encode $PWD into an OSC 7 path. A directory name may contain BEL or
  # ESC bytes; embedded raw they would close the OSC 7 sequence and let the
  # tail inject a second, attacker-chosen control sequence. Every byte outside
  # the RFC 3986 unreserved set (plus '/' and ':' so path structure and drive
  # colons survive) is percent-encoded, so the payload is always exactly one
  # well-formed sequence. LC_ALL=C forces byte-wise iteration (multibyte runes
  # encode per UTF-8 byte). The result is cached and only recomputed when $PWD
  # changes, so the byte loop never runs on an unchanged directory.
  __odytty_encode_osc7() {
    if [ "${__ODYTTY_OSC7_PWD-}" = "$PWD" ]; then
      return
    fi
    __ODYTTY_OSC7_PWD="$PWD"
    local LC_ALL=C __odytty_str="$PWD" __odytty_out="" __odytty_safe __odytty_hex __odytty_ord
    while [ -n "$__odytty_str" ]; do
      __odytty_safe="${__odytty_str%%[!a-zA-Z0-9/:._~-]*}"
      __odytty_out="$__odytty_out$__odytty_safe"
      __odytty_str="${__odytty_str#"$__odytty_safe"}"
      if [ -n "$__odytty_str" ]; then
        printf -v __odytty_ord '%d' "'$__odytty_str"
        printf -v __odytty_hex '%%%02X' "$(( __odytty_ord & 0xFF ))"
        __odytty_out="$__odytty_out$__odytty_hex"
        __odytty_str="${__odytty_str#?}"
      fi
    done
    __ODYTTY_OSC7_ENC="$__odytty_out"
  }

  __odytty_prompt_command() {
    local __odytty_status=${__ODYTTY_LAST_STATUS:-$?}
    if [ -n "${__ODYTTY_COMMAND_STARTED-}" ]; then
      printf '\e]133;D;%s\a' "$__odytty_status"
      unset __ODYTTY_COMMAND_STARTED
    fi
    __odytty_encode_osc7
    printf '\e]7;file://%s\a' "$__ODYTTY_OSC7_ENC"
    printf '\e]133;A;click_events=1\a'
    unset __ODYTTY_PROMPT_EXECUTING
    # Prompt-scoped key enhancement (D-b): while the prompt owns the line, add
    # Kitty keyboard flag 0x1 (disambiguate ONLY -- Ctrl+C stays SIGINT). PS0
    # removes exactly that flag after readline accepts a real command and before
    # the command starts. Unlike the DEBUG trap, PS0 is not confused by commands
    # a Fedora prompt executes while rendering itself. Empty Enter prints no PS0,
    # and the idempotent add remains active for the next prompt. Only active when
    # OdyTTY advertises ODYTTY_KEY_ENHANCE; zero effect on launched programs.
    if [ -n "${ODYTTY_KEY_ENHANCE-}" ]; then
      printf '\e[=1;2u'
    fi
  }

  __odytty_debug_trap() {
    if [ -n "${__ODYTTY_PROMPT_EXECUTING-}" ]; then
      return
    fi
    case "$BASH_COMMAND" in
      __odytty_status_capture*|__odytty_prompt_command*|__odytty_debug_trap*|__odytty_append_prompt_command*|__odytty_prepend_prompt_command*) return ;;
    esac
    # Bash <4.4 never expands PS0. At the first real command boundary after a
    # prompt, remove the prompt-only Kitty disambiguation bit here instead.
    # PROMPT_COMMAND helpers are excluded by the guard above, internal hooks by
    # the case filter, and __ODYTTY_COMMAND_STARTED makes compound-command DEBUG
    # callbacks idempotent. Modern Bash stays on the earlier PS0 boundary.
    if [ -n "${ODYTTY_KEY_ENHANCE-}" ] &&
       [ -z "${__ODYTTY_BASH_HAS_PS0-}" ] &&
       [ -z "${__ODYTTY_COMMAND_STARTED-}" ]; then
      printf '\e[=1;3u'
    fi
    printf '\e]133;C\a'
    __ODYTTY_COMMAND_STARTED=1
  }

  __odytty_append_prompt_command() {
    case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
      "declare -a "*)
        local __odytty_pc
        for __odytty_pc in "${PROMPT_COMMAND[@]}"; do
          case ";$__odytty_pc;" in *";$1;"*) return ;; esac
        done
        PROMPT_COMMAND+=("$1")
        ;;
      *)
        case ";${PROMPT_COMMAND-};" in
          *";$1;"*) ;;
          ";"|";;" ) PROMPT_COMMAND="$1" ;;
          *) PROMPT_COMMAND="${PROMPT_COMMAND};$1" ;;
        esac
        ;;
    esac
  }

  __odytty_prepend_prompt_command() {
    case "$(declare -p PROMPT_COMMAND 2>/dev/null)" in
      "declare -a "*)
        local __odytty_pc
        for __odytty_pc in "${PROMPT_COMMAND[@]}"; do
          case ";$__odytty_pc;" in *";$1;"*) return ;; esac
        done
        PROMPT_COMMAND=("$1" "${PROMPT_COMMAND[@]}")
        ;;
      *)
        case ";${PROMPT_COMMAND-};" in
          *";$1;"*) ;;
          ";"|";;" ) PROMPT_COMMAND="$1" ;;
          *) PROMPT_COMMAND="$1;${PROMPT_COMMAND}" ;;
        esac
        ;;
    esac
  }

  # Button protocol emitters (docs/buttons.md). odytty_button prints a label
  # bracketed by the private OSC 133;P;odytty-button run, so OdyTTY renders a
  # clickable chip while any other terminal prints the plain label and drops
  # the unknown OSCs. odytty_button_clear invalidates all buttons, or every
  # button carrying one code. Both guard on ODYTTY_BUTTONS (the discovery
  # variable OdyTTY injects when its buttons setting is on) and degrade to the
  # plain label / a no-op without it, so scripts can call them unconditionally.
  odytty_button() {
    if [ $# -lt 2 ]; then
      echo 'usage: odytty_button CODE LABEL [ICON] [SCOPE]' >&2
      return 2
    fi
    case "$1" in
      ''|*[!0-9]*) echo 'odytty_button: CODE must be a positive integer' >&2; return 2 ;;
    esac
    if [ "$1" -eq 0 ]; then
      echo 'odytty_button: CODE must be a positive integer' >&2
      return 2
    fi
    if [ -z "${ODYTTY_BUTTONS-}" ]; then
      printf '%s' "$2"
      return 0
    fi
    printf '\e]133;P;odytty-button;code=%s%s%s\a%s\e]133;P;odytty-button;end\a' \
      "$1" "${3:+;icon=$3}" "${4:+;scope=$4}" "$2"
  }

  odytty_button_clear() {
    [ -n "${ODYTTY_BUTTONS-}" ] || return 0
    printf '\e]133;P;odytty-button;invalidate%s\a' "${1:+;code=$1}"
  }

  # Default key-enhancement bindings (D-b): give the prompt-scoped CSI-u keys
  # visible out-of-box behavior when OdyTTY advertises ODYTTY_KEY_ENHANCE. These
  # are ordinary readline (process-global) binds, but the sequences only ever
  # arrive while the prompt owns the line and the 0x1 flag is pushed, so binding
  # them globally is safe. Each bind is skipped when the user has already bound
  # the sequence: ~/.bashrc is sourced BEFORE this snippet, so a user rebind in
  # inputrc or .bashrc wins. To override afterwards, rebind the sequence with
  # `bind` (e.g. `bind '"\e[127;5u": kill-whole-line'`).
  if [ -n "${ODYTTY_KEY_ENHANCE-}" ]; then
    # Bash 4.4+ expands and prints PS0 after readline accepts a non-empty command
    # but before executing it. Prefix the user's existing PS0 with a removal of
    # only the disambiguate bit, so arbitrary child applications never inherit
    # the prompt protocol and prompt-rendering commands cannot disable it early.
    # Legacy Bash leaves the user's PS0 byte-for-byte untouched and uses the
    # guarded first-real-command DEBUG boundary above.
    if [ -n "${__ODYTTY_BASH_HAS_PS0-}" ]; then
      PS0=$'\e[=1;3u'"${PS0-}"
    fi

    __odytty_bind_if_unbound() {
      if ! { bind -p; bind -s; } 2>/dev/null | grep -qF -- "\"$1\""; then
        bind "\"$1\": $2" 2>/dev/null
      fi
    }
    # Ctrl+Backspace: delete the word before the cursor.
    __odytty_bind_if_unbound '\e[127;5u' 'backward-kill-word'
    # Shift+Enter: insert a literal newline (quoted-insert then LF) for
    # multi-line edits instead of submitting the line.
    __odytty_bind_if_unbound '\e[13;2u' '"\C-v\C-j"'
    # Ctrl+Enter: submit the line (safe placeholder; rebind as desired).
    __odytty_bind_if_unbound '\e[13;5u' 'accept-line'
  fi

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

  # Percent-encode $PWD into an OSC 7 path (see the bash snippet for the
  # injection rationale): raw BEL/ESC in a dirname would close the sequence and
  # inject a second one. Preserves RFC 3986 unreserved plus '/' and ':';
  # LC_ALL=C forces byte-wise iteration; cached per $PWD so the loop is skipped
  # on an unchanged directory.
  __odytty_encode_osc7() {
    if [ "${__ODYTTY_OSC7_PWD:-}" = "$PWD" ]; then
      return
    fi
    __ODYTTY_OSC7_PWD="$PWD"
    local LC_ALL=C __odytty_str="$PWD" __odytty_out="" __odytty_safe __odytty_hex __odytty_ord
    while [ -n "$__odytty_str" ]; do
      __odytty_safe="${__odytty_str%%[!a-zA-Z0-9/:._~-]*}"
      __odytty_out="$__odytty_out$__odytty_safe"
      __odytty_str="${__odytty_str#"$__odytty_safe"}"
      if [ -n "$__odytty_str" ]; then
        printf -v __odytty_ord '%d' "'$__odytty_str"
        printf -v __odytty_hex '%%%02X' "$(( __odytty_ord & 0xFF ))"
        __odytty_out="$__odytty_out$__odytty_hex"
        __odytty_str="${__odytty_str#?}"
      fi
    done
    __ODYTTY_OSC7_ENC="$__odytty_out"
  }

  __odytty_precmd() {
    local __odytty_status=$?
    if [ -n "${__ODYTTY_COMMAND_STARTED:-}" ]; then
      printf '\e]133;D;%s\a' "$__odytty_status"
      unset __ODYTTY_COMMAND_STARTED
    fi
    __odytty_encode_osc7
    printf '\e]7;file://%s\a' "$__ODYTTY_OSC7_ENC"
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

  # Prompt-scoped key enhancement (D-b): when OdyTTY advertises
  # ODYTTY_KEY_ENHANCE, push Kitty keyboard flag 0x1 (disambiguate ONLY --
  # Ctrl+C stays SIGINT) while the line editor owns the prompt, popped when the
  # line is accepted or aborted. line-init/line-finish pair once per prompt, so
  # push/pop stay balanced even on an empty Enter. Users then bind raw CSI-u
  # sequences (e.g. `bindkey '^[[13;5u' <widget>`). Zero effect on the programs
  # the shell launches; default off. Chain rather than clobber any existing
  # init/finish widget, mirroring the pre-redraw wrap above.
  if [ -n "${ODYTTY_KEY_ENHANCE-}" ]; then
    if (( ${+widgets[zle-line-init]} )); then
      zle -A zle-line-init __odytty_wrapped_line_init
      __odytty_line_init() {
        printf '\e[>1u'
        zle __odytty_wrapped_line_init -- "$@"
      }
    else
      __odytty_line_init() { printf '\e[>1u' }
    fi
    zle -N zle-line-init __odytty_line_init
    if (( ${+widgets[zle-line-finish]} )); then
      zle -A zle-line-finish __odytty_wrapped_line_finish
      __odytty_line_finish() {
        printf '\e[<1u'
        zle __odytty_wrapped_line_finish -- "$@"
      }
    else
      __odytty_line_finish() { printf '\e[<1u' }
    fi
    zle -N zle-line-finish __odytty_line_finish

    # Default key bindings (D-b): give the prompt-scoped CSI-u keys visible
    # out-of-box behavior. Each bind is skipped when the user has already bound
    # the sequence (a ~/.zshrc `bindkey`, sourced before this snippet, wins); to
    # override afterwards, rebind with `bindkey '\e[127;5u' <widget>`.
    __odytty_bindkey_if_unbound() {
      local existing="${$(bindkey "$1")##* }"
      if [[ "$existing" == "undefined-key" || -z "$existing" ]]; then
        bindkey "$1" "$2"
      fi
    }
    # Shift+Enter: insert a literal newline into the edit buffer (multi-line
    # edits) rather than submitting. A small widget keeps this clean in ZLE.
    __odytty_insert_newline() { LBUFFER+=$'\n' }
    zle -N __odytty_insert_newline
    # Ctrl+Backspace: delete the word before the cursor.
    __odytty_bindkey_if_unbound '\e[127;5u' backward-kill-word
    __odytty_bindkey_if_unbound '\e[13;2u' __odytty_insert_newline
    # Ctrl+Enter: submit the line (safe placeholder; rebind as desired).
    __odytty_bindkey_if_unbound '\e[13;5u' accept-line
  fi

  # Button protocol emitters (docs/buttons.md); same contract as the bash
  # helpers -- clickable in OdyTTY, plain label anywhere else.
  odytty_button() {
    if [ $# -lt 2 ]; then
      echo 'usage: odytty_button CODE LABEL [ICON] [SCOPE]' >&2
      return 2
    fi
    case "$1" in
      ''|*[!0-9]*) echo 'odytty_button: CODE must be a positive integer' >&2; return 2 ;;
    esac
    if [ "$1" -eq 0 ]; then
      echo 'odytty_button: CODE must be a positive integer' >&2
      return 2
    fi
    if [ -z "${ODYTTY_BUTTONS-}" ]; then
      printf '%s' "$2"
      return 0
    fi
    printf '\e]133;P;odytty-button;code=%s%s%s\a%s\e]133;P;odytty-button;end\a' \
      "$1" "${3:+;icon=$3}" "${4:+;scope=$4}" "$2"
  }

  odytty_button_clear() {
    [ -n "${ODYTTY_BUTTONS-}" ] || return 0
    printf '\e]133;P;odytty-button;invalidate%s\a' "${1:+;code=$1}"
  }

  case "$PS1" in
    *'133;B'*) ;;
    *) PS1="${PS1}%{\e]133;B\a%}" ;;
  esac
  add-zsh-hook precmd __odytty_precmd
  add-zsh-hook preexec __odytty_preexec
fi
"#;

// Duplicate-marks posture (fish >=4.0): fish emits its own OSC 133 A/B/C/D when
// it detects a cooperating terminal, so with integration on the terminal can
// receive each mark twice per prompt -- once from fish, once from this snippet.
// The decision here is TOLERATE, not suppress, for two reasons the consumer
// already backs:
//   * Row-anchored last-writer-wins: both emitters fire during the same prompt
//     render (no intervening newline), so their marks land on the SAME physical
//     row; the screen stores one mark per row (`prompt_mark = Some(kind)`), so
//     a doubled A/A, C/C, or D/D on a row collapses to a single mark of that
//     kind before it ever reaches `prompt_marks()`. See the row-collapse in
//     `Screen::handle_osc133`.
//   * Cross-row backstop: `prompt_marks::command_blocks` takes the FIRST
//     OutputStart/CommandEnd within a block and ignores the rest, so even if a
//     duplicate landed on an adjacent row (or a divergent duplicate `D`
//     arrived) the derived block is deterministic. Regression-pinned in
//     `prompt_marks::tests::doubled_fish_style_marks_derive_one_coherent_block`.
// Suppression was rejected: fish's native `133;A` does NOT carry
// `click_events=1`, so dropping the snippet's marks would forfeit the
// click-to-position advertisement (SH-CLICK) while keeping the doubling on the
// C/D side. Tolerating keeps click-to-move working and the mark stream stable.
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
        # Percent-encode $PWD into an OSC 7 path. `string escape --style=url`
        # keeps only RFC 3986 unreserved bytes, so a BEL/ESC in a dirname (which
        # would otherwise close the sequence and inject a second one) is encoded;
        # it also encodes '/', so '%2F' is restored to '/' to keep the path
        # structure. Builtins only -- no external forks.
        printf '\e]7;file://%s\a' (string escape --style=url -- $PWD | string replace -a '%2F' '/')
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

    # Button protocol emitters (docs/buttons.md); same contract as the bash
    # helpers -- clickable in OdyTTY, plain label anywhere else.
    function odytty_button --description 'Emit an OdyTTY clickable button label'
        if test (count $argv) -lt 2
            echo 'usage: odytty_button CODE LABEL [ICON] [SCOPE]' >&2
            return 2
        end
        if not string match -qr '^[0-9]+$' -- $argv[1]
            echo 'odytty_button: CODE must be a positive integer' >&2
            return 2
        end
        if test $argv[1] -eq 0
            echo 'odytty_button: CODE must be a positive integer' >&2
            return 2
        end
        if not set -q ODYTTY_BUTTONS
            printf '%s' "$argv[2]"
            return 0
        end
        set -l __odytty_params "code=$argv[1]"
        if test (count $argv) -ge 3; and test -n "$argv[3]"
            set __odytty_params "$__odytty_params;icon=$argv[3]"
        end
        if test (count $argv) -ge 4; and test -n "$argv[4]"
            set __odytty_params "$__odytty_params;scope=$argv[4]"
        end
        printf '\e]133;P;odytty-button;%s\a%s\e]133;P;odytty-button;end\a' "$__odytty_params" "$argv[2]"
    end

    function odytty_button_clear --description 'Invalidate OdyTTY buttons'
        if not set -q ODYTTY_BUTTONS
            return 0
        end
        if test (count $argv) -ge 1
            printf '\e]133;P;odytty-button;invalidate;code=%s\a' "$argv[1]"
        else
            printf '\e]133;P;odytty-button;invalidate\a'
        end
    end
end
"#;

// PowerShell shell-integration profile, injected on Windows with
// `-NoExit -Command`. Windows PowerShell 5.1 lacks the backtick-e escape, so the
// ESC/BEL bytes are built from `[char]27`/`[char]7`. The set-once
// `ODYTTY_SHELL_INTEGRATION` guard mirrors the unix snippets, the wrapped
// `prompt` emits `133;D` (previous command's `$LASTEXITCODE`, with a `-not $?`
// synthetic-nonzero fold so a failed cmdlet that never sets `$LASTEXITCODE`
// reports a failure instead of `D;0`) then `133;A;click_events=1` then the
// user's prompt then `133;B`, and the PSReadLine
// Enter handler emits `133;C` just before the command runs. The OSC 7 path is
// emitted only when the current location is on a `FileSystem` provider, and it
// carries `$PWD.ProviderPath` (the native filesystem path) rather than
// `$PWD.Path`: on a non-FileSystem PSDrive (registry `HKLM:`, cert, env) there
// is no filesystem cwd, so emitting one manufactured a bogus directory
// (`/HKLM:/SOFTWARE`) that later seeded a broken spawn (audit D-1); gating on the
// provider leaves cwd tracking untouched there instead. The path is
// percent-encoded (`%` -> `%25`) so a directory whose name contains `%` cannot
// produce a malformed escape that the parser would reject, freezing cwd
// tracking. `133;D` is gated on a per-command flag the Enter handler sets, so
// no phantom `CommandEnd{exit:0}` is stamped before the first command (mirrors
// the unix `__ODYTTY_COMMAND_STARTED` guard). The Enter handler emits `133;C`
// only when the buffer parses as complete; on an incomplete multiline
// continuation it inserts a newline (`AddLine`) without a spurious OutputStart.
// `click_events=1` matches the unix snippets; the click-to-position action
// stays consumer-gated by `sh_click` (default on). cmd.exe has no equivalent
// hook surface and is deliberately unsupported.
const POWERSHELL_SNIPPET: &str = r##"if (-not $env:ODYTTY_SHELL_INTEGRATION) {
    $env:ODYTTY_SHELL_INTEGRATION = "1"

    if (Test-Path Function:\prompt) {
        $global:__odytty_original_prompt = $function:prompt
    } else {
        $global:__odytty_original_prompt = { "PS $($executionContext.SessionState.Path.CurrentLocation)> " }
    }

    function global:prompt {
        # Capture $? BEFORE any other statement runs -- a variable assignment
        # succeeds and resets $? to $true, so the success flag of the user's
        # last command must be read on the very first line. $LASTEXITCODE only
        # tracks native executables and `exit`; a failed cmdlet (Get-ChildItem
        # on a missing path) leaves $? false but never touches $LASTEXITCODE, so
        # reporting the raw code would stamp 133;D;0 on a visible failure. When
        # the last command failed but the code still reads 0, synthesize 1 so
        # the command-status gutter never paints a failed cmdlet green. A native
        # exe's real nonzero code is preserved untouched.
        $__odytty_ok = $?
        $__odytty_exit = $LASTEXITCODE
        if ($null -eq $__odytty_exit) { $__odytty_exit = 0 }
        if (-not $__odytty_ok -and $__odytty_exit -eq 0) { $__odytty_exit = 1 }
        $esc = [char]27
        $bel = [char]7
        $out = ""
        if ($global:__odytty_command_started) {
            $out += "$esc]133;D;$__odytty_exit$bel"
            $global:__odytty_command_started = $false
        }
        if ($PWD.Provider.Name -eq 'FileSystem') {
            # Percent-encode each path segment so a control byte in a name cannot
            # close the OSC 7 sequence and inject a second one (parity with the
            # unix snippets; Windows forbids control chars in names, so this
            # closes the formal gap and also encodes spaces / non-ASCII).
            # Splitting on both separators and rejoining with '/' preserves the
            # path structure; the drive colon rides along percent-encoded and the
            # consumer decodes it before stripping the leading slash.
            $p = (($PWD.ProviderPath -split '[\\/]') | ForEach-Object { [uri]::EscapeDataString($_) }) -join '/'
            $out += "$esc]7;file:///$p$bel"
        }
        $out += "$esc]133;A;click_events=1$bel"
        $out += & $global:__odytty_original_prompt
        $out += "$esc]133;B$bel"
        $out
    }

    # Button protocol emitters (docs/buttons.md); the PowerShell spelling of
    # the unix odytty_button/odytty_button_clear helpers. Declared global: so
    # they survive the -NoExit -Command injection scope. Clickable in OdyTTY,
    # plain label anywhere else.
    function global:Write-OdyttyButton {
        param(
            [Parameter(Mandatory=$true)][uint32]$Code,
            [Parameter(Mandatory=$true)][string]$Label,
            [string]$Icon,
            [ValidateSet('block','sticky')][string]$Scope
        )
        if ($Code -lt 1) {
            throw 'Code must be a positive integer'
        }
        if (-not $env:ODYTTY_BUTTONS) {
            [Console]::Write($Label)
            return
        }
        $esc = [char]27
        $bel = [char]7
        $params = "code=$Code"
        if ($Icon) { $params = "$params;icon=$Icon" }
        if ($Scope) { $params = "$params;scope=$Scope" }
        [Console]::Write("$esc]133;P;odytty-button;$params$bel$Label$esc]133;P;odytty-button;end$bel")
    }

    function global:Clear-OdyttyButton {
        param([uint32]$Code)
        if (-not $env:ODYTTY_BUTTONS) {
            return
        }
        $esc = [char]27
        $bel = [char]7
        if ($PSBoundParameters.ContainsKey('Code')) {
            [Console]::Write("$esc]133;P;odytty-button;invalidate;code=$Code$bel")
        } else {
            [Console]::Write("$esc]133;P;odytty-button;invalidate$bel")
        }
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
    fn snippets_define_button_emitter_helpers() {
        // Button protocol emitters (docs/buttons.md): every integrated shell
        // gets a define helper and an invalidate helper speaking the Tier 2
        // `133;P;odytty-button` spelling. The label rides OUTSIDE the OSC (it
        // is the bracketed cell run), so non-supporting terminals print it as
        // plain text.
        for shell in ["bash", "zsh", "fish"] {
            let snippet = snippet_for_shell(shell).expect("snippet");
            assert!(
                snippet.contains("odytty_button"),
                "{shell}: missing the odytty_button helper"
            );
            assert!(
                snippet.contains("odytty_button_clear"),
                "{shell}: missing the odytty_button_clear helper"
            );
            assert!(
                snippet.contains("133;P;odytty-button;end"),
                "{shell}: define helper must close the bracketed run"
            );
            assert!(
                snippet.contains("133;P;odytty-button;invalidate"),
                "{shell}: clear helper must emit invalidate"
            );
        }
        let powershell = snippet_for_shell("powershell").expect("powershell");
        assert!(
            powershell.contains("function global:Write-OdyttyButton"),
            "powershell: missing the Write-OdyttyButton helper"
        );
        assert!(
            powershell.contains("function global:Clear-OdyttyButton"),
            "powershell: missing the Clear-OdyttyButton helper"
        );
        assert!(
            powershell.contains("]133;P;odytty-button;end"),
            "powershell: define helper must close the bracketed run"
        );
        assert!(
            powershell.contains("]133;P;odytty-button;invalidate"),
            "powershell: clear helper must emit invalidate"
        );
        assert!(
            powershell.contains("[ValidateSet('block','sticky')]"),
            "powershell: scope must be constrained to the protocol vocabulary"
        );
        // Discovery guard: every helper checks the ODYTTY_BUTTONS variable the
        // terminal injects when its buttons setting is on, degrading to the
        // plain label (define) or a no-op (clear) without it.
        for shell in ["bash", "zsh", "fish", "powershell"] {
            let snippet = snippet_for_shell(shell).expect("snippet");
            assert!(
                snippet.contains("ODYTTY_BUTTONS"),
                "{shell}: helpers must guard on the discovery variable"
            );
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
    fn shell_family_readout_covers_every_family_honestly() {
        // D-a/D-e: the Shell Integration readout enumerates one row per family,
        // and each row's posture matches how the switch actually reaches it.
        assert_eq!(ShellFamily::ALL.len(), 5);

        // The four injected families map back to an injection-capable ShellKind;
        // nushell is detection + docs only (no injection surface).
        assert_eq!(ShellFamily::Bash.injectable(), Some(ShellKind::Bash));
        assert_eq!(ShellFamily::Zsh.injectable(), Some(ShellKind::Zsh));
        assert_eq!(ShellFamily::Fish.injectable(), Some(ShellKind::Fish));
        assert_eq!(
            ShellFamily::PowerShell.injectable(),
            Some(ShellKind::PowerShell)
        );
        assert_eq!(ShellFamily::Nushell.injectable(), None);

        // Postures: bash/zsh/fish injected everywhere, PowerShell Windows-only,
        // nushell native-config.
        assert_eq!(ShellFamily::Bash.posture(), IntegrationPosture::Injected);
        assert_eq!(
            ShellFamily::PowerShell.posture(),
            IntegrationPosture::InjectedWindowsOnly
        );
        assert_eq!(
            ShellFamily::Nushell.posture(),
            IntegrationPosture::ConfigureNatively
        );

        // Every readout row is non-empty; the PowerShell row must state the
        // Windows/Console-API reality (never promise VT key bindings), and the
        // nushell row must point at the native config, not injection.
        for family in ShellFamily::ALL {
            assert!(!family.display_name().is_empty());
            assert!(!family.readout().is_empty());
        }
        assert!(ShellFamily::PowerShell.readout().contains("Windows only"));
        assert!(ShellFamily::PowerShell.readout().contains("PSReadLine"));
        assert!(
            ShellFamily::Nushell
                .readout()
                .contains("use_kitty_protocol")
        );
    }

    #[test]
    fn shell_family_detects_nushell_for_the_readout() {
        // D-e: nushell is recognized for the readout (detection + docs only).
        assert_eq!(ShellFamily::parse("nu"), Some(ShellFamily::Nushell));
        assert_eq!(ShellFamily::parse("nushell"), Some(ShellFamily::Nushell));
        assert_eq!(ShellFamily::parse("-nu"), Some(ShellFamily::Nushell));
        // The injected families still classify.
        assert_eq!(ShellFamily::parse("bash"), Some(ShellFamily::Bash));
        assert_eq!(ShellFamily::parse("pwsh"), Some(ShellFamily::PowerShell));
        // Unknown shells are None (no readout row, no injection).
        assert_eq!(ShellFamily::parse("cmd"), None);
        assert_eq!(ShellFamily::parse("dash"), None);
    }

    #[test]
    fn bash_and_zsh_prompt_scoped_key_enhancement_scopes_flag_one() {
        // D-b: bash and zsh enable Kitty keyboard flag 0x1 (disambiguate only)
        // while the prompt owns the line and remove it before the command runs.
        // Flag 0x8 would stop Ctrl+C generating SIGINT at the prompt, which the
        // design forbids.
        let bash = snippet(ShellKind::Bash);
        let zsh = snippet(ShellKind::Zsh);

        for (name, snip) in [("bash", bash), ("zsh", zsh)] {
            // Gated on the discovery variable OdyTTY injects only when the knob
            // is on; without it, no enhancement lifecycle is emitted.
            assert!(
                snip.contains("ODYTTY_KEY_ENHANCE"),
                "{name}: key enhancement must be gated on the discovery variable"
            );
            // Must NOT push the report-all-keys flag (0x8) -- Ctrl+C stays SIGINT.
            assert!(
                !snip.contains(r">8u") && !snip.contains(r">9u"),
                "{name}: must not push flags that break Ctrl+C SIGINT"
            );
        }

        // Bash uses idempotent add/remove operations. Bash 4.4+ removes through
        // PS0; legacy Bash (including macOS 3.2) removes at the guarded first
        // real-command DEBUG boundary.
        assert!(bash.contains(r"=1;2u"), "bash must add disambiguate mode");
        assert!(
            bash.contains(r"=1;3u"),
            "bash must remove disambiguate mode"
        );
        assert!(
            bash.contains("__odytty_bash_supports_ps0")
                && bash.contains("${BASH_VERSINFO[0]:-0}")
                && bash.contains("${BASH_VERSINFO[1]:-0}"),
            "bash must detect PS0 capability from the running shell"
        );
        assert!(
            bash.contains("if [ -n \"${__ODYTTY_BASH_HAS_PS0-}\" ]; then\n      PS0="),
            "modern bash must scope removal through PS0"
        );
        assert!(
            bash.contains(
                "[ -z \"${__ODYTTY_COMMAND_STARTED-}\" ]; then\n      printf '\\e[=1;3u'"
            ),
            "legacy bash must pop once at the first real-command DEBUG boundary"
        );

        // zsh uses the line-init/line-finish widget pair (chained, not
        // clobbered), mirroring the pre-redraw edit-region wrap.
        assert!(zsh.contains(r">1u"), "zsh must push disambiguate mode");
        assert!(zsh.contains(r"<1u"), "zsh must pop disambiguate mode");
        assert!(
            zsh.contains("zle -N zle-line-init __odytty_line_init"),
            "zsh must register a line-init widget for the push"
        );
        assert!(
            zsh.contains("zle -N zle-line-finish __odytty_line_finish"),
            "zsh must register a line-finish widget for the pop"
        );

        // fish and PowerShell get NO push/pop: fish manages the protocol itself
        // and PowerShell key bindings use the Console API, not a VT protocol.
        let fish = snippet(ShellKind::Fish);
        let ps = snippet(ShellKind::PowerShell);
        assert!(
            !fish.contains("ODYTTY_KEY_ENHANCE") && !fish.contains(">1u"),
            "fish must not carry the prompt-scoped key enhancement (self-managed)"
        );
        assert!(
            !ps.contains("ODYTTY_KEY_ENHANCE") && !ps.contains(">1u"),
            "PowerShell must not carry the push/pop (Console API path)"
        );
    }

    #[test]
    fn bash_and_zsh_key_enhancement_ship_default_binds() {
        // D-b follow-up: the knob must have visible out-of-box behavior, so the
        // bash/zsh snippets bind the prompt-scoped CSI-u keys under the
        // ODYTTY_KEY_ENHANCE guard -- Ctrl+Backspace (\e[127;5u) kills the
        // previous word, Shift+Enter (\e[13;2u) inserts a literal newline,
        // Ctrl+Enter (\e[13;5u) submits. Each is skipped when the user already
        // bound the sequence so a ~/.bashrc / ~/.zshrc rebind wins.
        let bash = snippet(ShellKind::Bash);
        let zsh = snippet(ShellKind::Zsh);

        // bash: readline binds via a skip-if-already-bound helper.
        assert!(
            bash.contains("__odytty_bind_if_unbound"),
            "bash must guard binds so a user rebind wins"
        );
        assert!(
            bash.contains(r"\e[127;5u") && bash.contains("backward-kill-word"),
            "bash must bind Ctrl+Backspace to backward-kill-word"
        );
        assert!(bash.contains(r"\e[13;2u"), "bash must bind Shift+Enter");
        assert!(
            bash.contains(r"\e[13;5u") && bash.contains("accept-line"),
            "bash must bind Ctrl+Enter to accept-line"
        );

        // zsh: bindkey via the same skip-if-bound guard + a newline widget.
        assert!(
            zsh.contains("__odytty_bindkey_if_unbound"),
            "zsh must guard binds so a user rebind wins"
        );
        assert!(
            zsh.contains(r"\e[127;5u") && zsh.contains("backward-kill-word"),
            "zsh must bind Ctrl+Backspace to backward-kill-word"
        );
        assert!(
            zsh.contains("__odytty_insert_newline") && zsh.contains(r"LBUFFER+=$'\n'"),
            "zsh must bind Shift+Enter to a literal-newline widget"
        );
        assert!(
            zsh.contains(r"\e[13;5u") && zsh.contains("accept-line"),
            "zsh must bind Ctrl+Enter to accept-line"
        );

        // All binds live under the key-enhancement guard, so a knob-off shell
        // installs nothing. fish (self-manages the protocol) and PowerShell
        // (Console API) carry no CSI-u default binds at all.
        let fish = snippet(ShellKind::Fish);
        let ps = snippet(ShellKind::PowerShell);
        assert!(
            !fish.contains(r"\e[127;5u") && !fish.contains("__odytty_bind"),
            "fish must not carry CSI-u default binds"
        );
        assert!(
            !ps.contains(r"\e[127;5u") && !ps.contains("bind_if_unbound"),
            "PowerShell must not carry CSI-u default binds"
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
        // Every emitter must percent-encode EVERY unsafe byte in the reported
        // cwd, not just `%`. A directory name may contain BEL or ESC bytes;
        // embedded raw they close the OSC 7 sequence and let the tail inject a
        // second control sequence (title change, OSC 52 write). The encoders
        // preserve only RFC 3986 unreserved plus the path separators and encode
        // everything else, so the payload is always exactly one well-formed
        // sequence that round-trips back to the literal path.
        let bash = snippet(ShellKind::Bash);
        assert!(
            bash.contains("__odytty_encode_osc7")
                && bash.contains("printf -v __odytty_hex '%%%02X' \"$(( __odytty_ord & 0xFF ))\""),
            "bash must byte-encode the OSC 7 cwd via the cached encoder, masking \
             the ordinal to one byte so bash 3.2 does not sign-extend bytes >= 0x80"
        );
        assert!(
            !bash.contains("${PWD//\\%/%25}"),
            "bash must not fall back to the %-only replacement"
        );
        let zsh = snippet(ShellKind::Zsh);
        assert!(
            zsh.contains("__odytty_encode_osc7")
                && zsh.contains("printf -v __odytty_hex '%%%02X' \"$(( __odytty_ord & 0xFF ))\""),
            "zsh must byte-encode the OSC 7 cwd via the cached encoder, masking \
             the ordinal to one byte to match the bash-3.2-portable form"
        );
        assert!(
            !zsh.contains("${PWD//\\%/%25}"),
            "zsh must not fall back to the %-only replacement"
        );
        let fish = snippet(ShellKind::Fish);
        assert!(
            fish.contains("string escape --style=url -- $PWD"),
            "fish must url-encode the OSC 7 cwd"
        );
        assert!(
            !fish.contains("string replace -a '%' '%25'"),
            "fish must not fall back to the %-only replacement"
        );
        let ps = snippet(ShellKind::PowerShell);
        assert!(
            ps.contains("[uri]::EscapeDataString"),
            "powershell must percent-encode each OSC 7 path segment"
        );
        assert!(
            !ps.contains("-replace '%','%25'"),
            "powershell must not fall back to the %-only replacement"
        );
    }

    #[test]
    fn bash_encode_osc7_is_cached_by_pwd() {
        // The byte loop is skipped when $PWD is unchanged so the encoder adds no
        // per-prompt cost on a stable directory.
        let bash = snippet(ShellKind::Bash);
        assert!(
            bash.contains("if [ \"${__ODYTTY_OSC7_PWD-}\" = \"$PWD\" ]; then"),
            "bash encoder must short-circuit on an unchanged PWD"
        );
        assert!(
            bash.contains("local LC_ALL=C"),
            "bash encoder must force byte-wise iteration with LC_ALL=C"
        );
    }

    #[test]
    fn powershell_snippet_gates_osc7_on_the_filesystem_provider() {
        // D-1 fails-before/passes-after: OSC 7 must be emitted only when the
        // current location is on the FileSystem provider, and it must carry
        // `$PWD.ProviderPath` (the native filesystem path), not `$PWD.Path`.
        // The old snippet emitted `file:///$($PWD.Path ...)` unconditionally, so
        // a non-FileSystem PSDrive (registry `HKLM:`, cert, env) manufactured a
        // bogus cwd (`/HKLM:/SOFTWARE`) that later seeded a broken spawn.
        let ps = snippet(ShellKind::PowerShell);
        assert!(
            ps.contains("if ($PWD.Provider.Name -eq 'FileSystem') {"),
            "OSC 7 emission must be gated on the FileSystem provider"
        );
        assert!(
            ps.contains("$PWD.ProviderPath -split"),
            "OSC 7 must use ProviderPath (native filesystem path), not Path"
        );
        assert!(
            !ps.contains("$PWD.Path"),
            "OSC 7 must not derive the cwd from $PWD.Path (provider-qualified)"
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

    #[test]
    fn powershell_snippet_folds_failed_cmdlet_into_nonzero_status() {
        // D-d fails-before/passes-after: `$LASTEXITCODE` only tracks native
        // executables and `exit`. A failed cmdlet (e.g. Get-ChildItem on a
        // missing path) leaves `$?` false but never touches `$LASTEXITCODE`, so
        // the old snippet reported `133;D;0` -- a visible failure painted as
        // success in the command-status gutter. The refinement captures `$?`
        // first (before any statement resets it) and, when the last command
        // failed but the code still reads 0, synthesizes a nonzero.
        let ps = snippet(ShellKind::PowerShell);
        assert!(
            ps.contains("$__odytty_ok = $?"),
            "must snapshot $? before any statement clobbers it"
        );
        assert!(
            ps.contains("if (-not $__odytty_ok -and $__odytty_exit -eq 0) { $__odytty_exit = 1 }"),
            "a failed cmdlet with a zero exit code must fold to a synthetic nonzero"
        );
        // The success flag must be captured on the FIRST line of the prompt
        // body, ahead of the `$LASTEXITCODE` read (an assignment resets $?).
        let ok_at = ps.find("$__odytty_ok = $?").expect("ok capture present");
        let exit_at = ps
            .find("$__odytty_exit = $LASTEXITCODE")
            .expect("exit read present");
        assert!(
            ok_at < exit_at,
            "$? must be read before $LASTEXITCODE, else the read resets it"
        );
        // A native exe's real nonzero code must be preserved untouched: the
        // fold only fires when the reported code is still 0.
        assert!(
            ps.contains("$__odytty_exit -eq 0"),
            "the fold must only apply when the exit code is still 0"
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

    /// Minimal percent-decoder mirroring the OSC 7 consumer's `%XX` rule, used
    /// to confirm the encoders round-trip. The production decoder
    /// (`core::screen::osc::percent_decode_path`) is module-private; this test
    /// copy keeps the same contract without widening its visibility.
    #[cfg(unix)]
    fn percent_decode_for_test(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(((h << 4) | l) as u8);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        out
    }

    #[cfg(unix)]
    #[test]
    fn bash_encodes_hostile_osc7_cwd_end_to_end() {
        // MED-01 end-to-end: a directory name carrying a full injection payload.
        // The BEL after "a b" would close OSC 7, then the `ESC]2;INJECT` title
        // change would ride the tail; a space and a non-ASCII byte prove general
        // byte encoding. With the encoder every unsafe byte is percent-encoded,
        // so the emitted OSC 7 is exactly one well-formed sequence that decodes
        // back to the real path and no injected sequence reaches the wire.
        let Some(bash) = find_bash() else {
            return;
        };
        let base = temp_integration_dir("bash-hostile");
        let name = "a b\x07\x1b]2;INJECT\x07\u{e9}";
        let dir = base.join(name);
        fs::create_dir_all(&dir).expect("mkdir");
        let rc = base.join("rc.bash");
        fs::write(&rc, format!("PS1='P\\$ '\n{BASH_SNIPPET}")).expect("write rc");

        // Feed bash pure-ASCII input that reconstructs the hostile leaf via
        // printf octal escapes, then `cd` into it. The raw control bytes never
        // pass through the interactive readline (which would mangle a `cd`
        // argument that literally contained an ESC); bash builds the exact bytes
        // internally. The base dir is ASCII, so single-quoting it is safe. The
        // octal escapes spell `a b <BEL> <ESC> ]2;INJECT <BEL> é` — byte-for-byte
        // the `name` created above.
        let input = format!(
            "cd \"$(printf '%s/a b\\007\\033]2;INJECT\\007\\303\\251' '{}')\"\nexit\n",
            base.display()
        );
        let out = run_bash_rc(&bash, &rc, &input);
        if !out.contains("\x1b]133;A") {
            // Interactive integration did not engage in this environment.
            let _ = fs::remove_dir_all(&base);
            return;
        }

        // The injected control sequence must never appear as raw bytes.
        assert!(
            !out.contains("\x1b]2;INJECT"),
            "hostile dirname leaked a raw title sequence onto the wire: {out:?}"
        );

        // The OSC 7 payload must be exactly one well-formed sequence that
        // decodes back to the real path. bash emits an OSC 7 at the first prompt
        // (the initial cwd) before `cd` runs, so take the LAST occurrence — the
        // prompt after the `cd` into the hostile directory.
        let marker = "\x1b]7;file://";
        let start = out.rfind(marker).expect("OSC 7 emitted");
        let rest = &out[start + marker.len()..];
        let end = rest.find('\x07').expect("OSC 7 BEL terminator");
        let payload = &rest[..end];
        let decoded = percent_decode_for_test(payload.as_bytes());
        let decoded = String::from_utf8_lossy(&decoded).into_owned();
        assert!(
            decoded.ends_with(name),
            "decoded OSC 7 path must round-trip the hostile dirname: got {decoded:?}"
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
            // This harness spawns bash DIRECTLY, bypassing the CommandBuilder
            // spawn path, so it models the product's nested-launch scrub itself:
            // strip an inherited ODYTTY_SHELL_INTEGRATION so the snippet guard
            // engages regardless of the test runner's own environment (the
            // runner may itself be an integrated odytty session). The product
            // scrub proper lives in the spawn path and is asserted at the
            // CommandBuilder/into_command layer (see pty::tests).
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

    /// Like [`run_bash_rc`] but with extra environment variables set on the
    /// child (e.g. `ODYTTY_KEY_ENHANCE=1` to exercise the default key binds).
    #[cfg(unix)]
    fn run_bash_rc_env(bash: &Path, rc: &Path, input: &str, env: &[(&str, &str)]) -> String {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let mut command = Command::new(bash);
        command
            .arg("--rcfile")
            .arg(rc)
            .arg("-i")
            .env_remove("ODYTTY_SHELL_INTEGRATION");
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
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
    fn bash_key_enhancement_default_bind_kills_previous_word() {
        // D-b follow-up acceptance: with the knob advertised
        // (ODYTTY_KEY_ENHANCE=1), the default Ctrl+Backspace (\e[127;5u) bind
        // must delete the previous word. Typing `printf 'OUT<%s>\n' one two`,
        // feeding the sequence, then Enter runs `printf ... one` -- so `OUT<one>`
        // is emitted and `OUT<two>` is not (the format brackets appear only in
        // the command's OUTPUT, never in readline's echo of the typed input, so
        // the assertion is immune to the echo stream).
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-keyenh");
        fs::create_dir_all(&dir).expect("dir");
        let rc = dir.join("rc.bash");
        fs::write(&rc, format!("PS1='P\\$ '\n{BASH_SNIPPET}")).expect("write rc");

        let out = run_bash_rc_env(
            &bash,
            &rc,
            "printf 'OUT<%s>\\n' one two\x1b[127;5u\nexit\n",
            &[("ODYTTY_KEY_ENHANCE", "1")],
        );
        // Self-skip if interactive integration/readline did not engage.
        if !out.contains("\x1b]133;A") {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            out.contains("OUT<one>"),
            "the surviving word must run: {out:?}"
        );
        assert!(
            !out.contains("OUT<two"),
            "Ctrl+Backspace must kill the last word before submit: {out:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn bash_ps0_capability_helper_classifies_legacy_and_modern_versions() {
        use std::process::{Command, Stdio};

        let Some(bash) = find_bash() else {
            return;
        };
        // Source the production snippet into a real Bash, then exercise both
        // sides of its argument-driven capability boundary regardless of which
        // Bash version the current CI leg provides.
        let script = format!(
            "{BASH_SNIPPET}\n\
             if __odytty_bash_supports_ps0 3 2; then exit 31; fi\n\
             if __odytty_bash_supports_ps0 4 3; then exit 32; fi\n\
             if ! __odytty_bash_supports_ps0 4 4; then exit 33; fi\n\
             if ! __odytty_bash_supports_ps0 5 0; then exit 34; fi\n\
             exit 0\n"
        );
        let status = Command::new(bash)
            .arg("--noprofile")
            .arg("--norc")
            .arg("-c")
            .arg(script)
            .env_remove("ODYTTY_SHELL_INTEGRATION")
            .env_remove("ODYTTY_KEY_ENHANCE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run bash capability boundary");
        assert_eq!(status.code(), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn bash_key_enhancement_adds_at_prompt_and_removes_before_commands() {
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-keyenh-lifecycle");
        fs::create_dir_all(&dir).expect("dir");
        let rc = dir.join("rc.bash");
        // Interactive Bash writes prompts (including PS0) to stderr. Merge it
        // into stdout inside the child so this harness sees the exact ordered
        // PTY-equivalent stream, and seed a user PS0 to pin coexistence.
        fs::write(
            &rc,
            format!("exec 2>&1\nPS0='USER-PS0'\nPS1='P\\$ '\n{BASH_SNIPPET}"),
        )
        .expect("write rc");

        let out = run_bash_rc_env(
            &bash,
            &rc,
            "printf 'CAP<%s>\\n' \"${__ODYTTY_BASH_HAS_PS0:-0}\"\nprintf 'PS0-CHECK<%s>\\n' \"$PS0\"\nprintf 'OUT\\n'\nexit\n",
            &[("ODYTTY_KEY_ENHANCE", "1")],
        );
        if !out.contains("\x1b]133;A") {
            let _ = fs::remove_dir_all(&dir);
            return;
        }

        let add = out
            .find("\x1b[=1;2u")
            .expect("prompt must add Kitty disambiguation");
        let remove = out[add..]
            .find("\x1b[=1;3u")
            .map(|offset| add + offset)
            .expect("PS0 must remove Kitty disambiguation");
        assert!(
            remove > add,
            "removal must follow prompt activation: {out:?}"
        );
        if out.contains("CAP<1>") {
            assert!(
                out.contains("\x1b[=1;3uUSER-PS0"),
                "modern Bash must remove through PS0 before preserving the user value: {out:?}"
            );
        } else {
            assert!(
                out.contains("CAP<0>"),
                "the installed capability decision must be observable: {out:?}"
            );
            assert!(
                out.contains("\x1b[=1;3u\x1b]133;C\x07"),
                "legacy Bash must remove at the first real-command DEBUG boundary: {out:?}"
            );
            assert!(
                out.contains("PS0-CHECK<USER-PS0>"),
                "legacy Bash must leave the non-executing user PS0 value untouched: {out:?}"
            );
        }

        let mut terminal = crate::core::Terminal::new(80, 24);
        terminal.advance(out.as_bytes());
        assert_eq!(
            terminal.keyboard_modes().kitty_keyboard_flags,
            0,
            "the real Bash stream must leave the exiting child in legacy mode"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn bash_legacy_key_enhancement_falls_back_at_first_debug_boundary() {
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-keyenh-legacy-fallback");
        fs::create_dir_all(&dir).expect("dir");
        let rc = dir.join("rc.bash");
        // Force the production legacy branch after installation and clear PS0
        // so this path is exercised even on a modern Linux Bash. On macOS 3.2
        // these are the naturally-selected semantics.
        fs::write(
            &rc,
            format!(
                "exec 2>&1\nPS1='P\\$ '\n{BASH_SNIPPET}\n\
                 __ODYTTY_BASH_HAS_PS0=\nPS0=\n"
            ),
        )
        .expect("write rc");

        let out = run_bash_rc_env(
            &bash,
            &rc,
            "printf 'LEGACY-OUT\\n'\nexit\n",
            &[("ODYTTY_KEY_ENHANCE", "1")],
        );
        if !out.contains("\x1b]133;A") {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        let add = out
            .find("\x1b[=1;2u")
            .expect("prompt must add Kitty disambiguation");
        let fallback = out[add..]
            .find("\x1b[=1;3u\x1b]133;C\x07")
            .map(|offset| add + offset)
            .expect("legacy DEBUG boundary must remove before OutputStart");
        assert!(fallback > add);

        let mut terminal = crate::core::Terminal::new(80, 24);
        terminal.advance(out.as_bytes());
        assert_eq!(
            terminal.keyboard_modes().kitty_keyboard_flags,
            0,
            "the forced legacy path must not leak prompt keyboard flags"
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
    fn bash_button_helper_emits_exact_wire_bytes() {
        // The define helper must emit byte-exact Tier 2 runs the B1 parser
        // accepts: params inside the OSC, label as plain bracketed cells, and
        // an `end` close. The clear helper covers both invalidate forms.
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-button");
        let rc = write_bash_rc_with_user_helper(&dir);

        let out = run_bash_rc(
            &bash,
            &rc,
            "export ODYTTY_BUTTONS=1\n\
             odytty_button 42 Deploy run sticky\n\
             odytty_button 7 Copy\n\
             odytty_button_clear\n\
             odytty_button_clear 9\n\
             exit\n",
        );
        if !out.contains("\x1b]133;A") {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            out.contains(
                "\x1b]133;P;odytty-button;code=42;icon=run;scope=sticky\x07\
                 Deploy\x1b]133;P;odytty-button;end\x07"
            ),
            "full-form define run malformed: {out:?}"
        );
        assert!(
            out.contains("\x1b]133;P;odytty-button;code=7\x07Copy\x1b]133;P;odytty-button;end\x07"),
            "minimal define run malformed: {out:?}"
        );
        assert!(
            out.contains("\x1b]133;P;odytty-button;invalidate\x07"),
            "invalidate-all form malformed: {out:?}"
        );
        assert!(
            out.contains("\x1b]133;P;odytty-button;invalidate;code=9\x07"),
            "invalidate-code form malformed: {out:?}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn bash_button_helper_degrades_to_plain_label_without_discovery_env() {
        // Without ODYTTY_BUTTONS in the environment (any other terminal, or
        // OdyTTY with the buttons setting off), the define helper prints the
        // bare label and the clear helper emits nothing, so scripts can call
        // them unconditionally.
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-button-degrade");
        let rc = write_bash_rc_with_user_helper(&dir);

        let out = run_bash_rc(
            &bash,
            &rc,
            "unset ODYTTY_BUTTONS\n\
             odytty_button 42 PlainLabel run sticky; echo rc=$?\n\
             odytty_button_clear\n\
             exit\n",
        );
        if !out.contains("\x1b]133;A") {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            out.contains("PlainLabel"),
            "label must still print without the discovery env: {out:?}"
        );
        assert!(
            !out.contains("odytty-button"),
            "no button OSC may be emitted without the discovery env: {out:?}"
        );
        assert!(out.contains("rc=0"), "degraded call must succeed: {out:?}");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn bash_button_helper_rejects_bad_codes_without_emitting() {
        // Zero, non-numeric, and missing-label invocations must fail (exit 2)
        // and emit NO button OSC at all -- a half-emitted define would leave
        // an open bracketed run in the stream.
        let Some(bash) = find_bash() else {
            return;
        };
        let dir = temp_integration_dir("bash-button-bad");
        let rc = write_bash_rc_with_user_helper(&dir);

        let out = run_bash_rc(
            &bash,
            &rc,
            "odytty_button 0 Nope; echo rc0=$?\n\
             odytty_button abc Nope; echo rc1=$?\n\
             odytty_button 5; echo rc2=$?\n\
             exit\n",
        );
        if !out.contains("\x1b]133;A") {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            !out.contains("odytty-button;code="),
            "rejected invocations must not emit a define: {out:?}"
        );
        for marker in ["rc0=2", "rc1=2", "rc2=2"] {
            assert!(out.contains(marker), "expected {marker} in: {out:?}");
        }
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

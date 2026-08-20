// SPDX-License-Identifier: GPL-3.0-only
//! The OSC 133 integration snippets themselves, and the selection of one by
//! shell kind.
//!
//! The snippets are pure text with no platform gate so the CLI can print any
//! of them anywhere. Their content is a compatibility contract with the
//! shells: the long comments above each one record why a construct is written
//! the way it is, and changing the text changes observable prompt behavior.

use super::detect::ShellKind;

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
pub(super) const BASH_SNIPPET: &str = r#"if [ -z "${ODYTTY_SHELL_INTEGRATION-}" ]; then
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
    # Kitty keyboard flag 0x1 (disambiguate). PS0 removes exactly that flag after
    # readline accepts a real command and before the command starts. Unlike the
    # DEBUG trap, PS0 is not confused by commands a Fedora prompt executes while
    # rendering itself. Empty Enter prints no PS0, and the idempotent add remains
    # active for the next prompt. Only active when OdyTTY advertises
    # ODYTTY_KEY_ENHANCE; zero effect on launched programs.
    #
    # WHILE THIS FLAG IS SET, Ctrl+C DOES NOT INTERRUPT. Disambiguation re-encodes
    # every Ctrl+key as CSI-u, so the terminal emits no 0x03 and the tty line
    # discipline raises no SIGINT; Ctrl+D and Ctrl+Z lose EOF and SIGTSTP the same
    # way, and readline has no bindable function that raises SIGINT to win them
    # back. That is why ODYTTY_KEY_ENHANCE is off by default and why the flag is
    # scoped to the prompt and dropped before any command runs.
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

pub(super) const ZSH_SNIPPET: &str = r#"if [ -z "${ODYTTY_SHELL_INTEGRATION:-}" ]; then
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
  # ODYTTY_KEY_ENHANCE, push Kitty keyboard flag 0x1 (disambiguate) while the
  # line editor owns the prompt, popped when the line is accepted or aborted.
  # line-init/line-finish pair once per prompt, so push/pop stay balanced even on
  # an empty Enter. Users then bind raw CSI-u sequences (e.g.
  # `bindkey '^[[13;5u' <widget>`). Zero effect on the programs the shell
  # launches; default off. Chain rather than clobber any existing init/finish
  # widget, mirroring the pre-redraw wrap above.
  #
  # WHILE THIS FLAG IS SET, Ctrl+C DOES NOT INTERRUPT. Disambiguation re-encodes
  # every Ctrl+key as CSI-u, so no 0x03 reaches the tty line discipline and no
  # SIGINT is raised; Ctrl+D and Ctrl+Z lose EOF and SIGTSTP the same way. ZLE can
  # be taught to recover -- `bindkey '^[[99;5u' send-break` restores Ctrl+C, and
  # the other Ctrl+keys the prompt needs must be bound the same way -- which is
  # why this is opt-in rather than a default.
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
    *) PS1="${PS1}"$'%{\e]133;B\a%}' ;;
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
pub(super) const FISH_SNIPPET: &str = r#"if not set -q ODYTTY_SHELL_INTEGRATION
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

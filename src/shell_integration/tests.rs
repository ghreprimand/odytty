// SPDX-License-Identifier: GPL-3.0-only
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
        bash.contains("[ -z \"${__ODYTTY_COMMAND_STARTED-}\" ]; then\n      printf '\\e[=1;3u'"),
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

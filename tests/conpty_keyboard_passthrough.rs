// SPDX-License-Identifier: GPL-3.0-only
//! ConPTY passthrough probes for the keyboard protocols (Kitty CSI-u and
//! xterm modifyOtherKeys).
//!
//! On Windows, conhost sits between the client program and the terminal:
//! client output is re-encoded by ConPTY's VT renderer, and terminal-side
//! input is parsed by conhost's input state machine and converted into
//! `INPUT_RECORD`s before the client sees it. Whether keyboard-protocol
//! sequences survive either direction depends on the conhost build (Windows
//! Terminal needed new conhost plumbing to support the Kitty protocol at
//! all), so it must be probed, not assumed.
//!
//! These probes document observed behavior on the CI `windows-latest` leg
//! (the authoritative Windows verification — there is no local Windows
//! machine). They are designed to PASS with either outcome: each asserts
//! only that the probe pipeline itself worked, and reports the observed
//! passthrough result in its output. A hard failure therefore means the
//! probe is broken, not that ConPTY behaves one way or the other; the
//! printed `PROBE-RESULT` lines are the finding (run the binary with
//! `--nocapture` to see them for passing tests).

#![cfg(windows)]

use std::ffi::OsString;
use std::io::Write;

use odytty::core::{Dimensions, Terminal};
use odytty::pty::PtySession;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// App → terminal: a client writes the Kitty keyboard query (`CSI ? u`), an
/// XTQMODKEYS query (`CSI ? 4 m`), and DA1 (`CSI c`) through ConPTY. If the
/// sequences reach the terminal intact, OdyTTY's parser answers the kitty
/// query and the XTQMODKEYS query; if conhost strips or rewrites them, the
/// replies are missing and the raw stream captures the mangling.
#[test]
fn conpty_keyboard_queries_app_to_terminal() {
    let script = concat!(
        "$e=[char]27;",
        "[Console]::Out.Write(\"$e[?u\");",
        "[Console]::Out.Write(\"$e[?4m\");",
        "[Console]::Out.Write(\"$e[c\");",
        "[Console]::Out.Write('conpty-kbd-probe-done');",
    );

    let dimensions = Dimensions::new(80, 24);
    let mut session = PtySession::spawn_exec(
        dimensions,
        OsString::from("powershell.exe"),
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(script),
        ],
        None,
    )
    .expect("spawn powershell under ConPTY");
    let bytes = session.read_to_end().expect("read ConPTY output");
    // EOF implies the child exited, so this resolves promptly; the status
    // discriminates a child that never ran from output that went astray.
    let exit_status = session.wait().expect("wait for probe child");

    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.advance(&bytes);

    // The sentinel proves the client ran and its output traversed ConPTY.
    let text = terminal.screen().plain_text();
    assert!(
        text.contains("conpty-kbd-probe-done"),
        "probe output never reached the terminal; child exit status {exit_status:?}; \
         raw ConPTY stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );

    // The finding: did the queries arrive intact? OdyTTY replies `CSI ? 0 u`
    // to the kitty query and `CSI > 4 ; 0 m` to XTQMODKEYS iff its parser saw
    // them unmodified.
    let replies = terminal.take_host_output();
    let kitty_query_survived = replies.windows(5).any(|w| w == b"\x1b[?0u");
    let xtqmodkeys_survived = replies.windows(7).any(|w| w == b"\x1b[>4;0m");
    println!(
        "PROBE-RESULT app->terminal: kitty CSI-u query passthrough = {kitty_query_survived}, \
         XTQMODKEYS passthrough = {xtqmodkeys_survived}"
    );
    if !(kitty_query_survived && xtqmodkeys_survived) {
        println!(
            "PROBE-RESULT raw ConPTY stream ({} bytes) hex = {}",
            bytes.len(),
            hex(&bytes)
        );
    }
}

/// Terminal → app: OdyTTY writes a CSI-u key encoding (Ctrl+Enter under the
/// disambiguate flag, `CSI 13 ; 5 u`) into the ConPTY input pipe, exactly as
/// the encoder would for a live keystroke. A client reads whatever conhost's
/// input converter delivers and echoes it back as hex. Identity means CSI-u
/// input survives to apps; anything else captures the INPUT_RECORD
/// translation residue.
#[test]
fn conpty_csi_u_terminal_to_app() {
    // The client drains keys for a bounded window and prints their char codes
    // as hex between markers. ReadKey sees the cooked key events conhost
    // produces from the VT input stream — the same view a console app gets.
    let script = concat!(
        "$sw=[Diagnostics.Stopwatch]::StartNew();$codes=@();",
        "while($sw.ElapsedMilliseconds -lt 5000){",
        "if([Console]::KeyAvailable){",
        "$k=[Console]::ReadKey($true);$codes+=[int]$k.KeyChar;",
        "if([int]$k.KeyChar -eq 10 -or [int]$k.KeyChar -eq 13){break}",
        "}else{Start-Sleep -Milliseconds 50}}",
        "$hex=($codes|ForEach-Object{$_.ToString('x2')}) -join '';",
        "[Console]::Out.Write(\"probe<$hex>done\");",
    );

    let dimensions = Dimensions::new(80, 24);
    let mut session = PtySession::spawn_exec(
        dimensions,
        OsString::from("powershell.exe"),
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(script),
        ],
        None,
    )
    .expect("spawn powershell under ConPTY");

    // The exact bytes the encoder produces for Ctrl+Enter under flag 0x1,
    // terminated with CR so the client's drain loop always ends.
    let mut writer = session.take_writer().expect("take ConPTY writer");
    writer
        .write_all(b"\x1b[13;5u\r")
        .and_then(|()| writer.flush())
        .expect("write CSI-u bytes into the ConPTY input pipe");

    let bytes = session.read_to_end().expect("read ConPTY output");
    // EOF implies the child exited, so this resolves promptly; the status
    // discriminates a child that never ran from output that went astray.
    let exit_status = session.wait().expect("wait for probe child");
    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.advance(&bytes);
    let text = terminal.screen().plain_text();

    // The pipeline invariant: the client ran, drained input, and reported.
    let (Some(start), Some(end)) = (text.find("probe<"), text.find(">done")) else {
        panic!(
            "probe report never reached the terminal; child exit status {exit_status:?}; \
             raw ConPTY stream was {} bytes, hex = {}",
            bytes.len(),
            hex(&bytes)
        );
    };
    let observed = &text[start + "probe<".len()..end];

    // The finding: identity would be 1b5b31333b3575 (ESC [ 1 3 ; 5 u) plus the
    // 0d terminator. Anything else is conhost's INPUT_RECORD translation.
    let identity = observed.starts_with("1b5b31333b3575");
    println!(
        "PROBE-RESULT terminal->app: CSI-u input survives ConPTY = {identity}, \
         observed key stream hex = <{observed}>"
    );
}

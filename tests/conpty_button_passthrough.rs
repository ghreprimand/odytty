// SPDX-License-Identifier: GPL-3.0-only
//! ConPTY passthrough probe for the button protocol OSCs (Button Protocol B1).
//!
//! The single biggest Windows risk for program-defined buttons is upstream of
//! OdyTTY: ConPTY historically re-encodes the VT stream and has been known to
//! drop or rewrite sequences it does not recognize, and passthrough behavior
//! varies by Windows build. This probe answers the question empirically on the
//! CI `windows-latest` leg (the authoritative Windows verification — there is
//! no local Windows machine): a Windows-native program running under the real
//! ConPTY emits both button spellings, and the test asserts the sequences
//! arrive at the terminal intact — i.e. the parse lands and the definitions
//! intern.
//!
//! If this test fails on a given Windows build, ConPTY is stripping the
//! sequences there: the feature still works for WSL sessions and any direct
//! byte source, but the native-Windows-shell emitter story would depend on
//! ConPTY passthrough. That finding gates the emitter phase and must be
//! recorded in the protocol documentation.

#![cfg(windows)]

use std::ffi::OsString;

use odytty::core::{Dimensions, Terminal};
use odytty::pty::PtySession;

/// Render a byte stream as lowercase hex for failure diagnostics. A failing
/// probe must carry the complete raw ConPTY stream in its panic message:
/// there is no interactive Windows debugging surface, so the panic text is
/// the only evidence of what actually traversed (or failed to traverse) the
/// pseudoconsole pipe.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Emit both button spellings from PowerShell under the real ConPTY loop and
/// assert the OdyTTY parser observes them unmodified.
#[test]
fn conpty_passes_button_oscs_through_unmodified() {
    // PowerShell builds the raw bytes from numeric char codes, so no escape
    // byte appears literally in the command line. BEL termination avoids any
    // quoting interaction with the ST (`ESC \`) form; both terminators are
    // legal for these OSCs.
    let script = concat!(
        "$e=[char]27;$b=[char]7;",
        "[Console]::Out.Write(\"$e]1337;Button=type=custom;code=42;icon=star$b\");",
        "[Console]::Out.Write(\"$e]133;P;odytty-button;code=7$b\");",
        "[Console]::Out.Write('Retry');",
        "[Console]::Out.Write(\"$e]133;P;odytty-button;end$b\");",
        "[Console]::Out.Write('conpty-button-probe-done');",
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
    // `read_to_end` only returns at pipe EOF, which the session produces
    // after the child exits, so this wait resolves promptly. The exit status
    // discriminates between failure modes: a clean exit 0 with a short
    // stream means the output went somewhere other than the pseudoconsole
    // pipe, while an abnormal status (e.g. a DLL-init failure) means the
    // child never ran its script at all.
    let exit_status = session.wait().expect("wait for probe child");

    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.set_buttons_enabled(true);
    terminal.advance(&bytes);

    // The sentinel proves the emitting program ran to completion and its
    // output traversed the ConPTY loop into the parser.
    let text = terminal.screen().plain_text();
    assert!(
        text.contains("conpty-button-probe-done"),
        "probe output never reached the terminal; child exit status {exit_status:?}; \
         raw ConPTY stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );
    // The label text prints as ordinary cells (the Tier 2 degrade contract).
    assert!(
        text.contains("Retry"),
        "the bracketed label text must render as plain cells; child exit status \
         {exit_status:?}; raw ConPTY stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );
    // Both definitions interned: the OSC payloads arrived byte-intact. If
    // ConPTY stripped or rewrote either sequence, the count drops and this
    // assertion is the finding (see the module doc for what that means).
    assert_eq!(
        terminal.button_entry_count(),
        2,
        "expected both button spellings to survive ConPTY passthrough; child exit \
         status {exit_status:?}; raw stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );
}

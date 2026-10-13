// SPDX-License-Identifier: GPL-3.0-only
//! ConPTY passthrough probe for the button protocol OSCs.
//!
//! The single biggest Windows risk for program-defined buttons is upstream of
//! OdyTTY: ConPTY historically re-encodes the VT stream and has been known to
//! drop or rewrite sequences it does not recognize, and passthrough behavior
//! varies by Windows build. This probe answers the question empirically on the
//! CI `windows-latest` leg (the authoritative Windows verification - there is
//! no local Windows machine): a Windows-native program running under the real
//! ConPTY emits both button spellings, and the test asserts the sequences
//! arrive at the terminal intact - i.e. the parse lands and the definitions
//! intern.
//!
//! If this test fails on a given Windows build, ConPTY is stripping the
//! sequences there: the feature still works for WSL sessions and any direct
//! byte source, but the native-Windows-shell emitter story would depend on
//! ConPTY passthrough. That finding gates the emitter phase and must be
//! recorded in the protocol documentation.

#![cfg(windows)]

use std::ffi::OsString;
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use odytty::core::{ButtonScope, ButtonState, Dimensions, Terminal};
use odytty::pty::PtySession;

/// Longest a probe may run before its child is killed and the test fails.
const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// Most output a probe may produce before its child is killed and the test
/// fails; the real probes print well under a kilobyte.
const PROBE_MAX_BYTES: usize = 256 * 1024;
/// How much of the end of the raw stream a failure message carries.
const DIAGNOSTIC_TAIL: usize = 4096;

/// Render the end of a byte stream as lowercase hex for failure diagnostics.
/// A failing probe carries the tail of the raw ConPTY stream in its panic
/// message: there is no interactive Windows debugging surface, so the panic
/// text is the evidence of what traversed (or failed to traverse) the
/// pseudoconsole pipe. The tail is capped so a flood cannot grow the message.
fn hex(bytes: &[u8]) -> String {
    let tail = &bytes[bytes.len().saturating_sub(DIAGNOSTIC_TAIL)..];
    let text: String = tail.iter().map(|b| format!("{b:02x}")).collect();
    if tail.len() < bytes.len() {
        format!("(last {} bytes) {text}", tail.len())
    } else {
        text
    }
}

/// The output of one bounded probe run.
struct ProbeRun {
    bytes: Vec<u8>,
    /// The child's exit status, or why none was obtained.
    exit_status: String,
    /// Set when the run was cut short (deadline or output cap) and the child
    /// was killed.
    aborted: Option<&'static str>,
}

/// Run `session` to completion with a deadline and an output cap. A reader
/// thread forwards chunks over a channel so the test thread never blocks in a
/// read; on the deadline or the cap the child is killed and the reader forced
/// to end, so a ConPTY end-of-file regression or an output flood fails the
/// test instead of hanging or growing it.
fn run_bounded(session: &mut PtySession) -> ProbeRun {
    let started = Instant::now();
    let mut reader = session.try_clone_reader().expect("clone ConPTY reader");
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(read) = reader.read(&mut chunk) {
            if read == 0 || sender.send(chunk[..read].to_vec()).is_err() {
                break;
            }
        }
    });

    let mut bytes = Vec::new();
    let mut aborted = None;
    loop {
        let remaining = PROBE_DEADLINE.saturating_sub(started.elapsed());
        match receiver.recv_timeout(remaining) {
            Ok(chunk) => {
                let room = PROBE_MAX_BYTES.saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if chunk.len() > room {
                    aborted = Some("output cap reached");
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                aborted = Some("deadline reached");
                break;
            }
            // The reader ended: end of file.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if aborted.is_some() {
        let _ = session.kill();
        session.force_reader_eof();
    }
    // The child has exited or been killed; poll for its status until the
    // overall deadline instead of waiting without a bound.
    let exit_status = loop {
        match session.try_wait() {
            Ok(Some(status)) => break format!("{status:?}"),
            Ok(None) if started.elapsed() < PROBE_DEADLINE => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = session.kill();
                aborted.get_or_insert("child did not exit before the deadline");
                break "still running".to_owned();
            }
            Err(error) => break format!("wait failed: {error}"),
        }
    };
    ProbeRun {
        bytes,
        exit_status,
        aborted,
    }
}

/// Baseline output-path probe: plain `cmd.exe /C echo` under the same spawn
/// path. cmd writes through its standard output handle with no host layer in
/// between, so this discriminates a general std-handle binding defect (this
/// fails too) from behavior specific to PowerShell's console host (this
/// passes while the PowerShell probe fails). The two are independent tests;
/// compare their results, not their order.
#[test]
fn conpty_cmd_echo_reaches_the_pty_stream() {
    let dimensions = Dimensions::new(80, 24);
    let mut session = PtySession::spawn_exec(
        dimensions,
        OsString::from("cmd.exe"),
        vec![
            OsString::from("/C"),
            OsString::from("echo conpty-cmd-probe-done"),
        ],
        None,
    )
    .expect("spawn cmd under ConPTY");
    let ProbeRun {
        bytes,
        exit_status,
        aborted,
    } = run_bounded(&mut session);
    assert!(
        aborted.is_none(),
        "cmd probe aborted ({aborted:?}); child exit status {exit_status}; raw ConPTY \
         stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );

    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.advance(&bytes);
    let text = terminal.screen().plain_text();
    assert!(
        text.contains("conpty-cmd-probe-done"),
        "plain cmd.exe echo never reached the terminal; child exit status \
         {exit_status}; raw ConPTY stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );
}

/// A hit cell, its button code, scope and state, and the hit rectangle.
type HitIdentity = (usize, usize, u32, ButtonScope, ButtonState, usize, usize);

/// The button hits of `terminal` over its whole viewport, without the
/// interned ids (which only name the order of interning).
fn hit_map(terminal: &Terminal, dimensions: Dimensions) -> Vec<HitIdentity> {
    let mut hits = Vec::new();
    for row in 0..dimensions.rows {
        for column in 0..dimensions.columns {
            if let Some(hit) = terminal.button_at(0, row, column) {
                hits.push((
                    row,
                    column,
                    hit.code,
                    hit.scope,
                    hit.state,
                    hit.start_col,
                    hit.len,
                ));
            }
        }
    }
    hits
}

/// Emit both button spellings from PowerShell under the real ConPTY loop and
/// assert the OdyTTY parser observes them unmodified.
#[test]
fn conpty_passes_button_oscs_through_unmodified() {
    // PowerShell builds the raw bytes from numeric char codes, so no escape
    // byte appears literally in the command line. BEL termination avoids any
    // quoting interaction with the ST (`ESC \`) form; both terminators are
    // legal for these OSCs.
    //
    // The leading diagnostics discriminate output paths when the probe fails:
    // `IsOutputRedirected` reports whether the child's standard output handle
    // is a console handle at all (it wraps `GetFileType`), and `Write-Host`
    // goes through the console API (the host UI) rather than the standard
    // handle, so its marker appearing in the pseudoconsole stream while the
    // `[Console]::Out` payload does not would pin the defect to std-handle
    // binding rather than console attachment.
    let script = concat!(
        "$e=[char]27;$b=[char]7;",
        "Write-Host \"wh-marker redir=$([Console]::IsOutputRedirected)\";",
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
    // The run is bounded: the child is killed and the test fails at the
    // deadline or the output cap. The exit status discriminates between
    // failure modes: a clean exit 0 with a short stream means the output went
    // somewhere other than the pseudoconsole pipe, while an abnormal status
    // (e.g. a DLL-init failure) means the child never ran its script at all.
    let ProbeRun {
        bytes,
        exit_status,
        aborted,
    } = run_bounded(&mut session);
    assert!(
        aborted.is_none(),
        "button probe aborted ({aborted:?}); child exit status {exit_status}; raw \
         ConPTY stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );

    let mut terminal = Terminal::new(dimensions.columns, dimensions.rows);
    terminal.set_buttons_enabled(true);
    terminal.advance(&bytes);

    // The sentinel proves the emitting program ran to completion and its
    // output traversed the ConPTY loop into the parser.
    let text = terminal.screen().plain_text();
    assert!(
        text.contains("conpty-button-probe-done"),
        "probe output never reached the terminal; child exit status {exit_status}; \
         raw ConPTY stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );
    // The label text prints as ordinary cells (the Tier 2 degrade contract).
    assert!(
        text.contains("Retry"),
        "the bracketed label text must render as plain cells; child exit status \
         {exit_status}; raw ConPTY stream was {} bytes, hex = {}",
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
         status {exit_status}; raw stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );

    // Two entries could still be two altered records, so compare the admitted
    // buttons with those of the exact sequences the script writes, fed
    // straight to a second terminal at the position where the label landed.
    // The labelled span (code 7) must match cell for cell: same code, scope,
    // state and hit rectangle. The point button's chip rectangle depends on
    // the cells around it, which ConPTY may lay out differently, so for it
    // the identity (code, scope, state) must match.
    let (row, column) = text
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            line.find("Retry")
                .map(|byte| (row, line[..byte].chars().count()))
        })
        .expect("the label row was found above");
    let mut reference = Terminal::new(dimensions.columns, dimensions.rows);
    reference.set_buttons_enabled(true);
    reference.advance(
        format!(
            "\x1b[{};{}H\x1b]1337;Button=type=custom;code=42;icon=star\x07\
             \x1b]133;P;odytty-button;code=7\x07Retry\x1b]133;P;odytty-button;end\x07",
            row + 1,
            column + 1
        )
        .as_bytes(),
    );
    let expected = hit_map(&reference, dimensions);
    let actual = hit_map(&terminal, dimensions);
    let labelled = |map: &[HitIdentity]| -> Vec<HitIdentity> {
        map.iter().copied().filter(|hit| hit.2 == 7).collect()
    };
    assert_eq!(
        labelled(&expected).len(),
        5,
        "the reference stream must admit the five-cell labelled button: {expected:?}"
    );
    assert_eq!(
        labelled(&actual),
        labelled(&expected),
        "the labelled button admitted through ConPTY differs from the exact sequence; child \
         exit status {exit_status}; raw stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );
    let identities = |map: &[HitIdentity]| -> Vec<String> {
        let mut found: Vec<String> = map
            .iter()
            .map(|hit| format!("{} {:?} {:?}", hit.2, hit.3, hit.4))
            .collect();
        found.sort();
        found.dedup();
        found
    };
    assert_eq!(
        identities(&actual),
        identities(&expected),
        "the buttons admitted through ConPTY differ from the exact sequences; child exit \
         status {exit_status}; raw stream was {} bytes, hex = {}",
        bytes.len(),
        hex(&bytes)
    );
}

// SPDX-License-Identifier: GPL-3.0-only
//! Background Test Connection probe worker.
//!
//! The Add / Edit connection form's *Test Connection* action hands a built host
//! here to be probed on a background thread so the UI never blocks. The probe
//! runs the system `ssh` binary - argv-only, never through a shell - as a
//! non-interactive `BatchMode` one-shot that only reports reachability +
//! key/agent auth (it has no password and must never store one). The classified
//! result is sent back over a channel and a redraw is woken so the form renders
//! it. A hard timeout guarantees the worker (and the form) never hang.

use std::process::Stdio;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::native::event_waker::EventWaker;

use super::super::pty::UserEvent;
use super::super::session::SessionToken;
use crate::ssh_connect::{PROBE_EVIDENCE_LINE_CAP, ProbeClass, ProbeEvidence, SshCommand};

/// Completion channel bound to the requesting form's identity.
pub(super) struct PendingConnectionProbe {
    pub(super) identity: std::sync::Arc<()>,
    pub(super) receiver: std::sync::mpsc::Receiver<Result<ProbeClass, String>>,
}

/// Hard cap on a probe: `ConnectTimeout` (5s) plus 1s slack, so a wedged `ssh`
/// can never keep the form spinning.
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

/// How long the probe waits for the stderr reader after the child is gone. A
/// descendant that inherited stderr (a `ProxyCommand`, for example) can hold
/// the pipe open past the child's exit; the probe then classifies from what
/// was read so far and leaves the reader to finish on its own.
const PROBE_STDERR_GRACE: Duration = Duration::from_millis(500);

/// Spawn the probe on a detached worker thread. Fire-and-forget: the worker owns
/// the command, sender, and proxy; the caller returns immediately with the
/// receiver end already stored.
///
/// Returns the thread-spawn result. Under thread exhaustion the worker cannot be
/// created; without a returned error the form would sit in "Testing…" forever
/// (the sender drops but polling never sets a state). The caller drives a visible
/// error state in that case (LOW-02).
pub(super) fn spawn_connection_probe(
    command: SshCommand,
    session: SessionToken,
    proxy: Option<EventWaker>,
    tx: Sender<Result<ProbeClass, String>>,
) -> std::io::Result<()> {
    crate::spawn_util::spawn_named("odytty-connection-probe", move || {
        let result = run_probe(command);
        let _ = tx.send(result);
        // Wake a redraw so the probe result renders promptly even when
        // the loop was idle waiting for input.
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(UserEvent::Redraw { session });
        }
    })?;
    Ok(())
}

/// Run the probe child to completion (or the hard timeout) and classify it.
fn run_probe(command: SshCommand) -> Result<ProbeClass, String> {
    let (program, args) = command.into_program_args();
    let mut command = super::win_spawn::helper_command(&program)
        .map_err(|err| format!("could not run ssh: {err}"))?;
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // C13: the probe spawns console `ssh.exe`; suppress its console window on
    // the GUI-subsystem binary (no-op on non-Windows).
    super::win_spawn::apply_no_console_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not run ssh: {err}"))?;

    // PLAUS-01: drain stderr concurrently so verbose `ssh -v` diagnostics can
    // never fill the OS pipe and wedge the child until the kill deadline - which
    // would misclassify a reachable host as Unreachable. The reader runs on its
    // own thread into a bounded buffer; the thread creation is fallible (LOW-02),
    // and if it cannot start the probe classifies from the exit status alone
    // (stderr is unavailable, but the probe result is still driven, never hung).
    // The reader hands its text back over a channel so the probe can stop
    // waiting for it after a bounded grace (see `PROBE_STDERR_GRACE`).
    let reader = child.stderr.take().and_then(|pipe| {
        let (tx, rx) = std::sync::mpsc::channel();
        match crate::spawn_util::spawn_named("odytty-probe-stderr", move || {
            let _ = tx.send(drain_reader(pipe));
        }) {
            Ok(_detached) => Some(rx),
            Err(err) => {
                tracing::warn!("probe stderr reader spawn failed: {err}");
                None
            }
        }
    });

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = collect_stderr(reader.as_ref(), PROBE_STDERR_GRACE);
                return Ok(stderr.classify(status.success()));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = collect_stderr(reader.as_ref(), PROBE_STDERR_GRACE);
                    return Ok(ProbeClass::Unreachable);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                // Never leave the child running or unreaped on a failed poll.
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("probe wait failed: {err}"));
            }
        }
    }
}

/// The reader's evidence, or what is known so far (none) when it has not
/// finished within `grace`. A reader that never started yields nothing.
fn collect_stderr(
    reader: Option<&std::sync::mpsc::Receiver<ProbeEvidence>>,
    grace: Duration,
) -> ProbeEvidence {
    reader
        .and_then(|rx| rx.recv_timeout(grace).ok())
        .unwrap_or_default()
}

/// Read the child's stderr to EOF, folding it into [`ProbeEvidence`] one line
/// at a time. Draining never stops early, so a verbose child can never block on
/// a full pipe, and no more than one line (at most
/// [`PROBE_EVIDENCE_LINE_CAP`] bytes, the rest of a longer line discarded) is
/// held at once, so the final method list is seen however much precedes it. The
/// text is parsed, never shown raw, and nothing credential-shaped is read
/// (BatchMode prints no secret).
pub(super) fn drain_reader<R: std::io::Read>(mut reader: R) -> ProbeEvidence {
    let mut evidence = ProbeEvidence::default();
    let mut line: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                for &byte in &chunk[..n] {
                    if byte == b'\n' {
                        evidence.observe_line(&String::from_utf8_lossy(&line));
                        line.clear();
                    } else if line.len() < PROBE_EVIDENCE_LINE_CAP {
                        line.push(byte);
                    }
                }
            }
            Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if !line.is_empty() {
        evidence.observe_line(&String::from_utf8_lossy(&line));
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Stderr shaped like `ssh -v` output: an early offer of interactive
    /// methods, far more diagnostics than any fixed prefix could hold, then the
    /// final publickey-only offer and denial.
    fn superseded_method_list_transcript() -> Vec<u8> {
        let mut text =
            String::from("debug1: Authentications that can continue: publickey,password\n");
        while text.len() < PROBE_EVIDENCE_LINE_CAP * 8 {
            text.push_str("debug1: intermediate diagnostic line with no verdict in it\n");
        }
        text.push_str("debug1: Authentications that can continue: publickey\n");
        text.push_str("user@host: Permission denied (publickey).\n");
        text.into_bytes()
    }

    #[test]
    fn the_final_method_list_wins_however_much_precedes_it() {
        let evidence = drain_reader(Cursor::new(superseded_method_list_transcript()));
        assert_eq!(evidence.classify(false), ProbeClass::AuthRejected);
    }

    #[test]
    fn a_later_interactive_list_still_wins_after_a_long_gap() {
        let mut text = String::from("debug1: Authentications that can continue: publickey\n");
        for _ in 0..400 {
            text.push_str("debug1: intermediate diagnostic line with no verdict in it\n");
        }
        text.push_str("debug1: Authentications that can continue: publickey,password\n");
        let evidence = drain_reader(Cursor::new(text.into_bytes()));
        assert_eq!(evidence.classify(false), ProbeClass::InteractiveAuth);
    }

    #[test]
    fn a_host_key_change_far_past_the_start_still_takes_precedence() {
        let mut text = String::from("debug1: Authentications that can continue: password\n");
        for _ in 0..400 {
            text.push_str("debug1: intermediate diagnostic line with no verdict in it\n");
        }
        text.push_str("WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\n");
        text.push_str("Host key verification failed.\n");
        let evidence = drain_reader(Cursor::new(text.into_bytes()));
        assert_eq!(evidence.classify(false), ProbeClass::HostKeyMismatch);
    }

    #[test]
    fn a_denial_without_a_method_list_past_the_old_cap_is_still_a_rejection() {
        let mut text = String::new();
        for _ in 0..400 {
            text.push_str("debug1: intermediate diagnostic line with no verdict in it\n");
        }
        text.push_str("Permission denied, please try again.");
        let evidence = drain_reader(Cursor::new(text.into_bytes()));
        assert_eq!(
            evidence.classify(false),
            ProbeClass::AuthRejected,
            "an unterminated final line is still read"
        );
    }

    /// Counts every byte handed out, so a test can see the source was drained
    /// to its end, not only that the evidence was bounded.
    struct CountingReader {
        remaining: usize,
        consumed: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl std::io::Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.remaining);
            buf[..n].fill(b'x');
            self.remaining -= n;
            self.consumed.set(self.consumed.get() + n);
            Ok(n)
        }
    }

    #[test]
    fn drain_reader_consumes_the_whole_source_without_holding_it() {
        // One endless line: the pipe is drained to EOF, nothing is retained,
        // and the line buffer never outgrows its cap.
        let total = PROBE_EVIDENCE_LINE_CAP * 64 + 17;
        let consumed = std::rc::Rc::new(std::cell::Cell::new(0));
        let reader = CountingReader {
            remaining: total,
            consumed: std::rc::Rc::clone(&consumed),
        };
        let evidence = drain_reader(reader);
        assert_eq!(evidence, ProbeEvidence::default());
        assert_eq!(consumed.get(), total, "the pipe is drained to EOF");
    }

    #[test]
    fn a_reader_held_open_past_the_child_does_not_hold_the_probe() {
        let (_tx, rx) = std::sync::mpsc::channel::<ProbeEvidence>();
        let start = Instant::now();
        assert_eq!(
            collect_stderr(Some(&rx), Duration::from_millis(20)),
            ProbeEvidence::default()
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the wait is bounded by the grace"
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let mut evidence = ProbeEvidence::default();
        evidence.observe_line("Permission denied");
        tx.send(evidence.clone()).expect("send");
        assert_eq!(
            collect_stderr(Some(&rx), Duration::from_millis(20)),
            evidence
        );
        assert_eq!(
            collect_stderr(None, Duration::from_millis(20)),
            ProbeEvidence::default()
        );
    }

    #[test]
    fn drain_reader_keeps_short_output_classifiable() {
        let evidence = drain_reader(Cursor::new(b"permission denied (publickey)".to_vec()));
        assert_eq!(evidence.classify(false), ProbeClass::AuthRejected);
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Background Test Connection probe worker (REMOTE-UX P4 / ODP-8).
//!
//! The Add / Edit connection form's *Test Connection* action hands a built host
//! here to be probed on a background thread so the UI never blocks. The probe
//! runs the system `ssh` binary - argv-only, never through a shell - as a
//! non-interactive `BatchMode` one-shot that only reports reachability +
//! key/agent auth (it has no password and must never store one). The tri-state
//! result is sent back over a channel and a redraw is woken so the form renders
//! it. A hard timeout guarantees the worker (and the form) never hang.

use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;

use super::super::pty::UserEvent;
use super::super::session::SessionToken;
use crate::ssh_connect::{ProbeClass, SshCommand, classify_probe};

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
    proxy: Option<EventLoopProxy<UserEvent>>,
    tx: Sender<Result<ProbeClass, String>>,
) -> std::io::Result<()> {
    crate::spawn_util::spawn_named("odytty-connection-probe", move || {
        let result = run_probe(command);
        let _ = tx.send(result);
        // Wake a redraw so the tri-state result renders promptly even when
        // the loop was idle waiting for input.
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(UserEvent::Redraw { session });
        }
    })?;
    Ok(())
}

/// Upper bound on retained probe stderr. The classifier only needs the early
/// auth/permission/reachability keywords, so a small cap is plenty; the reader
/// keeps draining past this bound (discarding the overflow) so the pipe never
/// fills and the child never blocks.
const PROBE_STDERR_CAP: usize = 8 * 1024;

/// Run the probe child to completion (or the hard timeout) and classify it.
fn run_probe(command: SshCommand) -> Result<ProbeClass, String> {
    let (program, args) = command.into_program_args();
    let mut command = Command::new(&program);
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
    // (stderr is unavailable, but the tri-state is still driven, never hung).
    // The reader hands its text back over a channel so the probe can stop
    // waiting for it after a bounded grace (see `PROBE_STDERR_GRACE`).
    let reader = child.stderr.take().and_then(|pipe| {
        let (tx, rx) = std::sync::mpsc::channel();
        match crate::spawn_util::spawn_named("odytty-probe-stderr", move || {
            let _ = tx.send(drain_bounded(pipe));
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
                return Ok(classify_probe(status.success(), &stderr));
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

/// The reader's text, or what is known so far (empty) when it has not
/// finished within `grace`. A reader that never started yields nothing.
fn collect_stderr(reader: Option<&std::sync::mpsc::Receiver<String>>, grace: Duration) -> String {
    reader
        .and_then(|rx| rx.recv_timeout(grace).ok())
        .unwrap_or_default()
}

/// Continuously read the child's stderr to EOF, retaining at most
/// [`PROBE_STDERR_CAP`] bytes. Draining never stops early on a full buffer, so a
/// verbose child can never block on a full pipe; only appending stops at the cap.
/// The text is parsed, never shown raw, and nothing credential-shaped is read
/// (BatchMode prints no secret).
fn drain_bounded(pipe: std::process::ChildStderr) -> String {
    drain_reader(pipe, PROBE_STDERR_CAP)
}

/// Generic bounded drain over any reader, so the cap behavior is unit-testable
/// without a live child. Reads to EOF, appending at most `cap` bytes but always
/// consuming the rest (so the source never blocks on backpressure).
fn drain_reader<R: std::io::Read>(mut reader: R, cap: usize) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < cap {
                    let room = cap - buf.len();
                    buf.extend_from_slice(&chunk[..n.min(room)]);
                }
                // Past the cap the bytes are read-and-discarded so the pipe keeps
                // draining and the child never blocks.
            }
            Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn drain_reader_retains_only_the_capped_prefix() {
        // PLAUS-01: a verbose child (far more than the cap) must be fully
        // consumed (no blocking) while only the leading `cap` bytes are kept.
        let input = vec![b'x'; PROBE_STDERR_CAP * 4];
        let drained = drain_reader(Cursor::new(input), PROBE_STDERR_CAP);
        assert_eq!(
            drained.len(),
            PROBE_STDERR_CAP,
            "retained buffer must be clamped to the cap"
        );
    }

    /// Counts every byte handed out, so a test can see the source was drained
    /// to its end, not only that the kept text was capped.
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
    fn drain_reader_consumes_the_whole_source_past_the_cap() {
        let total = PROBE_STDERR_CAP * 4 + 17;
        let consumed = std::rc::Rc::new(std::cell::Cell::new(0));
        let reader = CountingReader {
            remaining: total,
            consumed: std::rc::Rc::clone(&consumed),
        };
        let drained = drain_reader(reader, PROBE_STDERR_CAP);
        assert_eq!(drained.len(), PROBE_STDERR_CAP);
        assert_eq!(consumed.get(), total, "the pipe is drained to EOF");
    }

    #[test]
    fn a_reader_held_open_past_the_child_does_not_hold_the_probe() {
        let (_tx, rx) = std::sync::mpsc::channel::<String>();
        let start = Instant::now();
        assert_eq!(collect_stderr(Some(&rx), Duration::from_millis(20)), "");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the wait is bounded by the grace"
        );
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send("Permission denied".to_owned()).expect("send");
        assert_eq!(
            collect_stderr(Some(&rx), Duration::from_millis(20)),
            "Permission denied"
        );
        assert_eq!(collect_stderr(None, Duration::from_millis(20)), "");
    }

    #[test]
    fn drain_reader_keeps_short_output_intact() {
        let drained = drain_reader(Cursor::new(b"permission denied (publickey)".to_vec()), 4096);
        assert_eq!(drained, "permission denied (publickey)");
    }
}

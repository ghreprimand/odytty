// SPDX-License-Identifier: GPL-3.0-only
//! Background Test Connection probe worker (REMOTE-UX P4 / ODP-8).
//!
//! The Add / Edit connection form's *Test Connection* action hands a built host
//! here to be probed on a background thread so the UI never blocks. The probe
//! runs the system `ssh` binary — argv-only, never through a shell — as a
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

/// Hard cap on a probe: `ConnectTimeout` (5s) plus 1s slack, so a wedged `ssh`
/// can never keep the form spinning.
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

/// Spawn the probe on a detached worker thread. Fire-and-forget: the worker owns
/// the command, sender, and proxy; the caller returns immediately with the
/// receiver end already stored.
pub(super) fn spawn_connection_probe(
    command: SshCommand,
    session: SessionToken,
    proxy: Option<EventLoopProxy<UserEvent>>,
    tx: Sender<Result<ProbeClass, String>>,
) {
    let _ = std::thread::Builder::new()
        .name("odytty-connection-probe".to_owned())
        .spawn(move || {
            let result = run_probe(command);
            let _ = tx.send(result);
            // Wake a redraw so the tri-state result renders promptly even when
            // the loop was idle waiting for input.
            if let Some(proxy) = proxy {
                let _ = proxy.send_event(UserEvent::Redraw { session });
            }
        });
}

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

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr = read_stderr(&mut child);
                return Ok(classify_probe(status.success(), &stderr));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(ProbeClass::Unreachable);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(format!("probe wait failed: {err}")),
        }
    }
}

/// Drain the child's captured stderr after it has exited. `ssh -v` diagnostics
/// are small, so a fully-buffered read is safe; the text is parsed, never shown
/// raw, and nothing credential-shaped is read (BatchMode prints no secret).
fn read_stderr(child: &mut std::process::Child) -> String {
    use std::io::Read;
    let mut buf = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut buf);
    }
    buf
}

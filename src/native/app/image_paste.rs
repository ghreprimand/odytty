// SPDX-License-Identifier: GPL-3.0-only
//! Image paste-through upload worker (F6-i7).
//!
//! When a clipboard image is pasted into a remote *integrated* ssh tab and the
//! confirm prompt is accepted, the held PNG is handed here to be uploaded on a
//! background thread so the UI never blocks on the transfer. The upload runs the
//! system `ssh` binary — the same credential-delegating transport as the connect
//! path, never an embedded ssh — streaming the bytes into a remote `cat` that
//! creates the file `0600` under an unguessable `/tmp` name. On success the
//! remote path is NOT typed into the shell (a bare path on an empty prompt
//! would run on the next Enter and error); instead the completion is marshalled
//! to the main thread, which posts a one-line in-pane notice and copies the
//! path to the local clipboard so it can be pasted as an argument. On failure a
//! one-line notice is written into the pane. Either way a redraw is woken so
//! the result renders.

use std::process::{Command, Stdio};

use super::super::pty::UserEvent;
use super::super::session::RemoteUploadJob;
use crate::native::lock_recover;

/// Hand a confirmed image paste to a background upload worker. Fire-and-forget:
/// the worker owns every handle in `job`, so the caller returns immediately.
pub(super) fn spawn_upload_worker(job: RemoteUploadJob, png: Vec<u8>) {
    let _ = std::thread::Builder::new()
        .name("odytty-image-upload".to_owned())
        .spawn(move || run_upload(job, png));
}

fn run_upload(job: RemoteUploadJob, png: Vec<u8>) {
    let remote_path = crate::ssh_connect::remote_upload_target();
    match perform_upload(&job, &png, &remote_path) {
        Ok(()) => {
            // Register the path for best-effort cleanup on tab close, then hand
            // the completion to the main thread: it posts an in-pane notice and
            // copies the path to the local clipboard. Nothing is typed into the
            // shell. Clipboard I/O is main-thread/UI-bound on some platforms, so
            // it must not run on this worker.
            lock_recover(&job.uploaded).push(remote_path.clone());
            if let Some(proxy) = job.proxy.as_ref() {
                let _ = proxy.send_event(UserEvent::ImageUploaded {
                    session: job.session,
                    remote_path,
                });
            }
        }
        Err(reason) => {
            // The failure notice only writes the terminal model, so it stays on
            // the worker; a redraw is woken below.
            let banner = format!("\r\n\x1b[1;31m image upload failed \x1b[0m {reason}\r\n");
            lock_recover(&job.terminal).advance(banner.as_bytes());
            if let Some(proxy) = job.proxy.as_ref() {
                let _ = proxy.send_event(UserEvent::Redraw {
                    session: job.session,
                });
            }
        }
    }
}

/// Write the PNG to a local temp file, stream it into the remote `cat` over
/// `ssh`, and clean up the local temp. Returns a short reason on any failure so
/// the caller can surface it; never leaves a partial paste.
fn perform_upload(job: &RemoteUploadJob, png: &[u8], remote_path: &str) -> Result<(), String> {
    // Reuse the remote file's basename for the local temp, so both carry the
    // same unguessable name; the local file lives only for the transfer.
    let file_name = std::path::Path::new(remote_path)
        .file_name()
        .ok_or_else(|| "bad remote path".to_owned())?;
    let local = std::env::temp_dir().join(file_name);
    std::fs::write(&local, png).map_err(|err| format!("temp write: {err}"))?;

    let result = stream_upload(job, &local, remote_path);
    // Best-effort local cleanup regardless of upload outcome.
    let _ = std::fs::remove_file(&local);
    result
}

fn stream_upload(
    job: &RemoteUploadJob,
    local: &std::path::Path,
    remote_path: &str,
) -> Result<(), String> {
    let command = crate::ssh_connect::remote_upload_command(
        &job.destination,
        job.port,
        job.control_dir.as_deref(),
        remote_path,
    );
    let (program, args) = command.into_program_args();
    let stdin = std::fs::File::open(local).map_err(|err| format!("temp open: {err}"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // C13: the upload streams over console `ssh.exe` for seconds; suppress its
    // console window on the GUI-subsystem binary (no-op on non-Windows).
    super::win_spawn::apply_no_console_window(&mut command);
    let status = command
        .status()
        .map_err(|err| format!("ssh spawn: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(match status.code() {
            Some(code) => format!("ssh exited {code}"),
            None => "ssh terminated by signal".to_owned(),
        })
    }
}

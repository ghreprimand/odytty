// SPDX-License-Identifier: GPL-3.0-only
//! Image paste-through upload worker (F6-i7).
//!
//! When a clipboard image is pasted into a remote *integrated* ssh tab and the
//! confirm prompt is accepted, the held PNG is handed here to be uploaded on a
//! background thread so the UI never blocks on the transfer. The upload runs the
//! system `ssh` binary — the same credential-delegating transport as the connect
//! path, never an embedded ssh — streaming the bytes into a remote `cat` that
//! creates the file `0600` under an unguessable `/tmp` name. On success the
//! remote path is pasted into the shell (a text paste of the path, nothing is
//! executed); on failure a one-line notice is written into the pane. Either way
//! a redraw is woken so the result renders.

use std::process::{Command, Stdio};

use super::super::pty::UserEvent;
use super::super::session::RemoteUploadJob;
use crate::native::clipboard::write_paste_text;
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
            // Record the path for best-effort cleanup on tab close, then paste it
            // into the shell exactly like a text paste (no execution).
            lock_recover(&job.uploaded).push(remote_path.clone());
            let _ = write_paste_text(&job.terminal, &job.writer, &remote_path);
        }
        Err(reason) => {
            let banner = format!("\r\n\x1b[1;31m image upload failed \x1b[0m {reason}\r\n");
            lock_recover(&job.terminal).advance(banner.as_bytes());
        }
    }
    // Wake a redraw so the injected path / failure notice renders promptly.
    if let Some(proxy) = job.proxy.as_ref() {
        let _ = proxy.send_event(UserEvent::Redraw {
            session: job.session,
        });
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
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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

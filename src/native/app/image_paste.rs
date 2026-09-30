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
//!
//! The lifecycle is bounded (see `session::upload_lifecycle`): at most
//! `MAX_CONCURRENT_UPLOADS` uploads run at once, the `ssh` child is killed
//! and reaped at [`UPLOAD_DEADLINE`] or as soon as the tab closes, and a
//! worker that finishes after its tab closed removes its own remote file.

use std::process::{Command, Stdio};

use super::super::pty::UserEvent;
use super::super::session::RemoteUploadJob;
use super::super::session::upload_lifecycle::{
    BoundedExit, UPLOAD_DEADLINE, UPLOAD_SLOTS, UploadSettlement, run_bounded, settle_upload,
    spawn_remote_cleanup,
};
use crate::native::lock_recover;

/// Why a confirmed paste did not start a worker.
pub(super) enum UploadStartError {
    /// `MAX_CONCURRENT_UPLOADS` uploads are already running.
    Busy,
    /// The worker thread could not be created.
    Spawn(std::io::Error),
}

/// Hand a confirmed image paste to a background upload worker. Fire-and-forget:
/// the worker owns every handle in `job`, so the caller returns immediately.
///
/// Returns why no worker started: every upload slot is busy, or under thread
/// exhaustion the worker cannot be created. The confirmed paste would
/// otherwise be lost with no sign, so the caller surfaces a visible notice in
/// either case (LOW-02).
pub(super) fn spawn_upload_worker(
    job: RemoteUploadJob,
    png: Vec<u8>,
) -> Result<(), UploadStartError> {
    let slot = UPLOAD_SLOTS.try_acquire().ok_or(UploadStartError::Busy)?;
    crate::spawn_util::spawn_named("odytty-image-upload", move || {
        let _slot = slot;
        run_upload(job, png);
    })
    .map_err(UploadStartError::Spawn)?;
    Ok(())
}

fn run_upload(job: RemoteUploadJob, png: Vec<u8>) {
    if lock_recover(&job.uploaded).is_closed() {
        // The tab closed before the worker started; nothing was sent.
        return;
    }
    let remote_path = match crate::ssh_connect::remote_upload_target() {
        Ok(path) => path,
        Err(error) => {
            report_upload_failure(&job, format!("secure random name: {error}"));
            return;
        }
    };
    let outcome = perform_upload(&job, &png, &remote_path);
    let settlement = settle_upload(&job.uploaded, remote_path.clone(), outcome, |paths| {
        spawn_remote_cleanup(
            &job.destination,
            job.port,
            job.control_dir.as_deref(),
            &paths,
        );
    });
    match settlement {
        UploadSettlement::Delivered => {
            // The path is recorded for cleanup on tab close. Hand the completion
            // to the main thread: it posts an in-pane notice and copies the path
            // to the local clipboard. Nothing is typed into the shell.
            // Clipboard I/O is main-thread/UI-bound on some platforms, so it
            // must not run on this worker.
            if let Some(proxy) = job.proxy.as_ref() {
                let _ = proxy.send_event(UserEvent::ImageUploaded {
                    session: job.session,
                    remote_path,
                });
            }
        }
        UploadSettlement::Failed(reason) => report_upload_failure(&job, reason),
        UploadSettlement::TabClosed => {}
    }
}

fn report_upload_failure(job: &RemoteUploadJob, reason: String) {
    // The failure notice only writes the terminal model, so it stays on this
    // worker; entropy failure is visible and no remote upload is attempted.
    let banner = format!("\r\n\x1b[1;31m image upload failed \x1b[0m {reason}\r\n");
    lock_recover(&job.terminal).advance(banner.as_bytes());
    if let Some(proxy) = job.proxy.as_ref() {
        let _ = proxy.send_event(UserEvent::Redraw {
            session: job.session,
        });
    }
}

/// Write the PNG to a local temp file, stream it into the remote `cat` over
/// `ssh`, and clean up the local temp. Returns a short reason on any failure so
/// the caller can surface it; never leaves a partial paste.
///
/// The local temp is created privately: on Unix with `O_CREAT|O_EXCL` and mode
/// `0600`, so a pre-planted symlink or a world-readable file in the shared temp
/// dir cannot be reused for the briefly-staged paste; on Windows the per-user
/// temp directory is already ACL'd to the current user, so a plain create
/// matches that guarantee.
fn perform_upload(job: &RemoteUploadJob, png: &[u8], remote_path: &str) -> Result<(), String> {
    // Reuse the remote file's basename for the local temp, so both carry the
    // same unguessable name; the local file lives only for the transfer.
    let file_name = std::path::Path::new(remote_path)
        .file_name()
        .ok_or_else(|| "bad remote path".to_owned())?;
    let local = std::env::temp_dir().join(file_name);
    write_private_temp(&local, png).map_err(|err| format!("temp write: {err}"))?;

    let result = stream_upload(job, &local, remote_path);
    // Best-effort local cleanup regardless of upload outcome.
    let _ = std::fs::remove_file(&local);
    result
}

/// Create `path` privately and write `bytes`.
///
/// Unix: `O_CREAT|O_EXCL` with mode `0600` so the pasted image cannot land in a
/// world-readable file and a pre-planted symlink at the target path cannot be
/// followed (the exclusive create fails instead). Non-Unix: the per-user temp
/// directory is ACL'd to the current user, so a plain write suffices.
#[cfg(unix)]
fn write_private_temp(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private_temp(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
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
    let child = command.spawn().map_err(|err| format!("ssh spawn: {err}"))?;
    // Bounded wait: killed and reaped at the deadline or when the tab closes.
    match run_bounded(child, UPLOAD_DEADLINE, || {
        lock_recover(&job.uploaded).is_closed()
    }) {
        BoundedExit::Exited(status) if status.success() => Ok(()),
        BoundedExit::Exited(status) => Err(match status.code() {
            Some(code) => format!("ssh exited {code}"),
            None => "ssh terminated by signal".to_owned(),
        }),
        BoundedExit::TimedOut => Err(format!(
            "no response within {} s; the transfer was stopped",
            UPLOAD_DEADLINE.as_secs()
        )),
        BoundedExit::Cancelled => Err("the tab closed during the upload".to_owned()),
        BoundedExit::WaitFailed(error) => Err(format!("ssh wait failed: {error}")),
    }
}

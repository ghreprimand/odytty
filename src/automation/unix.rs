// SPDX-License-Identifier: GPL-3.0-only
//! Explicit owner-private Unix endpoints. One bounded request per connection.
//! Linux and macOS verify peer credentials independently of socket file mode.

use std::fs::{self, Metadata, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::dispatch::Submission;
use super::protocol::{self, ErrorCode, Reply, Request, Response};

mod connection;
use connection::{DeadlineStream, connect, peer_is_owner};

const MAX_CLIENTS: usize = 8;
const MAX_CONNECTIONS_PER_SECOND: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

type AcceptObserver = Arc<dyn Fn(&UnixStream) + Send + Sync>;

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "automation endpoint is not owner-private",
    )
}

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Resolve the directory once, then check every ancestor. A root-owned sticky
/// temporary directory is allowed; an untrusted writable ancestor is not.
fn endpoint_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "absolute endpoint path required",
        ));
    }
    let parent = fs::canonicalize(path.parent().ok_or_else(denied)?)?;
    let uid = unsafe { libc::geteuid() };
    for (index, ancestor) in parent.ancestors().enumerate() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || (metadata.uid() != uid && metadata.uid() != 0) {
            return Err(denied());
        }
        if index == 0 {
            if metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
                return Err(denied());
            }
        } else if metadata.mode() & 0o022 != 0
            && !(metadata.uid() == 0 && metadata.mode() & 0o1000 != 0)
        {
            return Err(denied());
        }
    }
    Ok(parent.join(path.file_name().ok_or_else(denied)?))
}

struct EndpointGuard {
    path: PathBuf,
    socket: Metadata,
    parent: Metadata,
}

impl Drop for EndpointGuard {
    fn drop(&mut self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        if fs::symlink_metadata(parent).is_ok_and(|m| same_file(&m, &self.parent))
            && fs::symlink_metadata(&self.path)
                .is_ok_and(|m| m.file_type().is_socket() && same_file(&m, &self.socket))
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Holds a bounded listener thread. Drop stops acceptance, cancels waiting
/// requests and joins at most eight workers, each under an absolute I/O deadline.
/// It never removes an existing endpoint to make startup succeed.
pub struct Server {
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Invoke off the first-terminal critical path, after explicit opt-in.
    /// `wake` must enqueue an event for the existing owner and return false when
    /// that owner is gone. It must not execute terminal actions on this thread.
    pub fn bind(
        path: &Path,
        submission: Submission,
        wake: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::bind_inner(path, submission, Arc::new(wake), None)
    }

    #[cfg(all(test, target_os = "linux"))]
    fn bind_with_accept_observer(
        path: &Path,
        submission: Submission,
        wake: impl Fn() -> bool + Send + Sync + 'static,
        observer: impl Fn(&UnixStream) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::bind_inner(path, submission, Arc::new(wake), Some(Arc::new(observer)))
    }

    fn bind_inner(
        path: &Path,
        submission: Submission,
        wake: Arc<dyn Fn() -> bool + Send + Sync>,
        accept_observer: Option<AcceptObserver>,
    ) -> io::Result<Self> {
        let path = endpoint_path(path)?;
        let parent = fs::symlink_metadata(path.parent().ok_or_else(denied)?)?;
        let listener = UnixListener::bind(&path)?;
        let guard = EndpointGuard {
            socket: fs::symlink_metadata(&path)?,
            path,
            parent,
        };
        fs::set_permissions(&guard.path, Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let thread = thread::Builder::new()
            .name("odytty-control".into())
            .spawn(move || {
                let _guard = guard;
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                let mut rate_started = Instant::now();
                let mut accepted = 0;
                while !stop.load(Ordering::Acquire) {
                    if rate_started.elapsed() >= Duration::from_secs(1) {
                        rate_started = Instant::now();
                        accepted = 0;
                    }
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].is_finished() {
                            let _ = workers.swap_remove(index).join();
                        } else {
                            index += 1;
                        }
                    }
                    // Saturation stops acceptance; it cannot grow userspace queues
                    // or spawn unbounded threads. Clients have a connect deadline.
                    if workers.len() < MAX_CLIENTS && accepted < MAX_CONNECTIONS_PER_SECOND {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if let Some(observer) = accept_observer.as_ref() {
                                    observer(&stream);
                                }
                                accepted += 1;
                                let submission = submission.clone();
                                let wake = wake.clone();
                                let stop = stop.clone();
                                if let Ok(worker) = thread::Builder::new()
                                    .name("odytty-control-client".into())
                                    .spawn(move || {
                                        let _ = serve(stream, submission, wake, stop);
                                    })
                                {
                                    workers.push(worker);
                                }
                                continue;
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                            Err(_) => break,
                        }
                    }
                    thread::sleep(POLL_INTERVAL);
                }
                stop.store(true, Ordering::Release);
                drop(listener);
                for worker in workers {
                    let _ = worker.join();
                }
            })?;
        Ok(Self {
            stopped,
            thread: Some(thread),
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    stream: UnixStream,
    submission: Submission,
    wake: Arc<dyn Fn() -> bool + Send + Sync>,
    stopped: Arc<AtomicBool>,
) -> io::Result<()> {
    peer_is_owner(&stream)?;
    let mut io = DeadlineStream::new(stream, IO_TIMEOUT);
    let request = match protocol::read_request(&mut io) {
        Ok(request) => request,
        Err(error) => {
            // A rejected frame has no trusted correlation ID. Zero identifies
            // a pre-dispatch protocol rejection; it never reports an outcome.
            if let Some(code) = error
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<ErrorCode>())
            {
                io.reset_deadline(IO_TIMEOUT);
                let _ = protocol::write_response(
                    &mut io,
                    &Response {
                        request_id: 0,
                        reply: Reply::Error(*code),
                    },
                );
            }
            return Err(error);
        }
    };
    let request_id = request.request_id;
    if stopped.load(Ordering::Acquire) || io.peer_finished()? {
        return Ok(());
    }
    let response = match submission.submit(request) {
        Ok(receipt) => {
            if stopped.load(Ordering::Acquire) || io.peer_finished()? {
                return Ok(());
            }
            if !wake() {
                return Ok(());
            } // Receipt drop cancels pending work.
            loop {
                if stopped.load(Ordering::Acquire) || io.peer_finished()? {
                    return Ok(());
                }
                if let Some(response) = receipt.try_response() {
                    break response;
                }
                thread::sleep(POLL_INTERVAL);
            }
        }
        Err(error) => Response {
            request_id,
            reply: Reply::Error(error),
        },
    };
    io.reset_deadline(IO_TIMEOUT);
    protocol::write_response(&mut io, &response)
}

/// Exchange exactly once. A partial write or lost reply can have an unknown
/// mutation outcome; callers must not retry. No path discovery or shell use.
pub fn request(path: &Path, request: &Request) -> io::Result<Response> {
    let path = endpoint_path(path)?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(denied());
    }
    let stream = connect(&path, IO_TIMEOUT)?;
    peer_is_owner(&stream)?;
    let mut io = DeadlineStream::new(stream, IO_TIMEOUT);
    protocol::write_request(&mut io, request)?;
    io.reset_deadline(IO_TIMEOUT + super::dispatch::REQUEST_TIMEOUT);
    let response = protocol::read_response(&mut io)?;
    let rejected_frame = response.request_id == 0
        && matches!(
            response.reply,
            Reply::Error(
                ErrorCode::InvalidRequest
                    | ErrorCode::VersionMismatch
                    | ErrorCode::UnsupportedCapability
                    | ErrorCode::TooLarge
            )
        );
    if response.request_id != request.request_id && !rejected_frame {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            ErrorCode::InvalidRequest,
        ));
    }
    Ok(response)
}

#[cfg(test)]
mod tests;

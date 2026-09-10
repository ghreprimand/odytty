// SPDX-License-Identifier: GPL-3.0-only
//! Owner-private Unix endpoints. One bounded request per connection; only the
//! quick-terminal CLI asks for bounded, fail-closed endpoint discovery.
//! Linux and macOS verify peer credentials independently of socket file mode.

use std::fs::{self, Metadata, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
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
pub(crate) const MAX_DISCOVERY_CANDIDATES: usize = 32;

#[derive(Debug)]
pub(crate) struct DiscoveryScanError {
    kind: io::ErrorKind,
    unresolved: Vec<PathBuf>,
}

impl DiscoveryScanError {
    fn new(kind: io::ErrorKind, unresolved: Vec<PathBuf>) -> Self {
        Self { kind, unresolved }
    }

    pub(crate) fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    pub(crate) fn unresolved_paths(&self) -> &[PathBuf] {
        &self.unresolved
    }
}

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
    fault: Arc<Mutex<Option<String>>>,
    #[cfg_attr(not(test), allow(dead_code))]
    wake: Arc<dyn Fn() -> bool + Send + Sync>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Reports the reason the listener thread stopped on its own, if it did.
    /// `None` means the listener is still accepting or was stopped by drop.
    pub fn fault(&self) -> Option<String> {
        self.fault.lock().map_or_else(
            |_| Some("listener state poisoned".to_owned()),
            |fault| fault.clone(),
        )
    }

    /// Record a terminal fault through the shared cell exactly as the listener
    /// thread does, including the owner wake, without an OS-level failure.
    #[cfg(test)]
    pub fn inject_fault_for_test(&self, reason: &str) {
        record_fault(&self.fault, &*self.wake, reason.to_owned());
    }

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
        let fault = Arc::new(Mutex::new(None));
        let fault_slot = fault.clone();
        let wake_handle = wake.clone();
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
                                match thread::Builder::new()
                                    .name("odytty-control-client".into())
                                    .spawn(move || {
                                        let _ = serve(stream, submission, wake, stop);
                                    }) {
                                    Ok(worker) => workers.push(worker),
                                    // The accepted stream drops here, so the
                                    // client sees a close instead of a hang.
                                    Err(error) => tracing::warn!(
                                        %error,
                                        "automation client worker spawn failed"
                                    ),
                                }
                                continue;
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                            Err(error) => {
                                record_fault(
                                    &fault_slot,
                                    &*wake,
                                    format!("accept failed: {error}"),
                                );
                                break;
                            }
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
            fault,
            wake: wake_handle,
            thread: Some(thread),
        })
    }
}

/// Records the first terminal listener failure; later ones keep the original.
/// The owner is woken so the runtime observes the fault on its next turn even
/// when the event loop is otherwise idle.
fn record_fault(slot: &Mutex<Option<String>>, wake: &dyn Fn() -> bool, reason: String) {
    if let Ok(mut fault) = slot.lock()
        && fault.is_none()
    {
        *fault = Some(reason);
    }
    let _ = wake();
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
    let mut io = DeadlineStream::new(stream, IO_TIMEOUT)?;
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
/// mutation outcome; callers must not retry. No implicit path discovery or
/// shell use occurs in this explicit request function.
pub fn request(path: &Path, request: &Request) -> io::Result<Response> {
    request_with_deadlines(
        path,
        request,
        IO_TIMEOUT,
        IO_TIMEOUT + super::dispatch::REQUEST_TIMEOUT,
    )
}

/// Exchange exactly once under a caller-supplied absolute operation budget.
/// Discovery uses this to keep all read-only probes inside its one-second
/// budget. Mutations continue to use [`request`] and are never retried.
pub(crate) fn request_with_timeout(
    path: &Path,
    request: &Request,
    timeout: Duration,
) -> io::Result<Response> {
    let deadline = Instant::now() + timeout;
    let path = validated_endpoint(path)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "discovery probe timed out",
        ));
    }
    let stream = connect(&path, remaining.min(IO_TIMEOUT))?;
    peer_is_owner(&stream)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "discovery probe timed out",
        ));
    }
    let mut io = DeadlineStream::new(stream, remaining)?;
    protocol::write_request(&mut io, request)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "discovery probe timed out",
        ));
    }
    io.reset_deadline(remaining);
    validate_response(protocol::read_response(&mut io)?, request)
}

fn request_with_deadlines(
    path: &Path,
    request: &Request,
    io_timeout: Duration,
    response_timeout: Duration,
) -> io::Result<Response> {
    let path = validated_endpoint(path)?;
    let stream = connect(&path, io_timeout)?;
    peer_is_owner(&stream)?;
    let mut io = DeadlineStream::new(stream, io_timeout)?;
    protocol::write_request(&mut io, request)?;
    io.reset_deadline(response_timeout);
    validate_response(protocol::read_response(&mut io)?, request)
}

fn validated_endpoint(path: &Path) -> io::Result<PathBuf> {
    let path = endpoint_path(path)?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(denied());
    }
    Ok(path)
}

fn validate_response(response: Response, request: &Request) -> io::Result<Response> {
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

/// Return only PID-shaped endpoint entries below the validated owner-private
/// runtime directory. Endpoint type, owner, and mode are rechecked by each
/// probe so a replacement between enumeration and connect is refused.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn discovery_candidates(
    runtime_base: Option<&std::ffi::OsStr>,
    deadline: Instant,
) -> Result<Vec<PathBuf>, DiscoveryScanError> {
    let base = runtime_base
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DiscoveryScanError::new(io::ErrorKind::NotFound, Vec::new()))?;
    discovery_candidates_in(&Path::new(base).join("odytty"), deadline)
}

/// Enumerate candidate sockets below the platform's established owner-private
/// endpoint directory without creating it or any other filesystem state.
pub(crate) fn discovery_candidates_in(
    directory: &Path,
    deadline: Instant,
) -> Result<Vec<PathBuf>, DiscoveryScanError> {
    match fs::symlink_metadata(directory) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(DiscoveryScanError::new(
                io::ErrorKind::NotFound,
                vec![directory.to_path_buf()],
            ));
        }
        Err(_) => return Err(discovery_uncertain(directory)),
    }
    let validated = endpoint_path(&directory.join("control-1.sock"))
        .map_err(|_| discovery_uncertain(directory))?;
    let directory = validated
        .parent()
        .ok_or_else(|| discovery_uncertain(directory))?;
    let mut candidates = Vec::new();
    let entries = fs::read_dir(directory).map_err(|_| discovery_uncertain(directory))?;
    for entry in entries {
        if Instant::now() >= deadline {
            return Err(DiscoveryScanError::new(
                io::ErrorKind::TimedOut,
                vec![directory.to_path_buf()],
            ));
        }
        let entry = entry.map_err(|_| discovery_uncertain(directory))?;
        if !is_control_endpoint_name(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|_| discovery_uncertain(&path))?;
        if !metadata.file_type().is_socket() {
            continue;
        }
        candidates.push(path);
        if candidates.len() > MAX_DISCOVERY_CANDIDATES {
            return Err(DiscoveryScanError::new(
                io::ErrorKind::InvalidData,
                vec![directory.to_path_buf()],
            ));
        }
    }
    if Instant::now() >= deadline {
        return Err(DiscoveryScanError::new(
            io::ErrorKind::TimedOut,
            vec![directory.to_path_buf()],
        ));
    }
    candidates.sort();
    Ok(candidates)
}

fn discovery_uncertain(path: &Path) -> DiscoveryScanError {
    DiscoveryScanError::new(io::ErrorKind::Other, vec![path.to_path_buf()])
}

fn is_control_endpoint_name(name: &std::ffi::OsStr) -> bool {
    let bytes = name.as_bytes();
    let Some(pid) = bytes
        .strip_prefix(b"control-")
        .and_then(|value| value.strip_suffix(b".sock"))
    else {
        return false;
    };
    !pid.is_empty()
        && pid.iter().all(u8::is_ascii_digit)
        && std::str::from_utf8(pid)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .is_some_and(|pid| pid != 0)
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: GPL-3.0-only
//! Owner-private Windows named-pipe transport. One bounded request per client.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_SEM_TIMEOUT, GENERIC_READ, GENERIC_WRITE, HANDLE,
    HLOCAL, LocalFree, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    CopySid, EqualSid, GetLengthSid, GetTokenInformation, IsValidSecurityDescriptor, IsValidSid,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{
    CancelIoEx, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    NAMED_PIPE_MODE, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PeekNamedPipe,
    WaitNamedPipeW,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::{PCWSTR, PWSTR};

use super::dispatch::Submission;
use super::protocol::{self, ErrorCode, Reply, Request, Response};

const PIPE_PREFIX: &[u16] = &[
    b'\\' as u16,
    b'\\' as u16,
    b'.' as u16,
    b'\\' as u16,
    b'p' as u16,
    b'i' as u16,
    b'p' as u16,
    b'e' as u16,
    b'\\' as u16,
    b'o' as u16,
    b'd' as u16,
    b'y' as u16,
    b't' as u16,
    b't' as u16,
    b'y' as u16,
    b'-' as u16,
    b'c' as u16,
    b'o' as u16,
    b'n' as u16,
    b't' as u16,
    b'r' as u16,
    b'o' as u16,
    b'l' as u16,
    b'-' as u16,
];
const MAX_CLIENTS: usize = 8;
const MAX_PIPE_INSTANCES: u32 = MAX_CLIENTS as u32 + 1;
const MAX_CONNECTIONS_PER_SECOND: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "automation named pipe is not owned by this user",
    )
}

fn windows_error(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code & 0xffff_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(error.to_string())
    }
}

fn error_code(error: &windows::core::Error) -> Option<u32> {
    let code = error.code().0 as u32;
    (code & 0xffff_0000 == 0x8007_0000).then_some(code & 0xffff)
}

fn is_error(error: &windows::core::Error, expected: u32) -> bool {
    error_code(error) == Some(expected)
}

struct OwnedHandle(HANDLE);

// SAFETY: Win32 kernel handles are process-wide values and may be used and
// closed from a different thread. This wrapper has one owning close path.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn owned_handle(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_invalid() {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: the successful Win32 call returned a new owning handle.
        Ok(OwnedHandle(handle))
    }
}

fn raw_handle(handle: &OwnedHandle) -> HANDLE {
    handle.0
}

fn wide_endpoint(path: &Path) -> io::Result<Vec<u16>> {
    // OsStr's encoded bytes preserve ASCII on Windows. The accepted endpoint
    // grammar is entirely ASCII, so no lossy Unicode conversion is needed.
    let bytes = path.as_os_str().as_encoded_bytes();
    let wide: Vec<u16> = bytes.iter().copied().map(u16::from).collect();
    let suffix = wide.strip_prefix(PIPE_PREFIX).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            r"endpoint must match \\.\pipe\odytty-control-<pid>",
        )
    })?;
    let pid = suffix.iter().try_fold(0u32, |pid, unit| {
        let digit = u32::from(unit.checked_sub(b'0' as u16)?);
        (digit <= 9).then_some(())?;
        pid.checked_mul(10)?.checked_add(digit)
    });
    if pid.is_none_or(|pid| pid == 0) || wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid automation named-pipe endpoint",
        ));
    }
    let mut terminated = wide;
    terminated.push(0);
    Ok(terminated)
}

pub fn endpoint(pid: u32) -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\odytty-control-{pid}"))
}

struct OwnedSid {
    words: Vec<usize>,
}

impl OwnedSid {
    /// Read-only view for comparison and string conversion. The pointer is
    /// derived from a shared borrow, so callers must not write through it.
    fn as_psid(&self) -> PSID {
        PSID(self.words.as_ptr().cast_mut().cast())
    }

    /// Writable destination for `CopySid`; derived from a mutable borrow.
    fn as_psid_mut(&mut self) -> PSID {
        PSID(self.words.as_mut_ptr().cast())
    }
}

fn process_user_sid(pid: Option<u32>) -> io::Result<OwnedSid> {
    let process = match pid {
        Some(pid) => Some(owned_handle(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                .map_err(windows_error)?,
        )?),
        None => None,
    };
    let process_handle = process
        .as_ref()
        .map_or_else(|| unsafe { GetCurrentProcess() }, raw_handle);
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process_handle, TOKEN_QUERY, &raw mut token) }
        .map_err(windows_error)?;
    let token = owned_handle(token)?;

    let mut needed = 0u32;
    let first =
        unsafe { GetTokenInformation(raw_handle(&token), TokenUser, None, 0, &raw mut needed) };
    if needed == 0 {
        return Err(first.err().map_or_else(denied, windows_error));
    }
    let words = usize::try_from(needed)
        .ok()
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<usize>() - 1))
        .map(|bytes| bytes / std::mem::size_of::<usize>())
        .ok_or_else(denied)?;
    let mut token_user = vec![0usize; words];
    unsafe {
        GetTokenInformation(
            raw_handle(&token),
            TokenUser,
            Some(token_user.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    }
    .map_err(windows_error)?;
    // SAFETY: GetTokenInformation initialized a suitably aligned TOKEN_USER.
    let source = unsafe { &*token_user.as_ptr().cast::<TOKEN_USER>() }
        .User
        .Sid;
    if !unsafe { IsValidSid(source) }.as_bool() {
        return Err(denied());
    }
    let length = unsafe { GetLengthSid(source) };
    if length == 0 {
        return Err(denied());
    }
    let sid_words = usize::try_from(length)
        .ok()
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<usize>() - 1))
        .map(|bytes| bytes / std::mem::size_of::<usize>())
        .ok_or_else(denied)?;
    let mut sid = OwnedSid {
        words: vec![0usize; sid_words],
    };
    unsafe { CopySid(length, sid.as_psid_mut(), source) }.map_err(windows_error)?;
    if !unsafe { IsValidSid(sid.as_psid()) }.as_bool() {
        return Err(denied());
    }
    Ok(sid)
}

fn verify_process_owner(pid: u32, expected: &OwnedSid) -> io::Result<()> {
    let actual = process_user_sid(Some(pid))?;
    unsafe { EqualSid(actual.as_psid(), expected.as_psid()) }.map_err(|_| denied())
}

struct LocalDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

fn owner_descriptor(owner: &OwnedSid) -> io::Result<LocalDescriptor> {
    let mut sid_text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(owner.as_psid(), &raw mut sid_text) }.map_err(windows_error)?;
    if sid_text.is_null() {
        return Err(denied());
    }
    let sid = unsafe { sid_text.to_string() };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(sid_text.0.cast())));
    }
    let sid = sid.map_err(|_| denied())?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})\0").encode_utf16().collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &raw mut descriptor,
            None,
        )
    }
    .map_err(windows_error)?;
    if descriptor.is_invalid() {
        return Err(denied());
    }
    let descriptor = LocalDescriptor(descriptor);
    if !unsafe { IsValidSecurityDescriptor(descriptor.0) }.as_bool() {
        return Err(denied());
    }
    Ok(descriptor)
}

fn pipe_mode() -> NAMED_PIPE_MODE {
    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS
}

fn create_pipe(name: &[u16], owner: &OwnedSid, first: bool) -> io::Result<OwnedHandle> {
    let descriptor = owner_descriptor(owner)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let mut flags = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        flags |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            flags,
            pipe_mode(),
            MAX_PIPE_INSTANCES,
            (protocol::MAX_MESSAGE_BYTES + 4) as u32,
            (protocol::MAX_MESSAGE_BYTES + 4) as u32,
            IO_TIMEOUT.as_millis() as u32,
            Some(&raw const attributes),
        )
    };
    owned_handle(handle)
}

fn remaining_millis(deadline: Instant) -> io::Result<u32> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(io::ErrorKind::TimedOut, ErrorCode::TimedOut));
    }
    Ok(remaining
        .as_millis()
        .saturating_add(1)
        .clamp(1, u128::from(u32::MAX - 1)) as u32)
}

fn cancel_and_drain(handle: HANDLE, overlapped: &OVERLAPPED) {
    let _ = unsafe { CancelIoEx(handle, Some(overlapped)) };
    let mut transferred = 0;
    // SAFETY: cancellation completion is drained before the OVERLAPPED and its
    // backing buffer leave scope. ERROR_OPERATION_ABORTED is expected here.
    let _ = unsafe { GetOverlappedResult(handle, overlapped, &raw mut transferred, true) };
}

struct PipeIo {
    handle: OwnedHandle,
    deadline: Instant,
    stopped: Option<Arc<AtomicBool>>,
}

impl PipeIo {
    fn new(handle: OwnedHandle, timeout: Duration) -> Self {
        Self {
            handle,
            deadline: Instant::now() + timeout,
            stopped: None,
        }
    }

    fn server(handle: OwnedHandle, timeout: Duration, stopped: Arc<AtomicBool>) -> Self {
        Self {
            handle,
            deadline: Instant::now() + timeout,
            stopped: Some(stopped),
        }
    }

    fn reset_deadline(&mut self, timeout: Duration) {
        self.deadline = Instant::now() + timeout;
    }

    fn finish(
        &self,
        started: windows::core::Result<()>,
        overlapped: &OVERLAPPED,
    ) -> io::Result<usize> {
        let handle = raw_handle(&self.handle);
        match started {
            Ok(()) => {
                let mut transferred = 0;
                unsafe { GetOverlappedResult(handle, overlapped, &raw mut transferred, false) }
                    .map_err(windows_error)?;
                Ok(transferred as usize)
            }
            Err(error) if is_error(&error, ERROR_IO_PENDING.0) => loop {
                if self
                    .stopped
                    .as_ref()
                    .is_some_and(|stopped| stopped.load(Ordering::Acquire))
                {
                    cancel_and_drain(handle, overlapped);
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        ErrorCode::Cancelled,
                    ));
                }
                let remaining = match remaining_millis(self.deadline) {
                    Ok(remaining) => remaining,
                    Err(error) => {
                        cancel_and_drain(handle, overlapped);
                        return Err(error);
                    }
                };
                let timeout = if self.stopped.is_some() {
                    remaining.min(POLL_INTERVAL.as_millis() as u32)
                } else {
                    remaining
                };
                let mut transferred = 0;
                match unsafe {
                    GetOverlappedResultEx(handle, overlapped, &raw mut transferred, timeout, false)
                } {
                    Ok(()) => return Ok(transferred as usize),
                    Err(error) if is_error(&error, WAIT_TIMEOUT.0) => {}
                    Err(error) => {
                        cancel_and_drain(handle, overlapped);
                        return Err(windows_error(error));
                    }
                }
            },
            Err(error) => Err(windows_error(error)),
        }
    }

    /// Mirrors the Unix contract: one-request connections must stay open and
    /// send no trailing bytes while awaiting a reply. A disconnected client,
    /// any unread trailing byte, or a peek error all cancel pending dispatch.
    fn peer_finished(&self) -> io::Result<bool> {
        let mut available = 0u32;
        match unsafe {
            PeekNamedPipe(
                raw_handle(&self.handle),
                None,
                0,
                None,
                Some(&raw mut available),
                None,
            )
        } {
            Ok(()) => Ok(available != 0),
            Err(error)
                if is_error(&error, ERROR_BROKEN_PIPE.0) || is_error(&error, ERROR_NO_DATA.0) =>
            {
                Ok(true)
            }
            Err(error) => Err(windows_error(error)),
        }
    }
}

impl Read for PipeIo {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let event = owned_handle(
            unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(windows_error)?,
        )?;
        let mut overlapped = OVERLAPPED {
            hEvent: raw_handle(&event),
            ..Default::default()
        };
        let length = bytes.len().min(u32::MAX as usize);
        let started = unsafe {
            ReadFile(
                raw_handle(&self.handle),
                Some(&mut bytes[..length]),
                None,
                Some(&raw mut overlapped),
            )
        };
        self.finish(started, &overlapped)
    }
}

impl Write for PipeIo {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let event = owned_handle(
            unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(windows_error)?,
        )?;
        let mut overlapped = OVERLAPPED {
            hEvent: raw_handle(&event),
            ..Default::default()
        };
        let length = bytes.len().min(u32::MAX as usize);
        let started = unsafe {
            WriteFile(
                raw_handle(&self.handle),
                Some(&bytes[..length]),
                None,
                Some(&raw mut overlapped),
            )
        };
        self.finish(started, &overlapped)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn wait_for_client(handle: &OwnedHandle, stopped: &AtomicBool) -> io::Result<bool> {
    let event = owned_handle(
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(windows_error)?,
    )?;
    let mut overlapped = OVERLAPPED {
        hEvent: raw_handle(&event),
        ..Default::default()
    };
    let started = unsafe { ConnectNamedPipe(raw_handle(handle), Some(&raw mut overlapped)) };
    match started {
        Ok(()) => return Ok(true),
        Err(error) if is_error(&error, ERROR_PIPE_CONNECTED.0) => return Ok(true),
        Err(error) if !is_error(&error, ERROR_IO_PENDING.0) => {
            return Err(windows_error(error));
        }
        Err(_) => {}
    }
    loop {
        if stopped.load(Ordering::Acquire) {
            cancel_and_drain(raw_handle(handle), &overlapped);
            return Ok(false);
        }
        let mut transferred = 0;
        match unsafe {
            GetOverlappedResultEx(
                raw_handle(handle),
                &raw const overlapped,
                &raw mut transferred,
                POLL_INTERVAL.as_millis() as u32,
                false,
            )
        } {
            Ok(()) => return Ok(true),
            Err(error) if is_error(&error, WAIT_TIMEOUT.0) => {}
            Err(error) => return Err(windows_error(error)),
        }
    }
}

/// Holds a bounded named-pipe listener thread. Every connected client is SID
/// checked before framing bytes are read. Dropping the last handle removes the
/// pipe name; Windows needs no filesystem cleanup.
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

    pub fn bind(
        path: &Path,
        submission: Submission,
        wake: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let name = Arc::new(wide_endpoint(path)?);
        let owner = Arc::new(process_user_sid(None)?);
        // FIRST_PIPE_INSTANCE is used only for this atomic name claim. Every
        // successor is created while a prior instance still holds the name.
        let first = create_pipe(&name, &owner, true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let fault = Arc::new(Mutex::new(None));
        let fault_slot = fault.clone();
        let wake: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(wake);
        let wake_handle = wake.clone();
        let thread = thread::Builder::new()
            .name("odytty-control".into())
            .spawn(move || {
                let mut pending = Some(first);
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                let mut rate_started = Instant::now();
                let mut accepted = 0usize;
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
                    if workers.len() >= MAX_CLIENTS || accepted >= MAX_CONNECTIONS_PER_SECOND {
                        thread::sleep(POLL_INTERVAL);
                        continue;
                    }
                    let Some(listener) = pending.as_ref() else {
                        break;
                    };
                    match wait_for_client(listener, &stop) {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(error) => {
                            record_fault(
                                &fault_slot,
                                &*wake,
                                format!("connect wait failed: {error}"),
                            );
                            break;
                        }
                    }
                    accepted += 1;
                    let connected = pending.take().expect("connected pipe instance");
                    // The successor is created before the connected instance is
                    // served, so the pipe name never disappears between clients.
                    // A creation failure ends listening: the name would be free
                    // for another process to claim, and reporting that beats
                    // serving one last client on a vanished endpoint.
                    pending = match create_pipe(&name, &owner, false) {
                        Ok(successor) => Some(successor),
                        Err(error) => {
                            record_fault(
                                &fault_slot,
                                &*wake,
                                format!("successor pipe instance failed: {error}"),
                            );
                            None
                        }
                    };
                    let submission = submission.clone();
                    let wake = wake.clone();
                    let worker_stop = stop.clone();
                    let worker_owner = owner.clone();
                    match thread::Builder::new()
                        .name("odytty-control-client".into())
                        .spawn(move || {
                            let _ = serve(connected, submission, wake, worker_stop, worker_owner);
                        }) {
                        Ok(worker) => workers.push(worker),
                        // The connected instance drops here, so the client sees
                        // a broken pipe instead of a hang.
                        Err(error) => {
                            tracing::warn!(%error, "automation client worker spawn failed");
                        }
                    }
                    if pending.is_none() {
                        break;
                    }
                }
                stop.store(true, Ordering::Release);
                drop(pending);
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
    handle: OwnedHandle,
    submission: Submission,
    wake: Arc<dyn Fn() -> bool + Send + Sync>,
    stopped: Arc<AtomicBool>,
    owner: Arc<OwnedSid>,
) -> io::Result<()> {
    let mut client_pid = 0u32;
    unsafe { GetNamedPipeClientProcessId(raw_handle(&handle), &raw mut client_pid) }
        .map_err(windows_error)?;
    verify_process_owner(client_pid, &owner)?;

    let mut io = PipeIo::server(handle, IO_TIMEOUT, stopped.clone());
    let request = match protocol::read_request(&mut io) {
        Ok(request) => request,
        Err(error) => {
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
            if stopped.load(Ordering::Acquire) || io.peer_finished()? || !wake() {
                return Ok(());
            }
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
    // The server handle closes after the bounded write without calling
    // DisconnectNamedPipe, which would discard bytes the client has not yet
    // consumed. FlushFileBuffers is deliberately not used because it blocks
    // until the client reads and has no timeout. Delivery of the buffered
    // response is therefore an empirical claim pinned by the round-trip and
    // oversized-reply tests on the Windows CI leg, not a documented guarantee.
    protocol::write_response(&mut io, &response)
}

/// A client that connects and closes before the listener thread observes the
/// connection leaves the only pipe instance in its closing state, and Windows
/// reports the name as absent until the listener creates the successor on its
/// next poll. A missing name is therefore retried for this bounded window
/// before it is reported; a genuinely absent endpoint still fails fast.
const NOT_FOUND_GRACE: Duration = Duration::from_millis(250);

fn connect(path: &Path, timeout: Duration) -> io::Result<PipeIo> {
    let name = wide_endpoint(path)?;
    let deadline = Instant::now() + timeout;
    let not_found_until = Instant::now() + NOT_FOUND_GRACE.min(timeout);
    loop {
        let wait = remaining_millis(deadline)?;
        if !unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), wait) }.as_bool() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND.0 as i32) {
                if Instant::now() < not_found_until {
                    thread::sleep(POLL_INTERVAL);
                    continue;
                }
                return Err(error);
            }
            if matches!(
                error.raw_os_error(),
                Some(code) if code == WAIT_TIMEOUT.0 as i32 || code == ERROR_SEM_TIMEOUT.0 as i32
            ) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, ErrorCode::TimedOut));
            }
            return Err(error);
        }
        let opened = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        match opened {
            Ok(handle) => return Ok(PipeIo::new(owned_handle(handle)?, timeout)),
            Err(error) if is_error(&error, ERROR_PIPE_BUSY.0) => continue,
            Err(error) => return Err(windows_error(error)),
        }
    }
}

/// Exchange exactly once. The endpoint must be an explicit local OdyTTY pipe;
/// no remote UNC name, discovery, shell parsing, or automatic retry is used.
pub fn request(path: &Path, request: &Request) -> io::Result<Response> {
    let owner = process_user_sid(None)?;
    let mut io = connect(path, IO_TIMEOUT)?;
    let mut server_pid = 0u32;
    unsafe { GetNamedPipeServerProcessId(raw_handle(&io.handle), &raw mut server_pid) }
        .map_err(windows_error)?;
    verify_process_owner(server_pid, &owner)?;

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

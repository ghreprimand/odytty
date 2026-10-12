// SPDX-License-Identifier: GPL-3.0-only
//! Credential verification and absolute-deadline socket I/O.

use super::*;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;

/// How long a connect waits before retrying a full accept queue.
pub(super) const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(5);

pub(crate) fn peer_is_owner(stream: &UnixStream) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let uid = {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: the connected fd and writable struct/length match SO_PEERCRED.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize != std::mem::size_of_val(&credentials) {
            return Err(denied());
        }
        credentials.uid
    };
    #[cfg(target_os = "macos")]
    let uid = {
        let mut uid = 0;
        let mut gid = 0;
        // SAFETY: the connected fd and writable credential outputs are valid.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        uid
    };
    if uid != unsafe { libc::geteuid() } {
        return Err(denied());
    }
    Ok(())
}

pub(crate) struct DeadlineStream {
    stream: UnixStream,
    deadline: Instant,
}

impl DeadlineStream {
    pub(crate) fn new(stream: UnixStream, timeout: Duration) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            deadline: Instant::now() + timeout,
        })
    }

    pub(super) fn reset_deadline(&mut self, timeout: Duration) {
        self.deadline = Instant::now() + timeout;
    }

    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "automation I/O deadline elapsed")
            })
    }

    fn wait(&self, events: libc::c_short) -> io::Result<()> {
        loop {
            let remaining = self.remaining()?;
            let mut poll = libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events,
                revents: 0,
            };
            // SAFETY: one live descriptor and a timeout bounded by the absolute
            // deadline. Read/write below surfaces POLLERR and POLLHUP precisely.
            let ready = unsafe {
                libc::poll(
                    &mut poll,
                    1,
                    remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if ready > 0 {
                return Ok(());
            }
            if ready == 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    /// One-request connections must stay open and send no trailing bytes while
    /// awaiting a reply. EOF, extra bytes, or an error cancel pending dispatch.
    pub(super) fn peer_finished(&self) -> io::Result<bool> {
        let mut byte = 0u8;
        // SAFETY: valid fd and one-byte writable buffer; peek consumes nothing.
        let result = unsafe {
            libc::recv(
                self.stream.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if result >= 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(false),
            _ => Err(error),
        }
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.wait(libc::POLLIN)?;
            match self.stream.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            self.wait(libc::POLLOUT)?;
            match self.stream.write(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Run `attempt` until it connects, fails for another reason, or `deadline`
/// passes. The first attempt is always made. A retry is made only while time
/// remains: the deadline is checked before every attempt after the first, so an
/// interrupted attempt cannot loop past it and a retry never runs, or succeeds,
/// after it. A full accept queue (`WouldBlock`) sleeps no longer than the time
/// left. Any other error, including `EINPROGRESS`, is returned for the caller.
pub(super) fn retry_connect(
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
    mut attempt: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let timed_out = || io::Error::new(io::ErrorKind::TimedOut, ErrorCode::TimedOut);
    let mut first = true;
    loop {
        if !first && now() >= deadline {
            return Err(timed_out());
        }
        first = false;
        match attempt() {
            Ok(()) => return Ok(()),
            // Interrupted before it started: try again while time remains.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            // A full accept queue: no connect was started, so trying again on
            // the same unconnected socket is safe.
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(now());
                if remaining.is_zero() {
                    return Err(timed_out());
                }
                sleep(CONNECT_RETRY_INTERVAL.min(remaining));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Connect to an AF_UNIX endpoint within `timeout`, never blocking past it.
///
/// The socket is nonblocking. A nonblocking AF_UNIX connect completes at once
/// or, when the listener's accept queue is full, fails with `EAGAIN` (Linux)
/// without starting; that is retried every few milliseconds until the deadline
/// and then reported as `TimedOut`, so a busy owner is waited for rather than
/// refused. `EINPROGRESS`, which AF_UNIX does not return on Linux, is still
/// handled by polling for writability against the same deadline.
pub(crate) fn connect(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    let bytes = path.as_os_str().as_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid automation endpoint length",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *source as libc::c_char;
    }
    let address_len = std::mem::offset_of!(libc::sockaddr_un, sun_path)
        .checked_add(bytes.len())
        .and_then(|length| length.checked_add(1))
        .and_then(|length| libc::socklen_t::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "endpoint path too long"))?;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = u8::try_from(address_len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "endpoint path too long"))?;
    }
    // SAFETY: socket returns an owned descriptor; it is immediately wrapped.
    #[cfg(target_os = "linux")]
    let socket_type = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(target_os = "macos")]
    let socket_type = libc::SOCK_STREAM;
    let fd = unsafe { libc::socket(libc::AF_UNIX, socket_type, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    #[cfg(target_os = "macos")]
    {
        // Darwin has no SOCK_CLOEXEC flag. Set close-on-exec immediately,
        // matching the platform's existing PTY descriptor setup.
        rustix::io::fcntl_setfd(&stream, rustix::io::FdFlags::CLOEXEC)?;
        let enabled: libc::c_int = 1;
        // SAFETY: a valid socket and an initialized integer option buffer.
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&enabled as *const libc::c_int).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    stream.set_nonblocking(true)?;
    let result = retry_connect(deadline, Instant::now, thread::sleep, || {
        // SAFETY: address is a fully initialized sockaddr_un of the supplied size.
        let result = unsafe {
            libc::connect(
                fd,
                (&address as *const libc::sockaddr_un).cast(),
                address_len,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    });
    if let Err(error) = result {
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, ErrorCode::TimedOut));
            }
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: one valid pollfd; duration is bounded by the caller.
            let ready = unsafe {
                libc::poll(
                    &mut poll,
                    1,
                    remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready == 0 {
                continue;
            }
            if let Some(error) = stream.take_error()? {
                return Err(error);
            }
            break;
        }
    }
    stream.set_nonblocking(false)?;
    Ok(stream)
}

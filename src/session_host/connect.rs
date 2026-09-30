// SPDX-License-Identifier: GPL-3.0-only
//! Bounded client-side connects to a session-host socket.
//!
//! `UnixStream::connect` blocks while the host's listen backlog is full, which
//! a wedged host (alive, never calling `accept()`) reaches as soon as enough
//! clients queue on it. Every caller that runs on a window's event loop (the
//! session list, kill, attach, and launch restore) must never wait on that, so
//! connects go through [`connect_within`], which uses a nonblocking socket and
//! gives up at a deadline instead.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// Pause between retries while a Linux listen backlog reports `EAGAIN`.
const BACKLOG_RETRY_PAUSE: Duration = Duration::from_millis(5);

/// Connect to `path`, waiting at most `deadline` for the host to take the
/// connection. A zero deadline makes exactly one attempt and never waits.
///
/// The returned stream is in blocking mode with no timeouts; callers bound
/// their own handshake (see [`bound_hello_write`]). Failures keep the OS error: `ENOENT` and `ECONNREFUSED` still mean no host
/// is listening, while a full backlog past the deadline reports `TimedOut`.
pub(crate) fn connect_within(path: &Path, deadline: Duration) -> io::Result<UnixStream> {
    let (address, address_len) = socket_address(path)?;
    // SAFETY: plain socket(2) call; the result is checked before use.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a freshly created, owned descriptor; the stream closes it
    // on every return path from here on.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    // SAFETY: fcntl on the descriptor owned by `stream`.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    stream.set_nonblocking(true)?;

    let end = Instant::now() + deadline;
    loop {
        // SAFETY: `address` is a fully initialized sockaddr_un and
        // `address_len` does not exceed its size.
        let rc = unsafe {
            libc::connect(
                stream.as_raw_fd(),
                (&raw const address).cast::<libc::sockaddr>(),
                address_len,
            )
        };
        if rc == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::EISCONN) => break,
            // Linux: the listen backlog is full. Retry until the deadline.
            Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => {
                if Instant::now() >= end {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "session host did not accept the connection in time",
                    ));
                }
                thread::sleep(
                    BACKLOG_RETRY_PAUSE.min(end.saturating_duration_since(Instant::now())),
                );
            }
            // Other platforms may report an in-flight connect instead.
            Some(libc::EINPROGRESS | libc::EALREADY) => {
                wait_writable(&stream, end)?;
                return finish(stream);
            }
            _ => return Err(error),
        }
    }
    finish(stream)
}

fn finish(stream: UnixStream) -> io::Result<UnixStream> {
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// Run `write` (the client hello) with the socket's send timeout set to the
/// time left before `end`, then clear it, so a host that never reads cannot
/// hold the hello write past the handshake deadline and later writes keep
/// their own semantics.
pub(crate) fn bound_hello_write<T>(
    stream: &UnixStream,
    end: Instant,
    write: impl FnOnce() -> T,
) -> io::Result<T> {
    let remaining = end
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1));
    stream.set_write_timeout(Some(remaining))?;
    let result = write();
    stream.set_write_timeout(None)?;
    Ok(result)
}

/// Wait for an in-flight connect to complete, then report its result.
fn wait_writable(stream: &UnixStream, end: Instant) -> io::Result<()> {
    loop {
        let remaining = end.saturating_duration_since(Instant::now());
        let timeout_ms = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
        let mut poll_fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&raw mut poll_fd, 1, timeout_ms) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "session host did not accept the connection in time",
            ));
        }
        return match stream.take_error()? {
            Some(error) => Err(error),
            None => Ok(()),
        };
    }
}

fn socket_address(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is plain old data; all-zero is a valid value.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session-host socket path does not fit a Unix socket address",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let base = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    let len = base + bytes.len() + 1;
    let len = libc::socklen_t::try_from(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket address length overflow",
        )
    })?;
    Ok((address, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn temp_socket(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "odytty-connect-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("s.sock")
    }

    #[test]
    fn connects_to_a_listening_socket_and_returns_blocking_stream() {
        let path = temp_socket("ok");
        let _listener = UnixListener::bind(&path).expect("bind");
        let stream = connect_within(&path, Duration::from_millis(200)).expect("connect");
        assert!(stream.write_timeout().expect("timeout").is_none());
        let end = Instant::now() + Duration::from_millis(200);
        bound_hello_write(&stream, end, || {
            assert!(stream.write_timeout().expect("timeout").is_some());
        })
        .expect("bounded write");
        assert!(stream.write_timeout().expect("timeout").is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_socket_keeps_not_found() {
        let path = temp_socket("missing");
        let error = connect_within(&path, Duration::ZERO).expect_err("no socket");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn stale_socket_keeps_connection_refused() {
        let path = temp_socket("stale");
        drop(UnixListener::bind(&path).expect("bind"));
        let error = connect_within(&path, Duration::ZERO).expect_err("stale");
        assert_eq!(error.raw_os_error(), Some(libc::ECONNREFUSED));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn oversized_path_is_rejected_before_any_syscall() {
        let long = std::path::PathBuf::from(format!("/tmp/{}", "x".repeat(200)));
        let error = connect_within(&long, Duration::ZERO).expect_err("too long");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

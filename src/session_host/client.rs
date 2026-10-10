// SPDX-License-Identifier: GPL-3.0-only
//! Minimal attach client for the local session-host protocol.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::protocol::{
    ClientFrame, ClientHello, HostFrame, HostFrameReader, MAX_CLIENT_INPUT_LEN, ProtocolError,
    read_host_hello, write_client_frame, write_client_hello, write_client_input,
};
use super::socket::{HELLO_READ_DEADLINE, SocketReadDeadline, validate_socket_parent};

#[derive(Debug)]
pub struct SessionHostClient {
    stream: UnixStream,
    /// Resumable frame reader: [`Self::read_frame`] polls with an absolute read deadline,
    /// and a timeout firing mid-frame must preserve the partial frame so the
    /// caller's retry resumes it instead of desyncing on leftover payload bytes
    /// (audit P1).
    frame_reader: HostFrameReader,
    poisoned: bool,
}

impl SessionHostClient {
    pub fn connect(socket_path: &Path, session_id: &str) -> Result<Self> {
        Self::connect_within_deadline(socket_path, session_id, HELLO_READ_DEADLINE)
    }

    /// [`Self::connect`] with an explicit hello-read deadline, for tests.
    pub(super) fn connect_within_deadline(
        socket_path: &Path,
        session_id: &str,
        hello_deadline: Duration,
    ) -> Result<Self> {
        validate_socket_parent(socket_path)?;
        // One deadline covers connect, hello write, and hello read. The connect
        // is nonblocking: a wedged host with a full listen backlog would
        // otherwise block the calling thread before any deadline applied.
        let hello_end = Instant::now() + hello_deadline;
        let stream = super::connect::connect_within(socket_path, hello_deadline)
            .with_context(|| format!("connect session-host {}", socket_path.display()))?;
        super::connect::bound_hello_write(&stream, hello_end, || {
            let mut writer = &stream;
            write_client_hello(&mut writer, &ClientHello::current(session_id))
        })
        .context("bound session-host client hello")?
        .context("write session-host client hello")?;
        // C-2: bound the hello read. `connect` succeeds against a wedged host via
        // the listen backlog even though the host never `accept()`s, so an
        // unbounded read here would freeze the calling thread forever. Scope the
        // deadline reader so the borrow ends before the stream is stored.
        {
            let mut guarded = SocketReadDeadline::new(&stream, hello_end);
            read_host_hello(&mut guarded)
        }
        .context("read session-host hello")?
        .into_result()
        .context("session-host attach rejected")?;
        Ok(Self {
            stream,
            frame_reader: HostFrameReader::default(),
            poisoned: false,
        })
    }

    pub fn read_frame(&mut self, timeout: Duration) -> Result<Option<HostFrame>> {
        if timeout.is_zero() {
            bail!("session-host frame poll timeout must be nonzero");
        }
        let end = Instant::now()
            .checked_add(timeout)
            .context("session-host frame poll timeout is out of range")?;
        let mut reader = SocketReadDeadline::new(&self.stream, end);
        match self.frame_reader.read(&mut reader) {
            Ok(frame) => Ok(Some(frame)),
            Err(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error).context("read session-host frame"),
        }
    }

    pub fn send_input(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_CLIENT_INPUT_LEN {
            return Err(ProtocolError::FrameTooLarge {
                len: bytes.len(),
                max: MAX_CLIENT_INPUT_LEN,
            }
            .into());
        }
        self.write_bounded(|writer| write_client_input(writer, bytes))
            .context("write session-host input frame")
    }

    pub fn resize(&mut self, columns: u32, rows: u32) -> Result<()> {
        if columns == 0 || rows == 0 {
            bail!("session-host resize dimensions must be nonzero");
        }
        self.write_bounded(|writer| {
            write_client_frame(writer, &ClientFrame::Resize { columns, rows })
        })
        .context("write session-host resize frame")
    }

    pub fn detach(&mut self) -> Result<()> {
        self.write_bounded(|writer| write_client_frame(writer, &ClientFrame::Detach))
            .context("write session-host detach frame")
    }

    /// Ask the host to terminate the whole session (manager "kill session").
    /// Mirrors [`Self::detach`]; the host SIGHUPs its shell and tears down,
    /// unlinking the socket so the session disappears from the registry.
    pub fn shutdown(&mut self) -> Result<()> {
        self.write_bounded(|writer| write_client_frame(writer, &ClientFrame::Shutdown))
            .context("write session-host shutdown frame")
    }

    /// Bound the whole command, retaining the zero-progress drop policy.
    /// Partial delivery closes the socket and refuses every later command.
    fn write_bounded(
        &mut self,
        write: impl FnOnce(
            &mut super::handshake::DeadlineWriter<'_>,
        ) -> std::result::Result<(), ProtocolError>,
    ) -> Result<()> {
        if self.poisoned {
            bail!("session-host stream is desynced by a prior failed write");
        }
        let result = write(&mut super::handshake::DeadlineWriter::new(
            &self.stream,
            Duration::from_secs(2),
            Duration::from_millis(500),
        ));
        match result {
            Ok(()) => Ok(()),
            Err(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(())
            }
            Err(error) => {
                self.poisoned = true;
                let _ = self.stream.shutdown(std::net::Shutdown::Both);
                Err(error.into())
            }
        }
    }
}

#[cfg(test)]
#[path = "client_deadline_tests.rs"]
mod deadline_tests;

// SPDX-License-Identifier: GPL-3.0-only
//! Minimal attach client for the local session-host protocol.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::protocol::{
    ClientFrame, ClientHello, HostFrame, ProtocolError, read_host_frame, read_host_hello,
    write_client_frame, write_client_hello,
};
use super::socket::validate_socket_parent;

#[derive(Debug)]
pub struct SessionHostClient {
    stream: UnixStream,
}

impl SessionHostClient {
    pub fn connect(socket_path: &Path, session_id: &str) -> Result<Self> {
        validate_socket_parent(socket_path)?;
        let mut stream = UnixStream::connect(socket_path)
            .with_context(|| format!("connect session-host {}", socket_path.display()))?;
        write_client_hello(&mut stream, &ClientHello::current(session_id))
            .context("write session-host client hello")?;
        read_host_hello(&mut stream)
            .context("read session-host hello")?
            .into_result()
            .context("session-host attach rejected")?;
        Ok(Self { stream })
    }

    pub fn read_frame(&mut self, timeout: Duration) -> Result<Option<HostFrame>> {
        // Best-effort poll timeout. On macOS, once the host has closed its end
        // (the session exited), `set_read_timeout` on the now peer-closed socket
        // returns EINVAL, whereas on Linux it succeeds. The host's final buffered
        // frames (e.g. `SessionExit`) are still readable, so failing here would
        // drop them and surface a confusing "Invalid argument" instead. A closed
        // peer makes the read below return promptly (buffered frame, then EOF)
        // rather than blocking, so dropping the poll timeout cannot hang; a live
        // peer never trips the EINVAL and the timeout still bounds the poll exactly
        // as before → byte-identical on Linux.
        let _ = self.stream.set_read_timeout(Some(timeout));
        match read_host_frame(&mut self.stream) {
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
        write_client_frame(&mut self.stream, &ClientFrame::Input(bytes.to_vec()))
            .context("write session-host input frame")
    }

    pub fn resize(&mut self, columns: u32, rows: u32) -> Result<()> {
        if columns == 0 || rows == 0 {
            bail!("session-host resize dimensions must be nonzero");
        }
        write_client_frame(&mut self.stream, &ClientFrame::Resize { columns, rows })
            .context("write session-host resize frame")
    }

    pub fn detach(&mut self) -> Result<()> {
        write_client_frame(&mut self.stream, &ClientFrame::Detach)
            .context("write session-host detach frame")
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Native window-as-client attach to a detached session-host (Phase 2).
//!
//! This is the GUI consumer of the session-host's documented attach wire
//! sequence (see `docs/panes-and-sessions-design.md` §6.1/§6.2). It is built
//! directly against the **public** [`crate::session_host::protocol`] contract and
//! the public socket helpers, not by extending the CLI/diagnostic
//! [`crate::session_host::SessionHostClient`] — two separate consumers of one
//! stable wire protocol, which keeps file ownership clean and lets the GUI split
//! read/write across threads.
//!
//! Flow:
//! 1. [`AttachClient::connect_within`] performs the handshake, reads exactly one initial
//!    [`HostFrame::Snapshot`], decodes it under bounded caps, and restores a live
//!    [`Terminal`] — the "close window, reopen, full scrollback intact" step.
//! 2. The connected `UnixStream` is `try_clone`d: the original is the write side
//!    (App thread: input/resize/detach), the clone is the read side, driven by
//!    [`spawn_attach_pump`] on a dedicated thread. Both share one kernel socket,
//!    so frame ordering is preserved and no lock sits on the keystroke path —
//!    mirroring the local-PTY pump in [`super::pty`].
//! 3. The client terminal is a **render mirror**: the host owns the authoritative
//!    model and already answered device queries against its own terminal, so the
//!    mirror's `take_host_output` is intentionally discarded (sending it back
//!    would double-respond).
//!
//! Attach is an additive alternate session **source**; a normal locally-spawned
//! session is untouched, so the single-session / single-pane render path stays
//! byte-identical.
//!
//! Wiring: the live App consumption (making an attached source a tab) lands in
//! [`super::session::WorkspaceSet::attach_in_new_tab`], which builds the input
//! [`PtyWriter`] from [`attach_input_writer`] so an attached session reuses the
//! exact same app-side input path as a local PTY (see design doc §6.2).

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use winit::event_loop::EventLoopProxy;

use crate::core::{SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal};
use crate::session_host::protocol::{
    ClientFrame, ClientHello, HostFrame, HostFrameReader, ProtocolError, read_host_frame,
    read_host_hello, write_client_frame, write_client_hello,
};
use crate::session_host::{existing_runtime_dir, session_socket_path, validate_socket_parent};

use super::pty::{PtyWriter, UserEvent};
use super::session::SessionToken;

/// Per-read timeout while waiting for the initial snapshot, so the deadline loop
/// can poll without blocking indefinitely on a single read.
const SNAPSHOT_POLL: Duration = Duration::from_millis(50);

/// A sink the attach pump uses to wake the UI when a frame lands. Implemented for
/// winit's [`EventLoopProxy`] in production and for an `mpsc` sender in tests, so
/// the whole pump is exercisable without a running event loop.
pub(super) trait AttachEventSink: Send + 'static {
    /// New host output (or an invalidate) landed in the mirror terminal; the
    /// window should rebuild + redraw the given session.
    fn redraw(&self, session: SessionToken);
    /// The hosted session ended (or the link dropped); the window should treat
    /// this session as exited.
    fn exited(&self, session: SessionToken);
}

impl AttachEventSink for EventLoopProxy<UserEvent> {
    fn redraw(&self, session: SessionToken) {
        let _ = self.send_event(UserEvent::Redraw { session });
    }

    fn exited(&self, session: SessionToken) {
        let _ = self.send_event(UserEvent::ShellExited { session });
    }
}

/// The write half of an attach connection: the App thread sends input, resize,
/// and detach frames through it. Reading is done by the [`AttachReader`] pump on
/// a separate thread over a clone of the same socket.
#[derive(Debug)]
pub(super) struct AttachClient {
    stream: UnixStream,
    detached: bool,
}

impl AttachClient {
    /// Connect to a hosted session by socket path and id, complete the handshake,
    /// receive and decode the initial snapshot, and restore a mirror terminal,
    /// bounding the wait for the host's initial snapshot by `deadline`.
    ///
    /// Returns the write-side client, the read-side reader (hand to
    /// [`spawn_attach_pump`]), and the restored [`Terminal`]. The hosted session
    /// is never mutated by a rejected attach (the host guarantees this). The
    /// restore/reattach batch passes whatever remains of its aggregate budget so
    /// K panes cannot each cost the full [`super::session::SNAPSHOT_DEADLINE`] (a `K * 5s` UI
    /// freeze); an interactive single attach passes [`super::session::SNAPSHOT_DEADLINE`].
    pub(super) fn connect_within(
        socket_path: &Path,
        session_id: &str,
        deadline: Duration,
    ) -> Result<(Self, AttachReader, Terminal)> {
        Self::connect_with(
            socket_path,
            session_id,
            SnapshotEnvelopeCaps::default(),
            deadline,
        )
    }

    /// [`Self::connect`] with explicit decode caps and snapshot deadline, for
    /// tests.
    fn connect_with(
        socket_path: &Path,
        session_id: &str,
        caps: SnapshotEnvelopeCaps,
        deadline: Duration,
    ) -> Result<(Self, AttachReader, Terminal)> {
        validate_socket_parent(socket_path)?;
        let mut stream = UnixStream::connect(socket_path)
            .with_context(|| format!("connect session-host {}", socket_path.display()))?;
        write_client_hello(&mut stream, &ClientHello::current(session_id))
            .context("write session-host client hello")?;
        read_host_hello(&mut stream)
            .context("read session-host hello")?
            .into_result()
            .context("session-host attach rejected")?;

        let snapshot = read_initial_snapshot(&mut stream, deadline)?;
        let envelope =
            SnapshotEnvelope::decode(&snapshot, caps).context("decode session snapshot")?;
        let terminal =
            Terminal::from_snapshot_envelope(&envelope).context("restore session terminal")?;

        let read_stream = stream
            .try_clone()
            .context("clone session-host attach stream")?;
        // The pump reads with blocking semantics; clear any poll timeout left on
        // the shared fd from the snapshot wait. Best-effort, matching the pump
        // thread's own clear in `run_attach_pump` (which governs the actual read
        // semantics): on macOS `set_read_timeout(None)` on a `try_clone`d stream
        // returns EINVAL, so propagating it with `?` would fail the whole attach
        // for no reason — the pump re-clears the timeout before its first read, so
        // nothing is lost. On Linux the call succeeds and ignoring success is a
        // no-op → byte-identical.
        let _ = read_stream.set_read_timeout(None);
        Ok((
            Self {
                stream,
                detached: false,
            },
            AttachReader {
                stream: read_stream,
            },
            terminal,
        ))
    }

    /// Forward keystrokes / pasted bytes to the hosted PTY as an `Input` frame.
    /// A no-op for empty input.
    pub(super) fn send_input(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        write_client_frame(&mut self.stream, &ClientFrame::Input(bytes.to_vec()))
            .context("write session-host input frame")
    }

    /// Forward a window resize to the host (which applies `TIOCSWINSZ` + reflow on
    /// its side). Dimensions must be nonzero.
    pub(super) fn resize(&mut self, columns: u32, rows: u32) -> Result<()> {
        if columns == 0 || rows == 0 {
            bail!("session-host resize dimensions must be nonzero");
        }
        write_client_frame(&mut self.stream, &ClientFrame::Resize { columns, rows })
            .context("write session-host resize frame")
    }

    /// Send a clean `Detach`: this client leaves but the hosted PTY + terminal
    /// model stay alive for later attach by id. Idempotent — repeated calls (and
    /// the `Drop` best-effort detach) send at most one frame.
    pub(super) fn detach(&mut self) -> Result<()> {
        if self.detached {
            return Ok(());
        }
        self.detached = true;
        write_client_frame(&mut self.stream, &ClientFrame::Detach)
            .context("write session-host detach frame")
    }
}

impl Drop for AttachClient {
    fn drop(&mut self) {
        // Window close without an explicit detach still leaves the host alive.
        let _ = self.detach();
    }
}

/// A [`Write`] sink that forwards bytes to a hosted session as `Input` frames.
///
/// This is the key to byte-identity: an attached session is given a [`PtyWriter`]
/// (`Arc<Mutex<Box<dyn Write + Send>>>`) wrapping this writer, so **every**
/// app-side input site (`handle_key_event`, IME, paste, bracketed-paste) writes
/// through the exact same `self.writer` path as a local PTY — the input routing
/// code is unchanged and the local path is untouched. Only the boxed sink
/// differs: a local session boxes the PTY master, an attached session boxes this.
pub(super) struct AttachInputWriter {
    client: Arc<Mutex<AttachClient>>,
}

impl Write for AttachInputWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut client) = self.client.lock() {
            client.send_input(buf).map_err(io::Error::other)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // `send_input` already framed-and-flushed each write to the socket.
        Ok(())
    }
}

/// Build the input [`PtyWriter`] for an attached session: a boxed
/// [`AttachInputWriter`] sharing the same [`AttachClient`] the resize/detach path
/// uses, so input/resize/detach all serialize through one socket lock.
pub(super) fn attach_input_writer(
    client: Arc<Mutex<AttachClient>>,
    session: SessionToken,
) -> std::io::Result<PtyWriter> {
    let boxed = super::pty_writer::writer_shim(
        Box::new(AttachInputWriter { client }) as Box<dyn Write + Send>,
        session,
    )?;
    Ok(Arc::new(Mutex::new(boxed)))
}

/// Resolve a hosted session id to its per-user socket path, mirroring the CLI's
/// `attach` id resolution (existing runtime dir → per-id socket → existence
/// check). `runtime_base` is `None` in production (derived from
/// `XDG_RUNTIME_DIR`) and set explicitly only by tests. Errors when the session
/// is not found so the caller can surface a clean "session not found" message.
pub(super) fn resolve_session_socket(
    runtime_base: Option<&Path>,
    session_id: &str,
) -> Result<PathBuf> {
    let Some(runtime_dir) = existing_runtime_dir(runtime_base)? else {
        bail!("session not found: {session_id}");
    };
    let socket = session_socket_path(&runtime_dir, session_id)?;
    if !socket.exists() {
        bail!("session not found: {session_id}");
    }
    Ok(socket)
}

/// The read half of an attach connection: a clone of the connected socket, owned
/// by the pump thread.
#[derive(Debug)]
pub(super) struct AttachReader {
    stream: UnixStream,
}

/// Read frames until the host's initial [`HostFrame::Snapshot`] arrives, or the
/// deadline elapses. Per the contract the snapshot is the first frame, but any
/// `Output`/`Invalidate` seen first is tolerated and ignored; `SessionExit` or
/// `Error` before the snapshot is a hard failure.
fn read_initial_snapshot(stream: &mut UnixStream, deadline: Duration) -> Result<Vec<u8>> {
    // Best-effort poll timeout. On macOS, if the host has ALREADY closed its end
    // by the time we get here — a session that exits right after sending the
    // snapshot, or (in tests) a fast fake host that writes the snapshot and drops
    // — `set_read_timeout` on the now peer-closed socket returns EINVAL, whereas
    // on Linux the same call succeeds. Propagating it (the previous `?`) would
    // discard a snapshot that is still sitting readable in the socket buffer.
    // Tolerate the failure: a closed peer makes reads return promptly (the
    // buffered frames, then EOF) rather than blocking, so the deadline loop below
    // stays bounded even without the poll timeout. When the peer is alive (the
    // normal case) this succeeds on both platforms and the poll timeout drives the
    // deadline exactly as before → byte-identical on Linux.
    let _ = stream.set_read_timeout(Some(SNAPSHOT_POLL));
    // Resumable reader: a `SNAPSHOT_POLL` timeout firing mid-frame (a multi-MB
    // snapshot arriving split under backpressure) keeps the partial frame, and
    // the retry below resumes it instead of desyncing on leftover payload bytes
    // (audit P1).
    let mut frame_reader = HostFrameReader::default();
    let start = Instant::now();
    while start.elapsed() < deadline {
        match frame_reader.read(stream) {
            Ok(HostFrame::Snapshot(bytes)) => return Ok(bytes),
            Ok(HostFrame::Output(_)) | Ok(HostFrame::Invalidate { .. }) => {}
            Ok(HostFrame::SessionExit { .. }) => bail!("session exited before snapshot"),
            Ok(HostFrame::Error(message)) => {
                bail!("session-host error before snapshot: {message}")
            }
            Err(err) if is_would_block(&err) => continue,
            Err(err) => return Err(err).context("read session-host snapshot frame"),
        }
    }
    bail!("session attach timed out before snapshot")
}

/// Spawn the pump thread that drives the attached session's mirror terminal from
/// live host frames and wakes the UI through `sink`.
pub(super) fn spawn_attach_pump(
    reader: AttachReader,
    terminal: Arc<Mutex<Terminal>>,
    sink: impl AttachEventSink,
    session: SessionToken,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("odytty-attach-pump".to_owned())
        .spawn(move || run_attach_pump(reader, terminal, sink, session))
        .expect("spawn attach pump thread")
}

/// The pump loop body (separated so tests can run it inline). Blocks on the
/// socket; returns when the session exits, the link drops, or a fatal protocol
/// error occurs.
fn run_attach_pump(
    reader: AttachReader,
    terminal: Arc<Mutex<Terminal>>,
    sink: impl AttachEventSink,
    session: SessionToken,
) {
    let mut stream = reader.stream;
    // Blocking reads on the dedicated pump thread: a clean EOF surfaces as an
    // UnexpectedEof disconnect and ends the loop.
    let _ = stream.set_read_timeout(None);
    loop {
        match read_host_frame(&mut stream) {
            Ok(HostFrame::Output(bytes)) => {
                if let Ok(mut term) = terminal.lock() {
                    term.advance(&bytes);
                    // Render mirror: the host already answered device queries
                    // against its own authoritative terminal, so discard any
                    // reply bytes the mirror produced rather than double-respond.
                    let _ = term.take_host_output();
                }
                sink.redraw(session);
            }
            Ok(HostFrame::Invalidate { .. }) => sink.redraw(session),
            Ok(HostFrame::Snapshot(bytes)) => {
                // Per the contract only one snapshot is sent (at attach), but if
                // the host ever re-snapshots mid-stream, restore from it rather
                // than mis-applying envelope bytes as raw output.
                if let Ok(envelope) =
                    SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
                    && let Ok(mut term) = terminal.lock()
                {
                    let _ = term.restore_from_envelope(&envelope);
                }
                sink.redraw(session);
            }
            Ok(HostFrame::SessionExit { .. }) => {
                sink.exited(session);
                break;
            }
            Ok(HostFrame::Error(message)) => {
                eprintln!("odytty: session-host error: {message}");
                sink.exited(session);
                break;
            }
            Err(err) if err.is_disconnect() => {
                sink.exited(session);
                break;
            }
            Err(err) if is_would_block(&err) => continue,
            Err(err) => {
                eprintln!("odytty: attach pump read error: {err}");
                sink.exited(session);
                break;
            }
        }
    }
}

fn is_would_block(err: &ProtocolError) -> bool {
    matches!(
        err,
        ProtocolError::Io(io_err)
            if matches!(io_err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
    )
}

#[cfg(test)]
mod tests;

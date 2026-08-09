// SPDX-License-Identifier: GPL-3.0-only
//! PTY-owning session-host loop for the first resumable-session slice.

use std::env;
use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::core::{
    Dimensions, SNAPSHOT_FORMAT_VERSION, SNAPSHOT_PROTOCOL_VERSION, SnapshotCaptureLimits,
    SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal,
};
use crate::pty::PtySession;

use super::pty_writer::HostPtyWriter;

use super::protocol::{
    ClientFrame, ClientFramePoll, ClientFrameReader, HostFrame, versions_compatible,
    write_host_frame, write_host_hello,
};
use super::socket::{RuntimePaths, bind_listener, runtime_paths, validate_socket_parent};

pub const MAX_HOST_SESSIONS: usize = 1;
const DEFAULT_COLUMNS: usize = 80;
const DEFAULT_ROWS: usize = 24;
const DEFAULT_MAX_CLIENTS: usize = 8;
const DEFAULT_DETACHED_IDLE_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
const HOST_LOOP_SLEEP: Duration = Duration::from_millis(10);
/// PTY-reader events are fixed at no more than 8 KiB of output each. Bounding
/// the channel to this many entries caps userspace buffering at roughly 2 MiB
/// and lets normal PTY backpressure slow a child that outpaces the host.
const PTY_EVENT_QUEUE_CAP: usize = 256;
/// Never let a continuously-writing child monopolize the host loop. After this
/// many events, return to client input, shutdown, child-exit, and idle handling.
const MAX_PTY_EVENTS_PER_TICK: usize = PTY_EVENT_QUEUE_CAP;
/// Bound queued attach-client frames and disconnect notices. Input frames have
/// their own byte ceiling in the protocol; this count cap bounds aggregate
/// userspace retention when several clients send concurrently.
const CLIENT_EVENT_QUEUE_CAP: usize = DEFAULT_MAX_CLIENTS * 2;
/// Yield to PTY output, child-exit, accept, and idle handling after one bounded
/// client batch even if an attached peer continues sending.
const MAX_CLIENT_EVENTS_PER_TICK: usize = CLIENT_EVENT_QUEUE_CAP;
/// Grace window between observing the hosted child has exited (via a direct
/// `try_wait`) and forcing host shutdown. It lets the PTY reader thread flush any
/// final buffered output and deliver its own EOF — the normal, in-order exit path
/// — before the fallback fires. On a healthy host the EOF path always wins this
/// race well inside the grace, so the fallback never triggers; it exists only so
/// that a delayed or missing PTY-master EOF (observed on macOS/BSD under load, or
/// if another process transiently holds the slave fd open) can never wedge the
/// host indefinitely. Generous so a slow runner still flushes output in order.
const CHILD_EXIT_DRAIN_GRACE: Duration = Duration::from_secs(2);
const ATTACH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const ATTACH_WRITE_TIMEOUT: Duration = Duration::from_millis(500);
/// Poll granularity for the post-handshake per-client reader. The reader wakes
/// this often to check whether an in-flight frame has stalled; between whole
/// frames a timeout is ignored, so an attached-but-idle client (a user simply
/// not typing) is never detached.
const CLIENT_READ_POLL_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum time a single client frame may remain partially received before the
/// host reclaims the connection. A same-user peer can send a frame header and
/// withhold the body to pin a reader thread and its client slot indefinitely;
/// bounding the mid-frame stall releases the slot instead of wedging it.
const CLIENT_FRAME_STALL_DEADLINE: Duration = Duration::from_secs(10);
const STARTUP_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Upper bound on client-supplied resize dimensions. The wire protocol carries
/// raw `u32` columns/rows and the socket is reachable by any same-user process,
/// so the host must not trust them: unclamped, a hostile `Resize` frame
/// (e.g. `0xFFFFFFFF × 0xFFFFFFFF`) drives `Terminal::resize` into a
/// multi-exabyte grid allocation that aborts the host and kills the session for
/// every attached client. 4096 columns/rows comfortably exceeds any real
/// display while keeping the worst-case grid a few hundred MB. The per-axis
/// bound alone is not enough: 4096 x 4096 is ~16.7M visible cells, four times
/// the default snapshot decoder's total-cell cap, so an accepted resize could
/// make the host emit snapshots its own consumers reject. Resizes are therefore
/// also clamped to the total-cell budget derived from the decoder caps
/// ([`SnapshotEnvelopeCaps::max_self_decodable_visible_cells`]).
const MAX_CLIENT_RESIZE_DIM: usize = 4096;

#[derive(Debug, Clone)]
pub enum HostCommand {
    DefaultShell {
        working_directory: Option<PathBuf>,
    },
    ShellCommand(String),
    Exec {
        program: OsString,
        args: Vec<OsString>,
        working_directory: Option<PathBuf>,
    },
}

impl Default for HostCommand {
    fn default() -> Self {
        Self::DefaultShell {
            working_directory: None,
        }
    }
}

impl HostCommand {
    fn spawn(&self, dimensions: Dimensions) -> Result<PtySession> {
        match self {
            Self::DefaultShell { working_directory } => {
                PtySession::spawn_default_shell_in(dimensions, working_directory.clone())
            }
            Self::ShellCommand(command) => PtySession::spawn_shell_command(dimensions, command),
            Self::Exec {
                program,
                args,
                working_directory,
            } => PtySession::spawn_exec(
                dimensions,
                program.clone(),
                args.clone(),
                working_directory.clone(),
            ),
        }
    }

    fn append_process_args(&self, command: &mut Command) {
        match self {
            Self::DefaultShell { working_directory } => {
                if let Some(path) = working_directory {
                    command.arg("--working-directory").arg(path);
                }
            }
            Self::ShellCommand(shell_command) => {
                command.arg("--shell-command").arg(shell_command);
            }
            Self::Exec {
                program,
                args,
                working_directory,
            } => {
                if let Some(path) = working_directory {
                    command.arg("--working-directory").arg(path);
                }
                command.arg("--exec").arg(program);
                for arg in args {
                    command.arg(arg);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub session_id: String,
    pub dimensions: Dimensions,
    pub command: HostCommand,
    pub runtime_base: Option<PathBuf>,
    pub socket_path: Option<PathBuf>,
    pub detached_idle_timeout: Duration,
    pub max_sessions: usize,
    pub max_clients: usize,
    pub snapshot_limits: SnapshotCaptureLimits,
    pub kitty_named_transports: bool,
}

impl HostConfig {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            dimensions: Dimensions::new(DEFAULT_COLUMNS, DEFAULT_ROWS),
            command: HostCommand::default(),
            runtime_base: None,
            socket_path: None,
            detached_idle_timeout: DEFAULT_DETACHED_IDLE_TIMEOUT,
            max_sessions: MAX_HOST_SESSIONS,
            max_clients: DEFAULT_MAX_CLIENTS,
            snapshot_limits: SnapshotCaptureLimits::default(),
            kitty_named_transports: false,
        }
    }

    pub fn runtime_paths(&self) -> Result<RuntimePaths> {
        if let Some(socket_path) = &self.socket_path {
            validate_socket_parent(socket_path)?;
            let lock = socket_path.with_extension("sock.lock");
            let dir = socket_path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("socket path has no parent"))?
                .to_owned();
            return Ok(RuntimePaths {
                dir,
                socket: socket_path.clone(),
                lock,
            });
        }
        runtime_paths(self.runtime_base.as_deref(), &self.session_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostExitReason {
    SessionExited,
    DetachedIdleTimeout,
    /// A client asked the host to terminate via [`ClientFrame::Shutdown`] (the
    /// manager's "kill session"). The shell is SIGHUP'd and reaped, and the
    /// socket + lock are cleaned up through the same teardown the other exit
    /// reasons use, so the session disappears from the registry.
    Killed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostExit {
    pub reason: HostExitReason,
    pub exit_code: Option<i32>,
}

#[derive(Debug)]
pub struct SpawnedHost {
    pub child: Child,
    pub socket_path: PathBuf,
}

pub fn spawn_host_on_demand(config: &HostConfig) -> Result<SpawnedHost> {
    let paths = config.runtime_paths()?;
    if UnixStream::connect(&paths.socket).is_ok() {
        bail!("session-host is already running for {}", config.session_id);
    }
    validate_scrollback_bound(config.snapshot_limits)?;

    let mut command = Command::new(env::current_exe().context("resolve current executable")?);
    command
        .arg("session-host")
        .arg("--session-id")
        .arg(&config.session_id)
        .arg("--socket")
        .arg(&paths.socket)
        .arg("--idle-timeout-ms")
        .arg(config.detached_idle_timeout.as_millis().to_string())
        .arg("--max-scrollback-rows")
        .arg(config.snapshot_limits.max_scrollback_rows.to_string())
        .arg("--cols")
        .arg(config.dimensions.columns.to_string())
        .arg("--rows")
        .arg(config.dimensions.rows.to_string());
    config.command.append_process_args(&mut command);

    let mut child = command.spawn().context("spawn session-host process")?;
    await_host_socket(&mut child, &paths.socket, STARTUP_CONNECT_TIMEOUT)?;
    Ok(SpawnedHost {
        child,
        socket_path: paths.socket,
    })
}

/// Wait for the just-spawned host child's socket to come up. If it never does
/// (audit C26), the child is killed and reaped before the error surfaces —
/// previously the `Child` was dropped on the error path, leaking a live orphan
/// host process (or a zombie, if it had already exited) that nothing would ever
/// `wait()` on. Kill-then-wait is safe on both outcomes: `kill` on an
/// already-exited-but-unreaped child is a no-op error we ignore, and `wait`
/// reaps either way.
pub(crate) fn await_host_socket(
    child: &mut Child,
    socket_path: &std::path::Path,
    timeout: Duration,
) -> Result<()> {
    if let Err(error) = wait_for_socket(socket_path, timeout) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(())
}

pub fn run_internal_host_from_args(args: &[String]) -> Result<()> {
    let mut config = parse_internal_host_args(args)?;
    config.kitty_named_transports = crate::settings::Settings::from_env().kitty_named_transports;
    run_host(config).map(|_| ())
}

/// Unlinks the session socket on ANY exit from [`run_host`] — including the `?`
/// error paths and a panic — so a stale `<id>.sock` never lingers to confuse
/// `resolve_session_socket` into a misleading connect error (audit C-5). The
/// four normal `Ok` exits also unlink explicitly (harmlessly redundant with this
/// guard); this catches the error and panic exits that previously leaked it.
struct SocketUnlinkGuard {
    socket: PathBuf,
}

impl Drop for SocketUnlinkGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub fn run_host(config: HostConfig) -> Result<HostExit> {
    if config.max_sessions == 0 || config.max_sessions > MAX_HOST_SESSIONS {
        bail!(
            "session-host supports exactly one session in this slice, got max_sessions={}",
            config.max_sessions
        );
    }
    if config.max_clients == 0 {
        bail!("session-host max_clients must be nonzero");
    }
    validate_scrollback_bound(config.snapshot_limits)?;

    let paths = config.runtime_paths()?;
    let (listener, _lock) = bind_listener(&paths.socket, &paths.lock)?;
    // C-5: guarantee the socket is unlinked on every exit, including the `?`
    // error paths and a panic, not only the four explicit `Ok` teardowns.
    let _socket_guard = SocketUnlinkGuard {
        socket: paths.socket.clone(),
    };
    let mut terminal = Terminal::new(config.dimensions.columns, config.dimensions.rows);
    terminal.set_kitty_named_transports_enabled(config.kitty_named_transports);
    terminal.set_local_hostname(crate::local_hostname::get());
    terminal.set_scrollback_limit(config.snapshot_limits.max_scrollback_rows);
    let mut session = config.command.spawn(config.dimensions)?;
    // C-1: the raw master fd is a BLOCKING writer. Writing to it inline from the
    // single host loop lets a hosted job that stopped reading its stdin wedge the
    // whole host (no broadcast, no accept, no Shutdown drain). Route every
    // PTY-master write through a bounded writer thread so the loop only enqueues.
    let pty_writer = HostPtyWriter::spawn(session.take_writer()?)
        .context("spawn session-host pty writer thread")?;

    let (pty_tx, pty_rx) = mpsc::sync_channel(PTY_EVENT_QUEUE_CAP);
    spawn_pty_reader(session.try_clone_reader()?, pty_tx)
        .context("spawn session-host pty reader thread")?;

    let (client_tx, client_rx) = mpsc::sync_channel(CLIENT_EVENT_QUEUE_CAP);
    let mut clients = Vec::new();
    let mut next_client_id = 1;
    let mut session_alive = true;
    let mut exit_code = None;
    let mut detached_since = Some(Instant::now());
    // Tracks the first moment a direct `try_wait` observed the child gone, so the
    // PTY-EOF path is given `CHILD_EXIT_DRAIN_GRACE` to deliver final output + its
    // own EOF before the robustness fallback forces shutdown. `None` while the
    // child is still running.
    let mut child_gone_since: Option<Instant> = None;

    loop {
        accept_pending_clients(
            &listener,
            &mut clients,
            &mut next_client_id,
            &client_tx,
            &terminal,
            &config,
        )?;

        drain_pty_events(
            &pty_rx,
            &mut terminal,
            &pty_writer,
            &mut clients,
            &mut session,
            &mut session_alive,
            &mut exit_code,
        )?;

        if !session_alive {
            let _ = std::fs::remove_file(&paths.socket);
            return Ok(HostExit {
                reason: HostExitReason::SessionExited,
                exit_code,
            });
        }

        // Robustness fallback: detect child exit directly, not only via PTY EOF.
        //
        // The normal exit path is `PtyEvent::Eof` from the reader thread (handled
        // in `drain_pty_events` above), which delivers the child's final output in
        // order and then flips `session_alive`. But a PTY-master read does not
        // always observe EOF promptly when the child exits — on macOS/BSD under
        // scheduler pressure, or whenever another process transiently holds a
        // slave fd open, that EOF can lag badly or never arrive, which would hang
        // the reader and any caller awaiting this host. So we also poll the child
        // directly. When it has gone, we still give the reader `CHILD_EXIT_DRAIN
        // _GRACE` to deliver its buffered output + EOF (the in-order path wins and
        // returns via the `!session_alive` branch above on every healthy run, so
        // this fallback stays inert); only if that grace elapses without EOF do we
        // broadcast `SessionExit` ourselves and shut the host down. On Linux EOF
        // is prompt, so the grace never elapses and this is inert → no behavior
        // change there.
        match session.try_wait() {
            Ok(Some(status)) => {
                let since = *child_gone_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= CHILD_EXIT_DRAIN_GRACE {
                    exit_code = status.code();
                    broadcast(
                        &mut clients,
                        &HostFrame::SessionExit {
                            exit_code: status.code(),
                        },
                    );
                    let _ = std::fs::remove_file(&paths.socket);
                    return Ok(HostExit {
                        reason: HostExitReason::SessionExited,
                        exit_code,
                    });
                }
            }
            Ok(None) => child_gone_since = None,
            Err(_) => {}
        }

        let shutdown_requested = drain_client_events(
            &client_rx,
            &mut clients,
            &pty_writer,
            &mut terminal,
            &mut session,
        )?;

        if shutdown_requested {
            // Manager "kill session": SIGHUP + reap the shell, then fall through
            // the same socket-unlink teardown the idle-timeout path uses so the
            // registry row disappears. The lock file is released when `_lock`
            // drops on return.
            let _ = session.kill();
            let status = session.wait().ok();
            let _ = std::fs::remove_file(&paths.socket);
            return Ok(HostExit {
                reason: HostExitReason::Killed,
                exit_code: status.and_then(|status| status.code()),
            });
        }

        if clients.is_empty() {
            let since = *detached_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= config.detached_idle_timeout {
                let _ = session.kill();
                let status = session.wait().ok();
                let _ = std::fs::remove_file(&paths.socket);
                return Ok(HostExit {
                    reason: HostExitReason::DetachedIdleTimeout,
                    exit_code: status.and_then(|status| status.code()),
                });
            }
        } else {
            detached_since = None;
        }

        thread::sleep(HOST_LOOP_SLEEP);
    }
}

fn accept_pending_clients(
    listener: &std::os::unix::net::UnixListener,
    clients: &mut Vec<ClientConnection>,
    next_client_id: &mut u64,
    client_tx: &SyncSender<ClientEvent>,
    terminal: &Terminal,
    config: &HostConfig,
) -> Result<()> {
    loop {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                // Per-connection failures (audit C6): every error out of
                // `handle_attach` — a hello/snapshot write to a peer that died
                // mid-handshake (BrokenPipe), a rejection write, a bounded write
                // timing out, an fd clone failing — concerns ONLY this one
                // connection. Propagating it (the previous `?`) tore down the
                // whole host and killed the session for every attached client;
                // log and drop the single connection instead, and keep serving.
                if let Err(error) = handle_attach(
                    &mut stream,
                    clients,
                    next_client_id,
                    client_tx,
                    terminal,
                    config,
                ) {
                    tracing::warn!("session-host: dropping client after attach failure: {error:#}");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error).context("accept session-host client"),
        }
    }
}

/// A [`Read`] adapter that enforces a single wall-clock deadline across an
/// entire multi-read handshake. Before each underlying read it shrinks the
/// socket's `SO_RCVTIMEO` to the remaining budget, so a peer that dribbles bytes
/// to keep resetting the per-read timeout still hits a hard total cap; once the
/// deadline passes, reads fail immediately.
struct HandshakeDeadlineReader<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
}

impl Read for HandshakeDeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let now = Instant::now();
        if now >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "session-host attach handshake exceeded its total deadline",
            ));
        }
        // Bound this read by the remaining total budget. Best-effort: on a dead
        // peer macOS can reject `SO_RCVTIMEO` with `EINVAL`, in which case the
        // read simply keeps the previously-set per-recv timeout.
        let _ = self.stream.set_read_timeout(Some(self.deadline - now));
        let mut source = self.stream;
        source.read(buf)
    }
}

fn handle_attach(
    stream: &mut UnixStream,
    clients: &mut Vec<ClientConnection>,
    next_client_id: &mut u64,
    client_tx: &SyncSender<ClientEvent>,
    terminal: &Terminal,
    config: &HostConfig,
) -> Result<()> {
    // Configure the just-accepted connection for a bounded-blocking handshake.
    //
    // The listener is nonblocking (set in `bind_listener` so the run loop can poll
    // `accept()`). On macOS/BSD an accept()ed connection INHERITS the listener's
    // `O_NONBLOCK`; on Linux it does not. We clear it and apply bounded
    // read/write deadlines so the handshake has the same blocking-with-deadline
    // semantics on both platforms.
    //
    // CRUCIAL macOS/BSD divergence (the bug this guards): a connection whose peer
    // has ALREADY closed by the time we accept it rejects `SO_RCVTIMEO` /
    // `SO_SNDTIMEO` (i.e. `set_read_timeout` / `set_write_timeout`) with `EINVAL`
    // on macOS, whereas the identical setsockopt SUCCEEDS on Linux. Such dead-peer
    // connections are routine here: `wait_for_socket`, `spawn_host_on_demand`, and
    // `cleanup_stale_socket` all `connect()` a liveness probe and drop it
    // immediately, and that probe is frequently first in the accept backlog ahead
    // of the real client. A probe carries no hello and is useless, so a setup
    // failure must drop ONLY this one connection and let the accept loop keep
    // serving. Propagating it (the previous `?`) tore down the entire host thread,
    // so the next — real — client read EOF mid-hello ("failed to fill whole
    // buffer"). On Linux these calls never fail, so this guard is unreachable
    // there → byte-identical.
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(ATTACH_HANDSHAKE_TIMEOUT))
            .is_err()
        || stream
            .set_write_timeout(Some(ATTACH_WRITE_TIMEOUT))
            .is_err()
    {
        return Ok(());
    }

    // Guard the whole hello read with a single wall-clock deadline. The
    // per-recv `SO_RCVTIMEO` above bounds one `read`, but `read_exact` restarts
    // it on every partial read, so a peer returning one byte per timeout could
    // otherwise keep the handshake — which runs inline in the single host loop —
    // alive forever, freezing broadcast, input, and shutdown for every attached
    // client. The deadline caps the total handshake regardless of drip rate.
    let handshake_deadline = Instant::now() + ATTACH_HANDSHAKE_TIMEOUT;
    let hello_result = {
        let mut guarded = HandshakeDeadlineReader {
            stream,
            deadline: handshake_deadline,
        };
        super::protocol::read_client_hello(&mut guarded)
    };
    let hello = match hello_result {
        Ok(hello) => hello,
        Err(error) => {
            let _ = write_host_hello(
                stream,
                &super::protocol::HostHello::rejected(format!("invalid hello: {error}")),
            );
            return Ok(());
        }
    };

    if clients.len() >= config.max_clients {
        write_host_hello(
            stream,
            &super::protocol::HostHello::rejected("session-host client cap reached"),
        )?;
        return Ok(());
    }
    if hello.session_id != config.session_id {
        write_host_hello(
            stream,
            &super::protocol::HostHello::rejected("session id mismatch"),
        )?;
        return Ok(());
    }
    if !versions_compatible(&hello) {
        write_host_hello(
            stream,
            &super::protocol::HostHello::rejected(format!(
                "incompatible protocol versions: host={} snapshot_format={} snapshot_protocol={}, expected host={} snapshot_format={} snapshot_protocol={}",
                hello.host_protocol_version,
                hello.snapshot_format_version,
                hello.snapshot_protocol_version,
                super::protocol::HOST_PROTOCOL_VERSION,
                SNAPSHOT_FORMAT_VERSION,
                SNAPSHOT_PROTOCOL_VERSION
            )),
        )?;
        return Ok(());
    }

    write_host_hello(stream, &super::protocol::HostHello::accepted())?;
    // Encoding is fallible for externally constructed envelopes with fields
    // exceeding their wire widths; a capture-derived envelope is structurally
    // bounded, so this only propagates on a genuine invariant break.
    let envelope = SnapshotEnvelope::from_terminal(terminal, config.snapshot_limits)
        .encode()
        .context("encode session snapshot")?;
    write_host_frame(stream, &HostFrame::Snapshot(envelope))?;

    // Handshake done. Drop the read deadline now: the per-client reader thread
    // below reads through a `try_clone`d fd that SHARES this socket's
    // `SO_RCVTIMEO`, so if we left the 2s handshake read-timeout in place the
    // reader would surface a spurious timeout error every 2s and the host would
    // detach an attached-but-quiet client (a user who simply is not typing). With
    // it cleared the reader blocks cleanly until the next frame or a clean EOF.
    // The bounded WRITE timeout is deliberately retained so a wedged client can
    // never stall the host's broadcast loop. On a live socket this clear succeeds
    // on macOS and Linux alike; if it ever did not, the reader simply keeps the
    // old bounded behavior, so it is best-effort.
    let _ = stream.set_read_timeout(None);

    let id = *next_client_id;
    *next_client_id += 1;
    // Clone BOTH fds before spawning the reader thread. If the writer-side
    // clone were attempted after the spawn and failed (fd exhaustion is the
    // exact stress this path faces), the `?` would return with the reader
    // thread already running and no ClientConnection entry to evict it
    // through the teardown path, orphaning the thread and its cloned fd.
    // With both clones up front, a clone failure leaves nothing behind.
    let writer = stream.try_clone().context("clone session-host writer")?;
    let reader = stream.try_clone().context("clone session-host client")?;
    admit_client(
        clients,
        id,
        writer,
        spawn_client_reader(id, reader, client_tx.clone()),
    );
    Ok(())
}

/// Final admission step for an accepted client: on a successful reader spawn
/// the connection joins the broadcast set; on a spawn failure ONLY this client
/// is evicted (its socket is shut down so the far side observes a disconnect
/// instead of a silent wedge) and the host keeps serving everyone else. Split
/// from [`handle_attach`] so the failure arm is unit-testable without
/// exhausting real OS threads.
fn admit_client(
    clients: &mut Vec<ClientConnection>,
    id: u64,
    writer: UnixStream,
    spawn_result: io::Result<()>,
) {
    match spawn_result {
        Ok(()) => clients.push(ClientConnection { id, stream: writer }),
        Err(_) => {
            // Both fd clones close: `writer` drops here and the shutdown
            // reaches the reader clone through the shared socket, mirroring
            // the eviction path in `broadcast`.
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// The observable state of the hosted child while reaping it after PTY EOF.
enum ChildWaitState {
    /// The child has been reaped; carries its exit code (may be `None`).
    Exited(Option<i32>),
    /// The child is still running (a process that closed its tty but lingers).
    Alive,
    /// `try_wait` itself failed; treat as terminal so the host stops waiting.
    Errored,
}

/// Poll `probe` until it reports the child gone or `grace` elapses, sleeping
/// `step` between polls. Returns the exit code on exit, or `None` on timeout /
/// error. Split out from [`bounded_child_wait`] so the timeout path is unit
/// testable without a real PTY child.
fn poll_child_exit<F>(mut probe: F, grace: Duration, step: Duration) -> Option<i32>
where
    F: FnMut() -> ChildWaitState,
{
    let deadline = Instant::now() + grace;
    loop {
        match probe() {
            ChildWaitState::Exited(code) => return code,
            ChildWaitState::Errored => return None,
            ChildWaitState::Alive => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(step);
            }
        }
    }
}

/// Reap the hosted child within a bounded grace after PTY-master EOF (audit C-6).
///
/// EOF means all slave fds closed, which is normally the child exiting but can
/// also be a process that closed its controlling tty and lingers. Poll
/// `try_wait` up to [`CHILD_EXIT_DRAIN_GRACE`] rather than blocking forever in
/// `wait`; return the child's exit code if it goes, or `None` on timeout so the
/// host still broadcasts `SessionExit` and tears down instead of wedging.
fn bounded_child_wait(session: &mut PtySession) -> Option<i32> {
    poll_child_exit(
        || match session.try_wait() {
            Ok(Some(status)) => ChildWaitState::Exited(status.code()),
            Ok(None) => ChildWaitState::Alive,
            Err(_) => ChildWaitState::Errored,
        },
        CHILD_EXIT_DRAIN_GRACE,
        HOST_LOOP_SLEEP,
    )
}

fn drain_pty_events(
    pty_rx: &Receiver<PtyEvent>,
    terminal: &mut Terminal,
    pty_writer: &HostPtyWriter,
    clients: &mut Vec<ClientConnection>,
    session: &mut PtySession,
    session_alive: &mut bool,
    exit_code: &mut Option<i32>,
) -> Result<()> {
    let mut processed = 0;
    while let Some(event) = next_pty_event(pty_rx, processed) {
        processed += 1;
        match event {
            PtyEvent::Output(bytes) => {
                terminal.advance(&bytes);
                let host_output = terminal.take_host_output();
                if !host_output.is_empty() {
                    // Non-blocking enqueue (C-1): never park the host loop on the fd.
                    pty_writer.write(&host_output);
                }
                broadcast(clients, &HostFrame::Output(bytes));
                broadcast(
                    clients,
                    &HostFrame::Invalidate {
                        render_revision: terminal.render_revision(),
                    },
                );
            }
            PtyEvent::Eof => {
                if *session_alive {
                    // C-6: master EOF means every slave fd closed, NOT necessarily
                    // that the child exited. A process that closes its controlling
                    // tty and lingers (a daemonizing wrapper) delivers EOF while it
                    // stays alive, so an unbounded `wait()` here would park the whole
                    // host loop forever. Bound the reap: poll `try_wait` up to the
                    // drain grace, then report exit regardless so clients are freed.
                    *exit_code = bounded_child_wait(session);
                    *session_alive = false;
                    broadcast(
                        clients,
                        &HostFrame::SessionExit {
                            exit_code: *exit_code,
                        },
                    );
                }
            }
            PtyEvent::Error(error) => {
                broadcast(clients, &HostFrame::Error(error.to_string()));
                return Err(error).context("read hosted pty");
            }
        }
    }
    Ok(())
}

/// Fetch one event without exceeding the per-loop fairness budget. Kept
/// separate from event handling so the starvation boundary is unit-testable
/// without constructing a live PTY session.
fn next_pty_event(pty_rx: &Receiver<PtyEvent>, processed: usize) -> Option<PtyEvent> {
    if processed >= MAX_PTY_EVENTS_PER_TICK {
        return None;
    }
    pty_rx.try_recv().ok()
}

/// Drain queued client events into the PTY / terminal. Returns `Ok(true)` when a
/// client asked the host to terminate ([`ClientFrame::Shutdown`]), so `run_host`
/// can break its loop and tear down; `Ok(false)` for normal traffic.
fn drain_client_events(
    client_rx: &Receiver<ClientEvent>,
    clients: &mut Vec<ClientConnection>,
    pty_writer: &HostPtyWriter,
    terminal: &mut Terminal,
    session: &mut PtySession,
) -> Result<bool> {
    // Drain the whole pending batch first, then act. Input is applied in order,
    // but resizes are coalesced: only the last resize from a still-attached
    // client is applied per drain, so a peer flooding column-changing resizes
    // (each of which drives a synchronous reflow) costs a single reflow per host
    // loop tick instead of one per frame.
    let mut batch = Vec::new();
    let mut processed = 0;
    while let Some(event) = next_client_event(client_rx, processed) {
        processed += 1;
        batch.push(event);
    }

    let mut shutdown_requested = false;
    for event in &batch {
        match event {
            ClientEvent::Frame(id, ClientFrame::Input(bytes)) => {
                if has_client(clients, *id) {
                    // Non-blocking enqueue (C-1): a client flooding input at a job
                    // that stopped reading stdin can no longer wedge the loop.
                    pty_writer.write(bytes);
                }
            }
            // Resizes are deferred and coalesced after the drain.
            ClientEvent::Frame(_, ClientFrame::Resize { .. }) => {}
            ClientEvent::Frame(_, ClientFrame::Shutdown) => {
                // Whole-session kill. Keep processing so already-queued frames
                // are handled, but flag the loop to tear down after this batch.
                shutdown_requested = true;
            }
            ClientEvent::Frame(id, ClientFrame::Detach)
            | ClientEvent::Disconnected(id)
            | ClientEvent::Error(id) => {
                clients.retain(|client| client.id != *id);
            }
        }
    }

    if let Some(dimensions) = latest_resize_in(&batch, clients) {
        // Clamp happens in `latest_resize_in`, BEFORE the terminal-model resize
        // allocates the grid (the PTY winsize u16 clamp in `session.resize` runs
        // too late to protect it).
        terminal.resize(dimensions.columns, dimensions.rows);
        session.resize(dimensions)?;
        // C-3: broadcast the applied dimensions to EVERY client, not just the one
        // that requested the resize. A bare `Invalidate` only triggers a repaint;
        // the peers' mirrors would keep advancing at their old width against
        // output the host now formats for `dimensions`. `Resized` carries the new
        // grid so each mirror resizes before it repaints (and carries the same
        // render revision an `Invalidate` would, so it doubles as the repaint
        // signal). The requesting client already resized its own mirror locally;
        // its pump guards the echo to a no-op when the dimensions are unchanged.
        broadcast(
            clients,
            &HostFrame::Resized {
                columns: dimensions.columns as u32,
                rows: dimensions.rows as u32,
                render_revision: terminal.render_revision(),
            },
        );
    }
    Ok(shutdown_requested)
}

fn next_client_event(client_rx: &Receiver<ClientEvent>, processed: usize) -> Option<ClientEvent> {
    if processed >= MAX_CLIENT_EVENTS_PER_TICK {
        return None;
    }
    client_rx.try_recv().ok()
}

/// Clamp untrusted wire dimensions to the model bound. The socket is reachable
/// by any same-user process, so raw `u32` columns/rows must be bounded before a
/// resize allocates the grid. Beyond the per-axis bound, the total cell count
/// is clamped to the largest visible grid the default snapshot decoder is
/// guaranteed to accept — a grid past that budget would make every future
/// snapshot of this session undecodable for its own attach/CLI consumers.
/// Rows give way (columns are kept) because a shell reflows to narrow heights
/// far more gracefully than to sub-width columns.
fn clamp_client_dimensions(columns: u32, rows: u32) -> Dimensions {
    let columns = (columns as usize).min(MAX_CLIENT_RESIZE_DIM);
    let rows = (rows as usize).min(MAX_CLIENT_RESIZE_DIM);
    let budget = SnapshotEnvelopeCaps::default().max_self_decodable_visible_cells();
    let rows = match columns.checked_mul(rows) {
        Some(cells) if cells <= budget => rows,
        _ => (budget / columns.max(1)).clamp(1, rows),
    };
    Dimensions::new(columns, rows)
}

/// Select the final resize in a drained batch whose client is still attached,
/// clamped to the model bound. Only this one dimension is applied, collapsing a
/// burst of resize frames into a single reflow.
fn latest_resize_in(batch: &[ClientEvent], clients: &[ClientConnection]) -> Option<Dimensions> {
    batch.iter().rev().find_map(|event| match event {
        ClientEvent::Frame(id, ClientFrame::Resize { columns, rows })
            if has_client(clients, *id) =>
        {
            Some(clamp_client_dimensions(*columns, *rows))
        }
        _ => None,
    })
}

fn broadcast(clients: &mut Vec<ClientConnection>, frame: &HostFrame) {
    clients.retain_mut(|client| {
        if write_host_frame(&mut client.stream, frame).is_ok() {
            return true;
        }
        // A failed write evicts the client, but dropping the write-side clone
        // alone leaves the reader thread's clone open: that thread would spin
        // on its idle-timeout poll forever, orphaning a thread and two fds
        // every time a client is evicted here — repeatable past the client
        // cap. shutdown(Both) closes the shared socket, so the reader
        // observes the disconnect and exits, releasing thread and fds.
        let _ = client.stream.shutdown(std::net::Shutdown::Both);
        false
    });
}

fn has_client(clients: &[ClientConnection], id: u64) -> bool {
    clients.iter().any(|client| client.id == id)
}

/// Spawn the PTY output pump. Fallible: thread creation can fail under
/// resource exhaustion, and a startup spawn failure must surface as a visible
/// host error rather than a panic unwinding the caller.
fn spawn_pty_reader(mut reader: Box<dyn Read + Send>, tx: SyncSender<PtyEvent>) -> io::Result<()> {
    thread::Builder::new()
        .name("odytty-host-pty-reader".to_owned())
        .spawn(move || {
            let mut buffer = [0; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = tx.send(PtyEvent::Eof);
                        break;
                    }
                    Ok(len) => {
                        if tx.send(PtyEvent::Output(buffer[..len].to_vec())).is_err() {
                            break;
                        }
                    }
                    // EINTR is a retry, not an error: a signal delivery mid-read
                    // must not tear down the whole session's output pump.
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        let _ = tx.send(PtyEvent::Error(error));
                        break;
                    }
                }
            }
        })
        .map(|_| ())
}

/// Spawn the per-client frame reader. Fallible: this runs in the host main
/// loop AFTER the handshake accepted the client, so a thread-spawn failure
/// under resource exhaustion must reject only this client — a panic here
/// would take down the hosted shell for every attached client at exactly the
/// moment the machine is under stress.
fn spawn_client_reader(
    id: u64,
    mut stream: UnixStream,
    tx: SyncSender<ClientEvent>,
) -> io::Result<()> {
    thread::Builder::new()
        .name(format!("odytty-host-client-{id}"))
        .spawn(move || {
            // Poll with a bounded read timeout instead of blocking forever. A client
            // that sends a frame header and then withholds the body would otherwise
            // pin this thread and its client slot indefinitely (repeatable to the
            // client cap to wedge every slot). Idle time BETWEEN frames stays
            // legitimate: a timeout with no partial frame is ignored, so a quiet
            // attached client is never detached. Best-effort: if the socket rejects
            // the timeout the reader falls back to blocking (the prior behavior).
            let _ = stream.set_read_timeout(Some(CLIENT_READ_POLL_TIMEOUT));
            let mut reader = ClientFrameReader::default();
            let mut frame_started: Option<Instant> = None;
            loop {
                match reader.read(&mut stream) {
                    Ok(ClientFramePoll::Frame(frame)) => {
                        frame_started = None;
                        // Both Detach and Shutdown are terminal for this reader: the
                        // client is going away (detach) or the whole host is (kill).
                        let last = matches!(frame, ClientFrame::Detach | ClientFrame::Shutdown);
                        if tx.send(ClientEvent::Frame(id, frame)).is_err() || last {
                            break;
                        }
                    }
                    Ok(ClientFramePoll::PartialTimeout) => {
                        // A frame is half-received. Start (or continue) the stall
                        // clock; reclaim the slot if the body stays withheld too long.
                        let started = *frame_started.get_or_insert_with(Instant::now);
                        if started.elapsed() >= CLIENT_FRAME_STALL_DEADLINE {
                            let _ = tx.send(ClientEvent::Error(id));
                            break;
                        }
                    }
                    Ok(ClientFramePoll::IdleTimeout) => {
                        // Idle between frames: keep waiting, do not detach.
                        frame_started = None;
                    }
                    Err(error) if error.is_disconnect() => {
                        let _ = tx.send(ClientEvent::Disconnected(id));
                        break;
                    }
                    Err(_) => {
                        let _ = tx.send(ClientEvent::Error(id));
                        break;
                    }
                }
            }
        })
        .map(|_| ())
}

fn wait_for_socket(socket_path: &std::path::Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(socket_path) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                thread::sleep(HOST_LOOP_SLEEP);
            }
            Err(error) if error.raw_os_error() == Some(libc::ECONNREFUSED) => {
                if Instant::now() >= deadline {
                    return Err(error)
                        .with_context(|| format!("connect {}", socket_path.display()));
                }
                thread::sleep(HOST_LOOP_SLEEP);
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error)
                        .with_context(|| format!("connect {}", socket_path.display()));
                }
                thread::sleep(HOST_LOOP_SLEEP);
            }
        }
    }
}

fn parse_internal_host_args(args: &[String]) -> Result<HostConfig> {
    let mut config = HostConfig::new("default");
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--session-id" => {
                index += 1;
                config.session_id = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("--session-id requires a value"))?
                    .clone();
                index += 1;
            }
            "--socket" => {
                index += 1;
                config.socket_path =
                    Some(PathBuf::from(args.get(index).ok_or_else(|| {
                        anyhow::anyhow!("--socket requires a value")
                    })?));
                index += 1;
            }
            "--runtime-dir" => {
                index += 1;
                config.runtime_base =
                    Some(PathBuf::from(args.get(index).ok_or_else(|| {
                        anyhow::anyhow!("--runtime-dir requires a value")
                    })?));
                index += 1;
            }
            "--idle-timeout-ms" => {
                index += 1;
                let millis = parse_u64(args.get(index), "--idle-timeout-ms")?;
                config.detached_idle_timeout = Duration::from_millis(millis);
                index += 1;
            }
            "--max-scrollback-rows" => {
                index += 1;
                config.snapshot_limits.max_scrollback_rows =
                    parse_usize(args.get(index), "--max-scrollback-rows")?;
                index += 1;
            }
            "--cols" => {
                index += 1;
                let columns = parse_usize(args.get(index), "--cols")?;
                config.dimensions = Dimensions::new(columns, config.dimensions.rows);
                index += 1;
            }
            "--rows" => {
                index += 1;
                let rows = parse_usize(args.get(index), "--rows")?;
                config.dimensions = Dimensions::new(config.dimensions.columns, rows);
                index += 1;
            }
            "--working-directory" => {
                index += 1;
                let path = PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--working-directory requires a value"))?,
                );
                match &mut config.command {
                    HostCommand::DefaultShell { working_directory }
                    | HostCommand::Exec {
                        working_directory, ..
                    } => *working_directory = Some(path),
                    HostCommand::ShellCommand(_) => {
                        bail!("--working-directory is not supported with --shell-command")
                    }
                }
                index += 1;
            }
            "--shell-command" => {
                index += 1;
                config.command = HostCommand::ShellCommand(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--shell-command requires a value"))?
                        .clone(),
                );
                index += 1;
            }
            "--exec" => {
                index += 1;
                let program = OsString::from(
                    args.get(index)
                        .ok_or_else(|| anyhow::anyhow!("--exec requires a program"))?,
                );
                let rest = args[index + 1..].iter().map(OsString::from).collect();
                config.command = HostCommand::Exec {
                    program,
                    args: rest,
                    working_directory: None,
                };
                break;
            }
            value => bail!("unknown session-host argument: {value}"),
        }
    }
    if config.dimensions.columns == 0 || config.dimensions.rows == 0 {
        bail!("session-host dimensions must be nonzero");
    }
    Ok(config)
}

fn validate_scrollback_bound(limits: SnapshotCaptureLimits) -> Result<()> {
    if limits.max_scrollback_rows == 0 {
        bail!("session-host max_scrollback_rows must be nonzero");
    }
    Ok(())
}

fn parse_usize(value: Option<&String>, name: &str) -> Result<usize> {
    let parsed = value
        .ok_or_else(|| anyhow::anyhow!("{name} requires a value"))?
        .parse::<usize>()
        .with_context(|| format!("parse {name}"))?;
    if parsed == 0 {
        bail!("{name} must be nonzero");
    }
    Ok(parsed)
}

fn parse_u64(value: Option<&String>, name: &str) -> Result<u64> {
    value
        .ok_or_else(|| anyhow::anyhow!("{name} requires a value"))?
        .parse::<u64>()
        .with_context(|| format!("parse {name}"))
}

#[derive(Debug)]
struct ClientConnection {
    id: u64,
    stream: UnixStream,
}

#[derive(Debug)]
enum PtyEvent {
    Output(Vec<u8>),
    Eof,
    Error(io::Error),
}

#[derive(Debug)]
enum ClientEvent {
    Frame(u64, ClientFrame),
    Disconnected(u64),
    Error(u64),
}

#[cfg(test)]
mod hardening_tests {
    use super::super::protocol::{HOST_PROTOCOL_MAGIC, read_client_hello};
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    // ---- helpers ----

    /// Build a `ClientConnection` with the given id backed by a live socket pair.
    /// The returned peer end must be kept alive for the connection to stay open.
    fn client_connection(id: u64) -> (ClientConnection, UnixStream) {
        let (peer, near) = UnixStream::pair().expect("socketpair");
        (ClientConnection { id, stream: near }, peer)
    }

    // ---- PTY output queue bounds and loop fairness ----

    #[test]
    fn pty_event_queue_has_a_fixed_capacity() {
        let (tx, _rx) = mpsc::sync_channel(PTY_EVENT_QUEUE_CAP);
        for _ in 0..PTY_EVENT_QUEUE_CAP {
            tx.try_send(PtyEvent::Eof).expect("queue entry fits");
        }
        assert!(matches!(
            tx.try_send(PtyEvent::Eof),
            Err(mpsc::TrySendError::Full(PtyEvent::Eof))
        ));
    }

    #[test]
    fn pty_event_drain_yields_after_the_per_tick_budget() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..=MAX_PTY_EVENTS_PER_TICK {
            tx.send(PtyEvent::Eof).expect("receiver remains live");
        }

        let mut processed = 0;
        while next_pty_event(&rx, processed).is_some() {
            processed += 1;
        }

        assert_eq!(processed, MAX_PTY_EVENTS_PER_TICK);
        assert!(matches!(rx.try_recv(), Ok(PtyEvent::Eof)));
    }

    #[test]
    fn client_event_queue_has_a_fixed_capacity() {
        let (tx, _rx) = mpsc::sync_channel(CLIENT_EVENT_QUEUE_CAP);
        for id in 0..CLIENT_EVENT_QUEUE_CAP as u64 {
            tx.try_send(ClientEvent::Disconnected(id))
                .expect("queue entry fits");
        }
        assert!(matches!(
            tx.try_send(ClientEvent::Disconnected(u64::MAX)),
            Err(mpsc::TrySendError::Full(ClientEvent::Disconnected(
                u64::MAX
            )))
        ));
    }

    #[test]
    fn client_event_drain_yields_after_the_per_tick_budget() {
        let (tx, rx) = mpsc::channel();
        for id in 0..=MAX_CLIENT_EVENTS_PER_TICK as u64 {
            tx.send(ClientEvent::Disconnected(id))
                .expect("receiver remains live");
        }

        let mut processed = 0;
        while next_client_event(&rx, processed).is_some() {
            processed += 1;
        }

        assert_eq!(processed, MAX_CLIENT_EVENTS_PER_TICK);
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientEvent::Disconnected(id)) if id == MAX_CLIENT_EVENTS_PER_TICK as u64
        ));
    }

    // ---- C-5: socket unlink guard ----

    #[test]
    fn socket_unlink_guard_removes_the_socket_on_drop() {
        // The guard must unlink the socket on ANY exit, so a stale <id>.sock never
        // lingers after an error/panic exit to confuse the next connect (C-5).
        let dir = std::env::temp_dir().join(format!(
            "odytty-c5-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let socket = dir.join("session-guard.sock");
        std::fs::write(&socket, b"").expect("create fake socket file");
        assert!(socket.exists(), "precondition: socket file exists");
        {
            let _guard = SocketUnlinkGuard {
                socket: socket.clone(),
            };
        }
        assert!(
            !socket.exists(),
            "SocketUnlinkGuard did not unlink the socket on drop"
        );
        // Dropping a guard for an already-absent socket must not panic.
        {
            let _guard = SocketUnlinkGuard {
                socket: socket.clone(),
            };
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- C-6: bounded child wait on PTY EOF ----

    #[test]
    fn poll_child_exit_returns_the_code_once_the_child_is_gone() {
        // A child that exits after a couple of polls yields its code promptly.
        let mut polls = 0;
        let code = poll_child_exit(
            || {
                polls += 1;
                if polls >= 2 {
                    ChildWaitState::Exited(Some(7))
                } else {
                    ChildWaitState::Alive
                }
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(code, Some(7));
    }

    #[test]
    fn poll_child_exit_times_out_on_a_lingering_child_instead_of_hanging() {
        // A process that closed its controlling tty but stays alive delivers EOF
        // while `try_wait` keeps reporting Alive; the poll must return None within
        // the grace rather than blocking the host loop forever (C-6).
        let start = Instant::now();
        let code = poll_child_exit(
            || ChildWaitState::Alive,
            Duration::from_millis(120),
            Duration::from_millis(5),
        );
        assert_eq!(code, None, "a lingering child must time out to None");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "poll_child_exit exceeded its grace: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn poll_child_exit_treats_a_wait_error_as_terminal() {
        let code = poll_child_exit(
            || ChildWaitState::Errored,
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(code, None);
    }

    // ---- F20: resize coalescing ----

    #[test]
    fn resize_burst_coalesces_to_the_latest_dimensions() {
        let (client, _peer) = client_connection(7);
        let clients = vec![client];
        let batch = vec![
            ClientEvent::Frame(
                7,
                ClientFrame::Resize {
                    columns: 80,
                    rows: 24,
                },
            ),
            ClientEvent::Frame(7, ClientFrame::Input(b"typed".to_vec())),
            ClientEvent::Frame(
                7,
                ClientFrame::Resize {
                    columns: 100,
                    rows: 30,
                },
            ),
            ClientEvent::Frame(
                7,
                ClientFrame::Resize {
                    columns: 120,
                    rows: 40,
                },
            ),
        ];
        // A burst of three resizes collapses to a single applied dimension: the
        // last one from a still-attached client.
        assert_eq!(
            latest_resize_in(&batch, &clients),
            Some(Dimensions::new(120, 40))
        );
    }

    #[test]
    fn resize_from_a_departed_client_is_not_applied() {
        // No attached clients: a resize left in the batch by a client that has
        // since detached must be dropped.
        let clients: Vec<ClientConnection> = Vec::new();
        let batch = vec![ClientEvent::Frame(
            7,
            ClientFrame::Resize {
                columns: 120,
                rows: 40,
            },
        )];
        assert_eq!(latest_resize_in(&batch, &clients), None);
    }

    #[test]
    fn client_resize_dimensions_are_clamped_to_the_model_bound() {
        // Per-axis clamp holds, and the total-cell budget then reduces rows:
        // a 4096 x 4096 request is ~16.7M cells, past what the default
        // snapshot decoder accepts, so accepting it would make every future
        // snapshot of the session undecodable for its own consumers.
        let budget = SnapshotEnvelopeCaps::default().max_self_decodable_visible_cells();
        let clamped = clamp_client_dimensions(u32::MAX, u32::MAX);
        assert_eq!(clamped.columns, MAX_CLIENT_RESIZE_DIM);
        assert!(clamped.rows >= 1);
        assert!(clamped.columns * clamped.rows <= budget);
        // Realistic sizes pass through untouched.
        assert_eq!(clamp_client_dimensions(100, 40), Dimensions::new(100, 40));
        assert_eq!(clamp_client_dimensions(500, 200), Dimensions::new(500, 200));
        // Columns are preserved at the expense of rows.
        let wide = clamp_client_dimensions(4096, 4096);
        assert_eq!(wide.columns, 4096);
        assert_eq!(wide.rows, budget / 4096);
    }

    #[test]
    fn admit_client_pushes_the_connection_on_successful_reader_spawn() {
        let (writer, _peer) = UnixStream::pair().expect("socketpair");
        let mut clients = Vec::new();
        admit_client(&mut clients, 7, writer, Ok(()));
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].id, 7);
    }

    #[test]
    fn admit_client_evicts_only_the_failed_client_on_reader_spawn_error() {
        // A reader-thread spawn failure (resource exhaustion) must reject
        // ONLY the new client: the connection is dropped with a visible
        // shutdown, and every already-attached client keeps its slot. A panic
        // here would kill the hosted shell for everyone at exactly the moment
        // the machine is under stress.
        let (existing_writer, _existing_peer) = UnixStream::pair().expect("socketpair");
        let mut clients = vec![ClientConnection {
            id: 1,
            stream: existing_writer,
        }];
        let (writer, peer) = UnixStream::pair().expect("socketpair");
        // Install the read timeout BEFORE the eviction closes the far end:
        // macOS rejects setsockopt(SO_RCVTIMEO) with EINVAL once the peer has
        // been shutdown(Both) and dropped, while Linux accepts it. Setting it
        // while the pair is still connected is deterministic on both.
        peer.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set peer timeout");
        admit_client(
            &mut clients,
            2,
            writer,
            Err(io::Error::other("thread spawn failed")),
        );
        assert_eq!(clients.len(), 1, "existing client keeps its slot");
        assert_eq!(clients[0].id, 1);
        // The failed client's socket was shut down: the far side observes a
        // clean disconnect instead of a silently wedged attach. Platforms
        // differ on how a shutdown(Both)+drop surfaces to the reader: Linux
        // reports Ok(0) EOF, while macOS/BSD reports ECONNRESET for the same
        // clean teardown. Both count as "disconnect observed"; anything else
        // (data, a timeout, a different error) still fails.
        let mut buffer = [0u8; 8];
        match (&peer).read(&mut buffer) {
            Ok(0) => {}
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
            Ok(n) => panic!("far side must see a disconnect, got {n} bytes"),
            Err(error) => panic!("far side must see a disconnect, got {error}"),
        }
    }

    // ---- F9: total-handshake deadline ----

    #[test]
    fn slow_handshake_hello_is_abandoned_at_the_total_deadline() {
        let (client, server) = UnixStream::pair().expect("socketpair");
        server
            .set_read_timeout(Some(ATTACH_HANDSHAKE_TIMEOUT))
            .expect("set handshake recv timeout");

        // Send only part of the protocol magic, then stall well past the
        // deadline while keeping the connection open (so this tests the deadline,
        // not a clean EOF).
        let writer = thread::spawn(move || {
            let mut client = client;
            let _ = client.write_all(&HOST_PROTOCOL_MAGIC[..2]);
            let _ = client.flush();
            thread::sleep(Duration::from_millis(500));
            drop(client);
        });

        let deadline = Instant::now() + Duration::from_millis(120);
        let mut guarded = HandshakeDeadlineReader {
            stream: &server,
            deadline,
        };
        let start = Instant::now();
        let result = read_client_hello(&mut guarded);
        let elapsed = start.elapsed();

        assert!(result.is_err(), "a stalled hello must be abandoned");
        assert!(
            elapsed < Duration::from_millis(400),
            "handshake must give up near its deadline, not block for the full stall: {elapsed:?}"
        );
        writer.join().expect("writer thread");
    }

    // ---- F19: post-handshake reader timeout ----

    #[test]
    fn withheld_frame_payload_surfaces_as_a_partial_timeout() {
        let (client, server) = UnixStream::pair().expect("socketpair");
        server
            .set_read_timeout(Some(Duration::from_millis(40)))
            .expect("set poll timeout");
        let mut server = server;
        let mut reader = ClientFrameReader::default();

        // (a) No bytes yet: idle between frames must not detach.
        match reader.read(&mut server).expect("idle read") {
            ClientFramePoll::IdleTimeout => {}
            other => panic!("expected IdleTimeout with no data, got {other:?}"),
        }

        // (b) Send a frame header claiming a large payload, then withhold it.
        let mut client = client;
        client.write_all(&[101]).expect("kind"); // Input frame
        client
            .write_all(&1000u32.to_be_bytes())
            .expect("declared length");
        client.flush().expect("flush header");

        let mut saw_partial = false;
        for _ in 0..8 {
            match reader.read(&mut server).expect("partial read") {
                ClientFramePoll::PartialTimeout => {
                    saw_partial = true;
                    break;
                }
                ClientFramePoll::IdleTimeout => {}
                other => panic!("unexpected poll while withheld: {other:?}"),
            }
        }
        assert!(
            saw_partial,
            "a received header with a withheld body must report PartialTimeout"
        );

        // (c) Deliver the body: the frame now completes, proving the reader
        // resumed the partial frame rather than desyncing.
        client.write_all(&vec![b'x'; 1000]).expect("body");
        client.flush().expect("flush body");
        loop {
            match reader.read(&mut server).expect("complete read") {
                ClientFramePoll::Frame(ClientFrame::Input(bytes)) => {
                    assert_eq!(bytes.len(), 1000);
                    break;
                }
                ClientFramePoll::PartialTimeout | ClientFramePoll::IdleTimeout => {}
                other => panic!("unexpected poll while completing: {other:?}"),
            }
        }
    }
}

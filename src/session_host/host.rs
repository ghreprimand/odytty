// SPDX-License-Identifier: GPL-3.0-only
//! PTY-owning session-host loop for the first resumable-session slice.

use std::env;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::core::{
    Dimensions, SNAPSHOT_FORMAT_VERSION, SNAPSHOT_PROTOCOL_VERSION, SnapshotCaptureLimits,
    SnapshotEnvelope, Terminal,
};
use crate::pty::PtySession;

use super::protocol::{
    ClientFrame, HostFrame, versions_compatible, write_host_frame, write_host_hello,
};
use super::socket::{RuntimePaths, bind_listener, runtime_paths, validate_socket_parent};

pub const MAX_HOST_SESSIONS: usize = 1;
const DEFAULT_COLUMNS: usize = 80;
const DEFAULT_ROWS: usize = 24;
const DEFAULT_MAX_CLIENTS: usize = 8;
const DEFAULT_DETACHED_IDLE_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
const HOST_LOOP_SLEEP: Duration = Duration::from_millis(10);
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
const STARTUP_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

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

    let child = command.spawn().context("spawn session-host process")?;
    wait_for_socket(&paths.socket, STARTUP_CONNECT_TIMEOUT)?;
    Ok(SpawnedHost {
        child,
        socket_path: paths.socket,
    })
}

pub fn run_internal_host_from_args(args: &[String]) -> Result<()> {
    let config = parse_internal_host_args(args)?;
    run_host(config).map(|_| ())
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
    let mut terminal = Terminal::new(config.dimensions.columns, config.dimensions.rows);
    terminal.set_scrollback_limit(config.snapshot_limits.max_scrollback_rows);
    let mut session = config.command.spawn(config.dimensions)?;
    let mut pty_writer = session.take_writer()?;

    let (pty_tx, pty_rx) = mpsc::channel();
    spawn_pty_reader(session.try_clone_reader()?, pty_tx);

    let (client_tx, client_rx) = mpsc::channel();
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
            &mut pty_writer,
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

        drain_client_events(
            &client_rx,
            &mut clients,
            &mut pty_writer,
            &mut terminal,
            &mut session,
        )?;

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
    client_tx: &Sender<ClientEvent>,
    terminal: &Terminal,
    config: &HostConfig,
) -> Result<()> {
    loop {
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                handle_attach(
                    &mut stream,
                    clients,
                    next_client_id,
                    client_tx,
                    terminal,
                    config,
                )?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error).context("accept session-host client"),
        }
    }
}

fn handle_attach(
    stream: &mut UnixStream,
    clients: &mut Vec<ClientConnection>,
    next_client_id: &mut u64,
    client_tx: &Sender<ClientEvent>,
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

    let hello = match super::protocol::read_client_hello(stream) {
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
    let envelope = SnapshotEnvelope::from_terminal(terminal, config.snapshot_limits).encode();
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
    let reader = stream.try_clone().context("clone session-host client")?;
    spawn_client_reader(id, reader, client_tx.clone());
    clients.push(ClientConnection {
        id,
        stream: stream.try_clone().context("clone session-host writer")?,
    });
    Ok(())
}

fn drain_pty_events(
    pty_rx: &Receiver<PtyEvent>,
    terminal: &mut Terminal,
    pty_writer: &mut Box<dyn Write + Send>,
    clients: &mut Vec<ClientConnection>,
    session: &mut PtySession,
    session_alive: &mut bool,
    exit_code: &mut Option<i32>,
) -> Result<()> {
    while let Ok(event) = pty_rx.try_recv() {
        match event {
            PtyEvent::Output(bytes) => {
                terminal.advance(&bytes);
                let host_output = terminal.take_host_output();
                if !host_output.is_empty() {
                    pty_writer
                        .write_all(&host_output)
                        .context("write terminal response to hosted pty")?;
                    pty_writer
                        .flush()
                        .context("flush terminal response to hosted pty")?;
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
                    let status = session.wait().context("wait hosted pty child")?;
                    *session_alive = false;
                    *exit_code = status.code();
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

fn drain_client_events(
    client_rx: &Receiver<ClientEvent>,
    clients: &mut Vec<ClientConnection>,
    pty_writer: &mut Box<dyn Write + Send>,
    terminal: &mut Terminal,
    session: &mut PtySession,
) -> Result<()> {
    while let Ok(event) = client_rx.try_recv() {
        match event {
            ClientEvent::Frame(id, ClientFrame::Input(bytes)) => {
                if has_client(clients, id) {
                    pty_writer
                        .write_all(&bytes)
                        .context("write attach-client input to pty")?;
                    pty_writer.flush().context("flush attach-client input")?;
                }
            }
            ClientEvent::Frame(id, ClientFrame::Resize { columns, rows }) => {
                if has_client(clients, id) {
                    let dimensions = Dimensions::new(columns as usize, rows as usize);
                    terminal.resize(dimensions.columns, dimensions.rows);
                    session.resize(dimensions)?;
                    broadcast(
                        clients,
                        &HostFrame::Invalidate {
                            render_revision: terminal.render_revision(),
                        },
                    );
                }
            }
            ClientEvent::Frame(id, ClientFrame::Detach)
            | ClientEvent::Disconnected(id)
            | ClientEvent::Error(id) => {
                clients.retain(|client| client.id != id);
            }
        }
    }
    Ok(())
}

fn broadcast(clients: &mut Vec<ClientConnection>, frame: &HostFrame) {
    clients.retain_mut(|client| write_host_frame(&mut client.stream, frame).is_ok());
}

fn has_client(clients: &[ClientConnection], id: u64) -> bool {
    clients.iter().any(|client| client.id == id)
}

fn spawn_pty_reader(mut reader: Box<dyn Read + Send>, tx: Sender<PtyEvent>) {
    thread::spawn(move || {
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
                Err(error) => {
                    let _ = tx.send(PtyEvent::Error(error));
                    break;
                }
            }
        }
    });
}

fn spawn_client_reader(id: u64, mut stream: UnixStream, tx: Sender<ClientEvent>) {
    thread::spawn(move || {
        loop {
            match super::protocol::read_client_frame(&mut stream) {
                Ok(frame) => {
                    let detach = matches!(frame, ClientFrame::Detach);
                    if tx.send(ClientEvent::Frame(id, frame)).is_err() || detach {
                        break;
                    }
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
    });
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

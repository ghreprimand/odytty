// SPDX-License-Identifier: GPL-3.0-only
//! SSH connect-action substrate.
//!
//! This module builds the only command line OdyTTY needs for SSH: an argv for
//! the system `ssh` binary from name-only connection fields. It never handles
//! passwords, private keys, passphrases, agent sockets, or OpenSSH config file
//! contents; authentication remains entirely delegated to `ssh`.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use crate::connection_hosts::ConnectionHost;
#[cfg(unix)]
use crate::core::Dimensions;
// The detached/resumable SSH path produces a session-host config; that transport
// is Unix-only. The non-detached "SSH in a tab" path (`ssh_command_for_host` →
// local PTY) stays cross-platform and works on Windows via the local ConPTY.
#[cfg(unix)]
use crate::session_host::{HostCommand, HostConfig};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCommand {
    program: OsString,
    args: Vec<OsString>,
}

impl SshCommand {
    pub fn new(program: impl Into<OsString>, args: Vec<OsString>) -> Self {
        Self {
            program: program.into(),
            args,
        }
    }

    pub fn program(&self) -> &OsString {
        &self.program
    }

    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    pub fn into_program_args(self) -> (OsString, Vec<OsString>) {
        (self.program, self.args)
    }

    #[cfg(unix)]
    pub fn into_host_command(self, working_directory: Option<PathBuf>) -> HostCommand {
        HostCommand::Exec {
            program: self.program,
            args: self.args,
            working_directory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshConnectError {
    EmptyField(&'static str),
    InvalidField(&'static str),
}

impl fmt::Display for SshConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(f, "empty SSH connection field: {field}"),
            Self::InvalidField(field) => write!(f, "invalid SSH connection field: {field}"),
        }
    }
}

impl std::error::Error for SshConnectError {}

/// Build the system-ssh argv for a saved connection entry.
///
/// Form:
///
/// - host only: `ssh -- HOST`
/// - user: `ssh -- USER@HOST`
/// - user + port: `ssh -p PORT -- USER@HOST`
///
/// `--` is deliberate: if a saved alias or host starts with `-`, it is still a
/// destination operand rather than another ssh option. No shell is involved.
/// The tri-state outcome of a Test Connection probe (ODP-8). A probe can only
/// verify reachability + *non-interactive* (key/agent) auth — it never has a
/// password and must never store one, so `InteractiveAuth` is the EXPECTED
/// result for a password host, not a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeClass {
    /// TCP + host key + non-interactive auth all succeeded (exit 0). Green.
    AuthOk,
    /// The handshake reached the server, but only interactive auth (password /
    /// keyboard-interactive) is offered, which `BatchMode` refuses. The host is
    /// up and a real connect WILL work — interactively. Amber, not a failure.
    InteractiveAuth,
    /// The host key does not match a stored key — a distinct, security-relevant
    /// failure (possible MITM or a re-keyed host). Red.
    HostKeyMismatch,
    /// Unreachable: connect timeout, connection refused, or DNS failure. Red.
    Unreachable,
}

/// Build the argv for a Test Connection probe (ODP-8): a non-interactive,
/// no-shell, one-shot `exit`. `BatchMode=yes` disables every prompt so the probe
/// can never hang on a password; `-v` surfaces the offered auth methods on
/// stderr for the amber discriminator; `ConnectTimeout=5` bounds the TCP wait.
/// Argv-only with the same `--` destination guard as the connect path; no
/// `ControlPath` is ever added (a probe must not disturb a live master).
pub fn ssh_probe_command_for_host(host: &ConnectionHost) -> Result<SshCommand, SshConnectError> {
    let destination = ssh_destination(host)?;
    let mut args = vec![
        OsString::from("-v"),
        OsString::from("-o"),
        OsString::from("ConnectTimeout=5"),
        OsString::from("-o"),
        OsString::from("BatchMode=yes"),
    ];
    if let Some(port) = host.port {
        args.push(OsString::from("-p"));
        args.push(OsString::from(port.to_string()));
    }
    push_identity_args(&mut args, host);
    args.push(OsString::from("--"));
    args.push(OsString::from(destination));
    args.push(OsString::from("exit"));
    Ok(SshCommand::new("ssh", args))
}

/// Classify a probe from its exit success and captured stderr. `exit_ok` (exit
/// 0) is unambiguous key/agent success. Otherwise the stderr text discriminates
/// a host-key mismatch (distinct, security-relevant) and a reachable-but-
/// interactive-auth host (the handshake happened; a password/keyboard method is
/// offered) from an outright unreachable host. Only OpenSSH's own English
/// diagnostics are matched; nothing credential-shaped is ever read or stored.
pub fn classify_probe(exit_ok: bool, stderr: &str) -> ProbeClass {
    if exit_ok {
        return ProbeClass::AuthOk;
    }
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("host key verification failed")
        || lower.contains("remote host identification has changed")
    {
        return ProbeClass::HostKeyMismatch;
    }
    // The handshake reached auth: the server offered a method BatchMode refuses,
    // or refused the key with a password fallback available. Either way the host
    // is up and a real connect proceeds interactively.
    if lower.contains("authentications that can continue")
        || lower.contains("keyboard-interactive")
        || lower.contains("password")
        || lower.contains("permission denied")
    {
        return ProbeClass::InteractiveAuth;
    }
    ProbeClass::Unreachable
}

/// Push `-i <path>` for a host's `IdentityFile`, when set (ODP-9 Tier 1). The
/// path is a separate argv element (never merged into `-i<path>`) so a path is
/// always read as the identity filename, never an ssh option. An empty path is
/// ignored. OdyTTY stores only the path; no key material is ever handled.
fn push_identity_args(args: &mut Vec<OsString>, host: &ConnectionHost) {
    if let Some(identity) = host
        .identity_file
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        args.push(OsString::from("-i"));
        args.push(OsString::from(identity));
    }
}

pub fn ssh_command_for_host(host: &ConnectionHost) -> Result<SshCommand, SshConnectError> {
    let destination = ssh_destination(host)?;
    let mut args = Vec::new();
    push_identity_args(&mut args, host);
    if let Some(port) = host.port {
        args.push(OsString::from("-p"));
        args.push(OsString::from(port.to_string()));
    }
    args.push(OsString::from("--"));
    args.push(OsString::from(destination));
    Ok(SshCommand::new("ssh", args))
}

/// Idle window (seconds) a shared `ssh` master is kept alive after its last
/// multiplexed session closes (`ControlPersist`). Ten minutes keeps a working
/// session's second-and-later tabs to a host near-instant, while a bounded value
/// means a leaked master is auto-reaped rather than lingering indefinitely.
#[cfg(not(windows))]
const SSH_CONTROL_PERSIST_SECS: &str = "600";

/// Runtime knobs for building a remote `ssh` argv. `integration` injects the
/// shell-integration bootstrap; `reuse` layers `ControlMaster` connection
/// multiplexing on top when a `control_dir` is available. Both are resolved from
/// the global setting and per-host override before the argv is built.
#[derive(Debug, Clone, Default)]
pub struct RemoteSshOptions {
    pub integration: bool,
    pub reuse: bool,
    /// Wrap the remote shell in a persistent `tmux` session (opt-in, default
    /// off). When set, the bootstrap `exec`s `tmux new-session -A -s odytty` so a
    /// dropped-and-reconnected link reattaches the same remote session with its
    /// state intact, degrading to a plain bash session when the remote has no
    /// `tmux`. Requires `integration` (the bootstrap is the only injection
    /// point); ignored when integration is off.
    pub tmux: bool,
    /// Directory OdyTTY owns for `ControlMaster` sockets. `None` disables reuse
    /// (unresolvable state dir, or a Windows client where OpenSSH has no
    /// socket-multiplexing support).
    pub control_dir: Option<PathBuf>,
    /// The `ControlPersist=` token for the reuse master (ODP-9 Tier 2): `no` to
    /// tear the master down with its last connection, or a window in whole
    /// seconds. `None` uses the built-in default window
    /// ([`SSH_CONTROL_PERSIST_SECS`]), so callers that do not resolve the
    /// `remote_persist` knob stay byte-identical to the historical fixed window.
    /// Ignored on a Windows client (the control options are compiled out there).
    pub control_persist: Option<String>,
}

/// Build the system-ssh argv for a saved connection entry, optionally injecting
/// OdyTTY's shell integration on the remote (bash-only, i1).
///
/// This is the i1 entry point retained for direct integration-only callers and
/// its tests; connection reuse is off. See
/// [`ssh_command_for_host_with_options`] for the full knob surface.
pub fn ssh_command_for_host_with_integration(
    host: &ConnectionHost,
    integration_enabled: bool,
) -> Result<SshCommand, SshConnectError> {
    ssh_command_for_host_with_options(
        host,
        &RemoteSshOptions {
            integration: integration_enabled,
            reuse: false,
            tmux: false,
            control_dir: None,
            control_persist: None,
        },
    )
}

/// Build the system-ssh argv for a saved connection entry from resolved remote
/// options.
///
/// When `opts.integration` is `false` the argv is byte-identical to
/// [`ssh_command_for_host`] — no remote command, no PTY-forcing `-t`, no control
/// options, nothing added. This is the exact guarantee callers rely on when
/// integration is turned off globally or opted out for a host.
///
/// When integration is enabled, the form becomes:
///
/// - `ssh -t [CONTROL-OPTS] [-p PORT] -- [USER@]HOST <bootstrap>`
///
/// where `<bootstrap>` is a self-contained POSIX-sh command that materializes
/// the bash integration rcfile from an inline base64 blob into a temporary file
/// and `exec`s an interactive bash pointed at it. The bootstrap is delivery-only
/// for the shared [`crate::shell_integration::bash_integration_rc`] payload;
/// nothing is persisted on the remote (the rcfile self-deletes on first read),
/// and every failure path falls back to a plain login shell so the connection is
/// never broken. Non-bash remote shells silently degrade to a plain session.
///
/// `[CONTROL-OPTS]` (`-o ControlMaster=auto -o ControlPersist=… -o
/// ControlPath=…`) are added only when `opts.reuse` is set and a `control_dir`
/// resolved — so the first tab to a host establishes a shared master and later
/// tabs multiplex over it with no fresh handshake. OpenSSH for Windows has no
/// socket multiplexing, so the control options are compiled out on Windows
/// entirely and reuse is a silent no-op there.
///
/// i1 is bash-only: the client always emits the bash bootstrap, and the remote
/// bootstrap self-selects bash-or-fallback at runtime. Extending detection to
/// zsh/fish is a later increment.
pub fn ssh_command_for_host_with_options(
    host: &ConnectionHost,
    opts: &RemoteSshOptions,
) -> Result<SshCommand, SshConnectError> {
    let destination = ssh_destination(host)?;
    if !opts.integration {
        return ssh_command_for_host(host);
    }
    let mut args = Vec::new();
    // `-t` forces a remote PTY so the injected bash starts interactive.
    args.push(OsString::from("-t"));
    // ControlMaster connection reuse. Compiled out on Windows (OpenSSH there has
    // no ControlMaster/ControlPersist/socket multiplexing), so a Windows client
    // never emits these options even when reuse is requested.
    #[cfg(not(windows))]
    if opts.reuse
        && let Some(dir) = opts.control_dir.as_deref()
    {
        let socket = control_socket_path(dir);
        args.push(OsString::from("-o"));
        args.push(OsString::from("ControlMaster=auto"));
        args.push(OsString::from("-o"));
        let persist = opts
            .control_persist
            .as_deref()
            .unwrap_or(SSH_CONTROL_PERSIST_SECS);
        args.push(OsString::from(format!("ControlPersist={persist}")));
        args.push(OsString::from("-o"));
        let mut control_path = OsString::from("ControlPath=");
        control_path.push(socket.as_os_str());
        args.push(control_path);
    }
    push_identity_args(&mut args, host);
    if let Some(port) = host.port {
        args.push(OsString::from("-p"));
        args.push(OsString::from(port.to_string()));
    }
    args.push(OsString::from("--"));
    args.push(OsString::from(destination));
    args.push(OsString::from(remote_bash_bootstrap(opts.tmux)));
    Ok(SshCommand::new("ssh", args))
}

/// Directory component of the remote temp path image paste-through uploads to.
/// A world-writable sticky dir present on every POSIX host; the file itself is
/// created `0600` with an unguessable random name (see [`remote_upload_target`]).
const REMOTE_UPLOAD_DIR: &str = "/tmp";

/// Compose the remote temp path a pasted image is uploaded to: an unguessable
/// random name under `/tmp` (F6-i7). The random component is drawn from
/// OS-seeded entropy so a local attacker cannot pre-create a symlink at the
/// path; combined with the `0600` create mode (the upload runs `umask 077`),
/// this closes the shared-`/tmp` race. The name uses only `[/tmp/a-z0-9.-]`, so
/// it is safe to single-quote into the remote shell command with no escaping.
pub fn remote_upload_target() -> String {
    format!(
        "{REMOTE_UPLOAD_DIR}/odytty-paste-{}.png",
        random_hex_token()
    )
}

/// A 128-bit hex token from OS-seeded entropy, dependency-free.
///
/// `RandomState` keys are randomized from the OS RNG per construction, so
/// hashing through fresh states yields values a remote or same-host attacker
/// cannot predict — the seed is never exposed. Not cryptographic, but the
/// unguessability requirement here is a `/tmp` filename, backed by `0600`
/// permissions; a stable per-call source of unpredictable bytes is exactly the
/// bar. A per-call `nanos ^ pid` spreads the input so two tokens minted in one
/// process are always distinct.
fn random_hex_token() -> String {
    use std::hash::BuildHasher;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ u64::from(std::process::id());
    let high = std::collections::hash_map::RandomState::new().hash_one(seed);
    let low = std::collections::hash_map::RandomState::new().hash_one(high);
    format!("{high:016x}{low:016x}")
}

/// Build the argv that uploads a pasted image to a remote temp path (F6-i7).
///
/// The upload runs the system `ssh` binary — the same credential-delegating
/// transport as the connect path, never an embedded ssh — with a remote command
/// that creates the file `0600` (`umask 077`) and streams the bytes from the
/// upload process's stdin:
///
/// - `ssh [-o ControlPath=…] [-p PORT] -- DEST "umask 077; cat > '<remote>'"`
///
/// The caller wires the local PNG file to the child's stdin. `cat` (rather than
/// `scp`) is deliberate: it guarantees the `0600` create mode atomically and
/// reuses the exact ssh option shape (and `ControlMaster` socket) of the connect
/// path, so the upload multiplexes over the live session with no second auth.
/// The remote path is an OdyTTY-minted `/tmp/odytty-paste-<hash>.png` (only
/// `[/tmp/a-z0-9.-]`), single-quoted with no escaping needed. On Windows the
/// program is still `ssh` (`ssh.exe`) and no `ControlPath` option is emitted
/// (OpenSSH there has no socket multiplexing), so the upload does its own
/// connect — functional, just not multiplexed.
pub fn remote_upload_command(
    destination: &str,
    port: Option<u16>,
    control_dir: Option<&std::path::Path>,
    remote_path: &str,
) -> SshCommand {
    let remote_command = format!("umask 077; cat > '{remote_path}'");
    SshCommand::new(
        "ssh",
        build_remote_exec_args(destination, port, control_dir, remote_command),
    )
}

/// Build the best-effort cleanup argv that removes uploaded paste files from the
/// remote on tab close/disconnect (F6-i7). `None` when there is nothing to
/// remove. Best-effort by nature: if the link has already dropped the command
/// cannot run, and the file persists until the remote's own `/tmp` reaper
/// removes it — a caveat documented honestly rather than a guaranteed deletion.
pub fn remote_cleanup_command(
    destination: &str,
    port: Option<u16>,
    control_dir: Option<&std::path::Path>,
    paths: &[String],
) -> Option<SshCommand> {
    if paths.is_empty() {
        return None;
    }
    // Each path is an OdyTTY-minted `/tmp/odytty-paste-<hash>.png`, safe to
    // single-quote with no escaping.
    let quoted = paths
        .iter()
        .map(|p| format!("'{p}'"))
        .collect::<Vec<_>>()
        .join(" ");
    let remote_command = format!("rm -f {quoted}");
    Some(SshCommand::new(
        "ssh",
        build_remote_exec_args(destination, port, control_dir, remote_command),
    ))
}

/// Shared argv assembly for a one-shot remote command over the connect path's
/// ssh transport: optional `ControlPath` reuse (Unix only), optional port, then
/// `-- DEST <remote-command>`. No `-t` (these are non-interactive one-shots).
fn build_remote_exec_args(
    destination: &str,
    port: Option<u16>,
    control_dir: Option<&std::path::Path>,
    remote_command: String,
) -> Vec<OsString> {
    let mut args = Vec::new();
    // Multiplex over the live master when one is available; compiled out on
    // Windows (OpenSSH there has no socket multiplexing), mirroring the connect
    // builder so a Windows client never emits a control option.
    #[cfg(not(windows))]
    if let Some(dir) = control_dir {
        let socket = control_socket_path(dir);
        args.push(OsString::from("-o"));
        let mut control_path = OsString::from("ControlPath=");
        control_path.push(socket.as_os_str());
        args.push(control_path);
    }
    #[cfg(windows)]
    let _ = control_dir;
    if let Some(port) = port {
        args.push(OsString::from("-p"));
        args.push(OsString::from(port.to_string()));
    }
    args.push(OsString::from("--"));
    args.push(OsString::from(destination));
    args.push(OsString::from(remote_command));
    args
}

/// Resolve whether remote integration is active for a host: an explicit per-host
/// setting wins, otherwise the global default applies.
pub fn remote_integration_enabled(host_integration: Option<bool>, global_default: bool) -> bool {
    host_integration.unwrap_or(global_default)
}

/// Resolve whether ControlMaster connection reuse is active for a host: an
/// explicit per-host setting wins, otherwise the global default applies.
pub fn remote_reuse_enabled(host_reuse: Option<bool>, global_default: bool) -> bool {
    host_reuse.unwrap_or(global_default)
}

/// Resolve whether tmux persistence is active for a host: an explicit per-host
/// setting wins, otherwise the global default applies.
pub fn remote_tmux_enabled(host_tmux: Option<bool>, global_default: bool) -> bool {
    host_tmux.unwrap_or(global_default)
}

/// Parse a `ControlPersist` value (a `remote_persist` preset or a per-host
/// `Persist` override) into the canonical token emitted after `ControlPersist=`:
/// `no` for an off token (the master dies with its last connection), otherwise
/// the window in whole seconds. Accepts an off token (`off`/`0`/`no`/`none`),
/// bare seconds (`600`), and `<n>h`/`<n>m`/`<n>s` combinations (`2h`, `1h30m`,
/// `90s`). Returns `None` for an unrecognized value so the caller can fall back
/// to the global default. Pure string handling — no shell, no I/O; the value is
/// only ever placed as a single `-o ControlPersist=<token>` argv element.
pub fn parse_control_persist(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if matches!(trimmed.as_str(), "off" | "0" | "no" | "none" | "false") {
        return Some("no".to_owned());
    }
    parse_duration_secs(&trimmed).map(|secs| secs.to_string())
}

/// Parse a duration into whole seconds. Accepts bare seconds (`600`) or a
/// sequence of `<n>h`/`<n>m`/`<n>s` units (`2h`, `1h30m`, `90s`). Returns `None`
/// for an empty string, a bad unit, overflow, or trailing digits without a unit.
fn parse_duration_secs(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Some(secs);
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut saw_unit = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        let value: u64 = num.parse().ok()?;
        num.clear();
        let mult: u64 = match ch {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        total = total.checked_add(value.checked_mul(mult)?)?;
        saw_unit = true;
    }
    // Trailing digits with no unit (e.g. "1h30") are rejected as ambiguous.
    if !num.is_empty() {
        return None;
    }
    saw_unit.then_some(total)
}

/// The `ControlMaster` socket template under OdyTTY's owned control dir.
///
/// `%C` is expanded by OpenSSH to a hash over its effective connection identity
/// (`%l%h%p%r`: local host, remote host, port, and remote user). Delegating the
/// key to OpenSSH is load-bearing: hashing only OdyTTY's textual `user@host`
/// destination would alias profiles that use different ports or routing config.
/// The fixed-width expansion also keeps the path under the platform `sun_path`
/// limit (104 bytes on macOS/BSD, 108 on Linux) without exposing the destination.
#[cfg(not(windows))]
fn control_socket_path(control_dir: &std::path::Path) -> PathBuf {
    control_dir.join("ssh-%C")
}

/// Create OdyTTY's `ControlMaster` socket directory with owner-only `0700`
/// permissions, so the multiplexing sockets are never group/world accessible.
/// Idempotent; tightens permissions on an existing same-owner dir too.
///
/// The final `ssh` leaf is handled without following symlinks: `mkdir` never
/// resolves a final-component symlink, the leaf is then validated via
/// `symlink_metadata` (real directory, owned by the effective UID), and any
/// permission repair happens through an `O_NOFOLLOW | O_DIRECTORY` handle with
/// `fchmod` semantics — so a planted symlink, a foreign-owned directory, or a
/// non-directory object at the leaf fails closed and its target is never
/// touched. Parent components keep the prior `create_dir_all` behavior; the
/// state parent is prepared and validated separately by the caller.
///
/// Unix-only: Windows has no `ControlMaster` multiplexing surface, so this
/// function has no Windows counterpart and the Windows `ssh_control_dir` path
/// always returns `None` without emitting control options.
#[cfg(unix)]
pub fn ensure_control_dir(control_dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    if let Some(parent) = control_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Non-recursive mkdir for the leaf: a pre-existing final-component symlink
    // (dangling or not) makes this fail with `AlreadyExists` instead of being
    // resolved, and the validation below then rejects it.
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(control_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }

    // SAFETY: `geteuid` takes no arguments, reads no memory, and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let metadata = std::fs::symlink_metadata(control_dir)?;
    if let Some(reason) = control_leaf_rejection(
        metadata.file_type().is_symlink(),
        metadata.file_type().is_dir(),
        metadata.uid(),
        euid,
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("ssh control dir rejected: {reason}"),
        ));
    }
    if metadata.mode() & 0o777 == 0o700 {
        return Ok(());
    }

    // Repair a broad mode through an opened handle, never a pathname chmod:
    // `O_NOFOLLOW` refuses a symlink swapped in after the stat above, and the
    // handle is re-validated before `File::set_permissions` (fchmod) so the
    // mode change can only ever land on the directory inode itself.
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(control_dir)?;
    let handle_metadata = handle.metadata()?;
    if let Some(reason) = control_leaf_rejection(
        false,
        handle_metadata.file_type().is_dir(),
        handle_metadata.uid(),
        euid,
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("ssh control dir rejected: {reason}"),
        ));
    }
    handle.set_permissions(std::fs::Permissions::from_mode(0o700))
}

/// The fail-closed leaf policy for [`ensure_control_dir`], separated from I/O
/// so the reject cases (symlink, non-directory, foreign owner) are directly
/// testable without fabricating filesystem objects owned by other users.
#[cfg(unix)]
fn control_leaf_rejection(
    is_symlink: bool,
    is_dir: bool,
    owner_uid: u32,
    euid: u32,
) -> Option<&'static str> {
    if is_symlink {
        return Some("path is a symlink");
    }
    if !is_dir {
        return Some("path is not a directory");
    }
    if owner_uid != euid {
        return Some("directory is owned by another user");
    }
    None
}

/// The default tab title for an SSH connection: `user@host` when a user is
/// known, otherwise the bare host. Unambiguous across identical aliases. An
/// explicit `host.title` override, when present, takes precedence at the call
/// site.
pub fn ssh_tab_title(host: &ConnectionHost) -> String {
    let target = host
        .host_name
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(host.alias.as_str());
    match host.user.as_deref() {
        Some(user) if !user.is_empty() => format!("{user}@{target}"),
        _ => target.to_owned(),
    }
}

/// The remote bash rcfile body: a self-delete prefix (so nothing is left on the
/// remote once bash has read it) followed by the shared integration payload.
/// Sharing [`crate::shell_integration::bash_integration_rc`] keeps the remote
/// and local integration byte-identical apart from the cleanup line.
fn remote_bash_rc() -> String {
    format!(
        "rm -f \"${{BASH_SOURCE[0]}}\" 2>/dev/null\n{}",
        crate::shell_integration::bash_integration_rc()
    )
}

/// Build the POSIX-sh bootstrap passed as the remote command.
///
/// Authored, inspectable text plus one base64 blob of [`remote_bash_rc`]. It
/// carries no local paths, usernames, or hostnames — only literal `$HOME`/
/// `$SHELL` shell variables that expand on the remote. Every step is guarded and
/// the command always terminates in an unconditional `exec`, so a shell always
/// lands even when bash, `base64`, or `mktemp` are missing.
fn remote_bash_bootstrap(tmux: bool) -> String {
    let blob = base64_encode(remote_bash_rc().as_bytes());
    // The interactive launch, once the rcfile is materialized. With tmux off the
    // bootstrap `exec`s bash directly (the i1 path). With tmux on it `exec`s a
    // persistent `tmux new-session -A -s odytty` (create-or-attach) running the
    // integrated bash, so a dropped-and-reconnected link reattaches the same
    // remote session — degrading to plain bash when the remote has no `tmux`, so
    // opting in never yields a broken session. `new -A` applies the command only
    // on CREATE; a reattach keeps the original shell (the integration is already
    // live), which also means the freshly written rcfile is only consumed on
    // create — a reattach leaves that small temp file for the OS to reap.
    let launch = if tmux {
        "if command -v tmux >/dev/null 2>&1; then \
         exec tmux new-session -A -s odytty bash --rcfile \"$__odytty_rc\" -i; \
         else exec bash --rcfile \"$__odytty_rc\" -i; fi"
    } else {
        "exec bash --rcfile \"$__odytty_rc\" -i"
    };
    format!(
        "ODYTTY_RC='{blob}'\n\
         if command -v bash >/dev/null 2>&1 && command -v base64 >/dev/null 2>&1; then\n  \
         __odytty_rc=\"$(mktemp 2>/dev/null || printf '%s' \"/tmp/.odytty-rc.$$\")\" && \
         printf '%s' \"$ODYTTY_RC\" | base64 -d > \"$__odytty_rc\" 2>/dev/null && \
         {{ {launch} ; }} || exec \"${{SHELL:-/bin/sh}}\" -l\n\
         fi\n\
         exec \"${{SHELL:-/bin/sh}}\" -l\n"
    )
}

/// Standard RFC 4648 base64 with padding.
///
/// Hand-rolled to keep the SSH-command construction path dependency-free; the
/// remote decodes it with the `base64` coreutil. Verified against the RFC 4648
/// test vectors in this module's tests.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(ALPHABET[((triple >> 18) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((triple >> 12) & 0x3f) as usize]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[((triple >> 6) & 0x3f) as usize])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[(triple & 0x3f) as usize])
        } else {
            '='
        });
    }
    out
}

/// Build a detached session-host config whose child is the system `ssh` binary.
///
/// This is the Phase-2 pairing point: an SSH connection is a normal hosted
/// child command, so reattach and bounded scrollback use the same local
/// session-host protocol as shell sessions. Unix-only (the session-host
/// transport is `#[cfg(unix)]`).
#[cfg(unix)]
pub fn detached_ssh_host_config(
    session_id: impl Into<String>,
    host: &ConnectionHost,
    runtime_base: Option<PathBuf>,
    dimensions: Dimensions,
) -> Result<HostConfig, SshConnectError> {
    let command = ssh_command_for_host(host)?;
    let mut config = HostConfig::new(session_id);
    config.runtime_base = runtime_base;
    config.dimensions = dimensions;
    config.command = command.into_host_command(None);
    Ok(config)
}

/// The `ssh` destination operand for a host: `USER@HOST` when a user is known,
/// else the bare host. Validated to reject credential-like or shell-unsafe
/// fields. Exposed so the connect path can capture the destination for the
/// image paste-through upload descriptor (F6-i7) without rebuilding the argv.
pub fn ssh_destination(host: &ConnectionHost) -> Result<String, SshConnectError> {
    let target = host
        .host_name
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(host.alias.as_str());
    validate_target_field("host", target)?;

    match host.user.as_deref() {
        Some(user) if !user.is_empty() => {
            validate_user_field(user)?;
            Ok(format!("{user}@{target}"))
        }
        _ => Ok(target.to_owned()),
    }
}

fn validate_user_field(value: &str) -> Result<(), SshConnectError> {
    validate_target_field("user", value)?;
    if value.contains('@') {
        return Err(SshConnectError::InvalidField("user"));
    }
    Ok(())
}

fn validate_target_field(field: &'static str, value: &str) -> Result<(), SshConnectError> {
    if value.is_empty() {
        return Err(SshConnectError::EmptyField(field));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
        || value.contains('@')
        || value.contains('\0')
    {
        return Err(SshConnectError::InvalidField(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_hosts::{ConnectionHost, ConnectionHostSource};

    fn host(alias: &str) -> ConnectionHost {
        ConnectionHost {
            alias: alias.to_owned(),
            host_name: None,
            user: None,
            port: None,
            theme: None,
            font: None,
            title: None,
            integration: None,
            reuse: None,
            tmux: None,
            protocol: None,
            identity_file: None,
            persist: None,
            source: ConnectionHostSource::Odytty,
        }
    }

    fn argv(command: &SshCommand) -> Vec<String> {
        std::iter::once(command.program().to_string_lossy().into_owned())
            .chain(
                command
                    .args()
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            )
            .collect()
    }

    fn bootstrap_arg(command: &SshCommand) -> String {
        command
            .args()
            .last()
            .expect("bootstrap arg")
            .to_string_lossy()
            .into_owned()
    }

    fn remote_host(alias: &str, user: &str, host_name: &str) -> ConnectionHost {
        let mut entry = host(alias);
        entry.user = Some(user.to_owned());
        entry.host_name = Some(host_name.to_owned());
        entry
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn per_host_integration_overrides_global_default() {
        assert!(remote_integration_enabled(None, true));
        assert!(!remote_integration_enabled(None, false));
        assert!(!remote_integration_enabled(Some(false), true));
        assert!(remote_integration_enabled(Some(true), false));
    }

    #[test]
    fn tab_title_prefers_user_at_host() {
        assert_eq!(
            ssh_tab_title(&remote_host("web1", "deploy", "web1.example.invalid")),
            "deploy@web1.example.invalid"
        );
        let mut no_user = host("web1");
        no_user.host_name = Some("web1.example.invalid".to_owned());
        assert_eq!(ssh_tab_title(&no_user), "web1.example.invalid");
        assert_eq!(ssh_tab_title(&host("bare")), "bare");
    }

    #[test]
    fn integration_off_argv_is_byte_identical_to_plain_connect() {
        let mut entry = remote_host("web1", "deploy", "web1.example.invalid");
        entry.port = Some(2222);
        let plain = ssh_command_for_host(&entry).expect("plain");
        let off = ssh_command_for_host_with_integration(&entry, false).expect("off");
        assert_eq!(plain, off);
        // Requesting reuse can never resurrect control options on the
        // integration-off path — it stays byte-identical to a plain connect.
        let off_but_reuse = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: false,
                reuse: true,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
                control_persist: None,
            },
        )
        .expect("off+reuse");
        assert_eq!(plain, off_but_reuse);
    }

    #[test]
    fn per_host_reuse_overrides_global_default() {
        assert!(remote_reuse_enabled(None, true));
        assert!(!remote_reuse_enabled(None, false));
        assert!(!remote_reuse_enabled(Some(false), true));
        assert!(remote_reuse_enabled(Some(true), false));
    }

    #[test]
    fn per_host_tmux_overrides_global_default() {
        assert!(remote_tmux_enabled(None, true));
        assert!(!remote_tmux_enabled(None, false));
        assert!(!remote_tmux_enabled(Some(false), true));
        assert!(remote_tmux_enabled(Some(true), false));
    }

    #[test]
    fn identity_file_adds_dash_i_to_the_connect_argv() {
        // ODP-9 Tier 1: a set IdentityFile puts `-i <path>` in the plain connect
        // argv, as a separate argv element before the destination operand.
        let mut h = host("k");
        h.identity_file = Some("/home/user/.ssh/id_ed25519.example".to_owned());
        let args = argv(&ssh_command_for_host(&h).expect("cmd"));
        let i = args.iter().position(|a| a == "-i").expect("-i present");
        assert_eq!(args[i + 1], "/home/user/.ssh/id_ed25519.example");
        // `-i` and its path come before the `--` destination separator.
        let sep = args.iter().position(|a| a == "--").expect("--");
        assert!(i + 1 < sep, "identity args precede the destination operand");
    }

    #[test]
    fn no_identity_file_emits_no_dash_i() {
        // Byte-identical to before when no IdentityFile is set.
        let args = argv(&ssh_command_for_host(&host("k")).expect("cmd"));
        assert!(
            !args.iter().any(|a| a == "-i"),
            "no -i without IdentityFile"
        );
    }

    #[test]
    fn probe_argv_is_batchmode_noninteractive_and_argv_only() {
        // ODP-8: the probe is a no-shell one-shot `exit` with BatchMode + a
        // bounded ConnectTimeout, argv-only, guarded by `--`.
        let mut h = remote_host("k", "deploy", "k.example.invalid");
        h.port = Some(2222);
        h.identity_file = Some("/home/user/.ssh/id_ed25519.example".to_owned());
        let args = argv(&ssh_probe_command_for_host(&h).expect("probe"));
        assert!(args.iter().any(|a| a == "BatchMode=yes"));
        assert!(args.iter().any(|a| a == "ConnectTimeout=5"));
        // Port, identity, and destination are all present, destination after --.
        let p = args.iter().position(|a| a == "-p").expect("-p");
        assert_eq!(args[p + 1], "2222");
        let i = args.iter().position(|a| a == "-i").expect("-i");
        assert_eq!(args[i + 1], "/home/user/.ssh/id_ed25519.example");
        let sep = args.iter().position(|a| a == "--").expect("--");
        assert_eq!(args[sep + 1], "deploy@k.example.invalid");
        // The remote command is a bare `exit`; no `-t`, no ControlPath.
        assert_eq!(args.last().map(String::as_str), Some("exit"));
        assert!(!args.iter().any(|a| a == "-t"));
        assert!(!args.iter().any(|a| a.starts_with("ControlPath")));
    }

    #[test]
    fn classify_probe_maps_the_three_states_and_hostkey() {
        // Exit 0 = key/agent auth OK.
        assert_eq!(classify_probe(true, ""), ProbeClass::AuthOk);
        // A host-key change is a distinct, security-relevant failure.
        assert_eq!(
            classify_probe(
                false,
                "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@"
            ),
            ProbeClass::HostKeyMismatch
        );
        assert_eq!(
            classify_probe(false, "Host key verification failed."),
            ProbeClass::HostKeyMismatch
        );
        // Reachable but interactive auth (the expected password-host state).
        assert_eq!(
            classify_probe(
                false,
                "debug1: Authentications that can continue: publickey,password"
            ),
            ProbeClass::InteractiveAuth
        );
        assert_eq!(
            classify_probe(
                false,
                "deploy@host: Permission denied (publickey,password)."
            ),
            ProbeClass::InteractiveAuth
        );
        // Unreachable classes.
        assert_eq!(
            classify_probe(false, "ssh: connect to host x port 22: Connection refused"),
            ProbeClass::Unreachable
        );
        assert_eq!(
            classify_probe(
                false,
                "ssh: Could not resolve hostname x: Name or service not known"
            ),
            ProbeClass::Unreachable
        );
    }

    #[test]
    fn identity_file_adds_dash_i_to_the_integration_argv() {
        // The integration (bootstrap) argv also carries `-i <path>` when set.
        let mut h = remote_host("k", "deploy", "k.example.invalid");
        h.identity_file = Some("/home/user/.ssh/id_ed25519.example".to_owned());
        let command = ssh_command_for_host_with_integration(&h, true).expect("cmd");
        let args = argv(&command);
        let i = args.iter().position(|a| a == "-i").expect("-i present");
        assert_eq!(args[i + 1], "/home/user/.ssh/id_ed25519.example");
        let sep = args.iter().position(|a| a == "--").expect("--");
        assert!(
            i + 1 < sep,
            "identity precedes destination in the bootstrap argv"
        );
    }

    #[test]
    fn remote_upload_target_is_random_and_well_formed() {
        let a = remote_upload_target();
        let b = remote_upload_target();
        assert!(a.starts_with("/tmp/odytty-paste-"));
        assert!(a.ends_with(".png"));
        // Unguessability requires two mints to differ.
        assert_ne!(a, b);
        // Only hex in the random stem, so the whole path is safe to single-quote
        // into a remote shell command with no escaping.
        let stem = a
            .trim_start_matches("/tmp/odytty-paste-")
            .trim_end_matches(".png");
        assert!(!stem.is_empty());
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[cfg(not(windows))]
    #[test]
    fn remote_upload_command_streams_over_ssh_reusing_the_master() {
        let dir = std::path::Path::new("/run/user/1000/odytty/ssh");
        let command = remote_upload_command(
            "deploy@web1.example.invalid",
            Some(2222),
            Some(dir),
            "/tmp/odytty-paste-abc.png",
        );
        assert_eq!(command.program(), &OsString::from("ssh"));
        let args = argv(&command);
        // Multiplex over the live master, port before the destination operand,
        // destination after `--`.
        assert!(
            args.iter()
                .any(|a| a == "ControlPath=/run/user/1000/odytty/ssh/ssh-%C")
        );
        let p = args.iter().position(|a| a == "-p").expect("-p");
        assert_eq!(args[p + 1], "2222");
        let sep = args.iter().position(|a| a == "--").expect("--");
        let dest = args
            .iter()
            .position(|a| a == "deploy@web1.example.invalid")
            .expect("dest");
        assert!(sep < dest);
        // The remote command creates the file 0600 and streams stdin into it.
        assert_eq!(
            args.last().map(String::as_str),
            Some("umask 077; cat > '/tmp/odytty-paste-abc.png'")
        );
        // A one-shot, not an interactive shell: no PTY-forcing `-t`.
        assert!(!args.iter().any(|a| a == "-t"));
    }

    #[test]
    fn remote_upload_command_without_a_control_dir_emits_no_control_option() {
        let command = remote_upload_command(
            "host.example.invalid",
            None,
            None,
            "/tmp/odytty-paste-x.png",
        );
        let joined = argv(&command).join(" ");
        assert!(!joined.contains("ControlPath"));
        assert!(joined.contains("umask 077; cat > '/tmp/odytty-paste-x.png'"));
        assert!(joined.starts_with("ssh -- host.example.invalid"));
    }

    #[test]
    fn remote_cleanup_command_is_none_when_nothing_was_uploaded() {
        assert!(remote_cleanup_command("host.example.invalid", None, None, &[]).is_none());
    }

    #[test]
    fn remote_cleanup_command_removes_each_uploaded_path() {
        let paths = vec![
            "/tmp/odytty-paste-a.png".to_owned(),
            "/tmp/odytty-paste-b.png".to_owned(),
        ];
        let command =
            remote_cleanup_command("host.example.invalid", None, None, &paths).expect("cmd");
        assert_eq!(
            argv(&command).last().map(String::as_str),
            Some("rm -f '/tmp/odytty-paste-a.png' '/tmp/odytty-paste-b.png'")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_remote_upload_command_emits_no_control_path() {
        let command = remote_upload_command(
            "deploy@web1.example.invalid",
            None,
            Some(std::path::Path::new("C:/odytty/ssh")),
            "/tmp/odytty-paste-abc.png",
        );
        assert_eq!(command.program(), &OsString::from("ssh"));
        let joined = argv(&command).join(" ");
        assert!(!joined.contains("ControlPath"));
        assert!(joined.contains("umask 077; cat"));
    }

    fn tmux_bootstrap(entry: &ConnectionHost, tmux: bool) -> String {
        let command = ssh_command_for_host_with_options(
            entry,
            &RemoteSshOptions {
                integration: true,
                reuse: false,
                tmux,
                control_dir: None,
                control_persist: None,
            },
        )
        .expect("argv");
        bootstrap_arg(&command)
    }

    #[test]
    fn tmux_off_bootstrap_execs_bash_without_any_tmux() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let bootstrap = tmux_bootstrap(&entry, false);
        assert!(bootstrap.contains("exec bash --rcfile"));
        assert!(!bootstrap.contains("tmux"));
    }

    #[test]
    fn tmux_on_bootstrap_nests_bash_inside_a_persistent_session_and_degrades() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let bootstrap = tmux_bootstrap(&entry, true);
        // Create-or-attach a persistent `odytty` session running the integrated
        // bash, so a reconnect reattaches the same remote state.
        assert!(bootstrap.contains("exec tmux new-session -A -s odytty bash --rcfile"));
        // Degrade path: a runtime `command -v tmux` guard falls back to plain
        // bash when the remote has no tmux, so opting in never breaks a session.
        assert!(bootstrap.contains("command -v tmux"));
        assert!(bootstrap.contains("else exec bash --rcfile"));
        // The unconditional plain-shell fallback (missing bash/base64) is intact.
        assert!(
            bootstrap
                .trim_end()
                .ends_with("exec \"${SHELL:-/bin/sh}\" -l")
        );
    }

    #[test]
    fn integration_off_argv_ignores_tmux_and_stays_byte_identical() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let plain = ssh_command_for_host(&entry).expect("plain");
        // tmux is a bootstrap-only wrap, so with integration off (no bootstrap)
        // it can never alter the argv — it stays byte-identical to a plain ssh.
        let off_but_tmux = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: false,
                reuse: false,
                tmux: true,
                control_dir: None,
                control_persist: None,
            },
        )
        .expect("off+tmux");
        assert_eq!(plain, off_but_tmux);
    }

    #[cfg(not(windows))]
    #[test]
    fn reuse_adds_control_options_after_pty_and_before_destination() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let command = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: true,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
                control_persist: None,
            },
        )
        .expect("argv");
        let args = argv(&command);
        let t_pos = args.iter().position(|a| a == "-t").expect("-t");
        let cm = args
            .iter()
            .position(|a| a == "ControlMaster=auto")
            .expect("ControlMaster");
        let sep = args.iter().position(|a| a == "--").expect("--");
        // Control options sit after `-t` and before the destination separator.
        assert!(t_pos < cm && cm < sep);
        assert!(args.iter().any(|a| a == "ControlPersist=600"));
        assert!(
            args.iter()
                .any(|a| a == "ControlPath=/run/user/1000/odytty/ssh/ssh-%C")
        );
        // Exactly three `-o` flags for the three control options.
        assert_eq!(args.iter().filter(|a| a.as_str() == "-o").count(), 3);
    }

    #[cfg(not(windows))]
    #[test]
    fn reuse_without_control_dir_emits_no_control_options() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let command = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: true,
                tmux: false,
                control_dir: None,
                control_persist: None,
            },
        )
        .expect("argv");
        let joined = argv(&command).join(" ");
        assert!(!joined.contains("ControlMaster"));
        assert!(joined.contains("-t"));
    }

    #[cfg(not(windows))]
    #[test]
    fn reuse_off_emits_no_control_options() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let command = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: false,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
                control_persist: None,
            },
        )
        .expect("argv");
        assert!(!argv(&command).join(" ").contains("ControlMaster"));
    }

    // ODP-9 Tier 2: the `remote_persist` knob replaces the historically-fixed
    // ControlPersist window. The default resolves to `None` here, which MUST keep
    // emitting the historical `ControlPersist=600` so the argv is byte-identical
    // to before the knob (the non-regression gate the ruling called out).
    #[cfg(not(windows))]
    #[test]
    fn default_persist_is_byte_identical_to_the_fixed_600s_window() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let with_default = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: true,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
                control_persist: None,
            },
        )
        .expect("argv");
        // Resolving the default `10m` preset to its token is the same string.
        let with_ten_min = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: true,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
                control_persist: Some("600".to_owned()),
            },
        )
        .expect("argv");
        assert!(
            argv(&with_default)
                .iter()
                .any(|a| a == "ControlPersist=600")
        );
        assert_eq!(argv(&with_default), argv(&with_ten_min));
    }

    #[cfg(not(windows))]
    #[test]
    fn explicit_persist_token_replaces_the_window_exactly_once() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        for (token, expected) in [("no", "ControlPersist=no"), ("7200", "ControlPersist=7200")] {
            let command = ssh_command_for_host_with_options(
                &entry,
                &RemoteSshOptions {
                    integration: true,
                    reuse: true,
                    tmux: false,
                    control_dir: Some(std::path::PathBuf::from("/run/user/1000/odytty/ssh")),
                    control_persist: Some(token.to_owned()),
                },
            )
            .expect("argv");
            let args = argv(&command);
            assert!(args.iter().any(|a| a == expected), "{expected} present");
            assert_eq!(
                args.iter()
                    .filter(|a| a.starts_with("ControlPersist="))
                    .count(),
                1,
                "exactly one ControlPersist option"
            );
        }
    }

    #[test]
    fn parse_control_persist_maps_off_tokens_and_durations() {
        // Off tokens tear the master down with its last connection.
        for off in ["off", "0", "no", "none", "false", "  OFF  "] {
            assert_eq!(parse_control_persist(off).as_deref(), Some("no"), "{off}");
        }
        // Presets and raw durations normalize to whole seconds.
        assert_eq!(parse_control_persist("10m").as_deref(), Some("600"));
        assert_eq!(parse_control_persist("30m").as_deref(), Some("1800"));
        assert_eq!(parse_control_persist("2h").as_deref(), Some("7200"));
        assert_eq!(parse_control_persist("1h30m").as_deref(), Some("5400"));
        assert_eq!(parse_control_persist("90s").as_deref(), Some("90"));
        assert_eq!(parse_control_persist("600").as_deref(), Some("600"));
        // Garbage yields None so the caller can fall back to the global default.
        assert_eq!(parse_control_persist("soon"), None);
        assert_eq!(parse_control_persist("1h30"), None);
        assert_eq!(parse_control_persist(""), None);
    }

    // Windows client: OpenSSH has no ControlMaster, so the reuse control options
    // (including ControlPersist) are compiled out — the knob is inert. This test
    // pins that a Windows argv carries NO ControlPersist regardless of the token.
    #[cfg(windows)]
    #[test]
    fn windows_never_emits_control_persist() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let command = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: true,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("C:/odytty/ssh")),
                control_persist: Some("7200".to_owned()),
            },
        )
        .expect("argv");
        let joined = argv(&command).join(" ");
        assert!(!joined.contains("ControlPersist"));
        assert!(!joined.contains("ControlMaster"));
    }

    #[cfg(not(windows))]
    #[test]
    fn control_socket_path_uses_openssh_connection_hash_within_sun_path_limit() {
        // `%C` expands to OpenSSH's 40-hex-character hash over `%l%h%p%r`, so
        // the same destination on ports 22 and 2222 cannot share a socket. The
        // expansion is fixed-width: only the control-dir length varies.
        let dir = std::path::Path::new("/run/user/1000/odytty/ssh");
        let socket = control_socket_path(dir);
        assert_eq!(socket, dir.join("ssh-%C"));
        let expanded_len = dir.as_os_str().len() + 1 + "ssh-".len() + 40;
        assert!(
            expanded_len < 104,
            "expanded socket path fits the shortest sun_path"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_control_dir_creates_owner_only_directory() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "odytty-ctrl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let dir = base.join("ssh");
        ensure_control_dir(&dir).expect("create control dir");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // Idempotent: a second call tightens rather than fails.
        ensure_control_dir(&dir).expect("second call");
        std::fs::remove_dir_all(&base).ok();
    }

    /// A unique scratch base under the OS temp dir for control-dir tests.
    #[cfg(unix)]
    fn control_dir_scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "odytty-ctrl-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ))
    }

    #[cfg(unix)]
    #[test]
    fn ensure_control_dir_rejects_symlink_leaf_without_touching_target() {
        use std::os::unix::fs::PermissionsExt;
        let base = control_dir_scratch("symlink");
        let target = base.join("target");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("keep.txt"), b"untouched").expect("seed target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .expect("broaden target");
        let dir = base.join("ssh");
        std::os::unix::fs::symlink(&target, &dir).expect("plant symlink leaf");

        let error = ensure_control_dir(&dir).expect_err("symlink leaf must fail closed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        // The symlink target keeps its permissions and content: no follow-chmod.
        let mode = std::fs::metadata(&target)
            .expect("target metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(
            std::fs::read(target.join("keep.txt")).expect("target content"),
            b"untouched"
        );
        // The planted link itself is left in place, not replaced or deleted.
        assert!(
            std::fs::symlink_metadata(&dir)
                .expect("leaf metadata")
                .file_type()
                .is_symlink()
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ensure_control_dir_rejects_dangling_symlink_leaf() {
        let base = control_dir_scratch("dangling");
        std::fs::create_dir_all(&base).expect("create base");
        let dir = base.join("ssh");
        std::os::unix::fs::symlink(base.join("missing"), &dir).expect("plant dangling link");

        let error = ensure_control_dir(&dir).expect_err("dangling symlink must fail closed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            std::fs::symlink_metadata(&dir)
                .expect("leaf metadata")
                .file_type()
                .is_symlink()
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ensure_control_dir_rejects_regular_file_leaf_without_touching_it() {
        let base = control_dir_scratch("file");
        std::fs::create_dir_all(&base).expect("create base");
        let dir = base.join("ssh");
        std::fs::write(&dir, b"not a directory").expect("plant file leaf");

        let error = ensure_control_dir(&dir).expect_err("file leaf must fail closed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read(&dir).expect("leaf content"),
            b"not a directory"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ensure_control_dir_repairs_broad_mode_on_same_owner_directory() {
        use std::os::unix::fs::PermissionsExt;
        let base = control_dir_scratch("repair");
        let dir = base.join("ssh");
        std::fs::create_dir_all(&dir).expect("create dir");
        std::fs::write(dir.join("ssh-deadbeef"), b"socket placeholder").expect("seed dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
            .expect("broaden dir");

        ensure_control_dir(&dir).expect("same-owner repair succeeds");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // Repair only tightens the mode; existing contents are preserved.
        assert_eq!(
            std::fs::read(dir.join("ssh-deadbeef")).expect("content"),
            b"socket placeholder"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn control_leaf_rejection_fails_closed_on_symlink_type_and_owner() {
        // Symlink beats everything else.
        assert_eq!(
            control_leaf_rejection(true, true, 1000, 1000),
            Some("path is a symlink")
        );
        // A non-directory object is rejected.
        assert_eq!(
            control_leaf_rejection(false, false, 1000, 1000),
            Some("path is not a directory")
        );
        // A directory owned by another user is rejected (root-squashed or
        // pre-planted foreign dirs never get sockets or a chmod).
        assert_eq!(
            control_leaf_rejection(false, true, 0, 1000),
            Some("directory is owned by another user")
        );
        // The happy path: real same-owner directory passes.
        assert_eq!(control_leaf_rejection(false, true, 1000, 1000), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_reuse_never_emits_control_options() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let command = ssh_command_for_host_with_options(
            &entry,
            &RemoteSshOptions {
                integration: true,
                reuse: true,
                tmux: false,
                control_dir: Some(std::path::PathBuf::from("C:/odytty/ssh")),
                control_persist: None,
            },
        )
        .expect("argv");
        let joined = argv(&command).join(" ");
        assert_eq!(command.program(), &OsString::from("ssh"));
        assert!(joined.contains("-t"));
        assert!(!joined.contains("ControlMaster"));
        assert!(!joined.contains("ControlPath"));
        assert!(!joined.contains("ControlPersist"));
    }

    #[test]
    fn integration_argv_forces_pty_and_injects_bootstrap() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let command = ssh_command_for_host_with_integration(&entry, true).expect("argv");
        let args = argv(&command);
        assert_eq!(args[0], "ssh");
        let t_pos = args.iter().position(|a| a == "-t").expect("-t");
        let sep = args.iter().position(|a| a == "--").expect("--");
        let dest = args
            .iter()
            .position(|a| a == "deploy@web1.example.invalid")
            .expect("dest");
        // `-t` forces the PTY, and the destination operand follows `--`.
        assert!(t_pos < sep && sep < dest);
        let bootstrap = bootstrap_arg(&command);
        assert!(bootstrap.starts_with("ODYTTY_RC='"));
        assert_eq!(bootstrap.matches("ODYTTY_RC=").count(), 1);
    }

    #[test]
    fn integration_argv_passes_port_before_destination() {
        let mut entry = remote_host("web1", "deploy", "web1.example.invalid");
        entry.port = Some(2222);
        let args = argv(&ssh_command_for_host_with_integration(&entry, true).expect("argv"));
        let p = args.iter().position(|a| a == "-p").expect("-p");
        assert_eq!(args[p + 1], "2222");
        let sep = args.iter().position(|a| a == "--").expect("--");
        assert!(p < sep);
    }

    #[test]
    fn bootstrap_blob_encodes_shared_integration_payload() {
        let command = ssh_command_for_host_with_integration(&host("web1"), true).expect("argv");
        let bootstrap = bootstrap_arg(&command);
        let rc = remote_bash_rc();
        // The remote rc reuses the shared local payload verbatim; only the
        // leading self-delete line differs, so local and remote cannot drift.
        assert!(rc.contains(&crate::shell_integration::bash_integration_rc()));
        assert!(rc.starts_with("rm -f \"${BASH_SOURCE[0]}\""));
        let blob = base64_encode(rc.as_bytes());
        assert!(bootstrap.contains(&format!("ODYTTY_RC='{blob}'")));
    }

    #[test]
    fn bootstrap_is_host_independent_and_leaks_no_identifiers() {
        let a = ssh_command_for_host_with_integration(
            &remote_host("a", "alice", "a.example.invalid"),
            true,
        )
        .expect("a");
        let b = ssh_command_for_host_with_integration(
            &remote_host("b", "bob", "b.example.invalid"),
            true,
        )
        .expect("b");
        // No host/user data reaches the remote command; it rides in ssh operands.
        assert_eq!(bootstrap_arg(&a), bootstrap_arg(&b));
        let bootstrap = bootstrap_arg(&a);
        assert!(!bootstrap.contains("alice"));
        assert!(!bootstrap.contains("a.example.invalid"));
    }

    #[test]
    fn bootstrap_always_terminates_in_an_exec() {
        let bootstrap =
            bootstrap_arg(&ssh_command_for_host_with_integration(&host("web1"), true).expect("a"));
        assert!(
            bootstrap
                .trim_end()
                .ends_with("exec \"${SHELL:-/bin/sh}\" -l")
        );
        // ControlMaster reuse is a later increment; i1 must not emit it.
        assert!(!bootstrap.contains("ControlMaster"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_integration_argv_uses_ssh_without_controlmaster() {
        let entry = remote_host("web1", "deploy", "web1.example.invalid");
        let command = ssh_command_for_host_with_integration(&entry, true).expect("argv");
        assert_eq!(command.program(), &OsString::from("ssh"));
        let joined = argv(&command).join(" ");
        assert!(joined.contains("-t"));
        assert!(!joined.contains("ControlMaster"));
        assert!(!joined.contains("ControlPath"));
    }

    #[test]
    fn builds_host_only_ssh_argv() {
        let command = ssh_command_for_host(&host("web1")).expect("argv");
        assert_eq!(argv(&command), vec!["ssh", "--", "web1"]);
    }

    #[test]
    fn builds_user_host_ssh_argv_from_resolved_host_name() {
        let mut entry = host("web1");
        entry.host_name = Some("web1.example.invalid".to_owned());
        entry.user = Some("deploy".to_owned());

        let command = ssh_command_for_host(&entry).expect("argv");
        assert_eq!(
            argv(&command),
            vec!["ssh", "--", "deploy@web1.example.invalid"]
        );
    }

    #[test]
    fn builds_user_port_host_ssh_argv() {
        let mut entry = host("web1");
        entry.host_name = Some("web1.example.invalid".to_owned());
        entry.user = Some("deploy".to_owned());
        entry.port = Some(2222);

        let command = ssh_command_for_host(&entry).expect("argv");
        assert_eq!(
            argv(&command),
            vec!["ssh", "-p", "2222", "--", "deploy@web1.example.invalid"]
        );
    }

    #[test]
    fn no_profile_or_credential_material_enters_argv() {
        let mut entry = host("web1");
        entry.host_name = Some("web1.example.invalid".to_owned());
        entry.user = Some("deploy".to_owned());
        entry.port = Some(2222);
        entry.theme = Some("odyssey".to_owned());
        entry.font = Some("Victor Mono".to_owned());
        entry.title = Some("synthetic-password-marker".to_owned());

        let command = ssh_command_for_host(&entry).expect("argv");
        let joined = argv(&command).join(" ");
        assert!(!joined.contains("odyssey"));
        assert!(!joined.contains("Victor"));
        assert!(!joined.contains("synthetic-password-marker"));
    }

    #[test]
    fn rejects_credential_like_destination_fields() {
        let mut entry = host("web1");
        entry.host_name = Some("deploy:synthetic-secret@example.invalid".to_owned());
        assert_eq!(
            ssh_command_for_host(&entry).expect_err("credential-like target rejected"),
            SshConnectError::InvalidField("host")
        );

        let mut entry = host("web1");
        entry.user = Some("deploy@example".to_owned());
        assert_eq!(
            ssh_command_for_host(&entry).expect_err("ambiguous user rejected"),
            SshConnectError::InvalidField("user")
        );
    }

    #[cfg(unix)]
    #[test]
    fn ssh_session_is_resumable_host_exec_command() {
        let mut entry = host("web1");
        entry.host_name = Some("web1.example.invalid".to_owned());
        entry.user = Some("deploy".to_owned());
        entry.port = Some(2222);

        let config = detached_ssh_host_config("ssh-web1", &entry, None, Dimensions::new(100, 30))
            .expect("host config");
        assert_eq!(config.session_id, "ssh-web1");
        assert_eq!(config.dimensions, Dimensions::new(100, 30));
        match config.command {
            HostCommand::Exec {
                program,
                args,
                working_directory,
            } => {
                assert_eq!(program, OsString::from("ssh"));
                assert_eq!(
                    args,
                    vec![
                        OsString::from("-p"),
                        OsString::from("2222"),
                        OsString::from("--"),
                        OsString::from("deploy@web1.example.invalid")
                    ]
                );
                assert_eq!(working_directory, None);
            }
            other => panic!("expected ssh exec host command, got {other:?}"),
        }
    }
}

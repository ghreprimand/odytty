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

use crate::connection_hosts::{ConnectionHost, ConnectionHostSource};
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

/// Outcome of a noninteractive Test Connection probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeClass {
    /// Host key and key/agent authentication succeeded.
    AuthOk,
    /// The server offers password or keyboard-interactive authentication.
    InteractiveAuth,
    /// The server rejected noninteractive authentication without an interactive fallback.
    AuthRejected,
    /// Host-key verification failed without a changed-key diagnostic.
    HostKeyUntrusted,
    /// The remote key differs from a stored key.
    HostKeyMismatch,
    /// Connection timeout, refusal, DNS error, or another unclassified failure.
    Unreachable,
}

/// Build the argv for a Test Connection probe: a non-interactive,
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
    if let Some(port) = ssh_port_override(host) {
        args.push(OsString::from("-p"));
        args.push(OsString::from(port.to_string()));
    }
    push_identity_args(&mut args, host);
    args.push(OsString::from("--"));
    args.push(OsString::from(destination));
    args.push(OsString::from("exit"));
    Ok(SshCommand::new("ssh", args))
}

/// Classify OpenSSH's English diagnostics without retaining their contents.
/// Only explicit offered-method lists establish interactive authentication.
pub fn classify_probe(exit_ok: bool, stderr: &str) -> ProbeClass {
    if exit_ok {
        return ProbeClass::AuthOk;
    }
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("remote host identification has changed") {
        return ProbeClass::HostKeyMismatch;
    }
    if lower.contains("host key verification failed") {
        return ProbeClass::HostKeyUntrusted;
    }
    let mut methods = None;
    for line in lower.lines() {
        if let Some((_, list)) = line.split_once("authentications that can continue:") {
            methods = Some(list.trim());
        } else if let Some((_, denial)) = line.split_once("permission denied (")
            && let Some((list, _)) = denial.split_once(')')
        {
            methods = Some(list.trim());
        }
    }
    if let Some(methods) = methods {
        if methods
            .split(',')
            .any(|method| matches!(method.trim(), "password" | "keyboard-interactive"))
        {
            return ProbeClass::InteractiveAuth;
        }
        return ProbeClass::AuthRejected;
    }
    if lower.contains("permission denied") {
        return ProbeClass::AuthRejected;
    }
    ProbeClass::Unreachable
}

/// OdyTTY-owned entries override the port; imported aliases delegate it to OpenSSH.
pub fn ssh_port_override(host: &ConnectionHost) -> Option<u16> {
    match host.source {
        ConnectionHostSource::Odytty => host.port,
        ConnectionHostSource::SshConfig => None,
    }
}

/// Push `-i <path>` for a host's `IdentityFile`, when set. The
/// path is a separate argv element (never merged into `-i<path>`) so a path is
/// always read as the identity filename, never an ssh option. An empty path is
/// ignored. OdyTTY stores only the path; no key material is ever handled.
fn push_identity_args(args: &mut Vec<OsString>, host: &ConnectionHost) {
    if host.source == ConnectionHostSource::SshConfig {
        return;
    }
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

/// Build argv for system SSH, with an alias operand for imported OpenSSH entries.
/// OdyTTY-owned entries use `[USER@]HOST` and optional identity and port arguments.
/// `--` keeps the destination an operand. No local shell is involved.
pub fn ssh_command_for_host(host: &ConnectionHost) -> Result<SshCommand, SshConnectError> {
    let destination = ssh_destination(host)?;
    let mut args = Vec::new();
    push_identity_args(&mut args, host);
    if let Some(port) = ssh_port_override(host) {
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
    /// The `ControlPersist=` token for the reuse master: `no` to
    /// tear the master down with its last connection, or a window in whole
    /// seconds. `None` uses the built-in default window
    /// ([`SSH_CONTROL_PERSIST_SECS`]), so callers that do not resolve the
    /// `remote_persist` knob stay byte-identical to the historical fixed window.
    /// Ignored on a Windows client (the control options are compiled out there).
    pub control_persist: Option<String>,
}

/// Build the system-ssh argv for a saved connection entry, optionally injecting
/// OdyTTY's shell integration on the remote (bash only).
///
/// This entry point is retained for direct integration-only callers and its
/// tests; connection reuse is off. See
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
/// [`ssh_command_for_host`]: no remote command, no PTY-forcing `-t`, no control
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
/// the rcfile self-deletes on first read,
/// and setup failure falls back to the configured login shell. A POSIX sh is
/// required; disable integration on servers without it.
///
/// `[CONTROL-OPTS]` (`-o ControlMaster=auto -o ControlPersist=… -o
/// ControlPath=…`) are added only when `opts.reuse` is set and a `control_dir`
/// resolved. The first tab to a host establishes a shared master and later
/// tabs multiplex over it with no fresh handshake. OpenSSH for Windows has no
/// socket multiplexing, so the control options are compiled out on Windows
/// entirely and reuse is a silent no-op there.
///
/// The remote command enters POSIX sh even when the login shell is fish, then
/// selects Bash integration or the configured login shell.
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
    if let Some(port) = ssh_port_override(host) {
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
/// random name under `/tmp`. The random component is drawn from
/// OS cryptographic entropy. The remote command uses noclobber as a second
/// boundary: a pre-existing path or symlink fails rather than being followed.
/// The name uses only `[/tmp/a-z0-9.-]`, so it is safe to single-quote into the
/// remote shell command with no escaping.
pub fn remote_upload_target() -> Result<String, getrandom::Error> {
    remote_upload_target_with(getrandom::fill)
}

/// Seam for [`remote_upload_target`]: `fill` supplies the 16 random bytes so a
/// test can drive the entropy-unavailable path. Production passes
/// `getrandom::fill`.
pub fn remote_upload_target_with(
    fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<String, getrandom::Error> {
    Ok(format!(
        "{REMOTE_UPLOAD_DIR}/odytty-paste-{}.png",
        random_hex_token_with(fill)?
    ))
}

/// Length of the hex token minted by [`random_hex_token_with`] (128 bits).
const REMOTE_UPLOAD_TOKEN_HEX_LEN: usize = 32;

/// True when `path` has exactly the shape [`remote_upload_target`] mints:
/// `/tmp/odytty-paste-<32 lowercase hex>.png`. The cleanup command only ever
/// names paths of this shape, so nothing outside OdyTTY-minted temp files can
/// reach the remote `rm`, whatever the caller's bookkeeping holds.
pub fn is_minted_remote_upload_path(path: &str) -> bool {
    let prefix = format!("{REMOTE_UPLOAD_DIR}/odytty-paste-");
    let Some(rest) = path.strip_prefix(&prefix) else {
        return false;
    };
    let Some(token) = rest.strip_suffix(".png") else {
        return false;
    };
    token.len() == REMOTE_UPLOAD_TOKEN_HEX_LEN
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A 128-bit hex token drawn directly from the operating system CSPRNG.
///
/// Failure is returned to the caller: image upload then fails closed before a
/// path or remote command is constructed.
fn random_hex_token_with(
    fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Connect timeout for one-shot upload and cleanup commands.
const REMOTE_EXEC_CONNECT_TIMEOUT: &str = "ConnectTimeout=10";

/// Build the argv that uploads a pasted image to a remote temp path.
///
/// The upload runs the system `ssh` binary (the same credential-delegating
/// transport as the connect path, never an embedded ssh), with a remote command
/// that creates the file `0600` (`umask 077`) and streams the bytes from the
/// upload process's stdin:
///
/// - `ssh -o ConnectTimeout=10 [-o ControlPath=…] [-p PORT] -- DEST "umask 077; set -C; cat > '<remote>'"`
///
/// The caller wires the local PNG file to the child's stdin. `cat` (rather than
/// `scp`) reuses the exact ssh option shape (and `ControlMaster` socket) of the
/// connect path, so the upload multiplexes over the live session with no second
/// auth. POSIX `set -C` makes redirection fail if the remote path already exists,
/// including a symlink, and `umask 077` gives a new file mode 0600.
/// The remote path is an OdyTTY-minted `/tmp/odytty-paste-<hash>.png` (only
/// `[/tmp/a-z0-9.-]`), single-quoted with no escaping needed. On Windows the
/// program is still `ssh` (`ssh.exe`) and no `ControlPath` option is emitted
/// (OpenSSH there has no socket multiplexing), so the upload does its own
/// connect: functional, just not multiplexed.
pub fn remote_upload_command(
    destination: &str,
    port: Option<u16>,
    control_dir: Option<&std::path::Path>,
    remote_path: &str,
) -> SshCommand {
    let remote_command = remote_posix_command(&format!("umask 077; set -C; cat > '{remote_path}'"));
    SshCommand::new(
        "ssh",
        build_remote_exec_args(destination, port, control_dir, remote_command),
    )
}

/// Build the best-effort cleanup argv that removes uploaded paste files from the
/// remote on tab close/disconnect. `None` when there is nothing to
/// remove. Best-effort by nature: if the link has already dropped the command
/// cannot run, and the file persists until the remote's own `/tmp` reaper
/// removes it. This is a documented caveat, not a guaranteed deletion.
pub fn remote_cleanup_command(
    destination: &str,
    port: Option<u16>,
    control_dir: Option<&std::path::Path>,
    paths: &[String],
) -> Option<SshCommand> {
    // Only OdyTTY-minted `/tmp/odytty-paste-<hex>.png` paths are removable:
    // anything else is dropped here, at the command boundary, so the cleanup
    // can never name a foreign path. The retained shape is `[/tmp/a-z0-9.-]`
    // only, safe to single-quote with no escaping.
    let quoted = paths
        .iter()
        .filter(|p| is_minted_remote_upload_path(p))
        .map(|p| format!("'{p}'"))
        .collect::<Vec<_>>()
        .join(" ");
    if quoted.is_empty() {
        return None;
    }
    let remote_command = format!("rm -f {quoted}");
    Some(SshCommand::new(
        "ssh",
        build_remote_exec_args(destination, port, control_dir, remote_command),
    ))
}

/// Shared argv assembly for a one-shot remote command over the connect path's
/// ssh transport: a bounded `ConnectTimeout`, optional `ControlPath` reuse
/// (Unix only), optional port, then `-- DEST <remote-command>`. No `-t` (these
/// are non-interactive one-shots). The connect timeout bounds only the TCP
/// connect of a fresh connection; the caller also bounds the whole process
/// lifetime, because a stalled multiplexed channel never reaches it.
fn build_remote_exec_args(
    destination: &str,
    port: Option<u16>,
    control_dir: Option<&std::path::Path>,
    remote_command: String,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-o"),
        OsString::from(REMOTE_EXEC_CONNECT_TIMEOUT),
    ];
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

/// The longest `ControlPersist` window a value may name (30 days). OpenSSH
/// accepts up to its integer range and fails the connection beyond it; the
/// master is meant to be a bounded convenience, so a longer window is clamped.
const MAX_CONTROL_PERSIST_SECS: u64 = 30 * 24 * 60 * 60;

/// Parse a `ControlPersist` value (a `remote_persist` preset or a per-host
/// `Persist` override) into the canonical token emitted after `ControlPersist=`:
/// `no` for an off token (the master dies with its last connection), otherwise
/// the window in whole seconds. Accepts an off token (`off`/`0`/`no`/`none`),
/// bare seconds (`600`), and `<n>h`/`<n>m`/`<n>s` combinations (`2h`, `1h30m`,
/// `90s`). A duration that totals zero (`0s`, `00`, `0m`) is the off token too:
/// OpenSSH reads `ControlPersist 0` as "persist indefinitely", the opposite of
/// the intent. A window beyond [`MAX_CONTROL_PERSIST_SECS`] is clamped to it.
/// Returns `None` for an unrecognized value so the caller can fall back to the
/// global default. Pure string handling with no shell and no I/O; the value is
/// only ever placed as a single `-o ControlPersist=<token>` argv element.
pub fn parse_control_persist(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if matches!(trimmed.as_str(), "off" | "0" | "no" | "none" | "false") {
        return Some("no".to_owned());
    }
    parse_duration_secs(&trimmed).map(|secs| match secs {
        0 => "no".to_owned(),
        secs => secs.min(MAX_CONTROL_PERSIST_SECS).to_string(),
    })
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
/// The fixed-width expansion keeps the file name short without exposing the
/// destination; the directory is checked against the platform `sun_path` limit
/// (104 bytes on macOS/BSD, 108 on Linux) by [`control_dir_fits_socket_limit`].
#[cfg(not(windows))]
fn control_socket_path(control_dir: &std::path::Path) -> PathBuf {
    control_dir.join("ssh-%C")
}

/// Longest socket path a Unix-domain `bind` accepts, without the terminating
/// NUL: `sun_path` is 108 bytes on Linux and 104 on macOS and the BSDs.
#[cfg(target_os = "linux")]
pub const SOCKET_PATH_MAX: usize = 107;
#[cfg(all(unix, not(target_os = "linux")))]
pub const SOCKET_PATH_MAX: usize = 103;

/// Width of `%C`, a hex SHA-1.
#[cfg(unix)]
const CONTROL_HASH_LEN: usize = 40;

/// OpenSSH binds the master to `<ControlPath>.<16 random characters>` before
/// it renames the socket into place, so the bound path is 17 bytes longer than
/// the final one.
#[cfg(unix)]
const CONTROL_TEMP_SUFFIX_LEN: usize = 17;

/// Whether a control socket under `control_dir` stays bindable within `limit`
/// bytes. The template is `<dir>/ssh-%C`; a directory that makes the master's
/// temporary path longer than `sun_path` would make every master listener
/// fail.
#[cfg(unix)]
pub fn control_dir_fits_socket_limit(control_dir: &std::path::Path, limit: usize) -> bool {
    let bound = control_dir.as_os_str().len() + 1 + "ssh-".len() + CONTROL_HASH_LEN;
    bound + CONTROL_TEMP_SUFFIX_LEN <= limit
}

/// The short per-user directory used for control sockets when the state
/// directory is too deep for `sun_path`: `/tmp/odytty-<uid>`, prepared by
/// [`ensure_control_dir`] (private, owned by the user, never a symlink).
#[cfg(unix)]
pub fn short_control_dir() -> PathBuf {
    // SAFETY: `geteuid` takes no arguments, reads no memory, and cannot fail.
    let euid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/tmp/odytty-{euid}"))
}

/// Create OdyTTY's `ControlMaster` socket directory with owner-only `0700`
/// permissions, so the multiplexing sockets are never group/world accessible.
/// Idempotent; tightens permissions on an existing same-owner dir too.
///
/// The final `ssh` leaf is handled without following symlinks: `mkdir` never
/// resolves a final-component symlink, the leaf is then validated via
/// `symlink_metadata` (real directory, owned by the effective UID), and any
/// permission repair happens through an `O_NOFOLLOW | O_DIRECTORY` handle with
/// `fchmod` semantics. A planted symlink, a foreign-owned directory, or a
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

/// Remote Bash startup: remove the wrapper, load login files in Bash order,
/// then use the shared interactive configuration and OSC integration body.
fn remote_bash_rc() -> String {
    format!(
        r#"rm -f "${{BASH_SOURCE[0]}}" 2>/dev/null
if [ -r /etc/profile ]; then
  . /etc/profile
fi
for __odytty_login_file in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
  if [ -r "$__odytty_login_file" ]; then
    . "$__odytty_login_file"
    break
  fi
done
unset __odytty_login_file
{}"#,
        crate::shell_integration::bash_integration_rc()
    )
}

/// Pass POSIX syntax as one argument to sh, including through a fish login shell.
fn remote_posix_command(script: &str) -> String {
    format!("sh -c '{}'", script.replace('\'', "'\\''"))
}

/// Build a quoted `sh -c` command for the remote login shell.
///
/// Authored, inspectable text plus one base64 blob of [`remote_bash_rc`]. It
/// carries no local paths, usernames, or hostnames, only literal `$HOME`/
/// `$SHELL` variables expand on the remote. A POSIX sh is required. Missing
/// Bash or base64 and setup failure fall back to the remote login shell.
fn remote_bash_bootstrap(tmux: bool) -> String {
    let blob = base64_encode(remote_bash_rc().as_bytes());
    // The interactive launch, once the rcfile is materialized. With tmux off the
    // bootstrap `exec`s bash directly. With tmux on it `exec`s a
    // persistent `tmux new-session -A -s odytty` (create-or-attach) running the
    // integrated bash, so a dropped-and-reconnected link reattaches the same
    // remote session. It degrades to plain bash when the remote has no `tmux`, so
    // opting in never yields a broken session. `new -A` applies the command only
    // on CREATE; a reattach keeps the original shell (the integration is already
    // live), which also means the freshly written rcfile is only consumed on
    // create; a reattach leaves that small temp file for the OS to reap.
    let launch = if tmux {
        "if command -v tmux >/dev/null 2>&1; then \
         exec tmux new-session -A -s odytty bash --rcfile \"$__odytty_rc\" -i; \
         else exec bash --rcfile \"$__odytty_rc\" -i; fi"
    } else {
        "exec bash --rcfile \"$__odytty_rc\" -i"
    };
    let script = format!(
        "ODYTTY_RC='{blob}'\n\
         if command -v bash >/dev/null 2>&1 && command -v base64 >/dev/null 2>&1; then\n  \
         __odytty_rc=\"$(mktemp 2>/dev/null || printf '%s' \"/tmp/.odytty-rc.$$\")\" && \
         printf '%s' \"$ODYTTY_RC\" | base64 -d > \"$__odytty_rc\" 2>/dev/null && \
         {{ {launch} ; }} || exec \"${{SHELL:-/bin/sh}}\" -l\n\
         fi\n\
         exec \"${{SHELL:-/bin/sh}}\" -l\n"
    );
    remote_posix_command(&script)
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
/// An SSH connection is a normal hosted
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

/// The SSH operand: imported OpenSSH alias, or OdyTTY-owned `[USER@]HOST`. Validated to reject credential-like or shell-unsafe
/// fields. Exposed so the connect path can capture the destination for the
/// image paste-through upload descriptor without rebuilding the argv.
pub fn ssh_destination(host: &ConnectionHost) -> Result<String, SshConnectError> {
    if host.source == ConnectionHostSource::SshConfig {
        validate_target_field("alias", &host.alias)?;
        return Ok(host.alias.clone());
    }
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
mod tests;

#[cfg(all(test, unix))]
mod startup_tests;

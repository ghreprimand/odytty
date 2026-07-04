// SPDX-License-Identifier: GPL-3.0-only
//! SSH connect-action substrate.
//!
//! This module builds the only command line OdyTTY needs for SSH: an argv for
//! the system `ssh` binary from name-only connection fields. It never handles
//! passwords, private keys, passphrases, agent sockets, or OpenSSH config file
//! contents; authentication remains entirely delegated to `ssh`.

use std::ffi::OsString;
use std::fmt;
#[cfg(unix)]
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
pub fn ssh_command_for_host(host: &ConnectionHost) -> Result<SshCommand, SshConnectError> {
    let destination = ssh_destination(host)?;
    let mut args = Vec::new();
    if let Some(port) = host.port {
        args.push(OsString::from("-p"));
        args.push(OsString::from(port.to_string()));
    }
    args.push(OsString::from("--"));
    args.push(OsString::from(destination));
    Ok(SshCommand::new("ssh", args))
}

/// Build the system-ssh argv for a saved connection entry, optionally injecting
/// OdyTTY's shell integration on the remote.
///
/// When `integration_enabled` is `false` the argv is byte-identical to
/// [`ssh_command_for_host`] — no remote command, no PTY-forcing `-t`, nothing
/// added. This is the exact guarantee callers rely on when integration is turned
/// off globally or opted out for a host.
///
/// When enabled, the form becomes:
///
/// - `ssh -t [-p PORT] -- [USER@]HOST <bootstrap>`
///
/// where `<bootstrap>` is a self-contained POSIX-sh command that materializes
/// the bash integration rcfile from an inline base64 blob into a temporary file
/// and `exec`s an interactive bash pointed at it. The bootstrap is delivery-only
/// for the shared [`crate::shell_integration::bash_integration_rc`] payload;
/// nothing is persisted on the remote (the rcfile self-deletes on first read),
/// and every failure path falls back to a plain login shell so the connection is
/// never broken. Non-bash remote shells silently degrade to a plain session.
///
/// i1 is bash-only: the client always emits the bash bootstrap, and the remote
/// bootstrap self-selects bash-or-fallback at runtime. Extending detection to
/// zsh/fish is a later increment.
pub fn ssh_command_for_host_with_integration(
    host: &ConnectionHost,
    integration_enabled: bool,
) -> Result<SshCommand, SshConnectError> {
    if !integration_enabled {
        return ssh_command_for_host(host);
    }
    let destination = ssh_destination(host)?;
    let mut args = Vec::new();
    // `-t` forces a remote PTY so the injected bash starts interactive.
    args.push(OsString::from("-t"));
    if let Some(port) = host.port {
        args.push(OsString::from("-p"));
        args.push(OsString::from(port.to_string()));
    }
    args.push(OsString::from("--"));
    args.push(OsString::from(destination));
    args.push(OsString::from(remote_bash_bootstrap()));
    Ok(SshCommand::new("ssh", args))
}

/// Resolve whether remote integration is active for a host: an explicit per-host
/// setting wins, otherwise the global default applies.
pub fn remote_integration_enabled(host_integration: Option<bool>, global_default: bool) -> bool {
    host_integration.unwrap_or(global_default)
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
fn remote_bash_bootstrap() -> String {
    let blob = base64_encode(remote_bash_rc().as_bytes());
    format!(
        "ODYTTY_RC='{blob}'\n\
         if command -v bash >/dev/null 2>&1 && command -v base64 >/dev/null 2>&1; then\n  \
         __odytty_rc=\"$(mktemp 2>/dev/null || printf '%s' \"/tmp/.odytty-rc.$$\")\" && \
         printf '%s' \"$ODYTTY_RC\" | base64 -d > \"$__odytty_rc\" 2>/dev/null && \
         exec bash --rcfile \"$__odytty_rc\" -i || exec \"${{SHELL:-/bin/sh}}\" -l\n\
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

fn ssh_destination(host: &ConnectionHost) -> Result<String, SshConnectError> {
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

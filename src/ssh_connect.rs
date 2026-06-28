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

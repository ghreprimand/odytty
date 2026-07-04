// SPDX-License-Identifier: GPL-3.0-only
//! Pure connection-host data layer for the future SSH manager.
//!
//! OdyTTY's own hosts file is the default source. Reading OpenSSH config is an
//! explicit opt-in controlled by settings and remains name-only via
//! [`crate::ssh_config`]. Tests use synthetic files and injected loaders only.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::settings::Settings;
use crate::ssh_config::{SshConfigReadLimits, SshHostEntry, read_ssh_config_with_limits};

/// User-owned OdyTTY connection list filename under the OdyTTY config dir.
pub const CONNECTION_HOSTS_FILE_NAME: &str = "hosts.conf";
/// Default maximum bytes read from the OdyTTY-owned hosts list.
pub const DEFAULT_CONNECTION_HOSTS_MAX_BYTES: u64 = 256 * 1024;
/// Default maximum OdyTTY-owned host entries returned.
pub const DEFAULT_CONNECTION_HOSTS_MAX_ENTRIES: usize = 1024;
/// Default maximum characters retained for one surfaced field.
pub const DEFAULT_CONNECTION_HOSTS_MAX_FIELD_CHARS: usize = 512;

/// Bounds for the OdyTTY-owned hosts file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionHostsLimits {
    pub max_bytes: u64,
    pub max_entries: usize,
    pub max_field_chars: usize,
}

impl Default for ConnectionHostsLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_CONNECTION_HOSTS_MAX_BYTES,
            max_entries: DEFAULT_CONNECTION_HOSTS_MAX_ENTRIES,
            max_field_chars: DEFAULT_CONNECTION_HOSTS_MAX_FIELD_CHARS,
        }
    }
}

/// One connection candidate for a future quick-connect UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionHost {
    pub alias: String,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub theme: Option<String>,
    pub font: Option<String>,
    pub title: Option<String>,
    /// Per-host opt-out for remote OSC 133 shell integration. `None` inherits
    /// the global `remote_integration` setting; `Some(false)` forces a plain
    /// ssh session for this host even when the global default is on.
    pub integration: Option<bool>,
    /// Per-host opt-out for ControlMaster connection reuse. `None` inherits the
    /// global `remote_reuse` setting; `Some(false)` forces a fresh connection
    /// for this host even when the global default is on.
    pub reuse: Option<bool>,
    /// Per-host override for tmux persistence. `None` inherits the global
    /// `remote_tmux` setting; `Some(true)` wraps this host's remote shell in a
    /// persistent tmux session even when the global default is off (and
    /// `Some(false)` opts a host out when the default is on).
    pub tmux: Option<bool>,
    pub source: ConnectionHostSource,
}

/// Which local source produced a host row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionHostSource {
    /// OdyTTY-owned `hosts.conf`.
    Odytty,
    /// Opt-in, name-only OpenSSH config import.
    SshConfig,
}

/// Caller-provided paths for the two local sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionHostPaths {
    pub odytty_hosts: PathBuf,
    pub ssh_config: Option<PathBuf>,
}

impl ConnectionHostPaths {
    pub fn new(odytty_hosts: impl Into<PathBuf>, ssh_config: Option<PathBuf>) -> Self {
        Self {
            odytty_hosts: odytty_hosts.into(),
            ssh_config,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostBlock {
    aliases: Vec<String>,
    host_name: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    theme: Option<String>,
    font: Option<String>,
    title: Option<String>,
    integration: Option<bool>,
    reuse: Option<bool>,
    tmux: Option<bool>,
}

impl HostBlock {
    fn new(aliases: Vec<String>) -> Self {
        Self {
            aliases,
            host_name: None,
            user: None,
            port: None,
            theme: None,
            font: None,
            title: None,
            integration: None,
            reuse: None,
            tmux: None,
        }
    }
}

/// Resolve the OdyTTY-owned hosts path from a config directory.
pub fn hosts_file_path(config_dir: impl AsRef<Path>) -> PathBuf {
    config_dir.as_ref().join(CONNECTION_HOSTS_FILE_NAME)
}

/// Read the OdyTTY-owned hosts file with default limits.
///
/// Missing, unreadable, malformed, non-UTF-8, or oversized files return an
/// empty or partial bounded list without panicking.
pub fn read_odytty_hosts(path: impl AsRef<Path>) -> Vec<ConnectionHost> {
    read_odytty_hosts_with_limits(path, ConnectionHostsLimits::default())
}

/// Read the OdyTTY-owned hosts file with explicit limits.
pub fn read_odytty_hosts_with_limits(
    path: impl AsRef<Path>,
    limits: ConnectionHostsLimits,
) -> Vec<ConnectionHost> {
    if limits.max_bytes == 0 || limits.max_entries == 0 || limits.max_field_chars == 0 {
        return Vec::new();
    }
    let Ok(bytes) = fs::read(path.as_ref()) else {
        return Vec::new();
    };
    parse_odytty_hosts_bytes_with_limits(&bytes, limits)
}

/// Parse OdyTTY-owned hosts bytes with default limits.
pub fn parse_odytty_hosts_bytes(bytes: &[u8]) -> Vec<ConnectionHost> {
    parse_odytty_hosts_bytes_with_limits(bytes, ConnectionHostsLimits::default())
}

/// Parse OdyTTY-owned hosts bytes with explicit limits.
pub fn parse_odytty_hosts_bytes_with_limits(
    bytes: &[u8],
    limits: ConnectionHostsLimits,
) -> Vec<ConnectionHost> {
    if limits.max_bytes == 0 || limits.max_entries == 0 || limits.max_field_chars == 0 {
        return Vec::new();
    }
    let capped_len = bytes.len().min(limits.max_bytes as usize);
    let text = String::from_utf8_lossy(&bytes[..capped_len]);
    parse_odytty_hosts_text(&text, limits)
}

/// Save OdyTTY-owned hosts in a stable `Host <alias>` block format.
pub fn save_odytty_hosts(path: impl AsRef<Path>, hosts: &[ConnectionHost]) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format_odytty_hosts(hosts))
}

/// Format OdyTTY-owned hosts in the storage format documented for `hosts.conf`.
pub fn format_odytty_hosts(hosts: &[ConnectionHost]) -> String {
    let mut out = String::new();
    for host in hosts {
        if host.alias.trim().is_empty() {
            continue;
        }
        out.push_str("Host ");
        out.push_str(&quote_field(&host.alias));
        out.push('\n');
        push_optional_field(&mut out, "HostName", host.host_name.as_deref());
        push_optional_field(&mut out, "User", host.user.as_deref());
        if let Some(port) = host.port {
            out.push_str("    Port ");
            out.push_str(&port.to_string());
            out.push('\n');
        }
        push_optional_field(&mut out, "Theme", host.theme.as_deref());
        push_optional_field(&mut out, "Font", host.font.as_deref());
        push_optional_field(&mut out, "Title", host.title.as_deref());
        if let Some(integration) = host.integration {
            push_optional_field(
                &mut out,
                "Integration",
                Some(if integration { "on" } else { "off" }),
            );
        }
        if let Some(reuse) = host.reuse {
            push_optional_field(&mut out, "Reuse", Some(if reuse { "on" } else { "off" }));
        }
        if let Some(tmux) = host.tmux {
            push_optional_field(&mut out, "Tmux", Some(if tmux { "on" } else { "off" }));
        }
        out.push('\n');
    }
    out
}

/// Load and merge local connection sources using the resolved runtime setting.
pub fn load_connection_hosts(
    settings: &Settings,
    paths: &ConnectionHostPaths,
) -> Vec<ConnectionHost> {
    load_connection_hosts_with_loaders(
        settings.ssh_config_hosts,
        || read_odytty_hosts(&paths.odytty_hosts),
        || {
            paths
                .ssh_config
                .as_ref()
                .map(|path| read_ssh_config_with_limits(path, SshConfigReadLimits::default()))
                .unwrap_or_default()
        },
    )
}

/// Load and merge local connection sources with injectable readers for tests.
pub fn load_connection_hosts_with_loaders(
    include_ssh_config: bool,
    mut read_odytty_hosts: impl FnMut() -> Vec<ConnectionHost>,
    mut read_ssh_config_hosts: impl FnMut() -> Vec<SshHostEntry>,
) -> Vec<ConnectionHost> {
    let owned = read_odytty_hosts();
    let ssh = if include_ssh_config {
        read_ssh_config_hosts()
    } else {
        Vec::new()
    };
    merge_connection_hosts(owned, ssh)
}

/// Merge OdyTTY-owned hosts first, then opt-in OpenSSH config names.
///
/// Duplicate aliases keep the OdyTTY-owned row so user-authored per-host
/// profile fields win over imported names.
pub fn merge_connection_hosts(
    owned: Vec<ConnectionHost>,
    ssh_config_hosts: Vec<SshHostEntry>,
) -> Vec<ConnectionHost> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for host in owned {
        if host.alias.trim().is_empty() || !seen.insert(host.alias.clone()) {
            continue;
        }
        out.push(ConnectionHost {
            source: ConnectionHostSource::Odytty,
            ..host
        });
    }
    for host in ssh_config_hosts {
        if host.alias.trim().is_empty() || !seen.insert(host.alias.clone()) {
            continue;
        }
        out.push(ConnectionHost {
            alias: host.alias,
            host_name: host.host_name,
            user: host.user,
            port: host.port,
            theme: None,
            font: None,
            title: None,
            integration: None,
            reuse: None,
            tmux: None,
            source: ConnectionHostSource::SshConfig,
        });
    }
    out
}

fn parse_odytty_hosts_text(text: &str, limits: ConnectionHostsLimits) -> Vec<ConnectionHost> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut current = None;

    for raw_line in text.lines() {
        let tokens = tokenize_line(raw_line.trim_end_matches('\r'));
        let Some((keyword, args)) = directive(tokens) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                flush_block(&mut entries, &mut seen, current.take(), limits.max_entries);
                current = Some(HostBlock::new(concrete_aliases(
                    &args,
                    limits.max_field_chars,
                )));
            }
            "hostname" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.host_name = Some(trim_chars(value, limits.max_field_chars));
                }
            }
            "user" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.user = Some(trim_chars(value, limits.max_field_chars));
                }
            }
            "port" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.port = value.parse::<u16>().ok();
                }
            }
            "theme" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.theme = Some(trim_chars(value, limits.max_field_chars));
                }
            }
            "font" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.font = Some(trim_chars(value, limits.max_field_chars));
                }
            }
            "title" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.title = Some(trim_chars(value, limits.max_field_chars));
                }
            }
            "integration" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.integration = parse_host_bool(value);
                }
            }
            "reuse" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.reuse = parse_host_bool(value);
                }
            }
            "tmux" => {
                if let (Some(block), Some(value)) = (current.as_mut(), args.first()) {
                    block.tmux = parse_host_bool(value);
                }
            }
            _ => {}
        }

        if entries.len() >= limits.max_entries {
            current = None;
            break;
        }
    }

    flush_block(&mut entries, &mut seen, current, limits.max_entries);
    entries
}

fn tokenize_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut chars = line.trim_start().chars().peekable();

    while let Some(ch) = chars.next() {
        if quote.is_none() && ch == '#' && current.is_empty() {
            break;
        }
        if quote.is_none() && ch.is_whitespace() {
            push_token(&mut tokens, &mut current);
            continue;
        }
        if matches!(quote, Some(q) if q == ch) {
            quote = None;
            continue;
        }
        if quote.is_none() && (ch == '"' || ch == '\'') {
            quote = Some(ch);
            continue;
        }
        if quote.is_some() && ch == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        current.push(ch);
    }

    push_token(&mut tokens, &mut current);
    tokens
}

fn push_token(tokens: &mut Vec<String>, current: &mut String) {
    if current.is_empty() {
        return;
    }
    tokens.push(std::mem::take(current));
}

fn directive(tokens: Vec<String>) -> Option<(String, Vec<String>)> {
    let first = tokens.first()?;
    if let Some((keyword, value)) = first.split_once('=') {
        let mut args = Vec::new();
        if !value.is_empty() {
            args.push(value.to_string());
        }
        args.extend(tokens.iter().skip(1).cloned());
        return Some((keyword.to_ascii_lowercase(), args));
    }
    if tokens.len() >= 3 && tokens.get(1).map(String::as_str) == Some("=") {
        return Some((
            first.to_ascii_lowercase(),
            tokens.iter().skip(2).cloned().collect(),
        ));
    }
    Some((
        first.to_ascii_lowercase(),
        tokens.into_iter().skip(1).collect(),
    ))
}

fn concrete_aliases(args: &[String], max_chars: usize) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut seen = HashSet::new();
    for arg in args {
        if arg.is_empty() || arg.starts_with('!') || arg.contains('*') || arg.contains('?') {
            continue;
        }
        let alias = trim_chars(arg, max_chars);
        if seen.insert(alias.clone()) {
            aliases.push(alias);
        }
    }
    aliases
}

fn flush_block(
    entries: &mut Vec<ConnectionHost>,
    seen: &mut HashSet<String>,
    block: Option<HostBlock>,
    max_entries: usize,
) {
    let Some(block) = block else {
        return;
    };
    for alias in block.aliases {
        if entries.len() >= max_entries {
            break;
        }
        if !seen.insert(alias.clone()) {
            continue;
        }
        entries.push(ConnectionHost {
            alias,
            host_name: block.host_name.clone(),
            user: block.user.clone(),
            port: block.port,
            theme: block.theme.clone(),
            font: block.font.clone(),
            title: block.title.clone(),
            integration: block.integration,
            reuse: block.reuse,
            tmux: block.tmux,
            source: ConnectionHostSource::Odytty,
        });
    }
}

/// Parse a hosts.conf boolean field value (`on`/`off`/`yes`/`no`/`true`/`false`,
/// case-insensitive). Unrecognized values return `None` so the global default
/// stands rather than silently forcing a state.
fn parse_host_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Some(true),
        "off" | "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn push_optional_field(out: &mut String, key: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    out.push_str("    ");
    out.push_str(key);
    out.push(' ');
    out.push_str(&quote_field(value));
    out.push('\n');
}

fn quote_field(value: &str) -> String {
    if value
        .chars()
        .all(|ch| !ch.is_whitespace() && ch != '"' && ch != '\'' && ch != '#')
    {
        return value.to_owned();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn limits() -> ConnectionHostsLimits {
        ConnectionHostsLimits {
            max_bytes: 4096,
            max_entries: 16,
            max_field_chars: 128,
        }
    }

    fn host(alias: &str, source: ConnectionHostSource) -> ConnectionHost {
        ConnectionHost {
            alias: alias.to_owned(),
            host_name: Some(format!("{alias}.example.invalid")),
            user: None,
            port: None,
            theme: None,
            font: None,
            title: None,
            integration: None,
            reuse: None,
            tmux: None,
            source,
        }
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).expect("create synthetic temp dir");
        path
    }

    #[test]
    fn parses_odytty_owned_hosts_with_profile_fields() {
        let entries = parse_odytty_hosts_bytes_with_limits(
            br#"
                # synthetic OdyTTY-owned hosts list
                Host web1 web2
                    HostName gateway.example.invalid
                    User deploy
                    Port 2222
                    Theme odyssey
                    Font "Victor Mono"
                    Title "Synthetic Web"
            "#,
            limits(),
        );

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].alias, "web1");
        assert_eq!(
            entries[0].host_name.as_deref(),
            Some("gateway.example.invalid")
        );
        assert_eq!(entries[0].user.as_deref(), Some("deploy"));
        assert_eq!(entries[0].port, Some(2222));
        assert_eq!(entries[0].theme.as_deref(), Some("odyssey"));
        assert_eq!(entries[0].font.as_deref(), Some("Victor Mono"));
        assert_eq!(entries[0].title.as_deref(), Some("Synthetic Web"));
        assert_eq!(entries[0].integration, None);
        assert_eq!(entries[0].source, ConnectionHostSource::Odytty);
    }

    #[test]
    fn parses_and_round_trips_per_host_integration_optout() {
        let entries = parse_odytty_hosts_bytes_with_limits(
            b"Host secure\n    HostName secure.example.invalid\n    Integration off\n",
            limits(),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].integration, Some(false));

        // The field survives a save/reload round-trip.
        let formatted = format_odytty_hosts(&entries);
        assert!(formatted.contains("Integration off"));
        let reparsed = parse_odytty_hosts_bytes_with_limits(formatted.as_bytes(), limits());
        assert_eq!(reparsed[0].integration, Some(false));
    }

    #[test]
    fn per_host_integration_ignores_unrecognized_values() {
        let entries = parse_odytty_hosts_bytes_with_limits(
            b"Host a\nHost b\n    Integration maybe\n    Integration on\n",
            limits(),
        );
        // Absent field inherits the global default (None); a later valid value
        // wins over an earlier unrecognized one.
        assert_eq!(entries[0].integration, None);
        assert_eq!(entries[1].integration, Some(true));
    }

    #[test]
    fn parses_and_round_trips_per_host_reuse_optout() {
        let entries = parse_odytty_hosts_bytes_with_limits(
            b"Host bastion\n    HostName bastion.example.invalid\n    Reuse off\n",
            limits(),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reuse, Some(false));
        assert_eq!(entries[0].integration, None);

        let formatted = format_odytty_hosts(&entries);
        assert!(formatted.contains("Reuse off"));
        let reparsed = parse_odytty_hosts_bytes_with_limits(formatted.as_bytes(), limits());
        assert_eq!(reparsed[0].reuse, Some(false));
    }

    #[test]
    fn parses_and_round_trips_per_host_tmux_override() {
        let entries = parse_odytty_hosts_bytes_with_limits(
            b"Host persistent\n    HostName persistent.example.invalid\n    Tmux on\n",
            limits(),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tmux, Some(true));
        assert_eq!(entries[0].reuse, None);
        assert_eq!(entries[0].integration, None);

        let formatted = format_odytty_hosts(&entries);
        assert!(formatted.contains("Tmux on"));
        let reparsed = parse_odytty_hosts_bytes_with_limits(formatted.as_bytes(), limits());
        assert_eq!(reparsed[0].tmux, Some(true));
    }

    #[test]
    fn save_round_trips_odytty_owned_hosts() {
        let dir = temp_dir("odytty-connection-hosts");
        let path = hosts_file_path(&dir);
        let mut entry = host("web 1", ConnectionHostSource::Odytty);
        entry.user = Some("deploy".to_owned());
        entry.title = Some("Synthetic Web".to_owned());

        save_odytty_hosts(&path, &[entry.clone()]).expect("save synthetic hosts");
        let loaded = read_odytty_hosts_with_limits(&path, limits());

        assert_eq!(loaded, vec![entry]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_and_malformed_hosts_files_degrade_to_empty_or_partial() {
        let dir = temp_dir("odytty-connection-hosts-missing");
        assert!(read_odytty_hosts_with_limits(dir.join("missing"), limits()).is_empty());

        let partial = parse_odytty_hosts_bytes_with_limits(
            b"Host good\nHostName good.example.invalid\n\xff",
            limits(),
        );
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].alias, "good");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn caps_are_enforced_for_owned_hosts() {
        let capped = ConnectionHostsLimits {
            max_bytes: 48,
            max_entries: 1,
            max_field_chars: 4,
        };
        let entries = parse_odytty_hosts_bytes_with_limits(
            b"Host abcdef\nHostName abcdef.example.invalid\nHost second\n",
            capped,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "abcd");
        assert_eq!(entries[0].host_name.as_deref(), Some("abcd"));
    }

    #[test]
    fn disabled_ssh_config_integration_never_calls_ssh_loader() {
        let entries = load_connection_hosts_with_loaders(
            false,
            || vec![host("owned", ConnectionHostSource::Odytty)],
            || panic!("disabled SSH config source must not be read"),
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "owned");
    }

    #[test]
    fn opt_in_ssh_config_hosts_are_merged_after_owned_hosts() {
        let entries = load_connection_hosts_with_loaders(
            true,
            || vec![host("owned", ConnectionHostSource::Odytty)],
            || {
                vec![
                    SshHostEntry {
                        alias: "owned".to_owned(),
                        host_name: Some("ssh-owned.example.invalid".to_owned()),
                        user: Some("ssh".to_owned()),
                        port: Some(2200),
                    },
                    SshHostEntry {
                        alias: "remote".to_owned(),
                        host_name: Some("remote.example.invalid".to_owned()),
                        user: None,
                        port: None,
                    },
                ]
            },
        );

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["owned", "remote"]
        );
        assert_eq!(entries[0].source, ConnectionHostSource::Odytty);
        assert_eq!(entries[1].source, ConnectionHostSource::SshConfig);
    }

    #[test]
    fn loaded_connection_hosts_honor_settings_opt_in() {
        let dir = temp_dir("odytty-connection-hosts-settings");
        let owned_path = hosts_file_path(&dir);
        let ssh_path = dir.join("ssh_config");
        fs::write(&owned_path, b"Host owned\nHostName owned.example.invalid\n")
            .expect("write synthetic owned hosts");
        fs::write(&ssh_path, b"Host remote\nHostName remote.example.invalid\n")
            .expect("write synthetic ssh config");
        let paths = ConnectionHostPaths::new(&owned_path, Some(ssh_path));

        let disabled = load_connection_hosts(&Settings::default(), &paths);
        let enabled = load_connection_hosts(
            &Settings {
                ssh_config_hosts: true,
                ..Settings::default()
            },
            &paths,
        );

        assert_eq!(
            disabled
                .iter()
                .map(|entry| entry.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["owned"]
        );
        assert_eq!(
            enabled
                .iter()
                .map(|entry| entry.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["owned", "remote"]
        );
        fs::remove_dir_all(dir).ok();
    }
}

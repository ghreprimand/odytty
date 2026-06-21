// SPDX-License-Identifier: GPL-3.0-only
//! Bounded, name-only OpenSSH config reader for the future connection manager.
//!
//! Privacy boundary: this module never discovers a user's SSH config path on
//! its own and never follows `Include`. Callers must pass the exact path or
//! bytes they want parsed. Only quick-connect display fields are surfaced:
//! concrete `Host` aliases plus optional `HostName`, `User`, and `Port`.
//! Key material directives such as `IdentityFile` are ignored.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Default maximum bytes read from the beginning of one ssh config file.
pub const DEFAULT_SSH_CONFIG_MAX_BYTES: u64 = 256 * 1024;
/// Default maximum concrete host entries returned.
pub const DEFAULT_SSH_CONFIG_MAX_ENTRIES: usize = 1024;
/// Default maximum characters retained for one surfaced field.
pub const DEFAULT_SSH_CONFIG_MAX_FIELD_CHARS: usize = 512;

/// Bounds for reading and parsing a caller-provided OpenSSH config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SshConfigReadLimits {
    /// Maximum bytes read from the start of the supplied file/content.
    pub max_bytes: u64,
    /// Maximum concrete host entries returned.
    pub max_entries: usize,
    /// Maximum characters retained for aliases and display fields.
    pub max_field_chars: usize,
}

impl Default for SshConfigReadLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_SSH_CONFIG_MAX_BYTES,
            max_entries: DEFAULT_SSH_CONFIG_MAX_ENTRIES,
            max_field_chars: DEFAULT_SSH_CONFIG_MAX_FIELD_CHARS,
        }
    }
}

/// A concrete SSH host candidate suitable for a future quick-connect list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHostEntry {
    /// Concrete alias from a `Host` pattern. Wildcards and negated patterns are
    /// not surfaced.
    pub alias: String,
    /// Optional `HostName` value from the same `Host` block.
    pub host_name: Option<String>,
    /// Optional `User` value from the same `Host` block.
    pub user: Option<String>,
    /// Optional `Port` value from the same `Host` block.
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostBlock {
    aliases: Vec<String>,
    host_name: Option<String>,
    user: Option<String>,
    port: Option<u16>,
}

impl HostBlock {
    fn new(aliases: Vec<String>) -> Self {
        Self {
            aliases,
            host_name: None,
            user: None,
            port: None,
        }
    }
}

/// Read a caller-supplied OpenSSH config path with default hard limits.
///
/// Missing, unreadable, non-file, binary, malformed, or oversized files return
/// the readable bounded subset, or an empty vector. `Include` directives are
/// skipped; the parser never follows additional filesystem paths.
pub fn read_ssh_config(path: impl AsRef<Path>) -> Vec<SshHostEntry> {
    read_ssh_config_with_limits(path, SshConfigReadLimits::default())
}

/// Read a caller-supplied OpenSSH config path with explicit hard limits.
pub fn read_ssh_config_with_limits(
    path: impl AsRef<Path>,
    limits: SshConfigReadLimits,
) -> Vec<SshHostEntry> {
    let Some(bytes) = read_bounded_prefix(path.as_ref(), limits.max_bytes) else {
        return Vec::new();
    };
    parse_ssh_config_bytes_with_limits(&bytes, limits)
}

/// Parse OpenSSH config bytes with default hard limits.
pub fn parse_ssh_config_bytes(bytes: &[u8]) -> Vec<SshHostEntry> {
    parse_ssh_config_bytes_with_limits(bytes, SshConfigReadLimits::default())
}

/// Parse OpenSSH config bytes with explicit hard limits.
///
/// `Match` blocks are ignored until the next `Host` block because their
/// conditions are runtime-dependent. `Include` is skipped rather than resolved,
/// keeping this pure parser bounded to the caller-provided content.
pub fn parse_ssh_config_bytes_with_limits(
    bytes: &[u8],
    limits: SshConfigReadLimits,
) -> Vec<SshHostEntry> {
    if limits.max_bytes == 0 || limits.max_entries == 0 || limits.max_field_chars == 0 {
        return Vec::new();
    }

    let capped_len = bytes.len().min(limits.max_bytes as usize);
    let text = String::from_utf8_lossy(&bytes[..capped_len]);
    parse_ssh_config_text(&text, limits)
}

fn parse_ssh_config_text(text: &str, limits: SshConfigReadLimits) -> Vec<SshHostEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut current = None;
    let mut in_match = false;

    for raw_line in text.lines() {
        let tokens = tokenize_line(raw_line.trim_end_matches('\r'));
        let Some((keyword, args)) = directive(tokens) else {
            continue;
        };

        match keyword.as_str() {
            "host" => {
                flush_block(&mut entries, &mut seen, current.take(), limits.max_entries);
                in_match = false;
                current = Some(HostBlock::new(concrete_aliases(
                    &args,
                    limits.max_field_chars,
                )));
            }
            "match" => {
                flush_block(&mut entries, &mut seen, current.take(), limits.max_entries);
                in_match = true;
            }
            "include" => {}
            _ if in_match => {}
            "hostname" => {
                if let (Some(block), Some(value)) = (current.as_mut(), first_arg(&args)) {
                    block.host_name = Some(trim_chars(value, limits.max_field_chars));
                }
            }
            "user" => {
                if let (Some(block), Some(value)) = (current.as_mut(), first_arg(&args)) {
                    block.user = Some(trim_chars(value, limits.max_field_chars));
                }
            }
            "port" => {
                if let (Some(block), Some(value)) = (current.as_mut(), first_arg(&args)) {
                    block.port = value.parse::<u16>().ok();
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

fn read_bounded_prefix(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    if max_bytes == 0 {
        return None;
    }
    let file = File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(max_bytes.min(8192) as usize);
    file.take(max_bytes).read_to_end(&mut bytes).ok()?;
    Some(bytes)
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
        if !is_concrete_alias(arg) {
            continue;
        }
        let alias = trim_chars(arg, max_chars);
        if seen.insert(alias.clone()) {
            aliases.push(alias);
        }
    }
    aliases
}

fn is_concrete_alias(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('!') && !value.contains('*') && !value.contains('?')
}

fn first_arg(args: &[String]) -> Option<&str> {
    args.first().map(String::as_str)
}

fn flush_block(
    entries: &mut Vec<SshHostEntry>,
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
        entries.push(SshHostEntry {
            alias,
            host_name: block.host_name.clone(),
            user: block.user.clone(),
            port: block.port,
        });
    }
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn limits() -> SshConfigReadLimits {
        SshConfigReadLimits {
            max_bytes: 4096,
            max_entries: 16,
            max_field_chars: 128,
        }
    }

    fn parse(bytes: &[u8]) -> Vec<SshHostEntry> {
        parse_ssh_config_bytes_with_limits(bytes, limits())
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
    fn parses_well_formed_multi_host_config() {
        let entries = parse(
            br#"
                # synthetic fixture only
                Host web1 web2
                    HostName bastion.example.invalid
                    User deploy
                    Port 2222

                Host "quoted-host"
                    HostName quoted.example.invalid
            "#,
        );

        assert_eq!(
            entries,
            vec![
                SshHostEntry {
                    alias: "web1".to_string(),
                    host_name: Some("bastion.example.invalid".to_string()),
                    user: Some("deploy".to_string()),
                    port: Some(2222),
                },
                SshHostEntry {
                    alias: "web2".to_string(),
                    host_name: Some("bastion.example.invalid".to_string()),
                    user: Some("deploy".to_string()),
                    port: Some(2222),
                },
                SshHostEntry {
                    alias: "quoted-host".to_string(),
                    host_name: Some("quoted.example.invalid".to_string()),
                    user: None,
                    port: None,
                },
            ]
        );
    }

    #[test]
    fn wildcard_and_negated_host_patterns_are_not_quick_connect_entries() {
        let entries = parse(
            br#"
                Host * !blocked *.example.invalid concrete
                    HostName concrete.example.invalid
            "#,
        );

        assert_eq!(
            entries,
            vec![SshHostEntry {
                alias: "concrete".to_string(),
                host_name: Some("concrete.example.invalid".to_string()),
                user: None,
                port: None,
            }]
        );
    }

    #[test]
    fn include_is_skipped_and_never_resolved() {
        let entries = parse(
            br#"
                Include /tmp/synthetic-ssh-config-include
                Host local-only
                    HostName local.example.invalid
            "#,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "local-only");
    }

    #[test]
    fn match_block_is_ignored_until_next_host() {
        let entries = parse(
            br#"
                Match host matched.example.invalid
                    HostName hidden.example.invalid
                    User ignored
                Host visible
                    HostName visible.example.invalid
            "#,
        );

        assert_eq!(
            entries,
            vec![SshHostEntry {
                alias: "visible".to_string(),
                host_name: Some("visible.example.invalid".to_string()),
                user: None,
                port: None,
            }]
        );
    }

    #[test]
    fn malformed_and_non_utf8_input_degrades_without_panicking() {
        let entries =
            parse(b"Host good\nHostName good.example.invalid\n\xff\xfe\0\nHost\nPort nope\n");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "good");
        assert_eq!(
            entries[0].host_name.as_deref(),
            Some("good.example.invalid")
        );
    }

    #[test]
    fn caps_total_bytes_entries_and_field_chars() {
        let capped = SshConfigReadLimits {
            max_bytes: 45,
            max_entries: 1,
            max_field_chars: 4,
        };
        let entries = parse_ssh_config_bytes_with_limits(
            b"Host abcdef\nHostName abcdef.example.invalid\nHost second\n",
            capped,
        );

        assert_eq!(
            entries,
            vec![SshHostEntry {
                alias: "abcd".to_string(),
                host_name: Some("abcd".to_string()),
                user: None,
                port: None,
            }]
        );
    }

    #[test]
    fn dedupes_aliases_while_preserving_first_seen_order() {
        let entries = parse(
            br#"
                Host one two one
                    HostName first.example.invalid
                Host two three
                    HostName second.example.invalid
            "#,
        );

        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two", "three"]
        );
        assert_eq!(
            entries[1].host_name.as_deref(),
            Some("first.example.invalid")
        );
        assert_eq!(
            entries[2].host_name.as_deref(),
            Some("second.example.invalid")
        );
    }

    #[test]
    fn identity_file_and_key_directives_are_not_surfaced() {
        let entries = parse(
            br#"
                Host web1
                    HostName web1.example.invalid
                    IdentityFile /synthetic/ignored-identity-file
                    CertificateFile /synthetic/private/cert.pub
                    LocalCommand echo synthetic
            "#,
        );
        let debug = format!("{entries:?}");

        assert_eq!(entries.len(), 1);
        assert!(!debug.contains("IdentityFile"));
        assert!(!debug.contains("ignored-identity-file"));
        assert!(!debug.contains("CertificateFile"));
        assert!(!debug.contains("LocalCommand"));
    }

    #[test]
    fn reads_only_the_caller_supplied_path() {
        let dir = temp_dir("odytty-ssh-config");
        let config = dir.join("config");
        fs::write(
            &config,
            b"Host web1\nHostName web1.example.invalid\nInclude ignored\n",
        )
        .expect("write synthetic ssh config");

        let entries = read_ssh_config_with_limits(&config, limits());

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "web1");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_and_non_file_paths_return_empty_lists() {
        let dir = temp_dir("odytty-ssh-config-non-file");
        let missing = dir.join("missing");

        assert!(read_ssh_config_with_limits(&missing, limits()).is_empty());
        assert!(read_ssh_config_with_limits(&dir, limits()).is_empty());
        fs::remove_dir_all(dir).ok();
    }
}

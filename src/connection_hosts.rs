// SPDX-License-Identifier: GPL-3.0-only
//! Connection-host list: read, parse, append, edit, and remove entries of
//! OdyTTY's hosts file, and merge the opt-in OpenSSH names.
//!
//! OdyTTY's own hosts file is the default source. Reading OpenSSH config is an
//! explicit opt-in controlled by settings and remains name-only via
//! [`crate::ssh_config`]. Tests use synthetic files and injected loaders only.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
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

/// One connection candidate for the connection manager and quick-connect list.
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
    /// Reserved connection protocol. `None` (and the only currently accepted
    /// value, `ssh`) select the SSH transport; the field exists so a future
    /// protocol needs no file-format migration. Any value is preserved across a
    /// round trip; the connect path is SSH-only for now.
    pub protocol: Option<String>,
    /// Path to an SSH private key (`IdentityFile`). When set, the connect argv
    /// gains `-i <path>`; OdyTTY stores only the path, never any key material.
    /// The once-and-done alternative to typing a password is `ssh-copy-id`.
    pub identity_file: Option<String>,
    /// Per-host override for the reuse master's `ControlPersist` window.
    /// `None` inherits the global `remote_persist` setting; any value
    /// here is passed through as the raw `ssh` ControlPersist token (e.g. `off`,
    /// `2h`, `600`). Unix-only in effect - never emitted on a Windows client.
    pub persist: Option<String>,
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
    protocol: Option<String>,
    identity_file: Option<String>,
    persist: Option<String>,
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
            protocol: None,
            identity_file: None,
            persist: None,
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
    // Stream only a bounded prefix, plus one byte that shows whether the file
    // is longer, so an oversized or adversarial hosts file can never be read
    // whole into memory. The parser keeps only the complete lines of a cut
    // prefix; mutation paths use the rejecting mode below because they must
    // preserve every existing byte.
    let Ok(bytes) = read_hosts_file(path.as_ref(), limits.max_bytes.saturating_add(1), false)
    else {
        return Vec::new();
    };
    parse_odytty_hosts_bytes_with_limits(&bytes, limits)
}

fn read_hosts_file(path: &Path, max_bytes: u64, reject_oversized: bool) -> io::Result<Vec<u8>> {
    let path_metadata = fs::metadata(path)?;
    if !path_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "connection-hosts path is not a regular file",
        ));
    }
    if reject_oversized && path_metadata.len() > max_bytes {
        return Err(hosts_file_over_limit(max_bytes));
    }

    let file = open_hosts_file(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "opened connection-hosts path is not a regular file",
        ));
    }
    if reject_oversized && opened_metadata.len() > max_bytes {
        return Err(hosts_file_over_limit(max_bytes));
    }

    let read_limit = max_bytes.saturating_add(u64::from(reject_oversized));
    let mut bytes =
        Vec::with_capacity(usize::try_from(opened_metadata.len().min(read_limit)).unwrap_or(0));
    file.take(read_limit).read_to_end(&mut bytes)?;
    if reject_oversized && u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(hosts_file_over_limit(max_bytes));
    }
    Ok(bytes)
}

fn open_hosts_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

fn hosts_file_over_limit(max_bytes: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("connection-hosts file exceeds the {max_bytes}-byte limit"),
    )
}

fn read_hosts_for_mutation(path: &Path) -> io::Result<Vec<u8>> {
    read_hosts_file(path, DEFAULT_CONNECTION_HOSTS_MAX_BYTES, true)
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
    let bytes = strip_utf8_bom(bytes).1;
    let cap = usize::try_from(limits.max_bytes).unwrap_or(usize::MAX);
    let kept = if bytes.len() > cap {
        // The cap cut the file. A line the cut ended (`HostName long.exa`)
        // would parse as a plausible but wrong value, so only the complete
        // lines before the last newline inside the cap are kept.
        let prefix = &bytes[..cap];
        let complete = prefix
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |end| end + 1);
        tracing::warn!(
            limit = limits.max_bytes,
            "connection-hosts file is longer than its read limit; later hosts are ignored"
        );
        &prefix[..complete]
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(kept);
    parse_odytty_hosts_text(&text, limits)
}

/// The UTF-8 byte order mark some Windows editors write at the start of a file.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Split a leading UTF-8 byte order mark from `bytes`: `(mark, rest)`, where
/// `mark` is empty when there is none. Readers parse `rest`; writers put `mark`
/// back in front, so editing a host keeps the file's encoding as it was.
fn strip_utf8_bom(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.strip_prefix(UTF8_BOM) {
        Some(rest) => (UTF8_BOM, rest),
        None => (&[], bytes),
    }
}

/// The hosts text a mutation splices, after its byte order mark. A file that is
/// not valid UTF-8 is refused and left untouched: rewriting it from a lossy
/// decode would replace every invalid byte anywhere in it with U+FFFD.
fn mutation_source(bytes: &[u8]) -> io::Result<(&[u8], &str)> {
    let (bom, body) = strip_utf8_bom(bytes);
    let text = std::str::from_utf8(body).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the connection-hosts file is not valid UTF-8; fix it in a text editor before editing hosts here",
        )
    })?;
    Ok((bom, text))
}

/// Push a single `Host` block (no trailing blank-line separator) into `out`,
/// rendering the `Host` line from the host's single alias.
#[cfg(test)]
fn push_host_block(out: &mut String, host: &ConnectionHost) {
    push_host_block_aliased(out, std::slice::from_ref(&host.alias), host)
        .expect("validated connection host fields");
}

/// Push a `Host` block whose `Host` line carries `aliases` verbatim. The parser
/// flattens a multi-alias `Host a b` block into one entry per alias, so an
/// in-place edit re-renders the block with all its sibling aliases by passing
/// them here; field lines come from `host`.
fn push_host_block_aliased(
    out: &mut String,
    aliases: &[String],
    host: &ConnectionHost,
) -> io::Result<()> {
    validate_serializable_host(host)?;
    if aliases.iter().any(|alias| contains_control(alias)) {
        return Err(control_character_error());
    }
    if !aliases.iter().all(|alias| is_concrete_alias(alias)) {
        return Err(not_concrete_alias_error());
    }
    out.push_str("Host");
    for alias in aliases {
        out.push(' ');
        out.push_str(&quote_field(alias)?);
    }
    out.push('\n');
    // Only emit HostName when it differs from the primary alias - a plain
    // `Host x` block connects to `x` directly, so a redundant `HostName x` is
    // noise.
    if let Some(host_name) = host.host_name.as_deref()
        && host_name != host.alias
    {
        push_optional_field(out, "HostName", Some(host_name))?;
    }
    push_optional_field(out, "User", host.user.as_deref())?;
    if let Some(port) = host.port {
        out.push_str("    Port ");
        out.push_str(&port.to_string());
        out.push('\n');
    }
    push_optional_field(out, "Theme", host.theme.as_deref())?;
    push_optional_field(out, "Font", host.font.as_deref())?;
    push_optional_field(out, "Title", host.title.as_deref())?;
    push_optional_field(out, "IdentityFile", host.identity_file.as_deref())?;
    push_optional_field(out, "Persist", host.persist.as_deref())?;
    if let Some(integration) = host.integration {
        push_optional_field(
            out,
            "Integration",
            Some(if integration { "on" } else { "off" }),
        )?;
    }
    if let Some(reuse) = host.reuse {
        push_optional_field(out, "Reuse", Some(if reuse { "on" } else { "off" }))?;
    }
    if let Some(tmux) = host.tmux {
        push_optional_field(out, "Tmux", Some(if tmux { "on" } else { "off" }))?;
    }
    // Reserved Protocol field: preserved across a round trip, SSH-only at
    // connect time.
    push_optional_field(out, "Protocol", host.protocol.as_deref())?;
    Ok(())
}

/// A parsed ad-hoc connection target from a typed `[user@]host[:port]` query.
///
/// Produced by [`parse_adhoc_target`] when the connection-manager query matches
/// no saved host but is a well-formed destination. Field values are already
/// validated to be shell- and argv-safe (no whitespace, no leading `-`, no `@`
/// inside a part, port in `1..=65535`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdhocTarget {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl AdhocTarget {
    /// The `[user@]host[:port]` display/identity string for this target.
    pub fn display(&self) -> String {
        let mut out = String::new();
        if let Some(user) = &self.user {
            out.push_str(user);
            out.push('@');
        }
        out.push_str(&self.host);
        if let Some(port) = self.port {
            out.push(':');
            out.push_str(&port.to_string());
        }
        out
    }

    /// Build a `ConnectionHost` for the connect path. The alias is the host part
    /// (so a saved block reads `Host <host>`), and per-host integration/reuse/
    /// tmux are left `None` so the global defaults apply - ad-hoc connections
    /// carry no per-host overrides.
    pub fn to_connection_host(&self) -> ConnectionHost {
        ConnectionHost {
            alias: self.host.clone(),
            // The alias IS the host, so no separate HostName is needed; the
            // connect path falls back to the alias as the ssh destination.
            host_name: None,
            user: self.user.clone(),
            port: self.port,
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
}

/// Whether a host/user token is a safe, well-formed name: non-empty ASCII of
/// letters, digits, `.`, `-`, or `_`, and never leading with `-` (which the
/// system `ssh` would read as an option - the argv `--` guard is a second line
/// of defense, not the only one).
pub(crate) fn is_valid_adhoc_part(value: &str) -> bool {
    if value.is_empty() || value.starts_with('-') {
        return false;
    }
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
}

/// Parse a typed connection-manager query as an ad-hoc `[user@]host[:port]`
/// destination, or `None` when it is empty, contains whitespace, is missing a
/// host, has an out-of-range/non-numeric port, or carries an option-injecting
/// leading `-`. The returned parts are argv-safe.
pub fn parse_adhoc_target(query: &str) -> Option<AdhocTarget> {
    // Reject anything with whitespace outright - a real destination never has
    // spaces, and this also rejects the empty/whitespace-only query.
    if query.is_empty() || query.chars().any(char::is_whitespace) {
        return None;
    }

    // Split an optional single `user@` prefix. A second `@` is rejected.
    let (user, rest) = match query.split_once('@') {
        Some((user_part, rest)) => {
            if rest.contains('@') || !is_valid_adhoc_part(user_part) {
                return None;
            }
            (Some(user_part.to_owned()), rest)
        }
        None => (None, query),
    };

    // Split an optional `:port` suffix. Only a single colon is allowed (host
    // names here are not bracketed IPv6 literals, which are not supported).
    let (host, port) = match rest.split_once(':') {
        Some((host_part, port_part)) => {
            if port_part.is_empty() || port_part.contains(':') {
                return None;
            }
            let port: u16 = port_part.parse().ok().filter(|p| *p >= 1)?;
            (host_part, Some(port))
        }
        None => (rest, None),
    };

    if !is_valid_adhoc_part(host) {
        return None;
    }

    Some(AdhocTarget {
        user,
        host: host.to_owned(),
        port,
    })
}

/// Outcome of an ad-hoc save-to-hosts.conf append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendHostOutcome {
    /// A new `Host` block was appended to the file.
    Appended,
    /// A block with this exact alias already existed; the file was not touched.
    AlreadyExists,
}

/// Append a `Host` block for `host` to the OdyTTY-owned hosts file at `path`,
/// preserving existing content byte-for-byte and writing atomically (temp file
/// in the same directory, then rename). Creates the file and parent directories
/// when absent. If a block with the same exact alias already exists, the file
/// is left untouched and [`AppendHostOutcome::AlreadyExists`] is returned.
pub fn append_adhoc_host(
    path: impl AsRef<Path>,
    host: &ConnectionHost,
) -> io::Result<AppendHostOutcome> {
    let path = path.as_ref();
    validate_serializable_host(host)?;
    let existing_bytes = match read_hosts_for_mutation(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err),
    };

    // Exact-alias collision: connect, but never duplicate a saved block. The
    // block scan has no entry cap, so a host past the reader's entry limit
    // still counts; the mutation read already bounds the file size.
    let existing_text = String::from_utf8_lossy(strip_utf8_bom(&existing_bytes).1);
    if find_block(
        &parse_host_blocks(&existing_text, DEFAULT_CONNECTION_HOSTS_MAX_FIELD_CHARS),
        &host.alias,
    )
    .is_some()
    {
        return Ok(AppendHostOutcome::AlreadyExists);
    }

    let mut block = String::new();
    push_host_block_aliased(&mut block, std::slice::from_ref(&host.alias), host)?;

    // Preserve the existing bytes verbatim, then append the new block after a
    // clean blank-line separator.
    let mut out: Vec<u8> = existing_bytes;
    if !out.is_empty() {
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        if !out.ends_with(b"\n\n") {
            out.push(b'\n');
        }
    }
    out.extend_from_slice(block.as_bytes());

    write_hosts_bytes_atomic(path, &out)?;
    Ok(AppendHostOutcome::Appended)
}

/// Atomically write `bytes` to `path` through the shared policy-driven writer
/// under the Config policy: an exclusively-created sibling temp (so a pre-planted
/// name cannot be followed), data + parent-directory fsync, a stricter existing
/// mode preserved, then an atomic rename over the target (Unix and Windows). A
/// crash mid-write can only leave the temp behind, never a half-written target.
fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    crate::state_dir::write_atomic(path, bytes, crate::state_dir::WriteMode::Config)
}

fn write_hosts_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > DEFAULT_CONNECTION_HOSTS_MAX_BYTES {
        return Err(hosts_file_over_limit(DEFAULT_CONNECTION_HOSTS_MAX_BYTES));
    }
    write_bytes_atomic(path, bytes)
}

/// A `Host` block located by byte span in the source file, for in-place edit
/// and remove. `content_end` is the byte just past the block's last body line
/// (an Edit replaces `start..content_end`); `remove_end` extends through the
/// block's trailing blank-line separator (a Remove deletes `start..remove_end`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockSpan {
    start: usize,
    content_end: usize,
    remove_end: usize,
    aliases: Vec<String>,
    /// In-block lines the parser does not model (comments, unknown directives),
    /// captured verbatim (newline-stripped) and re-emitted after the known
    /// fields when the block is edited.
    unknown_lines: Vec<String>,
}

/// Outcome of an in-place edit or remove against the OdyTTY-owned hosts file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostsEditOutcome {
    /// The target block was found and the file was rewritten atomically.
    Written,
    /// No block matched the requested alias (or the file was absent/empty); the
    /// file was left untouched.
    NotFound,
}

/// Parse `source` into `Host` block spans, preserving each block's byte range
/// and its unrecognized in-block lines. A block runs from its `Host` line to the
/// first following blank line or next `Host` line (comments and unknown
/// directives inside that contiguous run are captured verbatim); trailing blank
/// lines up to the next block are recorded as the removable separator. Content
/// before the first `Host` line is a preamble owned by no block and is never
/// spliced.
fn parse_host_blocks(source: &str, max_chars: usize) -> Vec<BlockSpan> {
    // Line table: (byte offset of the line, the line text incl. trailing '\n').
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        lines.push((offset, line));
        offset += line.len();
    }
    let total = source.len();
    let n = lines.len();

    let line_keyword = |raw: &str| directive(tokenize_line(raw.trim_end_matches(['\r', '\n'])));
    let is_blank = |raw: &str| raw.trim_end_matches(['\r', '\n']).trim().is_empty();
    let is_host = |raw: &str| {
        line_keyword(raw)
            .map(|(kw, _)| kw == "host")
            .unwrap_or(false)
    };

    let mut blocks = Vec::new();
    let mut i = 0;
    // Skip the preamble: anything before the first `Host` line.
    while i < n && !is_host(lines[i].1) {
        i += 1;
    }
    while i < n {
        let (start, host_line) = lines[i];
        let aliases = match line_keyword(host_line) {
            Some((kw, args)) if kw == "host" => concrete_aliases(&args, max_chars),
            _ => Vec::new(),
        };
        let mut unknown_lines = Vec::new();
        let mut j = i + 1;
        while j < n {
            let raw = lines[j].1;
            if is_blank(raw) {
                break;
            }
            match line_keyword(raw) {
                Some((kw, _)) if kw == "host" => break,
                Some((kw, _)) if is_known_host_field(&kw) => {}
                // A comment (no directive) or an unrecognized directive: keep it
                // verbatim so an edit re-emits it rather than dropping it.
                _ => unknown_lines.push(raw.trim_end_matches(['\r', '\n']).to_owned()),
            }
            j += 1;
        }
        let content_end = if j < n { lines[j].0 } else { total };
        // Trailing blank-line separator, up to the next non-blank line.
        let mut k = j;
        while k < n && is_blank(lines[k].1) {
            k += 1;
        }
        let remove_end = if k < n { lines[k].0 } else { total };
        blocks.push(BlockSpan {
            start,
            content_end,
            remove_end,
            aliases,
            unknown_lines,
        });
        // Advance to the next `Host` line; inter-block comments/blanks are the
        // next block's preamble and belong to no block.
        i = k;
        while i < n && !is_host(lines[i].1) {
            i += 1;
        }
    }
    blocks
}

/// Index of the block whose alias list contains `alias`, if any.
fn find_block(blocks: &[BlockSpan], alias: &str) -> Option<usize> {
    blocks
        .iter()
        .position(|block| block.aliases.iter().any(|a| a == alias))
}

/// Re-render one block's body: its `Host` line, the known fields from
/// `updated`, then the preserved unknown lines. `target_alias` is replaced by
/// `updated.alias` in the block's alias list so a single-alias rename works and
/// any sibling aliases survive.
fn render_edited_block(
    block: &BlockSpan,
    target_alias: &str,
    updated: &ConnectionHost,
) -> io::Result<String> {
    let new_aliases: Vec<String> = if block.aliases.is_empty() {
        vec![updated.alias.clone()]
    } else {
        block
            .aliases
            .iter()
            .map(|a| {
                if a == target_alias {
                    updated.alias.clone()
                } else {
                    a.clone()
                }
            })
            .collect()
    };
    let mut rendered = String::new();
    push_host_block_aliased(&mut rendered, &new_aliases, updated)?;
    for line in &block.unknown_lines {
        rendered.push_str(line);
        rendered.push('\n');
    }
    Ok(rendered)
}

/// Splice a byte-identical edit of the block owning `target_alias`, replacing
/// only that block's body with `updated` re-rendered. Every other byte -
/// comments, blank lines, other blocks' unknown fields - is preserved. Returns
/// `None` when no block matches. The source is valid UTF-8 without its byte
/// order mark ([`mutation_source`]).
fn splice_host_block_edit(
    source: &str,
    target_alias: &str,
    updated: &ConnectionHost,
    max_chars: usize,
) -> Option<String> {
    let blocks = parse_host_blocks(source, max_chars);
    let idx = find_block(&blocks, target_alias)?;
    let block = &blocks[idx];
    // An existing sibling alias with a control character is never re-emitted.
    // Treat that malformed block as non-editable rather than rewriting it.
    let mut rendered = render_edited_block(block, target_alias, updated).ok()?;
    // Stay byte-aligned when the original block ended at EOF without a newline.
    let had_trailing_newline =
        block.content_end > 0 && source.as_bytes().get(block.content_end - 1) == Some(&b'\n');
    if !had_trailing_newline {
        while rendered.ends_with('\n') {
            rendered.pop();
        }
    }
    let mut out = String::with_capacity(source.len() + rendered.len());
    out.push_str(&source[..block.start]);
    out.push_str(&rendered);
    out.push_str(&source[block.content_end..]);
    Some(out)
}

/// Splice a removal of the block owning `target_alias`, deleting its body and
/// its trailing blank-line separator so no doubled gap remains. Every other
/// byte is preserved. Returns `None` when no block matches.
fn splice_host_block_remove(source: &str, target_alias: &str, max_chars: usize) -> Option<String> {
    let blocks = parse_host_blocks(source, max_chars);
    let idx = find_block(&blocks, target_alias)?;
    let block = &blocks[idx];
    let mut out = String::with_capacity(source.len());
    out.push_str(&source[..block.start]);
    out.push_str(&source[block.remove_end..]);
    Some(out)
}

/// Edit the OdyTTY-owned `Host` block whose alias list contains `target_alias`,
/// re-rendering only that block from `updated` and splicing it over its byte
/// span - every other byte (comments, blank lines, unknown fields in other
/// blocks) is preserved. The write is atomic (temp sibling + rename). A missing
/// file, empty file, or unmatched alias returns [`HostsEditOutcome::NotFound`]
/// and never writes.
///
/// The one caveat, worth surfacing in an editing UI: the edited host's own
/// known fields are re-serialized to canonical form (ordering/whitespace); its
/// unknown lines and all other hosts and comments are untouched. A leading
/// byte order mark is kept. A file that is not valid UTF-8 is refused with
/// `InvalidData` and never written.
pub fn edit_host_block(
    path: impl AsRef<Path>,
    target_alias: &str,
    updated: &ConnectionHost,
) -> io::Result<HostsEditOutcome> {
    let path = path.as_ref();
    if contains_control(target_alias) {
        return Err(control_character_error());
    }
    validate_serializable_host(updated)?;
    let bytes = match read_hosts_for_mutation(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HostsEditOutcome::NotFound),
        Err(err) => return Err(err),
    };
    let (bom, source) = mutation_source(&bytes)?;
    match splice_host_block_edit(
        source,
        target_alias,
        updated,
        DEFAULT_CONNECTION_HOSTS_MAX_FIELD_CHARS,
    ) {
        Some(next) => {
            write_hosts_bytes_atomic(path, &[bom, next.as_bytes()].concat())?;
            Ok(HostsEditOutcome::Written)
        }
        None => Ok(HostsEditOutcome::NotFound),
    }
}

/// Remove the OdyTTY-owned `Host` block whose alias list contains
/// `target_alias`, deleting its byte span and trailing blank-line separator;
/// every other byte, including a leading byte order mark, is preserved. Atomic
/// write. A missing/empty file or an unmatched alias returns
/// [`HostsEditOutcome::NotFound`] and never writes; a file that is not valid
/// UTF-8 is refused with `InvalidData` and never written.
pub fn remove_host_block(
    path: impl AsRef<Path>,
    target_alias: &str,
) -> io::Result<HostsEditOutcome> {
    let path = path.as_ref();
    if contains_control(target_alias) {
        return Err(control_character_error());
    }
    let bytes = match read_hosts_for_mutation(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HostsEditOutcome::NotFound),
        Err(err) => return Err(err),
    };
    let (bom, source) = mutation_source(&bytes)?;
    match splice_host_block_remove(
        source,
        target_alias,
        DEFAULT_CONNECTION_HOSTS_MAX_FIELD_CHARS,
    ) {
        Some(next) => {
            write_hosts_bytes_atomic(path, &[bom, next.as_bytes()].concat())?;
            Ok(HostsEditOutcome::Written)
        }
        None => Ok(HostsEditOutcome::NotFound),
    }
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
            protocol: None,
            identity_file: None,
            persist: None,
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
        if keyword == "host" {
            flush_block(&mut entries, &mut seen, current.take(), limits.max_entries);
            current = Some(HostBlock::new(concrete_aliases(
                &args,
                limits.max_field_chars,
            )));
        } else if let Some(block) = current.as_mut() {
            apply_host_field(block, &keyword, &args, limits.max_field_chars);
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

/// Whether `alias` names one concrete host: not empty, not a negation (`!`),
/// and no wildcard (`*`, `?`). The reader skips any other alias, so the writers
/// refuse one rather than save a host that would never load.
fn is_concrete_alias(alias: &str) -> bool {
    !(alias.is_empty() || alias.starts_with('!') || alias.contains(['*', '?']))
}

fn not_concrete_alias_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "a connection-host alias must be one concrete name: not empty, no leading `!`, no `*` or `?`",
    )
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
            protocol: block.protocol.clone(),
            identity_file: block.identity_file.clone(),
            persist: block.persist.clone(),
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

/// Apply one recognized `hosts.conf` field directive to `block`, returning
/// `true` when `keyword` names a known host field. Unknown keywords return
/// `false` so a byte-preserving caller can capture the raw line verbatim.
fn apply_host_field(
    block: &mut HostBlock,
    keyword: &str,
    args: &[String],
    max_chars: usize,
) -> bool {
    let value = args.first();
    match keyword {
        "hostname" => {
            if let Some(v) = value {
                block.host_name = Some(trim_chars(v, max_chars));
            }
        }
        "user" => {
            if let Some(v) = value {
                block.user = Some(trim_chars(v, max_chars));
            }
        }
        "port" => {
            if let Some(v) = value {
                // Digits only, 1-65535: `0` and a signed `+22` are not ports.
                block.port = v
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
                    .then(|| v.parse::<u16>().ok())
                    .flatten()
                    .filter(|port| *port >= 1);
            }
        }
        "theme" => {
            if let Some(v) = value {
                block.theme = Some(trim_chars(v, max_chars));
            }
        }
        "font" => {
            if let Some(v) = value {
                block.font = Some(trim_chars(v, max_chars));
            }
        }
        "title" => {
            if let Some(v) = value {
                block.title = Some(trim_chars(v, max_chars));
            }
        }
        "identityfile" => {
            if let Some(v) = value {
                block.identity_file = Some(trim_chars(v, max_chars));
            }
        }
        "integration" => {
            if let Some(v) = value {
                block.integration = parse_host_bool(v);
            }
        }
        "reuse" => {
            if let Some(v) = value {
                block.reuse = parse_host_bool(v);
            }
        }
        "tmux" => {
            if let Some(v) = value {
                block.tmux = parse_host_bool(v);
            }
        }
        "protocol" => {
            if let Some(v) = value {
                block.protocol = Some(trim_chars(v, max_chars));
            }
        }
        "persist" => {
            if let Some(v) = value {
                block.persist = Some(trim_chars(v, max_chars));
            }
        }
        _ => return false,
    }
    true
}

/// Whether `keyword` (already lowercased) names a recognized host field.
fn is_known_host_field(keyword: &str) -> bool {
    matches!(
        keyword,
        "hostname"
            | "user"
            | "port"
            | "theme"
            | "font"
            | "title"
            | "identityfile"
            | "integration"
            | "reuse"
            | "tmux"
            | "protocol"
            | "persist"
    )
}

/// Write `key value` when `value` is set and not empty. An empty value would
/// be a bare key that reads back as unset, so it is written as unset.
fn push_optional_field(out: &mut String, key: &str, value: Option<&str>) -> io::Result<()> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    out.push_str("    ");
    out.push_str(key);
    out.push(' ');
    out.push_str(&quote_field(value)?);
    out.push('\n');
    Ok(())
}

fn quote_field(value: &str) -> io::Result<String> {
    if contains_control(value) {
        return Err(control_character_error());
    }
    if value
        .chars()
        .all(|ch| !ch.is_whitespace() && ch != '"' && ch != '\'' && ch != '#')
    {
        return Ok(value.to_owned());
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}

fn contains_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn control_character_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "connection-host fields cannot contain control characters",
    )
}

fn validate_serializable_host(host: &ConnectionHost) -> io::Result<()> {
    let values = [
        Some(host.alias.as_str()),
        host.host_name.as_deref(),
        host.user.as_deref(),
        host.theme.as_deref(),
        host.font.as_deref(),
        host.title.as_deref(),
        host.protocol.as_deref(),
        host.identity_file.as_deref(),
        host.persist.as_deref(),
    ];
    if values.into_iter().flatten().any(contains_control) {
        return Err(control_character_error());
    }
    if !is_concrete_alias(&host.alias) {
        return Err(not_concrete_alias_error());
    }
    if host.port == Some(0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a connection-host port must be from 1 to 65535",
        ));
    }
    Ok(())
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
#[path = "connection_hosts/tests.rs"]
mod tests;

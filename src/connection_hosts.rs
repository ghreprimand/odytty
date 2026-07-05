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
    /// Reserved connection protocol. `None` (and the only currently accepted
    /// value, `ssh`) select the SSH transport; the field exists so a future
    /// protocol needs no file-format migration. Any value is preserved across a
    /// round trip; the connect path is SSH-only for now.
    pub protocol: Option<String>,
    /// Path to an SSH private key (`IdentityFile`). When set, the connect argv
    /// gains `-i <path>`; OdyTTY stores only the path, never any key material.
    /// The once-and-done alternative to typing a password is `ssh-copy-id`.
    pub identity_file: Option<String>,
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

/// Push a single `Host` block (no trailing blank-line separator) into `out`,
/// rendering the `Host` line from the host's single alias.
fn push_host_block(out: &mut String, host: &ConnectionHost) {
    push_host_block_aliased(out, std::slice::from_ref(&host.alias), host);
}

/// Push a `Host` block whose `Host` line carries `aliases` verbatim. The parser
/// flattens a multi-alias `Host a b` block into one entry per alias, so an
/// in-place edit re-renders the block with all its sibling aliases by passing
/// them here; field lines come from `host`.
fn push_host_block_aliased(out: &mut String, aliases: &[String], host: &ConnectionHost) {
    out.push_str("Host");
    for alias in aliases {
        out.push(' ');
        out.push_str(&quote_field(alias));
    }
    out.push('\n');
    // Only emit HostName when it differs from the primary alias — a plain
    // `Host x` block connects to `x` directly, so a redundant `HostName x` is
    // noise.
    if let Some(host_name) = host.host_name.as_deref()
        && host_name != host.alias
    {
        push_optional_field(out, "HostName", Some(host_name));
    }
    push_optional_field(out, "User", host.user.as_deref());
    if let Some(port) = host.port {
        out.push_str("    Port ");
        out.push_str(&port.to_string());
        out.push('\n');
    }
    push_optional_field(out, "Theme", host.theme.as_deref());
    push_optional_field(out, "Font", host.font.as_deref());
    push_optional_field(out, "Title", host.title.as_deref());
    push_optional_field(out, "IdentityFile", host.identity_file.as_deref());
    if let Some(integration) = host.integration {
        push_optional_field(
            out,
            "Integration",
            Some(if integration { "on" } else { "off" }),
        );
    }
    if let Some(reuse) = host.reuse {
        push_optional_field(out, "Reuse", Some(if reuse { "on" } else { "off" }));
    }
    if let Some(tmux) = host.tmux {
        push_optional_field(out, "Tmux", Some(if tmux { "on" } else { "off" }));
    }
    // Reserved Protocol field: preserved across a round trip, SSH-only at
    // connect time.
    push_optional_field(out, "Protocol", host.protocol.as_deref());
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
    /// tmux are left `None` so the global defaults apply — ad-hoc connections
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
            source: ConnectionHostSource::Odytty,
        }
    }
}

/// Whether a host/user token is a safe, well-formed name: non-empty ASCII of
/// letters, digits, `.`, `-`, or `_`, and never leading with `-` (which the
/// system `ssh` would read as an option — the argv `--` guard is a second line
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
    // Reject anything with whitespace outright — a real destination never has
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
    // names here are not bracketed IPv6 literals — out of scope this cycle).
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
    let existing_bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err),
    };

    // Exact-alias collision: connect, but never duplicate a saved block.
    if parse_odytty_hosts_bytes(&existing_bytes)
        .iter()
        .any(|existing| existing.alias == host.alias)
    {
        return Ok(AppendHostOutcome::AlreadyExists);
    }

    let mut block = String::new();
    push_host_block(&mut block, host);

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

    write_bytes_atomic(path, &out)?;
    Ok(AppendHostOutcome::Appended)
}

/// Atomically write `bytes` to `path`: create the parent dir, write a temp
/// sibling, then rename it over the target (atomic on Unix and Windows). A crash
/// mid-write can only leave the temp file behind, never a half-written target.
fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("hosts.conf");
    let tmp_name = format!(".{base}.{}.{nanos}.tmp", std::process::id());
    let tmp = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    };
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            Err(err)
        }
    }
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
fn render_edited_block(block: &BlockSpan, target_alias: &str, updated: &ConnectionHost) -> String {
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
    push_host_block_aliased(&mut rendered, &new_aliases, updated);
    for line in &block.unknown_lines {
        rendered.push_str(line);
        rendered.push('\n');
    }
    rendered
}

/// Splice a byte-identical edit of the block owning `target_alias`, replacing
/// only that block's body with `updated` re-rendered. Every other byte —
/// comments, blank lines, other blocks' unknown fields — is preserved. Returns
/// `None` when no block matches. Valid UTF-8 in/out; a source with invalid UTF-8
/// is lossily normalized (matching the reader), so the byte-identity guarantee
/// holds for well-formed files.
fn splice_host_block_edit(
    source: &str,
    target_alias: &str,
    updated: &ConnectionHost,
    max_chars: usize,
) -> Option<String> {
    let blocks = parse_host_blocks(source, max_chars);
    let idx = find_block(&blocks, target_alias)?;
    let block = &blocks[idx];
    let mut rendered = render_edited_block(block, target_alias, updated);
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
/// span — every other byte (comments, blank lines, unknown fields in other
/// blocks) is preserved. The write is atomic (temp sibling + rename). A missing
/// file, empty file, or unmatched alias returns [`HostsEditOutcome::NotFound`]
/// and never writes.
///
/// The one caveat, worth surfacing in an editing UI: the edited host's own
/// known fields are re-serialized to canonical form (ordering/whitespace); its
/// unknown lines and all other hosts and comments are untouched.
pub fn edit_host_block(
    path: impl AsRef<Path>,
    target_alias: &str,
    updated: &ConnectionHost,
) -> io::Result<HostsEditOutcome> {
    let path = path.as_ref();
    let source = match fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HostsEditOutcome::NotFound),
        Err(err) => return Err(err),
    };
    match splice_host_block_edit(
        &source,
        target_alias,
        updated,
        DEFAULT_CONNECTION_HOSTS_MAX_FIELD_CHARS,
    ) {
        Some(next) => {
            write_bytes_atomic(path, next.as_bytes())?;
            Ok(HostsEditOutcome::Written)
        }
        None => Ok(HostsEditOutcome::NotFound),
    }
}

/// Remove the OdyTTY-owned `Host` block whose alias list contains
/// `target_alias`, deleting its byte span and trailing blank-line separator;
/// every other byte is preserved. Atomic write. A missing/empty file or an
/// unmatched alias returns [`HostsEditOutcome::NotFound`] and never writes.
pub fn remove_host_block(
    path: impl AsRef<Path>,
    target_alias: &str,
) -> io::Result<HostsEditOutcome> {
    let path = path.as_ref();
    let source = match fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HostsEditOutcome::NotFound),
        Err(err) => return Err(err),
    };
    match splice_host_block_remove(
        &source,
        target_alias,
        DEFAULT_CONNECTION_HOSTS_MAX_FIELD_CHARS,
    ) {
        Some(next) => {
            write_bytes_atomic(path, next.as_bytes())?;
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
            protocol: block.protocol.clone(),
            identity_file: block.identity_file.clone(),
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
                block.port = v.parse::<u16>().ok();
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
    )
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
            protocol: None,
            identity_file: None,
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
    fn parse_adhoc_target_accepts_all_destination_shapes() {
        assert_eq!(
            parse_adhoc_target("host.example.invalid"),
            Some(AdhocTarget {
                user: None,
                host: "host.example.invalid".to_owned(),
                port: None,
            })
        );
        assert_eq!(
            parse_adhoc_target("deploy@host.example.invalid"),
            Some(AdhocTarget {
                user: Some("deploy".to_owned()),
                host: "host.example.invalid".to_owned(),
                port: None,
            })
        );
        assert_eq!(
            parse_adhoc_target("host.example.invalid:2200"),
            Some(AdhocTarget {
                user: None,
                host: "host.example.invalid".to_owned(),
                port: Some(2200),
            })
        );
        assert_eq!(
            parse_adhoc_target("deploy@host.example.invalid:22"),
            Some(AdhocTarget {
                user: Some("deploy".to_owned()),
                host: "host.example.invalid".to_owned(),
                port: Some(22),
            })
        );
    }

    #[test]
    fn parse_adhoc_target_rejects_unsafe_and_malformed_input() {
        // Empty / whitespace / embedded space.
        assert_eq!(parse_adhoc_target(""), None);
        assert_eq!(parse_adhoc_target("   "), None);
        assert_eq!(parse_adhoc_target("bad host"), None);
        // Option-injection leading dash (host and user parts).
        assert_eq!(parse_adhoc_target("-oProxyCommand=x"), None);
        assert_eq!(parse_adhoc_target("-host.example.invalid"), None);
        assert_eq!(parse_adhoc_target("-u@host.example.invalid"), None);
        // Missing host, empty user, double `@`.
        assert_eq!(parse_adhoc_target("user@"), None);
        assert_eq!(parse_adhoc_target("@host.example.invalid"), None);
        assert_eq!(parse_adhoc_target("a@b@host.example.invalid"), None);
        // Bad ports.
        assert_eq!(parse_adhoc_target("host.example.invalid:"), None);
        assert_eq!(parse_adhoc_target("host.example.invalid:0"), None);
        assert_eq!(parse_adhoc_target("host.example.invalid:70000"), None);
        assert_eq!(parse_adhoc_target("host.example.invalid:ssh"), None);
        assert_eq!(parse_adhoc_target("host.example.invalid:22:22"), None);
    }

    #[test]
    fn adhoc_target_builds_a_defaults_only_connection_host() {
        let host = parse_adhoc_target("deploy@host.example.invalid:2200")
            .expect("valid target")
            .to_connection_host();
        assert_eq!(host.alias, "host.example.invalid");
        assert_eq!(
            host.host_name, None,
            "alias is the host, no separate HostName"
        );
        assert_eq!(host.user.as_deref(), Some("deploy"));
        assert_eq!(host.port, Some(2200));
        // Ad-hoc carries no per-host overrides — global defaults apply.
        assert_eq!(host.integration, None);
        assert_eq!(host.reuse, None);
        assert_eq!(host.tmux, None);
    }

    #[test]
    fn append_adhoc_host_creates_file_and_writes_a_block() {
        let dir = temp_dir("odytty-adhoc-append");
        let path = dir.join("nested").join(CONNECTION_HOSTS_FILE_NAME);
        let host = parse_adhoc_target("deploy@host.example.invalid:2200")
            .expect("valid")
            .to_connection_host();

        let outcome = append_adhoc_host(&path, &host).expect("append");
        assert_eq!(outcome, AppendHostOutcome::Appended);

        let written = fs::read_to_string(&path).expect("read back");
        assert!(written.contains("Host host.example.invalid"));
        assert!(
            !written.contains("HostName"),
            "no redundant HostName for a bare host"
        );
        assert!(written.contains("    User deploy"));
        assert!(written.contains("    Port 2200"));

        // The written block round-trips through the parser.
        let reparsed = read_odytty_hosts_with_limits(&path, limits());
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].alias, "host.example.invalid");
        assert_eq!(reparsed[0].user.as_deref(), Some("deploy"));
        assert_eq!(reparsed[0].port, Some(2200));

        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn append_adhoc_host_preserves_existing_content_and_appends() {
        let dir = temp_dir("odytty-adhoc-preserve");
        let path = dir.join(CONNECTION_HOSTS_FILE_NAME);
        // Existing content with a comment and custom spacing to prove byte
        // preservation of everything before the appended block.
        let existing = "# my hosts\nHost existing\n    HostName existing.example.invalid\n";
        fs::write(&path, existing).expect("seed");

        let host = parse_adhoc_target("new.example.invalid")
            .expect("valid")
            .to_connection_host();
        let outcome = append_adhoc_host(&path, &host).expect("append");
        assert_eq!(outcome, AppendHostOutcome::Appended);

        let written = fs::read_to_string(&path).expect("read back");
        assert!(
            written.starts_with(existing),
            "existing bytes preserved verbatim"
        );
        assert!(written.contains("Host new.example.invalid"));
        // Both hosts parse.
        let reparsed = read_odytty_hosts_with_limits(&path, limits());
        assert_eq!(reparsed.len(), 2);

        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn append_adhoc_host_skips_write_on_exact_alias_collision() {
        let dir = temp_dir("odytty-adhoc-collision");
        let path = dir.join(CONNECTION_HOSTS_FILE_NAME);
        let existing = "Host host.example.invalid\n    User someone\n";
        fs::write(&path, existing).expect("seed");

        let host = parse_adhoc_target("host.example.invalid")
            .expect("valid")
            .to_connection_host();
        let outcome = append_adhoc_host(&path, &host).expect("append");
        assert_eq!(outcome, AppendHostOutcome::AlreadyExists);

        // File untouched byte-for-byte.
        let written = fs::read_to_string(&path).expect("read back");
        assert_eq!(written, existing);

        fs::remove_dir_all(dir).ok();
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

        // The field survives a render/reload round-trip.
        let mut formatted = String::new();
        push_host_block(&mut formatted, &entries[0]);
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

        let mut formatted = String::new();
        push_host_block(&mut formatted, &entries[0]);
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

        let mut formatted = String::new();
        push_host_block(&mut formatted, &entries[0]);
        assert!(formatted.contains("Tmux on"));
        let reparsed = parse_odytty_hosts_bytes_with_limits(formatted.as_bytes(), limits());
        assert_eq!(reparsed[0].tmux, Some(true));
    }

    #[test]
    fn append_round_trips_odytty_owned_hosts() {
        let dir = temp_dir("odytty-connection-hosts");
        let path = hosts_file_path(&dir);
        let mut entry = host("web 1", ConnectionHostSource::Odytty);
        entry.user = Some("deploy".to_owned());
        entry.title = Some("Synthetic Web".to_owned());

        append_adhoc_host(&path, &entry).expect("append synthetic host");
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

    // ---- ODP-4B: block-span byte-splice edit / remove ----

    /// A hand-annotated fixture: a leading comment, three blocks (one
    /// multi-alias), an in-block comment, an unknown field, a tab indent, and
    /// blank-line separators — the shapes an in-place edit must not disturb.
    fn annotated_fixture() -> String {
        [
            "# OdyTTY hosts \u{2014} hand annotated",
            "",
            "Host alpha",
            "    HostName alpha.example.invalid",
            "    User alice",
            "    # alpha is the bastion",
            "    XCustomField keep-me",
            "",
            "Host beta gamma",
            "\tHostName beta.example.invalid",
            "    User bob",
            "",
            "# a note that belongs to delta",
            "Host delta",
            "    HostName delta.example.invalid",
            "",
        ]
        .join("\n")
    }

    fn max_chars() -> usize {
        limits().max_field_chars
    }

    #[test]
    fn edit_replaces_only_the_target_block_bytes() {
        let orig = annotated_fixture();
        let mut updated = host("beta", ConnectionHostSource::Odytty);
        updated.host_name = Some("beta.example.invalid".to_owned());
        updated.user = Some("bob-2".to_owned());

        let edited =
            splice_host_block_edit(&orig, "beta", &updated, max_chars()).expect("beta edited");

        // Everything before the edited block is byte-identical...
        let head = orig.find("Host beta gamma").expect("beta present");
        assert_eq!(
            &edited[..head],
            &orig[..head],
            "preamble + alpha block untouched"
        );
        // ...and everything from the delta preamble comment onward is too.
        let tail = orig
            .find("# a note that belongs to delta")
            .expect("delta note present");
        assert!(
            edited.ends_with(&orig[tail..]),
            "delta note + delta block untouched"
        );
        // The edited block took the new user and canonicalized its tab indent;
        // its sibling alias survived.
        assert!(edited.contains("Host beta gamma"), "sibling alias kept");
        assert!(edited.contains("    User bob-2"));
        assert!(
            !edited.contains("\tHostName beta"),
            "tab indent canonicalized"
        );
        assert!(!edited.contains("    User bob\n"), "old user replaced");
    }

    #[test]
    fn edit_preserves_unknown_lines_inside_the_edited_block() {
        let orig = annotated_fixture();
        let mut updated = host("alpha", ConnectionHostSource::Odytty);
        updated.host_name = Some("alpha.example.invalid".to_owned());
        updated.user = Some("alice-2".to_owned());

        let edited =
            splice_host_block_edit(&orig, "alpha", &updated, max_chars()).expect("alpha edited");

        assert!(edited.contains("    User alice-2"), "new user applied");
        // The unknown field and in-block comment survive, re-emitted after the
        // known fields.
        assert!(
            edited.contains("    XCustomField keep-me"),
            "unknown field kept"
        );
        assert!(
            edited.contains("    # alpha is the bastion"),
            "in-block comment kept"
        );
        let user_at = edited.find("User alice-2").unwrap();
        let unknown_at = edited.find("XCustomField keep-me").unwrap();
        assert!(
            unknown_at > user_at,
            "unknown lines follow the known fields"
        );
    }

    #[test]
    fn edit_then_parse_round_trips_the_new_value() {
        let dir = temp_dir("odytty-edit-parse");
        let path = hosts_file_path(&dir);
        fs::write(&path, annotated_fixture()).expect("seed");

        let mut updated = host("alpha", ConnectionHostSource::Odytty);
        updated.host_name = Some("alpha.example.invalid".to_owned());
        updated.user = Some("alice-2".to_owned());
        let outcome = edit_host_block(&path, "alpha", &updated).expect("edit");
        assert_eq!(outcome, HostsEditOutcome::Written);

        let reparsed = read_odytty_hosts_with_limits(&path, limits());
        let alpha = reparsed
            .iter()
            .find(|h| h.alias == "alpha")
            .expect("alpha still present");
        assert_eq!(alpha.user.as_deref(), Some("alice-2"));
        // The unknown field remains in the raw file after the round trip.
        let raw = fs::read_to_string(&path).expect("read back");
        assert!(raw.contains("XCustomField keep-me"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remove_middle_block_leaves_no_doubled_gap() {
        let orig = annotated_fixture();
        let edited = splice_host_block_remove(&orig, "beta", max_chars()).expect("beta removed");

        let head = orig.find("Host beta gamma").unwrap();
        assert_eq!(&edited[..head], &orig[..head], "prefix untouched");
        let tail = orig.find("# a note that belongs to delta").unwrap();
        assert!(edited.ends_with(&orig[tail..]), "suffix untouched");
        assert!(!edited.contains("Host beta gamma"));
        assert!(!edited.contains("User bob"));
        assert!(
            !edited.contains("\n\n\n"),
            "trailing blank consumed, no doubled gap"
        );
    }

    #[test]
    fn remove_first_block_keeps_the_preamble() {
        let orig = annotated_fixture();
        let edited = splice_host_block_remove(&orig, "alpha", max_chars()).expect("alpha removed");

        let head = orig.find("Host alpha").unwrap();
        assert_eq!(&edited[..head], &orig[..head], "leading comment preserved");
        let tail = orig.find("Host beta gamma").unwrap();
        assert!(
            edited.ends_with(&orig[tail..]),
            "remaining blocks untouched"
        );
        assert!(edited.starts_with("# OdyTTY hosts"));
        assert!(!edited.contains("Host alpha"));
        assert!(
            !edited.contains("XCustomField"),
            "alpha's unknown field left with it"
        );
    }

    #[test]
    fn remove_last_block_orphans_only_its_own_body() {
        let orig = annotated_fixture();
        let edited = splice_host_block_remove(&orig, "delta", max_chars()).expect("delta removed");

        let head = orig.find("Host delta").unwrap();
        assert_eq!(&edited[..head], &orig[..head], "prefix untouched");
        assert!(!edited.contains("Host delta"));
        // The note above delta is its preamble (blank-line separated), owned by
        // no block, so it stays.
        assert!(edited.contains("# a note that belongs to delta"));
        assert!(edited.contains("Host beta gamma"));
    }

    #[test]
    fn edit_and_remove_on_a_missing_file_report_not_found() {
        let dir = temp_dir("odytty-edit-missing");
        let path = dir.join("does-not-exist").join(CONNECTION_HOSTS_FILE_NAME);
        let updated = host("x", ConnectionHostSource::Odytty);

        assert_eq!(
            edit_host_block(&path, "x", &updated).expect("edit missing"),
            HostsEditOutcome::NotFound
        );
        assert_eq!(
            remove_host_block(&path, "x").expect("remove missing"),
            HostsEditOutcome::NotFound
        );
        assert!(!path.exists(), "a not-found edit never creates the file");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn edit_on_empty_or_unmatched_file_leaves_it_untouched() {
        let dir = temp_dir("odytty-edit-untouched");
        let path = hosts_file_path(&dir);

        // Empty file: nothing to edit.
        fs::write(&path, b"").expect("seed empty");
        assert_eq!(
            edit_host_block(&path, "any", &host("any", ConnectionHostSource::Odytty))
                .expect("edit empty"),
            HostsEditOutcome::NotFound
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "");

        // Populated file, alias that matches no block: bytes unchanged.
        let fixture = annotated_fixture();
        fs::write(&path, &fixture).expect("seed fixture");
        assert_eq!(
            remove_host_block(&path, "nonexistent").expect("remove unmatched"),
            HostsEditOutcome::NotFound
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), fixture);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn edit_stays_byte_aligned_at_eof_without_a_trailing_newline() {
        let orig = "Host solo\n    User x".to_owned();
        let mut updated = host("solo", ConnectionHostSource::Odytty);
        updated.host_name = None;
        updated.user = Some("y".to_owned());

        let edited =
            splice_host_block_edit(&orig, "solo", &updated, max_chars()).expect("solo edited");
        assert_eq!(
            edited, "Host solo\n    User y",
            "no trailing newline injected"
        );
    }

    #[test]
    fn protocol_field_parses_emits_and_preserves_odd_values() {
        // The reserved Protocol field parses and round-trips through a render.
        let entries = parse_odytty_hosts_bytes_with_limits(
            b"Host p\n    HostName p.example.invalid\n    Protocol ssh\n",
            limits(),
        );
        assert_eq!(entries[0].protocol.as_deref(), Some("ssh"));
        let mut rendered = String::new();
        push_host_block(&mut rendered, &entries[0]);
        assert!(rendered.contains("Protocol ssh"));

        // A non-`ssh` value is preserved verbatim (reserved, forward-compatible)
        // rather than dropped, so a hand-added protocol survives a reparse.
        let odd = parse_odytty_hosts_bytes_with_limits(b"Host q\n    Protocol quic\n", limits());
        assert_eq!(odd[0].protocol.as_deref(), Some("quic"));
    }

    #[test]
    fn identity_file_parses_emits_and_survives_an_edit() {
        // ODP-9 Tier 1: IdentityFile stores a key PATH (never a secret) and
        // round-trips through a render.
        let entries = parse_odytty_hosts_bytes_with_limits(
            b"Host k\n    HostName k.example.invalid\n    IdentityFile /home/user/.ssh/id_ed25519.example\n",
            limits(),
        );
        assert_eq!(
            entries[0].identity_file.as_deref(),
            Some("/home/user/.ssh/id_ed25519.example")
        );
        let mut rendered = String::new();
        push_host_block(&mut rendered, &entries[0]);
        assert!(rendered.contains("IdentityFile /home/user/.ssh/id_ed25519.example"));

        // An in-place edit that sets the identity path splices it in while every
        // other byte is preserved.
        let dir = temp_dir("odytty-identity-edit");
        let path = hosts_file_path(&dir);
        fs::write(&path, annotated_fixture()).expect("seed");
        let mut updated = host("alpha", ConnectionHostSource::Odytty);
        updated.host_name = Some("alpha.example.invalid".to_owned());
        updated.identity_file = Some("/home/user/.ssh/alpha.example".to_owned());
        assert_eq!(
            edit_host_block(&path, "alpha", &updated).expect("edit"),
            HostsEditOutcome::Written
        );
        let reparsed = read_odytty_hosts_with_limits(&path, limits());
        let alpha = reparsed.iter().find(|h| h.alias == "alpha").expect("alpha");
        assert_eq!(
            alpha.identity_file.as_deref(),
            Some("/home/user/.ssh/alpha.example")
        );
        let raw = fs::read_to_string(&path).expect("read back");
        assert!(
            raw.contains("XCustomField keep-me"),
            "unknown field preserved"
        );
        fs::remove_dir_all(dir).ok();
    }
}

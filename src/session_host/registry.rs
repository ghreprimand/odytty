// SPDX-License-Identifier: GPL-3.0-only
//! Local-only session registry helpers for the public CLI surface.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use super::SessionHostClient;
use super::protocol::ListedSession;
use super::socket::{
    existing_runtime_dir, session_id_from_socket_name, session_metadata_path, session_socket_path,
};

const METADATA_MODE: u32 = 0o600;
/// Detached-session metadata contains five short text fields. This generous
/// ceiling leaves ample room for future compatible fields while preventing a
/// replaced file from forcing an unbounded allocation during session listing.
pub(super) const MAX_SESSION_METADATA_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub id: String,
    pub name: String,
    pub created_unix_ms: u128,
    pub pane_count: usize,
}

pub fn write_session_metadata(runtime_dir: &Path, metadata: &SessionMetadata) -> Result<()> {
    let path = session_metadata_path(runtime_dir, &metadata.id)?;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .with_context(|| format!("write session metadata {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(METADATA_MODE))
        .with_context(|| format!("chmod session metadata {}", path.display()))?;
    writeln!(file, "version=1")?;
    writeln!(file, "id={}", escape_metadata_value(&metadata.id))?;
    writeln!(file, "name={}", escape_metadata_value(&metadata.name))?;
    writeln!(file, "created_unix_ms={}", metadata.created_unix_ms)?;
    writeln!(file, "pane_count={}", metadata.pane_count)?;
    Ok(())
}

pub fn read_session_metadata(runtime_dir: &Path, id: &str) -> Result<Option<SessionMetadata>> {
    let path = session_metadata_path(runtime_dir, id)?;
    let text = match read_session_metadata_text(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read session metadata {}", path.display()));
        }
    };

    let mut version: Option<&str> = None;
    let mut metadata_id = None;
    let mut name = None;
    let mut created_unix_ms = None;
    let mut pane_count = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "version" => version = Some(value),
            "id" => metadata_id = Some(unescape_metadata_value(value)),
            "name" => name = Some(unescape_metadata_value(value)),
            "created_unix_ms" => created_unix_ms = value.parse::<u128>().ok(),
            "pane_count" => pane_count = value.parse::<usize>().ok(),
            _ => {}
        }
    }

    // Version gate (audit C27): every writer stamps `version=1`, and this
    // reader only knows v1 semantics. A file declaring any other version (a
    // newer binary's format, or a corrupted line) must not be half-parsed with
    // v1 rules — treat it like a missing file so the caller falls back to
    // defaults. A file with no `version=` line at all is tolerated as v1.
    if let Some(version) = version
        && version.trim() != "1"
    {
        return Ok(None);
    }
    let Some(metadata_id) = metadata_id.filter(|metadata_id| metadata_id == id) else {
        return Ok(None);
    };
    Ok(Some(SessionMetadata {
        id: metadata_id,
        name: name.unwrap_or_else(|| id.to_owned()),
        created_unix_ms: created_unix_ms.unwrap_or_else(now_unix_ms),
        pane_count: pane_count.unwrap_or(1).max(1),
    }))
}

fn read_session_metadata_text(path: &Path) -> io::Result<String> {
    let file = crate::state_dir::open_existing_sensitive(path)?;
    let len = file.metadata()?.len();
    if len > MAX_SESSION_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "session metadata is {len} bytes, over the {MAX_SESSION_METADATA_BYTES}-byte limit"
            ),
        ));
    }

    // Read at most one byte past the ceiling. The descriptor metadata check
    // rejects an already-oversized file cheaply; this second bound also catches
    // a regular file that grows between metadata() and the read.
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(MAX_SESSION_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SESSION_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("session metadata exceeds the {MAX_SESSION_METADATA_BYTES}-byte limit"),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "session metadata is not valid UTF-8",
        )
    })
}

/// Directory entries examined per listing. The runtime directory is private
/// to the user, but a runaway producer must still not turn one Navigator open
/// into an unbounded walk.
pub(super) const MAX_REGISTRY_ENTRIES_EXAMINED: usize = 4096;

/// How a session socket answered a liveness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SocketLiveness {
    /// A host accepted the connection.
    Listening,
    /// No host is listening: the socket is missing or stale.
    Absent,
    /// A host may exist but did not take the connection (for example a full
    /// listen backlog), or the probe failed for another reason.
    Unresponsive,
}

/// Probe a session socket without a handshake. The probe makes one
/// nonblocking connect attempt and closes it at once: no hello is sent, so the
/// host never captures or encodes a snapshot for a listing, and a wedged host
/// cannot make the caller wait.
pub(super) fn probe_socket(socket_path: &Path) -> SocketLiveness {
    match super::connect::connect_within(socket_path, Duration::ZERO) {
        Ok(_) => SocketLiveness::Listening,
        Err(error) if is_absent_socket_error(&error) => SocketLiveness::Absent,
        Err(_) => SocketLiveness::Unresponsive,
    }
}

/// Connect errors that mean no host is serving the socket.
fn is_absent_socket_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
        || matches!(
            error.raw_os_error(),
            Some(libc::ECONNREFUSED | libc::ENOENT)
        )
}

/// List detached sessions. Each socket is classified with [`probe_socket`];
/// stale sockets are skipped, and a host that exists but does not take the
/// connection is listed as `unresponsive` rather than hidden. Listing never
/// attaches, never waits on a host, and examines at most
/// [`MAX_REGISTRY_ENTRIES_EXAMINED`] directory entries.
pub fn list_live_sessions(runtime_base: Option<&Path>) -> Result<Vec<ListedSession>> {
    let Some(runtime_dir) = existing_runtime_dir(runtime_base)? else {
        return Ok(Vec::new());
    };
    let now = now_unix_ms();
    let mut sessions = Vec::new();
    for entry in fs::read_dir(&runtime_dir)
        .with_context(|| format!("read session runtime dir {}", runtime_dir.display()))?
        .take(MAX_REGISTRY_ENTRIES_EXAMINED)
    {
        // Per-entry failures skip THAT entry: one unreadable dirent, invalid
        // socket path, or corrupt metadata file must not abort the whole
        // listing and hide every other live session.
        let Ok(entry) = entry else {
            continue;
        };
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(id) = session_id_from_socket_name(file_name) else {
            continue;
        };
        let Ok(socket_path) = session_socket_path(&runtime_dir, id) else {
            continue;
        };
        let state = match probe_socket(&socket_path) {
            SocketLiveness::Listening => "running",
            SocketLiveness::Unresponsive => "unresponsive",
            SocketLiveness::Absent => continue,
        };
        // A live session with unreadable metadata still lists, using the
        // id-derived fallbacks below, rather than failing the whole listing.
        let metadata = read_session_metadata(&runtime_dir, id).unwrap_or(None);
        let created_unix_ms = metadata
            .as_ref()
            .map(|metadata| metadata.created_unix_ms)
            .unwrap_or_else(|| socket_created_unix_ms(&socket_path).unwrap_or(now));
        sessions.push(ListedSession {
            id: id.to_owned(),
            name: metadata
                .as_ref()
                .map(|metadata| metadata.name.clone())
                .unwrap_or_else(|| id.to_owned()),
            state,
            age_ms: now.saturating_sub(created_unix_ms),
            pane_count: metadata
                .as_ref()
                .map(|metadata| metadata.pane_count)
                .unwrap_or(1)
                .max(1),
        });
    }
    sessions.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(sessions)
}

/// Terminate a detached session by id: resolve its socket, connect, and send a
/// [`ClientFrame::Shutdown`](super::protocol::ClientFrame::Shutdown). The host
/// SIGHUPs its shell, exits, and unlinks the socket, so the session leaves the
/// registry.
///
/// Only a session that is provably gone counts as success without a shutdown:
/// a missing runtime directory, or a socket that is missing or refuses the
/// connection (no host listening), so a double-kill or a race with idle
/// timeout stays quiet. Every other failure is returned: a host that does not
/// answer its hello, rejects the handshake, or cannot take the connection is
/// still alive and still listed, and the caller must say so. The connect and
/// hello are bounded by the client's hello deadline. `runtime_base` is `None`
/// in production (derived from `XDG_RUNTIME_DIR`); tests pass an explicit base.
pub fn kill_session(runtime_base: Option<&Path>, id: &str) -> Result<()> {
    let Some(runtime_dir) = existing_runtime_dir(runtime_base)? else {
        return Ok(());
    };
    let socket_path = session_socket_path(&runtime_dir, id)?;
    let mut client = match SessionHostClient::connect(&socket_path, id) {
        Ok(client) => client,
        Err(error) if session_already_gone(&error) => return Ok(()),
        Err(error) => return Err(error.context(format!("end session {id}"))),
    };
    // Drain the post-handshake snapshot frame before sending Shutdown. The host
    // writes the snapshot right after the hello; if we dropped the connection
    // before reading it, that write would race a `BrokenPipe` and make the host
    // exit through its error path instead of the clean Shutdown teardown.
    // Reading one frame synchronizes past the snapshot write so the host always
    // tears down cleanly and unlinks its socket. A read error is non-fatal; the
    // kill is still sent.
    let _ = client.read_frame(Duration::from_millis(200));
    client
        .shutdown()
        .with_context(|| format!("end session {id}"))?;
    Ok(())
}

/// Whether a connect failure means the session no longer exists: the root
/// cause is the socket connect itself reporting no listener.
fn session_already_gone(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<io::Error>())
        .any(is_absent_socket_error)
}

fn socket_created_unix_ms(path: &Path) -> Result<u128> {
    Ok(fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis())
}

pub fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn escape_metadata_value(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            ch => escaped.push(ch),
        }
    }
    escaped
}

fn unescape_metadata_value(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

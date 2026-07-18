// SPDX-License-Identifier: GPL-3.0-only
//! Local-only session registry helpers for the public CLI surface.

use std::fs::{self, OpenOptions};
use std::io::Write;
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
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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

pub fn list_live_sessions(runtime_base: Option<&Path>) -> Result<Vec<ListedSession>> {
    let Some(runtime_dir) = existing_runtime_dir(runtime_base)? else {
        return Ok(Vec::new());
    };
    let now = now_unix_ms();
    let mut sessions = Vec::new();
    for entry in fs::read_dir(&runtime_dir)
        .with_context(|| format!("read session runtime dir {}", runtime_dir.display()))?
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
        let Ok(mut client) = SessionHostClient::connect(&socket_path, id) else {
            continue;
        };
        let _ = client.read_frame(Duration::from_millis(200));
        let _ = client.detach();
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
            state: "running",
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
/// registry. Idempotent-ish: a missing runtime dir or a dead/absent socket means
/// the session is already gone, which is success, not an error — so a double-kill
/// or a race with idle-timeout never surfaces a failure. `runtime_base` is `None`
/// in production (derived from `XDG_RUNTIME_DIR`); tests pass an explicit base.
pub fn kill_session(runtime_base: Option<&Path>, id: &str) -> Result<()> {
    let Some(runtime_dir) = existing_runtime_dir(runtime_base)? else {
        return Ok(());
    };
    let socket_path = session_socket_path(&runtime_dir, id)?;
    let mut client = match SessionHostClient::connect(&socket_path, id) {
        Ok(client) => client,
        // A dead or absent socket = the session is already gone. The host
        // unlinks its socket on exit, so connect failing here is the expected
        // "already reaped" outcome, not an error.
        Err(_) => return Ok(()),
    };
    // Drain the post-handshake snapshot frame before sending Shutdown, exactly
    // like `list_live_sessions`. The host writes the snapshot right after the
    // hello; if we dropped the connection before reading it, that write would
    // race a `BrokenPipe` and make the host exit through its error path instead
    // of the clean Shutdown teardown. Reading one frame synchronizes past the
    // snapshot write so the host always tears down cleanly and unlinks its
    // socket. A read error is non-fatal — we still send the kill.
    let _ = client.read_frame(Duration::from_millis(200));
    client.shutdown()?;
    Ok(())
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

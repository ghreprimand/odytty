// SPDX-License-Identifier: GPL-3.0-only
//! Detached session-host foundation for resumable OdyTTY sessions.
//!
//! This module is intentionally outside `src/native/`: it owns process/socket
//! lifecycle, PTY pumping, and terminal snapshots without importing windowing,
//! GPU, or render code.

// The socket transport (Unix-domain sockets, flock, mode bits, XDG_RUNTIME_DIR)
// is Unix-only. `protocol` stays ungated — it is pure wire types (including the
// relocated `ListedSession`) with no platform dependency, preserving a future
// named-pipe transport on Windows without re-untangling.
#[cfg(unix)]
mod client;
#[cfg(unix)]
mod host;
pub mod protocol;
#[cfg(unix)]
mod pty_writer;
#[cfg(unix)]
mod registry;
#[cfg(unix)]
mod socket;

// `ListedSession` is a pure type and lives in `protocol`; it is re-exported here
// (ungated) so the always-compiled attach overlay and the `list` formatter keep
// their existing `crate::session_host::ListedSession` import path on every
// platform (the list is simply always empty on Windows).
pub use protocol::ListedSession;

#[cfg(unix)]
pub use client::SessionHostClient;
#[cfg(unix)]
pub use host::{
    HostCommand, HostConfig, HostExit, HostExitReason, MAX_HOST_SESSIONS, run_host,
    run_internal_host_from_args, spawn_host_on_demand,
};
#[cfg(unix)]
pub use registry::{
    SessionMetadata, kill_session, list_live_sessions, now_unix_ms, read_session_metadata,
    write_session_metadata,
};
#[cfg(unix)]
pub(crate) use socket::SocketReadDeadline;
#[cfg(unix)]
pub use socket::{
    RuntimePaths, StartupLock, cleanup_stale_socket, existing_runtime_dir, prepare_runtime_dir,
    runtime_base_from_env, runtime_dir_path, runtime_paths, session_id_from_socket_name,
    session_metadata_path, session_socket_path, validate_runtime_dir, validate_socket_parent,
};

#[cfg(all(test, unix))]
mod tests;

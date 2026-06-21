// SPDX-License-Identifier: GPL-3.0-only
//! Detached session-host foundation for resumable OdyTTY sessions.
//!
//! This module is intentionally outside `src/native/`: it owns process/socket
//! lifecycle, PTY pumping, and terminal snapshots without importing windowing,
//! GPU, or render code.

mod client;
mod host;
pub mod protocol;
mod socket;

pub use client::SessionHostClient;
pub use host::{
    HostCommand, HostConfig, HostExit, HostExitReason, MAX_HOST_SESSIONS, run_host,
    run_internal_host_from_args, spawn_host_on_demand,
};
pub use socket::{
    RuntimePaths, StartupLock, cleanup_stale_socket, prepare_runtime_dir, runtime_base_from_env,
    runtime_paths, session_socket_path, validate_runtime_dir, validate_socket_parent,
};

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: GPL-3.0-only
//! Session and workspace state facade.
//!
//! [`WorkspaceSet`] owns one flat arena of [`Session`]s keyed by
//! [`SessionToken`], plus the workspace, tab, and pane trees that reference
//! those tokens. The implementation is split by responsibility; this file keeps
//! the module paths every caller already uses.
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`model`] | Tokens, session, tab and workspace fields, arena and structural accessors |
//! | [`transport`] | Sources, construction, pump, local, remote, attach, upload, reconnect, backend resize |
//! | [`presentation`] | Cursor, title, viewport, timers, latches, geometry and tab-bar data |
//! | [`lifecycle`] | Bounded joins, close, shutdown, exit, removal, pane, tab and workspace lifecycle |
//! | [`persistence`] | Capture, restore, append, validation, fingerprint and rollback |
//!
//! Dependency direction runs model first, then transport, presentation,
//! lifecycle, and persistence.

mod lifecycle;
mod model;
mod persistence;
mod presentation;
mod transport;

#[cfg(test)]
mod tests;

pub(super) use lifecycle::SHUTDOWN_REAP_DEADLINE;
pub(super) use model::{Session, SessionToken, WorkspaceSet};
pub(super) use persistence::RestoreReport;
pub(super) use presentation::CursorComparison;
pub(super) use transport::{apply_local_backend_caps, seed_initial_working_directory};

#[cfg(not(test))]
pub(super) use transport::RemoteUploadJob;

/// How long an interactive attach waits for the host's initial snapshot frame,
/// and the aggregate ceiling for a whole reattach batch, before giving up.
/// Bounded so a stalled or misbehaving host cannot hang window startup forever.
/// Defined here (cross-platform) because the restore path is compiled on every
/// platform; the Unix-only attach transport re-exports it for its own use.
pub(super) const SNAPSHOT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
pub(super) use transport::HeadlessSession;

// SPDX-License-Identifier: GPL-3.0-only
//! Versioned, OdyTTY-owned terminal snapshot envelope for resumable sessions.
//!
//! This is the Phase 2 persistence boundary: it serializes owned DTOs copied
//! out of the terminal model, not private `Screen` / `Scrollback` internals and
//! not third-party derives over those internals. The first format version keeps
//! the state subset intentionally constrained: dimensions, visible grid,
//! bounded physical scrollback, cursor, and basic terminal modes.
//!
//! Responsibilities are split so each one can be reviewed on its own, and the
//! dependency direction runs strictly downwards:
//!
//! - [`format`] -- wire identity: magic, versions, section ids, pinned sizes.
//! - [`compat`] -- which format versions decode here and what appended fields
//!   default to for older ones.
//! - [`caps`] -- capture limits and decode-side resource caps.
//! - [`error`] -- the refusal vocabulary shared by both directions.
//! - [`model`] -- the owned DTOs and their pure terminal-type conversions.
//! - [`validate`] -- the bounds a field must satisfy before it is narrowed.
//! - [`encode`] -- producing wire bytes.
//! - [`decode`] -- reading untrusted wire bytes back.
//! - [`capture`] -- copying live terminal state into an envelope its own
//!   default decoder is guaranteed to accept.
//!
//! `format`, `compat`, `caps`, and `error` depend on nothing above them;
//! `model` depends only on those; `encode`, `decode`, `validate`, and
//! `capture` sit on top and never depend on each other's internals except
//! through the row/prelude writers `capture` needs to measure its own budget.

mod caps;
mod capture;
mod compat;
mod decode;
mod encode;
mod error;
mod format;
mod model;
mod validate;

pub use caps::{SnapshotCaptureLimits, SnapshotEnvelopeCaps};
pub use error::SnapshotEnvelopeError;
pub use format::{SNAPSHOT_FORMAT_VERSION, SNAPSHOT_MAGIC, SNAPSHOT_PROTOCOL_VERSION};
pub use model::{
    SnapshotAttrs, SnapshotBasicModes, SnapshotCell, SnapshotEnvelope, SnapshotLayoutState,
    SnapshotMetadata, SnapshotPromptMark, SnapshotRow, SnapshotScrollRegion, SnapshotTerminalState,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_bound_tests;

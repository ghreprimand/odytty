// SPDX-License-Identifier: GPL-3.0-only
//! Cross-version acceptance policy for decoded snapshots.
//!
//! The format has no rewriting migration step: every accepted older version is
//! read directly by the current decoder, and each field appended by a later
//! version restores at a documented power-on default. This module holds the
//! two decisions that policy consists of -- which versions are accepted, and
//! from which version each appended field is present on the wire -- so a
//! version bump changes them in one place rather than in scattered decoder
//! comparisons.

use super::format::{SNAPSHOT_FORMAT_VERSION, SNAPSHOT_PROTOCOL_VERSION};

/// Format version that first carried the packed charset-mode byte. Snapshots
/// older than this restore at the charset power-on state (ASCII G0/G1, GL=G0).
pub(super) const CHARSET_MODES_MIN_FORMAT_VERSION: u16 = 3;

/// Whether this build decodes a snapshot carrying these header versions.
///
/// Format versions are accepted from 1 up to the version this build writes;
/// the protocol version must match exactly, because it describes the attach
/// conversation rather than the container layout.
pub(super) fn is_supported_version(format_version: u16, protocol_version: u16) -> bool {
    (1..=SNAPSHOT_FORMAT_VERSION).contains(&format_version)
        && protocol_version == SNAPSHOT_PROTOCOL_VERSION
}

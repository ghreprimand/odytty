// SPDX-License-Identifier: GPL-3.0-only
//! Bounds for external palette file reads and parsing.

/// Maximum bytes read from one palette source file.
pub const MAX_EXTERNAL_PALETTE_BYTES: u64 = 64 * 1024;
/// Maximum physical lines scanned in a line-oriented palette file.
pub const MAX_EXTERNAL_PALETTE_LINES: usize = 4_096;
/// Maximum key/value entries retained while parsing.
pub const MAX_EXTERNAL_PALETTE_ENTRIES: usize = 512;
/// Poll interval for content-hash palette watching (mirrors config reload cadence).
pub const EXTERNAL_PALETTE_POLL_INTERVAL_MS: u64 = 750;

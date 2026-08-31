// SPDX-License-Identifier: GPL-3.0-only
//! Provider-neutral external palette following (v0.14 Phase B).
//!
//! Public feature name: **External palette following**. Third-party formats
//! (Omarchy-compatible `colors.toml`, pywal-compatible `colors.json`, and
//! Base16 key aliases) appear only as independent compatibility details.
//! Support is independent: no endorsement, partnership, official integration,
//! or upstream modification is implied or required.
//!
//! Contract version 1: a complete, validated color payload projects into the
//! existing [`Theme`] / [`ThemeSpec`] seam. Partial, malformed, oversized, or
//! unsupported sources fail closed and never blend with built-in defaults.

mod fingerprint;
mod follow;
mod limits;
mod parse;

pub use fingerprint::{ContentFingerprint, fingerprint_bytes};
pub use follow::{
    ExternalPaletteFollow, FollowPollOutcome, FollowStatus, palette_read_count_for_test,
    reset_palette_read_count_for_test,
};
pub use limits::*;
pub use parse::{
    ExternalPaletteError, ExternalPaletteProvider, NormalizedExternalPalette, parse_palette_bytes,
};

/// Version of the provider-neutral external-palette contract.
pub const EXTERNAL_PALETTE_CONTRACT_VERSION: u32 = 1;

#[cfg(test)]
mod tests;

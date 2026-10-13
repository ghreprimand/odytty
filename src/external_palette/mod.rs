// SPDX-License-Identifier: GPL-3.0-only
//! Follow complete local palettes through ThemeSpec without mixing defaults.
//!
//! Public feature name: **External palette following**. Compatibility formats
//! (`colors.toml`, `colors.json` and Base16 key aliases) are named only where
//! a format has to be identified; no endorsement, partnership or integration is
//! implied.
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

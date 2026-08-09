// SPDX-License-Identifier: GPL-3.0-only
//! CPU-side text support: font loading and terminal color resolution.
//!
//! This module is deliberately GPU-agnostic so it can be unit-tested without a
//! window or `wgpu` device. The monospace glyph atlas lives in [`crate::atlas`];
//! its [`CellSize`]/[`GlyphAtlas`] types are re-exported here so existing
//! `crate::text::…` call sites keep resolving. The native renderer uses the
//! color helpers below plus the atlas to build per-cell quads.
//!
//! ## Font sourcing
//!
//! The default text face is bundled Victor Mono (JetBrains Mono is also bundled
//! and selectable). The settings layer can still
//! provide an explicit font path or system font family; bad overrides fall back
//! to the bundled face so startup never depends on host font installation.
//!
//! ## Module boundaries
//!
//! This file is a facade: it owns no logic, and re-exports each submodule so
//! every historical `crate::text::…` path keeps resolving. The submodules are
//! private, so the facade remains the only path to them and the split adds no
//! public surface.
//!
//! | Module | Owns |
//! |---|---|
//! | [`bundled`] | compiled-in faces, font-file loading, [`TextError`] |
//! | [`discovery`] | search directories, font-file collection, name normalization |
//! | [`face_meta`] | font-table metadata and the family inventory built from it |
//! | [`resolve`] | choosing a face for a requested family or weight |
//! | [`symbols`] | symbol/Nerd-font fallback order and source labelling |
//! | [`metrics`] | raster and advance probes on a loaded face |
//! | [`color`] | default/ANSI palettes, runtime overrides, sRGB and linear math |
//! | [`symbol_map`] | SYMMAP codepoint-range rules |
//!
//! Dependencies run one way: `resolve` reads `discovery` and `face_meta`,
//! `symbols` reads `discovery`, `bundled` and `metrics`, and `color` and
//! `symbol_map` depend on neither. Nothing depends on the facade.

/// The glyph atlas and its cell metrics live in [`crate::atlas`]; re-exported
/// here so `crate::text::{CellSize, GlyphAtlas}` call sites keep resolving.
pub use crate::atlas::{CellSize, FontStyle, GlyphAtlas, SubpixelMode};

mod bundled;
mod color;
mod discovery;
mod face_meta;
mod metrics;
mod resolve;
mod symbol_map;
mod symbols;

pub use bundled::*;
pub use color::*;
pub use discovery::*;
pub use face_meta::*;
pub use metrics::*;
pub use resolve::*;
pub use symbol_map::*;
pub use symbols::*;

#[cfg(test)]
mod tests;

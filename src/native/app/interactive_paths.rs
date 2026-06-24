// SPDX-License-Identifier: GPL-3.0-only
//! INTERACTIVE-PATHS — the production stat-gate probe.
//!
//! The pure path engine (`crate::paths`) is std-only and deliberately never
//! touches the filesystem; its single I/O seam is the [`ResolveProbe`] trait.
//! This module supplies the one production implementation, [`FsResolveProbe`],
//! which lives in `native/` precisely so `src/paths/` stays pure. The probe is
//! a zero-field struct constructed at the hover site (Phase 7) — its only job is
//! to classify an absolute path as a file or directory, or report that it does
//! not exist, via a single `symlink_metadata` call.
//!
//! `symlink_metadata` (not `metadata`) is used so a symlink is classified by the
//! link itself rather than its target — no traversal, no following into a
//! possibly-hostile target, and no surprise on a dangling link. The call only
//! runs when `interactive_paths` is on AND a syntactic path span sits under the
//! pointer, so the default (feature-off) path makes zero `stat` calls.

use crate::paths::{FsKind, ResolveProbe};

/// Production stat-gate: classifies an absolute path via `std::fs`. Only the
/// `cfg(not(test))` hover arm constructs it; under the test target the hover
/// path uses [`MapProbe`] instead, so the struct is (correctly) unused there.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct FsResolveProbe;

impl ResolveProbe for FsResolveProbe {
    fn classify(&self, abs_path: &str) -> Option<FsKind> {
        let meta = std::fs::symlink_metadata(abs_path).ok()?;
        Some(if meta.is_dir() {
            FsKind::Dir
        } else {
            FsKind::File
        })
    }
}

/// Synthetic, in-memory stat-gate for native tests — the ONLY "filesystem" any
/// native hover test touches. Mirrors the engine's internal `MapProbe` but is
/// reachable from the `native` test modules so they can inject a fixed fs map
/// instead of reaching the real filesystem.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MapProbe(std::collections::HashMap<String, FsKind>);

#[cfg(test)]
impl MapProbe {
    /// Build a synthetic fs from `(absolute_path, kind)` entries.
    pub(crate) fn new<I>(entries: I) -> Self
    where
        I: IntoIterator<Item = (&'static str, FsKind)>,
    {
        MapProbe(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        )
    }
}

#[cfg(test)]
impl ResolveProbe for MapProbe {
    fn classify(&self, abs_path: &str) -> Option<FsKind> {
        self.0.get(abs_path).copied()
    }
}

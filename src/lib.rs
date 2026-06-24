// SPDX-License-Identifier: GPL-3.0-only
pub mod app;
pub mod atlas;
pub mod boxdraw;
pub mod color;
pub mod connection_hosts;
pub mod core;
pub mod cvd;
pub mod desktop;
pub mod emoji;
pub mod fuzzy;
pub mod graphics;
pub mod grid;
pub mod hints;
pub mod input;
pub mod native;
pub mod palette;
pub mod palette_catalog;
pub mod palette_gen;
pub mod palette_sources;
pub mod parser;
pub mod paths;
pub mod pty;
pub mod render;
pub mod selection;
pub mod session_host;
pub mod settings;
pub mod ssh_config;
pub mod ssh_connect;
pub mod text;
pub mod theme;
pub mod theme_author;

/// Process-global render knobs (`text::MIN_CONTRAST`, `atlas::STEM_DARKEN`,
/// `boxdraw::BOX_THICKNESS`) are read by the render path and mutated by a
/// handful of tests. `cargo test` runs tests in parallel threads sharing one
/// process, so a test that flips one of these globals can be observed by an
/// unrelated test that asserts the default — a schedule-dependent flake.
///
/// This module is the single serialization point: every test that *sets* one of
/// these globals, or that *reads* one expecting a specific baseline, takes this
/// lock for the duration of its body and restores the global before releasing.
/// Holding one shared lock (rather than per-global locks) keeps the ordering
/// trivially deadlock-free. Poison is recovered with `into_inner` so a panicking
/// test does not wedge the rest of the suite.
#[cfg(test)]
pub(crate) mod test_lock {
    use std::sync::{Mutex, MutexGuard};

    static RENDER_GLOBALS_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the shared render-globals lock for the current test body. Hold the
    /// returned guard until the test finishes mutating/reading the globals.
    pub(crate) fn render_globals_lock() -> MutexGuard<'static, ()> {
        RENDER_GLOBALS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

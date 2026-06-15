// SPDX-License-Identifier: GPL-3.0-only
//! Copy-mode handler: enter keyboard scrollback selection ("copy") mode.
//!
//! This is the thin entry point the `CopyMode` bindable action dispatches into.
//! It lives in its own `app` submodule so the copy-mode feature can be filled
//! in without colliding with the other per-feature handlers that share the
//! binding-dispatch surface.
//!
//! It returns whether it consumed the key. Until copy mode is implemented it
//! returns `false`, so the bound chord falls through to the PTY encode path
//! exactly as an unbound key would — the plain path stays byte-identical.

use super::*;

impl App {
    /// Enter keyboard scrollback selection mode. Returns `true` when the key was
    /// consumed; `false` lets the chord fall through to the PTY.
    pub(super) fn enter_copy_mode(&mut self) -> bool {
        false
    }
}

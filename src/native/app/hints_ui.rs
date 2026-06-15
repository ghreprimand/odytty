// SPDX-License-Identifier: GPL-3.0-only
//! Hints handler: activate keyboard pattern-select hints (URLs / paths / SHAs).
//!
//! This is the thin entry point the `Hints` bindable action dispatches into. It
//! lives in its own `app` submodule so the hints feature can be filled in
//! without colliding with the other per-feature handlers that share the
//! binding-dispatch surface.
//!
//! It returns whether it consumed the key. Until hints are implemented it
//! returns `false`, so the bound chord falls through to the PTY encode path
//! exactly as an unbound key would — the plain path stays byte-identical.

use super::*;

impl App {
    /// Activate keyboard pattern-select hints. Returns `true` when the key was
    /// consumed; `false` lets the chord fall through to the PTY.
    pub(super) fn activate_hints(&mut self) -> bool {
        false
    }
}

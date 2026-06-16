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

use super::overlay_registry::OverlayCtx;
use super::*;

impl App {
    /// Activate keyboard pattern-select hints. Returns `true` when the key was
    /// consumed; `false` lets the chord fall through to the PTY.
    pub(super) fn activate_hints(&mut self) -> bool {
        false
    }

    // --- overlay-registry / modal-gate contributor slots (Wave-15) ---
    // All no-ops today: the hints feature packet fills these bodies in this
    // file only, touching neither `app/mod.rs` nor `render_helpers.rs`.

    /// Paint hint labels onto the snapshot cells. No-op until hints ship.
    pub(in crate::native) fn paint_hints_cells(&self, _snapshot: &mut Snapshot, _ctx: &OverlayCtx) {
    }

    /// Hints render-cache fragment — inert until the label overlay is active.
    pub(super) fn hints_overlay_signature(&self) -> OverlayFragment {
        OverlayFragment::Inert
    }

    /// Whether the hints-select modal is active (captures keys). `false` today.
    pub(super) fn hints_selecting(&self) -> bool {
        false
    }

    /// Handle a key while the hints-select modal is active. No-op today.
    pub(super) fn hints_key(&mut self, _key: &WinitKey) {}
}

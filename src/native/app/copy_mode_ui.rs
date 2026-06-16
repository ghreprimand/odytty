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

use super::overlay_registry::OverlayCtx;
use super::*;

impl App {
    /// Enter keyboard scrollback selection mode. Returns `true` when the key was
    /// consumed; `false` lets the chord fall through to the PTY.
    pub(super) fn enter_copy_mode(&mut self) -> bool {
        false
    }

    // --- overlay-registry / modal-gate contributor slots (Wave-15) ---
    // All no-ops today: the copy-mode feature packet fills these bodies in this
    // file only, touching neither `app/mod.rs` nor `render_helpers.rs`.

    /// Paint the copy-mode caret/selection onto the snapshot cells. No-op until
    /// copy mode ships.
    pub(in crate::native) fn paint_copy_mode_cells(
        &self,
        _snapshot: &mut Snapshot,
        _ctx: &OverlayCtx,
    ) {
    }

    /// Copy-mode render-cache fragment — inert until copy mode is active.
    pub(super) fn copy_mode_overlay_signature(&self) -> OverlayFragment {
        OverlayFragment::Inert
    }

    /// Whether copy-mode is active (captures keys AND the mouse). `false` today.
    pub(super) fn copy_mode_active(&self) -> bool {
        false
    }

    /// Handle a key while copy-mode is active. No-op today.
    pub(super) fn copy_mode_key(&mut self, _key: &WinitKey) {}
}

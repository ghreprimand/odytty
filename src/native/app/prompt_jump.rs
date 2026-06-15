// SPDX-License-Identifier: GPL-3.0-only
//! Prompt-navigation handlers (OSC 133): jump the viewport to the previous or
//! next shell-prompt boundary.
//!
//! These are the thin entry points the `JumpPromptPrev` / `JumpPromptNext`
//! bindable actions dispatch into. They live in their own `app` submodule so
//! the prompt-navigation feature can be filled in without colliding with the
//! other per-feature handlers that share the binding-dispatch surface.
//!
//! Each returns whether it consumed the key. Until the navigation behavior is
//! implemented they return `false`, so the bound chord falls through to the PTY
//! encode path exactly as an unbound key would — the plain path stays
//! byte-identical.

use super::*;

impl App {
    /// Jump the viewport to the previous shell prompt. Returns `true` when the
    /// key was consumed; `false` lets the chord fall through to the PTY.
    pub(super) fn jump_prompt_prev(&mut self) -> bool {
        false
    }

    /// Jump the viewport to the next shell prompt. Returns `true` when the key
    /// was consumed; `false` lets the chord fall through to the PTY.
    pub(super) fn jump_prompt_next(&mut self) -> bool {
        false
    }
}

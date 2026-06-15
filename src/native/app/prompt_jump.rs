// SPDX-License-Identifier: GPL-3.0-only
//! Prompt-navigation handlers (OSC 133): jump the viewport to the previous or
//! next shell-prompt boundary.
//!
//! These are the thin entry points the `JumpPromptPrev` / `JumpPromptNext`
//! bindable actions dispatch into. They live in their own `app` submodule so
//! the prompt-navigation feature can be filled in without colliding with the
//! other per-feature handlers that share the binding-dispatch surface.
//!
//! Each returns whether it consumed the key. When no prompt exists in the
//! requested direction (the first prompt for a backward jump, the last for a
//! forward jump) the handler returns `false`, so the bound chord falls through
//! to the PTY encode path exactly as an unbound key would — at the ends of the
//! transcript the plain path stays byte-identical.

use super::*;

use crate::core::{Align, JumpDirection, prompt_jump as core_prompt_jump};

impl App {
    /// Jump the viewport to the previous shell prompt. Returns `true` when the
    /// key was consumed; `false` (no earlier prompt) lets the chord fall
    /// through to the PTY. `pub(in crate::native)` so the native test suite can
    /// drive the jump directly without synthesising key events.
    pub(in crate::native) fn jump_prompt_prev(&mut self) -> bool {
        self.jump_to_prompt(JumpDirection::Prev)
    }

    /// Jump the viewport to the next shell prompt. Returns `true` when the key
    /// was consumed; `false` (no later prompt) lets the chord fall through to
    /// the PTY. `pub(in crate::native)` for the same test-seam reason as
    /// [`App::jump_prompt_prev`].
    pub(in crate::native) fn jump_prompt_next(&mut self) -> bool {
        self.jump_to_prompt(JumpDirection::Next)
    }

    /// Resolve and perform a prompt jump in `direction`, scrolling the viewport
    /// so the target prompt lands at the top of the screen ([`Align::Top`], the
    /// SH2 default — output reads downward below the prompt).
    ///
    /// The reference is the absolute row at the *top* of the current viewport
    /// (`scrollback_len - offset`), so repeated jumps step monotonically through
    /// the prompt list: each jump lands a prompt at the top, which becomes the
    /// reference for the next jump. Pure-core [`core_prompt_jump`] owns the
    /// target/offset math; this is thin wiring (gather marks, read the geometry,
    /// scroll). Returns whether a jump occurred — `false` clamps at the ends
    /// without wrapping and falls through to the PTY.
    fn jump_to_prompt(&mut self, direction: JumpDirection) -> bool {
        let scrollback_len = self.scrollback_len();
        let viewport_height = self.grid.rows;
        let reference_row = scrollback_len.saturating_sub(self.viewport.offset());
        let marks = match self.terminal.lock() {
            Ok(terminal) => terminal.screen().prompt_marks(),
            Err(_) => return false,
        };
        let Some((_target_row, offset)) = core_prompt_jump(
            &marks,
            reference_row,
            direction,
            Align::Top,
            viewport_height,
            scrollback_len,
        ) else {
            return false;
        };
        if self.viewport.jump_to(offset, scrollback_len) {
            self.on_viewport_changed();
        }
        // The jump resolved to a real target even when the viewport was already
        // there (a no-op scroll); consume the key either way so it never leaks
        // an unintended escape sequence to the PTY mid-transcript.
        true
    }
}

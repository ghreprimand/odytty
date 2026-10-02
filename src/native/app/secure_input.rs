// SPDX-License-Identifier: GPL-3.0-only
//! Per-window hold for secure keyboard entry. The process counter, the
//! process-wide wish, and the macOS primitive live in
//! `crate::native::secure_input`. This module decides when this window
//! acquires or releases one hold: the wish is on, and this window has
//! keyboard focus.

use crate::core::{Attrs, Cell, Snapshot};

use super::App;
use crate::native::render_helpers::OverlayFragment;
use crate::native::secure_input::{
    SECURE_INPUT_LABEL, secure_input_applies, secure_keyboard_wish, set_secure_input,
    set_secure_keyboard_wish,
};

impl App {
    /// Match this window's hold to the wish and to keyboard focus.
    ///
    /// Where the primitive does not apply, the window never holds and the OS
    /// flag stays clear. A shared config may still store the key.
    pub(in crate::native) fn sync_secure_keyboard_input(&mut self) {
        if !secure_input_applies() {
            return;
        }
        let want = secure_keyboard_wish() && self.focused;
        if want && !self.secure_input_held {
            set_secure_input(true);
            self.secure_input_held = true;
            self.needs_rebuild = true;
        } else if !want && self.secure_input_held {
            set_secure_input(false);
            self.secure_input_held = false;
            self.needs_rebuild = true;
        }
    }

    /// Palette toggle. No-op where the primitive does not apply, and never
    /// written to the PTY. The new wish is process-wide; sibling windows
    /// pick it up from [`Self::take_secure_wish_broadcast`].
    pub(in crate::native) fn toggle_secure_keyboard_input(&mut self) {
        if !secure_input_applies() {
            return;
        }
        self.publish_secure_keyboard_wish(!secure_keyboard_wish());
    }

    /// Record a process-wide wish from this window (palette or config reload)
    /// and apply it here. Other windows apply the same value when the host
    /// drains [`Self::take_secure_wish_broadcast`].
    pub(in crate::native) fn publish_secure_keyboard_wish(&mut self, wish: bool) {
        self.settings.secure_keyboard_input = wish;
        set_secure_keyboard_wish(wish);
        self.secure_wish_broadcast = true;
        self.sync_secure_keyboard_input();
    }

    /// Take a wish this window published for its siblings, if any.
    pub(in crate::native) fn take_secure_wish_broadcast(&mut self) -> Option<bool> {
        if !self.secure_wish_broadcast {
            return None;
        }
        self.secure_wish_broadcast = false;
        Some(self.settings.secure_keyboard_input)
    }

    /// Apply a wish published by another window of this process.
    pub(in crate::native) fn apply_process_secure_wish(&mut self, wish: bool) {
        self.settings.secure_keyboard_input = wish;
        set_secure_keyboard_wish(wish);
        self.sync_secure_keyboard_input();
    }

    /// Test seam: mark the hold without touching the OS primitive, so the
    /// render signature can be checked on every platform.
    #[cfg(test)]
    pub(in crate::native) fn set_secure_input_held_for_test(&mut self, held: bool) {
        self.secure_input_held = held;
    }

    /// Drop this window's hold, once. Used when the window closes and from
    /// [`Drop`], so the last window's close and process exit both release.
    pub(in crate::native) fn release_secure_input_hold(&mut self) {
        if self.secure_input_held {
            set_secure_input(false);
            self.secure_input_held = false;
        }
    }

    /// `Inert` unless this window holds secure input, so a toggle re-keys the
    /// frame and the default path stays unchanged.
    pub(in crate::native) fn secure_input_overlay_signature(&self) -> OverlayFragment {
        if self.secure_input_held {
            OverlayFragment::SecureInput
        } else {
            OverlayFragment::Inert
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.release_secure_input_hold();
    }
}

/// Paint `SECURE INPUT` at the top-left of the focused window. Inverse video
/// in the terminal's own colors. A pane too narrow for the full label is left
/// unchanged rather than showing a fragment. Chrome over the snapshot, not a
/// grid or PTY write.
pub(in crate::native) fn paint_secure_input_label(snapshot: &mut Snapshot, held: bool) {
    if !held {
        return;
    }
    let columns = snapshot.dimensions.columns;
    let label: Vec<char> = SECURE_INPUT_LABEL.chars().collect();
    if columns < label.len() || snapshot.dimensions.rows == 0 {
        return;
    }
    let mut attrs = Attrs::default();
    attrs.set_inverse(true);
    attrs.set_bold(true);
    let len = label.len();
    for (offset, ch) in label.iter().enumerate() {
        if let Some(cell) = snapshot.cells.get_mut(offset) {
            *cell = Cell::new(*ch, attrs);
        }
    }
    // The cell just past the label can be the spacer half of a wide glyph
    // whose lead was inside the label. Overwriting the lead orphans that
    // spacer; blank it.
    if let Some(tail) = snapshot.cells.get_mut(len)
        && tail.wide_continuation
    {
        let kept = tail.attrs;
        *tail = Cell::new(' ', kept);
    }
}

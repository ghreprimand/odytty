// SPDX-License-Identifier: GPL-3.0-only
//! Revision, dirty-state, and upload-layout queries.

use super::*;

impl GlyphAtlas {
    /// Monotonic revision, bumped on every pixel/dimension change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Return whether the atlas changed since the last call, clearing the flag.
    /// The native layer calls this each frame to decide when to re-upload.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Total slots currently allocated (fallback + ASCII + dynamic). Primarily
    /// for tests asserting growth and fallback-without-allocation behavior.
    pub fn slot_count(&self) -> u32 {
        self.next_slot
    }

    /// Coverage storage mode.
    pub fn subpixel_mode(&self) -> SubpixelMode {
        self.subpixel
    }

    /// Bytes in one atlas row, for GPU upload.
    pub fn bytes_per_row(&self) -> u32 {
        self.width * self.subpixel.bytes_per_pixel()
    }
}

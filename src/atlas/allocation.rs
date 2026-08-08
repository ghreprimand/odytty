// SPDX-License-Identifier: GPL-3.0-only
//! Dynamic slot allocation and page-based backing-store growth.

use super::*;

impl GlyphAtlas {
    /// Reserve `span` consecutive dynamic slots in a single atlas row and return
    /// the lead slot index, appending pages of rows (zero-filled) as capacity is
    /// exhausted. Returns `None` once [`MAX_ATLAS_SLOTS`] would be exceeded.
    ///
    /// A multi-cell (`span > 1`) allocation never straddles a row boundary:
    /// if the span would cross the row edge, filler slots are burned up to the
    /// next row boundary so the run starts at column 0 of the next row,
    /// keeping the inked region horizontally contiguous. One filler suffices
    /// for a wide (East Asian) pair; a longer contextual span burns as many as
    /// its overhang requires. Every consumed slot (filler + lead + reserved)
    /// gets a dense placeholder `slot_ink`/`slot_span` entry — the caller
    /// overwrites the lead's ink — so existing slots never move and UV rects
    /// handed out before a growth stay valid.
    pub(super) fn allocate_slots(&mut self, span: u32) -> Option<u32> {
        debug_assert!(span >= 1);
        // Defense-in-depth: a span wider than a full atlas row can never be
        // made contiguous — it would wrap across rows and corrupt neighboring
        // slots' pixels. Callers screen this earlier; refuse it here too.
        if span > self.cols {
            return None;
        }
        // A multi-cell span must not wrap across a row: burn fillers to the
        // next row boundary. A single filler is only sufficient for span == 2;
        // a longer span whose lead lands within span-1 columns of the row edge
        // needs every remaining column of the row burned, or the reserved
        // cells wrap onto the next row and the rasterized ink strip overwrites
        // other glyphs' pixels while the UV rect runs past the atlas edge.
        if span > 1 {
            while self.next_slot % self.cols + span > self.cols {
                self.push_placeholder_slot(1)?;
            }
        }
        if self.next_slot + span > self.max_slots {
            return None;
        }
        let lead = self.next_slot;
        self.grow_to_fit(lead + span - 1);
        for i in 0..span {
            // Lead carries the real span; reserved cells are never looked up.
            let s = if i == 0 { span as u8 } else { 1 };
            self.slot_ink.push(GlyphInk::cell(self.cell));
            self.slot_span.push(s);
        }
        self.next_slot += span;
        Some(lead)
    }

    /// Append a single dense placeholder slot with the given span (used for the
    /// filler slot that a wide allocation burns to avoid a row wrap). Returns
    /// `None` at the hard cap.
    fn push_placeholder_slot(&mut self, span: u8) -> Option<()> {
        if self.next_slot + 1 > self.max_slots {
            return None;
        }
        self.grow_to_fit(self.next_slot);
        self.slot_ink.push(GlyphInk::cell(self.cell));
        self.slot_span.push(span);
        self.next_slot += 1;
        Some(())
    }

    /// Grow `capacity_rows` (in [`ATLAS_GROW_ROWS`] pages) and the backing bitmap
    /// so `slot` is addressable, zero-filling new pixels. Existing slots never
    /// move. No-op when the slot already fits.
    fn grow_to_fit(&mut self, slot: u32) {
        let needed_rows = slot / self.cols + 1;
        if needed_rows > self.capacity_rows {
            while needed_rows > self.capacity_rows {
                self.capacity_rows += ATLAS_GROW_ROWS;
            }
            self.height = self.capacity_rows * slot_h(self.cell);
            self.data.resize(
                atlas_byte_len(self.width, self.height, self.subpixel.bytes_per_pixel()),
                0,
            );
            self.revision += 1;
            self.dirty = true;
        }
    }
}

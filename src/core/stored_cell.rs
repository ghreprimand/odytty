// SPDX-License-Identifier: GPL-3.0-only
//! Narrow scrollback storage: the ring's stored cell and its per-line
//! combining-mark sidecar.
//!
//! # Why the ring stores a different cell
//!
//! [`super::types::Cell`] is 44 bytes, and 16 of them are the four-slot
//! `combining` array (plus its length byte) that is empty for effectively every
//! cell in real content. Scrollback is overwhelmingly cells — 97.7% of the ring
//! at 100,000 hard-terminated lines — so the array is paid for on every stored
//! cell to serve a small minority of them.
//!
//! [`StoredCell`] is that cell with the array lifted out: base char, the whole
//! `Attrs`, and the two per-cell booleans packed into one byte, at 28 bytes.
//! Marks move to [`MarkTable`], a sidecar carried by the owning logical line and
//! keyed by flat-cell index, allocated only for a line that actually has marks.
//!
//! # What this deliberately does not do
//!
//! `Cell` itself is unchanged and stays `Copy`. Nothing outside this module's
//! callers sees `StoredCell`: the ring converts on the way in and on the way
//! out, so [`super::types::Cell::combining`] and
//! [`super::types::Cell::grapheme`] keep their exact behavior for every reader,
//! including the ones that hold a `Cell` with no terminal in scope (the
//! renderer iterates `Snapshot::cells` after the core has been left behind — a
//! cell there must still describe itself).
//!
//! No `Attrs` field is narrowed. Colour, every SGR bit, and the hyperlink id are
//! carried whole, because the saving here must not be bought with cell fidelity.
//!
//! # Cost that goes up
//!
//! A cell that *does* carry marks now costs 28 bytes plus a
//! [`MarkRun`] entry, which is more than the 44 it used to cost alone. That is
//! the intended trade: mark-bearing cells are rare enough that the per-cell
//! saving dominates, and content that is mostly combining marks is the case
//! this representation is worst for.

use super::types::{Attrs, Cell, MAX_COMBINING};

/// DECSCA character protection.
const F_PROTECTED: u8 = 1 << 0;
/// Trailing spacer of a wide (two-column) glyph.
const F_WIDE_CONTINUATION: u8 = 1 << 1;

/// One scrollback cell as the ring stores it: everything [`Cell`] carries
/// except the combining-mark array, which lives in the owning line's
/// [`MarkTable`].
///
/// Fields are private so the flag packing cannot be depended on from outside;
/// the only ways in and out are [`StoredCell::from_cell`] and
/// [`StoredCell::hydrate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core) struct StoredCell {
    ch: char,
    attrs: Attrs,
    flags: u8,
}

/// The whole point of this type is its size, so the compiler asserts it rather
/// than a comment claiming it. `Cell` is pinned alongside because the saving is
/// the *difference*: if `Cell` shrank and this did not, the trade would change
/// and should be re-decided rather than silently kept.
const _: () = assert!(std::mem::size_of::<StoredCell>() == 28);
const _: () = assert!(std::mem::size_of::<Cell>() == 44);

impl StoredCell {
    /// Narrow a `Cell` for storage. The caller is responsible for recording
    /// `cell.combining()` in the line's [`MarkTable`] — this type cannot, since
    /// it does not know its own index.
    pub(in crate::core) fn from_cell(cell: &Cell) -> Self {
        let mut flags = 0u8;
        if cell.protected {
            flags |= F_PROTECTED;
        }
        if cell.wide_continuation {
            flags |= F_WIDE_CONTINUATION;
        }
        Self {
            ch: cell.ch,
            attrs: cell.attrs,
            flags,
        }
    }

    /// Rebuild the full `Cell`, re-attaching `marks`.
    ///
    /// Marks are re-attached through [`Cell::push_combining`] rather than by
    /// writing the array directly, so the `MAX_COMBINING` bound and its
    /// drop-on-overflow semantics are the same code that enforced them on the
    /// way in. A second copy of that rule here is exactly how the two would
    /// drift apart.
    #[inline]
    pub(in crate::core) fn hydrate(self, marks: &[char]) -> Cell {
        let mut cell = Cell::from_parts(
            self.ch,
            self.attrs,
            self.flags & F_PROTECTED != 0,
            self.flags & F_WIDE_CONTINUATION != 0,
        );
        for &mark in marks {
            cell.push_combining(mark);
        }
        cell
    }

    #[inline]
    pub(in crate::core) fn ch(self) -> char {
        self.ch
    }

    #[inline]
    pub(in crate::core) fn attrs(self) -> Attrs {
        self.attrs
    }

    #[inline]
    pub(in crate::core) fn wide_continuation(self) -> bool {
        self.flags & F_WIDE_CONTINUATION != 0
    }
}

/// The combining marks of one stored cell, keyed by its flat-cell index within
/// the owning logical line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MarkRun {
    /// Flat-cell index within the logical line.
    ///
    /// `usize`, deliberately, and not a narrower integer. A soft-wrapped
    /// logical line is not bounded by the terminal width — it is bounded by
    /// `MAX_LOGICAL_LINE_CELLS`, which is 2^20 — so a `u16` key would truncate
    /// on ordinary output and silently attach marks to the wrong base
    /// character. `usize` is the type flat indices already have everywhere else
    /// in the store (`ButtonSpan::start_col` included), so there is no
    /// conversion on this path that *could* truncate. The extra bytes cost
    /// nothing measurable: this table is per-marked-cell, not per-cell.
    index: usize,
    marks: [char; MAX_COMBINING],
    len: u8,
}

/// A logical line's combining marks, keyed by flat-cell index.
///
/// Shaped exactly like `button_spans`: a field of the logical line, empty and
/// unallocated for the mark-free line (which is nearly all of them), moving,
/// cloning, and dropping with its line. That is what makes "per-screen,
/// bounded, evicted with its owning cells" true by construction rather than by
/// argument — there is no table anywhere else to leak into or forget to evict.
///
/// Entries are held sorted by `index`, strictly increasing. Every mutator
/// preserves that, and [`MarkTable::debug_check`] asserts it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::core) struct MarkTable {
    entries: Vec<MarkRun>,
}

impl MarkTable {
    #[inline]
    pub(in crate::core) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(in crate::core) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(in crate::core) fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// Record `marks` at flat index `index`. Indices must arrive strictly
    /// increasing — every caller walks a row or a line front to back — which is
    /// what keeps the table sorted without ever sorting it.
    pub(in crate::core) fn push(&mut self, index: usize, marks: &[char]) {
        debug_assert!(!marks.is_empty(), "an empty run must not be stored");
        debug_assert!(
            self.entries.last().is_none_or(|last| last.index < index),
            "mark runs must arrive strictly increasing"
        );
        let mut run = MarkRun {
            index,
            marks: ['\0'; MAX_COMBINING],
            len: 0,
        };
        for &mark in marks.iter().take(MAX_COMBINING) {
            run.marks[run.len as usize] = mark;
            run.len += 1;
        }
        self.entries.push(run);
    }

    /// Marks at flat index `index`, empty when the cell carries none.
    ///
    /// The empty-table early-out is what makes the mark-free line — the
    /// overwhelmingly common case — pay a single branch rather than a search.
    #[inline]
    pub(in crate::core) fn marks_at(&self, index: usize) -> &[char] {
        if self.entries.is_empty() {
            return &[];
        }
        match self.entries.binary_search_by_key(&index, |run| run.index) {
            Ok(at) => {
                let run = &self.entries[at];
                &run.marks[..run.len as usize]
            }
            Err(_) => &[],
        }
    }

    /// Whether any marked cell sits at or after `from`. Used by the
    /// trailing-blank trim, which must not treat a cell carrying marks as
    /// blank however plain its base character looks.
    pub(in crate::core) fn any_at_or_after(&self, from: usize) -> bool {
        self.entries.last().is_some_and(|last| last.index >= from)
    }

    /// Drop the first `drop` cells' marks and shift the rest down, matching a
    /// front-drain of the line's cells.
    pub(in crate::core) fn drop_front(&mut self, drop: usize) {
        if self.entries.is_empty() {
            return;
        }
        self.entries.retain(|run| run.index >= drop);
        for run in &mut self.entries {
            run.index -= drop;
        }
    }

    /// Reclaim reserved-but-unused capacity on a line whose length is final.
    ///
    /// The tolerance is the table's own and is deliberately much tighter than
    /// the one the cell vector uses. A run is 32 bytes against a stored cell's
    /// 28, but the table is short — one entry per *marked* cell, not per cell —
    /// so doubling overshoot is a large fraction of a small allocation rather
    /// than a small fraction of a large one. Left at the cell vector's
    /// 64-entry tolerance this was measured leaving 2 KB unused on a
    /// dense-marked line, which turned a 32-byte-per-marked-cell sidecar into
    /// 51 bytes and ate most of the margin the representation is supposed to
    /// have. Shrinking is a one-off at the hard-terminate transition on an
    /// allocation that most lines never make at all.
    const FINALIZE_SLACK_TOLERANCE: usize = 4;

    pub(in crate::core) fn finalize_capacity(&mut self) {
        if self.entries.capacity() - self.entries.len() > Self::FINALIZE_SLACK_TOLERANCE {
            self.entries.shrink_to_fit();
        }
    }

    /// Assert the table's own invariants against a line of `cells` cells:
    /// sorted strictly increasing, every index in range, every run non-empty
    /// and within `MAX_COMBINING`.
    ///
    /// Debug-only, and called at the seams where the table is re-keyed. A
    /// violation of any of these is a corruption that would otherwise surface
    /// far from its cause, as marks rendered on the wrong character.
    pub(in crate::core) fn debug_check(&self, cells: usize) {
        if cfg!(debug_assertions) {
            let mut previous: Option<usize> = None;
            for run in &self.entries {
                debug_assert!(
                    previous.is_none_or(|p| p < run.index),
                    "mark runs out of order at {}",
                    run.index
                );
                debug_assert!(
                    run.index < cells,
                    "mark run at {} past {cells} cells",
                    run.index
                );
                debug_assert!(
                    run.len > 0 && run.len as usize <= MAX_COMBINING,
                    "mark run at {} has length {}",
                    run.index,
                    run.len
                );
                previous = Some(run.index);
            }
        }
    }
}

/// Heap bytes a mark table of `capacity` runs occupies. Zero for the
/// overwhelmingly common mark-free line, which never allocates.
pub(in crate::core) fn marks_bytes(capacity: usize) -> u64 {
    (capacity as u64).saturating_mul(std::mem::size_of::<MarkRun>() as u64)
}

/// Heap bytes a stored-cell allocation of `capacity` cells occupies.
///
/// Deliberately separate from [`super::scrollback::cells_bytes`], which counts
/// live-grid rows: the grid holds `Cell` and the ring holds [`StoredCell`], so
/// one shared function would now silently misattribute one of them.
pub(in crate::core) fn stored_cells_bytes(capacity: usize) -> u64 {
    (capacity as u64).saturating_mul(std::mem::size_of::<StoredCell>() as u64)
}

/// Append one physical row's cells to a logical line's storage, recording any
/// combining marks in `marks` re-keyed by the flat index the append starts at.
///
/// This is the **single re-keying site** in the store. Every path that builds
/// or extends a logical line from physical rows routes through it, so the
/// offset arithmetic exists once instead of once per caller. That matters more
/// than it looks: an off-by-N here does not lose marks, it moves them onto the
/// wrong base character — which reads as wrong glyphs rather than missing ones,
/// and no "did we keep every mark" assertion would catch it. Pushing the cell
/// and its marks in the same loop over the same index is what makes the two
/// impossible to shift independently.
pub(in crate::core) fn adopt_row_cells(
    cells: &mut Vec<StoredCell>,
    marks: &mut MarkTable,
    row: &[Cell],
) {
    let offset = cells.len();
    cells.reserve(row.len());
    for (i, cell) in row.iter().enumerate() {
        let attached = cell.combining();
        if !attached.is_empty() {
            marks.push(offset + i, attached);
        }
        cells.push(StoredCell::from_cell(cell));
    }
    marks.debug_check(cells.len());
}

/// Rebuild a full `Cell` run from stored cells and their sidecar.
pub(in crate::core) fn hydrate_all(cells: &[StoredCell], marks: &MarkTable) -> Vec<Cell> {
    cells
        .iter()
        .enumerate()
        .map(|(i, stored)| stored.hydrate(marks.marks_at(i)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::Attrs;

    fn marked(ch: char, marks: &[char]) -> Cell {
        let mut cell = Cell::new(ch, Attrs::default());
        for &mark in marks {
            cell.push_combining(mark);
        }
        cell
    }

    #[test]
    fn narrow_and_hydrate_round_trips_every_field() {
        let mut cell = marked('e', &['\u{301}', '\u{308}']);
        cell.protected = true;
        cell.wide_continuation = true;
        let stored = StoredCell::from_cell(&cell);
        assert_eq!(stored.hydrate(cell.combining()), cell);
    }

    #[test]
    fn hydrate_applies_the_same_overflow_bound_as_the_cell() {
        // Five marks offered, four storable: the truncation must be the
        // `Cell`'s own, not a second rule in this module.
        let offered = ['\u{301}', '\u{302}', '\u{303}', '\u{304}', '\u{305}'];
        let cell = marked('a', &offered);
        assert_eq!(cell.combining().len(), MAX_COMBINING);
        let stored = StoredCell::from_cell(&cell);
        assert_eq!(stored.hydrate(cell.combining()), cell);
    }

    #[test]
    fn drop_front_discards_and_shifts() {
        let mut table = MarkTable::default();
        table.push(0, &['\u{301}']);
        table.push(5, &['\u{308}']);
        table.push(9, &['\u{327}']);
        table.drop_front(5);
        assert_eq!(table.marks_at(0), &['\u{308}']);
        assert_eq!(table.marks_at(4), &['\u{327}']);
        assert_eq!(table.len(), 2);
        table.debug_check(5);
    }

    #[test]
    fn any_at_or_after_gates_the_blank_trim() {
        let mut table = MarkTable::default();
        assert!(!table.any_at_or_after(0));
        table.push(3, &['\u{301}']);
        assert!(table.any_at_or_after(0));
        assert!(table.any_at_or_after(3));
        assert!(!table.any_at_or_after(4));
    }

    #[test]
    fn adopt_row_cells_keys_marks_to_their_own_cells() {
        let row = vec![
            Cell::new('a', Attrs::default()),
            marked('b', &['\u{301}']),
            Cell::new('c', Attrs::default()),
        ];
        let mut cells = Vec::new();
        let mut marks = MarkTable::default();
        adopt_row_cells(&mut cells, &mut marks, &row);
        adopt_row_cells(&mut cells, &mut marks, &row);
        assert_eq!(cells.len(), 6);
        assert_eq!(hydrate_all(&cells, &marks), [row.clone(), row].concat());
    }
}

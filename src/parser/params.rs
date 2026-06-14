// SPDX-License-Identifier: GPL-3.0-only
//! Owned CSI/DCS parameter container — OdyTTY-original storage.
//!
//! `Params` models a VT control sequence's parameter list the way the
//! terminal core consumes it: an ordered list of **parameters**, each of which
//! is a non-empty list of **subparameters** (the colon-separated groups in
//! sequences like `38:2::r:g:b`). The shape (`Params::iter()` yielding `&[u16]`
//! per parameter) is preserved so the live `Screen` dispatch and the differential
//! oracle see exactly the same surface they did under the first-generation
//! state core.
//!
//! ## Storage decision (PA2-r, primary-spec)
//!
//! `MAX_PARAMS = 32` slots total (parameters + subparameters combined), which
//! is the canonical DEC ANSI parameter cap. The reimplementation uses:
//!
//! - `values: [u16; 32]` — every parameter and subparameter value, in order.
//!   Inline; zero heap.
//! - `len: u8` — count of valid slots (`<= MAX_PARAMS`).
//! - `starts: u32` — a **boundary bitmap**: bit `i` set ⇔ slot `i` is the
//!   first value of a new top-level parameter. Bit clear ⇔ slot `i` continues
//!   the parameter started at the most-recent set bit ≤ `i`. Because
//!   `MAX_PARAMS = 32`, exactly one `u32` encodes every possible boundary, with
//!   no parallel array.
//! - `closed: bool` — `true` when the most-recent insertion finalised the
//!   current parameter (a `push`), `false` when it left the parameter open for
//!   another subparam (an `extend`). Drives whether the next insertion opens a
//!   new parameter or continues the current one.
//!
//! Saturating-on-overflow arithmetic and the "32 slots → set `ignore`" rule
//! match the parser policy pinned by golden fixtures.
//!
//! ## Originality note
//!
//! This is an OdyTTY-designed encoding written from the primary specs (ECMA-48
//! parameter syntax and the DEC ANSI 32-slot cap). It is structurally distinct
//! from the first-generation `Params` (two `Vec`s — a flattened value list and
//! a parallel `group_len_at` array): the new design is allocation-free, the
//! boundary metadata is one machine word, and group reconstruction is a
//! bit-scan rather than an array walk. The public surface (`iter`, `len`,
//! `is_empty`, `iter`, `IntoIterator`) is deliberately small and owned by
//! OdyTTY's parser/screen seam.

/// Maximum number of flattened parameter slots (parameters + subparameters).
/// The canonical DEC ANSI parameter cap; once full, the parser sets the
/// dispatch `ignore` flag instead of growing without bound.
pub(crate) const MAX_PARAMS: usize = 32;

/// An ordered list of VT control parameters, each a list of subparameters.
///
/// Built incrementally by [`OdyParser`](crate::parser::OdyParser) as digits and
/// separators arrive, and read back by the core via [`Params::iter`], which
/// yields one `&[u16]` per parameter (the parameter's subparameters). A bare
/// `ESC [ A` yields a single parameter `[0]`; `ESC [ 38:2::1:2:3 m` yields one
/// parameter `[38, 2, 0, 1, 2, 3]`; `ESC [ 1;2;3 m` yields three parameters
/// `[1]`, `[2]`, `[3]`.
#[derive(Debug, Clone)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    /// Boundary bitmap: bit `i` set ⇔ slot `i` begins a top-level parameter.
    starts: u32,
    /// Number of valid slots in `values`.
    len: u8,
    /// Whether the most-recent insertion left the current parameter closed
    /// (next insertion opens a new one) or open (next continues this one).
    closed: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

impl Params {
    /// An empty parameter list.
    pub fn new() -> Self {
        Self {
            values: [0; MAX_PARAMS],
            starts: 0,
            len: 0,
            closed: true,
        }
    }

    /// Number of parameters **and** subparameters stored (flattened count).
    /// Matches the canonical parser's `len`, so [`Self::is_full`] trips at the
    /// same byte.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether no parameters are present (a bare control with no digits).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether storage is exhausted; the parser sets `ignore` instead of pushing
    /// further parameters once this is true.
    pub(crate) fn is_full(&self) -> bool {
        self.len as usize == MAX_PARAMS
    }

    /// Drop every stored parameter (called on each new escape sequence). Tail
    /// slots of `values` are left untouched; equality and iteration only ever
    /// look at the first `len` slots.
    pub(crate) fn clear(&mut self) {
        self.starts = 0;
        self.len = 0;
        self.closed = true;
    }

    /// Finish the current parameter with `item` as its final value and mark the
    /// parameter closed (used for the `;` separator and the terminating final
    /// byte). No-op when full — the caller has already set `ignore`.
    pub(crate) fn push(&mut self, item: u16) {
        if self.is_full() {
            return;
        }
        let slot = self.len as u32;
        // Slot starts a new parameter iff the prior parameter was closed (or
        // this is the very first slot — closed defaults to `true`).
        if self.closed {
            self.starts |= 1u32 << slot;
        }
        self.values[slot as usize] = item;
        self.len += 1;
        self.closed = true;
    }

    /// Append `item` as another subparameter of the current parameter (the `:`
    /// separator), keeping the parameter open. No-op when full.
    pub(crate) fn extend(&mut self, item: u16) {
        if self.is_full() {
            return;
        }
        let slot = self.len as u32;
        // If the current parameter is closed (or this is the first slot), this
        // extend opens a NEW parameter at this slot (a leading `:` before any
        // `;` still starts parameter 0). Otherwise the slot is a continuation.
        if self.closed {
            self.starts |= 1u32 << slot;
        }
        self.values[slot as usize] = item;
        self.len += 1;
        self.closed = false;
    }

    /// Iterate the parameters, yielding each parameter's subparameter slice.
    pub fn iter(&self) -> ParamsIter<'_> {
        ParamsIter {
            params: self,
            cur: 0,
        }
    }
}

/// Two parameter lists are equal iff they store the same `len` values in the
/// same order with the same group boundaries. Tail slots of the inline array
/// and bits of `starts` beyond `len` are deliberately ignored — they may carry
/// arbitrary state after `clear()`. `closed` is transient parser bookkeeping
/// and does not affect observable grouping, so it's excluded from equality.
impl PartialEq for Params {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        let n = self.len as usize;
        if self.values[..n] != other.values[..n] {
            return false;
        }
        let mask: u32 = if n == 0 {
            0
        } else if n >= 32 {
            !0
        } else {
            (1u32 << n) - 1
        };
        (self.starts & mask) == (other.starts & mask)
    }
}

impl Eq for Params {}

/// Iterator over [`Params`], yielding one `&[u16]` per parameter.
pub struct ParamsIter<'a> {
    params: &'a Params,
    /// Slot index where the next group begins.
    cur: usize,
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<Self::Item> {
        let n = self.params.len as usize;
        if self.cur >= n {
            return None;
        }
        let start = self.cur;
        // Find the next set bit strictly greater than `start`; that's the next
        // group's start, or `n` if no more groups exist.
        let mask_above = if start + 1 >= 32 {
            0u32
        } else {
            !((1u32 << (start + 1)) - 1)
        };
        let above = self.params.starts & mask_above;
        let end = if above == 0 {
            n
        } else {
            (above.trailing_zeros() as usize).min(n)
        };
        self.cur = end;
        Some(&self.params.values[start..end])
    }
}

impl<'a> IntoIterator for &'a Params {
    type Item = &'a [u16];
    type IntoIter = ParamsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

//! Owned CSI/DCS parameter container for the OdyTTY VT parser.
//!
//! This is an OdyTTY-owned replacement for `vte::Params`, modelling the same
//! shape the terminal core already consumes: an ordered list of **parameters**,
//! each of which is a non-empty list of **subparameters** (the colon-separated
//! groups in sequences like `38:2::r:g:b`). The accumulation limits and
//! saturating arithmetic match the canonical DEC ANSI parser exactly so the
//! differential oracle (vte vs OdyParser, both driving the same [`Screen`]) sees
//! byte-identical parameter lists.
//!
//! [`Screen`]: crate::core::Screen

/// Maximum number of flattened parameter slots (parameters + subparameters),
/// matching the canonical parser. Once full, further parameter bytes set the
/// dispatch `ignore` flag rather than growing without bound.
pub(crate) const MAX_PARAMS: usize = 32;

/// An ordered list of VT control parameters, each a list of subparameters.
///
/// Built incrementally by [`OdyParser`](crate::parser::OdyParser) as digits and
/// separators arrive, and read back by the core via [`Params::iter`], which
/// yields one `&[u16]` per parameter (the parameter's subparameters). A bare
/// `ESC [ A` yields a single parameter `[0]`; `ESC [ 38:2::1:2:3 m` yields one
/// parameter `[38, 2, 0, 1, 2, 3]`; `ESC [ 1;2;3 m` yields three parameters
/// `[1]`, `[2]`, `[3]`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Params {
    /// Flattened storage of every parameter and subparameter value, in order.
    /// Length never exceeds [`MAX_PARAMS`].
    values: Vec<u16>,
    /// Parallel to `values`: at each parameter's **start** index this holds the
    /// number of subparameters in that parameter; at interior subparameter
    /// positions it is `0`. Mirrors the canonical parser's bookkeeping so group
    /// boundaries reconstruct exactly.
    group_len_at: Vec<u8>,
    /// Number of subparameters accumulated into the current (open) parameter so
    /// far, used to locate the current group's start when extending it.
    current_subparams: u8,
}

impl Params {
    /// An empty parameter list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of parameters **and** subparameters stored (flattened count).
    /// Matches the canonical parser's `len`, so [`Self::is_full`] trips at the
    /// same byte.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether no parameters are present (a bare control with no digits).
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Whether storage is exhausted; the parser sets `ignore` instead of pushing
    /// further parameters once this is true.
    pub(crate) fn is_full(&self) -> bool {
        self.values.len() == MAX_PARAMS
    }

    /// Drop every stored parameter (called on each new escape sequence).
    pub(crate) fn clear(&mut self) {
        self.values.clear();
        self.group_len_at.clear();
        self.current_subparams = 0;
    }

    /// Finish the current parameter with `item` as its final value and open a
    /// fresh parameter. Used for the `;` separator and the terminating final
    /// byte. No-op when full (the caller has already set `ignore`).
    pub(crate) fn push(&mut self, item: u16) {
        if self.is_full() {
            return;
        }
        let start = self.values.len() - self.current_subparams as usize;
        self.values.push(item);
        self.group_len_at.push(0);
        self.group_len_at[start] = self.current_subparams + 1;
        self.current_subparams = 0;
    }

    /// Append `item` as another subparameter of the current parameter (the `:`
    /// separator), keeping the parameter open. No-op when full.
    pub(crate) fn extend(&mut self, item: u16) {
        if self.is_full() {
            return;
        }
        let start = self.values.len() - self.current_subparams as usize;
        self.values.push(item);
        self.group_len_at.push(0);
        self.group_len_at[start] = self.current_subparams + 1;
        self.current_subparams += 1;
    }

    /// Iterate the parameters, yielding each parameter's subparameter slice.
    pub fn iter(&self) -> ParamsIter<'_> {
        ParamsIter {
            params: self,
            index: 0,
        }
    }

    /// Rebuild an owned [`Params`] from a `vte::Params`, preserving parameter and
    /// subparameter grouping exactly. Used by the live (vte-driven) seam so the
    /// shared core dispatch logic operates on the owned type while vte remains
    /// the production parser this packet.
    pub fn from_vte(params: &vte::Params) -> Self {
        let mut out = Self::new();
        for group in params.iter() {
            match group.split_last() {
                Some((last, leading)) => {
                    for &sub in leading {
                        out.extend(sub);
                    }
                    out.push(*last);
                }
                // vte never yields an empty group, but stay total just in case.
                None => out.push(0),
            }
        }
        out
    }
}

/// Iterator over [`Params`], yielding one `&[u16]` per parameter.
pub struct ParamsIter<'a> {
    params: &'a Params,
    index: usize,
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.params.values.len() {
            return None;
        }
        let len = self.params.group_len_at[self.index] as usize;
        let group = &self.params.values[self.index..self.index + len];
        self.index += len;
        Some(group)
    }
}

impl<'a> IntoIterator for &'a Params {
    type Item = &'a [u16];
    type IntoIter = ParamsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

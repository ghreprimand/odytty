// SPDX-License-Identifier: GPL-3.0-only
//! Incremental Unicode 17.0.0 extended grapheme cluster boundaries (UAX #29,
//! rules GB3 to GB13 and GB999), over a generated property table.
//!
//! The state is bounded and per cluster: every rule that looks back (GB9c
//! conjuncts, GB11 emoji ZWJ sequences, GB12 and GB13 regional indicator
//! pairs) only needs context inside the cluster being extended, so a caller
//! starts a fresh state at each boundary. The segmenter decides boundaries
//! only; terminal cell ownership and width are separate rules.
mod data;

#[cfg(test)]
mod tests;

/// Grapheme_Cluster_Break values, in the generator's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Other,
    Cr,
    Lf,
    Control,
    Extend,
    Zwj,
    RegionalIndicator,
    Prepend,
    SpacingMark,
    L,
    V,
    T,
    Lv,
    Lvt,
}

impl Class {
    fn from_packed(packed: u8) -> Self {
        match packed & 0x0f {
            1 => Self::Cr,
            2 => Self::Lf,
            3 => Self::Control,
            4 => Self::Extend,
            5 => Self::Zwj,
            6 => Self::RegionalIndicator,
            7 => Self::Prepend,
            8 => Self::SpacingMark,
            9 => Self::L,
            10 => Self::V,
            11 => Self::T,
            12 => Self::Lv,
            13 => Self::Lvt,
            _ => Self::Other,
        }
    }
}

const PICTOGRAPHIC: u8 = 0x10;
const INCB_MASK: u8 = 0x60;
const INCB_LINKER: u8 = 0x20;
const INCB_CONSONANT: u8 = 0x40;
const INCB_EXTEND: u8 = 0x60;

fn packed(ch: char) -> u8 {
    let cp = ch as u32;
    if ch.is_ascii() {
        // Printable ASCII has no grapheme properties; the table agrees.
        return match cp {
            0x0d => 1,
            0x0a => 2,
            0x00..=0x1f | 0x7f => 3,
            _ => 0,
        };
    }
    let index = data::PROPERTIES.partition_point(|&(_, end, _)| end < cp);
    data::PROPERTIES
        .get(index)
        .filter(|&&(start, _, _)| start <= cp)
        .map_or(0, |&(_, _, value)| value)
}

/// GB11 progress: `Extended_Pictographic Extend*`, then a ZWJ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pictographic {
    None,
    Open,
    AfterZwj,
}

/// GB9c progress: an InCB consonant, then InCB extends or linkers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conjunct {
    None,
    Consonant,
    Linked,
}

/// Boundary state for the cluster ending at the last accepted scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GraphemeState {
    previous: Class,
    regional_odd: bool,
    pictographic: Pictographic,
    conjunct: Conjunct,
}

impl GraphemeState {
    /// State for a cluster that starts with `first`.
    pub(crate) fn new(first: char) -> Self {
        let mut state = Self {
            previous: Class::Control,
            regional_odd: false,
            pictographic: Pictographic::None,
            conjunct: Conjunct::None,
        };
        state.advance(packed(first));
        state
    }

    /// Whether `next` continues the cluster (no boundary before it). On
    /// acceptance the state advances; on a boundary it is left unchanged and
    /// the caller starts a new cluster with [`GraphemeState::new`].
    pub(crate) fn accepts(&mut self, next: char) -> bool {
        let next = packed(next);
        let joins = self.joins(next);
        if joins {
            self.advance(next);
        }
        joins
    }

    fn joins(&self, next: u8) -> bool {
        use Class::*;
        let class = Class::from_packed(next);
        match (self.previous, class) {
            (Cr, Lf) => true,
            (Cr | Lf | Control, _) | (_, Cr | Lf | Control) => false,
            (L, L | V | Lv | Lvt) | (Lv | V, V | T) | (Lvt | T, T) => true,
            (_, Extend | Zwj | SpacingMark) | (Prepend, _) => true,
            _ if next & INCB_MASK == INCB_CONSONANT && self.conjunct == Conjunct::Linked => true,
            (Zwj, _) if next & PICTOGRAPHIC != 0 && self.pictographic == Pictographic::AfterZwj => {
                true
            }
            (RegionalIndicator, RegionalIndicator) => self.regional_odd,
            _ => false,
        }
    }

    fn advance(&mut self, next: u8) {
        let class = Class::from_packed(next);
        self.regional_odd = class == Class::RegionalIndicator
            && !(self.previous == Class::RegionalIndicator && self.regional_odd);
        self.pictographic = if next & PICTOGRAPHIC != 0 {
            Pictographic::Open
        } else {
            match (self.pictographic, class) {
                (Pictographic::Open, Class::Extend) => Pictographic::Open,
                (Pictographic::Open, Class::Zwj) => Pictographic::AfterZwj,
                _ => Pictographic::None,
            }
        };
        self.conjunct = match (next & INCB_MASK, self.conjunct) {
            (INCB_CONSONANT, _) => Conjunct::Consonant,
            (INCB_LINKER, Conjunct::Consonant | Conjunct::Linked) => Conjunct::Linked,
            (INCB_EXTEND, state) => state,
            _ => Conjunct::None,
        };
        self.previous = class;
    }
}

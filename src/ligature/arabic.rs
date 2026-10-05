// SPDX-License-Identifier: GPL-3.0-only
//! Arabic joining-run membership and harakat placement.
//!
//! A cell joins an Arabic shaping run when its base is a joining letter (or
//! tatweel) and every retained combining mark is an Arabic harakat from
//! [`is_arabic_harakat`]. Before shaping, a run is split at any marked cell
//! whose marks the shaping face does not map, so that cell keeps the
//! monochrome combining path (which never draws a missing mark) instead of a
//! `.notdef` box over its base.
//!
//! Shaped mark glyphs keep their OpenType placement relative to their base:
//! each glyph after the first in a marked cell's cluster records its pen
//! offset from the cluster's first glyph in font units
//! ([`crate::atlas::ShapedGlyphKey::mark_offset`]). Unmarked cells and Latin
//! runs record a zero offset, so their atlas identities and pixels are
//! unchanged.

use std::ops::Range;

use swash::text::{Codepoint as _, JoiningType, Script};

use crate::core::Cell;
use crate::text::FontHandle;

/// Arabic joining base (or tatweel) eligible for contextual init/medi/fina/isol.
///
/// Uses Unicode script + joining-type properties from swash. Transparent marks
/// and non-joining Arabic punctuation/digits stay out so they break runs rather
/// than pollute joining context. Platform-neutral.
#[inline]
pub(super) fn is_arabic_joining_base(ch: char) -> bool {
    let props = ch.properties();
    match props.joining_type() {
        JoiningType::D
        | JoiningType::R
        | JoiningType::L
        | JoiningType::Alaph
        | JoiningType::DalathRish => {
            // Tatweel (U+0640) is Join_Causing with Script::Common; include it so
            // kashida stretches participate in Arabic runs.
            props.script() == Script::Arabic || ch == '\u{0640}'
        }
        _ => false,
    }
}

/// Arabic nonspacing marks (General_Category Mn, Joining_Type T) from the
/// Arabic and Arabic Extended-A blocks: Quranic annotation signs, the
/// harakat fathatan through wavy hamza below, superscript alef, and the
/// small high and low signs. A fixed table, because several of these carry
/// Script=Inherited and so cannot be selected by script. Arabic
/// Supplement, Extended-B, and Extended-C marks stay out, as do U+08CA
/// through U+08D2: the shaping engine's character data predates them and
/// does not treat them as transparent, so they would break joining.
const ARABIC_HARAKAT: &[(u32, u32)] = &[
    (0x0610, 0x061A),
    (0x064B, 0x065F),
    (0x0670, 0x0670),
    (0x06D6, 0x06DC),
    (0x06DF, 0x06E4),
    (0x06E7, 0x06E8),
    (0x06EA, 0x06ED),
    (0x08D3, 0x08E1),
    (0x08E3, 0x08FF),
];

/// Whether `ch` is a mark that may ride an Arabic base inside a joining run.
#[inline]
pub(super) fn is_arabic_harakat(ch: char) -> bool {
    let value = u32::from(ch);
    ARABIC_HARAKAT
        .iter()
        .any(|&(first, last)| (first..=last).contains(&value))
}

/// Whether a cell with base `cell.ch` may carry its combining marks into an
/// Arabic joining run. A cell without marks always may.
#[inline]
pub(super) fn marks_join_arabic_run(cell: &Cell) -> bool {
    let marks = cell.combining();
    marks.is_empty()
        || (is_arabic_joining_base(cell.ch) && marks.iter().all(|&m| is_arabic_harakat(m)))
}

/// Sub-ranges of `start..end` that exclude every marked cell carrying a mark
/// `font` does not map. Unmarked runs come back whole.
pub(super) fn mapped_mark_segments(
    cells: &[Cell],
    start: usize,
    end: usize,
    font: &FontHandle,
) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut segment = start;
    for (column, cell) in cells.iter().enumerate().take(end).skip(start) {
        let unmapped = cell
            .combining()
            .iter()
            .any(|&mark| font.glyph_id(mark).0 == 0);
        if unmapped {
            if segment < column {
                segments.push(segment..column);
            }
            segment = column + 1;
        }
    }
    if segment < end {
        segments.push(segment..end);
    }
    segments
}

/// Pen offset of glyph `index` of a cluster from the cluster's first glyph,
/// in font units with y up: preceding advances plus the glyph's own offset,
/// less the first glyph's offset. Saturates to the `i16` range; a NaN offset
/// becomes zero.
pub(super) fn cluster_pen_offset(
    advances_before: f32,
    x: f32,
    y: f32,
    first: (f32, f32),
) -> [i16; 2] {
    let dx = advances_before + x - first.0;
    let dy = y - first.1;
    // `as` saturates and maps NaN to 0, clamping untrusted font positioning.
    [dx.round() as i16, dy.round() as i16]
}

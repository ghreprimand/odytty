// SPDX-License-Identifier: GPL-3.0-only
//! Diagonal-edged blocks (`U+1FB3C..=U+1FB67`) and negative diagonals
//! (`U+1FBBD..=U+1FBBF`) from Symbols for Legacy Computing, drawn with the
//! antialiased polygon filler.
//!
//! Each diagonal block is the cell rectangle cut by one line between two named
//! boundary anchors, keeping the side that holds the named corner. Anchors use
//! the Unicode character names: LEFT, CENTRE and RIGHT are x = 0, w/2 and w;
//! UPPER, UPPER MIDDLE, LOWER MIDDLE and LOWER are y = 0, h/3, 2h/3 and h, the
//! same thirds as the sextants. The 22 upper fills `U+1FB52..=U+1FB67` use the
//! same lines as the lower fills 22 codepoints earlier with the opposite
//! corner, so each pair tiles into one solid cell.
//!
//! The negative diagonals are a fully inked cell with light diagonal strokes
//! cut out: the cross of `U+2573`, the middle-right to lower-centre stroke, and
//! the diamond through the four edge centres.

use super::polygon::{PolygonCoverage, clip_half_plane, stroke};
use super::{Canvas, light_thickness};

/// A cell-boundary anchor: `x = w * x2 / 2`, `y = h * y3 / 3`.
type Anchor = (u8, u8);

const UL: Anchor = (0, 0);
const UML: Anchor = (0, 1);
const LML: Anchor = (0, 2);
const LL: Anchor = (0, 3);
const UC: Anchor = (1, 0);
const LC: Anchor = (1, 3);
const UR: Anchor = (2, 0);
const UMR: Anchor = (2, 1);
const LMR: Anchor = (2, 2);
const LR: Anchor = (2, 3);

/// `U+1FB3C..=U+1FB51`: line start, line end, and the kept corner.
const LOWER_FILLS: [(Anchor, Anchor, Anchor); 22] = [
    (LML, LC, LL),  // 1FB3C
    (LML, LR, LL),  // 1FB3D
    (UML, LC, LL),  // 1FB3E
    (UML, LR, LL),  // 1FB3F
    (UL, LC, LL),   // 1FB40
    (UML, UC, LR),  // 1FB41
    (UML, UR, LR),  // 1FB42
    (LML, UC, LR),  // 1FB43
    (LML, UR, LR),  // 1FB44
    (LL, UC, LR),   // 1FB45
    (LML, UMR, LR), // 1FB46
    (LC, LMR, LR),  // 1FB47
    (LL, LMR, LR),  // 1FB48
    (LC, UMR, LR),  // 1FB49
    (LL, UMR, LR),  // 1FB4A
    (LC, UR, LR),   // 1FB4B
    (UC, UMR, LL),  // 1FB4C
    (UL, UMR, LL),  // 1FB4D
    (UC, LMR, LL),  // 1FB4E
    (UL, LMR, LL),  // 1FB4F
    (UC, LR, LL),   // 1FB50
    (UML, LMR, LL), // 1FB51
];

/// A covered polygon glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PolygonGlyph {
    /// Cell cut by the line `from`-`to`, keeping the side holding `corner`.
    Fill {
        from: Anchor,
        to: Anchor,
        corner: Anchor,
    },
    /// Fully inked cell with light strokes through the listed segments cut
    /// out.
    Negative(&'static [Segment]),
}

/// A stroke between two points `(x2, y2)` at `x = w * x2 / 2`,
/// `y = h * y2 / 2`: cell corners and edge centres.
type Segment = ((u8, u8), (u8, u8));

const NEGATIVE_CROSS: &[Segment] = &[((0, 0), (2, 2)), ((0, 2), (2, 0))];
const NEGATIVE_MIDDLE_RIGHT_TO_LOWER_CENTRE: &[Segment] = &[((2, 1), (1, 2))];
const NEGATIVE_DIAMOND: &[Segment] = &[
    ((1, 0), (2, 1)),
    ((2, 1), (1, 2)),
    ((1, 2), (0, 1)),
    ((0, 1), (1, 0)),
];

/// Map a codepoint to its polygon glyph, or `None`.
pub(super) fn polygon_table(ch: char) -> Option<PolygonGlyph> {
    let cp = ch as u32;
    match cp {
        0x1FB3C..=0x1FB67 => {
            let i = (cp - 0x1FB3C) as usize;
            let (from, to, corner) = LOWER_FILLS[i % 22];
            let corner = if i < 22 { corner } else { opposite(corner) };
            Some(PolygonGlyph::Fill { from, to, corner })
        }
        0x1FBBD => Some(PolygonGlyph::Negative(NEGATIVE_CROSS)),
        0x1FBBE => Some(PolygonGlyph::Negative(
            NEGATIVE_MIDDLE_RIGHT_TO_LOWER_CENTRE,
        )),
        0x1FBBF => Some(PolygonGlyph::Negative(NEGATIVE_DIAMOND)),
        _ => None,
    }
}

/// The diagonally opposite cell corner.
fn opposite((x2, y3): Anchor) -> Anchor {
    (2 - x2, 3 - y3)
}

pub(super) fn render_polygon_glyph(c: &mut Canvas, glyph: PolygonGlyph) {
    let (w, h) = (c.w as f32, c.h as f32);
    let mut cov = PolygonCoverage::new(c.w, c.h);
    match glyph {
        PolygonGlyph::Fill { from, to, corner } => {
            let at = |(x2, y3): Anchor| (w * f32::from(x2) / 2.0, h * f32::from(y3) / 3.0);
            let cell = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
            let mut poly = Vec::with_capacity(5);
            clip_half_plane(&cell, at(from), at(to), at(corner), &mut poly);
            cov.add(&poly);
            cov.write(c, false);
        }
        PolygonGlyph::Negative(segments) => {
            let t = light_thickness(c.w, c.h) as f32;
            let at = |(x2, y2): (u8, u8)| (w * f32::from(x2) / 2.0, h * f32::from(y2) / 2.0);
            for &(a, b) in segments {
                cov.add(&stroke(at(a), at(b), t, t));
            }
            cov.write(c, true);
        }
    }
}

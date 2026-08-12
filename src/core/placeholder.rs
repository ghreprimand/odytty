// SPDX-License-Identifier: GPL-3.0-only
//! Kitty graphics Unicode placeholders (`U=1`): resolving placeholder cells in
//! the text grid into image placements.
//!
//! The protocol splits image display into two halves. A client first creates a
//! *virtual placement* (`a=T,U=1` or `a=p,U=1`) that names an image and the
//! cell grid it should be split across, but places nothing. It then prints
//! ordinary text — the placeholder character [`PLACEHOLDER_CHAR`] (U+10EEEE)
//! carrying combining diacritics — into the cells where the image tiles should
//! appear. Because the position lives in the text itself, the image scrolls,
//! reflows, is overwritten, and is erased exactly as the text is, with no
//! placement bookkeeping. That is the whole point: applications that know
//! nothing about the graphics protocol (tmux, vim, and the TUI toolkits layered
//! on them) move the image correctly for free.
//!
//! Each placeholder cell encodes:
//!
//! - the **image id** in its foreground color (24-bit RGB, or an 8-bit palette
//!   index in 256-color mode), with an optional high byte in a third diacritic;
//! - the **placement id** in its underline color, when one is set;
//! - the **tile row** in the first diacritic and the **tile column** in the
//!   second, as indices into [`ROWCOLUMN_DIACRITICS`].
//!
//! Omitted diacritics inherit from the placeholder cell to the left under the
//! spec's left-to-right rules, implemented in [`decode_row`].
//!
//! ## Untrusted input
//!
//! Every value here originates in PTY bytes. Diacritic indices are bounded by
//! the table (0..297); tile row/column are range-checked against the virtual
//! placement's extent and dropped when out of range; source rectangles are
//! computed in `u64` and narrowed only after clamping to the image bounds; run
//! lengths are clamped to the viewport width by the caller's slice. Nothing
//! here allocates proportional to a client-supplied count.
//!
//! ## Windows
//!
//! Platform-neutral. Placeholder decoding is pure grid arithmetic with no
//! platform surface; the Unix-only part of the Kitty graphics stack remains the
//! shared-memory transport (`t=s`), which is unchanged by this module.

use crate::core::types::{Cell, Color};
use crate::graphics::{GraphicsProtocol, ImageScene, PlacementId, SourceRect, VisiblePlacement};

/// The Kitty graphics Unicode placeholder character.
pub const PLACEHOLDER_CHAR: char = '\u{10EEEE}';

/// Id namespace for synthesized placeholder placements. Real placements are
/// numbered from a counter starting at 1, so reserving the top bit keeps the
/// two spaces disjoint without coordination. The rest of the id is derived from
/// the run's viewport position, which makes the id stable frame to frame — the
/// render layer's frame signature compares placement ids, so an unstable id
/// would force a full rebuild every frame.
const PLACEHOLDER_ID_NAMESPACE: u64 = 1 << 63;

/// The 297 row/column diacritics, ascending by code point so index lookup is a
/// binary search. Derived from the Unicode 6.0.0 combining-class-230 set the
/// kitty graphics protocol specifies for Unicode placeholders: the index of a
/// diacritic in this table IS the row / column / image-id-high-byte value it
/// encodes.
static ROWCOLUMN_DIACRITICS: [char; 297] = [
    '\u{0305}',
    '\u{030D}',
    '\u{030E}',
    '\u{0310}',
    '\u{0312}',
    '\u{033D}',
    '\u{033E}',
    '\u{033F}',
    '\u{0346}',
    '\u{034A}',
    '\u{034B}',
    '\u{034C}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035B}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036A}',
    '\u{036B}',
    '\u{036C}',
    '\u{036D}',
    '\u{036E}',
    '\u{036F}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059C}',
    '\u{059D}',
    '\u{059E}',
    '\u{059F}',
    '\u{05A0}',
    '\u{05A1}',
    '\u{05A8}',
    '\u{05A9}',
    '\u{05AB}',
    '\u{05AC}',
    '\u{05AF}',
    '\u{05C4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065A}',
    '\u{065B}',
    '\u{065D}',
    '\u{065E}',
    '\u{06D6}',
    '\u{06D7}',
    '\u{06D8}',
    '\u{06D9}',
    '\u{06DA}',
    '\u{06DB}',
    '\u{06DC}',
    '\u{06DF}',
    '\u{06E0}',
    '\u{06E1}',
    '\u{06E2}',
    '\u{06E4}',
    '\u{06E7}',
    '\u{06E8}',
    '\u{06EB}',
    '\u{06EC}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073A}',
    '\u{073D}',
    '\u{073F}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074A}',
    '\u{07EB}',
    '\u{07EC}',
    '\u{07ED}',
    '\u{07EE}',
    '\u{07EF}',
    '\u{07F0}',
    '\u{07F1}',
    '\u{07F3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081B}',
    '\u{081C}',
    '\u{081D}',
    '\u{081E}',
    '\u{081F}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082A}',
    '\u{082B}',
    '\u{082C}',
    '\u{082D}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0F82}',
    '\u{0F83}',
    '\u{0F86}',
    '\u{0F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

/// Index of `ch` in [`ROWCOLUMN_DIACRITICS`] — the numeric value that diacritic
/// encodes. `None` for any other character, including ordinary combining marks
/// that happen to sit on a placeholder cell.
pub(crate) fn diacritic_index(ch: char) -> Option<usize> {
    ROWCOLUMN_DIACRITICS.binary_search(&ch).ok()
}

/// The id bits a color carries. Truecolor gives 24 bits, a palette index gives
/// 8. `Color::Default` carries no id, which is what makes an unstyled U+10EEEE
/// (as produced by, say, `cat` on a binary file) resolve to nothing.
fn color_id(color: Color) -> Option<u32> {
    match color {
        Color::Default => None,
        Color::Indexed(index) => Some(u32::from(index)),
        Color::Rgb(red, green, blue) => {
            Some((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue))
        }
    }
}

/// One decoded placeholder cell. `id_high`, `foreground` and `underline` are
/// retained because the next cell to the right may inherit from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaceholderCell {
    image_id: u32,
    placement_id: Option<u32>,
    tile_row: usize,
    tile_column: usize,
    id_high: u32,
    foreground: Color,
    underline: Option<Color>,
}

/// A horizontal run of placeholder cells that address consecutive tiles of one
/// image on one grid row. Runs exist so a full-width image costs one placement
/// instead of one per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaceholderRun {
    pub(crate) image_id: u32,
    pub(crate) placement_id: Option<u32>,
    pub(crate) tile_row: usize,
    pub(crate) tile_column: usize,
    /// First grid column of the run.
    pub(crate) column: usize,
    pub(crate) length: usize,
}

/// Decode one grid row into placeholder runs, applying the spec's left-to-right
/// inheritance rules for omitted diacritics.
pub(crate) fn decode_row(cells: &[Cell]) -> Vec<PlaceholderRun> {
    let mut runs: Vec<PlaceholderRun> = Vec::new();
    let mut previous: Option<(usize, PlaceholderCell)> = None;

    for (column, cell) in cells.iter().enumerate() {
        if cell.ch != PLACEHOLDER_CHAR || cell.wide_continuation {
            previous = None;
            continue;
        }
        let left = previous
            .and_then(|(left_column, decoded)| (left_column + 1 == column).then_some(decoded));
        let Some(decoded) = decode_cell(cell, left) else {
            previous = None;
            continue;
        };

        let extended = runs.last_mut().is_some_and(|run| {
            let contiguous = run.column + run.length == column
                && run.tile_column + run.length == decoded.tile_column;
            let same_image = run.image_id == decoded.image_id
                && run.placement_id == decoded.placement_id
                && run.tile_row == decoded.tile_row;
            if contiguous && same_image {
                run.length += 1;
                true
            } else {
                false
            }
        });
        if !extended {
            runs.push(PlaceholderRun {
                image_id: decoded.image_id,
                placement_id: decoded.placement_id,
                tile_row: decoded.tile_row,
                tile_column: decoded.tile_column,
                column,
                length: 1,
            });
        }
        previous = Some((column, decoded));
    }
    runs
}

/// Decode a single placeholder cell, given the decoded cell immediately to its
/// left (when there is one and it is adjacent).
///
/// The diacritics are read in order as (row, column, image-id high byte); any
/// of the three may be omitted, in which case the missing values come from the
/// left neighbour under the spec's rules. Each rule additionally requires the
/// neighbour to carry the same foreground and underline colors — those colors
/// are the id, so a cell whose colors differ names a different image and can
/// never be a continuation of it.
fn decode_cell(cell: &Cell, left: Option<PlaceholderCell>) -> Option<PlaceholderCell> {
    let foreground = cell.attrs.foreground;
    let low = color_id(foreground)?;
    let underline = cell.attrs.underline_color;
    let placement_id = underline.and_then(color_id).filter(|id| *id != 0);

    let mut marks = cell
        .combining()
        .iter()
        .filter_map(|mark| diacritic_index(*mark));
    let row_mark = marks.next();
    let column_mark = marks.next();
    let high_mark = marks.next();

    // Only a neighbour carrying the same id colors can be inherited from.
    let inherit = left.filter(|prev| prev.foreground == foreground && prev.underline == underline);

    let (tile_row, tile_column, id_high) = match (row_mark, column_mark, high_mark) {
        (Some(row), Some(column), Some(high)) => {
            // The third diacritic is the most significant BYTE of the id, so an
            // index above 255 is not representable. Reject rather than mask:
            // masking would silently resolve to some unrelated image.
            let high = u32::try_from(high).ok().filter(|high| *high <= 0xFF)?;
            (row, column, high)
        }
        (Some(row), Some(column), None) => {
            let high = inherit
                .filter(|prev| prev.tile_row == row && prev.tile_column + 1 == column)
                .map_or(0, |prev| prev.id_high);
            (row, column, high)
        }
        (Some(row), None, None) => match inherit.filter(|prev| prev.tile_row == row) {
            Some(prev) => (row, prev.tile_column.saturating_add(1), prev.id_high),
            // No usable neighbour: this is the start of a row, so the column is
            // zero. The spec's own shorthand example relies on exactly this —
            // it gives a row diacritic to the first cell of each line and
            // nothing at all to the rest.
            None => (row, 0, 0),
        },
        (None, None, None) => {
            let prev = inherit?;
            (
                prev.tile_row,
                prev.tile_column.saturating_add(1),
                prev.id_high,
            )
        }
        // Marks are consumed positionally, so a later mark can never be present
        // without its predecessors.
        _ => return None,
    };

    Some(PlaceholderCell {
        image_id: (id_high << 24) | (low & 0x00FF_FFFF),
        placement_id,
        tile_row,
        tile_column,
        id_high,
        foreground,
        underline,
    })
}

/// Resolve the placeholder runs on one viewport row into visible placements,
/// appending them to `out`. Runs that name an unknown image, an image with no
/// virtual placement, or a tile outside the virtual placement's grid resolve to
/// nothing — a placeholder is inert text until its prototype exists.
pub(crate) fn collect_row_placeholders(
    scene: &ImageScene,
    viewport_row: usize,
    cells: &[Cell],
    out: &mut Vec<VisiblePlacement>,
) {
    for run in decode_row(cells) {
        let Some(prototype) = scene.find_virtual_placement(run.image_id, run.placement_id) else {
            continue;
        };
        let Some(image) = scene.store().get(prototype.image_id) else {
            continue;
        };
        if run.tile_row >= prototype.rows || run.tile_column >= prototype.columns {
            continue;
        }
        // A client may print more placeholder cells than the virtual placement
        // has columns; the spec says only part of the image is then displayed.
        let length = run.length.min(prototype.columns - run.tile_column);
        if length == 0 {
            continue;
        }

        // Tile bounds in source pixels. The division is exact-endpoint based so
        // adjacent tiles abut with no gap and no overlap even when the image
        // does not divide evenly by the grid. u64 throughout: the products are
        // (u32 extent x u32 dimension) and only narrow after the divide.
        let width = u64::from(image.width);
        let height = u64::from(image.height);
        let columns = prototype.columns as u64;
        let rows = prototype.rows as u64;
        let left = (run.tile_column as u64 * width) / columns;
        let right = ((run.tile_column + length) as u64 * width) / columns;
        let top = (run.tile_row as u64 * height) / rows;
        let bottom = ((run.tile_row + 1) as u64 * height) / rows;

        out.push(VisiblePlacement {
            id: placeholder_placement_id(viewport_row, run.column),
            image_id: prototype.image_id,
            protocol: GraphicsProtocol::Kitty,
            row: viewport_row,
            column: run.column,
            source: SourceRect {
                x: left as u32,
                y: top as u32,
                width: (right - left).max(1) as u32,
                height: (bottom - top).max(1) as u32,
            },
            display_columns: length,
            display_rows: 1,
            pixel_offset_x: 0,
            pixel_offset_y: 0,
            z_index: prototype.z_index,
            generation: prototype.generation,
        });
    }
}

/// Stable synthetic placement id for the run starting at (`row`, `column`) of
/// the viewport. Deterministic in the run's position so an unchanged frame
/// produces an unchanged signature.
fn placeholder_placement_id(row: usize, column: usize) -> PlacementId {
    PlacementId(
        PLACEHOLDER_ID_NAMESPACE | ((row as u64 & 0xFFFF) << 32) | ((column as u64 & 0xFFFF) << 16),
    )
}

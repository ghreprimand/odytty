// SPDX-License-Identifier: GPL-3.0-only
//! Contextual joining scripts beyond Arabic: Syriac, N'Ko, and Mandaic.
//!
//! A cell joins a run of its script when its base is a joining letter of that
//! script (Joining_Type D, R, L, or C, from the generated
//! [`joining_data`](super::joining_data) tables) and every retained mark is a
//! transparent nonspacing mark of the same script. Runs never mix scripts.
//! Arabic keeps its own tables and its swash shaping, byte for byte; these
//! scripts are shaped with harfrust, whose Arabic-family shaper implements
//! their joining (including the Syriac final alaph forms), on the run's text
//! face only, as Arabic is. The "plain" comparison for an overlay is each
//! cell's own cmap glyph, which is what the per-cell path draws.
//!
//! Every run is shaped in its script's own direction, right to left for all
//! three, and its glyphs are read back in logical order, so each cell takes
//! the contextual form of its logical position whether the row is drawn in
//! logical cell order (bidi reordering off) or in display order. Shaping a
//! right-to-left script left to right would instead treat the cells as
//! visual order and give the first cell its final form. Inherited marks
//! (for example U+0308 over Syriac) and marks of another script break the
//! run, so their cells keep the monochrome combining path. Platform-neutral.

use harfrust::{Buffer, Font, ShapeOptions, ShapePlan, ShaperFont};

use super::joining_data::{JOINING_LETTERS, JOINING_MARKS};
use super::{RunText, ShapedGlyph};
use crate::text::FontHandle;

/// A contextual joining script with its own run kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum JoiningScript {
    Arabic,
    Syriac,
    Nko,
    Mandaic,
}

impl JoiningScript {
    fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Syriac),
            2 => Some(Self::Nko),
            3 => Some(Self::Mandaic),
            _ => None,
        }
    }

    fn id(self) -> u8 {
        match self {
            Self::Arabic => 0,
            Self::Syriac => 1,
            Self::Nko => 2,
            Self::Mandaic => 3,
        }
    }

    /// The script's horizontal direction.
    fn right_to_left(self) -> bool {
        match self {
            Self::Arabic | Self::Syriac | Self::Nko | Self::Mandaic => true,
        }
    }

    fn harfrust_script(self) -> harfrust::Script {
        match self {
            Self::Arabic => harfrust::Script::ARABIC,
            Self::Syriac => harfrust::Script::SYRIAC,
            Self::Nko => harfrust::Script::NKO,
            Self::Mandaic => harfrust::Script::MANDAIC,
        }
    }
}

/// First scalar of the generated tables; anything below it is in neither.
const FIRST_TABLE_SCALAR: u32 = 0x0710;

fn lookup(table: &[(u32, u32, u8)], ch: char) -> Option<u8> {
    let cp = ch as u32;
    if cp < FIRST_TABLE_SCALAR {
        return None;
    }
    let index = table.partition_point(|&(_, end, _)| end < cp);
    table
        .get(index)
        .filter(|&&(start, _, _)| start <= cp)
        .map(|&(_, _, id)| id)
}

/// The script whose joining run a letter `ch` belongs to, other than Arabic.
#[inline]
pub(super) fn other_joining_script(ch: char) -> Option<JoiningScript> {
    lookup(JOINING_LETTERS, ch).and_then(JoiningScript::from_id)
}

/// Whether `ch` is a transparent mark of `script` that rides its letter
/// inside a joining run. Arabic uses its harakat table.
#[inline]
pub(super) fn is_joining_mark(script: JoiningScript, ch: char) -> bool {
    match script {
        JoiningScript::Arabic => super::arabic::is_arabic_harakat(ch),
        other => lookup(JOINING_MARKS, ch) == Some(other.id()),
    }
}

/// The per-cell glyphs of `run_text`: each scalar's own cmap glyph, at its
/// cell. This is the comparison that decides whether joining changed a cell.
pub(super) fn cmap_glyphs(run_text: &RunText, font: &FontHandle) -> Vec<ShapedGlyph> {
    let mut glyphs = Vec::with_capacity(run_text.text.len());
    for (column, range) in run_text.cell_bytes.iter().enumerate() {
        let marked = run_text.marked.get(column).copied().unwrap_or(false);
        for ch in run_text.text[range.clone()].chars() {
            glyphs.push(ShapedGlyph {
                id: font.glyph_id(ch).0,
                source_start: column,
                source_end: column + 1,
                mark_offset: [0, 0],
                marked,
            });
        }
    }
    glyphs
}

fn direction(script: JoiningScript) -> harfrust::Direction {
    if script.right_to_left() {
        harfrust::Direction::RightToLeft
    } else {
        harfrust::Direction::LeftToRight
    }
}

/// The reusable shaping plan of `script` on `data`, in the script's
/// direction, with no language and the font's default features.
pub(super) fn plan(data: &Font, script: JoiningScript) -> ShapePlan {
    ShapePlan::new(
        data,
        direction(script),
        Some(script.harfrust_script()),
        None,
        &[],
    )
}

/// Shape a joining run of `script` with harfrust in the script's direction
/// and return its glyphs in logical order, each with its source cells and,
/// for a mark after its cluster's first glyph, its font-unit pen offset from
/// that glyph. `None` for a shaping error, `.notdef`, or a glyph id past
/// `u16`.
pub(super) fn shape_joining_run(
    data: &Font,
    plan: &ShapePlan,
    run_text: &RunText,
    script: JoiningScript,
) -> Option<Vec<ShapedGlyph>> {
    let right_to_left = script.right_to_left();
    let font = ShaperFont::new(data);
    let mut buffer = Buffer::new();
    buffer.push_str(&run_text.text);
    buffer.set_script(Some(script.harfrust_script()));
    buffer.set_direction(direction(script));
    harfrust::shape(&font, &mut buffer, ShapeOptions::new().plan(Some(plan))).ok()?;
    // Visual pen positions first, then logical order.
    let mut pen: i32 = 0;
    let mut placed = Vec::with_capacity(buffer.len());
    for (info, position) in buffer.glyph_infos().iter().zip(buffer.glyph_positions()) {
        let id = u16::try_from(info.glyph_id).ok().filter(|&id| id != 0)?;
        placed.push((
            id,
            info.cluster as usize,
            pen.saturating_add(position.x_offset),
            position.y_offset,
        ));
        pen = pen.saturating_add(position.x_advance);
    }
    if right_to_left {
        placed.reverse();
    }
    let mut starts: Vec<usize> = placed.iter().map(|&(_, cluster, _, _)| cluster).collect();
    starts.sort_unstable();
    starts.dedup();
    let mut glyphs = Vec::with_capacity(placed.len());
    let mut first: Option<(usize, i32, i32)> = None;
    for &(id, cluster, x, y) in &placed {
        let source_start = run_text.column_at_byte(cluster)?;
        let next = starts
            .iter()
            .find(|&&start| start > cluster)
            .copied()
            .unwrap_or(run_text.text.len());
        let source_end = next
            .checked_sub(1)
            .and_then(|last| run_text.column_at_byte(last))
            .map_or(source_start + 1, |last| last.max(source_start) + 1);
        let marked = run_text
            .marked
            .get(source_start..source_end)
            .is_some_and(|cells| cells.contains(&true));
        let mark_offset = match first {
            Some((start, x0, y0)) if start == cluster && marked => [
                clamp_i16(x.saturating_sub(x0)),
                clamp_i16(y.saturating_sub(y0)),
            ],
            _ => {
                first = Some((cluster, x, y));
                [0, 0]
            }
        };
        glyphs.push(ShapedGlyph {
            id,
            source_start,
            source_end,
            mark_offset,
            marked,
        });
    }
    Some(glyphs)
}

/// Clamp untrusted font positioning into the overlay key's range.
fn clamp_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

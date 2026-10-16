// SPDX-License-Identifier: GPL-3.0-only
//! Directional shaping runs for display order (`bidi_reorder`).
//!
//! On a row the display map reorders, compatible runs split wherever the
//! resolved level changes, so no shaping context or ligature crosses a
//! direction boundary. Each level run is shaped with an explicit direction:
//! joining runs (Arabic, Syriac, N'Ko, Mandaic) at an odd level shape right
//! to left. Latin and operator runs at an odd level are not shaped: their
//! cells draw as scalar glyphs, with mirroring applied at presentation, so a
//! left-to-right ligature such as `->` never draws inside right-to-left
//! text. Rows the map leaves in logical order carry no levels and shape
//! exactly as without a map. Rows of an explicit-mode paragraph, in either
//! direction, hold text the application arranged itself, so no run on them
//! takes contextual joining: their levels carry [`EXPLICIT_LEVEL`], which also
//! keeps their row-cache keys apart.

use swash::shape::Direction;

use super::{LigatureFonts, LigatureShaper, RowPlan, RunText, joining_script, shaping_run_bounds};
use crate::core::Cell;
use crate::grid::{BidiDisplayMap, ColorRunCoverage};
#[cfg(test)]
use crate::{
    core::Snapshot,
    grid::ColorGlyphRun,
    ligature::{LatinShapingFeatures, LigatureRun},
};

/// Marks a level on a row of an explicit-mode paragraph. UAX #9 levels stay
/// below 128, so the bit never collides with a level, and it keeps a level's
/// parity.
pub(super) const EXPLICIT_LEVEL: u8 = 0x80;

/// The level vector that keys and segments `row`: every cell's resolved level
/// when `bidi` reorders the row, with [`EXPLICIT_LEVEL`] set on an
/// explicit-mode row (which keeps levels even in the identity layout), and
/// empty otherwise.
pub(super) fn row_levels(bidi: Option<&BidiDisplayMap>, row: usize, columns: usize) -> Vec<u8> {
    match bidi {
        Some(map) if map.row_is_reordered(row) || map.row_is_explicit(row) => {
            let marker = if map.row_is_explicit(row) {
                EXPLICIT_LEVEL
            } else {
                0
            };
            (0..columns)
                .map(|column| map.level(row, column) | marker)
                .collect()
        }
        _ => Vec::new(),
    }
}

impl LigatureShaper {
    /// Shape one reordered row per level run. `levels` holds one level per
    /// cell of `cells`.
    pub(super) fn shape_row_levels<F: LigatureFonts>(
        &mut self,
        cells: &[Cell],
        fonts: &F,
        row: usize,
        coverage: &ColorRunCoverage,
        levels: &[u8],
    ) -> RowPlan {
        let level = |column: usize| levels.get(column).copied().unwrap_or(0);
        let mut runs = Vec::new();
        for (start, end, style) in shaping_run_bounds(cells, row, coverage, fonts) {
            let mut segment = start;
            while segment < end {
                let segment_level = level(segment);
                let mut segment_end = segment + 1;
                while segment_end < end && level(segment_end) == segment_level {
                    segment_end += 1;
                }
                let right_to_left = segment_level % 2 == 1;
                let explicit = segment_level & EXPLICIT_LEVEL != 0;
                let joining = joining_script(cells[segment].ch).is_some();
                // Explicit text takes no contextual joining in either
                // direction; its left-to-right Latin ligatures stay.
                let shaped = if explicit {
                    !joining && !right_to_left
                } else {
                    !right_to_left || joining
                };
                if segment_end - segment >= 2 && shaped {
                    let direction = if right_to_left {
                        Direction::RightToLeft
                    } else {
                        Direction::LeftToRight
                    };
                    let run_text = RunText::from_cells(&cells[segment..segment_end]);
                    runs.extend(self.shape_compatible_run(
                        &run_text,
                        segment,
                        style,
                        fonts.ligature_font(style),
                        direction,
                    ));
                }
                segment = segment_end;
            }
        }
        RowPlan { runs }
    }

    /// Presentation runs for `snapshot` under `display` with default features,
    /// through the production cached path.
    #[cfg(test)]
    pub(crate) fn build_runs_bidi<F: LigatureFonts>(
        &mut self,
        snapshot: &Snapshot,
        fonts: &F,
        color_runs: &[ColorGlyphRun],
        display: &BidiDisplayMap,
    ) -> Vec<LigatureRun> {
        self.build_runs_with_features_and_bidi(
            true,
            snapshot,
            fonts,
            color_runs,
            LatinShapingFeatures::default(),
            Some(display),
        )
    }
}

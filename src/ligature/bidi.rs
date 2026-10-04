// SPDX-License-Identifier: GPL-3.0-only
//! Test-only bidi shaping seam: directional shaping runs for display order.
//!
//! Compatible runs split wherever the resolved level changes, so no shaping
//! context or ligature crosses a direction boundary. Each level run is shaped
//! with an explicit direction: Arabic joining runs at an odd level shape
//! right to left. Latin and operator runs at an odd level are not shaped:
//! their cells draw as scalar glyphs, with mirroring applied at presentation,
//! so a left-to-right ligature such as `->` never draws inside right-to-left
//! text. Plans are not cached. No production path calls this module.

use swash::shape::Direction;

use super::{
    LigatureFonts, LigatureRun, LigatureShaper, RunText, compatible_run_bounds,
    is_arabic_joining_base,
};
use crate::core::Snapshot;
use crate::grid::{BidiDisplayMap, ColorGlyphRun, ColorRunCoverage};

impl LigatureShaper {
    /// Presentation runs for `snapshot` under `display`, shaped per level run.
    pub(crate) fn build_runs_bidi<F: LigatureFonts>(
        &mut self,
        snapshot: &Snapshot,
        fonts: &F,
        color_runs: &[ColorGlyphRun],
        display: &BidiDisplayMap,
    ) -> Vec<LigatureRun> {
        let cols = snapshot.dimensions.columns;
        let coverage = ColorRunCoverage::new(color_runs, cols, snapshot.dimensions.rows);
        let mut output = Vec::new();
        for (row, cells) in snapshot.cells.chunks(cols).enumerate() {
            for (start, end, style) in compatible_run_bounds(cells, row, &coverage) {
                let mut segment = start;
                while segment < end {
                    let level = display.level(row, segment);
                    let mut segment_end = segment + 1;
                    while segment_end < end && display.level(row, segment_end) == level {
                        segment_end += 1;
                    }
                    let right_to_left = level % 2 == 1;
                    let arabic = is_arabic_joining_base(cells[segment].ch);
                    if segment_end - segment >= 2 && (!right_to_left || arabic) {
                        let direction = if right_to_left {
                            Direction::RightToLeft
                        } else {
                            Direction::LeftToRight
                        };
                        let run_text = RunText::from_cells(&cells[segment..segment_end]);
                        let runs = self.shape_compatible_run(
                            &run_text,
                            segment,
                            style,
                            fonts.ligature_font(style),
                            direction,
                        );
                        output.extend(runs.into_iter().map(|run| LigatureRun {
                            row,
                            start: run.start,
                            end: run.end,
                            glyphs: run.glyphs,
                        }));
                    }
                    segment = segment_end;
                }
            }
        }
        output
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Per-command success/fail gutter (SH2): a thin coloured bar at the left edge
//! of each finished command's prompt row, green for an explicit `exit 0` and red
//! for a non-zero exit.
//!
//! The status is sourced from the OSC 133 command blocks in core
//! ([`command_blocks`] / [`command_status`]) and the colour from the active
//! ANSI palette (index 10 bright-green / index 9 bright-red), so it daltonises
//! for free through the U4 CVD adaptation that
//! already rewrites the palette. Only [`CommandStatus::Success`] /
//! [`CommandStatus::Fail`] draw a bar; a still-running command
//! ([`CommandStatus::Running`]) or a prompt with no recoverable exit
//! ([`CommandStatus::Unknown`]) deliberately shows nothing, so the gutter never
//! presents ambiguity as a verdict.
//!
//! Ships off by default behind the `command_status_gutter` setting. While off,
//! [`App::command_status_gutter_overlays`] returns no quads before touching the
//! terminal, so the composed overlay list is byte-identical to today and the
//! default render path is unchanged.

use super::*;

use crate::core::{CommandStatus, PromptKind, command_blocks, command_status};
use crate::grid::SolidQuad;
use crate::theme::Srgb;

/// Width of the gutter bar in physical pixels. Thin, so with the default
/// non-zero window padding the bar lives in the left margin and overlaps no
/// glyphs; at zero padding it is a narrow sliver at the left of column 0.
const GUTTER_WIDTH_PX: f32 = 3.0;

/// Fraction of a cell height trimmed from the top and bottom of each bar, so
/// bars on adjacent rows read as distinct segments rather than one column.
const GUTTER_ROW_INSET_FRAC: f32 = 0.12;

/// Opacity of the gutter bars. Near-opaque so the verdict reads clearly while
/// still sitting visually behind crisp text were padding ever zero.
const GUTTER_ALPHA: f32 = 0.9;

/// Bright-green ANSI index for a success bar.
const ANSI_BRIGHT_GREEN: usize = 10;
/// Bright-red ANSI index for a failure bar.
const ANSI_BRIGHT_RED: usize = 9;

impl App {
    /// Build the success/fail gutter overlay quads for the current viewport, or
    /// an empty list when the gutter is disabled (the default) or there are no
    /// finished commands on screen.
    ///
    /// Gated on `command_status_gutter`: when off it returns before locking the
    /// terminal or reading marks, so the off path adds no work and the composed
    /// overlay set is byte-identical to today. `pub(in crate::native)` so the
    /// native test suite can assert the inverted gate directly.
    pub(in crate::native) fn command_status_gutter_overlays(
        &self,
        scrollback_len: usize,
        cell: CellSize,
        padding: WindowPadding,
    ) -> Vec<SolidQuad> {
        if !self.settings.command_status_gutter {
            return Vec::new();
        }
        let viewport_offset = self.viewport.offset();
        let marks = match self.terminal.lock() {
            Ok(terminal) => terminal.screen().prompt_marks(),
            Err(_) => return Vec::new(),
        };
        command_status_gutter_quads(
            &marks,
            viewport_offset,
            scrollback_len,
            self.grid,
            cell,
            padding,
            &self.effective_theme.palette,
        )
    }
}

/// Pure geometry: map the on-screen finished-command prompt rows to gutter bar
/// quads. One bar per visible [`CommandStatus::Success`] / [`CommandStatus::Fail`]
/// block, anchored at the block's prompt row. Absolute rows follow the OSC 133
/// mark convention (row `0` = oldest scrollback); the live viewport's top
/// absolute row is `scrollback_len - viewport_offset`.
pub(super) fn command_status_gutter_quads(
    marks: &[(usize, PromptKind)],
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
    padding: WindowPadding,
    palette: &[Srgb; 16],
) -> Vec<SolidQuad> {
    let rows = dimensions.rows;
    let cell_h = cell.height as f32;
    if rows == 0 || cell_h <= 0.0 {
        return Vec::new();
    }
    let top_abs = scrollback_len.saturating_sub(viewport_offset);
    let pad = padding.as_f32();
    let inset = cell_h * GUTTER_ROW_INSET_FRAC;
    let x1 = GUTTER_WIDTH_PX;

    let mut quads = Vec::new();
    for block in command_blocks(marks) {
        let color = match command_status(&block) {
            CommandStatus::Success => palette_bar_color(palette[ANSI_BRIGHT_GREEN]),
            CommandStatus::Fail => palette_bar_color(palette[ANSI_BRIGHT_RED]),
            CommandStatus::Running | CommandStatus::Unknown => continue,
        };
        let row = block.prompt_row;
        if row < top_abs {
            continue;
        }
        let screen_row = row - top_abs;
        if screen_row >= rows {
            continue;
        }
        let y0 = pad + screen_row as f32 * cell_h + inset;
        let y1 = pad + (screen_row + 1) as f32 * cell_h - inset;
        quads.push(SolidQuad {
            rect: [0.0, y0, x1, y1],
            color,
        });
    }
    quads
}

/// Convert an ANSI palette colour to a linear-RGBA gutter bar colour at the
/// gutter opacity. Mirrors the scroll-indicator colour path
/// ([`text::foreground_linear`]) so the bar matches the renderer's colour space.
fn palette_bar_color(color: Srgb) -> [f32; 4] {
    let mut linear = text::foreground_linear(Color::Rgb(color.0, color.1, color.2));
    linear[3] = GUTTER_ALPHA;
    linear
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLS: usize = 80;
    const ROWS: usize = 24;
    const CELL: CellSize = CellSize {
        width: 8,
        height: 10,
        baseline: 8,
    };

    fn dims() -> Dimensions {
        Dimensions::new(COLS, ROWS)
    }

    /// A palette where green/red are unmistakable primaries so colour checks are
    /// unambiguous regardless of the default theme.
    fn palette() -> [Srgb; 16] {
        let mut p = [(0, 0, 0); 16];
        p[ANSI_BRIGHT_GREEN] = (0, 255, 0);
        p[ANSI_BRIGHT_RED] = (255, 0, 0);
        p
    }

    /// `A` prompt at `prompt_row`, `C` output one row later, `D exit` two rows
    /// later — a complete finished command block.
    fn finished(prompt_row: usize, exit: i32) -> Vec<(usize, PromptKind)> {
        vec![
            (prompt_row, PromptKind::PromptStart),
            (prompt_row + 1, PromptKind::OutputStart),
            (prompt_row + 2, PromptKind::CommandEnd { exit: Some(exit) }),
        ]
    }

    #[test]
    fn no_marks_yields_no_bars() {
        let quads =
            command_status_gutter_quads(&[], 0, 0, dims(), CELL, WindowPadding::ZERO, &palette());
        assert!(quads.is_empty());
    }

    #[test]
    fn success_bar_is_green_fail_bar_is_red() {
        let green = text::foreground_linear(Color::Rgb(0, 255, 0));
        let red = text::foreground_linear(Color::Rgb(255, 0, 0));

        let ok = command_status_gutter_quads(
            &finished(0, 0),
            0,
            0,
            dims(),
            CELL,
            WindowPadding::ZERO,
            &palette(),
        );
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].color, [green[0], green[1], green[2], GUTTER_ALPHA]);

        let fail = command_status_gutter_quads(
            &finished(0, 1),
            0,
            0,
            dims(),
            CELL,
            WindowPadding::ZERO,
            &palette(),
        );
        assert_eq!(fail.len(), 1);
        assert_eq!(fail[0].color, [red[0], red[1], red[2], GUTTER_ALPHA]);
    }

    #[test]
    fn bar_sits_at_the_prompt_row_left_edge_with_inset() {
        // Prompt on absolute row 3, viewport at the live tail (offset 0,
        // scrollback 0) → screen row 3.
        let quads = command_status_gutter_quads(
            &finished(3, 0),
            0,
            0,
            dims(),
            CELL,
            WindowPadding::ZERO,
            &palette(),
        );
        assert_eq!(quads.len(), 1);
        let inset = CELL.height as f32 * GUTTER_ROW_INSET_FRAC;
        let cell_h = CELL.height as f32;
        assert_eq!(
            quads[0].rect,
            [
                0.0,
                3.0 * cell_h + inset,
                GUTTER_WIDTH_PX,
                4.0 * cell_h - inset
            ]
        );
    }

    #[test]
    fn bar_persists_after_the_next_prompt_merges_over_the_command_end() {
        // The universal shell shape: `D` and the next prompt's `A` land on one
        // row, stamped as a merged PromptStartAfterEnd. The finished block's
        // bar must still draw once the next prompt exists — this is the
        // flash-then-vanish regression case.
        let green = text::foreground_linear(Color::Rgb(0, 255, 0));
        let marks = vec![
            (0, PromptKind::PromptStart),
            (1, PromptKind::OutputStart),
            (2, PromptKind::PromptStartAfterEnd { prev_exit: Some(0) }),
        ];
        let quads = command_status_gutter_quads(
            &marks,
            0,
            0,
            dims(),
            CELL,
            WindowPadding::ZERO,
            &palette(),
        );
        assert_eq!(quads.len(), 1, "the finished block keeps its bar");
        assert_eq!(quads[0].color, [green[0], green[1], green[2], GUTTER_ALPHA]);
        // The bar sits on the finished block's prompt row, not the new one.
        let inset = CELL.height as f32 * GUTTER_ROW_INSET_FRAC;
        assert_eq!(quads[0].rect[1], inset);
    }

    #[test]
    fn running_and_unknown_blocks_draw_nothing() {
        // `A` + `C` with no `D` → Open output → Running → no bar.
        let running = vec![(0, PromptKind::PromptStart), (1, PromptKind::OutputStart)];
        assert!(
            command_status_gutter_quads(
                &running,
                0,
                0,
                dims(),
                CELL,
                WindowPadding::ZERO,
                &palette()
            )
            .is_empty()
        );

        // Lone `A` (prompt awaiting input) → Empty output, no exit → Unknown.
        let awaiting = vec![(0, PromptKind::PromptStart)];
        assert!(
            command_status_gutter_quads(
                &awaiting,
                0,
                0,
                dims(),
                CELL,
                WindowPadding::ZERO,
                &palette()
            )
            .is_empty()
        );
    }

    #[test]
    fn rows_scrolled_out_of_the_viewport_are_culled() {
        // Prompt on absolute row 0, but the viewport top is row 5 (scrolled to
        // the live tail past it): the bar is off-screen above → culled.
        let above = command_status_gutter_quads(
            &finished(0, 0),
            0,
            5,
            dims(),
            CELL,
            WindowPadding::ZERO,
            &palette(),
        );
        assert!(above.is_empty());

        // Prompt on absolute row 100 while only 24 rows are visible from the top
        // → off-screen below → culled.
        let below = command_status_gutter_quads(
            &finished(100, 0),
            0,
            0,
            dims(),
            CELL,
            WindowPadding::ZERO,
            &palette(),
        );
        assert!(below.is_empty());
    }

    #[test]
    fn padding_offsets_the_bar_vertically() {
        let pad = WindowPadding::from_logical(12.0, 1.0);
        let quads =
            command_status_gutter_quads(&finished(0, 0), 0, 0, dims(), CELL, pad, &palette());
        assert_eq!(quads.len(), 1);
        let inset = CELL.height as f32 * GUTTER_ROW_INSET_FRAC;
        assert_eq!(quads[0].rect[1], pad.as_f32() + inset);
    }
}

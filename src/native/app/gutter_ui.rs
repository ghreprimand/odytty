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
//! Ships on by default behind the `command_status_gutter` setting; command
//! marks are on out of the box, so the verdict bars appear wherever an
//! integrated shell emits them. While off,
//! [`App::command_status_gutter_overlays`] returns no quads before touching the
//! terminal, so the composed overlay list is byte-identical to a plain margin
//! and that render path is unchanged.

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
    /// an empty list when the gutter is disabled or there are no finished
    /// commands on screen.
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
    command_status_gutter_quads_at_origin(
        marks,
        viewport_offset,
        scrollback_len,
        dimensions,
        cell,
        [0.0, padding.as_f32()],
        palette,
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PaneGutterGeometry {
    pub(super) dimensions: Dimensions,
    pub(super) cell: CellSize,
    pub(super) origin: [f32; 2],
    pub(super) clip_rect: [f32; 4],
}

/// Multi-pane analogue of [`command_status_gutter_quads`]. The mark geometry
/// is built directly in window coordinates from the pane's gliding content
/// origin, then clipped to that pane's at-rest grid rectangle so a status bar
/// cannot cross a divider during sub-row scrolling.
pub(super) fn pane_command_status_gutter_quads(
    marks: &[(usize, PromptKind)],
    viewport_offset: usize,
    scrollback_len: usize,
    geometry: PaneGutterGeometry,
    palette: &[Srgb; 16],
) -> Vec<SolidQuad> {
    let mut quads = command_status_gutter_quads_at_origin(
        marks,
        viewport_offset,
        scrollback_len,
        geometry.dimensions,
        geometry.cell,
        geometry.origin,
        palette,
    );
    for quad in &mut quads {
        quad.rect[0] = quad.rect[0].max(geometry.clip_rect[0]);
        quad.rect[1] = quad.rect[1].max(geometry.clip_rect[1]);
        quad.rect[2] = quad.rect[2].min(geometry.clip_rect[2]);
        quad.rect[3] = quad.rect[3].min(geometry.clip_rect[3]);
    }
    quads.retain(|quad| quad.rect[0] < quad.rect[2] && quad.rect[1] < quad.rect[3]);
    quads
}

fn command_status_gutter_quads_at_origin(
    marks: &[(usize, PromptKind)],
    viewport_offset: usize,
    scrollback_len: usize,
    dimensions: Dimensions,
    cell: CellSize,
    origin: [f32; 2],
    palette: &[Srgb; 16],
) -> Vec<SolidQuad> {
    let rows = dimensions.rows;
    let cell_h = cell.height as f32;
    if rows == 0 || cell_h <= 0.0 {
        return Vec::new();
    }
    let top_abs = scrollback_len.saturating_sub(viewport_offset);
    let inset = cell_h * GUTTER_ROW_INSET_FRAC;
    let x1 = origin[0] + GUTTER_WIDTH_PX;

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
        let y0 = origin[1] + screen_row as f32 * cell_h + inset;
        let y1 = origin[1] + (screen_row + 1) as f32 * cell_h - inset;
        quads.push(SolidQuad {
            rect: [origin[0], y0, x1, y1],
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

    #[cfg(unix)]
    #[test]
    fn fedora_style_bash_stream_reaches_success_and_fail_gutter_bars() {
        use std::process::{Command, Stdio};

        let Some(bash) = ["/bin/bash", "/usr/bin/bash"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .find(|path| path.exists())
        else {
            return;
        };
        let dir =
            std::env::temp_dir().join(format!("odytty-fedora-bash-gutter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create Fedora Bash fixture directory");

        // Fedora's systemd OSC-context profile declares PROMPT_COMMAND as an
        // array whose first element is deliberately empty, installs its helper
        // in a later element, and prefixes PS0 with a command substitution.
        // Model that startup shape in the user rcfile which OdyTTY's generated
        // wrapper sources first.
        // Merge stderr before Bash starts prompting so the capture preserves
        // the exact PTY-equivalent order of prompts, marks, and command echo.
        std::fs::write(
            dir.join(".bashrc"),
            "exec 2>&1\n\
             PROMPT_COMMAND=('')\n\
             __systemd_osc_context_precmdline() { printf '\\e]3008;start=test;type=shell\\e\\\\'; }\n\
             __systemd_osc_context_ps0() { printf '\\e]3008;start=command;type=command\\e\\\\'; }\n\
             PROMPT_COMMAND+=(__systemd_osc_context_precmdline)\n\
             PS0='$( __systemd_osc_context_ps0 )'\n\
             PS1='P\\$ '\n",
        )
        .expect("write Fedora Bash fixture");
        let wrapper = dir.join("odytty.bash");
        std::fs::write(&wrapper, crate::shell_integration::bash_integration_rc())
            .expect("write OdyTTY Bash wrapper");

        let mut child = Command::new(bash)
            .arg("--rcfile")
            .arg(&wrapper)
            .arg("-i")
            .env("HOME", &dir)
            .env("TERM", "xterm-256color")
            .env_remove("ODYTTY_SHELL_INTEGRATION")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Bash");
        child
            .stdin
            .take()
            .expect("Bash stdin")
            .write_all(b"true\nfalse\nexit\n")
            .expect("drive Bash");
        let output = child.wait_with_output().expect("wait for Bash");
        if !output.stdout.windows(6).any(|window| window == b"]133;A") {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let mut terminal = crate::core::Terminal::new(COLS, ROWS);
        terminal.advance(&output.stdout);
        let marks = terminal.prompt_marks();
        let quads = command_status_gutter_quads(
            &marks,
            0,
            terminal.screen().scrollback_len(),
            dims(),
            CELL,
            WindowPadding::ZERO,
            &palette(),
        );

        let green = text::foreground_linear(Color::Rgb(0, 255, 0));
        let red = text::foreground_linear(Color::Rgb(255, 0, 0));
        assert!(
            quads
                .iter()
                .any(|quad| quad.color == [green[0], green[1], green[2], GUTTER_ALPHA]),
            "true must produce a green gutter bar; marks={marks:?}"
        );
        assert!(
            quads
                .iter()
                .any(|quad| quad.color == [red[0], red[1], red[2], GUTTER_ALPHA]),
            "false must produce a red gutter bar; marks={marks:?}"
        );

        // The split renderer consumes the same terminal-derived marks but must
        // translate them into the pane's window-space origin. This is the
        // production helper used by every PaneRender overlay lane.
        let pane_origin = [37.0, 23.0];
        let pane_clip = [
            37.0,
            23.0,
            37.0 + COLS as f32 * 8.0,
            23.0 + ROWS as f32 * 10.0,
        ];
        let pane_quads = pane_command_status_gutter_quads(
            &marks,
            0,
            terminal.screen().scrollback_len(),
            PaneGutterGeometry {
                dimensions: dims(),
                cell: CELL,
                origin: pane_origin,
                clip_rect: pane_clip,
            },
            &palette(),
        );
        assert!(
            pane_quads
                .iter()
                .any(|quad| quad.color == [green[0], green[1], green[2], GUTTER_ALPHA]),
            "split-pane true must produce a translated green gutter bar; marks={marks:?}"
        );
        assert!(
            pane_quads
                .iter()
                .any(|quad| quad.color == [red[0], red[1], red[2], GUTTER_ALPHA]),
            "split-pane false must produce a translated red gutter bar; marks={marks:?}"
        );
        assert!(
            pane_quads.iter().all(|quad| quad.rect[0] == pane_origin[0]),
            "split-pane gutter must be translated to the pane origin"
        );
        let _ = std::fs::remove_dir_all(dir);
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

    #[test]
    fn pane_bar_is_clipped_to_its_grid_rect() {
        let origin = [40.0, 18.0];
        let clip = [41.0, 20.0, 400.0, 25.0];
        let quads = pane_command_status_gutter_quads(
            &finished(0, 0),
            0,
            0,
            PaneGutterGeometry {
                dimensions: dims(),
                cell: CELL,
                origin,
                clip_rect: clip,
            },
            &palette(),
        );
        assert_eq!(quads.len(), 1);
        assert_eq!(quads[0].rect, [41.0, 20.0, 43.0, 25.0]);
    }
}

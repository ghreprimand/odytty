// SPDX-License-Identifier: GPL-3.0-only
//! Verified OSC 133 command ranges for user-authorized actions.
//!
//! The terminal grid and scrollback remain authoritative. This module derives
//! transient absolute-row ranges from logical-line prompt marks; it never owns
//! terminal text or turns the grid into a block document. Existing tolerant
//! [`super::CommandBlock`] derivation still serves presentation. User actions
//! use [`verified_command_ranges`], which requires one ordered prompt/output/end
//! triple and fails closed on absent, partial, duplicated, or stale metadata.

use super::PromptKind;
use crate::core::search::AbsolutePoint;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedCommandRange {
    pub prompt_row: usize,
    pub prompt_column: usize,
    pub output_start: usize,
    /// Inclusive last output row, excluding the OSC 133 `D` boundary.
    pub output_end: usize,
    /// Inclusive last output column when the `D` boundary shares its row with
    /// a final unterminated output fragment. `None` means the full row.
    pub output_end_column: Option<usize>,
    pub exit: Option<i32>,
}

/// Opaque, generation-bound authority for one verified command range.
///
/// Native code may retain this handle across a menu or save-dialog interaction,
/// but it cannot inspect or rewrite its anchors. Resolution must go through
/// [`resolve_verified_command_handle`], which rejects a live-buffer generation
/// change as well as evicted, partial, or otherwise changed marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRangeHandle {
    generation: u64,
    prompt_row: usize,
    prompt_column: usize,
    output_start: usize,
    output_end: usize,
    output_end_column: Option<usize>,
    exit: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRangePart {
    Output,
    PromptAndCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDirection {
    Prev,
    Next,
}

/// Derive only complete, internally ordered command ranges.
///
/// A following prompt without an explicit `D` does not make a range
/// trustworthy. A merged `D`+`A` prompt is explicit because
/// [`PromptKind::PromptStartAfterEnd`] records that collision. Duplicate or
/// out-of-order marks invalidate the block instead of choosing a winner.
pub fn verified_command_ranges(
    marks: &[(usize, PromptKind)],
    columns: usize,
    last_row: usize,
) -> Vec<VerifiedCommandRange> {
    if columns == 0 {
        return Vec::new();
    }
    if marks.windows(2).any(|pair| pair[0].0 > pair[1].0) {
        return Vec::new();
    }
    let prompt_indices: Vec<usize> = marks
        .iter()
        .enumerate()
        .filter(|(_, (_, kind))| is_prompt(*kind))
        .map(|(index, _)| index)
        .collect();
    let mut ranges = Vec::with_capacity(prompt_indices.len());

    for (slot, &prompt_index) in prompt_indices.iter().enumerate() {
        let (prompt_row, prompt_column) = prompt_point(marks[prompt_index], columns);
        if prompt_row > last_row {
            continue;
        }
        let next_prompt_index = prompt_indices.get(slot + 1).copied();
        let end_index = next_prompt_index.unwrap_or(marks.len());
        let body = &marks[prompt_index + 1..end_index];
        let mut output_marks: Vec<_> = body
            .iter()
            .filter_map(|&(row, kind)| {
                matches!(
                    kind,
                    PromptKind::OutputStart | PromptKind::OutputStartAndEndAt { .. }
                )
                .then_some(row)
            })
            .collect();
        if let Some(next) = next_prompt_index
            && let (row, PromptKind::PromptStartAfterOutputEndAt { .. }) = marks[next]
        {
            output_marks.push(row);
        }
        let end_marks: Vec<_> = body
            .iter()
            .filter_map(|&(row, kind)| match kind {
                PromptKind::CommandEnd { exit } => Some((row, 0, exit)),
                PromptKind::CommandEndAt {
                    exit,
                    logical_offset,
                } => Some((
                    row.saturating_add(logical_offset as usize / columns),
                    logical_offset as usize % columns,
                    exit,
                )),
                PromptKind::OutputStartAndEndAt {
                    exit,
                    logical_offset,
                } => Some((
                    row.saturating_add(logical_offset as usize / columns),
                    logical_offset as usize % columns,
                    exit,
                )),
                _ => None,
            })
            .collect();

        let Some(&output_start) = output_marks.first() else {
            continue;
        };
        if output_marks.len() != 1 {
            continue;
        }
        let explicit_end = match (end_marks.as_slice(), next_prompt_index) {
            ([(row, column, exit)], _) => Some((*row, *column, *exit)),
            ([], Some(next)) => match marks[next] {
                (row, PromptKind::PromptStartAfterEnd { prev_exit }) => Some((row, 0, prev_exit)),
                (
                    row,
                    PromptKind::PromptStartAfterEndAt {
                        prev_exit,
                        end_logical_offset,
                    },
                ) => Some((
                    row.saturating_add(end_logical_offset as usize / columns),
                    end_logical_offset as usize % columns,
                    prev_exit,
                )),
                (
                    row,
                    PromptKind::PromptStartAfterOutputEndAt {
                        prev_exit,
                        end_logical_offset,
                    },
                ) => Some((
                    row.saturating_add(end_logical_offset as usize / columns),
                    end_logical_offset as usize % columns,
                    prev_exit,
                )),
                _ => None,
            },
            _ => None,
        };
        let Some((end_boundary_row, end_boundary_column, exit)) = explicit_end else {
            continue;
        };
        let end_is_after_output = output_start < end_boundary_row
            || (output_start == end_boundary_row && end_boundary_column > 0);
        if prompt_row > output_start || !end_is_after_output {
            continue;
        }
        if let Some(next) = next_prompt_index
            && end_boundary_row > prompt_point(marks[next], columns).0
        {
            continue;
        }
        let (output_end, output_end_column) = if end_boundary_column == 0 {
            (end_boundary_row - 1, None)
        } else {
            (end_boundary_row, Some(end_boundary_column - 1))
        };
        if output_end > last_row {
            continue;
        }
        ranges.push(VerifiedCommandRange {
            prompt_row,
            prompt_column,
            output_start,
            output_end,
            output_end_column,
            exit,
        });
    }
    ranges
}

/// Mint opaque handles for the complete ranges in one live-buffer generation.
pub fn verified_command_handles(
    marks: &[(usize, PromptKind)],
    generation: u64,
    columns: usize,
    last_row: usize,
) -> Vec<CommandRangeHandle> {
    verified_command_ranges(marks, columns, last_row)
        .into_iter()
        .map(|range| CommandRangeHandle {
            generation,
            prompt_row: range.prompt_row,
            prompt_column: range.prompt_column,
            output_start: range.output_start,
            output_end: range.output_end,
            output_end_column: range.output_end_column,
            exit: range.exit,
        })
        .collect()
}

/// Resolve a handle against the current live buffer, failing closed on any
/// generation or boundary change.
pub fn resolve_verified_command_handle(
    handle: CommandRangeHandle,
    marks: &[(usize, PromptKind)],
    generation: u64,
    columns: usize,
    last_row: usize,
) -> Option<VerifiedCommandRange> {
    if handle.generation != generation {
        return None;
    }
    verified_command_ranges(marks, columns, last_row)
        .into_iter()
        .find(|range| {
            range.prompt_row == handle.prompt_row
                && range.prompt_column == handle.prompt_column
                && range.output_start == handle.output_start
                && range.output_end == handle.output_end
                && range.output_end_column == handle.output_end_column
                && range.exit == handle.exit
        })
}

/// Choose a generation-bound handle from a whole-selection row span. An empty
/// selection chooses the most recent complete range; a cross-range selection
/// yields no authority.
pub fn verified_command_handle_for_rows(
    marks: &[(usize, PromptKind)],
    generation: u64,
    selected_rows: Option<(usize, usize)>,
    columns: usize,
    last_row: usize,
) -> Option<CommandRangeHandle> {
    let handles = verified_command_handles(marks, generation, columns, last_row);
    match selected_rows {
        Some((start, end)) => {
            let lo = start.min(end);
            let hi = start.max(end);
            let mut matches = handles
                .into_iter()
                .filter(|handle| lo >= handle.prompt_row && hi <= handle.output_end);
            let handle = matches.next()?;
            matches.next().is_none().then_some(handle)
        }
        None => handles.last().copied(),
    }
}

pub fn verified_command_cell_range(
    range: VerifiedCommandRange,
    part: CommandRangePart,
    columns: usize,
) -> (AbsolutePoint, AbsolutePoint) {
    let start_row = match part {
        CommandRangePart::Output => range.output_start,
        CommandRangePart::PromptAndCommand => range.prompt_row,
    };
    let start_column = match part {
        CommandRangePart::Output => 0,
        CommandRangePart::PromptAndCommand => range.prompt_column,
    };
    (
        AbsolutePoint {
            row: start_row,
            column: start_column,
        },
        AbsolutePoint {
            row: range.output_end,
            column: range
                .output_end_column
                .unwrap_or_else(|| columns.saturating_sub(1)),
        },
    )
}

pub fn verified_command_for_rows(
    ranges: &[VerifiedCommandRange],
    start_row: usize,
    end_row: usize,
) -> Option<VerifiedCommandRange> {
    let lo = start_row.min(end_row);
    let hi = start_row.max(end_row);
    let mut matches = ranges
        .iter()
        .copied()
        .filter(|range| lo >= range.prompt_row && hi <= range.output_end);
    let range = matches.next()?;
    matches.next().is_none().then_some(range)
}

pub fn failed_command_target(
    ranges: &[VerifiedCommandRange],
    reference_row: usize,
    direction: CommandDirection,
) -> Option<usize> {
    ranges
        .iter()
        .filter(|range| range.exit.is_some_and(|exit| exit != 0))
        .map(|range| range.prompt_row)
        .filter(|&row| match direction {
            CommandDirection::Prev => row < reference_row,
            CommandDirection::Next => row > reference_row,
        })
        .reduce(|best, row| match direction {
            CommandDirection::Prev => best.max(row),
            CommandDirection::Next => best.min(row),
        })
}

fn is_prompt(kind: PromptKind) -> bool {
    matches!(
        kind,
        PromptKind::PromptStart
            | PromptKind::PromptStartAfterEnd { .. }
            | PromptKind::PromptStartAfterEndAt { .. }
            | PromptKind::PromptStartAfterOutputEndAt { .. }
    )
}

fn prompt_point(mark: (usize, PromptKind), columns: usize) -> (usize, usize) {
    match mark {
        (
            row,
            PromptKind::PromptStartAfterEndAt {
                end_logical_offset, ..
            },
        ) => (
            row.saturating_add(end_logical_offset as usize / columns),
            end_logical_offset as usize % columns,
        ),
        (
            row,
            PromptKind::PromptStartAfterOutputEndAt {
                end_logical_offset, ..
            },
        ) => (
            row.saturating_add(end_logical_offset as usize / columns),
            end_logical_offset as usize % columns,
        ),
        (row, _) => (row, 0),
    }
}

/// The output region of a [`CommandBlock`], derived from the marks that bound
/// it. Absolute-row coordinates (row `0` = oldest scrollback), matching
/// [`crate::core::screen::Screen::prompt_marks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutput {
    /// The command produced no addressable output region: either no
    /// [`PromptKind::OutputStart`] (`C`) mark exists in the block (the prompt is
    /// awaiting input), or the command finished without printing — in which case
    /// the `C` and `D` marks collide on one row and `D` wins, leaving the block
    /// with a [`PromptKind::CommandEnd`] but no `OutputStart`. Nothing to select.
    Empty,
    /// Output spans these inclusive absolute rows, bounded above by the first
    /// mark following the `OutputStart` (the block's own `CommandEnd`, or — when
    /// no `D` arrived — the next prompt).
    Rows { start: usize, end: usize },
    /// Output began at `start` but no following mark bounds it yet: the command
    /// is still running, or this is the last block in the buffer. The consumer
    /// clamps the end to the live buffer's last row.
    Open { start: usize },
}

/// A single shell command, derived from the ordered OSC 133 mark list produced
/// by [`crate::core::screen::Screen::prompt_marks`]. This tolerant presentation
/// model serves the existing success/fail gutter and compatibility helpers.
/// User-authorized select/copy/search/export actions instead require
/// [`VerifiedCommandRange`] so partial metadata never becomes authority.
///
/// All rows are absolute (row `0` = oldest scrollback). The derivation tolerates
/// partial / malformed transcripts without panicking, mirroring the parser's
/// defensive posture: a prompt with no `C` is awaiting input
/// ([`CommandOutput::Empty`], no `exit`); a `C` with no `D` is still running
/// ([`CommandOutput::Open`], `exit: None`); a stray `D`/`C` before the first
/// prompt belongs to no block and is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandBlock {
    /// Absolute row of the [`PromptKind::PromptStart`] (`A`/`B`) opening the
    /// block — the prompt and typed command line.
    pub prompt_row: usize,
    /// Absolute row of the [`PromptKind::OutputStart`] (`C`), once the command
    /// has executed; `None` while the prompt is awaiting input.
    pub output_start: Option<usize>,
    /// The output region (see [`CommandOutput`]).
    pub output: CommandOutput,
    /// Exit status from the block's closing [`PromptKind::CommandEnd`] (`D`):
    /// `Some(0)` = success, `Some(n)` = failure, `None` = still running or the
    /// shell reported no numeric code.
    pub exit: Option<i32>,
}

/// Derive the ordered list of [`CommandBlock`]s from the ascending
/// `(absolute_row, kind)` mark list returned by
/// [`crate::core::screen::Screen::prompt_marks`].
///
/// A block opens at each [`PromptKind::PromptStart`] and runs until the next
/// `PromptStart` (or the end of the marks). Within that span the first
/// [`PromptKind::OutputStart`] begins the output region and the first
/// [`PromptKind::CommandEnd`] supplies the exit status.
///
/// **Exit-association nuance:** the shell emits `D` immediately *before* the
/// next prompt, so a block's `CommandEnd` lands on a row below its output but
/// still above the next `PromptStart`. Because the block span is
/// `[PromptStart, next PromptStart)`, that `D` falls inside this block — its
/// exit is associated with the *preceding* output, never the following prompt.
///
/// The function is pure and never panics on any mark sequence (out-of-order,
/// missing, or duplicate marks degrade gracefully).
pub fn command_blocks(marks: &[(usize, PromptKind)]) -> Vec<CommandBlock> {
    // Indices of the prompt marks — the block delimiters. A merged
    // PromptStartAfterEnd row is a prompt for delimiting purposes; its
    // displaced exit is consumed by the PREVIOUS block below.
    let prompt_indices: Vec<usize> = marks
        .iter()
        .enumerate()
        .filter(|(_, (_, kind))| {
            matches!(
                kind,
                PromptKind::PromptStart
                    | PromptKind::PromptStartAfterEnd { .. }
                    | PromptKind::PromptStartAfterEndAt { .. }
                    | PromptKind::PromptStartAfterOutputEndAt { .. }
            )
        })
        .map(|(index, _)| index)
        .collect();

    let mut blocks = Vec::with_capacity(prompt_indices.len());
    for (slot, &start_index) in prompt_indices.iter().enumerate() {
        let prompt_row = marks[start_index].0;
        // This block owns the marks in (start_index, end_index): everything
        // between this prompt and the next prompt (or the end of the list).
        let end_index = prompt_indices.get(slot + 1).copied().unwrap_or(marks.len());
        let next_prompt_row = prompt_indices.get(slot + 1).map(|&index| marks[index].0);

        let mut output_start = None;
        let mut command_end_row = None;
        let mut exit = None;
        for &(row, kind) in &marks[start_index + 1..end_index] {
            match kind {
                PromptKind::OutputStart if output_start.is_none() => output_start = Some(row),
                PromptKind::CommandEnd { exit: code } if command_end_row.is_none() => {
                    command_end_row = Some(row);
                    exit = code;
                }
                PromptKind::CommandEndAt {
                    exit: code,
                    logical_offset: _,
                } if command_end_row.is_none() => {
                    command_end_row = Some(row);
                    exit = code;
                }
                PromptKind::OutputStartAndEndAt {
                    exit: code,
                    logical_offset: _,
                } => {
                    if output_start.is_none() {
                        output_start = Some(row);
                    }
                    if command_end_row.is_none() {
                        command_end_row = Some(row);
                        exit = code;
                    }
                }
                _ => {}
            }
        }
        // A next prompt that merged over this block's `D` (the universal
        // same-row `D`+`A` shape) carries the displaced exit: treat it as a
        // virtual CommandEnd on the next prompt's row. It bounds the output
        // exactly where the next prompt row already did, so output regions
        // are unchanged. A real interior `D` (first-wins, as ever) takes
        // precedence over the displaced one.
        if command_end_row.is_none()
            && let Some(&next_index) = prompt_indices.get(slot + 1)
            && let (row, PromptKind::PromptStartAfterEnd { prev_exit }) = marks[next_index]
        {
            command_end_row = Some(row);
            exit = prev_exit;
        }
        if let Some(&next_index) = prompt_indices.get(slot + 1)
            && let (
                row,
                PromptKind::PromptStartAfterOutputEndAt {
                    prev_exit,
                    end_logical_offset: _,
                },
            ) = marks[next_index]
        {
            if output_start.is_none() {
                output_start = Some(row);
            }
            if command_end_row.is_none() {
                command_end_row = Some(row);
                exit = prev_exit;
            }
        }
        if command_end_row.is_none()
            && let Some(&next_index) = prompt_indices.get(slot + 1)
            && let (
                row,
                PromptKind::PromptStartAfterEndAt {
                    prev_exit,
                    end_logical_offset,
                },
            ) = marks[next_index]
        {
            // The tolerant row-only block model has no width parameter. Keep
            // the logical-line row as its coarse boundary; verified action
            // ranges use the offset-aware path above.
            let _ = end_logical_offset;
            command_end_row = Some(row);
            exit = prev_exit;
        }

        let output = match output_start {
            None => CommandOutput::Empty,
            Some(start) => {
                // Bound the output by the first mark after it. The CommandEnd
                // (`D`) bounds it most tightly when present; otherwise the next
                // prompt does. The `D` row itself is the "finished" marker, not
                // output, so output ends at `bound - 1`.
                let bound = match (command_end_row, next_prompt_row) {
                    (Some(end), Some(next)) => Some(end.min(next)),
                    (Some(end), None) => Some(end),
                    (None, Some(next)) => Some(next),
                    (None, None) => None,
                };
                match bound {
                    Some(bound) if bound > start => CommandOutput::Rows {
                        start,
                        end: bound - 1,
                    },
                    // Degenerate (a bounding mark at or before the output start):
                    // no addressable output region.
                    Some(_) => CommandOutput::Empty,
                    None => CommandOutput::Open { start },
                }
            }
        };

        blocks.push(CommandBlock {
            prompt_row,
            output_start,
            output,
            exit,
        });
    }
    blocks
}

/// Direction for [`jump_target`]: navigate to the previous (older) or next
/// (newer) prompt relative to a cursor row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpDirection {
    /// Toward older scrollback — the nearest [`PromptKind::PromptStart`] whose
    /// row is strictly *less* than the current row.
    Prev,
    /// Toward newer output — the nearest [`PromptKind::PromptStart`] whose row is
    /// strictly *greater* than the current row.
    Next,
}

/// Return the absolute row of the previous / next [`PromptKind::PromptStart`]
/// relative to `current_row`, or `None` when there is no prompt past the cursor
/// in that direction (a no-op at the buffer's first / last prompt).
///
/// This backs the prompt-jump navigation (Ctrl+Shift+Up/Down): from any cursor
/// row, hop to the nearest prompt boundary. The comparison is **strict**, so a
/// cursor already sitting on a prompt row jumps clear of it rather than sticking.
///
/// Pure and order-independent: it scans for the min-above / max-below
/// `PromptStart` rather than assuming the mark list is sorted, so an
/// out-of-order or partial transcript still yields the correct neighbour and
/// never panics. Non-`PromptStart` marks (`C` / `D`) are not jump targets.
pub fn jump_target(
    marks: &[(usize, PromptKind)],
    current_row: usize,
    direction: JumpDirection,
) -> Option<usize> {
    marks
        .iter()
        .filter(|(_, kind)| {
            matches!(
                kind,
                PromptKind::PromptStart
                    | PromptKind::PromptStartAfterEnd { .. }
                    | PromptKind::PromptStartAfterEndAt { .. }
                    | PromptKind::PromptStartAfterOutputEndAt { .. }
            )
        })
        .map(|&(row, _)| row)
        .filter(|&row| match direction {
            JumpDirection::Prev => row < current_row,
            JumpDirection::Next => row > current_row,
        })
        .reduce(|best, row| match direction {
            // Prev wants the largest row still below the cursor (closest from
            // above); Next wants the smallest row still above it.
            JumpDirection::Prev => best.max(row),
            JumpDirection::Next => best.min(row),
        })
}

/// The inclusive absolute-row range to select for a command's output, or `None`
/// when the block has nothing addressable to select.
///
/// `last_row` is the live buffer's last absolute row, used to clamp an
/// [`CommandOutput::Open`] region (a still-running command, or the final block):
/// the open output runs from its start through the current tail.
///
/// - [`CommandOutput::Rows`] `{ start, end }` → `Some((start, end))` (the bounded
///   span as derived; `end` is clamped to `last_row` defensively).
/// - [`CommandOutput::Open`] `{ start }` → `Some((start, last_row))`, clamped so
///   `end >= start` even if the buffer is somehow shorter than the mark.
/// - [`CommandOutput::Empty`] → `None` (a prompt awaiting input, or a command
///   that printed nothing — there is no output region to select).
///
/// Retained for compatibility and cell-equivalence tests. User-authorized
/// actions use [`verified_command_cell_range`] over a verified range. Pure;
/// never panics regardless of `last_row` relative to the block's rows.
pub fn command_output_range(block: &CommandBlock, last_row: usize) -> Option<(usize, usize)> {
    match block.output {
        CommandOutput::Empty => None,
        CommandOutput::Rows { start, end } => Some((start, end.min(last_row).max(start))),
        CommandOutput::Open { start } => Some((start, last_row.max(start))),
    }
}

/// The inclusive absolute **cell** range to select for a command's output, as an
/// [`AbsolutePoint`] `(start, end)` pair, or `None` when the block has nothing
/// addressable to select.
///
/// Command output is line-oriented, so the range spans whole rows: it begins at
/// the first output row's column `0` and ends at the last output row's last
/// column (`columns - 1`). The verified native action path uses
/// [`verified_command_cell_range`] with the same coordinate convention.
///
/// `last_row` clamps an open (still-running / final) region exactly as in
/// [`command_output_range`], which this builds on. `columns` is the grid width;
/// the inclusive last column is `columns - 1`. The `end` is **inclusive**,
/// matching the [`crate::core::search::SearchMatch`] convention the highlighter
/// already consumes.
///
/// Edge cases (pure; never panics):
/// - [`CommandOutput::Empty`] → `None`.
/// - `columns == 0` (a zero-width grid) → the last column saturates to `0`, so
///   the range degenerates to column `0` on each row rather than underflowing.
pub fn command_output_cell_range(
    block: &CommandBlock,
    last_row: usize,
    columns: usize,
) -> Option<(AbsolutePoint, AbsolutePoint)> {
    let (start_row, end_row) = command_output_range(block, last_row)?;
    let last_column = columns.saturating_sub(1);
    Some((
        AbsolutePoint {
            row: start_row,
            column: 0,
        },
        AbsolutePoint {
            row: end_row,
            column: last_column,
        },
    ))
}

/// The display status of a command block for the success/fail gutter.
///
/// Deliberately conservative: it **never assumes success** from absence. A
/// command only reads [`CommandStatus::Success`] on an explicit `exit 0`; a
/// missing exit degrades to [`CommandStatus::Running`] (output still open) or
/// [`CommandStatus::Unknown`] (no exit was ever recorded — e.g. a shell that
/// emits prompts but no `D`, or a transcript truncated mid-block). The
/// same-row `D`+`A` stamp no longer loses the exit: [`merge_mark`] preserves
/// it as [`PromptKind::PromptStartAfterEnd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    /// The command finished with an explicit exit status of `0`.
    Success,
    /// The command finished with an explicit non-zero exit status.
    Fail,
    /// No exit status yet and the output region is still open — the command is
    /// (or may be) still running.
    Running,
    /// No exit status and no open output: either a prompt awaiting input, or a
    /// finished command whose exit was never reported (no `D` arrived, or the
    /// transcript was truncated mid-block). The gutter must not present this
    /// as success or failure.
    Unknown,
}

/// Map a [`CommandBlock`] to its [`CommandStatus`] for the gutter.
///
/// An explicit exit wins unconditionally (`Some(0)` → success, any other code →
/// failure). Without an exit, an [`CommandOutput::Open`] region reads as
/// [`CommandStatus::Running`]; anything else degrades to
/// [`CommandStatus::Unknown`]. Pure and total.
pub fn command_status(block: &CommandBlock) -> CommandStatus {
    match block.exit {
        Some(0) => CommandStatus::Success,
        Some(_) => CommandStatus::Fail,
        None => match block.output {
            CommandOutput::Open { .. } => CommandStatus::Running,
            CommandOutput::Empty | CommandOutput::Rows { .. } => CommandStatus::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PromptKind = PromptKind::PromptStart;
    const C: PromptKind = PromptKind::OutputStart;

    fn d(exit: Option<i32>) -> PromptKind {
        PromptKind::CommandEnd { exit }
    }

    #[test]
    fn requires_one_ordered_c_and_explicit_d() {
        assert_eq!(
            verified_command_ranges(&[(0, A), (1, C), (4, d(Some(0)))], 80, 10),
            vec![VerifiedCommandRange {
                prompt_row: 0,
                prompt_column: 0,
                output_start: 1,
                output_end: 3,
                output_end_column: None,
                exit: Some(0),
            }]
        );
        assert!(verified_command_ranges(&[(0, A), (1, C), (4, A)], 80, 10).is_empty());
        assert!(verified_command_ranges(&[(0, A), (1, C)], 80, 10).is_empty());
        assert!(
            verified_command_ranges(&[(0, A), (1, C), (2, C), (4, d(Some(0)))], 80, 10).is_empty()
        );
        assert!(
            verified_command_ranges(&[(0, A), (1, C), (4, d(Some(0))), (5, d(Some(1)))], 80, 10,)
                .is_empty()
        );
        assert!(verified_command_ranges(&[(0, A), (4, d(Some(0))), (1, C)], 80, 10).is_empty());
    }

    #[test]
    fn merged_end_and_prompt_is_verified() {
        let marks = [
            (0, A),
            (1, C),
            (4, PromptKind::PromptStartAfterEnd { prev_exit: Some(7) }),
        ];
        let ranges = verified_command_ranges(&marks, 80, 10);
        assert_eq!(ranges[0].output_end, 3);
        assert_eq!(ranges[0].exit, Some(7));
    }

    #[test]
    fn cell_parts_and_selection_stay_bounded() {
        let range = VerifiedCommandRange {
            prompt_row: 2,
            prompt_column: 0,
            output_start: 4,
            output_end: 8,
            output_end_column: None,
            exit: Some(0),
        };
        let (start, end) = verified_command_cell_range(range, CommandRangePart::Output, 80);
        assert_eq!((start.row, start.column), (4, 0));
        assert_eq!((end.row, end.column), (8, 79));
        let (start, _) = verified_command_cell_range(range, CommandRangePart::PromptAndCommand, 0);
        assert_eq!(start.row, 2);
        assert_eq!(verified_command_for_rows(&[range], 3, 7), Some(range));
        assert_eq!(verified_command_for_rows(&[range], 1, 7), None);
    }

    #[test]
    fn failed_navigation_ignores_unknown_and_success() {
        let ranges = [
            VerifiedCommandRange {
                prompt_row: 0,
                prompt_column: 0,
                output_start: 1,
                output_end: 2,
                output_end_column: None,
                exit: Some(1),
            },
            VerifiedCommandRange {
                prompt_row: 4,
                prompt_column: 0,
                output_start: 5,
                output_end: 6,
                output_end_column: None,
                exit: Some(0),
            },
            VerifiedCommandRange {
                prompt_row: 8,
                prompt_column: 0,
                output_start: 9,
                output_end: 10,
                output_end_column: None,
                exit: None,
            },
            VerifiedCommandRange {
                prompt_row: 12,
                prompt_column: 0,
                output_start: 13,
                output_end: 14,
                output_end_column: None,
                exit: Some(7),
            },
        ];
        assert_eq!(
            failed_command_target(&ranges, 10, CommandDirection::Prev),
            Some(0)
        );
        assert_eq!(
            failed_command_target(&ranges, 10, CommandDirection::Next),
            Some(12)
        );
        assert_eq!(
            failed_command_target(&ranges, 0, CommandDirection::Prev),
            None
        );
    }

    #[test]
    fn opaque_handle_re_resolves_only_in_the_same_generation() {
        let marks = [(0, A), (1, C), (4, d(Some(0)))];
        let handle = verified_command_handle_for_rows(&marks, 7, None, 80, 10).expect("handle");
        assert!(resolve_verified_command_handle(handle, &marks, 7, 80, 10).is_some());
        assert!(resolve_verified_command_handle(handle, &marks, 8, 80, 10).is_none());
        assert!(resolve_verified_command_handle(handle, &marks[..2], 7, 80, 10).is_none());
    }

    #[test]
    fn offset_end_keeps_the_final_fragment_and_next_prompt_column() {
        let marks = [
            (0, A),
            (1, C),
            (
                2,
                PromptKind::PromptStartAfterEndAt {
                    prev_exit: Some(0),
                    end_logical_offset: 4,
                },
            ),
        ];
        let ranges = verified_command_ranges(&marks, 80, 10);
        assert_eq!(ranges[0].output_end, 2);
        assert_eq!(ranges[0].output_end_column, Some(3));
        let (_, end) = verified_command_cell_range(ranges[0], CommandRangePart::Output, 80);
        assert_eq!((end.row, end.column), (2, 3));
    }

    #[test]
    fn combined_output_end_and_prompt_is_a_verified_soft_wrapped_range() {
        let marks = [
            (0, A),
            (
                1,
                PromptKind::PromptStartAfterOutputEndAt {
                    prev_exit: Some(0),
                    end_logical_offset: 100,
                },
            ),
        ];
        assert_eq!(
            verified_command_ranges(&marks, 80, 10),
            vec![VerifiedCommandRange {
                prompt_row: 0,
                prompt_column: 0,
                output_start: 1,
                output_end: 2,
                output_end_column: Some(19),
                exit: Some(0),
            }]
        );

        let completed_without_next_prompt = [
            (0, A),
            (
                1,
                PromptKind::OutputStartAndEndAt {
                    exit: Some(0),
                    logical_offset: 100,
                },
            ),
        ];
        assert_eq!(
            verified_command_ranges(&completed_without_next_prompt, 80, 10),
            verified_command_ranges(&marks, 80, 10)
        );
    }

    #[test]
    fn shared_row_selection_fails_closed_instead_of_guessing_a_command() {
        let marks = [
            (0, A),
            (1, C),
            (
                2,
                PromptKind::PromptStartAfterEndAt {
                    prev_exit: Some(0),
                    end_logical_offset: 4,
                },
            ),
            (3, C),
            (5, d(Some(0))),
        ];
        let ranges = verified_command_ranges(&marks, 80, 10);
        assert_eq!(ranges.len(), 2);
        assert!(verified_command_handle_for_rows(&marks, 7, Some((2, 2)), 80, 10).is_none());
        assert!(verified_command_for_rows(&ranges, 2, 2).is_none());
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! OSC 133 semantic prompt marking (SH1): the per-row mark model and the pure
//! parse of an OSC 133 payload into a [`PromptKind`].
//!
//! OSC 133 (the "FinalTerm" / shell-integration protocol) lets a cooperating
//! shell tell the terminal where each prompt, command, and command output begin
//! and where a command finished (with its exit status). OdyTTY stores the
//! reported boundary as an advisory mark on the physical row the cursor sat on
//! when the sequence arrived; the mark is anchored to the *logical* line so it
//! survives scroll-out into scrollback and re-wrapping at a new width (it always
//! rides the first physical row of the re-wrapped logical line).
//!
//! This is **inert foundation state** (SH1): the marks are captured and made
//! queryable through the [`super::screen::Screen`] poll API, but nothing on the
//! render path reads them and they never reach the [`super::types::Snapshot`].
//! The command-aware UX that consumes them lands later (SH2/SH-CLICK).
//!
//! Parsing here is pure and defensive: any malformed or unrecognized payload
//! yields `None` (the caller leaves the row's existing mark untouched) and no
//! input byte sequence can panic — mirroring the OSC 7 parse policy.

/// A semantic boundary reported by the shell via OSC 133, anchored to a single
/// physical row. Small and `Copy` so it rides on every [`super::screen::Line`]
/// and [`super::scrollback::LogicalLine`] for free.
///
/// Sub-command mapping (OSC 133 letter → kind):
/// - `A` (prompt start) and `B` (command/input start) → [`PromptKind::PromptStart`].
///   Both sit on the prompt row; OdyTTY's row-anchored model keeps a single
///   "prompt region" boundary rather than distinguishing the prompt text from
///   the typed command on the same line. A dedicated command-input boundary can
///   be added later if SH2 needs the A/B split.
/// - `C` (command executed / output start) → [`PromptKind::OutputStart`].
/// - `D` (command finished) → [`PromptKind::CommandEnd`] with the optional exit
///   status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// The row begins a shell prompt / command input (OSC 133 `A` or `B`).
    PromptStart,
    /// The row begins command output (OSC 133 `C`).
    OutputStart,
    /// The command finished (OSC 133 `D`); `exit` is its status when the shell
    /// reported a numeric code (absent / non-numeric → `None`).
    CommandEnd { exit: Option<i32> },
}

/// Parse an OSC 133 payload — the `;`-split parts *after* the leading `133` — into
/// a [`PromptKind`]. Returns `None` for an empty or unrecognized sub-command so
/// the caller leaves the current row's mark untouched. Never panics on any byte
/// sequence.
pub(in crate::core) fn parse_osc133(parts: &[&[u8]]) -> Option<PromptKind> {
    let letter = parts.first().and_then(|p| p.first()).copied()?;
    match letter {
        b'A' | b'B' => Some(PromptKind::PromptStart),
        b'C' => Some(PromptKind::OutputStart),
        b'D' => Some(PromptKind::CommandEnd {
            exit: parts.get(1).and_then(|p| parse_exit_code(p)),
        }),
        _ => None,
    }
}

/// Parse a command exit status: ASCII decimal digits only. Empty, signed, or
/// otherwise non-numeric payloads (e.g. a `aid=7` key/value part) yield `None`,
/// as do values that overflow `i32`. Defensive — mirrors the OSC 7 parse policy
/// (no panic on any input).
fn parse_exit_code(part: &[u8]) -> Option<i32> {
    if part.is_empty() {
        return None;
    }
    let mut value: i32 = 0;
    for &byte in part {
        let digit = match byte {
            b'0'..=b'9' => i32::from(byte - b'0'),
            _ => return None,
        };
        value = value.checked_mul(10)?.checked_add(digit)?;
    }
    Some(value)
}

/// The output region of a [`CommandBlock`], derived from the marks that bound
/// it. Absolute-row coordinates (row `0` = oldest scrollback), matching
/// [`super::screen::Screen::prompt_marks`].
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
/// by [`super::screen::Screen::prompt_marks`]. This is the shared substance the
/// command-aware UX (jump-to-prompt, command-output select/copy, the
/// success/fail gutter) and OSC 133 click-to-position all consume; deriving it
/// once in core keeps one coordinate convention and one set of edge-case rules.
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
/// [`super::screen::Screen::prompt_marks`].
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
    // Indices of the PromptStart marks — the block delimiters.
    let prompt_indices: Vec<usize> = marks
        .iter()
        .enumerate()
        .filter(|(_, (_, kind))| matches!(kind, PromptKind::PromptStart))
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
                _ => {}
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_subcommand() {
        assert_eq!(parse_osc133(&[b"A"]), Some(PromptKind::PromptStart));
        assert_eq!(parse_osc133(&[b"B"]), Some(PromptKind::PromptStart));
        assert_eq!(parse_osc133(&[b"C"]), Some(PromptKind::OutputStart));
        assert_eq!(
            parse_osc133(&[b"D"]),
            Some(PromptKind::CommandEnd { exit: None })
        );
    }

    #[test]
    fn parses_exit_code() {
        assert_eq!(
            parse_osc133(&[b"D", b"0"]),
            Some(PromptKind::CommandEnd { exit: Some(0) })
        );
        assert_eq!(
            parse_osc133(&[b"D", b"130"]),
            Some(PromptKind::CommandEnd { exit: Some(130) })
        );
    }

    #[test]
    fn malformed_exit_code_is_none() {
        // Non-numeric, signed, key/value, and empty exit parts all yield None
        // without changing the variant or panicking.
        assert_eq!(
            parse_osc133(&[b"D", b"xx"]),
            Some(PromptKind::CommandEnd { exit: None })
        );
        assert_eq!(
            parse_osc133(&[b"D", b"-1"]),
            Some(PromptKind::CommandEnd { exit: None })
        );
        assert_eq!(
            parse_osc133(&[b"D", b"aid=7"]),
            Some(PromptKind::CommandEnd { exit: None })
        );
        assert_eq!(
            parse_osc133(&[b"D", b""]),
            Some(PromptKind::CommandEnd { exit: None })
        );
    }

    #[test]
    fn exit_code_overflow_is_none() {
        assert_eq!(
            parse_osc133(&[b"D", b"99999999999999999999"]),
            Some(PromptKind::CommandEnd { exit: None })
        );
    }

    #[test]
    fn unknown_or_empty_is_none() {
        assert_eq!(parse_osc133(&[b"Z"]), None);
        assert_eq!(parse_osc133(&[b""]), None);
        assert_eq!(parse_osc133(&[]), None);
        // Extra parts after a known letter are ignored, not an error.
        assert_eq!(
            parse_osc133(&[b"A", b"aid=1"]),
            Some(PromptKind::PromptStart)
        );
    }

    // --- CommandBlock derivation (SH2 core) ---

    const A: PromptKind = PromptKind::PromptStart;
    const C: PromptKind = PromptKind::OutputStart;
    fn d(exit: Option<i32>) -> PromptKind {
        PromptKind::CommandEnd { exit }
    }

    #[test]
    fn no_marks_yield_no_blocks() {
        assert_eq!(command_blocks(&[]), Vec::new());
    }

    #[test]
    fn well_formed_transcript_derives_block() {
        // A@0 (prompt+input) → C@1 (output start) → D@3 exit 0 → A@4 (next prompt).
        // The first block: prompt row 0, output rows [1, 2] (bounded by D@3 − 1),
        // exit 0. The trailing A@4 opens a second, still-running block.
        let marks = [(0, A), (1, C), (3, d(Some(0))), (4, A)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks.len(), 2);

        assert_eq!(blocks[0].prompt_row, 0);
        assert_eq!(blocks[0].output_start, Some(1));
        assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 2 });
        assert_eq!(blocks[0].exit, Some(0));

        // The second block has only its prompt so far: awaiting input.
        assert_eq!(blocks[1].prompt_row, 4);
        assert_eq!(blocks[1].output_start, None);
        assert_eq!(blocks[1].output, CommandOutput::Empty);
        assert_eq!(blocks[1].exit, None);
    }

    #[test]
    fn output_bounded_by_next_prompt_when_no_command_end() {
        // A@0 → C@1 → A@5: no D, but the next prompt bounds the output at [1, 4].
        let marks = [(0, A), (1, C), (5, A)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 4 });
        assert_eq!(blocks[0].exit, None);
    }

    #[test]
    fn command_end_without_output_start_is_empty_with_exit() {
        // A command that printed nothing collides C and D on one row; D wins, so
        // the block carries a CommandEnd but no OutputStart. Output is Empty, the
        // exit is still captured.
        let marks = [(0, A), (1, d(Some(2))), (2, A)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].output_start, None);
        assert_eq!(blocks[0].output, CommandOutput::Empty);
        assert_eq!(blocks[0].exit, Some(2));
    }

    #[test]
    fn running_command_has_open_output_and_no_exit() {
        // A@0 → C@1 with no following mark: the command is still running. Output
        // is open from row 1; the consumer clamps the end to the buffer.
        let marks = [(0, A), (1, C)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].output, CommandOutput::Open { start: 1 });
        assert_eq!(blocks[0].exit, None);
    }

    #[test]
    fn prompt_awaiting_input_has_empty_output() {
        // A lone prompt (A with no C): awaiting input, no output region, no exit.
        let marks = [(0, A)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].prompt_row, 0);
        assert_eq!(blocks[0].output_start, None);
        assert_eq!(blocks[0].output, CommandOutput::Empty);
        assert_eq!(blocks[0].exit, None);
    }

    #[test]
    fn exit_associates_with_preceding_block_not_following_prompt() {
        // D is emitted just before the NEXT prompt, so its row sits below the
        // first command's output but above the second prompt. The exit must bind
        // to the FIRST (preceding) block, never the second.
        let marks = [
            (0, A),
            (1, C),
            (4, d(Some(7))), // closes the first command
            (5, A),          // next prompt, immediately after D
            (6, C),
        ];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks.len(), 2);
        // First block owns the exit and output [1, 3] (bounded by D@4 − 1).
        assert_eq!(blocks[0].exit, Some(7));
        assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 3 });
        // Second block is the new running command — no exit leaked from D@4.
        assert_eq!(blocks[1].exit, None);
        assert_eq!(blocks[1].output, CommandOutput::Open { start: 6 });
    }

    #[test]
    fn failure_exit_is_preserved() {
        let marks = [(0, A), (1, C), (2, d(Some(130))), (3, A)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].exit, Some(130));
    }

    #[test]
    fn stray_marks_before_first_prompt_are_ignored() {
        // A D and C with no preceding prompt belong to no block.
        let marks = [(0, d(Some(1))), (1, C), (2, A), (3, C)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].prompt_row, 2);
        assert_eq!(blocks[0].output, CommandOutput::Open { start: 3 });
    }

    #[test]
    fn single_line_output_is_one_row() {
        // C@1 immediately followed by D@2: a one-row output region [1, 1].
        let marks = [(0, A), (1, C), (2, d(Some(0)))];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 1 });
    }

    #[test]
    fn duplicate_marks_do_not_panic_and_take_the_first() {
        // Defensive: duplicate OutputStart / CommandEnd rows take the first of
        // each and never panic.
        let marks = [(0, A), (1, C), (2, C), (3, d(Some(0))), (4, d(Some(9)))];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].output_start, Some(1));
        assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 2 });
        assert_eq!(blocks[0].exit, Some(0));
    }
}

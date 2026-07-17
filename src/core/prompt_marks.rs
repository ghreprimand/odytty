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

use super::search::AbsolutePoint;

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

/// The OSC 133 click-to-position directive carried on a prompt-start payload as
/// a `click_events=N` key/value attribute (SH-CLICK).
///
/// A cooperating shell announces, on each prompt, whether its line editor will
/// accept click-to-position: `click_events=1` enables it, `click_events=0`
/// withdraws it. OdyTTY tracks the latest explicit directive as terminal state
/// (default off); the native pointer layer reads that state and, on a click
/// within the live input region (modeled by
/// [`InputRegion`](super::input_region::InputRegion)), synthesizes the
/// cursor-key presses that move the shell cursor (F2).
///
/// **Charter boundary:** this is the click-to-position slice only. Core parses
/// the enable/disable and models the input geometry; it never takes over shell
/// input, never does multi-cursor or undo, and never writes to the host on its
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickEvents {
    /// `click_events=1`: the shell's line editor accepts click-to-position.
    Enable,
    /// `click_events=0`: the shell withdrew click-to-position support.
    Disable,
}

/// Scan an OSC 133 payload — the `;`-split parts *after* the leading `133` — for
/// a `click_events=N` attribute on a prompt-start (`A`/`B`), returning the
/// directive when present and well-formed (SH-CLICK).
///
/// Returns `None` (leave the current click-events state untouched) when:
/// - the payload is not a prompt-start (`C` / `D` / unknown letters carry no
///   click-events directive);
/// - no `click_events` key is present; or
/// - the value is anything other than the exact `1` / `0` (an empty, multi-byte,
///   or non-binary value is dropped, not an error).
///
/// Pure and defensive: any byte sequence yields a directive or `None` and never
/// panics, mirroring [`parse_osc133`]. Unknown / oversized sibling attributes
/// (e.g. `aid=7`) are simply skipped.
pub(in crate::core) fn parse_click_events(parts: &[&[u8]]) -> Option<ClickEvents> {
    // Only a prompt-start (A/B) carries the click-events directive.
    let letter = parts.first().and_then(|p| p.first()).copied()?;
    if !matches!(letter, b'A' | b'B') {
        return None;
    }
    // Find the `click_events=<val>` attribute among the remaining parts.
    for part in &parts[1..] {
        let Some(equals) = part.iter().position(|&b| b == b'=') else {
            continue;
        };
        let (key, value) = part.split_at(equals);
        if key != b"click_events" {
            continue;
        }
        // `value` still carries the leading `=`; the bytes after it are the value.
        return match &value[1..] {
            b"1" => Some(ClickEvents::Enable),
            b"0" => Some(ClickEvents::Disable),
            // Empty / multi-byte / non-binary values are dropped (unchanged).
            _ => None,
        };
    }
    None
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
        .filter(|(_, kind)| matches!(kind, PromptKind::PromptStart))
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
/// Backs command-output select/copy (select-only by default). Pure; never
/// panics regardless of `last_row` relative to the block's rows.
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
/// Command output is line-oriented, so the selection spans whole rows: it begins
/// at the first output row's column `0` and ends at the last output row's last
/// column (`columns - 1`). This is the authoritative span — the native
/// select/copy layer highlights `start..=end` directly and expands nothing, so
/// the coordinate convention lives in exactly one place (consistent with
/// [`jump_target`] / [`prompt_jump`]).
///
/// `last_row` clamps an open (still-running / final) region exactly as in
/// [`command_output_range`], which this builds on. `columns` is the grid width;
/// the inclusive last column is `columns - 1`. The `end` is **inclusive**,
/// matching the [`super::search::SearchMatch`] convention the highlighter
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
/// [`CommandStatus::Unknown`] (the exit was lost — e.g. the SH1 row-collapse
/// where `D` and the next prompt's `A` land on one row and the exit is dropped).
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
    /// finished command whose exit was lost to the SH1 row collapse. The gutter
    /// must not present this as success or failure.
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

/// Where a revealed target row should sit within the viewport when a jump or
/// reveal scrolls it into view.
///
/// SH2 prompt-jump uses [`Align::Top`] (D-SH2-2): a jump lands the prompt at the
/// top of the viewport so its command output reads downward below it. The other
/// placements are exposed for callers that want the legacy centered reveal
/// (search uses center) or a bottom-anchored reveal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// The target row sits at the top of the viewport — prompt on top, output
    /// below. The SH2 prompt-jump default.
    Top,
    /// The target row sits at the vertical center of the viewport. Matches the
    /// historical search-reveal behaviour.
    Center,
    /// The target row sits at the bottom of the viewport.
    Bottom,
}

/// Resolve the scrollback **viewport offset** that brings `target_row` into view
/// at the requested [`Align`]. Pure viewport geometry shared by SH2 prompt-jump
/// and any future reveal; the generalization of the native search reveal (which
/// is the [`Align::Center`] case).
///
/// ## Coordinate convention
/// - `target_row` is absolute (row `0` = oldest scrollback), matching
///   [`command_blocks`] / [`jump_target`].
/// - `viewport_height` is the number of visible terminal rows.
/// - `scrollback_len` is the number of rows scrolled out above the live screen;
///   equivalently the absolute row of the live viewport's top.
/// - The returned **offset** is measured up from the live tail: `0` shows the
///   live tail, and larger values scroll further back. The viewport's top
///   absolute row is `scrollback_len - offset`.
///
/// ## Clamping
/// The desired top is clamped to `[0, scrollback_len]`, so the offset is never
/// negative (you cannot scroll below the live tail — a target already on or
/// below the live screen yields offset `0`) and never exceeds `scrollback_len`
/// (you cannot scroll above the oldest row). A `viewport_height` of `0` degrades
/// to a top-anchored reveal. Pure; never panics for any inputs.
pub fn viewport_offset_for_row(
    target_row: usize,
    align: Align,
    viewport_height: usize,
    scrollback_len: usize,
) -> usize {
    let desired_top = match align {
        Align::Top => target_row,
        Align::Center => target_row.saturating_sub(viewport_height / 2),
        Align::Bottom => target_row.saturating_sub(viewport_height.saturating_sub(1)),
    };
    let top = desired_top.min(scrollback_len);
    scrollback_len.saturating_sub(top)
}

/// Resolve a prompt-jump in one call: from `reference_row`, find the prev / next
/// prompt ([`jump_target`]) and the viewport offset that reveals it at `align`
/// ([`viewport_offset_for_row`]). Returns `(target_prompt_row, viewport_offset)`,
/// or `None` when there is no prompt past the reference in that direction.
///
/// This is the thin composition the SH2 native wiring calls: the native layer
/// supplies the current reference row (derived from the live viewport position)
/// and the viewport geometry, and receives both the absolute row to focus and
/// the offset to scroll to — no jump logic in the front end.
///
/// **End behaviour is clamp, no wrap:** at the first prompt a `Prev` jump and at
/// the last prompt a `Next` jump return `None` (a no-op), rather than wrapping to
/// the opposite end. This mirrors [`jump_target`]'s strict-neighbour contract and
/// is asserted in [`prompt_jump_clamps_at_ends_without_wrapping`].
pub fn prompt_jump(
    marks: &[(usize, PromptKind)],
    reference_row: usize,
    direction: JumpDirection,
    align: Align,
    viewport_height: usize,
    scrollback_len: usize,
) -> Option<(usize, usize)> {
    let target = jump_target(marks, reference_row, direction)?;
    let offset = viewport_offset_for_row(target, align, viewport_height, scrollback_len);
    Some((target, offset))
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

    // --- SH-CLICK: click-events parse + click report ---

    #[test]
    fn click_events_enable_and_disable_parse() {
        assert_eq!(
            parse_click_events(&[b"A", b"click_events=1"]),
            Some(ClickEvents::Enable)
        );
        assert_eq!(
            parse_click_events(&[b"A", b"click_events=0"]),
            Some(ClickEvents::Disable)
        );
        // `B` (command-input start) is also a prompt-start and carries it too.
        assert_eq!(
            parse_click_events(&[b"B", b"click_events=1"]),
            Some(ClickEvents::Enable)
        );
    }

    #[test]
    fn click_events_absent_or_plain_prompt_is_none() {
        // A plain prompt with no click_events attribute leaves state untouched.
        assert_eq!(parse_click_events(&[b"A"]), None);
        // Sibling attributes without click_events are skipped, not an error.
        assert_eq!(parse_click_events(&[b"A", b"aid=7"]), None);
    }

    #[test]
    fn click_events_only_on_prompt_start() {
        // C / D / unknown letters carry no click-events directive even if a
        // stray click_events token rides along.
        assert_eq!(parse_click_events(&[b"C", b"click_events=1"]), None);
        assert_eq!(parse_click_events(&[b"D", b"click_events=1"]), None);
        assert_eq!(parse_click_events(&[b"Z", b"click_events=1"]), None);
        assert_eq!(parse_click_events(&[b""]), None);
        assert_eq!(parse_click_events(&[]), None);
    }

    #[test]
    fn click_events_malformed_value_is_dropped() {
        // Empty, multi-byte, and non-binary values yield None (leave unchanged),
        // never panic. Only the exact `1` / `0` are recognized.
        assert_eq!(parse_click_events(&[b"A", b"click_events="]), None);
        assert_eq!(parse_click_events(&[b"A", b"click_events=2"]), None);
        assert_eq!(parse_click_events(&[b"A", b"click_events=10"]), None);
        assert_eq!(parse_click_events(&[b"A", b"click_events=yes"]), None);
        // A key that merely starts with the name must not match.
        assert_eq!(parse_click_events(&[b"A", b"click_events_extra=1"]), None);
    }

    #[test]
    fn click_events_finds_attribute_among_siblings() {
        // The directive is found regardless of its position among other attrs.
        assert_eq!(
            parse_click_events(&[b"A", b"aid=7", b"click_events=1", b"k=v"]),
            Some(ClickEvents::Enable)
        );
        // The first click_events attribute wins.
        assert_eq!(
            parse_click_events(&[b"A", b"click_events=1", b"click_events=0"]),
            Some(ClickEvents::Enable)
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
    fn doubled_fish_style_marks_derive_one_coherent_block() {
        // D-c (fish >=4.0 duplicate-marks hazard): with shell integration on,
        // fish emits its own OSC 133 A/B/C/D natively AND the OdyTTY snippet
        // emits them, so each mark can arrive twice per prompt (possibly a
        // DIVERGENT duplicate `D`). The decision is TOLERATE.
        //
        // The primary defense is at the screen layer: both emitters fire during
        // the same prompt render, so their marks anchor to the same physical
        // row, and the row stores one mark per row (last-writer-wins) -- a
        // same-row A/A, C/C or D/D collapses to a single mark before it ever
        // reaches `prompt_marks()`. This test pins the cross-row BACKSTOP: even
        // in the pessimistic case where a duplicate lands on an adjacent row and
        // a second `D` reports a different exit, `command_blocks` takes the first
        // OutputStart/CommandEnd within the block, so the derivation stays
        // deterministic and the divergent duplicate never corrupts the exit.
        let marks = [
            (0, A),          // snippet prompt-start
            (1, A),          // fish native prompt-start (adjacent, pessimistic)
            (2, C),          // snippet output-start
            (3, C),          // fish native output-start (adjacent)
            (5, d(Some(0))), // snippet command-end: the real exit
            (6, d(Some(1))), // divergent native duplicate: MUST be ignored
            (7, A),          // next prompt
        ];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks.len(), 3);
        // The command block spans from the second prompt-start (row 1) to the
        // next prompt (row 7): first C wins the output start, first D wins the
        // exit -- the divergent D@6 exit 1 is dropped.
        assert_eq!(blocks[1].output_start, Some(2));
        assert_eq!(blocks[1].output, CommandOutput::Rows { start: 2, end: 4 });
        assert_eq!(blocks[1].exit, Some(0));
        // Status is derived from that single, deterministic exit.
        assert_eq!(command_status(&blocks[1]), CommandStatus::Success);
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

    // --- jump_target (SH2 prompt navigation) ---

    #[test]
    fn jump_target_empty_marks_is_none() {
        assert_eq!(jump_target(&[], 5, JumpDirection::Prev), None);
        assert_eq!(jump_target(&[], 5, JumpDirection::Next), None);
    }

    #[test]
    fn jump_prev_finds_nearest_prompt_below() {
        // Prompts at rows 0, 4, 10. From row 7, Prev → 4 (largest below 7).
        let marks = [(0, A), (1, C), (4, A), (5, C), (10, A)];
        assert_eq!(jump_target(&marks, 7, JumpDirection::Prev), Some(4));
    }

    #[test]
    fn jump_next_finds_nearest_prompt_above() {
        // From row 5, Next → 10 (smallest above 5).
        let marks = [(0, A), (1, C), (4, A), (5, C), (10, A)];
        assert_eq!(jump_target(&marks, 5, JumpDirection::Next), Some(10));
    }

    #[test]
    fn jump_is_strict_so_a_cursor_on_a_prompt_hops_clear() {
        // Cursor exactly on the prompt at row 4: Prev jumps to 0, Next to 10 —
        // never sticks on the row it started on.
        let marks = [(0, A), (4, A), (10, A)];
        assert_eq!(jump_target(&marks, 4, JumpDirection::Prev), Some(0));
        assert_eq!(jump_target(&marks, 4, JumpDirection::Next), Some(10));
    }

    #[test]
    fn jump_returns_none_at_the_boundaries() {
        // Below the first prompt there is no Prev; above the last there is no Next.
        let marks = [(2, A), (6, A)];
        assert_eq!(jump_target(&marks, 2, JumpDirection::Prev), None);
        assert_eq!(jump_target(&marks, 6, JumpDirection::Next), None);
        // Before any prompt: no Prev, Next is the first prompt.
        assert_eq!(jump_target(&marks, 0, JumpDirection::Prev), None);
        assert_eq!(jump_target(&marks, 0, JumpDirection::Next), Some(2));
    }

    #[test]
    fn jump_ignores_non_prompt_marks() {
        // Only PromptStart rows are targets; C / D rows are skipped.
        let marks = [(0, A), (3, C), (4, d(Some(0)))];
        // From row 5 there is no prompt above (3 and 4 are C/D, not prompts).
        assert_eq!(jump_target(&marks, 5, JumpDirection::Next), None);
        assert_eq!(jump_target(&marks, 5, JumpDirection::Prev), Some(0));
    }

    #[test]
    fn jump_is_order_independent() {
        // Unsorted marks still yield the correct nearest neighbour.
        let marks = [(10, A), (0, A), (4, A)];
        assert_eq!(jump_target(&marks, 7, JumpDirection::Prev), Some(4));
        assert_eq!(jump_target(&marks, 7, JumpDirection::Next), Some(10));
    }

    // --- command_output_range (SH2 select/copy) ---

    #[test]
    fn output_range_rows_returns_the_span() {
        let block = CommandBlock {
            prompt_row: 0,
            output_start: Some(1),
            output: CommandOutput::Rows { start: 1, end: 4 },
            exit: Some(0),
        };
        // last_row well past the span: the bounded Rows span is returned as-is.
        assert_eq!(command_output_range(&block, 99), Some((1, 4)));
    }

    #[test]
    fn output_range_open_clamps_to_last_row() {
        let block = CommandBlock {
            prompt_row: 0,
            output_start: Some(2),
            output: CommandOutput::Open { start: 2 },
            exit: None,
        };
        // Open output runs from its start through the live tail.
        assert_eq!(command_output_range(&block, 8), Some((2, 8)));
    }

    #[test]
    fn output_range_empty_is_none() {
        let block = CommandBlock {
            prompt_row: 0,
            output_start: None,
            output: CommandOutput::Empty,
            exit: None,
        };
        assert_eq!(command_output_range(&block, 8), None);
    }

    #[test]
    fn output_range_clamps_never_inverts_when_buffer_is_short() {
        // Defensive: a last_row below the output start must not produce an
        // inverted (end < start) range. Both shapes clamp end up to start.
        let open = CommandBlock {
            prompt_row: 0,
            output_start: Some(5),
            output: CommandOutput::Open { start: 5 },
            exit: None,
        };
        assert_eq!(command_output_range(&open, 2), Some((5, 5)));

        let rows = CommandBlock {
            prompt_row: 0,
            output_start: Some(5),
            output: CommandOutput::Rows { start: 5, end: 9 },
            exit: None,
        };
        // end clamps down to last_row, but never below start.
        assert_eq!(command_output_range(&rows, 7), Some((5, 7)));
        assert_eq!(command_output_range(&rows, 3), Some((5, 5)));
    }

    // --- command_output_cell_range (SH2 select/copy, Option (b)) ---

    fn point(row: usize, column: usize) -> AbsolutePoint {
        AbsolutePoint { row, column }
    }

    #[test]
    fn cell_range_spans_full_width_rows() {
        // A multi-row output [1, 4] over an 80-column grid selects from
        // (1, 0) through the last cell of row 4, (4, 79) inclusive.
        let block = CommandBlock {
            prompt_row: 0,
            output_start: Some(1),
            output: CommandOutput::Rows { start: 1, end: 4 },
            exit: Some(0),
        };
        assert_eq!(
            command_output_cell_range(&block, 99, 80),
            Some((point(1, 0), point(4, 79)))
        );
    }

    #[test]
    fn cell_range_open_clamps_to_last_row() {
        // Open output's end row clamps to last_row (mirrors command_output_range),
        // then spans the full width of that row.
        let block = CommandBlock {
            prompt_row: 0,
            output_start: Some(2),
            output: CommandOutput::Open { start: 2 },
            exit: None,
        };
        assert_eq!(
            command_output_cell_range(&block, 8, 80),
            Some((point(2, 0), point(8, 79)))
        );
    }

    #[test]
    fn cell_range_single_row_output() {
        // A one-row output is (r, 0)..=(r, columns-1).
        let block = CommandBlock {
            prompt_row: 0,
            output_start: Some(3),
            output: CommandOutput::Rows { start: 3, end: 3 },
            exit: Some(0),
        };
        assert_eq!(
            command_output_cell_range(&block, 99, 40),
            Some((point(3, 0), point(3, 39)))
        );
    }

    #[test]
    fn cell_range_empty_output_is_none() {
        let block = CommandBlock {
            prompt_row: 0,
            output_start: None,
            output: CommandOutput::Empty,
            exit: None,
        };
        assert_eq!(command_output_cell_range(&block, 8, 80), None);
    }

    #[test]
    fn cell_range_zero_width_grid_degenerates_to_column_zero() {
        // Defensive: columns == 0 must not underflow; the last column saturates
        // to 0, so each row's span collapses to column 0.
        let block = CommandBlock {
            prompt_row: 0,
            output_start: Some(1),
            output: CommandOutput::Rows { start: 1, end: 2 },
            exit: Some(0),
        };
        assert_eq!(
            command_output_cell_range(&block, 99, 0),
            Some((point(1, 0), point(2, 0)))
        );
        // A one-column grid: last column is 0 as well.
        assert_eq!(
            command_output_cell_range(&block, 99, 1),
            Some((point(1, 0), point(2, 0)))
        );
    }

    // --- command_status (SH2 gutter) ---

    fn block_with(output: CommandOutput, exit: Option<i32>) -> CommandBlock {
        // command_status reads only `output` and `exit`; output_start is
        // irrelevant here, so leave it None.
        CommandBlock {
            prompt_row: 0,
            output_start: None,
            output,
            exit,
        }
    }

    #[test]
    fn status_success_only_on_explicit_zero() {
        assert_eq!(
            command_status(&block_with(
                CommandOutput::Rows { start: 1, end: 2 },
                Some(0)
            )),
            CommandStatus::Success
        );
    }

    #[test]
    fn status_fail_on_any_nonzero_exit() {
        assert_eq!(
            command_status(&block_with(
                CommandOutput::Rows { start: 1, end: 2 },
                Some(1)
            )),
            CommandStatus::Fail
        );
        assert_eq!(
            command_status(&block_with(CommandOutput::Empty, Some(130))),
            CommandStatus::Fail
        );
    }

    #[test]
    fn status_running_when_output_open_and_no_exit() {
        assert_eq!(
            command_status(&block_with(CommandOutput::Open { start: 1 }, None)),
            CommandStatus::Running
        );
    }

    #[test]
    fn status_unknown_when_exit_lost_to_row_collapse() {
        // Output bounded (Rows) but no exit: the D was lost to the SH1 row
        // collapse. Must read Unknown, never Success.
        assert_eq!(
            command_status(&block_with(CommandOutput::Rows { start: 1, end: 2 }, None)),
            CommandStatus::Unknown
        );
    }

    #[test]
    fn status_unknown_for_prompt_awaiting_input() {
        // A bare prompt (Empty output, no exit) is neither running nor finished.
        assert_eq!(
            command_status(&block_with(CommandOutput::Empty, None)),
            CommandStatus::Unknown
        );
    }

    #[test]
    fn status_explicit_exit_wins_over_open_output() {
        // Defensive: an explicit exit takes precedence even if the output were
        // somehow still Open — never report Running over a real exit code.
        assert_eq!(
            command_status(&block_with(CommandOutput::Open { start: 1 }, Some(0))),
            CommandStatus::Success
        );
        assert_eq!(
            command_status(&block_with(CommandOutput::Open { start: 1 }, Some(3))),
            CommandStatus::Fail
        );
    }

    // --- viewport_offset_for_row (SH2 reveal geometry) ---

    #[test]
    fn align_top_puts_target_at_viewport_top() {
        // scrollback 100, height 24. Target row 50, Top → top row is 50, so the
        // offset up from the live tail is scrollback_len - 50 = 50.
        assert_eq!(viewport_offset_for_row(50, Align::Top, 24, 100), 50);
    }

    #[test]
    fn align_center_matches_the_legacy_search_reveal() {
        // Center reproduces the old native viewport_offset_for_match: the top is
        // target - height/2 = 50 - 12 = 38, offset = 100 - 38 = 62.
        assert_eq!(viewport_offset_for_row(50, Align::Center, 24, 100), 62);
    }

    #[test]
    fn align_bottom_puts_target_at_viewport_bottom() {
        // Bottom: top = target - (height - 1) = 50 - 23 = 27, offset = 100 - 27 = 73.
        assert_eq!(viewport_offset_for_row(50, Align::Bottom, 24, 100), 73);
    }

    #[test]
    fn offset_clamps_to_live_tail_for_targets_on_or_below_the_live_screen() {
        // A target at/below the live viewport top can't scroll the view below the
        // live tail: the offset clamps to 0.
        assert_eq!(viewport_offset_for_row(100, Align::Top, 24, 100), 0);
        assert_eq!(viewport_offset_for_row(150, Align::Top, 24, 100), 0);
        // Near the tail, Top still reveals it a little above the tail.
        assert_eq!(viewport_offset_for_row(98, Align::Top, 24, 100), 2);
    }

    #[test]
    fn offset_clamps_to_oldest_row_and_never_underflows() {
        // Row 0 with a tall reveal can't scroll past the oldest row: the offset
        // saturates at scrollback_len, never underflowing.
        assert_eq!(viewport_offset_for_row(0, Align::Center, 24, 100), 100);
        assert_eq!(viewport_offset_for_row(0, Align::Bottom, 24, 100), 100);
        // Empty scrollback degrades to offset 0 for any align/height.
        assert_eq!(viewport_offset_for_row(0, Align::Top, 0, 0), 0);
        assert_eq!(viewport_offset_for_row(5, Align::Center, 24, 0), 0);
    }

    #[test]
    fn zero_height_degrades_to_a_top_anchored_reveal() {
        // height 0: Center/Bottom subtract nothing, so all three aligns coincide
        // with Top — no panic on the division/subtraction.
        assert_eq!(viewport_offset_for_row(50, Align::Center, 0, 100), 50);
        assert_eq!(viewport_offset_for_row(50, Align::Bottom, 0, 100), 50);
        assert_eq!(viewport_offset_for_row(50, Align::Top, 0, 100), 50);
    }

    // --- prompt_jump (SH2 native-thin composition) ---

    #[test]
    fn prompt_jump_composes_target_and_offset() {
        // Prompts at 0, 4, 10; scrollback 100, height 24. From row 7, Prev → the
        // prompt at row 4, revealed Top → offset 96.
        let marks = [(0, A), (1, C), (4, A), (5, C), (10, A)];
        assert_eq!(
            prompt_jump(&marks, 7, JumpDirection::Prev, Align::Top, 24, 100),
            Some((4, 96))
        );
        // Next from row 5 → the prompt at row 10, Top → offset 90.
        assert_eq!(
            prompt_jump(&marks, 5, JumpDirection::Next, Align::Top, 24, 100),
            Some((10, 90))
        );
    }

    #[test]
    fn prompt_jump_clamps_at_ends_without_wrapping() {
        // Clamp, no wrap: Prev at the first prompt and Next at the last prompt
        // return None (a no-op), never wrapping to the opposite end.
        let marks = [(2, A), (6, A)];
        assert_eq!(
            prompt_jump(&marks, 2, JumpDirection::Prev, Align::Top, 24, 100),
            None
        );
        assert_eq!(
            prompt_jump(&marks, 6, JumpDirection::Next, Align::Top, 24, 100),
            None
        );
    }

    #[test]
    fn prompt_jump_honours_the_requested_align() {
        // The same jump under different aligns yields the same target row but
        // distinct offsets — confirming the align is threaded through.
        let marks = [(0, A), (50, A)];
        let top = prompt_jump(&marks, 10, JumpDirection::Next, Align::Top, 24, 100);
        let center = prompt_jump(&marks, 10, JumpDirection::Next, Align::Center, 24, 100);
        assert_eq!(top, Some((50, 50)));
        assert_eq!(center, Some((50, 62)));
    }
}

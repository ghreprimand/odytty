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
//! The marks remain render-neutral: they never enter the cell
//! [`super::types::Snapshot`] or change terminal bytes. Shipped consumers read
//! them through the [`super::screen::Screen`] API to jump between prompts,
//! derive command/output ranges and status gutters, and support prompt-aware
//! click/edit behavior. Versioned terminal snapshot envelopes serialize the
//! marks separately so an attached or restored terminal retains its semantic
//! boundaries without turning the grid into a block document.
//!
//! Parsing here is pure and defensive: any malformed or unrecognized payload
//! yields `None` (the caller leaves the row's existing mark untouched) and no
//! input byte sequence can panic — mirroring the OSC 7 parse policy.

#[cfg(test)]
use super::search::AbsolutePoint;

mod command_ranges;

pub use command_ranges::{
    CommandBlock, CommandDirection, CommandOutput, CommandRangeHandle, CommandRangePart,
    CommandStatus, JumpDirection, VerifiedCommandRange, command_blocks, command_output_cell_range,
    command_output_range, command_status, failed_command_target, jump_target,
    resolve_verified_command_handle, verified_command_cell_range, verified_command_for_rows,
    verified_command_handle_for_rows, verified_command_handles, verified_command_ranges,
};

/// A semantic boundary reported by the shell via OSC 133, anchored to a single
/// physical row. Small and `Copy` so it rides on every [`super::screen::Line`]
/// and [`super::scrollback::LogicalLine`] for free.
///
/// Sub-command mapping (OSC 133 letter → kind):
/// - `A` (prompt start) and `B` (command/input start) → [`PromptKind::PromptStart`].
///   Both sit on the prompt row; OdyTTY's row-anchored model keeps a single
///   "prompt region" boundary for block derivation rather than distinguishing
///   the prompt text from typed input in this enum. The screen separately tracks
///   the OSC 133 `B` row and column for the shipped [`InputRegion`](super::input_region::InputRegion).
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
    /// A stamped command end with its cell offset inside the logical line.
    ///
    /// The parser produces [`PromptKind::CommandEnd`]; the screen enriches it
    /// at stamp time. Keeping the offset with the logical-line mark lets
    /// command-output actions retain a final unterminated output fragment
    /// across reflow without selecting the following prompt.
    CommandEndAt {
        exit: Option<i32>,
        logical_offset: u32,
    },
    /// The row begins a shell prompt AND carried the previous command's end
    /// before the prompt was stamped. Real shells emit `D` (command finished)
    /// and the next prompt's `A` back to back in the same hook with no
    /// intervening newline, so both land on one physical row; the row-anchored
    /// model holds one mark per row, and a plain last-write-wins stamp would
    /// destroy the exit status. This variant is produced by [`merge_mark`] at
    /// stamp time: the prompt wins the row, and the displaced exit rides along
    /// as `prev_exit` so [`command_blocks`] can attribute it to the block the
    /// `D` was closing. For every prompt-shaped question (jump targets, block
    /// delimiting) this row IS a [`PromptKind::PromptStart`].
    PromptStartAfterEnd { prev_exit: Option<i32> },
    /// A same-line command end followed by the next prompt, retaining the
    /// command-end offset as well as its exit status.
    PromptStartAfterEndAt {
        prev_exit: Option<i32>,
        end_logical_offset: u32,
    },
    /// An output start and command end that share one logical line.
    ///
    /// A long unterminated output line can soft-wrap before `D` arrives. Both
    /// `C` and the offset-bearing `D` are anchored to the logical line's first
    /// physical row, so this composite preserves both explicit boundaries.
    OutputStartAndEndAt {
        exit: Option<i32>,
        logical_offset: u32,
    },
    /// A next prompt sharing the logical line represented by
    /// [`PromptKind::OutputStartAndEndAt`]. The variant proves that `C`, `D`,
    /// and the following `A` all arrived; verified actions need not infer a
    /// missing output-start boundary from displayed text.
    PromptStartAfterOutputEndAt {
        prev_exit: Option<i32>,
        end_logical_offset: u32,
    },
}

/// Merge a freshly parsed OSC 133 mark into a row's existing mark (SH1 stamps
/// one mark per physical row).
///
/// The default is last-write-wins, matching the row-anchored model's original
/// behavior. The exception is a prompt landing on a row that already carries a
/// command's exit status: real shells emit `133;D` then `133;A` consecutively
/// in the same prompt hook with no newline between, so the next prompt ALWAYS
/// lands on the finished command's `D` row. Overwriting would destroy the exit
/// (the success/fail gutter could then never show a verdict once the next
/// prompt appeared); instead the row becomes
/// [`PromptKind::PromptStartAfterEnd`] carrying the displaced exit.
///
/// A prompt stamped onto an existing merged mark preserves the displaced exit:
/// shells emit `A` (prompt start) and `B` (command start) as separate
/// sequences on the same row, and the second stamp must not drop what the
/// first preserved.
///
/// An offset-bearing `D` over `C` also preserves both boundaries: a
/// soft-wrapped unterminated output line anchors both marks to the same logical
/// row even though it contains real visible output. A zero-offset collision is
/// still the no-output collapse. The remaining combinations retain
/// last-write-wins behavior for malformed or partial transcripts. Pure and
/// total.
pub(in crate::core) fn merge_mark(existing: Option<PromptKind>, new: PromptKind) -> PromptKind {
    match (existing, new) {
        (Some(PromptKind::CommandEnd { exit }), PromptKind::PromptStart) => {
            PromptKind::PromptStartAfterEnd { prev_exit: exit }
        }
        (
            Some(PromptKind::CommandEndAt {
                exit,
                logical_offset,
            }),
            PromptKind::PromptStart,
        ) => PromptKind::PromptStartAfterEndAt {
            prev_exit: exit,
            end_logical_offset: logical_offset,
        },
        (Some(PromptKind::PromptStartAfterEnd { prev_exit }), PromptKind::PromptStart) => {
            PromptKind::PromptStartAfterEnd { prev_exit }
        }
        (
            Some(PromptKind::PromptStartAfterEndAt {
                prev_exit,
                end_logical_offset,
            }),
            PromptKind::PromptStart,
        ) => PromptKind::PromptStartAfterEndAt {
            prev_exit,
            end_logical_offset,
        },
        (
            Some(PromptKind::OutputStart),
            PromptKind::CommandEndAt {
                exit,
                logical_offset: logical_offset @ 1..,
            },
        ) => PromptKind::OutputStartAndEndAt {
            exit,
            logical_offset,
        },
        (
            Some(PromptKind::OutputStartAndEndAt {
                exit,
                logical_offset,
            }),
            PromptKind::PromptStart,
        ) => PromptKind::PromptStartAfterOutputEndAt {
            prev_exit: exit,
            end_logical_offset: logical_offset,
        },
        (
            Some(PromptKind::PromptStartAfterOutputEndAt {
                prev_exit,
                end_logical_offset,
            }),
            PromptKind::PromptStart,
        ) => PromptKind::PromptStartAfterOutputEndAt {
            prev_exit,
            end_logical_offset,
        },
        (_, new) => new,
    }
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
    fn merged(prev_exit: Option<i32>) -> PromptKind {
        PromptKind::PromptStartAfterEnd { prev_exit }
    }

    #[test]
    fn no_marks_yield_no_blocks() {
        assert_eq!(command_blocks(&[]), Vec::new());
    }

    // --- merge_mark (same-row stamp collisions) ---

    #[test]
    fn prompt_over_command_end_preserves_the_exit() {
        // The universal shell shape: `D` then the next prompt's `A` land on
        // one row. The prompt wins the row; the exit rides along.
        assert_eq!(merge_mark(Some(d(Some(0))), A), merged(Some(0)));
        assert_eq!(merge_mark(Some(d(Some(127))), A), merged(Some(127)));
        assert_eq!(merge_mark(Some(d(None)), A), merged(None));
    }

    #[test]
    fn second_prompt_stamp_keeps_the_displaced_exit() {
        // Shells emit `A` then `B` on the prompt row (both map to
        // PromptStart); the second stamp must not drop what the first merge
        // preserved.
        assert_eq!(merge_mark(Some(merged(Some(2))), A), merged(Some(2)));
    }

    #[test]
    fn offset_end_over_output_start_preserves_both_explicit_boundaries() {
        let end = PromptKind::CommandEndAt {
            exit: Some(0),
            logical_offset: 100,
        };
        let combined = PromptKind::OutputStartAndEndAt {
            exit: Some(0),
            logical_offset: 100,
        };
        assert_eq!(merge_mark(Some(C), end), combined);
        assert_eq!(
            merge_mark(Some(combined), A),
            PromptKind::PromptStartAfterOutputEndAt {
                prev_exit: Some(0),
                end_logical_offset: 100,
            }
        );
    }

    #[test]
    fn all_other_stamp_collisions_stay_last_write_wins() {
        // Empty row: plain stamp.
        assert_eq!(merge_mark(None, A), A);
        assert_eq!(merge_mark(None, d(Some(0))), d(Some(0)));
        // C then a zero-offset D on one row: the no-output collapse, D wins.
        assert_eq!(merge_mark(Some(C), d(Some(1))), d(Some(1)));
        // D then C, duplicate D, prompt then C/D: freshest mark wins,
        // matching the original row-anchored semantics.
        assert_eq!(merge_mark(Some(d(Some(1))), C), C);
        assert_eq!(merge_mark(Some(d(Some(1))), d(Some(2))), d(Some(2)));
        assert_eq!(merge_mark(Some(A), C), C);
        assert_eq!(merge_mark(Some(A), d(Some(3))), d(Some(3)));
        assert_eq!(merge_mark(Some(merged(Some(1))), C), C);
        assert_eq!(merge_mark(Some(merged(Some(1))), d(Some(4))), d(Some(4)));
    }

    // --- command_blocks with merged prompt rows ---

    #[test]
    fn merged_next_prompt_supplies_the_previous_blocks_exit() {
        // A@0 → C@1 → merged A/D@3: the displaced exit closes block 0 and the
        // output is bounded exactly as a next-prompt row always bounded it.
        let marks = [(0, A), (1, C), (3, merged(Some(0)))];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].prompt_row, 0);
        assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 2 });
        assert_eq!(blocks[0].exit, Some(0));
        // The merged row also opens the next block, awaiting input.
        assert_eq!(blocks[1].prompt_row, 3);
        assert_eq!(blocks[1].output, CommandOutput::Empty);
        assert_eq!(blocks[1].exit, None);
    }

    #[test]
    fn merged_prompt_failure_exit_reaches_the_previous_block() {
        let marks = [(0, A), (1, C), (4, merged(Some(127)))];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].exit, Some(127));
        assert_eq!(command_status(&blocks[0]), CommandStatus::Fail);
    }

    #[test]
    fn interior_command_end_wins_over_a_displaced_exit() {
        // A real interior D (first-wins, as ever) takes precedence over the
        // next prompt's displaced exit — the displaced one belongs to a
        // duplicate/malformed D in that case.
        let marks = [(0, A), (1, C), (3, d(Some(0))), (5, merged(Some(9)))];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks[0].exit, Some(0));
        assert_eq!(blocks[0].output, CommandOutput::Rows { start: 1, end: 2 });
    }

    #[test]
    fn leading_merged_prompt_ignores_its_displaced_exit() {
        // The first mark being a merged prompt means the block that exit
        // belonged to was trimmed away; nothing to attach it to.
        let marks = [(2, merged(Some(1))), (3, C)];
        let blocks = command_blocks(&marks);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].prompt_row, 2);
        assert_eq!(blocks[0].exit, None);
    }

    #[test]
    fn merged_prompt_rows_are_jump_targets() {
        let marks = [(0, A), (1, C), (5, merged(Some(0)))];
        assert_eq!(jump_target(&marks, 3, JumpDirection::Next), Some(5));
        assert_eq!(jump_target(&marks, 5, JumpDirection::Prev), Some(0));
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

    // --- command_output_range (legacy tolerant helper) ---

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

    // --- command_output_cell_range (legacy tolerant helper) ---

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

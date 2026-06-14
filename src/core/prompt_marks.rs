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
}

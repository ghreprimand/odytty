//! DECSCUSR cursor-style + blink-policy behavior for the terminal core
//! (`CSI Ps SP q`): per-style selection, host-default policy, and the
//! RIS/DECSTR reset semantics. Kept in a sibling file so `tests.rs` stays under
//! the modularity ceiling.

use super::*;

/// DECSCUSR styles 1–6 select the documented shape + blink pairing: odd values
/// blink, even values are steady; 1/2 block, 3/4 underline, 5/6 bar.
#[test]
fn decscusr_selects_style_and_blink_per_value() {
    let mut terminal = Terminal::new(8, 2);
    // Power-on default: blinking block.
    assert_eq!(terminal.cursor_style(), CursorStyle::Block);
    assert!(terminal.cursor_blinking());

    terminal.advance(b"\x1b[2 q"); // steady block
    assert_eq!(terminal.cursor_style(), CursorStyle::Block);
    assert!(!terminal.cursor_blinking());

    terminal.advance(b"\x1b[3 q"); // blinking underline
    assert_eq!(terminal.cursor_style(), CursorStyle::Underline);
    assert!(terminal.cursor_blinking());

    terminal.advance(b"\x1b[4 q"); // steady underline
    assert_eq!(terminal.cursor_style(), CursorStyle::Underline);
    assert!(!terminal.cursor_blinking());

    terminal.advance(b"\x1b[5 q"); // blinking bar
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);
    assert!(terminal.cursor_blinking());

    terminal.advance(b"\x1b[6 q"); // steady bar
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);
    assert!(!terminal.cursor_blinking());

    terminal.advance(b"\x1b[1 q"); // blinking block
    assert_eq!(terminal.cursor_style(), CursorStyle::Block);
    assert!(terminal.cursor_blinking());
}

/// DECSCUSR 0 (and an omitted parameter, which defaults to 0) resets to the
/// host default policy rather than a hardcoded shape.
#[test]
fn decscusr_zero_resets_to_host_default() {
    let mut terminal = Terminal::new(8, 2);
    terminal.set_cursor_defaults(CursorStyle::Bar, false);
    // Defaults applied immediately as the effective cursor.
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);
    assert!(!terminal.cursor_blinking());

    // An app override, then DECSCUSR 0 returns to the configured default.
    terminal.advance(b"\x1b[1 q"); // blinking block
    assert_eq!(terminal.cursor_style(), CursorStyle::Block);
    assert!(terminal.cursor_blinking());
    terminal.advance(b"\x1b[0 q");
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);
    assert!(!terminal.cursor_blinking());

    // An omitted parameter is treated as 0.
    terminal.advance(b"\x1b[2 q"); // steady block override
    terminal.advance(b"\x1b[ q"); // bare DECSCUSR == 0
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);
    assert!(!terminal.cursor_blinking());
}

/// Unknown DECSCUSR parameters are ignored, leaving the cursor unchanged.
#[test]
fn decscusr_unknown_value_is_ignored() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"\x1b[3 q"); // blinking underline
    terminal.advance(b"\x1b[9 q"); // out of range -> no change
    assert_eq!(terminal.cursor_style(), CursorStyle::Underline);
    assert!(terminal.cursor_blinking());
}

/// A `q` final without the space intermediate is not DECSCUSR and must not
/// touch the cursor style.
#[test]
fn plain_q_without_intermediate_is_not_decscusr() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"\x1b[2 q"); // steady block
    terminal.advance(b"\x1b[1q"); // no SP intermediate -> ignored
    assert_eq!(terminal.cursor_style(), CursorStyle::Block);
    assert!(!terminal.cursor_blinking());
}

/// RIS restores the cursor shape/blink to the host default policy.
#[test]
fn ris_restores_default_cursor_policy() {
    let mut terminal = Terminal::new(8, 2);
    terminal.set_cursor_defaults(CursorStyle::Underline, false);
    terminal.advance(b"\x1b[5 q"); // blinking bar override
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);

    terminal.advance(b"\x1bc"); // RIS
    assert_eq!(terminal.cursor_style(), CursorStyle::Underline);
    assert!(!terminal.cursor_blinking());
}

/// DECSTR (soft reset) also returns the cursor to the host default policy.
#[test]
fn decstr_restores_default_cursor_policy() {
    let mut terminal = Terminal::new(8, 2);
    terminal.set_cursor_defaults(CursorStyle::Block, true);
    terminal.advance(b"\x1b[6 q"); // steady bar override
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);
    assert!(!terminal.cursor_blinking());

    terminal.advance(b"\x1b[!p"); // DECSTR
    assert_eq!(terminal.cursor_style(), CursorStyle::Block);
    assert!(terminal.cursor_blinking());
}

/// `set_cursor_defaults` applies immediately when no application override has
/// occurred, so a configured default shows from power-on.
#[test]
fn set_cursor_defaults_applies_immediately() {
    let mut terminal = Terminal::new(8, 2);
    terminal.set_cursor_defaults(CursorStyle::Bar, false);
    let snapshot = terminal.snapshot();
    // Snapshot still carries only visibility; the style/blink are read via the
    // dedicated accessors so the renderer can pick the shape.
    assert!(snapshot.cursor_visible);
    assert_eq!(terminal.cursor_style(), CursorStyle::Bar);
    assert!(!terminal.cursor_blinking());
}

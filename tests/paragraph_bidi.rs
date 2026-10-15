// SPDX-License-Identifier: GPL-3.0-only
//! Paragraph bidi model state (terminal-wg BiDi recommendation): BDSM
//! (`CSI 8 h/l`), SCP (`CSI Ps SP k`), the DECRQM report, the stamping rules,
//! and the value's survival through wrap joins, reflow, scrollback, the
//! alternate screen, and snapshots. Project-authored control streams only.

use odytty::core::{
    ParagraphBidi, ParagraphDirection, SnapshotCaptureLimits, SnapshotEnvelope,
    SnapshotEnvelopeCaps, Terminal,
};

const RTL: &[u8] = b"\x1b[2 k";
const LTR: &[u8] = b"\x1b[1 k";
const DEFAULT: &[u8] = b"\x1b[0 k";

fn feed(terminal: &mut Terminal, parts: &[&[u8]]) {
    for part in parts {
        terminal.advance(part);
    }
}

fn direction(terminal: &Terminal, row: usize) -> ParagraphDirection {
    terminal.screen().paragraph_bidi_at(row).direction()
}

fn rtl() -> ParagraphBidi {
    ParagraphBidi::DEFAULT.with_direction(ParagraphDirection::RightToLeft)
}

fn report(terminal: &mut Terminal) -> String {
    terminal.take_host_output();
    terminal.advance(b"\x1b[8$p");
    String::from_utf8(terminal.take_host_output()).unwrap()
}

#[test]
fn decrqm_ansi_8_reports_implicit() {
    let mut terminal = Terminal::new(10, 3);
    assert_eq!(report(&mut terminal), "\x1b[8;1$y");
}

#[test]
fn bdsm_reset_reports_explicit() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"\x1b[8l");
    assert_eq!(report(&mut terminal), "\x1b[8;2$y");
    assert!(terminal.screen().paragraph_bidi_current().explicit());
    assert!(terminal.screen().paragraph_bidi_at(0).explicit());
    terminal.advance(b"\x1b[8h");
    assert_eq!(report(&mut terminal), "\x1b[8;1$y");
}

#[test]
fn scp_rtl_stamps_paragraph_at_its_start() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(RTL);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::Default);
    // Only the direction changes on the stamped paragraph; its mode stays.
    terminal.advance(b"\x1b[8l\r\n\x1b[8h");
    assert!(!terminal.screen().paragraph_bidi_at(1).explicit());
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
    // Reinforcing the current value still stamps a paragraph at its start.
    feed(&mut terminal, &[b"\x1b[3;1H", LTR, b"\x1b[1;1H", LTR]);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::LeftToRight);
}

#[test]
fn scp_mid_paragraph_waits_for_next_paragraph() {
    let mut terminal = Terminal::new(4, 4);
    feed(&mut terminal, &[b"ab", RTL]);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::Default);
    assert_eq!(
        terminal.screen().paragraph_bidi_current().direction(),
        ParagraphDirection::RightToLeft
    );
    // Column 0 of a continuation row is not a paragraph's first position.
    feed(&mut terminal, &[DEFAULT, b"\x1b[3;1Hwxyzv\r", RTL]);
    assert_eq!(direction(&terminal, 2), ParagraphDirection::Default);
    assert_eq!(direction(&terminal, 3), ParagraphDirection::Default);
    // A line feed into a new paragraph applies the current value.
    feed(&mut terminal, &[b"\x1b[1;3H\r\n"]);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::Default);
}

#[test]
fn ed2_stamps_every_screen_row_with_current_properties() {
    let mut terminal = Terminal::new(6, 4);
    feed(&mut terminal, &[b"a\r\nb\r\nc\r\nd\x1b[2;2H", RTL]);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::Default);
    terminal.advance(b"\x1b[2J");
    for row in 0..4 {
        assert_eq!(direction(&terminal, row), ParagraphDirection::RightToLeft);
    }
    // ED 0 stamps the rows below the cursor, not the cursor's own row.
    feed(&mut terminal, &[LTR, b"\x1b[0J"]);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 2), ParagraphDirection::LeftToRight);
    assert_eq!(direction(&terminal, 3), ParagraphDirection::LeftToRight);
    // ED 1 stamps the rows above it.
    feed(&mut terminal, &[DEFAULT, b"\x1b[1J"]);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::Default);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
}

#[test]
fn scrolled_in_rows_take_the_current_properties() {
    let mut terminal = Terminal::new(6, 3);
    feed(&mut terminal, &[b"\x1b[2;2H", RTL, b"\x1b[S"]);
    assert_eq!(direction(&terminal, 2), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::Default);
    feed(&mut terminal, &[DEFAULT, b"\x1b[T"]);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::Default);
    assert_eq!(direction(&terminal, 2), ParagraphDirection::Default);
}

#[test]
fn wrap_join_keeps_first_paragraph_properties() {
    let mut terminal = Terminal::new(4, 3);
    feed(&mut terminal, &[b"\x1b[2;1H", RTL, b"\x1b[3;2H", DEFAULT]);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
    // Text from row 0 wraps into row 1, joining the two paragraphs.
    terminal.advance(b"\x1b[1;1Habcde");
    assert_eq!(direction(&terminal, 1), ParagraphDirection::Default);
    // The other way round: an RTL first paragraph keeps RTL on the join.
    feed(
        &mut terminal,
        &[b"\x1b[2J\x1b[1;1H", RTL, b"\x1b[2;1H", LTR],
    );
    assert_eq!(direction(&terminal, 1), ParagraphDirection::LeftToRight);
    terminal.advance(b"\x1b[1;1Hfghij");
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
    // An erase that splits the paragraph leaves both halves with its value.
    terminal.advance(b"\x1b[1;2H\x1b[K");
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
}

#[test]
fn paragraph_properties_survive_reflow_and_scrollback() {
    let mut terminal = Terminal::new(4, 3);
    feed(&mut terminal, &[RTL, b"abcdef", DEFAULT, b"\r\nxy"]);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 2), ParagraphDirection::Default);
    // Wider: the RTL paragraph rejoins onto one row.
    terminal.resize(8, 3);
    assert_eq!(direction(&terminal, 0), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::Default);
    // Narrower again, then scroll the paragraph into scrollback.
    terminal.resize(3, 3);
    terminal.advance(b"\r\n\r\n\r\n\r\n");
    let history: Vec<ParagraphDirection> = terminal
        .visible_search_rows(usize::MAX)
        .iter()
        .map(|row| row.paragraph_bidi.direction())
        .collect();
    assert_eq!(
        &history[..2],
        &[
            ParagraphDirection::RightToLeft,
            ParagraphDirection::RightToLeft
        ]
    );
    assert_eq!(history[2], ParagraphDirection::Default);
    // Re-wrapping history at another width keeps the value on every row.
    terminal.resize(2, 3);
    let rows = terminal.visible_search_rows(usize::MAX);
    assert!(rows.len() >= 3);
    assert!(rows[..3].iter().all(|row| row.paragraph_bidi == rtl()));
}

#[test]
fn wrap_into_a_scrolled_row_joins_the_scrollback_paragraph() {
    let mut terminal = Terminal::new(3, 1);
    terminal.set_scrollback_limit(16);
    feed(&mut terminal, &[RTL, b"abc", DEFAULT, b"d"]);
    // The continuation row on screen belongs to the RTL paragraph in history.
    assert_eq!(direction(&terminal, 0), ParagraphDirection::RightToLeft);
    terminal.advance(b"\r\ne");
    assert_eq!(direction(&terminal, 0), ParagraphDirection::Default);
}

#[test]
fn alternate_screen_leaves_primary_properties_untouched() {
    let mut terminal = Terminal::new(6, 3);
    feed(&mut terminal, &[RTL, b"x\r\n", DEFAULT]);
    terminal.advance(b"\x1b[?1049h");
    assert_eq!(direction(&terminal, 0), ParagraphDirection::Default);
    feed(&mut terminal, &[LTR, b"\x1b[2J"]);
    assert_eq!(direction(&terminal, 2), ParagraphDirection::LeftToRight);
    terminal.advance(b"\x1b[?1049l");
    assert_eq!(direction(&terminal, 0), ParagraphDirection::RightToLeft);
    assert_eq!(direction(&terminal, 1), ParagraphDirection::Default);
}

fn round_trip(terminal: &Terminal) -> (SnapshotEnvelope, Vec<u8>, Terminal) {
    let envelope = SnapshotEnvelope::from_terminal(terminal, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().unwrap();
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
    assert_eq!(decoded, envelope);
    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
    (envelope, bytes, restored)
}

fn section_count(bytes: &[u8]) -> u16 {
    // Magic (15), format and protocol versions (2 each), then the producer
    // version as a u16 length and bytes, then the section count.
    let at = 15 + 4;
    let len = u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let count = at + 2 + len;
    u16::from_le_bytes([bytes[count], bytes[count + 1]])
}

#[test]
fn paragraph_properties_roundtrip_snapshot_envelope() {
    let mut terminal = Terminal::new(4, 3);
    terminal.set_scrollback_limit(16);
    // A snapshot without bidi state carries no paragraph bidi section.
    let (plain, plain_bytes, _) = round_trip(&terminal);
    assert!(plain.paragraph_bidi.is_default());
    assert_eq!(section_count(&plain_bytes), 5);

    feed(
        &mut terminal,
        &[RTL, b"abcdef\r\n", DEFAULT, b"g\r\nh\r\ni\r\n", b"\x1b[8l"],
    );
    let (envelope, bytes, restored) = round_trip(&terminal);
    assert_eq!(section_count(&bytes), 6);
    assert!(envelope.paragraph_bidi.current.explicit());
    assert_eq!(
        restored.screen().paragraph_bidi_current(),
        terminal.screen().paragraph_bidi_current()
    );
    for offset in [0, usize::MAX] {
        let before: Vec<ParagraphBidi> = terminal
            .visible_search_rows(offset)
            .iter()
            .map(|row| row.paragraph_bidi)
            .collect();
        let after: Vec<ParagraphBidi> = restored
            .visible_search_rows(offset)
            .iter()
            .map(|row| row.paragraph_bidi)
            .collect();
        assert_eq!(before, after, "offset {offset}");
    }
    assert_eq!(
        restored.screen().paragraph_bidi_at(0),
        terminal.screen().paragraph_bidi_at(0)
    );
}

#[test]
fn snapshot_paragraph_bidi_rows_must_cover_the_captured_rows() {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance(RTL);
    let mut envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    envelope.paragraph_bidi.rows.push(ParagraphBidi::DEFAULT);
    assert!(envelope.encode().is_err());
    assert!(Terminal::from_snapshot_envelope(&envelope).is_err());
}

#[test]
fn ris_and_decstr_restore_implicit_default() {
    let mut terminal = Terminal::new(6, 3);
    feed(&mut terminal, &[b"\x1b[8l", RTL]);
    terminal.advance(b"\x1b[!p");
    assert_eq!(
        terminal.screen().paragraph_bidi_current(),
        ParagraphBidi::DEFAULT
    );
    assert_eq!(report(&mut terminal), "\x1b[8;1$y");
    feed(&mut terminal, &[b"\x1b[8l", RTL, b"\x1bc"]);
    assert_eq!(
        terminal.screen().paragraph_bidi_current(),
        ParagraphBidi::DEFAULT
    );
    assert_eq!(
        terminal.screen().paragraph_bidi_at(0),
        ParagraphBidi::DEFAULT
    );
}

#[test]
fn scp_hostile_parameters_are_ignored_without_panic() {
    let mut terminal = Terminal::new(6, 3);
    for sequence in [
        &b"\x1b[3 k"[..],
        b"\x1b[65535 k",
        b"\x1b[99999999999 k",
        b"\x1b[2;1 k",
        b"\x1b[2;2 k",
        b"\x1b[2;65535 k",
        b"\x1b[?2 k",
        b"\x1b[2 !k",
        b"\x1b[3;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0;0 k",
    ] {
        terminal.advance(sequence);
        assert_eq!(
            terminal.screen().paragraph_bidi_current(),
            ParagraphBidi::DEFAULT,
            "{sequence:?}"
        );
    }
    // A missing first parameter is 0, the terminal default.
    feed(&mut terminal, &[RTL, b"\x1b[ k"]);
    assert_eq!(
        terminal.screen().paragraph_bidi_current(),
        ParagraphBidi::DEFAULT
    );
}

// SPDX-License-Identifier: GPL-3.0-only
//! Transcript-driven coverage for DEC G0/G1 charset designation, SO/SI GL
//! selection, and the Special Graphics translation — the state machine behind
//! terminfo/ncurses ACS line drawing (`smacs`/`rmacs`, `enacs = ESC ( B
//! ESC ) 0`). The full interaction matrix is pinned: designation + shift-in/
//! shift-out text, mid-line GL switching, wrap across charset state, DECSC/
//! DECRC save-restore, RIS and DECSTR resets, alternate-screen isolation,
//! REP replay, snapshot round-trip (including the pre-charset format), wide
//! non-interaction, and unknown-designator fallback (parser totality).

use super::screen::Terminal;
use super::snapshot_envelope::{SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps};
use super::types::CharsetModes;

fn visible_text(terminal: &Terminal) -> String {
    terminal
        .screen()
        .plain_text()
        .trim_end_matches('\n')
        .to_string()
}

fn first_row(terminal: &Terminal) -> String {
    visible_text(terminal)
        .lines()
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

// ─── Designation + SO/SI selection ─────────────────────────────────────────

#[test]
fn g0_graphics_designation_translates_immediately() {
    // ESC ( 0 designates G0 = Special Graphics; GL is already G0 at power-on,
    // so translation starts with no SO needed (the common ncurses
    // `smacs=\E(0` terminfo variant).
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b(0lqqk");
    assert_eq!(first_row(&term), "\u{250C}\u{2500}\u{2500}\u{2510}");
}

#[test]
fn g1_designation_needs_shift_out_and_shift_in_returns_ascii() {
    // enacs=\E(B\E)0 + smacs=^N + rmacs=^O: the classic ncurses ACS setup.
    // G1 carries graphics; SO (0x0E) selects it, SI (0x0F) returns to ASCII
    // G0 mid-line.
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b(B\x1b)0\x0eqqx\x0fab");
    assert_eq!(first_row(&term), "\u{2500}\u{2500}\u{2502}ab");
}

#[test]
fn shift_out_with_ascii_g1_changes_nothing() {
    // SO selects G1, but G1 is still ASCII: text prints verbatim.
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x0eqx\x0f");
    assert_eq!(first_row(&term), "qx");
}

#[test]
fn characters_outside_the_graphics_range_pass_through() {
    // The map covers 0x5F..=0x7E only: uppercase, digits, punctuation below
    // 0x5F, and multi-byte UTF-8 print unchanged even with graphics active.
    let mut term = Terminal::new(24, 3);
    term.advance(b"\x1b(0AZ09-+ \xc3\xa9");
    assert_eq!(first_row(&term), "AZ09-+ \u{e9}");
}

#[test]
fn every_graphics_glyph_is_narrow_and_maps_inside_the_range() {
    // Wide-cell non-interaction: all 32 mapped glyphs must advance exactly
    // one cell, so ACS drawing never creates wide pairs or spacers.
    let mut term = Terminal::new(40, 3);
    term.advance(b"\x1b(0_`abcdefghijklmnopqrstuvwxyz{|}~");
    let row = first_row(&term);
    assert_eq!(row.chars().count(), 32, "one cell per graphics glyph");
    assert!(
        row.chars().all(|ch| !('\x5f'..='\x7e').contains(&ch)),
        "every mapped glyph leaves the input range (idempotence domain)"
    );
    assert_eq!(
        row.chars().filter(|&ch| ch == '\u{2500}').count(),
        1,
        "q maps to the horizontal line exactly once"
    );
}

#[test]
fn explicit_ascii_designation_disables_translation() {
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b(0q\x1b(Bq");
    assert_eq!(first_row(&term), "\u{2500}q");
}

#[test]
fn unknown_designators_fall_back_to_ascii_without_panicking() {
    // Parser totality: national replacement sets (A, K, ...) and arbitrary
    // finals designate ASCII — never a panic, never a wedged graphics state.
    // (`%` is a VT intermediate byte, so `ESC ( 5` stands in for an unknown
    // final that actually dispatches.)
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b(0q\x1b(Aq\x1b(0q\x1b(5q");
    assert_eq!(first_row(&term), "\u{2500}q\u{2500}q");
    assert_eq!(
        term.charset_modes(),
        CharsetModes {
            gl_g1: false,
            g0_graphics: false,
            g1_graphics: false
        },
        "the last unknown final left G0 at ASCII"
    );
}

// ─── Wrap and REP interaction ──────────────────────────────────────────────

#[test]
fn graphics_state_survives_soft_wrap() {
    // Charset state is cursor-independent: a line-drawing run that wraps at
    // the right edge keeps translating on the continuation row.
    let mut term = Terminal::new(4, 3);
    term.advance(b"\x1b(0qqqqqq");
    let text = visible_text(&term);
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap_or_default(),
        "\u{2500}\u{2500}\u{2500}\u{2500}"
    );
    assert_eq!(
        lines.next().unwrap_or_default().trim_end(),
        "\u{2500}\u{2500}"
    );
}

#[test]
fn rep_replays_the_translated_glyph() {
    // REP repeats the last printed graphic character — the stored value is
    // the already-translated glyph, so the repeat draws line segments even
    // though the map would no longer apply after SI/redesignation.
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b(0q\x1b(B\x1b[3b");
    assert_eq!(first_row(&term), "\u{2500}\u{2500}\u{2500}\u{2500}");
}

// ─── DECSC/DECRC, RIS, DECSTR ──────────────────────────────────────────────

#[test]
fn decsc_decrc_saves_and_restores_designations_and_gl() {
    let mut term = Terminal::new(20, 3);
    // Designate G1 graphics + shift out, save, then reset everything charset.
    term.advance(b"\x1b)0\x0e\x1b7\x0f\x1b)B");
    assert_eq!(
        term.charset_modes(),
        CharsetModes::default(),
        "SI + ASCII redesignation cleared the live state"
    );
    term.advance(b"\x1b8q");
    assert_eq!(
        first_row(&term),
        "\u{2500}",
        "DECRC restored G1 graphics + the SO GL selection"
    );
}

#[test]
fn ris_resets_charset_state() {
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b(0\x1b)0\x0e\x1bcq");
    assert_eq!(first_row(&term), "q");
    assert_eq!(term.charset_modes(), CharsetModes::default());
}

#[test]
fn decstr_resets_charset_state() {
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b(0\x1b)0\x0e\x1b[!pq");
    assert_eq!(first_row(&term), "q");
    assert_eq!(term.charset_modes(), CharsetModes::default());
}

// ─── Alternate-screen isolation ────────────────────────────────────────────

#[test]
fn alt_screen_starts_ascii_and_primary_designation_is_restored_on_exit() {
    let mut term = Terminal::new(20, 3);
    // Primary designates graphics; the alt screen must start clean, and
    // leaving must restore the primary's designation (kitty-flag pattern).
    term.advance(b"\x1b(0");
    term.advance(b"\x1b[?1049h");
    assert_eq!(
        term.charset_modes(),
        CharsetModes::default(),
        "fresh alt screen starts at the charset power-on state"
    );
    term.advance(b"q");
    assert_eq!(
        first_row(&term),
        "q",
        "alt screen prints ASCII untranslated"
    );
    term.advance(b"\x1b[?1049lq");
    assert!(
        term.charset_modes().active_graphics(),
        "primary graphics designation restored on exit"
    );
}

#[test]
fn alt_screen_designation_does_not_leak_into_primary() {
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b[?1049h\x1b(0");
    assert!(term.charset_modes().active_graphics());
    term.advance(b"\x1b[?1049l");
    assert_eq!(
        term.charset_modes(),
        CharsetModes::default(),
        "TUI graphics designation discarded with the alt screen"
    );
    term.advance(b"q");
    assert_eq!(first_row(&term), "q");
}

// ─── Snapshot round-trip ───────────────────────────────────────────────────

#[test]
fn charset_state_round_trips_through_the_snapshot_envelope() {
    let mut term = Terminal::new(20, 3);
    term.advance(b"\x1b)0\x0eq");
    let envelope = SnapshotEnvelope::from_terminal(&term, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().expect("encode");
    let decoded =
        SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).expect("decode");
    let mut restored = Terminal::from_snapshot_envelope(&decoded).expect("restore");
    assert_eq!(
        restored.charset_modes(),
        CharsetModes {
            gl_g1: true,
            g0_graphics: false,
            g1_graphics: true
        }
    );
    // The restored terminal keeps translating: the attach stream continues
    // mid-ACS-run without a re-designation, and the grid content itself was
    // stored post-translation.
    restored.advance(b"x");
    assert!(
        visible_text(&restored).contains('\u{2502}'),
        "restored GL selection stays live for subsequent bytes"
    );
    assert!(visible_text(&restored).contains('\u{2500}'));
}

#[test]
fn pre_charset_snapshots_decode_with_default_charset_state() {
    // A format v2 snapshot (no charset byte) must decode cleanly with the
    // power-on charset state — the version-gated appended-field contract.
    let mut term = Terminal::new(10, 2);
    term.advance(b"ok");
    let envelope = SnapshotEnvelope::from_terminal(&term, SnapshotCaptureLimits::default());
    let mut bytes = envelope.encode().expect("encode");

    // Rewrite the header version 3 -> 2 and strip the appended charset byte
    // from the terminal-state section, shrinking its table length by one.
    // Header (all integers little-endian): magic(15) + version(2) +
    // protocol(2) + producer string(u16 len + bytes) + section count(2),
    // then 5 table entries of 12 bytes (len u64 at entry offset +4).
    let version_at = 15;
    assert_eq!(
        u16::from_le_bytes([bytes[version_at], bytes[version_at + 1]]),
        3
    );
    bytes[version_at..version_at + 2].copy_from_slice(&2u16.to_le_bytes());
    let producer_len = u16::from_le_bytes([bytes[19], bytes[20]]) as usize;
    let table_start = 21 + producer_len + 2;
    let terminal_len_at = table_start + 4;
    let terminal_len = u64::from_le_bytes(
        bytes[terminal_len_at..terminal_len_at + 8]
            .try_into()
            .expect("section length"),
    ) as usize;
    bytes[terminal_len_at..terminal_len_at + 8]
        .copy_from_slice(&((terminal_len - 1) as u64).to_le_bytes());
    // The terminal-state payload starts right after the 5-entry table; its
    // charset byte is the last prelude byte (the v3 prelude is 31 bytes; a
    // v2 payload keeps the first 30).
    let payload_start = table_start + 5 * 12;
    bytes.remove(payload_start + 30);

    let decoded =
        SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).expect("v2 decode");
    assert_eq!(
        decoded.terminal.basic_modes.charsets,
        CharsetModes::default()
    );
    let restored = Terminal::from_snapshot_envelope(&decoded).expect("restore");
    assert!(visible_text(&restored).contains("ok"));
}

#[test]
fn reserved_charset_bits_fail_decode_cleanly() {
    let mut term = Terminal::new(10, 2);
    term.advance(b"x");
    let envelope = SnapshotEnvelope::from_terminal(&term, SnapshotCaptureLimits::default());
    let mut bytes = envelope.encode().expect("encode");
    // Locate the charset byte (last prelude byte of the terminal-state
    // payload, directly after the section table) and set a reserved bit.
    // Layout notes in `pre_charset_snapshots_decode_with_default_charset_state`.
    let producer_len = u16::from_le_bytes([bytes[19], bytes[20]]) as usize;
    let payload_start = 21 + producer_len + 2 + 5 * 12;
    bytes[payload_start + 30] |= 0b1000;
    let error = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
        .expect_err("reserved bits must fail decode");
    assert!(matches!(
        error,
        super::snapshot_envelope::SnapshotEnvelopeError::InvalidEnum("charset modes", _)
    ));
}

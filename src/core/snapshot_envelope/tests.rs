// SPDX-License-Identifier: GPL-3.0-only
use super::*;

use super::caps::DEFAULT_MAX_STRING_BYTES;
use super::capture::truncate_to_char_boundary;
use super::decode::decode_prompt_marks;
use super::encode::{
    SectionPayload, encode_dynamic_colors, encode_prompt_kind, encode_prompt_marks,
    encode_sections, encode_sections_for_version,
};
use super::format::{
    MAX_CELL_WIRE_BYTES, ROW_WIRE_OVERHEAD_BYTES, SECTION_DYNAMIC_COLORS, SECTION_FLAG_REQUIRED,
    SECTION_LAYOUT_STATE, SECTION_METADATA, SECTION_PROMPT_MARKS, SECTION_TERMINAL_STATE,
    TERMINAL_STATE_PRELUDE_WIRE_BYTES,
};
use crate::core::prompt_marks::PromptKind;
use crate::core::screen::Terminal;
use crate::core::types::{
    Cell, Color, CursorStyle, Dimensions, DynamicColors, MouseEncoding, MouseTracking, Position,
    RgbColor, UnderlineStyle,
};

#[test]
fn hostile_prompt_mark_count_fails_cleanly_without_over_reserve() {
    // A declared mark count at the cap with a near-empty payload must fail
    // on the first short read, not force a count-sized up-front
    // allocation. The capped reserve keeps the attempt bounded by what
    // the payload could actually encode; behavior (a clean error) is
    // unchanged for hostile and honest inputs alike.
    let caps = SnapshotEnvelopeCaps::default();
    let max = caps.max_scrollback_rows.saturating_add(caps.max_rows);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(max as u32).to_be_bytes());
    // One truncated mark's worth of payload, nowhere near `max` marks.
    bytes.extend_from_slice(&[0, 0]);
    assert!(
        decode_prompt_marks(&bytes, caps).is_err(),
        "a short payload behind a huge count must error"
    );

    // An honest small section still decodes.
    let marks = [SnapshotPromptMark {
        row: 3,
        kind: PromptKind::PromptStart,
    }];
    let encoded = encode_prompt_marks(&marks);
    let decoded = decode_prompt_marks(&encoded, caps).expect("honest section decodes");
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].row, 3);
}

#[test]
fn prompt_kind_wire_bytes_are_exact() {
    // Exact-byte pins for every PromptKind tag, including the appended
    // merged-prompt tag 3 and offset-bearing tags 4 through 7. These bytes are the
    // cross-version contract; a drift here breaks decode on the other side of
    // an attach.
    let mut out = Vec::new();
    encode_prompt_kind(&mut out, PromptKind::PromptStart);
    assert_eq!(out, [0]);

    out.clear();
    encode_prompt_kind(&mut out, PromptKind::OutputStart);
    assert_eq!(out, [1]);

    out.clear();
    encode_prompt_kind(&mut out, PromptKind::CommandEnd { exit: None });
    assert_eq!(out, [2, 0]);

    out.clear();
    encode_prompt_kind(&mut out, PromptKind::CommandEnd { exit: Some(7) });
    assert_eq!(out, [2, 1, 7, 0, 0, 0]);

    out.clear();
    encode_prompt_kind(
        &mut out,
        PromptKind::PromptStartAfterEnd { prev_exit: None },
    );
    assert_eq!(out, [3, 0]);

    out.clear();
    encode_prompt_kind(
        &mut out,
        PromptKind::PromptStartAfterEnd {
            prev_exit: Some(258),
        },
    );
    assert_eq!(out, [3, 1, 2, 1, 0, 0]);

    out.clear();
    encode_prompt_kind(
        &mut out,
        PromptKind::CommandEndAt {
            exit: Some(7),
            logical_offset: 258,
        },
    );
    assert_eq!(out, [4, 1, 7, 0, 0, 0, 2, 1, 0, 0]);

    out.clear();
    encode_prompt_kind(
        &mut out,
        PromptKind::PromptStartAfterEndAt {
            prev_exit: None,
            end_logical_offset: 258,
        },
    );
    assert_eq!(out, [5, 0, 2, 1, 0, 0]);

    out.clear();
    encode_prompt_kind(
        &mut out,
        PromptKind::OutputStartAndEndAt {
            exit: Some(7),
            logical_offset: 258,
        },
    );
    assert_eq!(out, [6, 1, 7, 0, 0, 0, 2, 1, 0, 0]);

    out.clear();
    encode_prompt_kind(
        &mut out,
        PromptKind::PromptStartAfterOutputEndAt {
            prev_exit: None,
            end_logical_offset: 258,
        },
    );
    assert_eq!(out, [7, 0, 2, 1, 0, 0]);
}

#[test]
fn merged_prompt_mark_round_trips_through_the_section() {
    let caps = SnapshotEnvelopeCaps::default();
    let marks = [
        SnapshotPromptMark {
            row: 0,
            kind: PromptKind::PromptStart,
        },
        SnapshotPromptMark {
            row: 2,
            kind: PromptKind::PromptStartAfterEnd { prev_exit: Some(1) },
        },
        SnapshotPromptMark {
            row: 5,
            kind: PromptKind::PromptStartAfterEnd { prev_exit: None },
        },
        SnapshotPromptMark {
            row: 7,
            kind: PromptKind::CommandEndAt {
                exit: Some(9),
                logical_offset: 13,
            },
        },
        SnapshotPromptMark {
            row: 9,
            kind: PromptKind::PromptStartAfterEndAt {
                prev_exit: Some(9),
                end_logical_offset: 13,
            },
        },
        SnapshotPromptMark {
            row: 11,
            kind: PromptKind::OutputStartAndEndAt {
                exit: Some(0),
                logical_offset: 101,
            },
        },
        SnapshotPromptMark {
            row: 13,
            kind: PromptKind::PromptStartAfterOutputEndAt {
                prev_exit: Some(0),
                end_logical_offset: 101,
            },
        },
    ];
    let encoded = encode_prompt_marks(&marks);
    let decoded = decode_prompt_marks(&encoded, caps).expect("section decodes");
    assert_eq!(decoded, marks);
}

#[test]
fn unknown_prompt_kind_tag_fails_cleanly() {
    // Defensive decode: a future/corrupt tag is a clean InvalidEnum, not a
    // misread stream. This is what an older decoder reports when handed a
    // snapshot containing a tag it does not know.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes()); // count
    bytes.extend_from_slice(&3u32.to_le_bytes()); // row
    bytes.push(200); // unknown kind tag
    assert!(matches!(
        decode_prompt_marks(&bytes, SnapshotEnvelopeCaps::default()),
        Err(SnapshotEnvelopeError::InvalidEnum("PromptKind", 200))
    ));
}

fn sample_terminal() -> Terminal {
    let mut terminal = Terminal::new(16, 3);
    terminal.set_scrollback_limit(8);
    terminal.set_base_colors(
        RgbColor::new(0x11, 0x22, 0x33),
        RgbColor::new(0x04, 0x05, 0x06),
        RgbColor::new(0xAA, 0xBB, 0xCC),
    );
    terminal.advance(b"\x1b]2;Snapshot Test\x1b\\");
    terminal.advance(b"\x1b]7;file://localhost/tmp/odytty-snapshot\x1b\\");
    terminal.advance(
        b"alpha\nbeta\n\x1b[31mgamma\x1b[0m\n\x1b[?2004h\x1b[?1004h\x1b[?1006h\x1b[?1003h\x1b[?2026h\x1b[?1h\x1b=\x1b[6 q",
    );
    terminal.advance(b"\x1b]133;A\x07prompt\n\x1b]133;C\x07out\n\x1b]133;D;7\x07");
    terminal.advance(b"\x1b[2;3r\x1b[9G\x1b[0g");
    terminal.advance("wide \u{1f680}\ncomb e\u{301}".as_bytes());
    terminal
}

#[test]
fn oversized_title_and_cwd_are_bounded_at_capture_so_reattach_succeeds() {
    // An OSC 2 title / OSC 7 cwd longer than the decoder's per-string cap
    // must not brick reattach: capture bounds each to the cap so the whole
    // envelope (grid, scrollback, modes) still decodes, with the strings
    // truncated rather than the file rejected.
    let mut terminal = Terminal::new(16, 3);
    let long_title = "T".repeat(5000);
    let long_cwd = format!("/tmp/{}", "d".repeat(5000));
    terminal.advance(format!("\x1b]2;{long_title}\x1b\\").as_bytes());
    terminal.advance(format!("\x1b]7;file://localhost{long_cwd}\x1b\\").as_bytes());
    terminal.advance(b"grid stays intact\n");

    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().expect("encode");
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
        .expect("an oversized title/cwd must still decode, truncated");

    let title = decoded.metadata.title.expect("title present");
    assert!(
        title.len() <= DEFAULT_MAX_STRING_BYTES,
        "title bounded to the decode cap"
    );
    assert!(
        title.starts_with('T'),
        "title content preserved (truncated)"
    );
    let cwd = decoded
        .metadata
        .working_directory
        .expect("working directory present");
    assert!(
        cwd.len() <= DEFAULT_MAX_STRING_BYTES,
        "cwd bounded to the decode cap"
    );
    // The grid content survives the reattach that the unbounded string
    // would otherwise have aborted.
    assert_eq!(decoded.terminal.dimensions, Dimensions::new(16, 3));
}

#[test]
fn truncate_to_char_boundary_never_splits_a_codepoint() {
    // A multibyte codepoint straddling the cut point is dropped whole, so
    // the result is always valid UTF-8 and within the byte bound.
    let value = "a".repeat(4095) + "\u{1f680}"; // 4-byte emoji crosses 4096
    let cut = truncate_to_char_boundary(&value, DEFAULT_MAX_STRING_BYTES);
    assert!(cut.len() <= DEFAULT_MAX_STRING_BYTES);
    assert_eq!(cut.len(), 4095, "the straddling emoji is dropped whole");
    // A string already within the bound is returned unchanged.
    assert_eq!(truncate_to_char_boundary("short", 4096), "short");
}

#[test]
fn envelope_round_trip_is_byte_stable() {
    let terminal = sample_terminal();
    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().expect("encode");
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
    assert_eq!(decoded.encode().expect("encode"), bytes);
    assert_eq!(decoded.terminal.dimensions, Dimensions::new(16, 3));
    assert!(decoded.terminal.basic_modes.bracketed_paste);
    assert!(decoded.terminal.basic_modes.focus_reporting);
    assert_eq!(
        decoded.terminal.basic_modes.mouse.tracking,
        MouseTracking::AnyEvent
    );
    assert_eq!(
        decoded.terminal.basic_modes.mouse.encoding,
        MouseEncoding::Sgr
    );
    assert!(decoded.terminal.basic_modes.synchronized_output);
    assert!(decoded.terminal.basic_modes.keyboard.application_cursor);
    assert!(decoded.terminal.basic_modes.keyboard.application_keypad);
    assert_eq!(decoded.terminal.cursor_style, CursorStyle::Bar);
    assert!(!decoded.terminal.cursor_blink);
    assert_eq!(
        decoded.dynamic_colors.foreground,
        RgbColor::new(0x11, 0x22, 0x33)
    );
    assert_eq!(decoded.metadata.title.as_deref(), Some("Snapshot Test"));
    assert_eq!(
        decoded.metadata.working_directory.as_deref(),
        Some("/tmp/odytty-snapshot")
    );
    assert!(!decoded.prompt_marks.is_empty());
    assert_eq!(
        decoded.layout.scroll_region,
        Some(SnapshotScrollRegion { top: 1, bottom: 2 })
    );
    assert!(!decoded.layout.tab_stops[8]);
}

#[test]
fn decoded_envelope_restores_fresh_terminal_state() {
    let original = sample_terminal();
    let limits = SnapshotCaptureLimits::default();
    let envelope = SnapshotEnvelope::from_terminal(&original, limits);
    let bytes = envelope.encode().expect("encode");
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();

    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

    assert_eq!(
        restored.snapshot_state(limits.max_scrollback_rows),
        decoded.terminal
    );
    assert_eq!(restored.dynamic_colors(), &decoded.dynamic_colors);
    assert_eq!(SnapshotMetadata::from_terminal(&restored), decoded.metadata);
    assert_eq!(
        restored
            .prompt_marks()
            .into_iter()
            .map(|(row, kind)| SnapshotPromptMark { row, kind })
            .collect::<Vec<_>>(),
        decoded.prompt_marks
    );
    assert_eq!(restored.snapshot_layout_state(), decoded.layout);
    assert_eq!(restored.snapshot(), original.snapshot());
    assert_eq!(restored.cursor_style(), CursorStyle::Bar);
    assert!(!restored.cursor_blinking());
    assert!(restored.synchronized_output_enabled());
    assert!(restored.bracketed_paste_enabled());
    assert!(restored.focus_reporting());
}

#[test]
fn restore_into_existing_terminal_replaces_state_and_resets_parser() {
    let original = sample_terminal();
    let decoded = SnapshotEnvelope::decode(
        &SnapshotEnvelope::from_terminal(&original, SnapshotCaptureLimits::default())
            .encode()
            .expect("encode"),
        SnapshotEnvelopeCaps::default(),
    )
    .unwrap();
    let mut restored = Terminal::new(4, 1);
    restored.advance(b"stale\x1b[31");

    restored.restore_from_envelope(&decoded).unwrap();
    restored.advance(b"Z");

    let mut expected = Terminal::from_snapshot_envelope(&decoded).unwrap();
    expected.advance(b"Z");
    assert_eq!(restored.snapshot(), expected.snapshot());
}

#[test]
fn v1_envelope_restores_with_current_defaults() {
    let terminal = sample_terminal();
    let mut terminal_payload =
        SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
            .terminal
            .encode();
    // A v1 terminal payload predates the format v3 charset byte (the
    // last prelude byte, offset 30); strip it so the fixture is v1-shaped.
    terminal_payload.remove(30);
    let bytes = encode_sections_for_version(
        1,
        "v1-test",
        SNAPSHOT_PROTOCOL_VERSION,
        &[SectionPayload {
            id: SECTION_TERMINAL_STATE,
            flags: SECTION_FLAG_REQUIRED,
            payload: terminal_payload,
        }],
    );
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

    assert_eq!(restored.dynamic_colors(), &DynamicColors::default());
    assert_eq!(
        SnapshotMetadata::from_terminal(&restored),
        SnapshotMetadata::default()
    );
    assert!(restored.prompt_marks().is_empty());
    assert_eq!(
        restored.snapshot_layout_state(),
        SnapshotLayoutState::defaults_for(decoded.terminal.dimensions)
    );
    assert_eq!(
        restored.snapshot_state(SnapshotCaptureLimits::default().max_scrollback_rows),
        decoded.terminal
    );
}

#[test]
fn minimal_terminal_restores() {
    let terminal = Terminal::new(1, 1);
    let decoded = SnapshotEnvelope::decode(
        &SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
            .encode()
            .expect("encode"),
        SnapshotEnvelopeCaps::default(),
    )
    .unwrap();
    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

    assert_eq!(restored.snapshot_state(0), decoded.terminal);
    assert_eq!(restored.snapshot(), terminal.snapshot());
}

#[test]
fn capped_scrollback_restores_only_captured_tail() {
    let mut terminal = Terminal::new(8, 2);
    terminal.set_scrollback_limit(0);
    for index in 0..12 {
        terminal.advance(format!("line{index:02}\n").as_bytes());
    }
    let limits = SnapshotCaptureLimits {
        max_scrollback_rows: 4,
    };
    let decoded = SnapshotEnvelope::decode(
        &SnapshotEnvelope::from_terminal(&terminal, limits)
            .encode()
            .expect("encode"),
        SnapshotEnvelopeCaps::default(),
    )
    .unwrap();
    assert_eq!(decoded.terminal.scrollback_rows.len(), 4);

    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

    assert_eq!(restored.screen().scrollback_len(), 4);
    assert_eq!(
        restored.snapshot_state(usize::MAX).scrollback_rows,
        decoded.terminal.scrollback_rows
    );
}

#[test]
fn direct_encode_matches_the_table_driven_encoder() {
    // The shipping encoder writes the terminal section straight into the
    // frame buffer with a backpatched section table; its bytes must match
    // the retained copy-based table-driven construction exactly, for a
    // richly populated envelope (marks, metadata, scroll region, colors).
    let terminal = sample_terminal();
    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let oracle = encode_sections(
        &envelope.producer_version,
        envelope.protocol_version,
        &[
            SectionPayload {
                id: SECTION_TERMINAL_STATE,
                flags: SECTION_FLAG_REQUIRED,
                payload: envelope.terminal.encode(),
            },
            SectionPayload {
                id: SECTION_DYNAMIC_COLORS,
                flags: 0,
                payload: encode_dynamic_colors(&envelope.dynamic_colors),
            },
            SectionPayload {
                id: SECTION_METADATA,
                flags: 0,
                payload: envelope.metadata.encode(),
            },
            SectionPayload {
                id: SECTION_PROMPT_MARKS,
                flags: 0,
                payload: encode_prompt_marks(&envelope.prompt_marks),
            },
            SectionPayload {
                id: SECTION_LAYOUT_STATE,
                flags: 0,
                payload: envelope.layout.encode(),
            },
        ],
    );
    assert_eq!(envelope.encode().expect("encode"), oracle);
}

#[test]
fn maximal_cell_wire_len_is_pinned() {
    // Worst-case cell: RGB underline color, RGB foreground/background,
    // hyperlink id, all boolean payload bytes, and a full complement of
    // combining marks. Its encoded size must match the constant the
    // capture/resize budgets are derived from; if the wire format grows,
    // this test forces the budget constant to move with it.
    let cell = SnapshotCell {
        ch: '\u{10FFFD}',
        attrs: SnapshotAttrs {
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            blink: true,
            strikethrough: true,
            inverse: true,
            hidden: true,
            underline_style: UnderlineStyle::Curly,
            underline_color: Some(Color::Rgb(1, 2, 3)),
            foreground: Color::Rgb(4, 5, 6),
            background: Color::Rgb(7, 8, 9),
            hyperlink: Some(77),
        },
        protected: true,
        wide_continuation: true,
        combining: vec!['\u{301}'; super::super::types::MAX_COMBINING],
    };
    let mut out = Vec::new();
    cell.encode(&mut out);
    assert_eq!(out.len(), MAX_CELL_WIRE_BYTES);
}

#[test]
fn terminal_state_prelude_wire_len_is_pinned() {
    let state = Terminal::new(4, 2).snapshot_state(0);
    let mut out = Vec::new();
    state.encode_prelude(&mut out);
    assert_eq!(out.len(), TERMINAL_STATE_PRELUDE_WIRE_BYTES);
}

/// A blank-padded terminal state with `scrollback` scrollback rows, each
/// row's first cell tagged with a row index so truncation order is
/// observable.
fn synthetic_state(columns: usize, rows: usize, scrollback: usize) -> SnapshotTerminalState {
    let make_row = |tag: char| {
        let mut cells: Vec<SnapshotCell> = vec![Cell::blank().into(); columns];
        cells[0].ch = tag;
        SnapshotRow {
            wrapped: false,
            cells,
        }
    };
    SnapshotTerminalState {
        dimensions: Dimensions::new(columns, rows),
        cursor: Position { row: 0, column: 0 },
        cursor_visible: true,
        cursor_style: CursorStyle::Block,
        cursor_blink: true,
        basic_modes: Terminal::new(4, 2).snapshot_state(0).basic_modes,
        scrollback_rows: (0..scrollback)
            .map(|index| make_row(if index + 1 == scrollback { 'N' } else { 'o' }))
            .collect(),
        visible_rows: (0..rows).map(|_| make_row('v')).collect(),
    }
}

#[test]
fn capture_bounding_truncates_oldest_scrollback_to_the_section_budget() {
    // 200 columns x 10k scrollback rows encodes a terminal section past
    // the decoder's section cap while staying under the frame cap: the
    // exact shape that made a session permanently un-attachable (the host
    // served a snapshot its own default consumers rejected). Bounding
    // must shed the OLDEST rows until the state is self-decodable.
    let mut state = synthetic_state(200, 50, 10_000);
    let unbounded = state.clone().encode();
    assert!(
        unbounded.len() > SnapshotEnvelopeCaps::default().max_section_len,
        "repro shape must exceed the section budget to prove the coupling"
    );

    let dropped = state.bound_to_decode_budget();
    assert!(dropped > 0, "over-budget state must shed rows");
    assert!(
        state.encode().len() <= SnapshotEnvelopeCaps::default().max_section_len,
        "bounded terminal section fits the decode budget"
    );
    // Newest row survives; only the oldest rows were shed.
    assert_eq!(
        state.scrollback_rows.last().expect("rows remain").cells[0].ch,
        'N'
    );

    // Pairwise invariant: the bounded capture decodes under the same
    // default caps its consumers use.
    let envelope = SnapshotEnvelope {
        producer_version: "test".to_owned(),
        protocol_version: SNAPSHOT_PROTOCOL_VERSION,
        terminal: state,
        dynamic_colors: DynamicColors::default(),
        metadata: SnapshotMetadata::default(),
        prompt_marks: Vec::new(),
        layout: SnapshotLayoutState::defaults_for(Dimensions::new(200, 50)),
    };
    let decoded = SnapshotEnvelope::decode(
        &envelope.encode().expect("encode"),
        SnapshotEnvelopeCaps::default(),
    )
    .expect("bounded capture output decodes under matching defaults");
    assert_eq!(
        decoded.terminal.scrollback_rows.len(),
        envelope.terminal.scrollback_rows.len()
    );
}

#[test]
fn capture_bounding_enforces_row_and_cell_budgets() {
    // Small custom caps make the count-based arms observable without
    // building 100k-row fixtures: the row-count cap and the total-cell
    // cap must each shed oldest scrollback independently of byte size.
    let caps = SnapshotEnvelopeCaps {
        max_scrollback_rows: 3,
        ..SnapshotEnvelopeCaps::default()
    };
    let mut state = synthetic_state(4, 2, 10);
    assert_eq!(state.bound_to_decode_budget_with(&caps), 7);
    assert_eq!(state.scrollback_rows.len(), 3);
    assert_eq!(state.scrollback_rows[2].cells[0].ch, 'N');

    let caps = SnapshotEnvelopeCaps {
        // 4 columns x (2 visible + 10 scrollback) = 48 cells; a 28-cell
        // budget leaves room for 5 scrollback rows beside the visible 2.
        max_cells: 28,
        ..SnapshotEnvelopeCaps::default()
    };
    let mut state = synthetic_state(4, 2, 10);
    assert_eq!(state.bound_to_decode_budget_with(&caps), 5);
    assert_eq!(state.scrollback_rows.len(), 5);
}

#[test]
fn deep_wide_session_snapshot_decodes_and_restores_end_to_end() {
    // End-to-end regression for the 200-column repro: a real terminal
    // with capture-limit-deep scrollback and prompt marks near the tail
    // must produce an envelope that decodes AND restores under default
    // caps, with marks rebased onto the truncated history.
    let mut terminal = Terminal::new(200, 50);
    terminal.set_scrollback_limit(SnapshotCaptureLimits::default().max_scrollback_rows);
    for index in 0..10_100u32 {
        terminal.advance(format!("row {index}\r\n").as_bytes());
    }
    terminal.advance(b"\x1b]133;A\x07tail prompt\r\n");

    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().expect("encode");
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
        .expect("deep wide capture decodes under default caps");
    assert!(
        decoded.terminal.scrollback_rows.len() < 10_000,
        "scrollback was truncated to fit the decode budget"
    );
    let total_rows = decoded.terminal.scrollback_rows.len() + decoded.terminal.visible_rows.len();
    assert!(!decoded.prompt_marks.is_empty(), "tail mark survives");
    assert!(
        decoded
            .prompt_marks
            .iter()
            .all(|mark| mark.row < total_rows),
        "marks are rebased inside the truncated history"
    );
    let restored = Terminal::from_snapshot_envelope(&decoded).expect("bounded snapshot restores");
    assert!(!restored.prompt_marks().is_empty());
}

#[test]
fn capture_limit_truncation_rebases_prompt_marks() {
    // Marks on rows older than the capture window are dropped; marks on
    // captured rows shift down by the number of rows cut ahead of them.
    let mut terminal = Terminal::new(8, 2);
    terminal.set_scrollback_limit(0);
    terminal.advance(b"\x1b]133;A\x07p0\n");
    for index in 0..10 {
        terminal.advance(format!("line{index:02}\n").as_bytes());
    }
    terminal.advance(b"\x1b]133;A\x07p1\n");

    let limits = SnapshotCaptureLimits {
        max_scrollback_rows: 4,
    };
    let envelope = SnapshotEnvelope::from_terminal(&terminal, limits);
    assert_eq!(envelope.terminal.scrollback_rows.len(), 4);
    let full_marks = terminal.prompt_marks();
    assert_eq!(full_marks.len(), 2, "both marks live in full history");
    let dropped = terminal.screen().scrollback_len() - 4;
    let expected: Vec<_> = full_marks
        .into_iter()
        .filter_map(|(row, kind)| {
            row.checked_sub(dropped)
                .map(|row| SnapshotPromptMark { row, kind })
        })
        .collect();
    assert_eq!(expected.len(), 1, "the pre-window mark is dropped");
    assert_eq!(envelope.prompt_marks, expected);
    // The rebased envelope restores cleanly (an unrebased mark would be
    // rejected as out of range or land on the wrong row).
    Terminal::from_snapshot_envelope(&envelope).expect("rebased marks restore");
}

#[test]
fn resize_budget_guarantees_worst_case_visible_grid_decodes() {
    // The advertised visible-cell budget must be honest: a grid at the
    // budget filled entirely with worst-case cells still encodes within
    // the section cap, with zero scrollback room required.
    let caps = SnapshotEnvelopeCaps::default();
    let budget = caps.max_self_decodable_visible_cells();
    assert!(budget >= 500_000, "budget covers any realistic display");
    assert!(budget <= caps.max_cells);
    // Worst-case per-cell wire size at the budget fits the section cap.
    let worst = budget
        .checked_mul(MAX_CELL_WIRE_BYTES)
        .and_then(|cells| cells.checked_add(caps.max_rows.checked_mul(ROW_WIRE_OVERHEAD_BYTES)?))
        .and_then(|total| total.checked_add(TERMINAL_STATE_PRELUDE_WIRE_BYTES + 8))
        .expect("budget arithmetic stays in range");
    assert!(worst <= caps.max_section_len);
}

#[test]
fn active_alternate_screen_restores_active_grid_and_mode_flag() {
    let mut terminal = Terminal::new(10, 3);
    terminal.advance(b"primary\nhistory\n");
    terminal.advance(b"\x1b[?1049h\x1b[2J\x1b[Halt-one\nalt-two\x1b[4 q");
    let decoded = SnapshotEnvelope::decode(
        &SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
            .encode()
            .expect("encode"),
        SnapshotEnvelopeCaps::default(),
    )
    .unwrap();
    assert!(decoded.terminal.basic_modes.alternate_screen);

    let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();

    assert!(restored.on_alternate_screen());
    assert_eq!(
        restored.snapshot_state(SnapshotCaptureLimits::default().max_scrollback_rows),
        decoded.terminal
    );
    assert_eq!(restored.snapshot(), terminal.snapshot());
    assert_eq!(restored.cursor_style(), CursorStyle::Underline);
}

#[test]
fn invalid_prompt_mark_is_rejected_on_restore() {
    let terminal = Terminal::new(2, 1);
    let mut envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    envelope.prompt_marks.push(SnapshotPromptMark {
        row: 99,
        kind: PromptKind::PromptStart,
    });

    assert!(matches!(
        Terminal::from_snapshot_envelope(&envelope),
        Err(SnapshotEnvelopeError::InvalidPromptMark { row: 99, rows: 1 })
    ));
}

#[test]
fn unknown_optional_section_is_ignored() {
    let terminal = sample_terminal();
    let terminal_payload =
        SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
            .terminal
            .encode();
    let bytes = encode_sections(
        "test",
        SNAPSHOT_PROTOCOL_VERSION,
        &[
            SectionPayload {
                id: 77,
                flags: 0,
                payload: vec![1, 2, 3],
            },
            SectionPayload {
                id: SECTION_TERMINAL_STATE,
                flags: SECTION_FLAG_REQUIRED,
                payload: terminal_payload,
            },
        ],
    );
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
    assert_eq!(decoded.producer_version, "test");
}

#[test]
fn unknown_required_section_is_rejected() {
    let bytes = encode_sections(
        "test",
        SNAPSHOT_PROTOCOL_VERSION,
        &[SectionPayload {
            id: 88,
            flags: SECTION_FLAG_REQUIRED,
            payload: vec![1, 2, 3],
        }],
    );
    assert_eq!(
        SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()),
        Err(SnapshotEnvelopeError::UnknownRequiredSection(88))
    );
}

#[test]
fn version_mismatch_is_rejected_cleanly() {
    let terminal = sample_terminal();
    let mut bytes = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
        .encode()
        .expect("encode");
    bytes[SNAPSHOT_MAGIC.len()] = 99;
    assert_eq!(
        SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()),
        Err(SnapshotEnvelopeError::UnsupportedVersion {
            format_version: 99,
            protocol_version: SNAPSHOT_PROTOCOL_VERSION,
        })
    );
}

#[test]
fn oversized_section_is_rejected_by_cap() {
    let terminal = sample_terminal();
    let bytes = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
        .encode()
        .expect("encode");
    let caps = SnapshotEnvelopeCaps {
        max_section_len: 1,
        ..SnapshotEnvelopeCaps::default()
    };
    let err = SnapshotEnvelope::decode(&bytes, caps).unwrap_err();
    assert!(matches!(err, SnapshotEnvelopeError::SectionTooLarge { .. }));
}

#[test]
fn v1_envelope_decodes_with_v2_defaults() {
    let terminal = sample_terminal();
    let mut terminal_payload =
        SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default())
            .terminal
            .encode();
    // A v1 terminal payload predates the format v3 charset byte (the
    // last prelude byte, offset 30); strip it so the fixture is v1-shaped.
    terminal_payload.remove(30);
    let bytes = encode_sections_for_version(
        1,
        "v1-test",
        SNAPSHOT_PROTOCOL_VERSION,
        &[SectionPayload {
            id: SECTION_TERMINAL_STATE,
            flags: SECTION_FLAG_REQUIRED,
            payload: terminal_payload,
        }],
    );
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default()).unwrap();
    assert_eq!(decoded.producer_version, "v1-test");
    assert_eq!(decoded.dynamic_colors, DynamicColors::default());
    assert_eq!(decoded.metadata, SnapshotMetadata::default());
    assert!(decoded.prompt_marks.is_empty());
    assert_eq!(
        decoded.layout,
        SnapshotLayoutState::defaults_for(decoded.terminal.dimensions)
    );
}

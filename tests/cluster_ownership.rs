// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored fixtures, licensed under the project license.
//! Core ownership is shared by Linux, macOS and Windows.

use odytty::core::{
    Position, SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps, Terminal,
};

fn source_text(terminal: &Terminal) -> String {
    terminal
        .snapshot()
        .cells
        .iter()
        .filter(|cell| !cell.wide_continuation && !cell.layout_padding)
        .map(|cell| cell.grapheme())
        .collect::<String>()
        .trim_end_matches(' ')
        .to_owned()
}

#[test]
fn deeper_clusters_and_leading_marks_are_retained() {
    for text in [
        "a\u{301}\u{302}\u{303}\u{304}\u{305}\u{306}",
        "\u{301}\u{302}",
    ] {
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(text.as_bytes());
        assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), text);
        assert_eq!(terminal.screen().cursor(), Position { row: 0, column: 1 });
    }
}

#[test]
fn controls_and_edits_terminate_extension_but_sgr_preserves_it() {
    for control in [
        "\r", "\x1b[1G", "\x1b[1D", "\x1b[@", "\x1b[P", "\x1b[X", "\x1b[K", "\x07",
    ] {
        let mut terminal = Terminal::new(8, 2);
        terminal.advance(b"A");
        terminal.advance(control.as_bytes());
        let before = terminal.snapshot();
        let cursor = terminal.screen().cursor();
        terminal.advance("\u{301}".as_bytes());
        assert_eq!(
            terminal
                .screen()
                .cell(cursor.row, cursor.column)
                .unwrap()
                .ch,
            '\u{301}',
            "{control:?}"
        );
        if cursor.column > 0 {
            assert_eq!(
                terminal.screen().cell(0, 0),
                Some(before.cells[0]),
                "{control:?}"
            );
        }
    }
    let mut terminal = Terminal::new(8, 2);
    terminal.advance("A\x1b[31m\u{301}".as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), "A\u{301}");
}

#[test]
fn thai_lao_and_tibetan_ownership_survives_every_byte_split() {
    for (text, width) in [
        ("\u{e01}\u{e48}", 1),
        ("\u{e81}\u{ec8}", 1),
        ("\u{e01}\u{e33}", 2),
        ("\u{e81}\u{eb3}", 2),
        ("\u{f40}\u{f90}\u{f72}", 1),
    ] {
        let mut whole = Terminal::new(12, 2);
        whole.advance(text.as_bytes());
        assert_eq!(whole.screen().cell(0, 0).unwrap().grapheme(), text);
        assert_eq!(whole.screen().cursor().column, width);
        for split in 0..=text.len() {
            let mut terminal = Terminal::new(12, 2);
            terminal.advance(&text.as_bytes()[..split]);
            terminal.advance(&text.as_bytes()[split..]);
            assert_eq!(terminal.snapshot().cells, whole.snapshot().cells);
            assert_eq!(terminal.screen().cursor(), whole.screen().cursor());
        }
        let envelope = SnapshotEnvelope::from_terminal(&whole, SnapshotCaptureLimits::default());
        let decoded =
            SnapshotEnvelope::decode(&envelope.encode().unwrap(), SnapshotEnvelopeCaps::default())
                .unwrap();
        let restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
        assert_eq!(restored.snapshot().cells, whole.snapshot().cells);
    }
}

#[test]
fn spacing_extension_at_right_edge_moves_the_owner_whole() {
    let mut terminal = Terminal::new(4, 3);
    terminal.advance("abc\u{e01}\u{e33}Z".as_bytes());
    assert!(terminal.screen().cell(0, 3).unwrap().layout_padding);
    assert_eq!(
        terminal.screen().cell(1, 0).unwrap().grapheme(),
        "\u{e01}\u{e33}"
    );
    assert!(terminal.screen().cell(1, 1).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(1, 2).unwrap().ch, 'Z');
    terminal.resize(8, 3);
    assert!(
        terminal
            .snapshot()
            .cells
            .iter()
            .any(|c| c.grapheme() == "\u{e01}\u{e33}")
    );
}

#[test]
fn bounded_owners_preserve_overflow_and_roundtrip_text() {
    let text = format!("A{}Z", "\u{301}".repeat(40));
    let mut terminal = Terminal::new(12, 3);
    for byte in text.as_bytes() {
        terminal.advance(std::slice::from_ref(byte));
    }
    assert_eq!(source_text(&terminal), text);
    assert_eq!(terminal.screen().cursor().column, 4);
    for cell in terminal.snapshot().cells {
        assert!(cell.combining().len() <= 16);
    }
    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let decoded =
        SnapshotEnvelope::decode(&envelope.encode().unwrap(), SnapshotEnvelopeCaps::default())
            .unwrap();
    let mut restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
    assert_eq!(source_text(&restored), text);
    for width in [2, 7, 3, 12] {
        restored.resize(width, 3);
        assert_eq!(source_text(&restored), text);
    }
}

#[test]
fn snapshots_preserve_both_open_and_terminated_extension_boundaries() {
    for prefix in ["A", "abc\u{e01}", "A\x07", "A\x1b[1G"] {
        let mut original = Terminal::new(4, 3);
        original.advance(prefix.as_bytes());
        let envelope = SnapshotEnvelope::from_terminal(&original, SnapshotCaptureLimits::default());
        let decoded =
            SnapshotEnvelope::decode(&envelope.encode().unwrap(), SnapshotEnvelopeCaps::default())
                .unwrap();
        let mut restored = Terminal::from_snapshot_envelope(&decoded).unwrap();
        for suffix in ["\u{301}", "\u{e33}", "Z"] {
            original.advance(suffix.as_bytes());
            restored.advance(suffix.as_bytes());
            assert_eq!(
                restored.snapshot().cells,
                original.snapshot().cells,
                "{prefix:?}"
            );
            assert_eq!(restored.screen().cursor(), original.screen().cursor());
        }
    }
}

#[test]
fn edits_rebuild_whole_owners_and_preserve_outside_neighbors() {
    for edit in ["\x1b[1X", "\x1b[1P", "\x1b[1@", "X"] {
        let mut terminal = Terminal::new(10, 2);
        terminal.advance("L\u{e01}\u{e33}R".as_bytes());
        terminal.advance(b"\x1b[3G");
        terminal.advance(edit.as_bytes());
        assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, 'L');
        assert!(
            !terminal
                .snapshot()
                .cells
                .iter()
                .any(|c| c.grapheme() == "\u{e01}\u{e33}"),
            "{edit:?}"
        );
        assert!(
            terminal.snapshot().cells.iter().any(|c| c.ch == 'R'),
            "{edit:?}"
        );
        terminal.advance("\u{301}".as_bytes());
        assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), "L");
    }
}

#[test]
fn prebase_vowels_and_unrelated_content_keep_separate_owners() {
    for text in ["X\u{e33}", "\u{e40}\u{e01}", "\u{ec0}\u{e81}"] {
        let mut terminal = Terminal::new(8, 2);
        terminal.advance(text.as_bytes());
        assert_eq!(terminal.screen().cursor().column, 2);
        assert_eq!(
            terminal
                .screen()
                .cell(0, 0)
                .unwrap()
                .grapheme()
                .chars()
                .count(),
            1
        );
        assert_eq!(source_text(&terminal), text);
    }
}

#[test]
fn source_ownership_maps_copy_and_search_through_history_reflow() {
    use odytty::core::SearchOptions;
    use odytty::selection::{CellPoint, SelectionRange, selected_text};
    let source = format!(
        "abc\u{e01}\u{e33}\u{f40}\u{f90}\u{f72}A{}Z",
        "\u{301}".repeat(40)
    );
    let mut terminal = Terminal::new(4, 12);
    terminal.advance(source.as_bytes());
    let selection = SelectionRange {
        start: CellPoint { row: 0, column: 0 },
        end: CellPoint { row: 11, column: 3 },
    };
    let copied = selected_text(&terminal.snapshot(), selection);
    // Snapshot copy retains its physical-row newline convention.
    assert_eq!(copied.replace('\n', ""), source);
    assert_eq!(
        terminal
            .search(&source, SearchOptions::case_sensitive())
            .len(),
        1
    );
    for _ in 0..20 {
        terminal.advance(b"\r\n");
    }
    for width in [8, 3, 9, 4] {
        terminal.resize(width, 12);
        assert_eq!(
            terminal
                .search(&source, SearchOptions::case_sensitive())
                .len(),
            1
        );
    }
}

#[test]
fn extending_an_owner_invalidates_the_rendered_frame() {
    let mut terminal = Terminal::new(4, 2);
    terminal.advance("\u{e01}".as_bytes());
    let before = terminal.render_revision();
    terminal.advance("\u{e33}".as_bytes());
    assert_ne!(terminal.render_revision(), before);
    let before = terminal.render_revision();
    terminal.advance("\u{e48}".as_bytes());
    assert_ne!(terminal.render_revision(), before);
}

#[test]
fn inserted_extension_preserves_following_source_text() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(b"LR\x1b[2G\x1b[4h");
    terminal.advance("\u{e01}\u{e33}".as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().ch, 'L');
    assert_eq!(
        terminal.screen().cell(0, 1).unwrap().grapheme(),
        "\u{e01}\u{e33}"
    );
    assert!(terminal.screen().cell(0, 2).unwrap().wide_continuation);
    assert_eq!(terminal.screen().cell(0, 3).unwrap().ch, 'R');
}

#[test]
fn unassigned_lao_scalar_is_not_a_consonant_owner() {
    let mut terminal = Terminal::new(8, 2);
    terminal.advance("\u{e83}\u{eb3}".as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), "\u{e83}");
    assert_eq!(terminal.screen().cell(0, 1).unwrap().grapheme(), "\u{eb3}");
}

#[test]
fn unattached_format_controls_keep_zero_columns_and_attached_controls_extend() {
    for ch in [
        '\u{ad}',
        '\u{34f}',
        '\u{61c}',
        '\u{180e}',
        '\u{200b}',
        '\u{200c}',
        '\u{200d}',
        '\u{200e}',
        '\u{202a}',
        '\u{202e}',
        '\u{2060}',
        '\u{2064}',
        '\u{2066}',
        '\u{fe00}',
        '\u{fe0f}',
        '\u{feff}',
        '\u{1bca0}',
        '\u{1d173}',
        '\u{e0001}',
        '\u{e0020}',
        '\u{e0100}',
        '\u{e01ef}',
    ] {
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(ch.to_string().as_bytes());
        assert_eq!(terminal.screen().cursor().column, 0, "{ch:?}");
        assert_eq!(source_text(&terminal), "", "{ch:?}");
        terminal.advance(format!("A{ch}").as_bytes());
        assert_eq!(terminal.screen().cursor().column, 1, "{ch:?}");
        assert_eq!(source_text(&terminal), format!("A{ch}"), "{ch:?}");
    }
}

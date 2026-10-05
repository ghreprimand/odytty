// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored GPL-3.0-only northern Indic ownership fixtures.
//! Terminal width units are distinct from Unicode grapheme boundaries.
use odytty::core::{
    Position, SearchOptions, SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps,
    Terminal,
};

const CONJUNCTS: [&str; 5] = [
    "\u{915}\u{94d}\u{937}",
    "\u{995}\u{9cd}\u{9b7}",
    "\u{a15}\u{a4d}\u{a15}",
    "\u{a95}\u{acd}\u{ab7}",
    "\u{b15}\u{b4d}\u{b37}",
];
fn frozen_cases() -> Vec<(&'static str, usize)> {
    let mut cases = vec![("\u{915}\u{93c}", 1), ("\u{915}\u{93e}", 2)];
    cases.extend(CONJUNCTS.into_iter().map(|s| (s, 2)));
    cases
}
fn restored(t: &Terminal) -> Terminal {
    let e = SnapshotEnvelope::from_terminal(t, SnapshotCaptureLimits::default());
    Terminal::from_snapshot_envelope(
        &SnapshotEnvelope::decode(&e.encode().unwrap(), SnapshotEnvelopeCaps::default()).unwrap(),
    )
    .unwrap()
}
#[test]
fn exact_frozen_width_and_owner_cases() {
    for (text, width) in frozen_cases() {
        let mut t = Terminal::new(20, 3);
        t.advance(text.as_bytes());
        assert_eq!(t.screen().cursor().column, width, "{text:?}");
        assert_eq!(t.screen().cell(0, 0).unwrap().grapheme(), text);
        assert_eq!(t.screen().cell(0, 1).unwrap().wide_continuation, width == 2);
        t.advance(b"X");
        assert_eq!(t.screen().cell(0, width).unwrap().ch, 'X');
    }
}
#[test]
fn every_utf8_split_and_snapshot_boundary_preserves_conjunct() {
    for (text, _) in frozen_cases() {
        let mut whole = Terminal::new(20, 3);
        whole.advance(text.as_bytes());
        for split in 0..=text.len() {
            let mut t = Terminal::new(20, 3);
            t.advance(&text.as_bytes()[..split]);
            t.advance(&text.as_bytes()[split..]);
            assert_eq!(t.snapshot().cells, whole.snapshot().cells);
            if text.is_char_boundary(split) {
                let mut prefix = Terminal::new(20, 3);
                prefix.advance(&text.as_bytes()[..split]);
                let mut t = restored(&prefix);
                t.advance(&text.as_bytes()[split..]);
                assert_eq!(t.snapshot().cells, whole.snapshot().cells);
                assert_eq!(t.screen().cursor(), whole.screen().cursor());
            }
        }
    }
}
#[test]
fn zwj_continues_while_zwnj_ends_consonant_ownership() {
    for text in CONJUNCTS {
        let mut chars: Vec<char> = text.chars().collect();
        chars.insert(2, '\u{200d}');
        let joined: String = chars.iter().collect();
        let mut t = Terminal::new(20, 2);
        t.advance(joined.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().grapheme(), joined);
        assert_eq!(t.screen().cursor().column, 2);
        chars[2] = '\u{200c}';
        let broken: String = chars.iter().collect();
        let mut t = Terminal::new(20, 2);
        t.advance(broken.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().combining(), &chars[1..3]);
        assert_eq!(t.screen().cell(0, 1).unwrap().ch, chars[3]);
        assert!(!t.screen().cell(0, 1).unwrap().wide_continuation);
    }
}
#[test]
fn streaming_expansion_wraps_whole_owner_and_preserves_irm_neighbors() {
    for text in CONJUNCTS {
        let mut t = Terminal::new(4, 3);
        t.advance(b"ABC");
        t.advance(text.as_bytes());
        assert!(t.screen().cell(0, 3).unwrap().layout_padding);
        assert_eq!(t.screen().cell(1, 0).unwrap().grapheme(), text);
        assert_eq!(t.screen().cursor(), Position { row: 1, column: 2 });
        let mut t = Terminal::new(10, 2);
        t.advance(b"LR\x1b[2G\x1b[4h");
        t.advance(text.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().ch, 'L');
        assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), text);
        assert_eq!(t.screen().cell(0, 3).unwrap().ch, 'R');
    }
}
#[test]
fn edits_and_controls_never_extend_stale_conjuncts() {
    for control in [
        "\r", "\x1b[1G", "\x1b[@", "\x1b[P", "\x1b[X", "\x1b[K", "\n",
    ] {
        let mut t = Terminal::new(20, 3);
        t.advance("\u{915}\u{94d}".as_bytes());
        t.advance(control.as_bytes());
        let at = t.screen().cursor();
        t.advance("\u{937}".as_bytes());
        assert_eq!(
            t.screen().cell(at.row, at.column).unwrap().grapheme(),
            "\u{937}"
        );
    }
    let mut t = Terminal::new(20, 2);
    t.advance("\u{915}\u{94d}\x1b[31m\u{937}".as_bytes());
    assert_eq!(t.screen().cell(0, 0).unwrap().grapheme(), CONJUNCTS[0]);
}
#[test]
fn history_search_copy_and_reflow_keep_logical_source() {
    for text in CONJUNCTS {
        let mut t = Terminal::new(8, 4);
        t.advance(format!("L{text}R").as_bytes());
        assert_eq!(
            odytty::selection::selected_text(
                &t.snapshot(),
                odytty::selection::SelectionRange {
                    start: odytty::selection::CellPoint { row: 0, column: 0 },
                    end: odytty::selection::CellPoint { row: 0, column: 7 }
                }
            )
            .trim_end(),
            format!("L{text}R")
        );
        for _ in 0..10 {
            t.advance(b"\r\n");
        }
        for width in [3, 9, 4, 12] {
            t.resize(width, 4);
            assert_eq!(t.search(text, SearchOptions::case_sensitive()).len(), 1);
        }
        let t = restored(&t);
        assert_eq!(t.search(text, SearchOptions::case_sensitive()).len(), 1);
    }
}
#[test]
fn distinct_scripts_digits_and_non_g1_owners_stay_separate() {
    for text in [
        "\u{915}\u{995}",
        "\u{915}\u{94d}1",
        "A\u{93e}",
        "\u{1b13}\u{1b44}\u{1b13}",
        "\u{915}\u{94d}\u{995}",
    ] {
        let mut t = Terminal::new(20, 2);
        t.advance(text.as_bytes());
        assert!(
            !t.screen().cell(0, 1).unwrap().wide_continuation,
            "{text:?}"
        );
    }
}

#[test]
fn unicode_17_grapheme_break_subset_preserves_source_owners() {
    let fixture = include_str!("fixtures/unicode-indic/GraphemeBreakTest-G1.txt");
    let mut count = 0;
    for line in fixture.lines() {
        let body = line.split('#').next().unwrap().trim();
        if body.is_empty() {
            continue;
        }
        let mut expected = Vec::new();
        let mut cluster = String::new();
        let mut text = String::new();
        for token in body.split_whitespace() {
            match token {
                "÷" if !cluster.is_empty() => {
                    expected.push(std::mem::take(&mut cluster));
                }
                "÷" | "×" => {}
                scalar => {
                    let ch = char::from_u32(u32::from_str_radix(scalar, 16).unwrap()).unwrap();
                    cluster.push(ch);
                    text.push(ch);
                }
            }
        }
        let mut t = Terminal::new(40, 2);
        t.advance(text.as_bytes());
        let actual: Vec<_> = t
            .snapshot()
            .cells
            .iter()
            .filter(|c| !c.wide_continuation && c.ch != ' ')
            .map(|c| c.grapheme())
            .collect();
        assert_eq!(actual, expected, "{body}");
        count += 1;
    }
    assert_eq!(count, 19);
}

#[test]
fn unicode_17_spacing_and_consonant_properties_cover_all_five_scripts() {
    let fixture = include_str!("fixtures/unicode-indic/G1-properties.txt");
    let bases = ['\u{915}', '\u{995}', '\u{a15}', '\u{a95}', '\u{b15}'];
    let viramas = ['\u{94d}', '\u{9cd}', '\u{a4d}', '\u{acd}', '\u{b4d}'];
    let mut spacing_counts = [0usize; 5];
    let mut consonant_counts = [0usize; 5];
    for line in fixture.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<_> = line.split(';').collect();
        let cp = char::from_u32(u32::from_str_radix(fields[0], 16).unwrap()).unwrap();
        let script: usize = fields[1].parse::<usize>().unwrap() - 1;
        let text = if fields[2] == "Mc" {
            spacing_counts[script] += 1;
            format!("{}{cp}", bases[script])
        } else if fields[3] == "Consonant" {
            consonant_counts[script] += 1;
            format!("{}{}{cp}", bases[script], viramas[script])
        } else {
            continue;
        };
        let mut t = Terminal::new(20, 2);
        t.advance(text.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().grapheme(), text, "{line}");
        assert_eq!(t.screen().cursor().column, 2, "{line}");
    }
    assert!(spacing_counts.into_iter().all(|n| n > 0));
    assert!(consonant_counts.into_iter().all(|n| n > 0));
    // Gurmukhi is intentionally a terminal width unit across an EGC break.
    assert!(fixture.contains("0A15;3;Lo;Consonant;"));
    assert!(fixture.contains("0915;1;Lo;Consonant;InCB;Consonant"));
    assert!(fixture.contains("094D;1;Mn;Virama;InCB;Linker"));
}

#[test]
fn repeated_conjuncts_and_spacing_extensions_stay_two_cells() {
    for text in CONJUNCTS {
        let chars: Vec<_> = text.chars().collect();
        let stack = format!("{text}{}{}", chars[1], chars[2]);
        let mut t = Terminal::new(20, 2);
        t.advance(stack.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().grapheme(), stack);
        assert_eq!(t.screen().cursor().column, 2);
    }
    let mut t = Terminal::new(20, 2);
    t.advance("\u{915}\u{93e}\u{93e}\u{301}".as_bytes());
    assert_eq!(t.screen().cursor().column, 2);
    assert_eq!(
        t.screen().cell(0, 0).unwrap().grapheme(),
        "\u{915}\u{93e}\u{93e}\u{301}"
    );
}

#[test]
fn source_bounds_and_invalid_cross_script_linkers_remain_bounded() {
    let source = format!("\u{915}{}", "\u{94d}\u{937}".repeat(30));
    let mut t = Terminal::new(80, 2);
    t.advance(source.as_bytes());
    let copied: String = t
        .snapshot()
        .cells
        .iter()
        .filter(|c| !c.wide_continuation && !c.layout_padding)
        .map(|c| c.grapheme())
        .collect::<String>()
        .trim_end()
        .to_owned();
    assert_eq!(copied, source);
    let mut t = Terminal::new(20, 2);
    t.advance("\u{915}\u{9cd}\u{937}".as_bytes());
    assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), "\u{937}");
    // An independent vowel is not a conjunct consonant.
    let mut t = Terminal::new(20, 2);
    t.advance("\u{905}\u{94d}\u{937}".as_bytes());
    assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), "\u{937}");
}

#[test]
fn terminal_application_redraw_and_alternate_screen_table_keep_owners() {
    // Authored line-editor redraw stream and full-screen table stream.
    let mut t = Terminal::new(20, 4);
    t.advance(format!("old\r\x1b[2K> {} ", CONJUNCTS[0]).as_bytes());
    assert_eq!(t.screen().cell(0, 2).unwrap().grapheme(), CONJUNCTS[0]);
    assert_eq!(t.screen().cursor().column, 5);
    t.advance(b"\x1b[?1049h");
    t.advance(format!("|{}|{}|\r\n", CONJUNCTS[1], CONJUNCTS[3]).as_bytes());
    assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), CONJUNCTS[1]);
    assert_eq!(t.screen().cell(0, 4).unwrap().grapheme(), CONJUNCTS[3]);
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.screen().cell(0, 2).unwrap().grapheme(), CONJUNCTS[0]);
}

#[test]
fn erase_or_overwrite_either_half_clears_the_whole_owner() {
    for text in CONJUNCTS {
        for command in ["\x1b[2GX", "\x1b[3GX", "\x1b[2G\x1b[X", "\x1b[3G\x1b[X"] {
            let mut t = Terminal::new(10, 2);
            t.advance(format!("L{text}R").as_bytes());
            t.advance(command.as_bytes());
            assert_eq!(t.screen().cell(0, 0).unwrap().ch, 'L');
            assert_eq!(t.screen().cell(0, 3).unwrap().ch, 'R');
            assert!(!t.screen().cell(0, 1).unwrap().wide_continuation);
            assert!(!t.screen().cell(0, 2).unwrap().wide_continuation);
            assert!(t.screen().cell(0, 1).unwrap().combining().is_empty());
        }
    }
}

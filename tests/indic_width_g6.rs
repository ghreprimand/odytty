// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored Chakma, Javanese, Grantha, and Tai Tham terminal ownership regressions.
//! Unicode data is licensed in fixtures/unicode-indic/LICENSE-UNICODE.txt.
//! Terminal width units are distinct from Unicode grapheme boundaries.
use odytty::core::{
    Position, SearchOptions, SnapshotCaptureLimits, SnapshotEnvelope, SnapshotEnvelopeCaps,
    Terminal,
};
use odytty::selection::{CellPoint, SelectionRange, selected_text};
const BASES: [char; 4] = ['\u{11107}', '\u{a98f}', '\u{11315}', '\u{1a20}'];
const VIRAMAS: [char; 4] = ['\u{11134}', '\u{a9c0}', '\u{1134d}', '\u{1a60}'];
const FROZEN: [[&str; 2]; 4] = [
    ["\u{11107}\u{1112c}", "\u{11107}\u{11134}\u{11107}"],
    ["\u{a98f}\u{a9b4}", "\u{a98f}\u{a9c0}\u{a98f}"],
    ["\u{11315}\u{1133e}", "\u{11315}\u{1134d}\u{11315}"],
    ["\u{1a20}\u{1a63}", "\u{1a20}\u{1a60}\u{1a20}"],
];
const PROPERTIES: &str = include_str!("fixtures/unicode-indic/G6-properties.txt");
fn conjunct(s: usize) -> String {
    format!("{}{}{}", BASES[s], VIRAMAS[s], BASES[s])
}
fn cases(s: usize) -> Vec<String> {
    let mut cases: Vec<_> = FROZEN[s].iter().map(|s| (*s).to_owned()).collect();
    cases.push(conjunct(s));
    cases
}
fn restored(t: &Terminal) -> Terminal {
    let e = SnapshotEnvelope::from_terminal(t, SnapshotCaptureLimits::default());
    Terminal::from_snapshot_envelope(
        &SnapshotEnvelope::decode(&e.encode().unwrap(), SnapshotEnvelopeCaps::default()).unwrap(),
    )
    .unwrap()
}
fn owner(t: &Terminal, row: usize, col: usize, text: &str) {
    assert_eq!(t.screen().cell(row, col).unwrap().grapheme(), text);
    assert!(!t.screen().cell(row, col).unwrap().wide_continuation);
    assert!(t.screen().cell(row, col + 1).unwrap().wide_continuation);
    assert!(
        t.screen()
            .cell(row, col + 1)
            .unwrap()
            .combining()
            .is_empty()
    );
}
fn width_and_owner(s: usize) {
    for text in cases(s) {
        for wide in [false, true] {
            let mut t = Terminal::new(20, 3);
            t.set_ambiguous_wide(wide);
            t.advance(text.as_bytes());
            assert_eq!(t.screen().cursor(), Position { row: 0, column: 2 });
            owner(&t, 0, 0, &text);
            t.advance(b"X\x1b[6n");
            assert_eq!(t.screen().cell(0, 2).unwrap().ch, 'X');
            assert_eq!(t.take_host_output(), b"\x1b[1;4R");
        }
    }
}
fn streaming_and_snapshot(s: usize) {
    for text in cases(s) {
        let mut whole = Terminal::new(20, 3);
        whole.advance(text.as_bytes());
        owner(&whole, 0, 0, &text);
        for split in 0..=text.len() {
            let mut t = Terminal::new(20, 3);
            t.advance(&text.as_bytes()[..split]);
            t.advance(&text.as_bytes()[split..]);
            assert_eq!(t.snapshot().cells, whole.snapshot().cells);
            assert_eq!(t.screen().cursor(), whole.screen().cursor());
            if text.is_char_boundary(split) {
                let mut prefix = Terminal::new(20, 3);
                prefix.advance(&text.as_bytes()[..split]);
                let mut t = restored(&prefix);
                t.advance(&text.as_bytes()[split..]);
                owner(&t, 0, 0, &text);
                assert_eq!(t.screen().cursor(), whole.screen().cursor());
            }
        }
        let mut t = Terminal::new(20, 3);
        for byte in text.as_bytes() {
            t.advance(&[*byte]);
        }
        assert_eq!(t.snapshot().cells, whole.snapshot().cells);
    }
}
fn joining_and_boundaries(s: usize) {
    let b = BASES[s];
    let v = VIRAMAS[s];
    for gap in ["\u{200d}", "\x1b[31m"] {
        if s == 0 && gap == "\u{200d}" {
            continue;
        }
        let mut t = Terminal::new(20, 3);
        t.advance(format!("{b}{v}{gap}{b}").as_bytes());
        owner(
            &t,
            0,
            0,
            &format!("{b}{v}{}{b}", if gap == "\u{200d}" { gap } else { "" }),
        );
        assert_eq!(t.screen().cursor().column, 2);
    }
    let mut t = Terminal::new(20, 3);
    t.advance(format!("{b}{v}\u{200c}{b}").as_bytes());
    assert_eq!(
        t.screen().cell(0, 0).unwrap().grapheme(),
        format!("{b}{v}\u{200c}")
    );
    assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), b.to_string());
    for control in [
        "\r", "\x1b[1G", "\x1b[@", "\x1b[P", "\x1b[X", "\x1b[K", "\n",
    ] {
        let mut t = Terminal::new(20, 3);
        t.advance(format!("{b}{v}").as_bytes());
        t.advance(control.as_bytes());
        let at = t.screen().cursor();
        t.advance(b.to_string().as_bytes());
        assert_eq!(
            t.screen().cell(at.row, at.column).unwrap().grapheme(),
            b.to_string()
        );
    }
}
fn wrapping_and_insert(s: usize) {
    for text in cases(s) {
        let mut t = Terminal::new(4, 3);
        t.advance(b"ABC");
        t.advance(text.as_bytes());
        assert!(t.screen().cell(0, 3).unwrap().layout_padding);
        owner(&t, 1, 0, &text);
        assert_eq!(t.screen().cursor(), Position { row: 1, column: 2 });
        let mut t = Terminal::new(10, 2);
        t.advance(b"LR\x1b[2G\x1b[4h");
        t.advance(text.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().ch, 'L');
        owner(&t, 0, 1, &text);
        assert_eq!(t.screen().cell(0, 3).unwrap().ch, 'R');
    }
}
fn edits(s: usize) {
    for text in cases(s) {
        for cmd in ["\x1b[2GX", "\x1b[3GX", "\x1b[2G\x1b[X", "\x1b[3G\x1b[X"] {
            let mut t = Terminal::new(10, 2);
            t.advance(format!("L{text}R").as_bytes());
            owner(&t, 0, 1, &text);
            t.advance(cmd.as_bytes());
            assert_eq!(t.screen().cell(0, 0).unwrap().ch, 'L');
            assert_eq!(t.screen().cell(0, 3).unwrap().ch, 'R');
            for col in [1, 2] {
                let c = t.screen().cell(0, col).unwrap();
                assert!(!c.wide_continuation);
                assert!(c.combining().is_empty());
            }
        }
    }
}
fn history_copy_search_reflow(s: usize) {
    for text in cases(s) {
        let mut t = Terminal::new(8, 4);
        t.advance(format!("L{text}R").as_bytes());
        owner(&t, 0, 1, &text);
        for end in [1, 2] {
            assert_eq!(
                selected_text(
                    &t.snapshot(),
                    SelectionRange {
                        start: CellPoint { row: 0, column: 1 },
                        end: CellPoint {
                            row: 0,
                            column: end
                        }
                    }
                ),
                text
            );
        }
        for _ in 0..10 {
            t.advance(b"\r\n");
        }
        for width in [3, 9, 4, 12] {
            t.resize(width, 4);
            assert_eq!(t.search(&text, SearchOptions::case_sensitive()).len(), 1);
        }
        assert_eq!(
            restored(&t)
                .search(&text, SearchOptions::case_sensitive())
                .len(),
            1
        );
    }
}
fn unicode_properties(s: usize) {
    let mut spacing = 0;
    let mut consonants = 0;
    for line in PROPERTIES.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<_> = line.split(';').collect();
        if f[1].parse::<usize>().unwrap() != s + 1 {
            continue;
        }
        let cp = char::from_u32(u32::from_str_radix(f[0], 16).unwrap()).unwrap();
        let text = if f[2] == "Mc" && f[3] != "Virama" {
            spacing += 1;
            format!("{}{cp}", BASES[s])
        } else if f[3] == "Consonant" {
            consonants += 1;
            format!("{}{}{cp}", BASES[s], VIRAMAS[s])
        } else {
            continue;
        };
        let mut t = Terminal::new(20, 3);
        t.advance(text.as_bytes());
        assert_eq!(t.screen().cursor().column, 2, "{line}");
        owner(&t, 0, 0, &text);
    }
    assert!(spacing > 0 && consonants > 0);
}
fn repeated_and_bounded(s: usize) {
    let b = BASES[s];
    let v = VIRAMAS[s];
    let text = format!("{b}{v}{b}{v}{b}");
    let mut t = Terminal::new(20, 2);
    t.advance(text.as_bytes());
    owner(&t, 0, 0, &text);
    assert_eq!(t.screen().cursor().column, 2);
    let source = format!("{b}{}", format!("{v}{b}").repeat(30));
    let mut t = Terminal::new(80, 2);
    t.advance(source.as_bytes());
    let copied = t
        .snapshot()
        .cells
        .iter()
        .filter(|c| !c.wide_continuation && !c.layout_padding)
        .map(|c| c.grapheme())
        .collect::<String>();
    assert_eq!(copied.trim_end(), source);
    assert!(
        t.snapshot()
            .cells
            .iter()
            .all(|c| c.grapheme().chars().count() <= 17)
    );
}
fn application_redraw(s: usize) {
    let text = conjunct(s);
    let mut t = Terminal::new(20, 4);
    t.advance(format!("old\r\x1b[2K> {text} X").as_bytes());
    owner(&t, 0, 2, &text);
    assert_eq!(t.screen().cell(0, 5).unwrap().ch, 'X');
    t.advance(b"\x1b[?1049h");
    t.advance(format!("|{text}|{text}|\r\n").as_bytes());
    owner(&t, 0, 1, &text);
    owner(&t, 0, 4, &text);
    t.advance(b"\x1b[?1049l");
    owner(&t, 0, 2, &text);
}
macro_rules! script_tests {
    ($name:ident, $s:expr) => {
        mod $name {
            macro_rules! active {
                ($test:ident) => {
                    #[test]
                    fn $test() {
                        super::$test($s);
                    }
                };
            }
            active!(width_and_owner);
            active!(streaming_and_snapshot);
            active!(joining_and_boundaries);
            active!(wrapping_and_insert);
            active!(edits);
            active!(history_copy_search_reflow);
            active!(unicode_properties);
            active!(repeated_and_bounded);
            active!(application_redraw);
        }
    };
}
script_tests!(chakma, 0);
script_tests!(javanese, 1);
script_tests!(grantha, 2);
script_tests!(tai_tham, 3);
#[test]
fn fixture_scope_and_license() {
    let mut counts = [0; 4];
    for line in PROPERTIES.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<_> = line.split(';').collect();
        counts[f[1].parse::<usize>().unwrap() - 1] += 1;
    }
    assert_eq!(counts, [71, 90, 85, 127]);
    assert!(PROPERTIES.starts_with("# Unicode 17.0.0"));
    assert!(
        include_str!("fixtures/unicode-indic/LICENSE-UNICODE.txt").contains("UNICODE LICENSE V3")
    );
}
#[test]
fn cross_script_and_ascii_boundaries() {
    for s in 0..4 {
        let b = BASES[s];
        let v = VIRAMAS[s];
        for next in ['\u{915}', '1', 'A'] {
            let mut t = Terminal::new(20, 2);
            t.advance(format!("{b}{v}{next}").as_bytes());
            assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), next.to_string());
            assert!(!t.screen().cell(0, 1).unwrap().wide_continuation);
        }
    }
}

#[test]
fn foreign_linkers_and_nonconsonant_bases_do_not_join() {
    for b in BASES {
        let foreign = '\u{94d}';
        let mut t = Terminal::new(20, 2);
        t.advance(format!("{b}{foreign}{b}").as_bytes());
        assert_eq!(t.screen().cursor().column, 2);
        assert_eq!(
            t.screen().cell(0, 0).unwrap().grapheme(),
            format!("{b}{foreign}")
        );
        assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), b.to_string());
        assert!(!t.screen().cell(0, 1).unwrap().wide_continuation);
    }
    for (s, vowel) in ['1', '1', '1', '1'].into_iter().enumerate() {
        let b = BASES[s];
        let v = VIRAMAS[s];
        let mut t = Terminal::new(20, 2);
        t.advance(format!("{vowel}{v}{b}").as_bytes());
        assert_eq!(t.screen().cursor().column, 2);
        assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), b.to_string());
        assert!(!t.screen().cell(0, 1).unwrap().wide_continuation);
    }
}

#[test]
fn measured_and_other_script_owners_do_not_join_across_script_boundaries() {
    for s in 0..4 {
        let b = BASES[s];
        let v = VIRAMAS[s];
        for text in [format!("\u{915}\u{94d}{b}"), format!("{b}{v}\u{915}")] {
            let mut t = Terminal::new(20, 2);
            t.advance(text.as_bytes());
            assert_eq!(t.screen().cursor().column, 2);
            assert!(!t.screen().cell(0, 1).unwrap().wide_continuation);
        }
    }
}

#[test]
fn deletion_and_exact_edge_wrap_preserve_measured_scripts() {
    for s in 0..4 {
        let text = conjunct(s);
        let mut t = Terminal::new(8, 2);
        t.advance(format!("L{text}R").as_bytes());
        t.advance(b"\x1b[2G\x1b[2P");
        assert_eq!(t.screen().plain_text().trim_end(), "LR");
        let mut t = Terminal::new(4, 3);
        t.advance(format!("AB{text}").as_bytes());
        t = restored(&t);
        t.advance(b"Z");
        owner(&t, 0, 2, &text);
        assert_eq!(t.screen().cursor(), Position { row: 1, column: 1 });
    }
}

#[test]
fn chakma_maayyaa_zwj_keeps_prior_separate_owner_policy() {
    let mut t = Terminal::new(16, 2);
    t.advance("\u{11107}\u{11134}\u{200d}\u{11107}".as_bytes());
    assert_eq!(
        t.screen().cell(0, 0).unwrap().grapheme(),
        "\u{11107}\u{11134}\u{200d}"
    );
    assert_eq!(t.screen().cell(0, 1).unwrap().grapheme(), "\u{11107}");
    assert_eq!(t.screen().cursor().column, 2);
}
#[test]
fn standalone_linking_signs_do_not_promote_the_base_footprint() {
    for s in 0..4 {
        let source = format!("{}{}", BASES[s], VIRAMAS[s]);
        let mut t = Terminal::new(16, 2);
        t.advance(source.as_bytes());
        assert_eq!(t.screen().cell(0, 0).unwrap().grapheme(), source);
        assert_eq!(t.screen().cursor().column, 1);
        assert!(!t.screen().cell(0, 1).unwrap().wide_continuation);
    }
}

#[test]
fn chakma_virama_keeps_zwj_links_separate_from_maayyaa_policy() {
    let text = "\u{11107}\u{11133}\u{200d}\u{11107}";
    for split in 0..=text.len() {
        let mut t = Terminal::new(16, 2);
        t.advance(&text.as_bytes()[..split]);
        t.advance(&text.as_bytes()[split..]);
        owner(&t, 0, 0, text);
        assert_eq!(t.screen().cursor().column, 2);
    }
}

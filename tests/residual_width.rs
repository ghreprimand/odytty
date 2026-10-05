// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored minimal retained-owner regressions.
use odytty::core::Terminal;

fn one_owner(text: &str) {
    let mut terminal = Terminal::new(20, 3);
    terminal.advance(text.as_bytes());
    assert_eq!(terminal.screen().cursor().column, 2, "{text:?}");
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), text);
    assert!(terminal.screen().cell(0, 1).unwrap().wide_continuation);
}

#[test]
fn tai_tham_spacing_vowel_before_stacker_retains_consonant() {
    one_owner("\u{1a20}\u{1a6e}\u{1a60}\u{1a2f}");
}

#[test]
fn tai_tham_spacing_vowel_after_stacked_consonant_retains_owner() {
    one_owner("\u{1a34}\u{1a6e}\u{1a60}\u{1a48}\u{1a71}");
}

#[test]
fn reference_linker_width_does_not_override_nonconsonant_boundaries() {
    for text in [
        "\u{1a63}\u{1a60}\u{1a32}\u{1a71}",
        "\u{0d7b}\u{0d4d}\u{0d31}\u{0d46}",
    ] {
        let mut terminal = Terminal::new(20, 3);
        terminal.advance(text.as_bytes());
        assert_eq!(terminal.screen().cursor().column, 3);
        assert!(!terminal.screen().cell(0, 1).unwrap().wide_continuation);
        assert!(terminal.screen().cell(0, 2).unwrap().wide_continuation);
    }
}

fn columns(text: &str) -> usize {
    let mut terminal = Terminal::new(40, 3);
    terminal.advance(text.as_bytes());
    terminal.screen().cursor().column
}

#[test]
fn tai_tham_every_spacing_sign_before_sakot_keeps_one_owner() {
    let signs = ['\u{1a55}', '\u{1a57}', '\u{1a61}', '\u{1a63}', '\u{1a64}']
        .into_iter()
        .chain('\u{1a6d}'..='\u{1a72}');
    for sign in signs {
        for (base, linked) in [('\u{1a20}', '\u{1a2f}'), ('\u{1a49}', '\u{1a3e}')] {
            one_owner(&format!("{base}{sign}\u{1a60}{linked}"));
            // Zero-width marks between the sign and SAKOT do not end the scan.
            one_owner(&format!("{base}{sign}\u{1a68}\u{1a60}{linked}"));
        }
    }
}

#[test]
fn tai_tham_sign_rule_stays_bounded() {
    // A sign after SAKOT, a ZWNJ request, and a foreign base keep the split.
    assert_eq!(columns("\u{1a20}\u{1a60}\u{1a6e}\u{1a2f}"), 3);
    assert_eq!(columns("\u{1a20}\u{1a6e}\u{1a60}\u{200c}\u{1a2f}"), 3);
    assert_eq!(columns("a\u{1a6e}\u{1a60}\u{1a2f}"), 3);
    // Other scripts keep a retained spacing sign as the linker-scan boundary.
    assert_eq!(columns("\u{915}\u{93e}\u{94d}\u{915}"), 3);
    assert_eq!(columns("\u{1780}\u{17b6}\u{17d2}\u{1780}"), 3);
    assert_eq!(columns("\u{1000}\u{102b}\u{1039}\u{1000}"), 3);
}

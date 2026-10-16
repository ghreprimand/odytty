// SPDX-License-Identifier: GPL-3.0-only
//! Grapheme cluster mode 2027 (Terminal Unicode Core): owners always follow
//! extended grapheme clusters with the documented terminal profile, so DECRQM
//! reports the mode permanently set and DECSET/DECRST change nothing.
use odytty::core::Terminal;

const REPORT: &[u8] = b"\x1b[?2027;3$y";

fn report(terminal: &mut Terminal) -> Vec<u8> {
    terminal.take_host_output();
    terminal.advance(b"\x1b[?2027$p");
    terminal.take_host_output()
}

#[test]
fn decrqm_2027_reports_permanently_set() {
    let mut terminal = Terminal::new(10, 2);
    assert_eq!(report(&mut terminal), REPORT);
}

#[test]
fn decset_and_decrst_2027_leave_report_at_3() {
    let mut terminal = Terminal::new(10, 2);
    for sequence in [&b"\x1b[?2027l"[..], b"\x1b[?2027h", b"\x1b[?2027;2026l"] {
        terminal.advance(sequence);
        assert_eq!(report(&mut terminal), REPORT, "{sequence:?}");
    }
    // Clustering itself is unchanged by a reset request.
    terminal.advance("\x1b[?2027l\x1b[H\u{1b13}\u{1b3e}".as_bytes());
    assert_eq!(
        terminal.screen().cell(0, 0).unwrap().grapheme(),
        "\u{1b13}\u{1b3e}"
    );
}

#[test]
fn ris_and_decstr_leave_2027_at_3() {
    let mut terminal = Terminal::new(10, 2);
    for reset in [&b"\x1bc"[..], b"\x1b[!p"] {
        terminal.advance(b"\x1b[?2027l");
        terminal.advance(reset);
        assert_eq!(report(&mut terminal), REPORT, "{reset:?}");
    }
}

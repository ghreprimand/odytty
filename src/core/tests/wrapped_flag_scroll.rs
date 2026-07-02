// SPDX-License-Identifier: GPL-3.0-only
//! C16 regression tests: DL/IL/scroll-region/RI/erase ops must sever the
//! soft-wrap (`Line::wrapped`) chain at every seam their row shuffle creates.
//!
//! `wrapped == true` on row N is a promise that row N+1 is the physical
//! continuation of the same logical line; `Screen::resize` (reflow) joins the
//! two back together. Every operation that removes, replaces, or displaces
//! row N+1 while keeping row N breaks that promise. Before the fix the flag
//! survived the shuffle, so the next width-changing resize would fuse
//! UNRELATED rows into one logical line — visible content corruption.
//!
//! Each test builds a soft-wrapped pair (a 15-char line on a 10-column grid:
//! `aaaaaaaaaa` wrapped + `bbbbb`), lets one op shuffle the continuation away,
//! then widens the grid and asserts the reflowed text did NOT fuse across the
//! severed seam.

use super::*;

fn visible_text(terminal: &Terminal) -> String {
    terminal
        .screen()
        .plain_text()
        .trim_end_matches('\n')
        .to_string()
}

/// A fresh 10x4 grid holding the wrapped pair on rows 0-1.
fn wrapped_pair() -> Terminal {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance(b"aaaaaaaaaabbbbb"); // row 0 (wrapped) + row 1
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\nbbbbb");
    terminal
}

/// DL (CSI M) deleting the continuation row: the wrapped row above the
/// shuffle must not claim the row that scrolled up into the gap.
#[test]
fn delete_lines_severs_soft_wrap_above_the_shuffle() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[3;1HCCC"); // row 2 content that will shuffle up
    terminal.advance(b"\x1b[2;1H\x1b[M"); // cursor to row 1, delete it

    terminal.resize(20, 4);
    // Pre-fix: rows fused into "aaaaaaaaaaCCC".
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\nCCC");
}

/// IL (CSI L) pushing the continuation row away: the wrapped row above must
/// not claim the fresh blank inserted at the cursor.
#[test]
fn insert_lines_severs_soft_wrap_above_the_shuffle() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[2;1H\x1b[L"); // blank inserted at row 1

    terminal.resize(20, 4);
    // Pre-fix: row 0 swallowed the blank row, collapsing the gap to
    // "aaaaaaaaaa\nbbbbb". The physical blank must survive reflow.
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\n\nbbbbb");
}

/// CSI S (SU) inside a DECSTBM region whose top sits right below a wrapped
/// row: scrolling the continuation out of the region must sever the seam
/// above the region.
#[test]
fn scroll_region_up_severs_soft_wrap_above_the_region() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[3;1HCCC"); // row 2
    terminal.advance(b"\x1b[2;4r\x1b[S\x1b[r"); // region rows 1..3, scroll up 1

    terminal.resize(20, 4);
    // Pre-fix: "bbbbb" left the region, "CCC" moved up under the wrapped row
    // and got fused into "aaaaaaaaaaCCC".
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\nCCC");
}

/// CSI T (SD): both seams. Top seam: the wrapped row above the region must
/// not claim the inserted blank. Bottom seam: a wrapped row displaced to the
/// region bottom whose continuation was discarded must not claim the
/// (unmoved) row below the region.
#[test]
fn scroll_region_down_severs_soft_wrap_at_both_seams() {
    // Top seam.
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[2;4r\x1b[T\x1b[r"); // region rows 1..3, scroll down 1

    terminal.resize(20, 4);
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\n\nbbbbb");

    // Bottom seam: region covers exactly the wrapped pair; SD discards the
    // continuation ("bbbbb") and moves the wrapped row to the region bottom,
    // directly above unrelated row-2 content.
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[3;1HCCC"); // row 2, outside the coming region
    terminal.advance(b"\x1b[1;2r\x1b[T\x1b[r"); // region rows 0..1, scroll down 1

    terminal.resize(20, 4);
    // Pre-fix: the displaced wrapped row fused with "CCC".
    assert_eq!(visible_text(&terminal), "\naaaaaaaaaa\nCCC");
}

/// The linefeed-at-region-bottom scroll path (`scroll_up_region`): scrolling
/// the region must sever the seam above the region top when the region's
/// first row (a wrap continuation) is discarded.
#[test]
fn linefeed_region_scroll_severs_soft_wrap_above_the_region() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[3;1HCCC"); // row 2
    terminal.advance(b"\x1b[2;4r"); // region rows 1..3
    terminal.advance(b"\x1b[4;1HDDD\n"); // LF at region bottom → scroll
    terminal.advance(b"\x1b[r");

    terminal.resize(20, 4);
    // Post-scroll rows: aaaaaaaaaa | CCC | DDD | blank. Pre-fix the wrapped
    // row fused with "CCC".
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\nCCC\nDDD");
}

/// RI (ESC M) at the region top inserts a blank under a wrapped row above the
/// region: the seam must be severed.
#[test]
fn reverse_index_severs_soft_wrap_above_the_region() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[2;4r\x1b[2;1H\x1bM\x1b[r"); // RI at region top (row 1)

    terminal.resize(20, 4);
    // Pre-fix: row 0 swallowed the inserted blank, pulling "bbbbb" back up.
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\n\nbbbbb");
}

/// EL2 (CSI 2K) replacing the continuation row with a blank: the wrapped row
/// above must not claim the blank.
#[test]
fn erase_line_full_severs_soft_wrap_above() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[3;1HCCC"); // row 2
    terminal.advance(b"\x1b[2;1H\x1b[2K"); // blank out row 1 in place

    terminal.resize(20, 4);
    // Pre-fix: row 0 joined the blank (trailing spaces vanish on trim), so the
    // physical blank row disappeared from the reflowed layout.
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\n\nCCC");
}

/// EL0 (CSI K) erasing a wrapped row through its right edge destroys the
/// content flow into the continuation: the flag must clear so reflow does not
/// fuse the remnant with the next row.
#[test]
fn erase_line_from_cursor_severs_the_rows_own_soft_wrap() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[1;5H\x1b[K"); // row 0, col 4: erase to right edge

    terminal.resize(20, 4);
    // Pre-fix: "aaaa" + blanks + "bbbbb" fused into one padded line.
    assert_eq!(visible_text(&terminal), "aaaa\nbbbbb");
}

/// NF7: ECH (CSI Ps X) whose clamped count reaches the right edge destroys
/// the content flow into the continuation row, exactly like EL0 — the row's
/// own wrapped flag must clear.
#[test]
fn erase_chars_through_right_edge_severs_the_rows_own_soft_wrap() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[1;5H\x1b[6X"); // row 0, col 4: erase 6 cells → cols 4..10

    terminal.resize(20, 4);
    // Pre-fix: "aaaa" + blanks + "bbbbb" fused into one padded line.
    assert_eq!(visible_text(&terminal), "aaaa\nbbbbb");
}

/// NF7 control: an ECH that stops short of the right edge is a purely
/// interior blank — the soft-wrap join must survive so reflow still fuses
/// the pair (with the interior blanks preserved).
#[test]
fn erase_chars_short_of_right_edge_keeps_soft_wrap() {
    let mut terminal = wrapped_pair();
    terminal.advance(b"\x1b[1;5H\x1b[3X"); // row 0, cols 4..7 blanked, edge intact

    terminal.resize(20, 4);
    assert_eq!(visible_text(&terminal), "aaaa   aaabbbbb");
}

/// NF6: ED2 (CSI 2J) replaces the visible screen wholesale, but a trailing
/// OPEN scrollback logical line still claimed row 0 as its continuation.
/// Reflow then fused scrolled-off history with whatever was printed after
/// the clear.
#[test]
fn erase_display_full_severs_trailing_scrollback_wrap() {
    let mut terminal = Terminal::new(10, 4);
    // One 50-char logical line: 5 physical rows on a 4-row grid, so the first
    // wrapped row scrolls off as an open scrollback line continuing into
    // visible row 0.
    terminal.advance("a".repeat(50).as_bytes());
    terminal.advance(b"\x1b[2J\x1b[1;1HXXX");

    terminal.resize(30, 4);
    // Pre-fix: the scrollback tail fused with the fresh content into
    // "aaaaaaaaaaXXX". Post-fix the history stays its own hard-terminated
    // line (visible here because the 2-row reflow result is bottom-anchored
    // into the 4-row window).
    assert_eq!(visible_text(&terminal), "aaaaaaaaaa\nXXX");
}

/// A 50-char logical line on a 10x4 grid: one physical row scrolls off as an
/// OPEN scrollback line whose `wrapped` flag claims visible row 0 as its
/// continuation. The NF10 seam fixture.
fn scrollback_tail_into_row0() -> Terminal {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance("a".repeat(50).as_bytes());
    terminal
}

/// NF10 seam (a): ED1 (CSI 1J) with the cursor below row 0 replaces row 0
/// wholesale — the trailing open scrollback line must not keep claiming it.
#[test]
fn erase_display_above_cursor_severs_trailing_scrollback_wrap() {
    let mut terminal = scrollback_tail_into_row0();
    terminal.advance(b"\x1b[2;1H\x1b[1J"); // cursor row 1: rows above replaced

    terminal.resize(30, 4);
    // Post-fix: history stays its own line, the blanked row 0 survives as a
    // physical blank, and rows 1-3 reflow as their own 30-char logical line
    // (minus the cell ED1 erased at the cursor). Pre-fix the scrollback tail
    // swallowed blank row 0 and the blank vanished from the layout.
    assert_eq!(
        visible_text(&terminal),
        format!("aaaaaaaaaa\n\n {}", "a".repeat(29))
    );
}

/// NF10 seam (b): EL2 (CSI 2K) at row 0 replaces the row with a fresh blank;
/// the guard `cursor.row > 0` skipped the predecessor entirely, leaving the
/// scrollback tail claiming the blank.
#[test]
fn erase_line_full_at_row_zero_severs_trailing_scrollback_wrap() {
    let mut terminal = scrollback_tail_into_row0();
    terminal.advance(b"\x1b[1;1H\x1b[2K");

    terminal.resize(30, 4);
    // Post-fix: history / physical blank / 30-char line. Pre-fix the blank
    // fused into the tail and disappeared.
    assert_eq!(
        visible_text(&terminal),
        format!("aaaaaaaaaa\n\n{}", "a".repeat(30))
    );
}

/// NF10 seam (c): DL (CSI M) at row 0 deletes the scrollback tail's
/// continuation — the tail must not claim the row that shuffles up.
#[test]
fn delete_lines_at_row_zero_severs_trailing_scrollback_wrap() {
    let mut terminal = scrollback_tail_into_row0();
    terminal.advance(b"\x1b[1;1H\x1b[M");

    terminal.resize(30, 4);
    // Post-fix: history stays a 10-char line above the surviving 30-char
    // line. Pre-fix they fused into one 40-char logical line.
    assert_eq!(
        visible_text(&terminal),
        format!("aaaaaaaaaa\n{}", "a".repeat(30))
    );
}

/// NF10 seam (c): IL (CSI L) at row 0 inserts a blank between the scrollback
/// tail and its displaced continuation.
#[test]
fn insert_lines_at_row_zero_severs_trailing_scrollback_wrap() {
    let mut terminal = scrollback_tail_into_row0();
    terminal.advance(b"\x1b[1;1H\x1b[L");

    terminal.resize(30, 4);
    // Post-fix: history / physical blank / remaining 30 chars (the last
    // visible row was pushed out). Pre-fix the tail swallowed the blank.
    assert_eq!(
        visible_text(&terminal),
        format!("aaaaaaaaaa\n\n{}", "a".repeat(30))
    );
}

/// NF10 seam (c): CSI S with a DECSTBM region whose top is row 0 discards
/// the tail's continuation out of the region.
#[test]
fn scroll_region_up_at_row_zero_severs_trailing_scrollback_wrap() {
    let mut terminal = scrollback_tail_into_row0();
    terminal.advance(b"\x1b[1;2r\x1b[S\x1b[r"); // region rows 0..1, scroll up 1

    terminal.resize(30, 4);
    // Row 0 discarded, row 1 moved up, blank at row 1; rows 2-3 unmoved.
    // The moved row starts its own logical line (scroll_region_up severs the
    // shifted row's own flag at the bottom seam), rows 2-3 stay joined.
    assert_eq!(
        visible_text(&terminal),
        format!("aaaaaaaaaa\naaaaaaaaaa\n\n{}", "a".repeat(20))
    );
}

/// NF10 alt-screen control (mirrors the NF6 pin): the same row-0 ops issued
/// on the ALT screen must NOT sever the primary scrollback tail — it still
/// validly continues into the SAVED primary row 0.
#[test]
fn row_zero_ops_on_alt_screen_keep_scrollback_wrap() {
    let mut terminal = scrollback_tail_into_row0();
    terminal.advance(b"\x1b[?1049h\x1b[1;1H\x1b[M\x1b[L\x1b[2K\x1b[2;1H\x1b[1J\x1b[?1049l");

    terminal.resize(30, 4);
    // The 50-char logical line rejoins across the scrollback/visible seam.
    assert_eq!(
        visible_text(&terminal),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\naaaaaaaaaaaaaaaaaaaa"
    );
}

/// NF6 control: ED2 issued on the ALT screen must NOT sever the primary
/// scrollback tail — it still validly continues into the saved primary
/// row 0, and reflow after returning must rejoin the logical line.
#[test]
fn erase_display_on_alt_screen_keeps_scrollback_wrap() {
    let mut terminal = Terminal::new(10, 4);
    terminal.advance("a".repeat(50).as_bytes());
    terminal.advance(b"\x1b[?1049h\x1b[2J\x1b[?1049l");

    terminal.resize(30, 4);
    // The 50-char logical line rejoins across the scrollback/visible seam.
    assert_eq!(
        visible_text(&terminal),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\naaaaaaaaaaaaaaaaaaaa"
    );
}

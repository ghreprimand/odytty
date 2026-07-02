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

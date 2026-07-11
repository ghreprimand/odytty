// SPDX-License-Identifier: GPL-3.0-only
//! Recovery guards for stale DECSTBM regions left by an interrupted TUI.

use super::*;

fn osc133_prompt_start() -> &'static [u8] {
    b"\x1b]133;A\x07"
}

#[test]
fn partial_region_with_cursor_below_margin_strands_output() {
    let mut terminal = Terminal::new(20, 10);

    terminal.advance(b"\x1b[1;9r\x1b[10;1H$ cmd\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");

    assert_eq!(terminal.screen().cursor(), Position { row: 9, column: 4 });
    assert_eq!(terminal.screen().scrollback_len(), 0);
    assert_eq!(terminal.screen().plain_text().lines().last(), Some("fivee"));
}

#[test]
fn row_resize_clears_active_partial_region_and_restores_scrolling() {
    let mut terminal = Terminal::new(20, 10);
    terminal.advance(b"\x1b[1;9r\x1b[10;1Hstranded\r\nagain");

    terminal.resize(20, 11);

    assert_eq!(terminal.snapshot_layout_state().scroll_region, None);
    for line in 0..12 {
        terminal.advance(format!("\r\nline-{line}").as_bytes());
    }
    assert!(terminal.screen().scrollback_len() > 0);
}

#[test]
fn column_only_resize_keeps_active_scroll_region() {
    let mut terminal = Terminal::new(20, 10);
    terminal.advance(b"\x1b[1;9r");
    let region = terminal.snapshot_layout_state().scroll_region;

    terminal.resize(24, 10);

    assert_eq!(terminal.snapshot_layout_state().scroll_region, region);
}

#[test]
fn row_resize_clears_stored_primary_scroll_region() {
    let mut terminal = Terminal::new(20, 10);
    terminal.advance(b"\x1b[1;9r\x1b[?1049h");

    terminal.resize(20, 11);
    terminal.advance(b"\x1b[?1049l");

    assert_eq!(terminal.snapshot_layout_state().scroll_region, None);
}

#[test]
fn prompt_start_clears_partial_region_on_primary_screen() {
    let mut terminal = Terminal::new(20, 10);
    terminal.advance(b"\x1b[1;9r");

    terminal.advance(osc133_prompt_start());

    assert_eq!(terminal.snapshot_layout_state().scroll_region, None);
}

#[test]
fn prompt_start_keeps_full_screen_region() {
    let mut terminal = Terminal::new(20, 10);
    terminal.advance(b"\x1b[1;10r");
    let region = terminal.snapshot_layout_state().scroll_region;

    terminal.advance(osc133_prompt_start());

    assert_eq!(terminal.snapshot_layout_state().scroll_region, region);
}

#[test]
fn prompt_start_keeps_partial_region_on_alternate_screen() {
    let mut terminal = Terminal::new(20, 10);
    terminal.advance(b"\x1b[?1049h\x1b[1;9r");
    let region = terminal.snapshot_layout_state().scroll_region;

    terminal.advance(osc133_prompt_start());

    assert_eq!(terminal.snapshot_layout_state().scroll_region, region);
}

#[test]
fn leaving_alternate_screen_does_not_leak_its_scroll_region() {
    let mut terminal = Terminal::new(20, 10);
    terminal.advance(b"\x1b[?1049h\x1b[1;9r\x1b[?1049l");

    assert_eq!(terminal.snapshot_layout_state().scroll_region, None);
}

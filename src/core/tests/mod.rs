// SPDX-License-Identifier: GPL-3.0-only
//! Behavioral tests for the terminal core: printing, SGR, cursor movement,
//! erase/scroll, alternate screen, scrollback/reflow, OSC titles, mouse-mode
//! tracking, wide/combining Unicode. Drives the public `Terminal`/`Screen` API
//! plus the crate-internal `MAX_COMBINING` bound.

use super::*;

mod bell;
mod chars_unicode;
mod erase_scroll;
mod kitty_keyboard;
mod osc_clipboard_colors;
mod osc_cwd;
mod osc_prompt;
mod rect;
mod repeat_tab_reflow;
mod reporting;
mod reset_osc_mouse;
mod sgr_cursor;
mod visible_search_rows;

pub(super) fn assert_blank_with_background(
    terminal: &Terminal,
    row: usize,
    column: usize,
    background: Color,
) {
    let cell = terminal.screen().cell(row, column).unwrap();
    assert_eq!(cell.ch, ' ');
    let mut expected = Attrs::default();
    expected.background = background;
    assert_eq!(cell.attrs, expected);
    assert!(!cell.wide_continuation);
}

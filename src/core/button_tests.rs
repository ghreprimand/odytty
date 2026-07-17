// SPDX-License-Identifier: GPL-3.0-only
//! Model-level coverage for the button protocol (B1): dispatch gating, span
//! anchoring, block/sticky lifetime, scroll-out flat-coordinate merge, reflow
//! re-projection, flood bounds, and reset behavior. Parser and table unit
//! tests live in `super::button`; these drive the full [`Terminal`] feed path.

use super::button::{
    ButtonIcon, ButtonScope, ButtonSpan, ButtonState, MAX_BUTTON_ENTRIES, MAX_BUTTON_SPANS_PER_LINE,
};
use super::screen::Terminal;
use super::types::{Attrs, Cell};

fn enabled_terminal(columns: usize, rows: usize) -> Terminal {
    let mut term = Terminal::new(columns, rows);
    term.set_buttons_enabled(true);
    term
}

fn osc(payload: &str) -> Vec<u8> {
    format!("\x1b]{payload}\x07").into_bytes()
}

fn feed(term: &mut Terminal, bytes: &[u8]) {
    term.advance(bytes);
}

fn feed_osc(term: &mut Terminal, payload: &str) {
    let bytes = osc(payload);
    feed(term, &bytes);
}

const T2_DEFINE: &str = "133;P;odytty-button;code=7";
const T2_DEFINE_STICKY: &str = "133;P;odytty-button;code=7;scope=sticky";
const T2_END: &str = "133;P;odytty-button;end";

// ---------------------------------------------------------------------------
// Gating
// ---------------------------------------------------------------------------

#[test]
fn gate_off_parses_and_ignores_both_spellings() {
    let mut term = Terminal::new(20, 5); // master gate off (default)
    feed_osc(&mut term, "1337;Button=type=custom;code=42;icon=star");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"Retry");
    feed_osc(&mut term, T2_END);
    assert_eq!(term.button_entry_count(), 0, "gate off: no table growth");
    assert!(
        term.screen().visible_row_button_spans(0).is_empty(),
        "gate off: no spans"
    );
    // The label still printed as ordinary text (Tier 2's degrade story).
    let snapshot = term.screen().snapshot();
    let row: String = (0..5).map(|col| snapshot.cells[col].ch).collect();
    assert_eq!(row, "Retry");
}

#[test]
fn iterm_compat_sub_gate_controls_tier1_only() {
    let mut term = enabled_terminal(20, 5);
    term.set_buttons_iterm_compat(false);
    feed_osc(&mut term, "1337;Button=type=custom;code=42;icon=star");
    assert_eq!(term.button_entry_count(), 0, "tier 1 refused");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"ok");
    feed_osc(&mut term, T2_END);
    assert_eq!(term.button_entry_count(), 1, "tier 2 unaffected");
}

#[test]
fn sticky_sub_gate_downgrades_to_block_scope() {
    let mut term = enabled_terminal(20, 5);
    term.set_buttons_sticky(false);
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    let span = term.screen().visible_row_button_spans(0)[0];
    let entry = term.screen().button_table().get(span.id).unwrap();
    assert_eq!(entry.scope, ButtonScope::Block);
}

// ---------------------------------------------------------------------------
// Tier 1 anchoring
// ---------------------------------------------------------------------------

#[test]
fn tier1_define_anchors_a_point_span_at_the_cursor() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"ab");
    feed_osc(&mut term, "1337;Button=type=custom;code=42;icon=star.fill");
    let spans = term.screen().visible_row_button_spans(0);
    assert_eq!(
        spans,
        &[ButtonSpan {
            id: spans[0].id,
            start_col: 2,
            len: 0
        }]
    );
    let entry = term.screen().button_table().get(spans[0].id).unwrap();
    assert_eq!(entry.code, 42);
    assert_eq!(entry.icon, ButtonIcon::Star);
    assert_eq!(entry.scope, ButtonScope::Sticky);
    assert_eq!(entry.state, ButtonState::Live);
}

#[test]
fn tier1_empty_code_form_invalidates_every_button() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, "1337;Button=type=custom;code=1");
    feed_osc(&mut term, "1337;Button=type=custom");
    let spans = term.screen().visible_row_button_spans(0);
    assert_eq!(spans.len(), 1, "the gray anchor keeps its span");
    let entry = term.screen().button_table().get(spans[0].id).unwrap();
    assert_eq!(entry.state, ButtonState::Invalidated);
}

// ---------------------------------------------------------------------------
// Tier 2 runs
// ---------------------------------------------------------------------------

#[test]
fn tier2_run_stamps_the_bracketed_label_as_a_span() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"$ ");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"Retry");
    feed_osc(&mut term, T2_END);
    let spans = term.screen().visible_row_button_spans(0);
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].start_col, spans[0].len), (2, 5));
    let entry = term.screen().button_table().get(spans[0].id).unwrap();
    assert_eq!(entry.code, 7);
    assert_eq!(
        entry.scope,
        ButtonScope::Block,
        "block is the default scope"
    );
    assert_eq!(entry.state, ButtonState::Live);
}

#[test]
fn tier2_wrapped_run_produces_one_segment_per_row() {
    let mut term = enabled_terminal(6, 5);
    feed(&mut term, b"$ ");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"Retry all"); // 9 cells from col 2: wraps at width 6
    feed_osc(&mut term, T2_END);
    let row0 = term.screen().visible_row_button_spans(0).to_vec();
    let row1 = term.screen().visible_row_button_spans(1).to_vec();
    assert_eq!((row0[0].start_col, row0[0].len), (2, 4));
    assert_eq!((row1[0].start_col, row1[0].len), (0, 5));
    assert_eq!(row0[0].id, row1[0].id, "one button, two row segments");
    assert_eq!(term.button_entry_count(), 1);
}

#[test]
fn tier2_empty_label_run_degrades_to_a_point_anchor() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"x");
    feed_osc(&mut term, T2_DEFINE);
    feed_osc(&mut term, T2_END);
    let spans = term.screen().visible_row_button_spans(0);
    assert_eq!((spans[0].start_col, spans[0].len), (1, 0));
    assert_eq!(term.button_entry_count(), 1);
}

#[test]
fn superseding_define_abandons_the_open_run_and_frees_the_pending_entry() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, "133;P;odytty-button;code=1");
    feed(&mut term, b"one");
    // A second define before `end`: the first run is abandoned; its pending
    // (span-less) entry frees.
    feed_osc(&mut term, "133;P;odytty-button;code=2");
    feed(&mut term, b"two");
    feed_osc(&mut term, T2_END);
    assert_eq!(term.button_entry_count(), 1);
    let spans = term.screen().visible_row_button_spans(0);
    assert_eq!(spans.len(), 1);
    let entry = term.screen().button_table().get(spans[0].id).unwrap();
    assert_eq!(entry.code, 2);
}

// ---------------------------------------------------------------------------
// Lifetime: block boundaries, sticky ring-coupling
// ---------------------------------------------------------------------------

#[test]
fn block_scope_invalidates_at_the_next_prompt_start() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    feed_osc(&mut term, "133;A"); // next prompt starts: the block ended
    let spans = term.screen().visible_row_button_spans(0);
    let entry = term.screen().button_table().get(spans[0].id).unwrap();
    assert_eq!(entry.state, ButtonState::Invalidated);
}

#[test]
fn block_scope_invalidates_at_command_done() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    feed_osc(&mut term, "133;D;0");
    let spans = term.screen().visible_row_button_spans(0);
    let entry = term.screen().button_table().get(spans[0].id).unwrap();
    assert_eq!(entry.state, ButtonState::Invalidated);
}

#[test]
fn sticky_scope_survives_block_boundaries() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    feed_osc(&mut term, "133;A");
    feed_osc(&mut term, "133;D;0");
    let spans = term.screen().visible_row_button_spans(0);
    let entry = term.screen().button_table().get(spans[0].id).unwrap();
    assert_eq!(entry.state, ButtonState::Live);
}

#[test]
fn sticky_refcount_frees_when_the_line_leaves_the_ring() {
    let mut term = enabled_terminal(20, 3);
    term.set_scrollback_limit(2);
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    assert_eq!(term.button_entry_count(), 1);
    // Scroll the button's line off screen and out of the 2-line ring.
    for _ in 0..8 {
        feed(&mut term, b"\r\nfiller");
    }
    assert_eq!(
        term.button_entry_count(),
        0,
        "the entry must free when its last referencing line is evicted"
    );
}

#[test]
fn invalidated_button_stays_gray_until_its_lines_leave() {
    let mut term = enabled_terminal(20, 3);
    term.set_scrollback_limit(2);
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    feed_osc(&mut term, "133;P;odytty-button;invalidate;code=7");
    assert_eq!(
        term.button_entry_count(),
        1,
        "a referenced dead entry keeps its slot for the gray render"
    );
    for _ in 0..8 {
        feed(&mut term, b"\r\nfiller");
    }
    assert_eq!(term.button_entry_count(), 0);
}

#[test]
fn ed3_scrollback_clear_frees_scrolled_out_sticky_buttons() {
    let mut term = enabled_terminal(20, 3);
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    for _ in 0..5 {
        feed(&mut term, b"\r\nfiller");
    }
    assert_eq!(
        term.button_entry_count(),
        1,
        "still referenced from the ring"
    );
    // ED2 clears the visible rows, ED3 the scrollback: no reference survives.
    feed(&mut term, b"\x1b[2J\x1b[3J");
    assert_eq!(term.button_entry_count(), 0);
}

// ---------------------------------------------------------------------------
// Scroll-out merge + projection
// ---------------------------------------------------------------------------

#[test]
fn scrolled_out_spans_merge_into_flat_logical_coordinates() {
    let mut term = enabled_terminal(10, 3);
    feed(&mut term, b"$ ");
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"Retry all now"); // 13 cells from col 2: wraps once
    feed_osc(&mut term, T2_END);
    // Push the wrapped pair fully into scrollback.
    feed(&mut term, b"\r\na\r\nb\r\nc");
    let store = term.screen().scrollback_store();
    assert!(store.logical_len() >= 1);
    let spans = store.logical_button_spans(0);
    // "Retry all now" is 13 cells from col 2: row 0 held cols 2..10
    // (8 cells), row 1 cols 0..5 (5 cells) — flat: [2,10) + [10,15).
    assert_eq!(spans.len(), 2);
    assert_eq!((spans[0].start_col, spans[0].len), (2, 8));
    assert_eq!((spans[1].start_col, spans[1].len), (10, 5));
    assert_eq!(term.button_entry_count(), 1);
}

#[test]
fn projection_reanchors_spans_across_width_changes() {
    // Direct Scrollback-level check, extending the prompt-mark projection
    // tests to column ranges: a flat span re-projects to the right row
    // segments at any width.
    use super::button::ButtonId;
    use super::screen::Line;
    use super::scrollback::Scrollback;
    use std::num::NonZeroU32;

    let cells: Vec<Cell> = "0123456789ab"
        .chars()
        .map(|ch| Cell::new(ch, Attrs::default()))
        .collect();
    let mut row = Line::unwrapped(cells);
    let id = ButtonId::new(NonZeroU32::new(9).unwrap());
    row.button_spans.push(ButtonSpan {
        id,
        start_col: 3,
        len: 7, // flat cells [3, 10)
    });
    let mut sb = Scrollback::new();
    sb.push_row(row);

    {
        // Width 4: rows [0..4) [4..8) [8..12). Span [3,10) → (0,[3,1]) (1,[0,4]) (2,[0,2]).
        let rows = sb.physical(4);
        let collect: Vec<(usize, usize, usize)> = rows
            .iter()
            .enumerate()
            .flat_map(|(r, line)| {
                line.button_spans
                    .iter()
                    .map(move |s| (r, s.start_col, s.len))
            })
            .collect();
        assert_eq!(collect, vec![(0, 3, 1), (1, 0, 4), (2, 0, 2)]);
    }
    {
        // Width 20: one row, span intact.
        let rows = sb.physical(20);
        let collect: Vec<(usize, usize, usize)> = rows
            .iter()
            .enumerate()
            .flat_map(|(r, line)| {
                line.button_spans
                    .iter()
                    .map(move |s| (r, s.start_col, s.len))
            })
            .collect();
        assert_eq!(collect, vec![(0, 3, 7)]);
    }
}

#[test]
fn resize_reprojects_live_spans_and_rebuilds_refcounts() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"$ ");
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"Retry");
    feed_osc(&mut term, T2_END);
    term.resize(8, 5);
    assert_eq!(
        term.button_entry_count(),
        1,
        "the re-projected spans must keep the entry referenced"
    );
    // The label coverage survives the re-wrap: total span length unchanged.
    let screen = term.screen();
    let mut total = 0usize;
    for row in 0..5 {
        for span in screen.visible_row_button_spans(row) {
            total += span.len;
        }
    }
    let store = screen.scrollback_store();
    for idx in 0..store.logical_len() {
        for span in store.logical_button_spans(idx) {
            total += span.len;
        }
    }
    assert_eq!(total, 5, "span coverage preserved across resize");
    term.resize(20, 5);
    assert_eq!(term.button_entry_count(), 1);
}

// ---------------------------------------------------------------------------
// Floods and caps
// ---------------------------------------------------------------------------

#[test]
fn per_line_span_cap_bounds_a_single_line_flood() {
    let mut term = enabled_terminal(80, 5);
    for code in 1..=(MAX_BUTTON_SPANS_PER_LINE as u32 + 10) {
        feed_osc(&mut term, &format!("1337;Button=type=custom;code={code}"));
    }
    assert_eq!(
        term.screen().visible_row_button_spans(0).len(),
        MAX_BUTTON_SPANS_PER_LINE,
        "a line stops accepting spans at the cap"
    );
}

#[test]
fn distinct_code_flood_is_bounded_and_refused_at_the_ceiling() {
    let mut term = enabled_terminal(80, 4);
    term.set_scrollback_limit(0); // unbounded history: nothing evicts
    let flood = MAX_BUTTON_ENTRIES as u32 + 64;
    for code in 1..=flood {
        feed_osc(
            &mut term,
            &format!("133;P;odytty-button;code={code};scope=sticky"),
        );
        feed(&mut term, b"x");
        feed_osc(&mut term, T2_END);
        feed(&mut term, b"\r\n");
    }
    assert!(
        term.button_entry_count() <= MAX_BUTTON_ENTRIES,
        "a distinct-code flood must not exceed the entry ceiling (got {})",
        term.button_entry_count()
    );
}

// ---------------------------------------------------------------------------
// Reset / alternate screen
// ---------------------------------------------------------------------------

#[test]
fn ris_clears_the_button_table() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    assert_eq!(term.button_entry_count(), 1);
    feed(&mut term, b"\x1bc");
    assert_eq!(term.button_entry_count(), 0);
    // The feature gates survive RIS (host configuration, not terminal state).
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    assert_eq!(term.button_entry_count(), 1);
}

#[test]
fn alt_screen_definitions_are_refused() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"\x1b[?1049h"); // enter alternate screen
    feed_osc(&mut term, "1337;Button=type=custom;code=1");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    assert_eq!(
        term.button_entry_count(),
        0,
        "alt screen refuses definitions"
    );
    feed(&mut term, b"\x1b[?1049l"); // back to primary
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"go");
    feed_osc(&mut term, T2_END);
    assert_eq!(term.button_entry_count(), 1, "primary accepts again");
}

#[test]
fn run_left_open_across_alt_screen_switch_is_abandoned() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"half");
    feed(&mut term, b"\x1b[?1049h"); // switch mid-run
    feed_osc(&mut term, T2_END); // stale end: no run to close
    assert_eq!(term.button_entry_count(), 0);
}

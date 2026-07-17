// SPDX-License-Identifier: GPL-3.0-only
//! Model-level coverage for the button protocol (B1): dispatch gating, span
//! anchoring, block/sticky lifetime, scroll-out flat-coordinate merge, reflow
//! re-projection, flood bounds, and reset behavior. Parser and table unit
//! tests live in `super::button`; these drive the full [`Terminal`] feed path.

use super::button::{
    ButtonIcon, ButtonScope, ButtonSpan, ButtonState, MAX_BUTTON_ENTRIES, MAX_BUTTON_SPANS_PER_LINE,
};
use super::screen::{SnapshotButton, Terminal};
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

// ---------------------------------------------------------------------------
// B2: viewport button-span exposure for render (visible_button_spans)
// ---------------------------------------------------------------------------

#[test]
fn visible_spans_are_empty_when_the_gate_is_off() {
    // Gate off (default): even a well-formed definition stream leaves the
    // render accessor empty and does no row walk — the zero-work guarantee the
    // byte-identical off path depends on.
    let mut term = Terminal::new(20, 5);
    feed_osc(&mut term, "1337;Button=type=custom;code=42;icon=star");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"Retry");
    feed_osc(&mut term, T2_END);
    assert!(
        term.screen().visible_button_spans(0).is_empty(),
        "gate off: render accessor exposes nothing"
    );
}

#[test]
fn visible_spans_expose_a_tier1_point_button_as_its_chip_rect() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"ab");
    feed_osc(&mut term, "1337;Button=type=custom;code=42;icon=star.fill");
    let spans = term.screen().visible_button_spans(0);
    assert_eq!(spans.len(), 1);
    let SnapshotButton {
        row,
        start_col,
        len,
        code,
        icon,
        state,
        point,
    } = spans[0];
    assert_eq!(row, 0);
    // Content "ab" ends at col 2; one gap column, then the padded pill
    // (cap, pad, icon, space, "42", pad, cap) = 8 cells starting at col 3.
    assert_eq!(start_col, 3, "chip rect starts one gap past content end");
    assert_eq!(len, 8, "resolved chip rect, not the zero-length anchor");
    assert_eq!(code, 42);
    assert_eq!(icon, ButtonIcon::Star);
    assert_eq!(state, ButtonState::Live);
    assert!(point, "flagged as a point chip for the render layer");
}

#[test]
fn a_point_chip_with_no_room_on_its_row_is_not_exposed() {
    // The cursor parks at the last column: content fills the row, so the
    // resolved rect has no cell to start on and the chip is dropped rather
    // than painted over content or off-grid.
    let mut term = enabled_terminal(6, 3);
    feed(&mut term, b"abcdef");
    feed_osc(&mut term, "1337;Button=type=custom;code=42;icon=star");
    assert_eq!(term.button_entry_count(), 1, "the definition itself lands");
    assert!(
        term.screen().visible_button_spans(0).is_empty(),
        "no room: no chip rect to expose"
    );
}

#[test]
fn visible_spans_expose_a_tier2_label_run() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"$ ");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"Retry");
    feed_osc(&mut term, T2_END);
    let spans = term.screen().visible_button_spans(0);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].row, 0);
    assert_eq!(spans[0].start_col, 2, "run starts after the '$ ' prompt");
    assert_eq!(spans[0].len, 5, "\"Retry\" is five label cells");
    assert_eq!(spans[0].code, 7);
    assert_eq!(spans[0].state, ButtonState::Live);
}

#[test]
fn visible_spans_carry_the_invalidated_state() {
    // An invalidated button keeps its span (still visible) but reports the
    // grayed state so the renderer paints it dead instead of live.
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, "1337;Button=type=custom;code=1");
    feed_osc(&mut term, "1337;Button=type=custom"); // empty-code form: invalidate all
    let spans = term.screen().visible_button_spans(0);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].state, ButtonState::Invalidated);
}

#[test]
fn visible_spans_track_the_row_a_button_sits_on() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"line0\r\n");
    feed(&mut term, b"xy");
    feed_osc(&mut term, "1337;Button=type=custom;code=9;icon=run");
    let spans = term.screen().visible_button_spans(0);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].row, 1, "button anchored on the second visible row");
    assert_eq!(
        spans[0].start_col, 3,
        "chip rect one gap column past the row's \"xy\" content"
    );
}

#[test]
fn visible_spans_project_a_button_scrolled_into_scrollback() {
    // A sticky button defined on the top row, then scrolled up into scrollback,
    // is re-exposed when the viewport pages back to it. Row/column stay aligned
    // with the cells snapshot_with_scrollback draws for the same offset.
    let mut term = enabled_terminal(10, 3);
    feed(&mut term, b"AB");
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"Go");
    feed_osc(&mut term, T2_END);
    // Push the defining row up out of the live grid.
    feed(&mut term, b"\r\n\r\n\r\n\r\n\r\n");
    assert!(
        term.screen().visible_button_spans(0).is_empty(),
        "scrolled out of the live viewport"
    );
    // Page back far enough that the defining row is on screen again.
    let mut found = None;
    for offset in 1..=6 {
        let spans = term.screen().visible_button_spans(offset);
        if let Some(btn) = spans.first() {
            found = Some((offset, *btn));
            break;
        }
    }
    let (offset, btn) = found.expect("sticky button re-exposed from scrollback");
    assert_eq!(btn.start_col, 2, "label run still starts after \"AB\"");
    assert_eq!(btn.len, 2, "\"Go\" is two label cells");
    assert_eq!(btn.code, 7);
    // The exposed row must match the cell grid the render draws at this offset.
    let snapshot = term.screen().snapshot_with_scrollback(offset);
    let base = btn.row * snapshot.dimensions.columns + btn.start_col;
    let label: String = (0..btn.len).map(|i| snapshot.cells[base + i].ch).collect();
    assert_eq!(label, "Go", "row/col align with the drawn cells");
}

// ---------------------------------------------------------------------------
// B3: pointer hit-test (button_at) — the click arm's resolution query
// ---------------------------------------------------------------------------

#[test]
fn button_at_resolves_a_labeled_span_on_the_live_grid() {
    let mut term = enabled_terminal(20, 5);
    feed(&mut term, b"$ ");
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"Retry");
    feed_osc(&mut term, T2_END);
    // Every label cell hits; the cells beside it miss.
    for col in 2..7 {
        let hit = term
            .button_at(0, 0, col)
            .unwrap_or_else(|| panic!("label cell {col} hits"));
        assert_eq!(hit.code, 7);
        assert_eq!(hit.state, ButtonState::Live);
        assert_eq!(hit.scope, ButtonScope::Block);
        assert_eq!((hit.row, hit.start_col, hit.len), (0, 2, 5));
    }
    assert!(term.button_at(0, 0, 1).is_none(), "cell before the span");
    assert!(term.button_at(0, 0, 7).is_none(), "cell after the span");
    assert!(term.button_at(0, 1, 3).is_none(), "other rows miss");
}

#[test]
fn button_at_aligns_the_point_hit_box_to_the_chip_rect() {
    // The demo shape that exposed the drift: the definition lands at the
    // cursor BEFORE the line text prints, so the anchor cell (col 0) and the
    // painted chip (past content end) are far apart. The hit box must be the
    // chip the user sees, not the invisible anchor.
    let mut term = enabled_terminal(30, 5);
    feed_osc(&mut term, "1337;Button=type=custom;code=5;icon=star");
    feed(&mut term, b"  point button line");
    // Content ends at col 19; gap col 19... content "  point button line" is
    // 19 cells (cols 0..19), so the chip rect is cols 20..27 (len 6 + 1 digit).
    for col in 20..27 {
        let hit = term
            .button_at(0, 0, col)
            .unwrap_or_else(|| panic!("chip cell {col} hits"));
        assert_eq!(hit.code, 5);
        assert_eq!(
            (hit.row, hit.start_col, hit.len),
            (0, 20, 7),
            "the hit carries the resolved chip rect"
        );
    }
    assert!(
        term.button_at(0, 0, 0).is_none(),
        "the raw anchor cell no longer hits: it is line text, not chip"
    );
    assert!(term.button_at(0, 0, 19).is_none(), "gap column misses");
    assert!(
        term.button_at(0, 0, 27).is_none(),
        "cell past the chip misses"
    );
}

#[test]
fn button_at_resolves_through_scrollback_offsets() {
    let mut term = enabled_terminal(10, 3);
    feed(&mut term, b"AB");
    feed_osc(&mut term, T2_DEFINE_STICKY);
    feed(&mut term, b"Go");
    feed_osc(&mut term, T2_END);
    feed(&mut term, b"\r\n\r\n\r\n\r\n\r\n");
    // Scrolled out: no hit anywhere in the live viewport at offset 0.
    assert!(term.button_at(0, 0, 2).is_none());
    // Page back until the defining row re-enters the viewport; the hit's
    // viewport row must agree with the query row.
    let found = (1..=6)
        .find_map(|offset| {
            (0..3).find_map(|row| term.button_at(offset, row, 2).map(|hit| (offset, row, hit)))
        })
        .expect("sticky button reachable from scrollback");
    let (_, row, hit) = found;
    assert_eq!(hit.row, row);
    assert_eq!((hit.start_col, hit.len), (2, 2));
    assert_eq!(hit.code, 7);
    assert_eq!(hit.scope, ButtonScope::Sticky);
}

#[test]
fn button_at_dies_when_the_master_gate_turns_off() {
    // The partial-gate hole class: definitions landed while the gate was on
    // must not stay CLICKABLE after the gate turns off — the pointer-side
    // query gates independently of the OSC arm.
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"ok");
    feed_osc(&mut term, T2_END);
    assert!(term.button_at(0, 0, 0).is_some(), "hit while gate on");
    term.set_buttons_enabled(false);
    assert!(
        term.button_at(0, 0, 0).is_none(),
        "gate off kills clickability outright"
    );
    term.set_buttons_enabled(true);
    assert!(
        term.button_at(0, 0, 0).is_some(),
        "re-enable restores the still-referenced span"
    );
}

#[test]
fn button_at_reports_invalidated_state_for_dead_spans() {
    // A block-scoped button whose block ended keeps its span (renders dimmed)
    // but the hit reports Invalidated — the pointer arm treats it as inert.
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"ok");
    feed_osc(&mut term, T2_END);
    feed_osc(&mut term, "133;A"); // prompt boundary: block scope dies
    let hit = term.button_at(0, 0, 0).expect("span still present");
    assert_eq!(hit.state, ButtonState::Invalidated);
}

#[test]
fn button_at_misses_on_the_alternate_screen() {
    let mut term = enabled_terminal(20, 5);
    feed_osc(&mut term, T2_DEFINE);
    feed(&mut term, b"ok");
    feed_osc(&mut term, T2_END);
    assert!(term.button_at(0, 0, 0).is_some());
    feed(&mut term, b"\x1b[?1049h"); // enter alt screen
    assert!(
        term.button_at(0, 0, 0).is_none(),
        "primary-screen spans are not clickable through alt content"
    );
    feed(&mut term, b"\x1b[?1049l"); // back to primary
    assert!(term.button_at(0, 0, 0).is_some());
}

#[test]
fn prompt_active_tracks_osc133_boundaries() {
    let mut term = enabled_terminal(20, 5);
    assert!(!term.prompt_active(), "no shell integration: never active");
    feed_osc(&mut term, "133;A");
    assert!(term.prompt_active(), "active after A");
    feed_osc(&mut term, "133;C");
    assert!(!term.prompt_active(), "cleared at output start");
}

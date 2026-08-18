// SPDX-License-Identifier: GPL-3.0-only
//! Adversarial coverage for combining-mark *storage* — handle table or
//! per-line sidecar alike.
//!
//! Pins **today's** observable grapheme behavior at the seams any denser
//! representation has to keep: `Cell` is `Copy` (a later mutation of one
//! copy must not change another), blank/overwrite/ICH/DCH, scroll, wrap,
//! scrollback eviction, alternate-screen swap, session-host round-trip, and
//! reflow. Distinct live clusters stay fully readable — there is no
//! degradation path to pin. `cell_equivalence.rs` is the acceptance suite
//! and is not touched here.
//!
//! Windows: no platform surface. Core storage and CSI only.

use super::*;
use crate::core::types::MAX_COMBINING;

const MARKS: [char; 5] = ['\u{0301}', '\u{0302}', '\u{0303}', '\u{0304}', '\u{0305}'];

fn cluster(base: char, mark_count: usize) -> String {
    let mut s = String::new();
    s.push(base);
    for mark in MARKS.iter().take(mark_count) {
        s.push(*mark);
    }
    s
}

/// Distinct combining cluster `i`: unique base and unique mark, both in-range.
fn unique_cluster(i: u32) -> String {
    let base = char::from(b'A' + (i % 26) as u8);
    // U+0300..=U+036F are combining diacritical marks (width 0).
    let mark = char::from_u32(0x0300 + (i % 0x70)).unwrap();
    let mut s = String::new();
    s.push(base);
    s.push(mark);
    s
}

fn snapshot_cell_grapheme(cell: &SnapshotCell) -> String {
    let mut s = String::new();
    s.push(cell.ch);
    for mark in &cell.combining {
        s.push(*mark);
    }
    s
}

fn physical_graphemes(terminal: &Terminal) -> Vec<String> {
    let state = terminal.snapshot_state(100_000);
    state
        .scrollback_rows
        .iter()
        .chain(state.visible_rows.iter())
        .flat_map(|row| {
            row.cells
                .iter()
                .filter(|cell| !cell.wide_continuation)
                .map(snapshot_cell_grapheme)
        })
        .collect()
}

fn visible_graphemes(terminal: &Terminal) -> Vec<String> {
    let snap = terminal.snapshot();
    snap.cells
        .iter()
        .filter(|cell| !cell.wide_continuation)
        .map(Cell::grapheme)
        .collect()
}

/// `Cell` is `Copy`. Mutating the original after a copy must not change the
/// copy's marks. A side table that shares a handle without copying the payload
/// would make the copy observe the later mark — wrong glyphs, not missing ones.
#[test]
fn copied_cell_does_not_alias_later_combining_on_the_original() {
    let mut cell = Cell::new('e', Attrs::default());
    assert!(cell.push_combining('\u{0301}'));
    let held = cell;
    assert!(cell.push_combining('\u{0302}'));
    assert_eq!(held.combining(), &['\u{0301}']);
    assert_eq!(cell.combining(), &['\u{0301}', '\u{0302}']);
    assert_ne!(held.grapheme(), cell.grapheme());
}

#[test]
fn blanking_the_original_does_not_strip_marks_from_a_held_copy() {
    let mut live = Cell::new('e', Attrs::default());
    assert!(live.push_combining('\u{0301}'));
    let held = live;
    live = Cell::blank();
    assert_eq!(held.grapheme(), cluster('e', 1));
    assert_eq!(live.ch, ' ');
    assert!(live.combining().is_empty());
}

/// Overwrite at the same grid slot must not leave the previous cell's marks
/// on the new occupant (stale handle resolving to a different cluster).
#[test]
fn overwrite_does_not_leave_previous_marks_on_the_new_occupant() {
    let first = cluster('e', 4);
    let second = cluster('x', 2);
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(first.as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), first);
    terminal.advance(b"\x1b[H");
    terminal.advance(second.as_bytes());
    let cell = terminal.screen().cell(0, 0).unwrap();
    assert_eq!(cell.grapheme(), second);
    assert_ne!(cell.grapheme(), first);
}

/// ICH shifts the combining cell right; writing a different cluster into the
/// vacated slot must keep both clusters attached to their own bases.
#[test]
fn ich_does_not_swap_marks_between_the_shifted_cell_and_the_new_occupant() {
    let shifted = cluster('e', 4);
    let occupant = cluster('x', 3);
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(shifted.as_bytes());
    terminal.advance(b"\x1b[H\x1b[@"); // ICH 1 at home
    terminal.advance(occupant.as_bytes());
    assert_eq!(
        terminal.screen().cell(0, 0).unwrap().grapheme(),
        occupant,
        "new occupant must keep its own marks"
    );
    assert_eq!(
        terminal.screen().cell(0, 1).unwrap().grapheme(),
        shifted,
        "shifted cell must keep the marks it had before ICH"
    );
}

#[test]
fn dch_moves_marks_with_the_cell_not_the_column_index() {
    let keep = cluster('e', 4);
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(b"Z");
    terminal.advance(keep.as_bytes());
    assert_eq!(terminal.screen().cell(0, 1).unwrap().grapheme(), keep);
    terminal.advance(b"\x1b[H\x1b[P"); // DCH 1 at home: drop Z, shift e left
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), keep);
    assert_ne!(terminal.screen().cell(0, 1).unwrap().grapheme(), keep);
}

#[test]
fn erase_then_write_elsewhere_does_not_revive_old_marks() {
    let old = cluster('e', 4);
    let fresh = cluster('x', 2);
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(old.as_bytes());
    terminal.advance(b"\x1b[H\x1b[K"); // EL 0
    assert!(terminal.screen().cell(0, 0).unwrap().combining().is_empty());
    terminal.advance(b"\x1b[1;3H");
    terminal.advance(fresh.as_bytes());
    assert!(terminal.screen().cell(0, 0).unwrap().combining().is_empty());
    assert_eq!(terminal.screen().cell(0, 2).unwrap().grapheme(), fresh);
}

/// Recycle every visible slot: first generation must vanish from the live
/// grid and the second generation must not inherit first-generation marks.
#[test]
fn filling_the_grid_twice_does_not_cross_talk_between_generations() {
    let mut terminal = Terminal::new(8, 4);
    let mut first = Vec::new();
    for i in 0..32u32 {
        let c = unique_cluster(i);
        terminal.advance(c.as_bytes());
        first.push(c);
    }
    let live_first = visible_graphemes(&terminal);
    for c in &first {
        assert!(live_first.contains(c), "first generation {c:?} missing");
    }

    terminal.advance(b"\x1b[2J\x1b[H");
    let mut second = Vec::new();
    for i in 100..132u32 {
        let c = unique_cluster(i);
        terminal.advance(c.as_bytes());
        second.push(c);
    }
    let live_second = visible_graphemes(&terminal);
    for c in &first {
        assert!(
            !live_second.contains(c),
            "first-generation cluster {c:?} leaked onto the recycled grid"
        );
    }
    for c in &second {
        assert!(live_second.contains(c), "second generation {c:?} missing");
    }
}

#[test]
fn scrolled_combining_stays_on_its_history_row_not_on_the_new_live_cell() {
    let old = cluster('e', 4);
    let new = cluster('x', 3);
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(old.as_bytes());
    terminal.advance(b"\r\nL2\r\nL3\r\n");
    terminal.advance(new.as_bytes());
    let physical = physical_graphemes(&terminal);
    assert!(
        physical.contains(&old),
        "scrolled cluster missing from history"
    );
    assert!(physical.contains(&new));
    let live = visible_graphemes(&terminal);
    assert!(
        !live.contains(&old),
        "scrolled marks must not sit on the live row"
    );
    assert!(live.contains(&new));
}

/// Sustained distinct clusters on the live grid: every written cluster remains
/// readable. There is no degradation path — a storage change that drops marks
/// under distinct-combining pressure is a behavior change.
#[test]
fn every_distinct_live_cluster_remains_readable() {
    let mut terminal = Terminal::new(16, 8);
    let mut written = Vec::new();
    for i in 0..128u32 {
        let c = unique_cluster(i);
        terminal.advance(c.as_bytes());
        written.push(c);
    }
    let live = visible_graphemes(&terminal);
    for c in &written {
        assert!(
            live.contains(c),
            "cluster {c:?} vanished under distinct-combining pressure"
        );
    }
}

#[test]
fn max_combining_boundary_is_exact_on_adjacent_cells() {
    let mut terminal = Terminal::new(8, 1);
    for n in 0..=5 {
        let c = cluster(char::from(b'A' + n as u8), n);
        terminal.advance(c.as_bytes());
        let cell = terminal.screen().cell(0, n).unwrap();
        let kept = n.min(MAX_COMBINING);
        assert_eq!(cell.combining().len(), kept, "column {n}");
        assert_eq!(cell.grapheme(), cluster(char::from(b'A' + n as u8), kept));
    }
}

#[test]
fn fifth_mark_drop_does_not_affect_the_neighbor_cell() {
    let four = cluster('e', 4);
    let five = cluster('x', 5);
    let mut terminal = Terminal::new(8, 1);
    terminal.advance(four.as_bytes());
    terminal.advance(five.as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), four);
    assert_eq!(
        terminal.screen().cell(0, 1).unwrap().grapheme(),
        cluster('x', 4)
    );
}

#[test]
fn alternate_screen_swaps_combining_state_as_a_unit() {
    let primary = cluster('e', 4);
    let alt = cluster('x', 3);
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(primary.as_bytes());
    terminal.advance(b"\x1b[?1049h");
    terminal.advance(alt.as_bytes());
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), alt);
    assert_ne!(terminal.screen().cell(0, 0).unwrap().grapheme(), primary);

    terminal.advance(b"\x1b[?1049l");
    assert_eq!(terminal.screen().cell(0, 0).unwrap().grapheme(), primary);

    terminal.advance(b"\x1b[?1049h");
    let live = visible_graphemes(&terminal);
    assert!(
        !live.contains(&primary),
        "re-entering 1049 must not leak primary combining onto a cleared alt"
    );
}

#[test]
fn snapshot_envelope_roundtrip_does_not_swap_two_distinct_clusters() {
    let left = cluster('e', 4);
    let right = cluster('x', 3);
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(left.as_bytes());
    terminal.advance(right.as_bytes());

    let envelope = SnapshotEnvelope::from_terminal(&terminal, SnapshotCaptureLimits::default());
    let bytes = envelope.encode().expect("honest capture must encode");
    let decoded = SnapshotEnvelope::decode(&bytes, SnapshotEnvelopeCaps::default())
        .expect("honest envelope must decode");
    let row = &decoded.terminal.visible_rows[0].cells;
    assert_eq!(snapshot_cell_grapheme(&row[0]), left);
    assert_eq!(snapshot_cell_grapheme(&row[1]), right);
    assert_ne!(
        snapshot_cell_grapheme(&row[0]),
        snapshot_cell_grapheme(&row[1])
    );
}

/// A combining cluster sitting at the wrap boundary must stay one grapheme
/// after a width-changing resize — marks follow the cell, not the old column.
#[test]
fn reflow_keeps_marks_on_the_same_base_across_a_wrap() {
    let c = cluster('e', 4);
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"AAAAAAA");
    terminal.advance(c.as_bytes());
    assert_eq!(terminal.screen().cell(0, 7).unwrap().grapheme(), c);
    terminal.resize(5, 4);
    let physical = physical_graphemes(&terminal);
    assert!(
        physical.contains(&c),
        "reflow dropped or split the combining cluster; grid={physical:?}"
    );
    assert!(
        physical.iter().filter(|g| *g == &c).count() == 1,
        "cluster must not duplicate across wrapped rows; grid={physical:?}"
    );
}

/// Two combining clusters on one hard-terminated line must stay in order after
/// they scroll into history. A per-line sidecar keyed only by line (not by cell
/// offset) would swap or fuse them.
#[test]
fn two_clusters_on_one_scrolled_line_keep_order() {
    let left = cluster('e', 4);
    let right = cluster('x', 3);
    let mut terminal = Terminal::new(8, 2);
    terminal.advance(left.as_bytes());
    terminal.advance(right.as_bytes());
    terminal.advance(b"\r\nL2\r\nL3\r\n");
    let physical = physical_graphemes(&terminal);
    let left_at = physical.iter().position(|g| g == &left);
    let right_at = physical.iter().position(|g| g == &right);
    assert!(
        left_at.is_some(),
        "left cluster missing from history; {physical:?}"
    );
    assert!(
        right_at.is_some(),
        "right cluster missing from history; {physical:?}"
    );
    assert!(
        left_at < right_at,
        "clusters swapped or fused; left={left_at:?} right={right_at:?} grid={physical:?}"
    );
}

/// Soft-wrap continuation: a cluster on the last column and a different cluster
/// on the next physical row of the same logical line. After a width change both
/// must survive unswapped — the wrap-boundary case a line sidecar gets wrong.
#[test]
fn soft_wrapped_continuation_keeps_both_clusters_across_resize() {
    let lead = cluster('e', 4);
    let cont = cluster('x', 3);
    let mut terminal = Terminal::new(8, 3);
    terminal.advance(b"AAAAAAA");
    terminal.advance(lead.as_bytes());
    terminal.advance(cont.as_bytes());
    assert_eq!(terminal.screen().cell(0, 7).unwrap().grapheme(), lead);
    assert_eq!(terminal.screen().cell(1, 0).unwrap().grapheme(), cont);
    terminal.resize(5, 4);
    let physical = physical_graphemes(&terminal);
    let lead_at = physical.iter().position(|g| g == &lead);
    let cont_at = physical.iter().position(|g| g == &cont);
    assert!(
        lead_at.is_some(),
        "lead cluster missing after reflow; {physical:?}"
    );
    assert!(
        cont_at.is_some(),
        "continuation cluster missing after reflow; {physical:?}"
    );
    assert!(
        lead_at < cont_at,
        "wrap-boundary clusters swapped; lead={lead_at:?} cont={cont_at:?} grid={physical:?}"
    );
}

/// Evicting oldest history must drop that line's clusters and leave every
/// retained line's clusters attached to it — not to a neighbor that survived.
#[test]
fn scrollback_eviction_does_not_reattach_evicted_marks_to_survivors() {
    let mut terminal = Terminal::new(8, 2);
    terminal.set_scrollback_limit(3);
    let mut written = Vec::new();
    for i in 0..10u32 {
        let c = unique_cluster(i);
        terminal.advance(c.as_bytes());
        terminal.advance(b"\r\n");
        written.push(c);
    }
    let physical = physical_graphemes(&terminal);
    let present: Vec<&String> = written.iter().filter(|c| physical.contains(c)).collect();
    assert!(
        present.len() < written.len(),
        "precondition: some history must have been evicted; present={present:?}"
    );
    for c in &present {
        assert_eq!(
            physical.iter().filter(|g| g == c).count(),
            1,
            "survivor {c:?} duplicated after eviction; grid={physical:?}"
        );
    }
    // The newest written clusters that still fit must be present and in order.
    let newest = &written[written.len() - 3..];
    let mut last = 0usize;
    for c in newest {
        let at = physical
            .iter()
            .position(|g| g == c)
            .unwrap_or_else(|| panic!("recent cluster {c:?} missing after eviction; {physical:?}"));
        assert!(at >= last, "recent clusters reordered; grid={physical:?}");
        last = at;
    }
}

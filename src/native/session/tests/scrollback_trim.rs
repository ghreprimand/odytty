// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored regression fixtures for front eviction during output.

use super::*;

fn capped_history() -> WorkspaceSet {
    let mut set = WorkspaceSet::new(build_session(), None);
    {
        let mut terminal = set.active().terminal.lock().expect("terminal");
        terminal.set_scrollback_limit(4);
        for _ in 0..16 {
            terminal.advance(b"match\r\n");
        }
    }
    set.reconcile_scrollback_trims();
    let len = set
        .active()
        .terminal
        .lock()
        .expect("terminal")
        .screen()
        .scrollback_len();
    assert_eq!(len, 4);
    let pushes = set
        .active()
        .terminal
        .lock()
        .expect("terminal")
        .screen()
        .pushed_row_count();
    set.active_mut()
        .anchor_viewport_for_render(len, pushes, false);
    set.active_mut().viewport.scroll_up(2, len);
    set
}

fn output(set: &mut WorkspaceSet, bytes: &[u8]) {
    let old = set
        .active()
        .terminal
        .lock()
        .expect("terminal")
        .scrollback_trim_epoch();
    set.active()
        .terminal
        .lock()
        .expect("terminal")
        .advance(bytes);
    assert_ne!(
        set.active()
            .terminal
            .lock()
            .expect("terminal")
            .scrollback_trim_epoch(),
        old
    );
    set.reconcile_scrollback_trims();
}

fn numbered_capped_history() -> WorkspaceSet {
    let mut set = WorkspaceSet::new(build_session(), None);
    {
        let mut terminal = set.active().terminal.lock().expect("terminal");
        terminal.set_scrollback_limit(16);
        for row in 0..40 {
            terminal.advance(format!("row {row:02}\r\n").as_bytes());
        }
    }
    set.reconcile_scrollback_trims();
    let pushes = set
        .active()
        .terminal
        .lock()
        .expect("terminal")
        .screen()
        .pushed_row_count();
    set.active_mut()
        .anchor_viewport_for_render(16, pushes, false);
    set.active_mut().viewport.scroll_up(5, 16);
    set.active_mut().search_restore_viewport = Some(5);
    set
}

fn top_visible_text(set: &WorkspaceSet) -> String {
    let pane = set.active();
    let terminal = pane.terminal.lock().expect("terminal");
    let snapshot = terminal
        .screen()
        .snapshot_with_scrollback(pane.viewport.offset());
    snapshot.cells[..snapshot.dimensions.columns]
        .iter()
        .map(|cell| cell.ch)
        .collect()
}

#[test]
fn trim_output_keeps_scrolled_viewport_at_history_cap() {
    let mut set = numbered_capped_history();
    let before = top_visible_text(&set);
    output(&mut set, b"new 1\r\nnew 2\r\nnew 3\r\n");
    assert_eq!(set.active().viewport.offset(), 8);
    assert_eq!(set.active().search_restore_viewport, Some(8));
    assert_eq!(top_visible_text(&set), before);
    let pushes = set
        .active()
        .terminal
        .lock()
        .expect("terminal")
        .screen()
        .pushed_row_count();
    assert_eq!(
        set.active_mut()
            .anchor_viewport_for_render(16, pushes, false),
        8,
        "render after trim must not count the same pushes twice"
    );
    assert_eq!(top_visible_text(&set), before);
}

#[test]
fn trim_output_pins_at_oldest_row_when_viewed_text_is_evicted() {
    let mut set = numbered_capped_history();
    let before = top_visible_text(&set);
    output(&mut set, &b"new\r\n".repeat(20));
    assert_eq!(set.active().viewport.offset(), 16);
    assert_eq!(set.active().search_restore_viewport, Some(16));
    assert_ne!(top_visible_text(&set), before);
    let terminal = set.active().terminal.lock().expect("terminal");
    let oldest = terminal.screen().snapshot_with_scrollback(16);
    let expected: String = oldest.cells[..oldest.dimensions.columns]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    drop(terminal);
    assert_eq!(top_visible_text(&set), expected);
}

#[test]
fn trim_output_keeps_search_query_and_refreshes_matches() {
    let mut set = capped_history();
    set.active_mut().search.open();
    for ch in "match".chars() {
        set.active_mut().search.push_char(ch);
    }
    let terminal = Arc::clone(&set.active().terminal);
    set.active_mut()
        .search
        .refresh(&terminal.lock().expect("terminal"));
    assert!(set.active().search.match_count() > 0);
    set.active_mut().search_restore_viewport = Some(2);
    output(&mut set, b"new\r\n");
    assert!(set.active().search.is_open());
    assert_eq!(set.active().search.render_signature().query, "match");
    assert_eq!(set.active().search_restore_viewport, Some(3));
    assert_eq!(
        set.active().search.match_count(),
        0,
        "old absolute matches are discarded"
    );
    set.active_mut()
        .search
        .refresh(&terminal.lock().expect("terminal"));
    let signature = set.active().search.render_signature();
    assert!(signature.matches.iter().all(|m| m.start.0 < 12));
    assert!(signature.current.is_some());
}

#[test]
fn trim_output_does_not_reset_live_row_fade_layout() {
    let mut set = capped_history();
    set.active_mut().viewport.reset_to_live();
    let now = Instant::now();
    set.active_mut().row_fade_starts = vec![Some(now); 8];
    set.active_mut().row_fade_dimensions = Some(Dimensions::new(20, 8));
    set.active_mut().row_fade_next_frame = Some(now);
    output(&mut set, b"new\r\n");
    assert_eq!(set.active().row_fade_starts, vec![Some(now); 8]);
    assert_eq!(
        set.active().row_fade_dimensions,
        Some(Dimensions::new(20, 8))
    );
    assert_eq!(set.active().row_fade_next_frame, Some(now));
    assert!(set.active().viewport.is_live());
}

#[test]
fn trim_output_still_clears_stale_selection_and_clamps_shorter_history() {
    let mut set = capped_history();
    set.active_mut().selection.set_range(test_selection());
    set.active_mut().search_restore_viewport = Some(2);
    set.active()
        .terminal
        .lock()
        .expect("terminal")
        .set_scrollback_limit(1);
    set.reconcile_scrollback_trims();
    assert!(set.active().selection.range().is_none());
    assert_eq!(set.active().viewport.offset(), 1);
    assert_eq!(set.active().search_restore_viewport, Some(1));
    set.active()
        .terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b[3J");
    set.reconcile_scrollback_trims();
    assert!(set.active().viewport.is_live());
}

#[test]
fn trim_output_preserves_scoped_search_restriction() {
    let mut set = capped_history();
    let terminal = Arc::clone(&set.active().terminal);
    let revision = terminal.lock().expect("terminal").render_revision();
    set.active_mut().search.open_scoped(
        crate::core::AbsolutePoint { row: 0, column: 0 },
        crate::core::AbsolutePoint { row: 1, column: 4 },
        revision,
    );
    set.active_mut().search.push_char('m');
    set.active_mut()
        .search
        .refresh(&terminal.lock().expect("terminal"));
    assert!(set.active().search.match_count() > 0);
    output(&mut set, b"match\r\n");
    set.active_mut()
        .search
        .refresh(&terminal.lock().expect("terminal"));
    let signature = set.active().search.render_signature();
    assert!(signature.open && signature.scoped);
    assert_eq!(signature.query, "m");
    assert!(signature.matches.is_empty());
    assert!(signature.current.is_none());
}

#[test]
fn trim_output_reconciles_background_panes_and_rebases_retained_copy_caret() {
    let mut set = capped_history();
    let first = set.active_focused_token();
    set.active_mut().copy_mode = Some(crate::native::copy_mode::CopyModeState::new(
        crate::selection::AbsoluteCellPoint { row: 1, column: 0 },
    ));
    set.split_active_for_test(SplitAxis::Columns, build_session_with_id(SessionToken(1)));
    set.get(first)
        .expect("background pane")
        .terminal
        .lock()
        .expect("terminal")
        .advance(b"new\r\n");
    set.reconcile_scrollback_trims();
    let pane = set.get(first).expect("background pane");
    assert_eq!(pane.viewport.offset(), 3);
    assert_eq!(pane.copy_mode.expect("retained copy caret").cursor().row, 0);
    assert!(set.active().viewport.is_live());
}

#[test]
fn trim_output_reflow_still_resets_viewport_and_search() {
    let mut set = capped_history();
    set.active_mut().search.open();
    set.active_mut().invalidate_layout_dependent_state();
    assert!(set.active().viewport.is_live());
    assert!(!set.active().search.is_open());
}

#[test]
fn trim_output_reconciliation_is_idempotent_without_another_eviction() {
    let mut set = capped_history();
    output(&mut set, b"new\r\n");
    set.active_mut().selection.set_range(test_selection());
    set.active_mut().search.open();
    set.active_mut().search.push_char('m');
    let terminal = Arc::clone(&set.active().terminal);
    set.active_mut()
        .search
        .refresh(&terminal.lock().expect("terminal"));
    let signature = set.active().search.render_signature();
    set.reconcile_scrollback_trims();
    assert_eq!(set.active().selection.range(), Some(test_selection()));
    assert_eq!(set.active().viewport.offset(), 3);
    assert_eq!(set.active().search.render_signature(), signature);
}

#[test]
fn trim_output_render_before_trim_does_not_count_pushes_twice() {
    let mut set = numbered_capped_history();
    let before = top_visible_text(&set);
    let pushes = {
        let mut terminal = set.active().terminal.lock().expect("terminal");
        terminal.advance(b"new 1\r\nnew 2\r\nnew 3\r\n");
        terminal.screen().pushed_row_count()
    };
    assert_eq!(
        set.active_mut()
            .anchor_viewport_for_render(16, pushes, false),
        8
    );
    set.reconcile_scrollback_trims();
    assert_eq!(set.active().viewport.offset(), 8);
    assert_eq!(set.active().search_restore_viewport, Some(8));
    assert_eq!(top_visible_text(&set), before);
}

#[test]
fn trim_output_live_search_return_stays_at_live_bottom() {
    let mut set = numbered_capped_history();
    set.active_mut().viewport.reset_to_live();
    set.active_mut().search_restore_viewport = Some(0);
    output(&mut set, b"new 1\r\nnew 2\r\nnew 3\r\n");
    assert!(set.active().viewport.is_live());
    assert_eq!(set.active().search_restore_viewport, Some(0));
}

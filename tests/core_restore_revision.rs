// SPDX-License-Identifier: GPL-3.0-only
//! Project-authored terminal ownership and invalidation regressions.
use odytty::core::{SnapshotCaptureLimits, SnapshotEnvelope, Terminal};

#[test]
fn ambiguous_reflow_invalidates_render_revision() {
    let mut terminal = Terminal::new(8, 3);
    terminal.advance("\u{00a1}x".as_bytes());
    let revision = terminal.render_revision();
    let before = terminal.snapshot();
    terminal.set_ambiguous_wide(true);
    assert_ne!(before, terminal.snapshot());
    assert_ne!(revision, terminal.render_revision());
}

#[test]
fn snapshot_restore_invalidates_previous_revision() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"B");
    let envelope = SnapshotEnvelope::from_terminal(&source, SnapshotCaptureLimits::default());
    let mut target = Terminal::new(8, 3);
    target.advance(b"A");
    let revision = target.render_revision();
    let previous = target.snapshot();
    target.restore_from_envelope(&envelope).unwrap();
    assert_ne!(previous, target.snapshot());
    assert_ne!(revision, target.render_revision());
}

#[test]
fn trimming_prompt_marks_sets_the_change_latch() {
    let mut terminal = Terminal::new(8, 2);
    terminal.set_scrollback_limit(8);
    terminal.advance(b"\x1b]133;A\x07P\r\nQ\r\nR\r\nS\r\nT");
    assert!(terminal.take_prompt_marks_changed());
    assert!(!terminal.prompt_marks().is_empty());
    terminal.set_scrollback_limit(1);
    assert!(terminal.prompt_marks().is_empty());
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn invalid_owned_envelope_is_rejected_before_allocation() {
    let source = Terminal::new(8, 3);
    let mut envelope = SnapshotEnvelope::from_terminal(&source, SnapshotCaptureLimits::default());
    envelope.terminal.dimensions.columns = usize::MAX;
    let result = std::panic::catch_unwind(|| Terminal::from_snapshot_envelope(&envelope));
    assert!(
        result.is_ok(),
        "fallible constructor must reject before allocating"
    );
    assert!(result.unwrap().is_err());
}

#[test]
fn restoring_a_snapshot_keeps_the_host_ambiguous_width_policy() {
    let mut source = Terminal::new(8, 3);
    source.advance(b"B");
    let envelope = SnapshotEnvelope::from_terminal(&source, SnapshotCaptureLimits::default());
    let mut target = Terminal::new(8, 3);
    target.set_ambiguous_wide(true);
    target.restore_from_envelope(&envelope).unwrap();
    target.advance("\r\u{00a1}".as_bytes());
    assert!(
        target.screen().cell(0, 1).unwrap().wide_continuation,
        "restore must retain the host-selected ambiguous-width policy"
    );
}

#[test]
fn deleting_a_marked_line_sets_the_prompt_change_latch() {
    let mut terminal = Terminal::new(8, 4);
    terminal.advance(b"\x1b[2;1H\x1b]133;A\x07P");
    assert!(terminal.take_prompt_marks_changed());
    terminal.advance(b"\x1b[2;1H\x1b[M");
    assert!(terminal.prompt_marks().is_empty());
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn inserting_a_line_moves_a_prompt_mark_and_sets_its_latch() {
    let mut terminal = Terminal::new(8, 4);
    terminal.advance(b"\x1b[2;1H\x1b]133;A\x07P");
    let before = terminal.prompt_marks();
    assert!(terminal.take_prompt_marks_changed());
    terminal.advance(b"\x1b[1;1H\x1b[L");
    assert_ne!(before, terminal.prompt_marks());
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn scrolling_a_region_holding_a_marked_row_sets_the_prompt_change_latch() {
    let mut terminal = Terminal::new(8, 4);
    terminal.advance(b"\x1b[1;3r\x1b[2;1H\x1b]133;A\x07P");
    assert!(terminal.take_prompt_marks_changed());
    terminal.advance(b"\x1b[S");
    assert!(terminal.take_prompt_marks_changed());
}

#[test]
fn restoring_a_snapshot_keeps_the_host_button_gate() {
    let source = Terminal::new(8, 3);
    let envelope = SnapshotEnvelope::from_terminal(&source, SnapshotCaptureLimits::default());
    let mut target = Terminal::new(8, 3);
    target.set_buttons_enabled(true);
    target.restore_from_envelope(&envelope).unwrap();
    target.advance(b"\x1b]133;P;odytty-button;code=7\x07go\x1b]133;P;odytty-button;end\x07");
    assert!(
        target.button_at(0, 0, 0).is_some(),
        "restore must retain the host-selected button gate"
    );
}

#[test]
fn restoring_over_marked_state_reports_the_removed_marks() {
    let source = Terminal::new(8, 3);
    let envelope = SnapshotEnvelope::from_terminal(&source, SnapshotCaptureLimits::default());
    let mut target = Terminal::new(8, 3);
    target.advance(b"\x1b]133;A\x07P");
    assert!(target.take_prompt_marks_changed());
    target.restore_from_envelope(&envelope).unwrap();
    assert!(target.prompt_marks().is_empty());
    assert!(target.take_prompt_marks_changed());
}

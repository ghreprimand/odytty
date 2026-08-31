// SPDX-License-Identifier: GPL-3.0-only

use crate::core::{
    MAX_NOTIFICATION_PAYLOAD_BYTES, MAX_PENDING_NOTIFICATIONS, NotificationSource, ProgressKind,
    Terminal,
};

#[test]
fn supported_notification_spellings_are_sanitized_and_drained() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b]9;build\nfinished\x07\x1b\\");
    terminal.advance(b"\x1b]777;notify;Compile;all\ttargets\x1b\\");

    let events = terminal.take_notifications();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].source, NotificationSource::Osc9);
    assert_eq!(events[0].body, "buildfinished");
    assert_eq!(events[1].source, NotificationSource::Osc777);
    assert_eq!(events[1].title.as_deref(), Some("Compile"));
    assert_eq!(events[1].body, "alltargets");
    assert!(terminal.take_notifications().is_empty());
}

#[test]
fn progress_is_bounded_typed_and_clearable() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b]9;4;1;42\x1b\\");
    let progress = terminal.take_progress_changed().flatten().unwrap();
    assert_eq!(progress.kind, ProgressKind::Normal);
    assert_eq!(progress.value, Some(42));

    terminal.advance(b"\x1b]9;4;3\x1b\\");
    let progress = terminal.take_progress_changed().flatten().unwrap();
    assert_eq!(progress.kind, ProgressKind::Indeterminate);
    assert_eq!(progress.value, None);

    terminal.advance(b"\x1b]9;4;0\x1b\\");
    assert_eq!(terminal.take_progress_changed(), Some(None));
}

#[test]
fn malformed_and_oversized_requests_are_ignored() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b]9;4;1;101\x1b\\");
    terminal.advance(b"\x1b]9;4;3;50\x1b\\");
    let payload = "x".repeat(MAX_NOTIFICATION_PAYLOAD_BYTES + 1);
    terminal.advance(format!("\x1b]9;{payload}\x1b\\").as_bytes());
    assert_eq!(terminal.take_progress_changed(), None);
    assert!(terminal.take_notifications().is_empty());
}

#[test]
fn notification_queue_is_bounded_and_deduplicated() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b]9;same\x1b\\\x1b]9;same\x1b\\");
    for index in 0..(MAX_PENDING_NOTIFICATIONS + 3) {
        terminal.advance(format!("\x1b]9;event-{index}\x1b\\").as_bytes());
    }
    let events = terminal.take_notifications();
    assert_eq!(events.len(), MAX_PENDING_NOTIFICATIONS);
    assert_eq!(events.first().unwrap().body, "event-3");
    assert_eq!(events.last().unwrap().body, "event-10");
}

#[test]
fn hard_reset_clears_transient_state() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b]9;pending\x1b\\\x1b]9;4;2;12\x1b\\\x1bc");
    assert!(terminal.take_notifications().is_empty());
    assert_eq!(terminal.take_progress_changed(), Some(None));
}

#[test]
fn explicit_command_end_edges_are_bounded_and_drained() {
    let mut terminal = Terminal::new(80, 24);
    terminal.advance(b"\x1b]133;A\x1b\\$ run\r\n\x1b]133;C\x1b\\output\r\n\x1b]133;D;7\x1b\\");
    assert_eq!(terminal.take_command_completions(), vec![Some(7)]);
    assert!(terminal.take_command_completions().is_empty());
}

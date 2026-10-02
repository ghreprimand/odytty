// SPDX-License-Identifier: GPL-3.0-only
//! Guarded broadcast input through the App input routes.

use super::*;
use crate::native::app::broadcast_input::{BroadcastLabel, RECEIVER_LABEL, paint_broadcast_label};
use crate::native::broadcast::{BroadcastPayload, BroadcastSummary, ReceiverInfo};
use crate::native::key_event_diagnostics::FanoutOutcome;
use crate::native::session::SessionToken;
use crate::settings::{BindableAction, KeyBindingKey, KeyBindingModifiers, KeyChord};
use std::io::Write;

type Recorded = Arc<Mutex<Vec<u8>>>;

#[derive(Clone, Default)]
struct RecordingWriter(Recorded);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("recorded bytes")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A writer whose pane has gone away: every write fails.
struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
}

fn recording() -> (PtyWriter, Recorded) {
    let recorded = Recorded::default();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter(recorded.clone()))));
    (writer, recorded)
}

/// A headless App whose first pane records its writes.
fn app() -> (App, Arc<Mutex<Terminal>>, Recorded) {
    let (writer, recorded) = recording();
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );
    (app, terminal, recorded)
}

/// Split the active tab with a pane backed by `writer`; returns its token and
/// terminal. Focus stays where the split seam leaves it; callers refocus.
fn split(app: &mut App, writer: PtyWriter) -> (SessionToken, Arc<Mutex<Terminal>>) {
    let before = app.active_tab_pane_tokens_for_test();
    let dimensions = NativeOptions::default().initial_grid;
    let terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    app.seed_headless_split_pane_for_test(true, terminal.clone(), writer, dimensions);
    let token = app
        .active_tab_pane_tokens_for_test()
        .into_iter()
        .find(|token| !before.contains(token))
        .expect("new pane token");
    (token, terminal)
}

fn recorded_split(app: &mut App) -> (SessionToken, Recorded) {
    let (writer, recorded) = recording();
    let (token, _terminal) = split(app, writer);
    (token, recorded)
}

/// Add `token` through the production palette action.
fn add_receiver(app: &mut App, token: SessionToken) {
    let focused = app.active_session_token_for_test();
    app.focus_session_token_for_test(token);
    assert!(!app.is_broadcast_receiver(token));
    app.handle_palette_action_for_test("toggle-broadcast");
    assert!(app.is_broadcast_receiver(token));
    app.focus_session_token_for_test(focused);
}

fn type_char(app: &mut App, ch: char) {
    let text: String = ch.to_string();
    app.drive_raw_key_event_for_test(
        WinitKey::Character(text.clone().into()),
        WinitKey::Character(text.into()),
        PhysicalKey::Code(KeyCode::KeyA),
        Modifiers::NONE,
        KeyEventType::Press,
    );
}

fn press_ctrl_shift_x(app: &mut App) {
    app.drive_raw_key_event_for_test(
        WinitKey::Character("X".into()),
        WinitKey::Character("x".into()),
        PhysicalKey::Code(KeyCode::KeyX),
        Modifiers {
            ctrl: true,
            alt: false,
            shift: true,
        },
        KeyEventType::Press,
    );
}

fn bytes(recorded: &Recorded) -> Vec<u8> {
    recorded.lock().expect("recorded bytes").clone()
}

#[test]
fn a_second_pane_is_not_a_receiver_until_toggled_and_the_focused_pane_gets_input_once() {
    let (mut app, _terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, second) = recorded_split(&mut app);
    app.focus_session_token_for_test(first_token);

    type_char(&mut app, 'a');
    assert_eq!(bytes(&first), b"a");
    assert!(
        bytes(&second).is_empty(),
        "no pane is a receiver by default"
    );
    assert!(!app.broadcast_active());

    add_receiver(&mut app, second_token);
    type_char(&mut app, 'b');
    assert_eq!(bytes(&first), b"ab", "the focused pane keeps its own write");
    assert_eq!(bytes(&second), b"b");

    // Adding the focused pane too must not deliver to it twice.
    add_receiver(&mut app, first_token);
    type_char(&mut app, 'c');
    assert_eq!(bytes(&first), b"abc");
    assert_eq!(bytes(&second), b"bc");

    // IME commits take the same fan-out.
    app.handle_ime(winit::event::Ime::Commit("ime".to_owned()));
    assert_eq!(bytes(&second), b"bcime");
}

#[test]
fn a_pane_created_after_the_set_exists_is_not_added() {
    let (mut app, _terminal, _first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, second) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);
    let (third_token, third) = recorded_split(&mut app);
    app.focus_session_token_for_test(first_token);

    type_char(&mut app, 'd');

    assert_eq!(bytes(&second), b"d");
    assert!(
        bytes(&third).is_empty(),
        "a split is never added on its own"
    );
    assert!(!app.is_broadcast_receiver(third_token));
    assert_eq!(app.broadcast_summary().receivers, 1);
}

#[test]
fn a_read_only_receiver_is_skipped_and_a_read_only_focused_pane_sends_nothing() {
    let (mut app, _terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, second) = recorded_split(&mut app);
    let (third_token, third) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);
    add_receiver(&mut app, third_token);
    assert!(app.set_pane_read_only(third_token, true));
    app.focus_session_token_for_test(first_token);

    type_char(&mut app, 'e');
    assert_eq!(bytes(&second), b"e");
    assert!(
        bytes(&third).is_empty(),
        "read-only receivers never receive"
    );

    // A read-only focused pane originates nothing at all.
    assert!(app.set_pane_read_only(first_token, true));
    type_char(&mut app, 'f');
    assert_eq!(bytes(&first), b"e");
    assert_eq!(bytes(&second), b"e");
}

#[test]
fn a_failing_receiver_is_dropped_with_a_notice_and_the_rest_still_receive() {
    let (mut app, _terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let failing: PtyWriter = Arc::new(Mutex::new(Box::new(FailingWriter)));
    let (failing_token, _terminal) = split(&mut app, failing);
    let (healthy_token, healthy) = recorded_split(&mut app);
    add_receiver(&mut app, failing_token);
    add_receiver(&mut app, healthy_token);
    app.focus_session_token_for_test(first_token);

    type_char(&mut app, 'g');

    assert_eq!(bytes(&first), b"g");
    assert_eq!(bytes(&healthy), b"g");
    assert!(
        !app.is_broadcast_receiver(failing_token),
        "only it is dropped"
    );
    assert!(app.is_broadcast_receiver(healthy_token));
    let notice = app.open_notice_message_for_test().expect("notice");
    assert!(
        notice.starts_with("Broadcast stopped for ") && notice.ends_with("input not delivered"),
        "{notice}"
    );
}

#[test]
fn a_newline_paste_writes_nothing_before_confirm_and_cancel_sends_nothing() {
    let (mut app, _terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_writer, second) = recording();
    let (second_token, second_terminal) = split(&mut app, second_writer);
    add_receiver(&mut app, second_token);
    app.focus_session_token_for_test(first_token);
    // The receiver encodes the paste for its own terminal's mode.
    second_terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b[?2004h");

    app.inject_paste_text_for_test("ls\nrm -i x\n");
    app.handle_paste_shortcut_for_test();
    assert!(
        app.risky_paste_pending_for_test(),
        "any newline is confirmed"
    );
    assert!(bytes(&first).is_empty());
    assert!(bytes(&second).is_empty());

    app.cancel_risky_paste_for_test();
    assert!(bytes(&first).is_empty(), "cancel sends nothing anywhere");
    assert!(bytes(&second).is_empty());

    app.inject_paste_text_for_test("ls\nrm -i x\n");
    app.handle_paste_shortcut_for_test();
    app.confirm_risky_paste_for_test(false);
    let text = "ls\nrm -i x\n";
    assert_eq!(
        bytes(&first),
        flatten_chunks(&encode_paste_chunks(text, false, PASTE_CHUNK_SIZE))
    );
    assert_eq!(
        bytes(&second),
        flatten_chunks(&encode_paste_chunks(text, true, PASTE_CHUNK_SIZE)),
        "the receiver's own bracketed mode frames its copy"
    );
}

#[test]
fn a_single_line_paste_goes_out_without_a_confirmation() {
    let (mut app, _terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, second) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);
    app.focus_session_token_for_test(first_token);

    app.inject_paste_text_for_test("echo one line");
    app.handle_paste_shortcut_for_test();

    assert!(!app.risky_paste_pending_for_test());
    assert_eq!(bytes(&first), b"echo one line");
    assert_eq!(bytes(&second), b"echo one line");
}

#[test]
fn escape_is_delivered_to_receivers() {
    let (mut app, _terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, second) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);
    app.focus_session_token_for_test(first_token);

    app.drive_raw_key_event_for_test(
        WinitKey::Named(NamedKey::Escape),
        WinitKey::Named(NamedKey::Escape),
        PhysicalKey::Code(KeyCode::Escape),
        Modifiers::NONE,
        KeyEventType::Press,
    );

    assert_eq!(bytes(&first), b"\x1b");
    assert_eq!(bytes(&second), b"\x1b");
    assert!(app.broadcast_active(), "Escape is not the escape hatch");
}

#[test]
fn ctrl_shift_x_clears_the_set_writes_zero_bytes_and_withdraws_a_pending_paste() {
    let (mut app, _terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, second) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);
    add_receiver(&mut app, first_token);
    app.focus_session_token_for_test(first_token);

    // Works with a confirmation open: it sits above the overlay guard.
    app.inject_paste_text_for_test("a\nb\n");
    app.handle_paste_shortcut_for_test();
    assert!(app.risky_paste_pending_for_test());

    press_ctrl_shift_x(&mut app);

    assert!(!app.broadcast_active());
    assert!(
        !app.risky_paste_pending_for_test(),
        "the stale confirm is withdrawn"
    );
    assert!(
        bytes(&first).is_empty(),
        "the chord is never written to a PTY"
    );
    assert!(bytes(&second).is_empty());

    // With the set already empty the chord still writes nothing.
    press_ctrl_shift_x(&mut app);
    assert!(bytes(&first).is_empty());
}

fn ctrl_shift(ch: char) -> KeyChord {
    KeyChord {
        modifiers: KeyBindingModifiers {
            ctrl: true,
            shift: true,
            alt: false,
            super_key: false,
        },
        key: KeyBindingKey::Character(ch),
    }
}

#[test]
fn ctrl_shift_x_is_unbound_in_the_default_global_binding_table() {
    let bindings = KeyBindings::default();
    // No action other than Stop Broadcast claims the chord in the default
    // table, so installing it took no key from another feature.
    for action in BindableAction::ALL {
        if action == BindableAction::StopBroadcast {
            continue;
        }
        assert!(
            !bindings
                .chords_for_action(action)
                .contains(&ctrl_shift('x')),
            "{action:?} also claims Ctrl+Shift+X"
        );
    }
    assert_eq!(
        bindings.action_for_chord(ctrl_shift('x')),
        Some(BindableAction::StopBroadcast)
    );
    assert_eq!(
        bindings.chord_for_action(BindableAction::ToggleBroadcast),
        None
    );
    // Every other default Ctrl+Shift letter binding is unchanged.
    let expected = [
        ('a', BindableAction::SessionAttach),
        ('b', BindableAction::ThemeBuilder),
        ('c', BindableAction::Copy),
        ('d', BindableAction::DuplicateTab),
        ('e', BindableAction::SplitColumns),
        ('f', BindableAction::Search),
        ('g', BindableAction::WorkspacePicker),
        ('h', BindableAction::ThemePicker),
        ('k', BindableAction::ClearInput),
        ('l', BindableAction::Hints),
        ('n', BindableAction::NewWindow),
        ('o', BindableAction::SplitRows),
        ('p', BindableAction::CommandPalette),
        ('r', BindableAction::SessionReplay),
        ('s', BindableAction::ConnectionManager),
        ('t', BindableAction::NewTab),
        ('v', BindableAction::Paste),
        ('w', BindableAction::CloseTab),
    ];
    for (ch, action) in expected {
        assert_eq!(
            bindings.action_for_chord(ctrl_shift(ch)),
            Some(action),
            "Ctrl+Shift+{ch}"
        );
    }
    for ch in ['i', 'j', 'm', 'q', 'u', 'y', 'z'] {
        assert_eq!(
            bindings.action_for_chord(ctrl_shift(ch)),
            None,
            "Ctrl+Shift+{ch}"
        );
    }
}

#[test]
fn mouse_reports_are_not_broadcast() {
    let (mut app, terminal, first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_writer, second) = recording();
    let (second_token, second_terminal) = split(&mut app, second_writer);
    add_receiver(&mut app, second_token);
    app.focus_session_token_for_test(first_token);
    for terminal in [&terminal, &second_terminal] {
        terminal
            .lock()
            .expect("terminal")
            .advance(b"\x1b[?1002h\x1b[?1006h");
    }
    app.set_pointer_cell_for_test(2, 3);

    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);

    assert!(
        !bytes(&first).is_empty(),
        "the focused pane gets its report"
    );
    assert!(
        bytes(&second).is_empty(),
        "mouse reports stay on the focused pane"
    );
}

#[test]
fn hidden_and_remote_receivers_are_counted() {
    let (mut app, _terminal, _first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, _second) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);
    let dimensions = NativeOptions::default().initial_grid;
    let (writer, _recorded) = recording();
    let position = app.push_headless_session_for_test(
        Arc::new(Mutex::new(Terminal::new(
            dimensions.columns,
            dimensions.rows,
        ))),
        writer,
        dimensions,
    );
    assert!(app.switch_to_session_for_test(position));
    let other_tab = app.active_session_token_for_test();
    app.set_active_remote_upload_for_test("deploy@host.example.invalid");
    app.handle_palette_action_for_test("toggle-broadcast");
    assert!(app.is_broadcast_receiver(other_tab));

    // From the new tab the split pane is hidden; the remote pane is visible.
    assert_eq!(
        app.broadcast_summary(),
        BroadcastSummary {
            receivers: 2,
            hidden: 1,
            remote: 1,
        }
    );
    app.focus_session_token_for_test(first_token);
    assert_eq!(
        app.broadcast_summary(),
        BroadcastSummary {
            receivers: 2,
            hidden: 1,
            remote: 1,
        },
        "from the first tab the remote pane is the hidden one"
    );
}

#[test]
fn restore_starts_from_an_empty_set_and_never_adds_restored_panes() {
    let (mut app, _terminal, _first) = app();
    let (second_token, _second) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);

    let snapshot = app.capture_shape_for_test();
    let json = snapshot.to_json_pretty();
    assert!(
        !json.to_ascii_lowercase().contains("broadcast"),
        "the set is never written to disk"
    );

    // Appending the saved layout adds panes; none of them joins the set.
    let before = app.broadcast_summary().receivers;
    app.append_snapshot_headless_for_test(&snapshot);
    assert_eq!(app.broadcast_summary().receivers, before);

    // A new process window (or a relaunch) restores into an empty set.
    let (mut restored, _terminal, _recorded) = app_after_restore(&snapshot);
    assert!(!restored.broadcast_active());
    restored.handle_palette_action_for_test("stop-broadcast");
    assert!(!restored.broadcast_active());
}

fn app_after_restore(
    snapshot: &crate::native::persistence::ShapeSnapshot,
) -> (App, Arc<Mutex<Terminal>>, Recorded) {
    let (mut restored, terminal, recorded) = app();
    restored.append_snapshot_headless_for_test(snapshot);
    (restored, terminal, recorded)
}

#[test]
fn closing_a_receiver_pane_drops_it_from_the_set() {
    let (mut app, _terminal, _first) = app();
    let (second_token, _second) = recorded_split(&mut app);
    add_receiver(&mut app, second_token);
    app.focus_session_token_for_test(second_token);

    app.close_focused_pane_for_test();

    assert!(
        !app.broadcast_active(),
        "a pane that no longer exists is not kept"
    );
}

#[test]
fn the_label_joins_the_render_signature_through_the_real_input_path() {
    use crate::native::render_helpers::OverlayFragment;
    let (mut app, _terminal, _first) = app();
    let first_token = app.active_session_token_for_test();
    let (second_token, _second) = recorded_split(&mut app);
    app.focus_session_token_for_test(first_token);
    assert_eq!(app.broadcast_overlay_signature(), OverlayFragment::Inert);

    add_receiver(&mut app, second_token);
    assert_eq!(
        app.broadcast_overlay_signature(),
        OverlayFragment::Broadcast {
            receivers: 1,
            hidden: 0,
            remote: 0,
        }
    );
    assert_eq!(
        app.broadcast_label_for(second_token, false),
        Some(BroadcastLabel::Receiver)
    );

    press_ctrl_shift_x(&mut app);
    assert_eq!(app.broadcast_overlay_signature(), OverlayFragment::Inert);
    assert_eq!(app.broadcast_label_for(second_token, false), None);
}

fn label_row(snapshot: &crate::core::Snapshot) -> String {
    snapshot.cells[..snapshot.dimensions.columns]
        .iter()
        .map(|cell| cell.ch)
        .collect()
}

#[test]
fn labels_paint_beside_read_only_and_fall_back_on_narrow_panes() {
    let summary = BroadcastSummary {
        receivers: 3,
        hidden: 1,
        remote: 1,
    };
    let mut terminal = Terminal::new(60, 4);
    let mut snapshot = terminal.snapshot();
    paint_broadcast_label(
        &mut snapshot,
        Some(&BroadcastLabel::Summary(summary)),
        false,
    );
    assert!(
        label_row(&snapshot).ends_with(" BROADCAST 3 hidden 1 remote 1  "),
        "{:?}",
        label_row(&snapshot)
    );

    let mut snapshot = terminal.snapshot();
    crate::native::app::read_only::paint_read_only_label(&mut snapshot, true);
    paint_broadcast_label(&mut snapshot, Some(&BroadcastLabel::Receiver), true);
    assert!(
        label_row(&snapshot).ends_with(&format!("{RECEIVER_LABEL} READ-ONLY  ")),
        "{:?}",
        label_row(&snapshot)
    );

    terminal.resize(14, 4);
    let mut narrow = terminal.snapshot();
    paint_broadcast_label(&mut narrow, Some(&BroadcastLabel::Summary(summary)), false);
    assert!(
        label_row(&narrow).contains("BC 3 h1 r1"),
        "{:?}",
        label_row(&narrow)
    );

    let mut untouched = terminal.snapshot();
    let before = label_row(&untouched);
    paint_broadcast_label(&mut untouched, None, false);
    assert_eq!(
        label_row(&untouched),
        before,
        "broadcast off paints nothing"
    );
}

#[test]
fn fanout_reports_each_receiver_outcome_for_the_diagnostics_trace() {
    let (mut app, _terminal, _first) = app();
    let first_token = app.active_session_token_for_test();
    let (delivered_token, delivered) = recorded_split(&mut app);
    let (read_only_token, read_only) = recorded_split(&mut app);
    let failing: PtyWriter = Arc::new(Mutex::new(Box::new(FailingWriter)));
    let (failing_token, _terminal) = split(&mut app, failing);
    for token in [first_token, delivered_token, read_only_token, failing_token] {
        add_receiver(&mut app, token);
    }
    assert!(app.set_pane_read_only(read_only_token, true));
    // A receiver whose pane no window resolves and no sibling window can own.
    let gone = SessionToken(u64::MAX);
    crate::native::lock_recover(&app.broadcast_handle())
        .insert(gone, ReceiverInfo { remote: false });
    app.focus_session_token_for_test(first_token);

    let trace = app.broadcast_to_receivers(BroadcastPayload::Bytes(b"x".to_vec()));
    assert_eq!(
        trace,
        vec![
            (gone.0, FanoutOutcome::Pruned),
            (first_token.0, FanoutOutcome::Focused),
            (delivered_token.0, FanoutOutcome::Delivered),
            (read_only_token.0, FanoutOutcome::ReadOnly),
            (failing_token.0, FanoutOutcome::WriteFailed),
        ]
    );
    assert_eq!(bytes(&delivered), b"x");
    assert!(bytes(&read_only).is_empty());

    // With sibling windows present the same unresolved token is queued for
    // its owner instead of pruned.
    crate::native::lock_recover(&app.broadcast_handle())
        .insert(gone, ReceiverInfo { remote: false });
    app.adopt_broadcast(app.broadcast_handle(), true);
    let trace = app.broadcast_to_receivers(BroadcastPayload::Bytes(b"y".to_vec()));
    assert!(
        trace.contains(&(gone.0, FanoutOutcome::QueuedOtherWindow)),
        "{trace:?}"
    );

    // No receivers: nothing is reported.
    app.handle_palette_action_for_test("stop-broadcast");
    assert!(
        app.broadcast_to_receivers(BroadcastPayload::Bytes(b"z".to_vec()))
            .is_empty()
    );
}

#[test]
fn phase9_mixed_local_remote_hidden_receivers_encode_their_own_paste_modes() {
    let (mut app, _terminal, origin) = app();
    let origin_token = app.active_session_token_for_test();
    let (local_token, local) = recorded_split(&mut app);
    add_receiver(&mut app, local_token);
    let dimensions = NativeOptions::default().initial_grid;
    let remote_terminal = Arc::new(Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    remote_terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b[?2004h");
    let (remote_writer, remote) = recording();
    let position = app.push_headless_session_for_test(remote_terminal, remote_writer, dimensions);
    assert!(app.switch_to_session_for_test(position));
    let remote_token = app.active_session_token_for_test();
    app.set_active_remote_upload_for_test("test@host.example.invalid");
    add_receiver(&mut app, remote_token);
    app.focus_session_token_for_test(origin_token);
    assert_eq!(
        app.broadcast_summary(),
        BroadcastSummary {
            receivers: 2,
            hidden: 1,
            remote: 1,
        }
    );

    type_char(&mut app, 'a');
    assert_eq!(bytes(&origin), b"a");
    assert_eq!(bytes(&local), b"a");
    assert_eq!(bytes(&remote), b"a");
    for recorded in [&origin, &local, &remote] {
        recorded.lock().expect("bytes").clear();
    }
    let text = "echo first\necho second\n";
    app.inject_paste_text_for_test(text);
    app.handle_paste_shortcut_for_test();
    assert!(app.risky_paste_pending_for_test());
    for recorded in [&origin, &local, &remote] {
        assert!(
            bytes(recorded).is_empty(),
            "confirmation precedes all writes"
        );
    }
    app.confirm_risky_paste_for_test(false);
    assert_eq!(
        bytes(&origin),
        flatten_chunks(&encode_paste_chunks(text, false, PASTE_CHUNK_SIZE))
    );
    assert_eq!(bytes(&local), bytes(&origin));
    assert_eq!(
        bytes(&remote),
        flatten_chunks(&encode_paste_chunks(text, true, PASTE_CHUNK_SIZE))
    );
}

#[test]
fn phase9_stacked_hidden_receiver_is_rechecked_before_paste_confirmation() {
    let (mut app, _terminal, origin) = app();
    let origin_token = app.active_session_token_for_test();
    let (receiver_token, receiver) = recorded_split(&mut app);
    add_receiver(&mut app, receiver_token);
    // Layout actions require a real content rectangle for each session.
    for token in app.active_tab_pane_tokens_for_test() {
        app.focus_session_token_for_test(token);
        app.set_test_cell_for_test(cell(8, 16));
        app.set_test_surface_for_test(800, 416, WindowPadding::ZERO);
    }
    app.focus_session_token_for_test(origin_token);
    app.reflow_active_panes_for_test();
    app.handle_palette_action_for_test("stack-panes");
    assert!(app.workspace_set().active_shows_only_focused());
    assert_eq!(
        app.broadcast_summary(),
        BroadcastSummary {
            receivers: 1,
            hidden: 1,
            remote: 0,
        }
    );
    type_char(&mut app, 's');
    assert_eq!(bytes(&origin), b"s");
    assert_eq!(
        bytes(&receiver),
        b"s",
        "stacked visibility never gates input"
    );
    origin.lock().expect("bytes").clear();
    receiver.lock().expect("bytes").clear();
    app.inject_paste_text_for_test("echo safe\n");
    app.handle_paste_shortcut_for_test();
    assert!(app.risky_paste_pending_for_test());
    assert!(app.set_pane_read_only(receiver_token, true));
    app.confirm_risky_paste_for_test(false);
    assert_eq!(bytes(&origin), b"echo safe\r");
    assert!(
        bytes(&receiver).is_empty(),
        "policy changes after opening confirmation are enforced"
    );
    assert!(
        app.is_broadcast_receiver(receiver_token),
        "read-only does not silently alter membership"
    );
}

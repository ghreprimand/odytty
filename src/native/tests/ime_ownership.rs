// SPDX-License-Identifier: GPL-3.0-only
//! A composition that began on one pane never commits into another, through
//! the real IME handler with recording writers: switching panes and then an
//! empty pre-edit or an IME enable/disable edge ends the composition without
//! handing its late commit to the newly active pane, however late it arrives,
//! while a composition that starts on the new pane commits there and a direct
//! commit after a key press, pointer press or window focus change reaches the
//! pane active at that point.

use std::io::{self, Write};

use winit::event::Ime;

use super::*;
use crate::native::session::SessionToken;

#[derive(Clone, Default)]
struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

type Recorded = Arc<Mutex<Vec<u8>>>;

/// Two panes with recording writers; the first is focused. Returns the app,
/// the two tokens, and their recorded bytes.
fn two_panes() -> (App, [SessionToken; 2], [Recorded; 2]) {
    let first = RecordingWriter::default();
    let first_bytes = first.0.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(first)));
    let (mut app, _terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );
    let a = app.active_session_token_for_test();
    let second = RecordingWriter::default();
    let second_bytes = second.0.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(second)));
    let dims = Dimensions::new(39, 24);
    let pane = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    app.seed_headless_split_pane_for_test(true, pane, writer, dims);
    let b = app.active_session_token_for_test();
    app.focus_session_token_for_test(a);
    (app, [a, b], [first_bytes, second_bytes])
}

fn bytes(recorded: &Recorded) -> Vec<u8> {
    recorded.lock().expect("bytes").clone()
}

#[test]
fn a_late_commit_after_an_empty_preedit_never_reaches_the_new_pane() {
    for edge in [
        Ime::Preedit(String::new(), None),
        Ime::Enabled,
        Ime::Disabled,
    ] {
        let (mut app, [_, b], [first, second]) = two_panes();
        app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
        app.focus_session_token_for_test(b);
        app.handle_ime(edge.clone());
        app.handle_ime(Ime::Commit("\u{4e2d}".to_owned()));
        assert!(
            bytes(&second).is_empty(),
            "{edge:?}: the old composition's commit stays out of the new pane"
        );
        assert!(
            bytes(&first).is_empty(),
            "{edge:?}: nor reaches the old pane"
        );
    }
}

#[test]
fn a_composition_started_on_the_new_pane_commits_there() {
    let (mut app, [_, b], [_, second]) = two_panes();
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    app.focus_session_token_for_test(b);
    app.handle_ime(Ime::Preedit(String::new(), None));
    app.handle_ime(Ime::Preedit("\u{6587}".to_owned(), None));
    app.handle_ime(Ime::Commit("\u{6587}".to_owned()));
    assert_eq!(bytes(&second), "\u{6587}".as_bytes());
}

#[test]
fn a_commit_without_a_composition_reaches_the_active_pane() {
    let (mut app, _, [first, _]) = two_panes();
    app.handle_ime(Ime::Commit("\u{e9}".to_owned()));
    assert_eq!(bytes(&first), "\u{e9}".as_bytes());
}

/// Compose on the first pane, switch to the second, and end the composition
/// there with an empty pre-edit.
fn cancelled_composition_on_first_pane() -> (App, [SessionToken; 2], [Recorded; 2]) {
    let (mut app, panes, recorded) = two_panes();
    app.handle_ime(Ime::Preedit("\u{4e2d}".to_owned(), None));
    app.focus_session_token_for_test(panes[1]);
    app.handle_ime(Ime::Preedit(String::new(), None));
    (app, panes, recorded)
}

/// Elapsed time is not authority: a commit that arrives long after the old
/// composition ended, with no input on the new pane in between, is refused.
#[test]
fn a_late_commit_stays_refused_however_late_it_arrives() {
    let (mut app, _, [first, second]) = cancelled_composition_on_first_pane();
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    app.handle_ime(Ime::Commit("\u{4e2d}".to_owned()));
    assert!(bytes(&second).is_empty(), "the aged late commit is refused");
    assert!(bytes(&first).is_empty(), "nor reaches the old pane");
}

/// A key press, a pointer press or a window focus change on the new pane is
/// the user acting there, so a direct commit after it (an emoji picker, an
/// on-screen keyboard) is delivered to the pane active at that point.
#[test]
fn a_direct_commit_after_an_explicit_transition_reaches_the_active_pane() {
    for transition in ["key", "pointer", "focus-out", "focus-in"] {
        let (mut app, [a, _], recorded) = cancelled_composition_on_first_pane();
        match transition {
            "key" => app.drive_named_key_for_test(winit::keyboard::NamedKey::Shift),
            "pointer" => {
                app.mouse_left_press_for_test();
                app.mouse_left_release_for_test();
            }
            "focus-out" => app.on_window_focus_changed_for_test(false),
            _ => app.on_window_focus_changed_for_test(true),
        }
        // A pointer press can itself focus a pane; the commit belongs to
        // whichever pane is active after the transition.
        let target = usize::from(app.active_session_token_for_test() != a);
        let before = recorded.each_ref().map(|r| bytes(r).len());
        app.handle_ime(Ime::Commit("\u{1f600}".to_owned()));
        assert_eq!(
            &bytes(&recorded[target])[before[target]..],
            "\u{1f600}".as_bytes(),
            "{transition}: the direct commit reaches the active pane"
        );
        assert_eq!(
            bytes(&recorded[1 - target]).len(),
            before[1 - target],
            "{transition}: and only that pane"
        );
    }
}

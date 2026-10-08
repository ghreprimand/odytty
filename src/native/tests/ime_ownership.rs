// SPDX-License-Identifier: GPL-3.0-only
//! A composition that began on one pane never commits into another, through
//! the real IME handler with recording writers: switching panes and then an
//! empty pre-edit or an IME enable/disable edge ends the composition without
//! handing its late commit to the newly active pane, while a composition that
//! starts on the new pane commits there.

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

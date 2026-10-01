// SPDX-License-Identifier: GPL-3.0-only
//! A consumed character chord must not deliver its glyph to any pane.
//!
//! Ctrl+Shift+N (and the other default Ctrl+Shift letter chords) consume the
//! press. Windows can still deliver the key-up, a modifier-less press of that
//! letter, or a one-character IME commit after the modifier cache has been
//! cleared, including to the new window. On the new window the press arrives
//! after the key-up, because the new window already has focus. Those leftovers
//! are not shell input.

use std::io::{self, Write};

use super::*;
use winit::event::Ime;

#[derive(Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct RecordingApp {
    app: App,
    bytes: Arc<Mutex<Vec<u8>>>,
    terminal: Arc<Mutex<Terminal>>,
}

fn recording_app() -> RecordingApp {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
        writer,
    );
    RecordingApp {
        app,
        bytes,
        terminal,
    }
}

fn bytes_of(bytes: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    bytes.lock().expect("bytes").clone()
}

fn press_chord(app: &mut App, ch: char) {
    app.drive_char_with_mods_typed_for_test(ch, true, true, KeyEventType::Press);
}

fn press_bare(app: &mut App, ch: char) {
    app.drive_char_with_mods_typed_for_test(ch, false, false, KeyEventType::Press);
}

fn release_bare(app: &mut App, ch: char) {
    app.drive_char_with_mods_typed_for_test(ch, false, false, KeyEventType::Release);
}

#[test]
fn ctrl_shift_letter_chords_drop_the_bare_press_of_that_letter() {
    for ch in ['n', 't', 'w'] {
        let RecordingApp {
            mut app,
            bytes,
            terminal: _terminal,
        } = recording_app();
        press_chord(&mut app, ch);
        press_bare(&mut app, ch);
        assert!(
            bytes_of(&bytes).is_empty(),
            "Ctrl+Shift+{ch} must not deliver '{ch}'"
        );
    }
}

#[test]
fn ctrl_shift_n_drops_a_one_character_ime_commit_and_keeps_later_text() {
    let RecordingApp {
        mut app,
        bytes,
        terminal: _terminal,
    } = recording_app();
    press_chord(&mut app, 'n');
    app.handle_ime(Ime::Commit("n".into()));
    assert!(
        bytes_of(&bytes).is_empty(),
        "the chord's IME commit is not shell input"
    );
    app.handle_ime(Ime::Commit("n".into()));
    assert_eq!(bytes_of(&bytes), b"n");
    app.handle_ime(Ime::Commit("ok".into()));
    assert_eq!(bytes_of(&bytes), b"nok");
}

#[test]
fn kitty_all_keys_release_of_a_consumed_chord_writes_nothing() {
    let RecordingApp {
        mut app,
        bytes,
        terminal,
    } = recording_app();
    terminal.lock().expect("terminal").advance(b"\x1b[=10u");
    press_chord(&mut app, 'n');
    release_bare(&mut app, 'n');
    assert!(
        bytes_of(&bytes).is_empty(),
        "kitty key-up of a consumed chord must not reach the PTY"
    );
    press_bare(&mut app, 'n');
    assert!(
        !bytes_of(&bytes).is_empty(),
        "an unmodified n after the chord is released is real input"
    );
}

#[test]
fn new_window_request_arms_the_sibling_and_a_pointer_request_does_not() {
    let RecordingApp {
        mut app,
        bytes,
        terminal: _terminal,
    } = recording_app();
    press_chord(&mut app, 'n');
    let request = app
        .take_new_window_request_for_test()
        .expect("new window request");
    assert_eq!(request.suppress_character, Some('n'));
    assert!(bytes_of(&bytes).is_empty());

    let RecordingApp {
        app: mut sibling,
        bytes: sibling_bytes,
        terminal: _terminal,
    } = recording_app();
    sibling.inherit_consumed_chord(request.suppress_character);
    release_bare(&mut sibling, 'n');
    sibling.handle_ime(Ime::Commit("N".into()));
    assert!(
        bytes_of(&sibling_bytes).is_empty(),
        "the new window must drop the chord's release and IME commit"
    );
    press_bare(&mut sibling, 'n');
    assert_eq!(bytes_of(&sibling_bytes), b"n");

    let RecordingApp {
        app: mut pointer,
        bytes: pointer_bytes,
        terminal: _terminal,
    } = recording_app();
    pointer.inherit_consumed_chord(None);
    press_bare(&mut pointer, 'n');
    assert_eq!(bytes_of(&pointer_bytes), b"n");
}

#[test]
fn inherited_latch_drops_the_press_that_follows_the_release() {
    let RecordingApp {
        app: mut sibling,
        bytes,
        terminal: _terminal,
    } = recording_app();
    sibling.inherit_consumed_chord(Some('n'));
    release_bare(&mut sibling, 'n');
    press_bare(&mut sibling, 'n');
    assert!(
        bytes_of(&bytes).is_empty(),
        "the new window's press after the key-up is still the chord glyph"
    );
    press_bare(&mut sibling, 'n');
    assert_eq!(bytes_of(&bytes), b"n");
}

#[test]
fn inherited_latch_survives_a_modifier_press() {
    let RecordingApp {
        app: mut sibling,
        bytes,
        terminal: _terminal,
    } = recording_app();
    sibling.inherit_consumed_chord(Some('n'));
    sibling.drive_named_key_for_test(winit::keyboard::NamedKey::Control);
    press_bare(&mut sibling, 'n');
    assert!(
        bytes_of(&bytes).is_empty(),
        "a modifier press on the new window must not forget the chord letter"
    );
}

#[test]
fn inherited_latch_drops_a_preedited_ime_commit() {
    let RecordingApp {
        app: mut sibling,
        bytes,
        terminal: _terminal,
    } = recording_app();
    sibling.inherit_consumed_chord(Some('n'));
    sibling.handle_ime(Ime::Preedit("n".into(), None));
    sibling.handle_ime(Ime::Commit("n".into()));
    assert!(
        bytes_of(&bytes).is_empty(),
        "a one-character pre-edit of the chord letter is still the chord glyph"
    );
    sibling.handle_ime(Ime::Commit("n".into()));
    assert_eq!(bytes_of(&bytes), b"n");
}

#[test]
fn inherited_latch_ends_on_a_different_character() {
    let RecordingApp {
        app: mut sibling,
        bytes,
        terminal: _terminal,
    } = recording_app();
    sibling.inherit_consumed_chord(Some('n'));
    press_bare(&mut sibling, 'x');
    press_bare(&mut sibling, 'n');
    assert_eq!(bytes_of(&bytes), b"xn");
}

#[cfg(windows)]
#[test]
fn win32_input_release_of_a_consumed_chord_writes_nothing() {
    let RecordingApp {
        mut app,
        bytes,
        terminal,
    } = recording_app();
    terminal.lock().expect("terminal").advance(b"\x1b[?9001h");
    press_chord(&mut app, 'n');
    release_bare(&mut app, 'n');
    assert!(
        bytes_of(&bytes).is_empty(),
        "Win32 key-up of a consumed chord must not reach the PTY"
    );
    press_bare(&mut app, 'n');
    assert!(
        !bytes_of(&bytes).is_empty(),
        "an unmodified n after the chord is released is real input"
    );
}

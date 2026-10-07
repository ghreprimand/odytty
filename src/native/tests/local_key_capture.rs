// SPDX-License-Identifier: GPL-3.0-only
//! Keys consumed by local UI never reach the PTY as half an event.
//!
//! Search, the multiplexer prefix, and held exit take a key press for
//! themselves. Kitty event reporting and Win32 input mode also encode key
//! releases (and repeats), so a release whose press stayed local must not be
//! written to the pane. A release whose press did reach the pane still is.
//! Toggle shortcuts act once per press: a held chord's repeats are consumed.

use std::io::Write;

use super::*;
use winit::keyboard::KeyCode;

type Recorded = Arc<Mutex<Vec<u8>>>;

struct RecordingWriter(Recorded);

impl Write for RecordingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("bytes").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn recording_writer() -> (PtyWriter, Recorded) {
    let recorded = Recorded::default();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter(recorded.clone()))));
    (writer, recorded)
}

fn recording_app(settings: Settings, modes: &[u8]) -> (App, Recorded) {
    let (writer, recorded) = recording_writer();
    let (app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        settings,
        writer,
    );
    terminal.lock().expect("terminal").advance(modes);
    (app, recorded)
}

fn take(recorded: &Recorded) -> Vec<u8> {
    std::mem::take(&mut *recorded.lock().expect("bytes"))
}

const KITTY_EVENTS: &[u8] = b"\x1b[=10u";
#[cfg(windows)]
const WIN32_INPUT: &[u8] = b"\x1b[?9001h";

fn mods(ctrl: bool, shift: bool) -> Modifiers {
    Modifiers {
        ctrl,
        shift,
        alt: false,
    }
}

fn char_event(app: &mut App, ch: char, code: KeyCode, held: Modifiers, kind: KeyEventType) {
    let logical = WinitKey::Character(ch.to_string().into());
    app.drive_raw_key_event_for_test(
        logical.clone(),
        logical,
        PhysicalKey::Code(code),
        held,
        kind,
    );
}

fn named_event(app: &mut App, key: NamedKey, code: KeyCode, kind: KeyEventType) {
    let logical = WinitKey::Named(key);
    app.drive_raw_key_event_for_test(
        logical.clone(),
        logical,
        PhysicalKey::Code(code),
        Modifiers::default(),
        kind,
    );
}

fn open_search(app: &mut App) {
    char_event(
        app,
        'f',
        KeyCode::KeyF,
        mods(true, true),
        KeyEventType::Press,
    );
    assert!(app.search_open_for_test(), "Ctrl+Shift+F opens search");
    char_event(
        app,
        'f',
        KeyCode::KeyF,
        mods(true, true),
        KeyEventType::Release,
    );
}

/// Type into search, close it with Escape, then type one key into the pane.
/// Only the last key's press and release reach the PTY.
fn search_keys_stay_local(modes: &[u8]) {
    let (mut app, recorded) = recording_app(Settings::default(), modes);
    open_search(&mut app);
    for kind in [
        KeyEventType::Press,
        KeyEventType::Repeat,
        KeyEventType::Release,
    ] {
        char_event(&mut app, 'a', KeyCode::KeyA, Modifiers::default(), kind);
    }
    assert_eq!(
        app.search_query_for_test(),
        "aa",
        "press and repeat type into the search field"
    );
    named_event(
        &mut app,
        NamedKey::Escape,
        KeyCode::Escape,
        KeyEventType::Press,
    );
    assert!(!app.search_open_for_test(), "Escape closes search");
    named_event(
        &mut app,
        NamedKey::Escape,
        KeyCode::Escape,
        KeyEventType::Release,
    );
    assert_eq!(
        take(&recorded),
        b"",
        "no release or repeat of a search key reaches the pane"
    );

    char_event(
        &mut app,
        'b',
        KeyCode::KeyB,
        Modifiers::default(),
        KeyEventType::Press,
    );
    let press = take(&recorded);
    assert!(!press.is_empty(), "a pane key press is encoded");
    char_event(
        &mut app,
        'b',
        KeyCode::KeyB,
        Modifiers::default(),
        KeyEventType::Release,
    );
    assert!(
        !take(&recorded).is_empty(),
        "the release of a key the pane saw pressed is still encoded"
    );
}

#[test]
fn kitty_releases_of_search_keys_never_reach_the_pane() {
    search_keys_stay_local(KITTY_EVENTS);
}

/// Win32 input mode changes the encoder only on Windows; elsewhere the mode
/// is tracked and legacy encoding (which sends no release) stays.
#[cfg(windows)]
#[test]
fn win32_releases_of_search_keys_never_reach_the_pane() {
    search_keys_stay_local(WIN32_INPUT);
}

#[test]
fn a_release_after_search_opens_still_reaches_the_pane_that_saw_the_press() {
    let (mut app, recorded) = recording_app(Settings::default(), KITTY_EVENTS);
    char_event(
        &mut app,
        'b',
        KeyCode::KeyB,
        Modifiers::default(),
        KeyEventType::Press,
    );
    assert!(!take(&recorded).is_empty());
    open_search(&mut app);
    take(&recorded);
    char_event(
        &mut app,
        'b',
        KeyCode::KeyB,
        Modifiers::default(),
        KeyEventType::Release,
    );
    assert!(
        !take(&recorded).is_empty(),
        "the pane receives the release of its own press"
    );
}

#[test]
fn prefix_keys_never_send_a_kitty_release() {
    let (mut app, _first) = recording_app(Settings::default(), b"");
    let (writer, recorded) = recording_writer();
    let pane = Arc::new(Mutex::new(Terminal::new(40, 24)));
    pane.lock().expect("terminal").advance(KITTY_EVENTS);
    app.seed_headless_split_pane_for_test(true, pane, writer, Dimensions::new(40, 24));
    assert_eq!(app.active_pane_count_for_test(), 2);

    // Ctrl+B enters the prefix; `z` is the zoom action.
    char_event(
        &mut app,
        'b',
        KeyCode::KeyB,
        mods(true, false),
        KeyEventType::Press,
    );
    char_event(
        &mut app,
        'b',
        KeyCode::KeyB,
        mods(true, false),
        KeyEventType::Release,
    );
    char_event(
        &mut app,
        'z',
        KeyCode::KeyZ,
        Modifiers::default(),
        KeyEventType::Press,
    );
    char_event(
        &mut app,
        'z',
        KeyCode::KeyZ,
        Modifiers::default(),
        KeyEventType::Release,
    );
    assert_eq!(
        take(&recorded),
        b"",
        "the prefix and its action key are local UI input"
    );
}

#[test]
fn holding_the_search_shortcut_opens_search_once() {
    let (mut app, recorded) = recording_app(Settings::default(), b"");
    char_event(
        &mut app,
        'f',
        KeyCode::KeyF,
        mods(true, true),
        KeyEventType::Press,
    );
    for _ in 0..3 {
        char_event(
            &mut app,
            'f',
            KeyCode::KeyF,
            mods(true, true),
            KeyEventType::Repeat,
        );
        assert!(app.search_open_for_test(), "a repeat never closes search");
    }
    assert_eq!(app.search_query_for_test(), "", "repeats type nothing");
    char_event(
        &mut app,
        'f',
        KeyCode::KeyF,
        mods(true, true),
        KeyEventType::Press,
    );
    assert!(!app.search_open_for_test(), "a second press closes search");
    assert_eq!(take(&recorded), b"");
}

#[test]
fn holding_a_read_only_toggle_flips_it_once() {
    let settings = Settings {
        key_bindings: vec![KeyBindingOverride {
            chord: KeyChord {
                modifiers: KeyBindingModifiers {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    super_key: false,
                },
                key: KeyBindingKey::Character('y'),
            },
            action: BindableAction::ToggleReadOnly,
        }],
        ..Settings::default()
    };
    let (mut app, recorded) = recording_app(settings, b"");
    char_event(
        &mut app,
        'y',
        KeyCode::KeyY,
        mods(true, true),
        KeyEventType::Press,
    );
    assert!(app.active_pane_read_only());
    for _ in 0..3 {
        char_event(
            &mut app,
            'y',
            KeyCode::KeyY,
            mods(true, true),
            KeyEventType::Repeat,
        );
        assert!(app.active_pane_read_only(), "a repeat never flips it back");
    }
    assert_eq!(take(&recorded), b"");
}

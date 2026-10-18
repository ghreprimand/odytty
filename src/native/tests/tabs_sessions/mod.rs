// SPDX-License-Identifier: GPL-3.0-only
//! Headless foundation tests for the multi-session tabs feature. The shared
//! fixtures live here; the tests are grouped by topic in the child modules.

use crate::core::Color;
use crate::native::app::interactive_paths::MapProbe;
use crate::native::session::HeadlessSession;
use crate::paths::FsKind;
use crate::settings::{TabBarHeight, TabRailWidth};

use super::super::pty::UserEvent;
use super::*;

#[derive(Clone, Default)]
struct RecordingWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().expect("bytes").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A tab's terminal and the bytes its writer recorded.
type SessionFixture = (Arc<Mutex<Terminal>>, Arc<Mutex<Vec<u8>>>);

/// One recorded pane over a headless session: its terminal, a writer that
/// records every byte the App sends toward the shell, the headless backing
/// state (whose resize calls a test can read), and the recorded bytes. No
/// child process is spawned, so the fixture cannot fail to build and the test
/// cannot skip.
type RecordedSession = (
    Arc<Mutex<Terminal>>,
    PtyWriter,
    Arc<HeadlessSession>,
    Arc<Mutex<Vec<u8>>>,
);

fn recorded_session(dims: Dimensions) -> RecordedSession {
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let headless = Arc::new(HeadlessSession::new(dims));
    (terminal, writer, headless, bytes)
}

/// An App whose first session is a recorded headless pane built from `options`.
fn recorded_app_with(options: NativeOptions) -> (App, RecordedSession) {
    let (terminal, writer, headless, bytes) = recorded_session(options.initial_grid);
    let app = App::new_headless(
        options,
        terminal.clone(),
        writer.clone(),
        headless.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    (app, (terminal, writer, headless, bytes))
}

fn single_session_app() -> (App, Arc<Mutex<Vec<u8>>>) {
    let (app, (_terminal, _writer, _headless, bytes)) = recorded_app_with(NativeOptions::default());
    (app, bytes)
}

fn app_with_two_sessions() -> (App, [SessionFixture; 2]) {
    let (mut app, (terminal_a, _writer_a, _headless_a, bytes_a)) =
        recorded_app_with(NativeOptions::default());
    let (terminal_b, writer_b, headless_b, bytes_b) =
        recorded_session(NativeOptions::default().initial_grid);
    app.push_headless_session_with_for_test(terminal_b.clone(), writer_b, headless_b);
    (app, [(terminal_a, bytes_a), (terminal_b, bytes_b)])
}

fn tab_bar_app() -> App {
    let (app, _fixtures) = app_with_two_sessions();
    app
}

/// Inject a fresh workspace (a single-tab, recorded headless workspace) and
/// switch to it, so the workspace rail has multiple slots and the active
/// workspace is single-tab (no top bar competes with the rail).
fn add_workspace(app: &mut App) {
    let dims = NativeOptions::default().initial_grid;
    let (terminal, writer, headless, _bytes) = recorded_session(dims);
    app.push_headless_workspace_with_for_test(terminal, writer, headless);
}

fn scrollback_bytes(lines: usize) -> Vec<u8> {
    let mut text = String::new();
    for i in 0..lines {
        text.push_str(&format!("row-{i:02}\r\n"));
    }
    text.into_bytes()
}

// --- rail seam drag → manual width, double-click → auto ---

/// A hermetic temp `odytty.conf` for a persistence test (unique per process +
/// test thread, seeded with a comment to prove non-destructive rewrite).
/// A config file in a scratch folder. Keep the folder guard alive for the
/// test; it removes the folder when dropped, including on a panic.
fn temp_rail_conf(tag: &str) -> (crate::test_dirs::ScratchDir, std::path::PathBuf) {
    let dir = crate::test_dirs::ScratchDir::new(&format!("odytty-{tag}-"));
    let path = dir.join("odytty.conf");
    std::fs::write(&path, "# kept\n").unwrap();
    (dir, path)
}

mod panes_and_prefix;
mod rail_autohide;
mod rail_chrome;
mod rename_and_exit;
mod sessions_and_tab_bar;
mod split_geometry;
mod workspace_rail;

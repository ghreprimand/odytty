// SPDX-License-Identifier: GPL-3.0-only
//! Headless foundation tests for the multi-session tabs feature. The shared
//! fixtures live here; the tests are grouped by topic in the child modules.

use crate::core::Color;
use crate::native::app::interactive_paths::MapProbe;
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

type SessionFixture = (
    Arc<Mutex<Terminal>>,
    Arc<Mutex<PtySession>>,
    Arc<Mutex<Vec<u8>>>,
);

#[allow(clippy::type_complexity)]
fn recorded_session(
    dims: Dimensions,
) -> Option<(
    Arc<Mutex<Terminal>>,
    PtyWriter,
    Arc<Mutex<PtySession>>,
    Arc<Mutex<Vec<u8>>>,
)> {
    let session = spawn_test_pause_shell(dims).ok()?;
    let _ = session.take_writer().ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    let pty = Arc::new(Mutex::new(session));
    Some((terminal, writer, pty, bytes))
}

fn single_session_app() -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal, writer, pty, bytes) = recorded_session(dims)?;
    let app = App::new(
        options,
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    Some((app, bytes))
}

fn app_with_two_sessions() -> Option<(App, [SessionFixture; 2])> {
    let options = NativeOptions::default();
    let dims = options.initial_grid;
    let (terminal_a, writer_a, pty_a, bytes_a) = recorded_session(dims)?;
    let (terminal_b, writer_b, pty_b, bytes_b) = recorded_session(dims)?;
    let mut app = App::new(
        options,
        terminal_a.clone(),
        writer_a,
        pty_a.clone(),
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    app.push_session_for_test(terminal_b.clone(), writer_b, pty_b.clone());
    Some((
        app,
        [(terminal_a, pty_a, bytes_a), (terminal_b, pty_b, bytes_b)],
    ))
}

fn tab_bar_app() -> Option<App> {
    let (app, _fixtures) = app_with_two_sessions()?;
    Some(app)
}

/// Inject a fresh workspace (a single-tab, recorded-PTY workspace) and switch to
/// it, so the workspace rail has multiple slots and the active workspace is
/// single-tab (no top bar competes with the rail). Returns `false` when no PTY
/// fixture is available (the caller should already have one from `tab_bar_app`).
fn add_workspace(app: &mut App) -> bool {
    let dims = NativeOptions::default().initial_grid;
    let Some((terminal, writer, pty, _bytes)) = recorded_session(dims) else {
        return false;
    };
    app.push_workspace_for_test(terminal, writer, pty);
    true
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

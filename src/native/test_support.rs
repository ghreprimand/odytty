// SPDX-License-Identifier: GPL-3.0-only
//! Shared helpers for native tests.

use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::core::{Dimensions, Terminal};
use crate::pty::PtySession;

pub(in crate::native) fn spawn_test_pause_shell(dimensions: Dimensions) -> Result<PtySession> {
    #[cfg(unix)]
    const PAUSE_COMMAND: &str = "sleep 1";
    #[cfg(windows)]
    const PAUSE_COMMAND: &str = "ping -n 2 127.0.0.1 >NUL";

    // On Windows, `timeout /t` can fail when stdin is not a console; ping gives
    // these PTY fixture tests the same short-lived hold without interactive I/O.
    PtySession::spawn_shell_command(dimensions, PAUSE_COMMAND)
}

use crate::native::NativeOptions;
use crate::native::app::App;
use crate::native::pty::PtyWriter;
use crate::native::session::HeadlessSession;
use crate::settings::{Settings, SettingsReloader};

/// A discarding [`PtyWriter`] sink: swallows every byte the App writes toward
/// the "shell". Pure UI tests never read it back, so an in-memory sink is a
/// complete stand-in for the real PTY writer with no OS resource.
pub(in crate::native) fn headless_writer() -> PtyWriter {
    Arc::new(Mutex::new(Box::new(std::io::sink())))
}

/// Build a fully-formed [`App`] over a headless (no-PTY) session for pure UI
/// tests, plus the shared terminal handle so a test can drive terminal state.
/// The caller supplies the `writer`, so a test that needs to observe emitted
/// input bytes passes a recording writer; a test that ignores them passes
/// [`headless_writer`]. No OS child, PTY, pump thread, or wake pipe is created,
/// so the fixture cannot inherit a real shell's kill+wait teardown. Never
/// returns `None`: a headless session always constructs.
pub(in crate::native) fn headless_app_with_writer(
    options: NativeOptions,
    dimensions: Dimensions,
    settings: Settings,
    writer: PtyWriter,
) -> (App, Arc<Mutex<Terminal>>) {
    headless_app_built(options, dimensions, settings, writer, true)
}

/// [`headless_app_with_writer`] whose terminal is deliberately UNSEEDED — a
/// bare `Terminal::new` with none of the settings-derived per-session
/// defaults. This is the restore/append spawn shape: sessions those paths
/// build start outside `initialize_session_with` and rely on the App's
/// model-state sweep to seed them afterward. Only tests that pin THAT
/// re-seeding should use this; every other fixture goes through
/// [`headless_app_with_writer`], which seeds via the production launch path.
pub(in crate::native) fn headless_app_unseeded_with(
    options: NativeOptions,
    dimensions: Dimensions,
    settings: Settings,
) -> (App, Arc<Mutex<Terminal>>) {
    headless_app_built(options, dimensions, settings, headless_writer(), false)
}

fn headless_app_built(
    options: NativeOptions,
    dimensions: Dimensions,
    settings: Settings,
    writer: PtyWriter,
    seed: bool,
) -> (App, Arc<Mutex<Terminal>>) {
    let mut model = Terminal::new(dimensions.columns, dimensions.rows);
    if seed {
        // Seed the settings-derived per-session defaults through the SAME
        // helper the production launch path uses
        // (`seed_launch_session_model`), so the harness exercises real
        // startup seeding instead of a hand-maintained copy. This is the
        // regression guard for the launch-pane button-gate gap: a per-session
        // default wired into the launch path is honored here too, and a
        // default missing there is missing here — tests catch the drift.
        super::seed_launch_session_model(&mut model, &settings);
    }
    let terminal = Arc::new(Mutex::new(model));
    let headless = Arc::new(HeadlessSession::new(dimensions));
    let app = App::new_headless(
        options,
        terminal.clone(),
        writer,
        headless,
        settings,
        SettingsReloader::for_current_process(std::time::Instant::now()),
    );
    (app, terminal)
}

/// [`headless_app_with_writer`] with a discarding writer sink, for the common
/// case where a pure UI test never inspects emitted input bytes.
pub(in crate::native) fn headless_app_with(
    options: NativeOptions,
    dimensions: Dimensions,
    settings: Settings,
) -> (App, Arc<Mutex<Terminal>>) {
    headless_app_with_writer(options, dimensions, settings, headless_writer())
}

/// The common pure-UI fixture: default options, an 80x24 grid, and default
/// settings, over a headless session. Returns the App and the shared terminal
/// handle.
pub(in crate::native) fn headless_app_for_test() -> (App, Arc<Mutex<Terminal>>) {
    headless_app_with(
        NativeOptions::default(),
        Dimensions::new(80, 24),
        Settings::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Dimensions;
    use crate::pty::ForegroundJob;

    /// The headless source owns no real PTY: its session exposes a
    /// `headless_session()` handle and no `local_pty()`. This is the whole point
    /// of the seam — a pure UI test must not fork+exec a shell it then has to
    /// synchronously kill and wait on at teardown.
    #[test]
    fn headless_app_owns_no_pty_child() {
        let (app, _terminal) = headless_app_for_test();
        // `App` derefs to its active `Session`.
        assert!(
            app.local_pty().is_none(),
            "a headless session must not own a real PTY child"
        );
        assert!(
            app.headless_session().is_some(),
            "a headless session exposes its headless backing state"
        );
    }

    /// The discarding writer accepts writes without error and without a real
    /// shell on the other end.
    #[test]
    fn headless_writer_swallows_writes() {
        use std::io::Write;
        let writer = headless_writer();
        let mut guard = writer.lock().expect("writer");
        assert!(guard.write_all(b"input bytes toward a shell").is_ok());
        assert!(guard.flush().is_ok());
    }

    /// A caller-supplied recording writer captures exactly the bytes the App
    /// emits, proving a headless fixture can still observe emitted input.
    #[test]
    fn headless_app_with_writer_uses_the_supplied_sink() {
        use std::io::Write;
        let bytes = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink = bytes.clone();
        struct Recorder(Arc<Mutex<Vec<u8>>>);
        impl Write for Recorder {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("bytes").extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer: PtyWriter = Arc::new(Mutex::new(Box::new(Recorder(sink))));
        let (app, _terminal) = headless_app_with_writer(
            NativeOptions::default(),
            Dimensions::new(80, 24),
            Settings::default(),
            writer,
        );
        crate::native::pty::write_chunks_blocking(&app.writer, &[b"abc".to_vec()])
            .expect("headless write");
        assert_eq!(bytes.lock().expect("bytes").as_slice(), b"abc");
    }

    /// The headless backing state records resize geometry (dimensions + call
    /// count) and defaults the foreground job to `Unknown`, with an injection
    /// seam for the running-job branch — the semantics a migrated geometry or
    /// confirm-close test relies on, without any syscall.
    #[test]
    fn headless_session_records_resize_and_reports_injected_foreground_job() {
        let dims = Dimensions::new(80, 24);
        let headless = HeadlessSession::new(dims);
        assert_eq!(headless.resize_call_count(), 0);
        assert_eq!(headless.dimensions(), dims);
        assert_eq!(headless.foreground_job(), ForegroundJob::Unknown);

        // A resize is recorded without a syscall.
        headless.record_cell_metrics(crate::core::CellMetrics::new(8, 16));
        headless.record_resize(Dimensions::new(100, 40));
        assert_eq!(headless.resize_call_count(), 1);
        assert_eq!(headless.dimensions(), Dimensions::new(100, 40));
        assert_eq!(
            headless.cell_metrics(),
            Some(crate::core::CellMetrics::new(8, 16))
        );

        // The foreground job is injectable for a confirm-close test.
        headless.set_foreground_job(ForegroundJob::Running);
        assert_eq!(headless.foreground_job(), ForegroundJob::Running);
    }

    /// A headless App reports a running foreground job when its backing state is
    /// injected, so a confirm-close test can exercise the prompt branch without a
    /// real child.
    #[test]
    fn headless_app_foreground_job_running_reflects_injection() {
        let (app, _terminal) = headless_app_for_test();
        assert!(!app.foreground_job_running(), "default is not running");
        app.headless_session()
            .expect("headless backing")
            .set_foreground_job(ForegroundJob::Running);
        assert!(
            app.foreground_job_running(),
            "an injected running job is reported through the production seam"
        );
    }
}

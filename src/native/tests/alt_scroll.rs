// SPDX-License-Identifier: GPL-3.0-only
//! ALT-SCROLL (DECSET 1007) routing tests. On the alternate screen — which has
//! no scrollback — a wheel notch is translated into cursor-key presses so a TUI
//! that does not track the mouse (a pager, Claude CLI, ...) still scrolls. These
//! pin the routing matrix: alt-screen + reporting-off ⇒ arrows (CSI, or SS3
//! under DECCKM); primary screen ⇒ no PTY write (local scrollback instead);
//! reporting-on ⇒ the normal wheel report wins; 1007 disabled ⇒ no arrows.
//!
//! Headless (no GPU/window): driven through the real `handle_mouse_wheel`
//! routing. Skipped when no PTY is available (CI sandboxes).

use super::*;

const COLS: usize = 80;
const ROWS: usize = 24;

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

/// Build an `App` whose PTY writes are recorded, after feeding `setup` (mode
/// sequences) into the terminal. The recorder is cleared before return so a test
/// only sees bytes the wheel produces. Returns `None` when no PTY is available.
fn app_with_setup(setup: &[u8]) -> Option<(App, Arc<Mutex<Vec<u8>>>)> {
    let dims = Dimensions::new(COLS, ROWS);
    let session = spawn_test_pause_shell(dims).ok()?;
    let recorder = RecordingWriter::default();
    let bytes = recorder.bytes.clone();
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
    let terminal = Arc::new(Mutex::new(Terminal::new(dims.columns, dims.rows)));
    {
        let mut t = terminal.lock().expect("terminal");
        t.advance(setup);
    }
    let pty = Arc::new(Mutex::new(session));
    let app = App::new(
        NativeOptions::default(),
        terminal,
        writer,
        pty,
        Settings::default(),
        crate::settings::SettingsReloader::for_current_process(Instant::now()),
    );
    bytes.lock().expect("bytes").clear();
    Some((app, bytes))
}

fn wheel_up(app: &mut App) {
    app.dispatch_wheel_for_test(1.0);
}
fn wheel_down(app: &mut App) {
    app.dispatch_wheel_for_test(-1.0);
}

fn recorded(bytes: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    bytes.lock().expect("bytes").clone()
}

#[test]
fn alt_screen_wheel_up_sends_up_arrows() {
    // Enter the alternate screen (1049h). A wheel-up notch becomes the default
    // 3 Up cursor keys in CSI form — byte-identical to three real Up presses.
    let Some((mut app, bytes)) = app_with_setup(b"\x1b[?1049h") else {
        return;
    };
    wheel_up(&mut app);
    assert_eq!(recorded(&bytes), b"\x1b[A".repeat(3));
}

#[test]
fn alt_screen_wheel_down_sends_down_arrows() {
    let Some((mut app, bytes)) = app_with_setup(b"\x1b[?1049h") else {
        return;
    };
    wheel_down(&mut app);
    assert_eq!(recorded(&bytes), b"\x1b[B".repeat(3));
}

#[test]
fn alt_screen_decckm_uses_ss3_arrows() {
    // DECCKM application-cursor mode (1h) must yield the SS3 form (\x1bOA), the
    // load-bearing encoding trap: a pager in app-cursor mode would not scroll on
    // the CSI form.
    let Some((mut app, bytes)) = app_with_setup(b"\x1b[?1049h\x1b[?1h") else {
        return;
    };
    wheel_up(&mut app);
    assert_eq!(recorded(&bytes), b"\x1bOA".repeat(3));
}

#[test]
fn primary_screen_wheel_does_not_write_to_pty() {
    // No alternate screen: the wheel moves the local scrollback viewport and
    // never writes to the PTY (alt-scroll translation must not fire).
    let Some((mut app, bytes)) = app_with_setup(b"") else {
        return;
    };
    wheel_up(&mut app);
    assert!(
        recorded(&bytes).is_empty(),
        "primary-screen wheel must not emit cursor keys"
    );
}

#[test]
fn alt_screen_with_mouse_reporting_suppresses_alt_scroll_arrows() {
    // With the app tracking the mouse (1000h), the report gate wins over
    // alt-scroll: the wheel must NOT be translated into cursor keys. (The exact
    // SGR/legacy wheel report depends on a pointer cell position, which a
    // headless app has no GPU metrics to resolve; that encoding is covered by
    // the wheel-zoom report tests. Here we pin only that alt-scroll is off.)
    let Some((mut app, bytes)) = app_with_setup(b"\x1b[?1049h\x1b[?1000h") else {
        return;
    };
    wheel_up(&mut app);
    let out = recorded(&bytes);
    assert_ne!(
        out,
        b"\x1b[A".repeat(3),
        "reporting must suppress alt-scroll"
    );
    assert!(
        !out.starts_with(b"\x1bO") && !out.starts_with(b"\x1b[A"),
        "no cursor keys while the app tracks the mouse, got {out:?}"
    );
}

#[test]
fn alt_screen_with_1007_disabled_sends_nothing() {
    // Alternate scroll explicitly disabled (1007l): the wheel falls through to
    // scrollback movement, which is a no-op on the (scrollback-less) alt screen,
    // so nothing reaches the PTY.
    let Some((mut app, bytes)) = app_with_setup(b"\x1b[?1049h\x1b[?1007l") else {
        return;
    };
    wheel_up(&mut app);
    assert!(
        recorded(&bytes).is_empty(),
        "1007-disabled wheel must not emit cursor keys"
    );
}

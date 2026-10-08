// SPDX-License-Identifier: GPL-3.0-only
//! Pointer input that the PTY refuses is reported like typed input, through
//! the real mouse path: a failed write or a failed flush of a mouse report
//! raises the input-not-delivered notice, and a delivered report raises none.

use std::io::{self, Write};

use super::*;

/// The notice typed keys raise when the PTY refuses input.
const INPUT_NOT_DELIVERED_NOTICE: &str = "Input not delivered: the session is not accepting input";

/// Fails its write, its flush, or neither.
struct ScriptedWriter {
    fail_write: bool,
    fail_flush: bool,
}

impl Write for ScriptedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            return Err(io::Error::other("pipe closed"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            return Err(io::Error::other("pipe closed"));
        }
        Ok(())
    }
}

fn press_in_reporting_app(fail_write: bool, fail_flush: bool) -> App {
    let writer: PtyWriter = Arc::new(Mutex::new(Box::new(ScriptedWriter {
        fail_write,
        fail_flush,
    })));
    let (mut app, terminal) = headless_app_with_writer(
        NativeOptions::default(),
        Dimensions::new(40, 12),
        Settings::default(),
        writer,
    );
    terminal
        .lock()
        .expect("terminal")
        .advance(b"\x1b[?1000h\x1b[?1006h");
    app.set_test_cell_for_test(CellSize {
        width: 8,
        height: 16,
        baseline: 12,
    });
    app.set_test_surface_for_test(320, 192, crate::native::WindowPadding::ZERO);
    app.pointer_move_for_test(20.0, 20.0);
    app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    app
}

#[test]
fn a_mouse_report_whose_write_fails_raises_the_notice() {
    let app = press_in_reporting_app(true, false);
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some(INPUT_NOT_DELIVERED_NOTICE)
    );
}

#[test]
fn a_mouse_report_whose_flush_fails_raises_the_notice() {
    let app = press_in_reporting_app(false, true);
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some(INPUT_NOT_DELIVERED_NOTICE)
    );
}

#[test]
fn a_delivered_mouse_report_raises_no_notice() {
    let app = press_in_reporting_app(false, false);
    assert_eq!(app.open_notice_message_for_test(), None);
}

// SPDX-License-Identifier: GPL-3.0-only
//! Pointer input that the PTY refuses is reported like typed input, through
//! the real mouse path: a failed write or a failed flush of a mouse report
//! raises the input-not-delivered notice, and a delivered report raises none.

use std::io::{self, Write};

use super::*;
use crate::native::app::open_notice::INPUT_NOT_DELIVERED_NOTICE;

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

/// Every refused report during one notice window shares that window: the
/// notice is neither re-raised nor given a later expiry by each event, so a
/// drag over a dead pipe does not hold the banner up or redraw per event.
#[test]
fn repeated_refused_reports_keep_one_notice_window() {
    let mut app = press_in_reporting_app(true, false);
    let raised = app
        .open_notice_raised_at_for_test()
        .expect("the first refusal raises the notice");
    std::thread::sleep(std::time::Duration::from_millis(15));
    for _ in 0..3 {
        app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
        app.dispatch_mouse_button_for_test(true, WinitMouseButton::Left);
    }
    assert_eq!(
        app.open_notice_raised_at_for_test(),
        Some(raised),
        "later refusals do not extend the window"
    );
    // Once the window has passed, the next refusal raises the notice afresh.
    app.update_open_notice_for_test(raised + crate::native::app::open_notice::NOTICE_DURATION);
    assert_eq!(app.open_notice_message_for_test(), None);
    app.dispatch_mouse_button_for_test(false, WinitMouseButton::Left);
    assert_eq!(
        app.open_notice_message_for_test().as_deref(),
        Some(INPUT_NOT_DELIVERED_NOTICE)
    );
}

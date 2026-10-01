// SPDX-License-Identifier: GPL-3.0-only
//! A lost GPU device keeps rendering paused. Once the device-lost state is
//! entered, a later keyboard, PTY, or resize redraw must not re-enter the
//! renderer, the render-only timers must leave the wake set, and the freeze
//! watchdog must not read the paused window as owing a frame.

use std::time::{Duration, Instant};

use super::*;

/// Mutation: make device loss process-global, clear it during presentation
/// release, or disable every sibling's timers when one App loses its device.
#[test]
fn phase9_surface_release_keeps_loss_sticky_and_sibling_timers_independent() {
    let mut failed = idle_app();
    let mut sibling = idle_app();
    sibling.arm_active_cursor_anim_for_test(Instant::now());
    let sibling_deadline = sibling.next_wake_deadline_for_window(true);
    assert!(sibling_deadline.is_some());
    failed.enter_gpu_device_lost();
    failed.release_surface();
    failed.release_surface();
    for bytes in [b"first output".as_slice(), b"\r\nsecond output".as_slice()] {
        failed.terminal.lock().expect("terminal").advance(bytes);
        failed.needs_rebuild = true;
        assert!(!failed.on_redraw_requested());
        assert!(failed.gpu_device_lost);
        assert!(!failed.watchdog_state().render_owed);
        assert!(!sibling.gpu_device_lost);
        assert_eq!(
            sibling.next_wake_deadline_for_window(true),
            sibling_deadline
        );
    }
    sibling.release_surface();
    assert!(
        !sibling.gpu_device_lost,
        "normal release is not device loss"
    );
}

fn idle_app() -> App {
    let (app, _terminal) = crate::native::test_support::headless_app_with(
        NativeOptions::default(),
        Dimensions::new(24, 80),
        Settings::default(),
    );
    app
}

#[test]
fn every_redraw_after_device_loss_skips_rendering() {
    let mut app = idle_app();
    app.enter_gpu_device_lost();
    for _ in 0..2 {
        app.needs_rebuild = true;
        assert!(
            !app.on_redraw_requested(),
            "a paused window returns before the GPU and still reaches the pending-exit check"
        );
        assert!(
            app.needs_rebuild,
            "no geometry is rebuilt while the device is lost"
        );
    }
    assert!(app.gpu_device_lost, "the pause is sticky across redraws");
}

#[test]
fn device_loss_removes_render_timers_from_the_wake_set() {
    let mut app = idle_app();
    let now = Instant::now();
    app.arm_active_cursor_anim_for_test(now);
    app.arm_active_cursor_blink_for_test(now);
    app.skipped_frame_retry_deadline = Some(now + Duration::from_millis(25));
    let presented = app.next_wake_deadline_for_window(true);
    assert!(
        presented.is_some_and(|deadline| deadline <= now + Duration::from_millis(200)),
        "a presented window wakes for its render timers"
    );

    app.enter_gpu_device_lost();
    assert_eq!(app.skipped_frame_retry_deadline, None);
    assert_eq!(
        app.next_wake_deadline_for_window(true),
        app.next_wake_deadline_for_surface(false),
        "a lost device keeps only the wakes of a window with no redraw consumer"
    );
}

#[test]
fn a_paused_window_owes_the_watchdog_no_frame() {
    let mut app = idle_app();
    app.needs_rebuild = true;
    assert!(app.watchdog_state().render_owed);
    app.enter_gpu_device_lost();
    app.needs_rebuild = true;
    assert!(
        !app.watchdog_state().render_owed,
        "latched work on a lost device is paused, not a stall"
    );
}

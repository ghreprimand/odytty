// SPDX-License-Identifier: GPL-3.0-only
//! Render-loop wiring for Kitty graphics animation (`a=f` / `a=a`).
//!
//! These tests pin the event-loop contract rather than the pixels (frame
//! semantics live in `core::kitty_animation_tests`):
//!
//! 1. An idle terminal, and a terminal showing a still image, arm no animation
//!    wake - the loop stays event-driven with zero self-wakes.
//! 2. A running animation on a visible placement arms exactly one bounded wake,
//!    never a past instant (which would busy-spin `WaitUntil`).
//! 3. Advancing across a frame boundary requests a repaint; advancing before it
//!    does not.
//! 4. Deleting the frames returns the loop to the no-wake state.
//! 5. Every visible split pane contributes deadlines and advances, including
//!    an unfocused pane.
//!
//! Platform-neutral: the wiring is clock arithmetic over core state, identical
//! on Linux, macOS, and Windows.

use std::time::{Duration, Instant};

use super::*;

/// Base64 for the small opaque payloads below, matching the encoder the core
/// tests use so the fixtures stay comparable.
fn b64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() >= 2 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() == 3 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn transmit_and_display(color: u8) -> Vec<u8> {
    let payload = b64(&[color, color, color, 255].repeat(4));
    format!("\x1b_Ga=T,f=32,t=d,s=2,v=2,i=1,c=2,r=2;{payload}\x1b\\").into_bytes()
}

fn add_frame(color: u8) -> Vec<u8> {
    let payload = b64(&[color, color, color, 255].repeat(4));
    format!("\x1b_Ga=f,i=1,f=32,s=2,v=2,z=50;{payload}\x1b\\").into_bytes()
}

fn app_with_terminal() -> (App, std::sync::Arc<std::sync::Mutex<Terminal>>) {
    headless_app_with(
        NativeOptions::default(),
        Dimensions::new(40, 8),
        Settings::default(),
    )
}

fn feed(terminal: &std::sync::Arc<std::sync::Mutex<Terminal>>, bytes: &[u8]) {
    let mut guard = crate::native::lock_recover(terminal);
    guard.advance(bytes);
    let _ = guard.take_host_output();
}

#[test]
fn no_animation_arms_no_wake() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = app_with_terminal();

    app.advance_graphics_animations(Instant::now());
    assert_eq!(
        app.animated_graphics_deadline_for_test(),
        None,
        "an idle terminal arms no animation wake"
    );

    // A still image is not an animation.
    feed(&terminal, &transmit_and_display(10));
    app.advance_graphics_animations(Instant::now());
    assert_eq!(
        app.animated_graphics_deadline_for_test(),
        None,
        "a still image arms no animation wake"
    );
}

#[test]
fn a_running_visible_animation_arms_one_bounded_future_wake() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = app_with_terminal();
    feed(&terminal, &transmit_and_display(10));
    feed(&terminal, &add_frame(40));
    feed(&terminal, b"\x1b_Ga=a,i=1,s=3,r=1,z=30\x1b\\");

    let start = Instant::now();
    app.advance_graphics_animations(start);
    let deadline = app
        .animated_graphics_deadline_for_test()
        .expect("a running animation arms a wake");

    assert!(
        deadline >= start,
        "the wake must never be in the past - a past WaitUntil busy-spins"
    );
    assert!(
        deadline <= start + Duration::from_millis(200),
        "and it must be bounded, not open-ended"
    );
}

#[test]
fn crossing_a_frame_boundary_requests_a_repaint_and_earlier_ticks_do_not() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = app_with_terminal();
    feed(&terminal, &transmit_and_display(10));
    feed(&terminal, &add_frame(40));
    feed(&terminal, b"\x1b_Ga=a,i=1,s=3,r=1,z=30\x1b\\");

    let start = Instant::now();
    app.advance_graphics_animations(start);
    app.clear_needs_rebuild_for_test();

    // Well before the first gap elapses: no frame flip, so no repaint request.
    app.advance_graphics_animations(start + Duration::from_millis(5));
    assert!(
        !app.needs_rebuild_for_test(),
        "a tick inside the current frame's gap must not request a frame"
    );

    // Past the gap: the frame flips, which must request a repaint.
    app.advance_graphics_animations(start + Duration::from_millis(60));
    assert!(
        app.needs_rebuild_for_test(),
        "a frame flip requests a repaint so the new pixels reach the screen"
    );
}

#[test]
fn deleting_frames_returns_the_loop_to_the_no_wake_state() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = app_with_terminal();
    feed(&terminal, &transmit_and_display(10));
    feed(&terminal, &add_frame(40));
    feed(&terminal, b"\x1b_Ga=a,i=1,s=3,r=1,z=30\x1b\\");
    app.advance_graphics_animations(Instant::now());
    assert!(app.animated_graphics_deadline_for_test().is_some());

    feed(&terminal, b"\x1b_Ga=d,d=f,i=1\x1b\\");
    app.advance_graphics_animations(Instant::now());

    assert_eq!(
        app.animated_graphics_deadline_for_test(),
        None,
        "with the frames gone the loop is back to zero animation wakes"
    );
}

#[test]
fn a_stopped_animation_arms_no_wake() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, terminal) = app_with_terminal();
    feed(&terminal, &transmit_and_display(10));
    feed(&terminal, &add_frame(40));
    feed(&terminal, b"\x1b_Ga=a,i=1,s=3,r=1,z=30\x1b\\");
    app.advance_graphics_animations(Instant::now());
    assert!(app.animated_graphics_deadline_for_test().is_some());

    feed(&terminal, b"\x1b_Ga=a,i=1,s=1\x1b\\");
    app.advance_graphics_animations(Instant::now());

    assert_eq!(
        app.animated_graphics_deadline_for_test(),
        None,
        "stopping the animation retires its wake"
    );
}

#[test]
fn an_unfocused_visible_split_pane_animates_and_contributes_the_wake() {
    let _guard = crate::test_lock::render_globals_lock();
    let (mut app, background) = app_with_terminal();
    let dimensions = Dimensions::new(40, 8);
    let focused = std::sync::Arc::new(std::sync::Mutex::new(Terminal::new(
        dimensions.columns,
        dimensions.rows,
    )));
    app.seed_headless_split_pane_for_test(
        true,
        focused,
        crate::native::test_support::headless_writer(),
        dimensions,
    );

    feed(&background, &transmit_and_display(10));
    feed(&background, &add_frame(40));
    feed(&background, b"\x1b_Ga=a,i=1,s=3,r=1,z=30\x1b\\");

    let start = Instant::now();
    app.advance_graphics_animations(start);
    assert!(
        app.animated_graphics_deadline_for_test().is_some(),
        "the unfocused visible pane contributes its animation deadline"
    );
    app.advance_graphics_animations(start + Duration::from_millis(60));

    let terminal = crate::native::lock_recover(&background);
    let visible = terminal.visible_graphics(0);
    let image = terminal
        .graphics()
        .store()
        .get(visible[0].image_id)
        .expect("background image");
    assert_eq!(image.rgba, [40, 40, 40, 255].repeat(4));
}

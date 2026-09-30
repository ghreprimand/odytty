// SPDX-License-Identifier: GPL-3.0-only
//! App-level coverage for the idle retry of a failed backend resize: the
//! event loop's wake set carries the retry, and the real maintenance pass
//! resends the size with no geometry event.

use crate::core::Dimensions;
use crate::native::layout::PaneRect;
use crate::native::test_support::headless_app_for_test;

#[test]
fn idle_maintenance_resends_a_failed_backend_resize_at_its_wake() {
    let (mut app, _terminal) = headless_app_for_test();
    let backend = app
        .sessions_mut_for_test()
        .active()
        .headless_session()
        .expect("headless backend")
        .clone();
    backend.fail_next_resizes(1);
    app.sessions_mut_for_test().resize_all_panes(
        PaneRect::new(0.0, 0.0, 800.0, 400.0),
        10,
        20,
        1.0,
        0.0,
    );
    assert_ne!(backend.dimensions(), Dimensions::new(80, 20), "refused");

    let retry = app
        .sessions_mut_for_test()
        .next_backend_resize_retry()
        .expect("the failed resize is scheduled");
    let wake = app
        .next_wake_deadline_for_surface_for_test(false)
        .expect("the loop wakes for the retry even with no surface");
    assert!(wake <= retry, "the retry is part of the wake set");

    app.run_about_to_wait_maintenance_for_test(retry);
    assert_eq!(backend.dimensions(), Dimensions::new(80, 20));
    assert_eq!(
        app.sessions_mut_for_test().next_backend_resize_retry(),
        None
    );
}

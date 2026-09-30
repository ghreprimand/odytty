// SPDX-License-Identifier: GPL-3.0-only
//! A backend resize that failed is retried from idle maintenance with a
//! bounded cadence, not only on the next geometry event.

use super::*;
use std::time::Duration;

fn backend(set: &WorkspaceSet, token: SessionToken) -> Arc<HeadlessSession> {
    set.get(token)
        .expect("pane present")
        .headless_session()
        .expect("headless backend")
        .clone()
}

fn content() -> PaneRect {
    PaneRect::new(0.0, 0.0, 800.0, 400.0)
}

#[test]
fn a_failed_backend_resize_is_resent_once_its_retry_is_due() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let backend = backend(&set, SessionToken(0));
    backend.fail_next_resizes(1);
    set.resize_all_panes(content(), 10, 20, 1.0, 0.0);
    assert_eq!(pane_dims(&set, SessionToken(0)), (80, 20), "model resized");
    assert_eq!(
        backend.dimensions(),
        Dimensions::new(20, 8),
        "backend refused"
    );

    let due = set
        .next_backend_resize_retry()
        .expect("a failed resize schedules a retry");
    let calls = backend.resize_call_count();
    set.retry_backend_resizes(due - Duration::from_millis(1));
    assert_eq!(backend.resize_call_count(), calls, "not before it is due");

    set.retry_backend_resizes(due);
    assert_eq!(backend.dimensions(), Dimensions::new(80, 20));
    assert_eq!(
        set.next_backend_resize_retry(),
        None,
        "no wake once accepted"
    );
}

#[test]
fn a_backend_that_stays_stalled_is_retried_with_a_capped_backoff() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let backend = backend(&set, SessionToken(0));
    backend.fail_next_resizes(10);
    set.resize_all_panes(content(), 10, 20, 1.0, 0.0);

    let mut gaps = Vec::new();
    let mut previous = set.next_backend_resize_retry().expect("scheduled");
    while backend.dimensions() != Dimensions::new(80, 20) {
        assert!(gaps.len() < 16, "the retry eventually lands");
        set.retry_backend_resizes(previous);
        match set.next_backend_resize_retry() {
            Some(next) => {
                gaps.push(next - previous);
                previous = next;
            }
            None => break,
        }
    }
    assert_eq!(backend.dimensions(), Dimensions::new(80, 20));
    assert!(gaps.windows(2).all(|pair| pair[1] >= pair[0]), "{gaps:?}");
    assert!(
        gaps.iter().all(|gap| *gap <= Duration::from_secs(5)),
        "{gaps:?}"
    );
}

#[test]
fn a_live_drag_cancels_the_retry_and_the_release_flushes() {
    let mut set = WorkspaceSet::new(build_session(), None);
    let backend = backend(&set, SessionToken(0));
    backend.fail_next_resizes(1);
    set.resize_all_panes(content(), 10, 20, 1.0, 0.0);
    assert!(set.next_backend_resize_retry().is_some());

    let dragged = PaneRect::new(0.0, 0.0, 600.0, 400.0);
    set.reflow_all_panes_for_drag(dragged, 10, 20, 1.0, 0.0);
    assert_eq!(
        set.next_backend_resize_retry(),
        None,
        "no intermediate size is pushed mid-drag"
    );
    set.resize_all_panes(dragged, 10, 20, 1.0, 0.0);
    assert_eq!(backend.dimensions(), Dimensions::new(60, 20));
}

// SPDX-License-Identifier: GPL-3.0-only
use super::*;

#[test]
fn synchronized_output_hold_releases_on_decrst() {
    let mut hold = SynchronizedOutputHold::default();
    let t0 = Instant::now();

    assert!(hold.should_hold(true, t0));
    assert_eq!(hold.deadline(), Some(t0 + SYNCHRONIZED_OUTPUT_TIMEOUT));
    assert!(!hold.should_hold(false, t0 + Duration::from_millis(20)));
    assert_eq!(hold.deadline(), None);

    assert!(hold.should_hold(true, t0 + Duration::from_millis(40)));
}

#[test]
fn synchronized_output_hold_times_out_without_sleeping() {
    let mut hold = SynchronizedOutputHold::default();
    let t0 = Instant::now();

    assert!(hold.should_hold(true, t0));
    assert!(!hold.is_due(t0 + SYNCHRONIZED_OUTPUT_TIMEOUT - Duration::from_millis(1)));
    assert!(hold.is_due(t0 + SYNCHRONIZED_OUTPUT_TIMEOUT));
    assert!(!hold.should_hold(true, t0 + SYNCHRONIZED_OUTPUT_TIMEOUT));

    assert!(
        !hold.should_hold(
            true,
            t0 + SYNCHRONIZED_OUTPUT_TIMEOUT + Duration::from_secs(1)
        ),
        "timed-out holds stay released until the app sends DECRST 2026"
    );
    assert!(!hold.should_hold(
        false,
        t0 + SYNCHRONIZED_OUTPUT_TIMEOUT + Duration::from_secs(2)
    ));
    assert!(hold.should_hold(
        true,
        t0 + SYNCHRONIZED_OUTPUT_TIMEOUT + Duration::from_secs(3)
    ));
}

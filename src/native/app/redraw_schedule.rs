// SPDX-License-Identifier: GPL-3.0-only
//! Timer-to-redraw scheduling helpers.

use std::time::Instant;

/// Return whether a timed animation has reached its scheduled frame boundary.
///
/// Future deadlines remain wake sources without requesting an immediate redraw,
/// allowing the event loop to sleep until `WaitUntil` reaches the boundary.
pub(in crate::native) fn timed_animation_redraw_due(
    now: Instant,
    deadline: Option<Instant>,
) -> bool {
    deadline.is_some_and(|deadline| deadline <= now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn absent_and_future_deadlines_are_not_due() {
        let now = Instant::now();
        assert!(!timed_animation_redraw_due(now, None));
        assert!(!timed_animation_redraw_due(
            now,
            Some(now + Duration::from_millis(16))
        ));
    }

    #[test]
    fn exact_and_past_deadlines_are_due() {
        let now = Instant::now();
        assert!(timed_animation_redraw_due(now, Some(now)));
        assert!(timed_animation_redraw_due(
            now,
            Some(now - Duration::from_millis(1))
        ));
    }
}

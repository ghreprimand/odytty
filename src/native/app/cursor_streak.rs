// SPDX-License-Identifier: GPL-3.0-only
//! Presentation-only large-jump cursor streak state.

use super::cursor_trail::{CursorTrailProfile, cursor_trail_profile};
use super::*;
use crate::core::CursorStyle;
use crate::native::gpu::CursorStreakRequest;

const CURSOR_STREAK_DWELL: Duration = Duration::from_millis(40);
const CURSOR_STREAK_FRAME: Duration = Duration::from_millis(16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorStreakAnchor {
    source: Position,
    destination: Position,
    style: CursorStyle,
    dimensions: Dimensions,
    cell: CellSize,
    strength: crate::settings::CursorTrailStrength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PresentedCursor {
    cursor: Position,
    style: CursorStyle,
    dimensions: Dimensions,
    cell: CellSize,
}

#[derive(Debug, Clone, Copy)]
enum CursorStreakPhase {
    Idle,
    Pending {
        anchor: CursorStreakAnchor,
        destination_since: Instant,
    },
    Active {
        anchor: CursorStreakAnchor,
        started_at: Instant,
        duration: Duration,
    },
}

/// Per-session follower state. The logical and rendered cursor never reads this
/// state; it only controls a below-glyph presentation request and bounded wake.
#[derive(Debug, Clone, Copy)]
pub(in crate::native) struct CursorStreakState {
    phase: CursorStreakPhase,
    serial: u64,
    epoch: u64,
    next_deadline: Option<Instant>,
    presented: Option<PresentedCursor>,
}

impl Default for CursorStreakState {
    fn default() -> Self {
        Self {
            phase: CursorStreakPhase::Idle,
            serial: 0,
            epoch: 0,
            next_deadline: None,
            presented: None,
        }
    }
}

impl CursorStreakState {
    pub(in crate::native) fn park(&mut self) {
        self.phase = CursorStreakPhase::Idle;
        self.epoch = 0;
        self.next_deadline = None;
        self.presented = None;
    }

    fn advance_epoch(&mut self) {
        self.serial = self.serial.wrapping_add(1).max(1);
        self.epoch = self.serial;
    }

    fn clear(&mut self) {
        if !matches!(self.phase, CursorStreakPhase::Idle) {
            self.advance_epoch();
        }
        self.phase = CursorStreakPhase::Idle;
        self.epoch = 0;
        self.next_deadline = None;
    }

    pub(in crate::native) fn discard_animation(&mut self) {
        self.clear();
    }

    pub(in crate::native) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(in crate::native) fn deadline(&self) -> Option<Instant> {
        self.next_deadline
    }

    fn manhattan(from: Position, to: Position) -> usize {
        from.column.abs_diff(to.column) + from.row.abs_diff(to.row)
    }

    fn duration(profile: CursorTrailProfile, distance: usize) -> Duration {
        let u = ((distance as f32 - 7.0) / 33.0).clamp(0.0, 1.0);
        if u <= 0.0 {
            return profile.duration_min;
        }
        if u >= 1.0 {
            return profile.duration_max;
        }
        let smooth = u * u * (3.0 - 2.0 * u);
        profile.duration_min
            + profile
                .duration_max
                .saturating_sub(profile.duration_min)
                .mul_f32(smooth)
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn update(
        &mut self,
        now: Instant,
        seed_prior: Option<(Position, Dimensions, CursorStyle)>,
        snapshot: &Snapshot,
        style: CursorStyle,
        cell: CellSize,
        enabled: bool,
        strength: crate::settings::CursorTrailStrength,
    ) {
        let prior = self.presented.or_else(|| {
            seed_prior.map(|(cursor, dimensions, style)| PresentedCursor {
                cursor,
                style,
                dimensions,
                cell,
            })
        });
        self.presented = Some(PresentedCursor {
            cursor: snapshot.cursor,
            style,
            dimensions: snapshot.dimensions,
            cell,
        });
        let eligible = enabled
            && snapshot.cursor_visible
            && snapshot.dimensions.columns > 0
            && snapshot.dimensions.rows > 0
            && cell.width > 0
            && cell.height > 0;
        let Some(prior) = prior.filter(|_| eligible) else {
            self.clear();
            return;
        };
        if prior.dimensions != snapshot.dimensions || prior.cell != cell {
            self.clear();
            return;
        }

        let current = snapshot.cursor;
        let moved = current != prior.cursor;
        let phase = self.phase;
        match phase {
            CursorStreakPhase::Idle => {
                if moved
                    && prior.style == style
                    && Self::manhattan(prior.cursor, current)
                        > super::cursor_frame::MAX_SLIDE_CELLS as usize
                {
                    self.phase = CursorStreakPhase::Pending {
                        anchor: CursorStreakAnchor {
                            source: prior.cursor,
                            destination: current,
                            style,
                            dimensions: snapshot.dimensions,
                            cell,
                            strength,
                        },
                        destination_since: now,
                    };
                    self.advance_epoch();
                    self.next_deadline = Some(now + CURSOR_STREAK_DWELL);
                } else {
                    self.next_deadline = None;
                }
            }
            CursorStreakPhase::Pending {
                mut anchor,
                mut destination_since,
            } => {
                if anchor.style != style
                    || anchor.dimensions != snapshot.dimensions
                    || anchor.cell != cell
                    || anchor.strength != strength
                {
                    self.clear();
                    return;
                }
                if moved {
                    if Self::manhattan(anchor.source, current)
                        <= super::cursor_frame::MAX_SLIDE_CELLS as usize
                    {
                        self.clear();
                        return;
                    }
                    anchor.destination = current;
                    destination_since = now;
                    self.phase = CursorStreakPhase::Pending {
                        anchor,
                        destination_since,
                    };
                    self.advance_epoch();
                    self.next_deadline = Some(now + CURSOR_STREAK_DWELL);
                } else if current != anchor.destination {
                    self.clear();
                } else if now >= destination_since + CURSOR_STREAK_DWELL {
                    let profile = cursor_trail_profile(anchor.strength);
                    let duration = Self::duration(profile, Self::manhattan(anchor.source, current));
                    self.phase = CursorStreakPhase::Active {
                        anchor,
                        started_at: now,
                        duration,
                    };
                    self.advance_epoch();
                    self.next_deadline = Some(now + CURSOR_STREAK_FRAME);
                } else {
                    self.next_deadline = Some(destination_since + CURSOR_STREAK_DWELL);
                }
            }
            CursorStreakPhase::Active {
                anchor,
                started_at,
                duration,
            } => {
                if moved
                    || current != anchor.destination
                    || anchor.style != style
                    || anchor.dimensions != snapshot.dimensions
                    || anchor.cell != cell
                    || anchor.strength != strength
                {
                    self.clear();
                    if moved
                        && prior.style == style
                        && Self::manhattan(prior.cursor, current)
                            > super::cursor_frame::MAX_SLIDE_CELLS as usize
                    {
                        self.phase = CursorStreakPhase::Pending {
                            anchor: CursorStreakAnchor {
                                source: prior.cursor,
                                destination: current,
                                style,
                                dimensions: snapshot.dimensions,
                                cell,
                                strength,
                            },
                            destination_since: now,
                        };
                        self.advance_epoch();
                        self.next_deadline = Some(now + CURSOR_STREAK_DWELL);
                    }
                    return;
                }
                if now.saturating_duration_since(started_at) >= duration {
                    self.clear();
                } else {
                    self.advance_epoch();
                    self.next_deadline = Some(now + CURSOR_STREAK_FRAME);
                }
            }
        }
    }

    pub(in crate::native) fn request(
        &self,
        now: Instant,
        clip_rect: [f32; 4],
    ) -> Option<CursorStreakRequest> {
        let CursorStreakPhase::Active {
            anchor,
            started_at,
            duration,
        } = self.phase
        else {
            return None;
        };
        let progress = (now.saturating_duration_since(started_at).as_secs_f32()
            / duration.as_secs_f32())
        .clamp(0.0, 1.0);
        (progress < 1.0).then_some(CursorStreakRequest {
            source: anchor.source,
            destination: anchor.destination,
            progress,
            strength: anchor.strength,
            clip_rect,
        })
    }
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::native) fn update_cursor_streak(
        &mut self,
        now: Instant,
        snapshot: &Snapshot,
        style: CursorStyle,
        cell: CellSize,
    ) {
        let enabled = self.settings.cursor_motion
            && self.settings.cursor_trail
            && !self.settings.reduced_motion
            && self.focused
            && self.viewport.offset() == 0;
        let strength = self.settings.cursor_trail_strength;
        self.cursor_streak
            .update(now, None, snapshot, style, cell, enabled, strength);
    }

    pub(super) fn cursor_streak_deadline(&self) -> Option<Instant> {
        self.cursor_streak.deadline()
    }

    pub(super) fn cursor_streak_epoch(&self) -> u64 {
        self.cursor_streak.epoch()
    }

    pub(super) fn cursor_streak_request(
        &self,
        now: Instant,
        clip_rect: [f32; 4],
    ) -> Option<CursorStreakRequest> {
        self.cursor_streak.request(now, clip_rect)
    }

    pub(super) fn clear_cursor_streak(&mut self) {
        self.cursor_streak.discard_animation();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dimensions() -> Dimensions {
        Dimensions::new(80, 24)
    }

    fn snapshot(cursor: Position) -> Snapshot {
        let dimensions = dimensions();
        Snapshot {
            dimensions,
            cursor,
            cursor_visible: true,
            colors: crate::core::DynamicColors::default(),
            cells: vec![crate::core::Cell::default(); dimensions.columns * dimensions.rows],
        }
    }

    fn cell() -> CellSize {
        CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        }
    }

    fn pos(column: usize) -> Position {
        Position { row: 3, column }
    }

    #[test]
    fn distances_through_six_are_idle_and_seven_is_pending() {
        let now = Instant::now();
        for distance in 0..=6 {
            let prior = snapshot(pos(2));
            let current = snapshot(pos(2 + distance));
            let mut state = CursorStreakState::default();
            state.update(
                now,
                Some((prior.cursor, prior.dimensions, CursorStyle::Block)),
                &current,
                CursorStyle::Block,
                cell(),
                true,
                crate::settings::CursorTrailStrength::Balanced,
            );
            assert!(
                state.deadline().is_none(),
                "distance {distance} stayed idle"
            );
        }
        let prior = snapshot(pos(2));
        let current = snapshot(pos(9));
        let mut state = CursorStreakState::default();
        state.update(
            now,
            Some((prior.cursor, prior.dimensions, CursorStyle::Block)),
            &current,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert_eq!(state.deadline(), Some(now + CURSOR_STREAK_DWELL));
        assert_ne!(state.epoch(), 0);
    }

    #[test]
    fn dwell_boundary_activates_and_finishes_with_bounded_wakes() {
        let now = Instant::now();
        let prior = snapshot(pos(2));
        let current = snapshot(pos(30));
        let mut state = CursorStreakState::default();
        state.update(
            now,
            Some((prior.cursor, prior.dimensions, CursorStyle::Bar)),
            &current,
            CursorStyle::Bar,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        state.update(
            now + CURSOR_STREAK_DWELL - Duration::from_nanos(1),
            Some((current.cursor, current.dimensions, CursorStyle::Bar)),
            &current,
            CursorStyle::Bar,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert!(
            state
                .request(
                    now + CURSOR_STREAK_DWELL - Duration::from_nanos(1),
                    [0.0; 4],
                )
                .is_none()
        );
        let active_at = now + CURSOR_STREAK_DWELL;
        state.update(
            active_at,
            Some((current.cursor, current.dimensions, CursorStyle::Bar)),
            &current,
            CursorStyle::Bar,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert!(
            state
                .request(active_at, [0.0, 0.0, 640.0, 384.0],)
                .is_some()
        );
        state.update(
            active_at + Duration::from_millis(220),
            Some((current.cursor, current.dimensions, CursorStyle::Bar)),
            &current,
            CursorStyle::Bar,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert!(state.deadline().is_none());
        assert_eq!(state.epoch(), 0);
    }

    #[test]
    fn destination_churn_preserves_source_and_restarts_dwell() {
        let now = Instant::now();
        let first = snapshot(pos(2));
        let second = snapshot(pos(20));
        let final_snapshot = snapshot(pos(35));
        let mut state = CursorStreakState::default();
        state.update(
            now,
            Some((first.cursor, first.dimensions, CursorStyle::Underline)),
            &second,
            CursorStyle::Underline,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        let churn = now + Duration::from_millis(20);
        state.update(
            churn,
            Some((second.cursor, second.dimensions, CursorStyle::Underline)),
            &final_snapshot,
            CursorStyle::Underline,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert_eq!(state.deadline(), Some(churn + CURSOR_STREAK_DWELL));
        state.update(
            churn + CURSOR_STREAK_DWELL,
            Some((
                final_snapshot.cursor,
                final_snapshot.dimensions,
                CursorStyle::Underline,
            )),
            &final_snapshot,
            CursorStyle::Underline,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        let request = state
            .request(churn + CURSOR_STREAK_DWELL, [0.0, 0.0, 640.0, 384.0])
            .expect("stable final destination activates");
        assert_eq!(request.source, first.cursor);
        assert_eq!(request.destination, final_snapshot.cursor);
    }

    #[test]
    fn synchronized_output_release_classifies_the_old_to_final_jump() {
        let now = Instant::now();
        let source = snapshot(pos(2));
        let final_snapshot = snapshot(pos(35));
        let mut state = CursorStreakState::default();
        state.update(
            now,
            None,
            &source,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );

        // A synchronized-output hold does not feed intermediate destinations
        // into the follower. Releasing it discards any frozen ribbon while
        // preserving the last presented raw cursor as the next source.
        state.discard_animation();
        let released_at = now + Duration::from_millis(80);
        state.update(
            released_at,
            None,
            &final_snapshot,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert_eq!(state.deadline(), Some(released_at + CURSOR_STREAK_DWELL));
        let active_at = released_at + CURSOR_STREAK_DWELL;
        state.update(
            active_at,
            None,
            &final_snapshot,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        let request = state
            .request(active_at, [0.0, 0.0, 640.0, 384.0])
            .expect("the coalesced final destination activates after dwell");
        assert_eq!(request.source, source.cursor);
        assert_eq!(request.destination, final_snapshot.cursor);
    }

    #[test]
    fn profile_duration_is_monotonic_and_clamped() {
        for strength in [
            crate::settings::CursorTrailStrength::Subtle,
            crate::settings::CursorTrailStrength::Balanced,
            crate::settings::CursorTrailStrength::Expressive,
        ] {
            let profile = cursor_trail_profile(strength);
            let d7 = CursorStreakState::duration(profile, 7);
            let d20 = CursorStreakState::duration(profile, 20);
            let d40 = CursorStreakState::duration(profile, 40);
            let d80 = CursorStreakState::duration(profile, 80);
            assert_eq!(d7, profile.duration_min);
            assert!(d7 <= d20 && d20 <= d40);
            assert_eq!(d40, profile.duration_max);
            assert_eq!(d80, profile.duration_max);
        }
    }

    #[test]
    fn discontinuities_clear_pending_without_a_request_or_wake() {
        let now = Instant::now();
        let prior = snapshot(pos(2));
        let current = snapshot(pos(20));
        for gate in ["off", "hidden", "style", "resize", "scale"] {
            let mut state = CursorStreakState::default();
            state.update(
                now,
                Some((prior.cursor, prior.dimensions, CursorStyle::Block)),
                &current,
                CursorStyle::Block,
                cell(),
                true,
                crate::settings::CursorTrailStrength::Balanced,
            );
            let mut changed = current.clone();
            let mut changed_cell = cell();
            let mut changed_style = CursorStyle::Block;
            let mut enabled = true;
            match gate {
                "off" => enabled = false,
                "hidden" => changed.cursor_visible = false,
                "style" => changed_style = CursorStyle::Bar,
                "resize" => changed.dimensions = Dimensions::new(79, 24),
                "scale" => changed_cell.width += 1,
                _ => unreachable!(),
            }
            state.update(
                now + Duration::from_millis(10),
                Some((current.cursor, current.dimensions, CursorStyle::Block)),
                &changed,
                changed_style,
                changed_cell,
                enabled,
                crate::settings::CursorTrailStrength::Balanced,
            );
            assert!(state.deadline().is_none(), "{gate} clears the deadline");
            assert!(
                state
                    .request(now + Duration::from_millis(10), [0.0; 4])
                    .is_none(),
                "{gate} emits no request"
            );
        }
    }

    #[test]
    fn maximum_streak_uses_at_most_fourteen_frame_wakes() {
        let now = Instant::now();
        let prior = snapshot(pos(0));
        let current = snapshot(pos(60));
        let mut state = CursorStreakState::default();
        state.update(
            now,
            Some((prior.cursor, prior.dimensions, CursorStyle::Block)),
            &current,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Expressive,
        );
        let activated = now + CURSOR_STREAK_DWELL;
        state.update(
            activated,
            Some((current.cursor, current.dimensions, CursorStyle::Block)),
            &current,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Expressive,
        );
        let mut frame_wakes = 0;
        while let Some(deadline) = state.deadline() {
            frame_wakes += 1;
            state.update(
                deadline,
                Some((current.cursor, current.dimensions, CursorStyle::Block)),
                &current,
                CursorStyle::Block,
                cell(),
                true,
                crate::settings::CursorTrailStrength::Expressive,
            );
            assert!(frame_wakes <= 14, "streak wake schedule stayed bounded");
        }
        assert_eq!(frame_wakes, 14);
    }

    #[test]
    fn churn_returning_near_source_and_active_movement_cancel_stale_paths() {
        let now = Instant::now();
        let source = snapshot(pos(2));
        let far = snapshot(pos(20));
        let near = snapshot(pos(5));
        let mut state = CursorStreakState::default();
        state.update(
            now,
            Some((source.cursor, source.dimensions, CursorStyle::Block)),
            &far,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        state.update(
            now + Duration::from_millis(10),
            Some((far.cursor, far.dimensions, CursorStyle::Block)),
            &near,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert!(state.deadline().is_none());

        state.update(
            now + Duration::from_millis(20),
            Some((near.cursor, near.dimensions, CursorStyle::Block)),
            &far,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        let active_at = now + Duration::from_millis(60);
        state.update(
            active_at,
            Some((far.cursor, far.dimensions, CursorStyle::Block)),
            &far,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert!(state.request(active_at, [0.0, 0.0, 640.0, 384.0]).is_some());

        let moved = snapshot(pos(40));
        state.update(
            active_at + Duration::from_millis(5),
            Some((far.cursor, far.dimensions, CursorStyle::Block)),
            &moved,
            CursorStyle::Block,
            cell(),
            true,
            crate::settings::CursorTrailStrength::Balanced,
        );
        assert!(
            state
                .request(
                    active_at + Duration::from_millis(5),
                    [0.0, 0.0, 640.0, 384.0]
                )
                .is_none(),
            "the old active ribbon is canceled before the new dwell"
        );
        assert_eq!(
            state.deadline(),
            Some(active_at + Duration::from_millis(45))
        );
    }
}

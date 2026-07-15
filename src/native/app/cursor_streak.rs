// SPDX-License-Identifier: GPL-3.0-only
//! Presentation-only large-jump cursor follower state.

use super::cursor_trail::cursor_trail_profile;
use super::*;
use crate::core::CursorStyle;
use crate::native::gpu::CursorStreakRequest;

const CURSOR_STREAK_FRAME: Duration = Duration::from_millis(16);
const CURSOR_STREAK_MAX_DT: Duration = Duration::from_millis(32);
const CURSOR_STREAK_SETTLE_EPSILON: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq)]
struct CursorFollower {
    destination: Position,
    presented_rect: [f32; 4],
    target_rect: [f32; 4],
    style: CursorStyle,
    dimensions: Dimensions,
    cell: CellSize,
    strength: crate::settings::CursorTrailStrength,
    last_frame_at: Instant,
    retargeted_at: Instant,
}

#[cfg(test)]
mod follower_tests {
    use super::*;
    use crate::settings::CursorTrailStrength;

    fn dimensions() -> Dimensions {
        Dimensions::new(80, 24)
    }

    fn snapshot(row: usize, column: usize) -> Snapshot {
        let dimensions = dimensions();
        Snapshot {
            dimensions,
            cursor: Position { row, column },
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

    fn start(
        now: Instant,
        source: Position,
        destination: Position,
        style: CursorStyle,
        strength: CursorTrailStrength,
    ) -> CursorStreakState {
        let mut state = CursorStreakState::default();
        let current = snapshot(destination.row, destination.column);
        state.update(
            now,
            Some((source, current.dimensions, style)),
            &current,
            style,
            cell(),
            true,
            strength,
        );
        state
    }

    fn request(state: &CursorStreakState, now: Instant) -> CursorStreakRequest {
        state
            .request(now, [0.0, 0.0, 640.0, 384.0])
            .expect("active follower request")
    }

    #[test]
    fn nearby_moves_stay_quiet_and_seven_cells_starts_on_the_same_frame() {
        let now = Instant::now();
        for distance in 0..=6 {
            let state = start(
                now,
                Position { row: 3, column: 2 },
                Position {
                    row: 3,
                    column: 2 + distance,
                },
                CursorStyle::Block,
                CursorTrailStrength::Balanced,
            );
            assert!(state.deadline().is_none(), "distance {distance} is quiet");
            assert!(state.request(now, [0.0; 4]).is_none());
        }

        let state = start(
            now,
            Position { row: 3, column: 2 },
            Position { row: 3, column: 9 },
            CursorStyle::Block,
            CursorTrailStrength::Balanced,
        );
        let follower = request(&state, now);
        assert_eq!(state.deadline(), Some(now + CURSOR_STREAK_FRAME));
        assert!(follower.rect[0] > 16.0, "trailing edge moved immediately");
        assert!(follower.rect[2] > 24.0, "leading edge moved immediately");
        assert!(follower.rect[2] < 80.0, "presentation did not teleport");
    }

    #[test]
    fn follower_stretches_mid_flight_then_settles_without_a_wake() {
        let now = Instant::now();
        let destination = Position { row: 3, column: 30 };
        let current = snapshot(destination.row, destination.column);
        let mut state = start(
            now,
            Position { row: 3, column: 2 },
            destination,
            CursorStyle::Block,
            CursorTrailStrength::Balanced,
        );
        let first = request(&state, now).rect;
        state.update(
            now + Duration::from_millis(64),
            None,
            &current,
            CursorStyle::Block,
            cell(),
            true,
            CursorTrailStrength::Balanced,
        );
        let middle = request(&state, now + Duration::from_millis(64)).rect;
        assert!(middle[0] > first[0] && middle[2] > first[2]);
        assert!(middle[2] - middle[0] > cell().width as f32);

        state.update(
            now + Duration::from_millis(240),
            None,
            &current,
            CursorStyle::Block,
            cell(),
            true,
            CursorTrailStrength::Balanced,
        );
        assert!(
            state
                .request(now + Duration::from_millis(240), [0.0; 4])
                .is_none()
        );
        assert!(state.deadline().is_none());
        assert_eq!(state.epoch(), 0);
    }

    #[test]
    fn profiles_separate_by_response_stretch_and_settle_at_common_dpi() {
        let now = Instant::now();
        let source = Position { row: 3, column: 2 };
        let destination = Position { row: 3, column: 30 };
        let subtle = request(
            &start(
                now,
                source,
                destination,
                CursorStyle::Block,
                CursorTrailStrength::Subtle,
            ),
            now,
        );
        let expressive = request(
            &start(
                now,
                source,
                destination,
                CursorStyle::Block,
                CursorTrailStrength::Expressive,
            ),
            now,
        );
        let subtle_width = subtle.rect[2] - subtle.rect[0];
        let expressive_width = expressive.rect[2] - expressive.rect[0];
        assert!(
            subtle.rect[0] > expressive.rect[0],
            "Subtle rear edge responds faster"
        );
        assert!(
            subtle_width < expressive_width,
            "Expressive stretches farther"
        );
        assert_eq!(
            subtle.alpha, expressive.alpha,
            "profiles do not depend on alpha"
        );

        let profile_subtle = cursor_trail_profile(CursorTrailStrength::Subtle);
        let profile_expressive = cursor_trail_profile(CursorTrailStrength::Expressive);
        assert!(profile_subtle.follower_max_settle < profile_expressive.follower_max_settle);
    }

    #[test]
    fn diagonal_move_and_midflight_reversal_retarget_without_teleporting() {
        let now = Instant::now();
        let source = Position { row: 2, column: 2 };
        let destination = Position {
            row: 12,
            column: 30,
        };
        let mut state = start(
            now,
            source,
            destination,
            CursorStyle::Underline,
            CursorTrailStrength::Expressive,
        );
        let initial = request(&state, now).rect;
        assert!(initial[0] > source.column as f32 * cell().width as f32);
        assert!(initial[1] > (source.row + 1) as f32 * cell().height as f32 - 4.0);

        let reverse = Position { row: 1, column: 1 };
        let reversed_snapshot = snapshot(reverse.row, reverse.column);
        state.update(
            now + Duration::from_millis(32),
            None,
            &reversed_snapshot,
            CursorStyle::Underline,
            cell(),
            true,
            CursorTrailStrength::Expressive,
        );
        let reversed = request(&state, now + Duration::from_millis(32));
        assert_eq!(reversed.destination, reverse);
        assert!(reversed.rect[0] < initial[0]);
        assert!(reversed.rect[2] > reverse.column as f32 * cell().width as f32);
        assert_eq!(reversed.rect.len(), 4, "follower remains axis aligned");
    }

    #[test]
    fn all_cursor_shapes_keep_their_cross_axis_footprint() {
        let now = Instant::now();
        for style in [CursorStyle::Block, CursorStyle::Bar, CursorStyle::Underline] {
            let follower = request(
                &start(
                    now,
                    Position { row: 3, column: 2 },
                    Position { row: 3, column: 30 },
                    style,
                    CursorTrailStrength::Balanced,
                ),
                now,
            );
            let height = follower.rect[3] - follower.rect[1];
            match style {
                CursorStyle::Block | CursorStyle::Bar => assert!((height - 16.0).abs() < 1e-4),
                CursorStyle::Underline => assert!(height <= 3.0),
            }
        }
    }

    #[test]
    fn synchronized_output_release_coalesces_to_one_immediate_target() {
        let now = Instant::now();
        let source = snapshot(3, 2);
        let final_snapshot = snapshot(3, 35);
        let mut state = CursorStreakState::default();
        state.update(
            now,
            None,
            &source,
            CursorStyle::Block,
            cell(),
            true,
            CursorTrailStrength::Balanced,
        );
        state.discard_animation();
        let released_at = now + Duration::from_millis(80);
        state.update(
            released_at,
            None,
            &final_snapshot,
            CursorStyle::Block,
            cell(),
            true,
            CursorTrailStrength::Balanced,
        );
        let follower = request(&state, released_at);
        assert_eq!(follower.destination, final_snapshot.cursor);
        assert_eq!(state.deadline(), Some(released_at + CURSOR_STREAK_FRAME));
    }

    #[test]
    fn discontinuities_park_follower_and_expressive_wakes_stay_bounded() {
        let now = Instant::now();
        let destination = Position { row: 3, column: 40 };
        let current = snapshot(destination.row, destination.column);
        let mut state = start(
            now,
            Position { row: 3, column: 2 },
            destination,
            CursorStyle::Block,
            CursorTrailStrength::Expressive,
        );
        let mut wakes = 1;
        for frame in 1..=24 {
            let at = now + CURSOR_STREAK_FRAME * frame;
            state.update(
                at,
                None,
                &current,
                CursorStyle::Block,
                cell(),
                true,
                CursorTrailStrength::Expressive,
            );
            if state.deadline().is_some() {
                wakes += 1;
            } else {
                break;
            }
        }
        assert!(wakes <= 20, "bounded at the 300 ms profile limit");
        assert!(state.deadline().is_none());

        let mut disabled = start(
            now,
            Position { row: 3, column: 2 },
            destination,
            CursorStyle::Block,
            CursorTrailStrength::Balanced,
        );
        disabled.update(
            now + CURSOR_STREAK_FRAME,
            None,
            &current,
            CursorStyle::Block,
            cell(),
            false,
            CursorTrailStrength::Balanced,
        );
        assert!(!disabled.active());
        assert!(disabled.deadline().is_none());

        for gate in ["hidden", "style", "resize", "scale"] {
            let mut gated = start(
                now,
                Position { row: 3, column: 2 },
                destination,
                CursorStyle::Block,
                CursorTrailStrength::Balanced,
            );
            let mut changed = current.clone();
            let mut changed_style = CursorStyle::Block;
            let mut changed_cell = cell();
            match gate {
                "hidden" => changed.cursor_visible = false,
                "style" => changed_style = CursorStyle::Bar,
                "resize" => changed.dimensions = Dimensions::new(79, 24),
                "scale" => changed_cell.width += 1,
                _ => unreachable!(),
            }
            gated.update(
                now + CURSOR_STREAK_FRAME,
                None,
                &changed,
                changed_style,
                changed_cell,
                true,
                CursorTrailStrength::Balanced,
            );
            assert!(!gated.active(), "{gate} parks the follower");
            assert!(gated.deadline().is_none(), "{gate} leaves no wake");
        }
    }
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
    Active(CursorFollower),
}

/// Per-session presentation follower. The terminal cursor target remains
/// immediate; only the cursor-shaped rectangle drawn for a large jump reads
/// this state.
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

    fn cursor_rect(position: Position, style: CursorStyle, cell: CellSize) -> [f32; 4] {
        let x0 = position.column as f32 * cell.width as f32;
        let y0 = position.row as f32 * cell.height as f32;
        let cell_w = cell.width as f32;
        let cell_h = cell.height as f32;
        match style {
            CursorStyle::Block => [x0, y0, x0 + cell_w, y0 + cell_h],
            CursorStyle::Underline => crate::grid::cursor_underline_rect(x0, y0, cell_w, cell_h),
            CursorStyle::Bar => crate::grid::cursor_bar_rect(x0, y0, cell_w, cell_h),
        }
    }

    fn edge_step(current: f32, target: f32, rate: f32, dt: f32) -> f32 {
        let response = 1.0 - (-rate * dt).exp();
        current + (target - current) * response
    }

    fn advance_follower(follower: &mut CursorFollower, dt: Duration) {
        let profile = cursor_trail_profile(follower.strength);
        let dt = dt.min(CURSOR_STREAK_MAX_DT).as_secs_f32();
        let current = follower.presented_rect;
        let target = follower.target_rect;
        let current_center = [
            0.5 * (current[0] + current[2]),
            0.5 * (current[1] + current[3]),
        ];
        let target_center = [0.5 * (target[0] + target[2]), 0.5 * (target[1] + target[3])];
        let dx = target_center[0] - current_center[0];
        let dy = target_center[1] - current_center[1];
        let (left_rate, right_rate) = if dx < 0.0 {
            (
                profile.follower_leading_rate,
                profile.follower_trailing_rate,
            )
        } else {
            (
                profile.follower_trailing_rate,
                profile.follower_leading_rate,
            )
        };
        let (top_rate, bottom_rate) = if dy < 0.0 {
            (
                profile.follower_leading_rate,
                profile.follower_trailing_rate,
            )
        } else {
            (
                profile.follower_trailing_rate,
                profile.follower_leading_rate,
            )
        };
        let mut next = [
            Self::edge_step(current[0], target[0], left_rate, dt),
            Self::edge_step(current[1], target[1], top_rate, dt),
            Self::edge_step(current[2], target[2], right_rate, dt),
            Self::edge_step(current[3], target[3], bottom_rate, dt),
        ];

        // Keep the elastic body local. The leading edge is authoritative while
        // the opposite edge is clamped to the profile's stretch budget.
        let target_w = (target[2] - target[0]).max(1.0);
        let max_w = target_w * profile.follower_max_stretch;
        if next[2] - next[0] > max_w {
            if dx < 0.0 {
                next[2] = next[0] + max_w;
            } else {
                next[0] = next[2] - max_w;
            }
        }
        let target_h = (target[3] - target[1]).max(1.0);
        let max_h = target_h * profile.follower_max_stretch;
        if next[3] - next[1] > max_h {
            if dy < 0.0 {
                next[3] = next[1] + max_h;
            } else {
                next[1] = next[3] - max_h;
            }
        }
        follower.presented_rect = next;
    }

    fn settled(follower: &CursorFollower) -> bool {
        follower
            .presented_rect
            .iter()
            .zip(follower.target_rect)
            .all(|(presented, target)| (presented - target).abs() <= CURSOR_STREAK_SETTLE_EPSILON)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_follower(
        &mut self,
        now: Instant,
        prior: PresentedCursor,
        current: Position,
        style: CursorStyle,
        dimensions: Dimensions,
        cell: CellSize,
        strength: crate::settings::CursorTrailStrength,
    ) {
        let mut follower = CursorFollower {
            destination: current,
            presented_rect: Self::cursor_rect(prior.cursor, style, cell),
            target_rect: Self::cursor_rect(current, style, cell),
            style,
            dimensions,
            cell,
            strength,
            last_frame_at: now,
            retargeted_at: now,
        };
        // The first changed frame already advances the presentation. This
        // removes the old blank dwell without depending on event-loop latency.
        Self::advance_follower(&mut follower, CURSOR_STREAK_FRAME);
        self.phase = CursorStreakPhase::Active(follower);
        self.advance_epoch();
        self.next_deadline = Some(now + CURSOR_STREAK_FRAME);
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
                    self.start_follower(
                        now,
                        prior,
                        current,
                        style,
                        snapshot.dimensions,
                        cell,
                        strength,
                    );
                } else {
                    self.next_deadline = None;
                }
            }
            CursorStreakPhase::Active(mut follower) => {
                if follower.style != style
                    || follower.dimensions != snapshot.dimensions
                    || follower.cell != cell
                {
                    self.clear();
                    return;
                }
                follower.strength = strength;
                if moved {
                    follower.destination = current;
                    follower.target_rect = Self::cursor_rect(current, style, cell);
                    follower.retargeted_at = now;
                } else if current != follower.destination {
                    self.clear();
                    return;
                }
                let dt = now.saturating_duration_since(follower.last_frame_at);
                follower.last_frame_at = now;
                Self::advance_follower(&mut follower, dt);
                let profile = cursor_trail_profile(follower.strength);
                if Self::settled(&follower)
                    || now.saturating_duration_since(follower.retargeted_at)
                        >= profile.follower_max_settle
                {
                    self.clear();
                    return;
                }
                self.phase = CursorStreakPhase::Active(follower);
                self.advance_epoch();
                self.next_deadline = Some(now + CURSOR_STREAK_FRAME);
            }
        }
    }

    pub(in crate::native) fn request(
        &self,
        _now: Instant,
        clip_rect: [f32; 4],
    ) -> Option<CursorStreakRequest> {
        let CursorStreakPhase::Active(follower) = self.phase else {
            return None;
        };
        Some(CursorStreakRequest {
            destination: follower.destination,
            rect: follower.presented_rect,
            alpha: 1.0,
            clip_rect,
        })
    }

    pub(in crate::native) fn active(&self) -> bool {
        matches!(self.phase, CursorStreakPhase::Active(_))
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
        self.cursor_streak
            .request(now, clip_rect)
            .map(|mut request| {
                request.alpha = self.cursor_blink_alpha();
                request
            })
    }

    pub(super) fn clear_cursor_streak(&mut self) {
        self.cursor_streak.discard_animation();
    }

    pub(super) fn cursor_streak_active(&self) -> bool {
        self.cursor_streak.active()
    }
}

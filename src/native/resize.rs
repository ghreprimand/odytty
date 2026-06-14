// SPDX-License-Identifier: GPL-3.0-only
use std::time::{Duration, Instant};

use winit::dpi::PhysicalSize;

use crate::text::CellSize;

pub(super) const RESIZE_DEBOUNCE_INTERVAL: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PendingResize {
    pub(super) cell: CellSize,
    pub(super) width_px: u32,
    pub(super) height_px: u32,
}

pub(super) fn pending_resize_for_surface(cell: CellSize, size: PhysicalSize<u32>) -> PendingResize {
    PendingResize {
        cell,
        width_px: size.width,
        height_px: size.height,
    }
}

pub(super) fn scale_factor_changed(current: f32, next: f32) -> bool {
    (next.max(1.0) - current.max(1.0)).abs() >= f32::EPSILON
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ResizeDebouncer {
    interval: Duration,
    pending: Option<PendingResize>,
    deadline: Option<Instant>,
    last_applied: Option<Instant>,
}

impl ResizeDebouncer {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            interval,
            pending: None,
            deadline: None,
            last_applied: None,
        }
    }

    pub(super) fn record(&mut self, resize: PendingResize, now: Instant) -> Option<PendingResize> {
        if self
            .last_applied
            .is_none_or(|last| now.saturating_duration_since(last) >= self.interval)
        {
            self.pending = None;
            self.deadline = None;
            self.last_applied = Some(now);
            return Some(resize);
        }

        let deadline = self.last_applied.expect("checked") + self.interval;
        self.pending = Some(resize);
        self.deadline = Some(deadline);
        None
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Option<PendingResize> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            self.last_applied = Some(now);
            return self.pending.take();
        }
        None
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

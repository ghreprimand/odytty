// SPDX-License-Identifier: GPL-3.0-only
//! Collection side of the env-gated memory-attribution diagnostic.
//!
//! [`crate::memory_report`] owns the record's shape, its arithmetic, the env
//! gate, and the platform resident-set read. This module owns the walk that
//! fills the record from live window state, plus the sampler that decides when
//! that walk happens.
//!
//! The sampler is a wake source, not a thread and not a poll loop. When the gate
//! is off it contributes no deadline at all, so the idle wake set is exactly
//! what it was; when the gate is on it adds one wake per interval, which reads
//! counters and appends a line. It never requests a redraw, because forcing a
//! frame in order to measure an idle process would change the quantity being
//! measured.

use std::time::{Duration, Instant};

use crate::memory_report::{
    GpuBytes, HostBytes, MemoryReport, read_process_memory, sample_interval,
};

use super::state::App;

/// Sampler state for the memory-attribution diagnostic.
///
/// Holds a period and the next due instant, or nothing at all when the gate is
/// off. The `Option` is the whole off path: [`Self::deadline`] returns `None`,
/// so the diagnostic adds no entry to the event loop's wake set and cannot
/// extend an idle terminal's wake behavior.
#[derive(Debug)]
pub(in crate::native) struct MemorySampler {
    schedule: Option<Schedule>,
}

#[derive(Debug)]
struct Schedule {
    interval: Duration,
    next: Instant,
}

impl MemorySampler {
    /// Build the sampler from the process environment. Reads the gate exactly
    /// once (it is memoized in [`crate::memory_report`]); a disabled gate yields
    /// an inert sampler.
    pub(in crate::native) fn from_env(now: Instant) -> Self {
        Self {
            schedule: sample_interval().map(|interval| Schedule {
                interval,
                next: now + interval,
            }),
        }
    }

    /// The next sampling instant, or `None` when the diagnostic is off. Folded
    /// into the event loop's wake set, where a `None` leaves the set unchanged.
    pub(in crate::native) fn deadline(&self) -> Option<Instant> {
        self.schedule.as_ref().map(|schedule| schedule.next)
    }

    /// Whether a sample is due at `now`, advancing the schedule when it is.
    ///
    /// The next instant is computed from `now` rather than by adding the
    /// interval to the previous deadline, so a delayed wake cannot leave the
    /// schedule pointing into the past and spin the loop.
    fn take_due(&mut self, now: Instant) -> bool {
        let Some(schedule) = self.schedule.as_mut() else {
            return false;
        };
        if now < schedule.next {
            return false;
        }
        schedule.next = now + schedule.interval;
        true
    }
}

impl App {
    /// Build a memory-attribution report from live window state.
    ///
    /// Walks every live pane for grid, scrollback, and graphics-store bytes, and
    /// the GPU state for atlas, background-image, post-process, and buffer
    /// bytes. Fields whose subsystem is not currently instantiated report a
    /// measured zero rather than being omitted, so a reader can tell "this costs
    /// nothing right now" apart from "this was not counted".
    ///
    /// Read-only: it takes `&self`, allocates nothing beyond the pane walk's
    /// short-lived locks, and changes no render or terminal state.
    pub(in crate::native) fn collect_memory_report(&self) -> MemoryReport {
        let mut host = HostBytes::default();
        let mut gpu = GpuBytes::default();
        let mut panes = 0u64;

        for session in self.sessions.iter() {
            panes = panes.saturating_add(1);
            let terminal = crate::native::lock_recover(&session.terminal);
            host.grid_cells = host.grid_cells.saturating_add(terminal.grid_bytes());
            host.scrollback_cells = host
                .scrollback_cells
                .saturating_add(terminal.scrollback_bytes());
            host.graphics_image_store = host
                .graphics_image_store
                .saturating_add(terminal.graphics_store_bytes());
        }

        if let Some(gpu_state) = self.gpu.as_ref() {
            gpu_state.fill_memory_report(&mut host, &mut gpu);
        }

        MemoryReport {
            process: read_process_memory(),
            host,
            gpu,
            panes,
        }
    }

    /// Sample and append one report line if the diagnostic is enabled and a
    /// sample is due. Called from `about_to_wait` maintenance; a single
    /// `Option` check when off.
    pub(super) fn run_memory_report_sample(&mut self, now: Instant) {
        if !self.memory_sampler.take_due(now) {
            return;
        }
        let report = self.collect_memory_report();
        crate::memory_report::append_report(&report);
    }
}

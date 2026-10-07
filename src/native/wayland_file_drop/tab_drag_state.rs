// SPDX-License-Identifier: GPL-3.0-only
//! Original wire-press identity and separate window/data-source completion.

use super::state::SurfaceIdent;

pub(super) const MAX_DRAG_WINDOWS: usize = 64;
pub(super) const MAX_DRAG_SLOTS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DragRect(pub(crate) [f64; 4]);

impl DragRect {
    pub(crate) fn contains(self, point: [f64; 2]) -> bool {
        let [x, y, width, height] = self.0;
        point.into_iter().all(f64::is_finite)
            && self.0.into_iter().all(f64::is_finite)
            && width > 0.0
            && height > 0.0
            && point[0] >= x
            && point[0] < x + width
            && point[1] >= y
            && point[1] < y + height
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DragSlot {
    pub(crate) token: u64,
    pub(crate) rect: DragRect,
    pub(crate) close: Option<DragRect>,
}

/// Geometry is published in surface-logical coordinates before the wire press.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DragRegion {
    pub(crate) ptr: u64,
    pub(crate) ident: SurfaceIdent,
    pub(crate) band: DragRect,
    pub(crate) slots: Vec<DragSlot>,
    pub(crate) new_slot: Option<DragRect>,
}

impl DragRegion {
    fn hit(&self, point: [f64; 2]) -> Option<u64> {
        if !self.band.contains(point) || self.new_slot.is_some_and(|rect| rect.contains(point)) {
            return None;
        }
        self.slots.iter().rev().find_map(|slot| {
            (slot.rect.contains(point) && !slot.close.is_some_and(|rect| rect.contains(point)))
                .then_some(slot.token)
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WirePress {
    pub(crate) identity: u64,
    pub(crate) serial: u32,
    pub(crate) seat: u32,
    pub(crate) region: DragRegion,
    pub(crate) token: u64,
    pub(crate) point: [f64; 2],
}

#[derive(Debug, Default)]
pub(super) struct PressLedger {
    next_identity: u64,
    press: Option<WirePress>,
}

impl PressLedger {
    pub(super) fn press(
        &mut self,
        serial: u32,
        seat: u32,
        region: &DragRegion,
        point: [f64; 2],
    ) -> Option<&WirePress> {
        // Another press always invalidates prior custody, including a press on
        // a different seat or a close button at the same position.
        self.press = None;
        let token = region.hit(point)?;
        self.next_identity = self.next_identity.checked_add(1)?;
        self.press = Some(WirePress {
            identity: self.next_identity,
            serial,
            seat,
            region: region.clone(),
            token,
            point,
        });
        self.press.as_ref()
    }

    pub(super) fn invalidate(&mut self) {
        self.press = None;
    }

    pub(super) fn captured(&self) -> Option<&WirePress> {
        self.press.as_ref()
    }

    pub(super) fn claim(
        &mut self,
        identity: u64,
        current: &DragRegion,
        token: u64,
        point: [f64; 2],
    ) -> Option<WirePress> {
        let press = self.press.as_ref()?;
        if press.identity != identity
            || &press.region != current
            || press.token != token
            || !point.into_iter().all(f64::is_finite)
            || (press.point[0] - point[0]).abs() > 0.001
            || (press.point[1] - point[1]).abs() > 0.001
        {
            return None;
        }
        self.press.take()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DragCompletion {
    Dropped,
    Cancelled,
    /// Start was refused; this does not end an existing protocol object.
    Refused,
}

/// A local rollback does not grant permission to destroy an ongoing extension.
#[derive(Debug, Default)]
pub(super) struct ProtocolLife {
    started: bool,
    ended: Option<DragCompletion>,
    source_finished: bool,
    rolled_back: bool,
    delivered: bool,
}

/// Ordered display callbacks prevent a stale acknowledgement from mapping a
/// later attachment. Completion and its drop target travel through one gate.
#[derive(Debug, Default)]
pub(super) struct DeliveryGate<T> {
    generation: u64,
    pending: Option<u64>,
    completion: Option<(DragCompletion, T)>,
}

impl<T> DeliveryGate<T> {
    pub(super) fn start(&mut self) {
        self.pending = Some(0);
    }

    pub(super) fn attach(&mut self) -> Option<u64> {
        self.generation = self.generation.checked_add(1)?;
        self.pending = Some(self.generation);
        Some(self.generation)
    }

    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn matches(&self, generation: u64) -> bool {
        self.pending == Some(generation)
    }

    pub(super) fn complete(
        &mut self,
        completion: DragCompletion,
        target: T,
    ) -> Option<(DragCompletion, T)> {
        let completed = (completion, target);
        if self.pending() {
            self.completion = Some(completed);
            None
        } else {
            Some(completed)
        }
    }

    pub(super) fn synchronize(&mut self, generation: u64) -> Option<Option<(DragCompletion, T)>> {
        if !self.matches(generation) {
            return None;
        }
        self.pending = None;
        Some(self.completion.take())
    }
}

impl ProtocolLife {
    pub(super) fn start(&mut self) {
        self.started = true;
    }

    pub(super) fn rollback(&mut self) {
        self.rolled_back = true;
    }

    pub(super) fn event(&mut self, outcome: DragCompletion) -> Option<DragCompletion> {
        if outcome == DragCompletion::Refused {
            return None;
        }
        // Some compositors send cancelled after drop_performed when no data
        // destination accepted the offer. Placement already ended at Drop.
        if self.ended.is_none() {
            self.ended = Some(outcome);
        }
        if outcome == DragCompletion::Cancelled {
            self.source_finished = true;
        }
        if self.delivered {
            return None;
        }
        self.delivered = true;
        Some(if self.rolled_back {
            DragCompletion::Cancelled
        } else {
            self.ended.expect("completion recorded")
        })
    }

    pub(super) fn finished(&mut self) {
        self.source_finished = true;
    }

    pub(super) fn may_destroy_extension(&self) -> bool {
        self.started && self.ended.is_some()
    }

    pub(super) fn may_destroy_source(&self) -> bool {
        self.may_destroy_extension() && self.source_finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> DragRegion {
        DragRegion {
            ptr: 10,
            ident: SurfaceIdent {
                window: 2,
                generation: 3,
            },
            band: DragRect([0.0, 0.0, 100.0, 20.0]),
            new_slot: None,
            slots: vec![DragSlot {
                token: 7,
                rect: DragRect([0.0, 0.0, 50.0, 20.0]),
                close: Some(DragRect([40.0, 0.0, 10.0, 20.0])),
            }],
        }
    }

    #[test]
    fn wayland_tab_original_serial_is_bound_to_wire_hit_and_incarnation() {
        let mut ledger = PressLedger::default();
        let region = region();
        let press = ledger.press(123, 4, &region, [12.0, 8.0]).unwrap().clone();
        assert_eq!(press.token, 7);
        assert_eq!(press.serial, 123);
        let mut stale = region.clone();
        stale.ident.generation += 1;
        assert!(
            ledger
                .claim(press.identity, &stale, 7, press.point)
                .is_none()
        );
        assert!(
            ledger
                .claim(press.identity, &region, 8, press.point)
                .is_none()
        );
        assert_eq!(
            ledger.claim(press.identity, &region, 7, press.point),
            Some(press)
        );
        assert!(ledger.captured().is_none());
    }

    #[test]
    fn wayland_tab_repeated_click_other_seat_release_and_geometry_reject_stale_press() {
        let mut ledger = PressLedger::default();
        let region = region();
        let old = ledger.press(10, 1, &region, [12.0, 8.0]).unwrap().clone();
        let next = ledger.press(11, 2, &region, old.point).unwrap().clone();
        assert_ne!(old.identity, next.identity);
        assert!(ledger.claim(old.identity, &region, 7, old.point).is_none());
        let mut moved = region.clone();
        moved.slots[0].rect.0[0] += 1.0;
        assert!(ledger.claim(next.identity, &moved, 7, next.point).is_none());
        ledger.invalidate();
        assert!(
            ledger
                .claim(next.identity, &region, 7, next.point)
                .is_none()
        );
        assert!(ledger.press(12, 1, &region, [45.0, 8.0]).is_none());
        assert!(ledger.press(13, 1, &region, [f64::NAN, 8.0]).is_none());
    }

    #[test]
    fn wayland_tab_local_rollback_and_raw_release_never_end_protocol_custody() {
        let mut life = ProtocolLife::default();
        life.start();
        life.rollback();
        assert!(!life.may_destroy_extension());
        assert!(!life.may_destroy_source());
        assert_eq!(
            life.event(DragCompletion::Dropped),
            Some(DragCompletion::Cancelled)
        );
        assert!(life.may_destroy_extension());
        assert!(!life.may_destroy_source());
        life.finished();
        assert!(life.may_destroy_source());
        assert_eq!(life.event(DragCompletion::Cancelled), None);
    }

    #[test]
    fn wayland_tab_drop_then_cancel_and_duplicate_callbacks_commit_once() {
        let mut life = ProtocolLife::default();
        life.start();
        assert_eq!(
            life.event(DragCompletion::Dropped),
            Some(DragCompletion::Dropped)
        );
        assert_eq!(life.event(DragCompletion::Cancelled), None);
        assert!(life.may_destroy_source());
        assert_eq!(life.event(DragCompletion::Dropped), None);
        let mut cancelled = ProtocolLife::default();
        cancelled.start();
        assert_eq!(
            cancelled.event(DragCompletion::Cancelled),
            Some(DragCompletion::Cancelled)
        );
        assert!(cancelled.may_destroy_source());
        assert_eq!(cancelled.event(DragCompletion::Dropped), None);
    }

    #[test]
    fn wayland_tab_sync_completion_and_drop_target_are_one_delivery() {
        let mut gate = DeliveryGate::<u64>::default();
        gate.start();
        assert_eq!(gate.attach(), Some(1));
        assert_eq!(gate.complete(DragCompletion::Dropped, 19), None);
        assert_eq!(gate.synchronize(0), None);
        assert!(gate.pending());
        assert_eq!(
            gate.synchronize(1),
            Some(Some((DragCompletion::Dropped, 19)))
        );
        assert!(!gate.pending());
        assert_eq!(gate.synchronize(1), None);
    }

    #[test]
    fn wayland_tab_reattach_rejects_the_prior_surface_acknowledgement() {
        let mut gate = DeliveryGate::<u64>::default();
        assert_eq!(gate.attach(), Some(1));
        assert_eq!(gate.attach(), Some(2));
        assert_eq!(gate.synchronize(1), None);
        assert_eq!(gate.complete(DragCompletion::Cancelled, 23), None);
        assert_eq!(
            gate.synchronize(2),
            Some(Some((DragCompletion::Cancelled, 23)))
        );
        assert_eq!(
            gate.complete(DragCompletion::Dropped, 31),
            Some((DragCompletion::Dropped, 31))
        );
    }

    #[test]
    fn wayland_tab_early_drop_and_ignored_start_keep_distinct_lifetimes() {
        let mut early = DeliveryGate::<u64>::default();
        early.start();
        assert_eq!(early.complete(DragCompletion::Dropped, 37), None);
        assert_eq!(
            early.synchronize(0),
            Some(Some((DragCompletion::Dropped, 37)))
        );
        let mut life = ProtocolLife::default();
        life.start();
        life.rollback();
        assert_eq!(life.event(DragCompletion::Refused), None);
        assert!(!life.may_destroy_extension());
        assert!(!life.may_destroy_source());
        assert_eq!(
            life.event(DragCompletion::Cancelled),
            Some(DragCompletion::Cancelled)
        );
        assert!(life.may_destroy_source());
    }
}

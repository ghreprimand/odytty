// SPDX-License-Identifier: GPL-3.0-only
//! Bounded bridge between the window owner and the existing Wayland listener.

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use super::tab_drag_state::{DragCompletion, DragRegion, PressLedger, WirePress};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DragTarget {
    pub(crate) window: u64,
    pub(crate) generation: u64,
    pub(crate) point: [f64; 2],
}

#[derive(Debug, Clone)]
pub(super) struct Attach {
    pub(super) press: WirePress,
    pub(super) source: Arc<winit::window::Window>,
    pub(super) destination: Option<Arc<winit::window::Window>>,
    pub(super) offset: [i32; 2],
}

#[derive(Debug, Default)]
pub(super) struct Shared {
    pub(super) available: bool,
    pub(super) wake_pending: bool,
    pub(super) regions: Vec<DragRegion>,
    pub(super) presses: PressLedger,
    pub(super) commands: VecDeque<Attach>,
    pub(super) active: Option<u64>,
    pub(super) reserved: Option<WirePress>,
    pub(super) started: bool,
    pub(super) released: bool,
    pub(super) attached: Option<u64>,
    pub(super) rollback: bool,
    pub(super) cancelled: Option<u64>,
    pub(super) target: Option<DragTarget>,
    pub(super) drop_target: Option<DragTarget>,
    pub(super) ended: Option<(u64, DragCompletion)>,
    pub(super) ended_target: Option<DragTarget>,
    pub(super) completion: Option<(u64, DragCompletion)>,
}

impl Shared {
    pub(super) fn note_drop(&mut self) {
        self.drop_target = self.target;
    }

    pub(super) fn completion_target(&self) -> Option<DragTarget> {
        self.drop_target.or(self.target)
    }

    pub(super) fn cancel_active(&mut self) {
        self.presses.invalidate();
        if self.ended.is_none()
            && let Some(identity) = self.active
        {
            self.rollback = true;
            self.attached = None;
            self.completion = Some((identity, DragCompletion::Cancelled));
        }
    }

    pub(super) fn disable(&mut self) {
        self.available = false;
        self.cancel_active();
    }

    pub(super) fn ordinary_button(&mut self) -> bool {
        let changed = self.presses.captured().is_some() || self.reserved.is_some();
        self.presses.invalidate();
        if self.reserved.is_some() {
            self.released = true;
            if !self.started {
                self.reserved = None;
            } else if self.ended.is_none()
                && let Some(identity) = self.active
            {
                // A successful DnD grab consumes ordinary pointer buttons.
                // A normal button after Start, even after its Sync, proves
                // that the compositor did not take that implicit grab.
                self.rollback = true;
                self.attached = None;
                self.ended = Some((identity, DragCompletion::Refused));
                self.completion = Some((identity, DragCompletion::Refused));
            }
        }
        changed
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TabDragBridge(Arc<Mutex<Shared>>, Option<Arc<OwnedFd>>);

impl TabDragBridge {
    pub(super) fn new(wake: OwnedFd) -> Self {
        Self(Arc::default(), Some(Arc::new(wake)))
    }

    pub(super) fn queue_wake(&self) -> bool {
        let mut shared = self.lock();
        if shared.wake_pending {
            return false;
        }
        shared.wake_pending = true;
        true
    }

    /// Only consuming the posted host event clears this flag; ordinary
    /// maintenance reads must not admit additional queued wakes.
    pub(crate) fn acknowledge_wake(&self) {
        self.lock().wake_pending = false;
    }

    pub(super) fn can_wake(&self) -> bool {
        self.1.is_some()
    }

    fn wake(&self) {
        if let Some(fd) = self.1.as_ref() {
            // SAFETY: the owned non-blocking socket is live and the byte is valid
            // for this call. EAGAIN means a prior wake is already pending.
            let byte = [1_u8];
            let _ =
                unsafe { libc::send(fd.as_raw_fd(), byte.as_ptr().cast(), 1, libc::MSG_NOSIGNAL) };
        }
    }
    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn publish(&self, regions: Vec<DragRegion>) {
        let mut state = self.lock();
        let slots = regions.iter().try_fold(0_usize, |count, region| {
            count.checked_add(region.slots.len())
        });
        if regions.len() > super::tab_drag_state::MAX_DRAG_WINDOWS
            || slots.is_none_or(|count| count > super::tab_drag_state::MAX_DRAG_SLOTS)
        {
            state.regions.clear();
            state.presses.invalidate();
        } else {
            state.regions = regions;
        }
    }

    pub(crate) fn available(&self) -> bool {
        self.lock().available
    }

    pub(crate) fn captured(&self) -> Option<WirePress> {
        let state = self.lock();
        (state.available && state.active.is_none())
            .then(|| state.presses.captured().cloned())
            .flatten()
    }

    pub(crate) fn reserve(&self, press: &WirePress) -> bool {
        let mut state = self.lock();
        if state.active == Some(press.identity) {
            return !state.rollback;
        }
        if state.active.is_some() || !state.available {
            return false;
        }
        let Some(region) = state
            .regions
            .iter()
            .find(|region| region.ident == press.region.ident)
            .cloned()
        else {
            return false;
        };
        let Some(reserved) = state
            .presses
            .claim(press.identity, &region, press.token, press.point)
        else {
            return false;
        };
        state.active = Some(press.identity);
        state.reserved = Some(reserved);
        state.started = false;
        state.released = false;
        state.rollback = false;
        state.attached = None;
        state.ended = None;
        state.target = None;
        state.drop_target = None;
        state.ended_target = None;
        state.completion = None;
        true
    }

    /// Queue at most one attachment. The destination remains unmapped until
    /// the listener acknowledges the same original wire-press identity.
    pub(crate) fn attach(
        &self,
        press: WirePress,
        source: Arc<winit::window::Window>,
        destination: Arc<winit::window::Window>,
        offset: [i32; 2],
    ) -> bool {
        let mut state = self.lock();
        if !state.available
            || state.commands.len() >= 2
            || state
                .ended
                .is_some_and(|(identity, _)| identity == press.identity)
        {
            return false;
        }
        if state
            .active
            .is_some_and(|identity| identity != press.identity)
        {
            return false;
        }
        state.attached = None;
        state.commands.push_back(Attach {
            press,
            source,
            destination: Some(destination),
            offset,
        });
        drop(state);
        self.wake();
        true
    }

    pub(crate) fn start(&self, press: WirePress, source: Arc<winit::window::Window>) -> bool {
        let mut state = self.lock();
        if state.started && state.active == Some(press.identity) && !state.rollback {
            return true;
        }
        if !state.available || state.commands.len() >= 2 || state.active != Some(press.identity) {
            return false;
        }
        state.commands.push_back(Attach {
            press,
            source,
            destination: None,
            offset: [0, 0],
        });
        drop(state);
        self.wake();
        true
    }

    pub(crate) fn ended(&self, identity: u64) -> Option<DragCompletion> {
        self.lock()
            .ended
            .filter(|(id, _)| *id == identity)
            .map(|(_, outcome)| outcome)
    }

    pub(crate) fn was_released(&self, identity: u64) -> bool {
        let state = self.lock();
        state.released
            && (state.active == Some(identity)
                || state.completion.is_some_and(|(id, _)| id == identity))
    }

    pub(crate) fn attached(&self, identity: u64) -> bool {
        self.lock().attached == Some(identity)
    }

    pub(crate) fn rollback(&self, identity: u64) {
        let mut state = self.lock();
        state.cancelled = Some(identity);
        if state.active == Some(identity) {
            state.rollback = true;
        }
        let queued = state
            .commands
            .iter()
            .any(|command| command.press.identity == identity);
        state
            .commands
            .retain(|command| command.press.identity != identity);
        if queued && !state.started {
            state.completion = Some((identity, DragCompletion::Cancelled));
        }
        if !state.started && state.active == Some(identity) {
            state.active = None;
            state.reserved = None;
        }
        drop(state);
        self.wake();
    }

    pub(crate) fn target(&self, identity: u64) -> Option<DragTarget> {
        let state = self.lock();
        (state.active == Some(identity))
            .then_some(state.target)
            .flatten()
    }

    pub(crate) fn take_completion(
        &self,
        identity: u64,
    ) -> Option<(DragCompletion, Option<DragTarget>)> {
        let mut state = self.lock();
        if state.completion.is_some_and(|(id, _)| id == identity) {
            state
                .completion
                .take()
                .map(|(_, result)| (result, state.ended_target.take()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::SurfaceIdent;
    use super::super::tab_drag_state::{DragRect, DragSlot, MAX_DRAG_SLOTS, MAX_DRAG_WINDOWS};
    use super::*;

    fn region() -> DragRegion {
        DragRegion {
            ptr: 10,
            ident: SurfaceIdent {
                window: 2,
                generation: 3,
            },
            band: DragRect([0.0, 0.0, 100.0, 20.0]),
            slots: vec![DragSlot {
                token: 7,
                rect: DragRect([0.0, 0.0, 50.0, 20.0]),
                close: None,
            }],
            new_slot: None,
        }
    }

    fn captured(bridge: &TabDragBridge) -> WirePress {
        let region = region();
        bridge.publish(vec![region.clone()]);
        let mut shared = bridge.lock();
        shared.available = true;
        shared
            .presses
            .press(123, 4, &region, [12.0, 8.0])
            .unwrap()
            .clone()
    }

    #[test]
    fn wayland_tab_reservation_survives_terminal_detach_but_not_changed_geometry() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        let mut changed = region();
        changed.slots[0].rect.0[0] += 1.0;
        bridge.publish(vec![changed]);
        assert!(!bridge.reserve(&press));
        bridge.publish(vec![region()]);
        assert!(bridge.reserve(&press));
        bridge.publish(Vec::new());
        assert_eq!(bridge.lock().reserved.as_ref(), Some(&press));
        assert!(bridge.captured().is_none());
    }

    #[test]
    fn wayland_tab_rollback_before_start_releases_reservation_without_protocol_end() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        assert!(bridge.reserve(&press));
        bridge.rollback(press.identity);
        assert!(bridge.lock().active.is_none());
        assert!(bridge.lock().reserved.is_none());
        assert_eq!(bridge.ended(press.identity), None);
        assert!(!bridge.reserve(&press));
    }

    #[test]
    fn wayland_tab_rollback_after_start_retains_bounded_protocol_custody() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        assert!(bridge.reserve(&press));
        bridge.lock().started = true;
        bridge.rollback(press.identity);
        assert_eq!(bridge.lock().active, Some(press.identity));
        assert!(bridge.lock().rollback);
        assert!(bridge.captured().is_none());
        assert_eq!(bridge.ended(press.identity), None);
    }

    #[test]
    fn wayland_tab_publication_caps_windows_and_total_hit_slots() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        bridge.publish(vec![region(); MAX_DRAG_WINDOWS + 1]);
        assert!(bridge.lock().regions.is_empty());
        assert!(!bridge.reserve(&press));
        let mut wide = region();
        wide.slots = vec![wide.slots[0].clone(); MAX_DRAG_SLOTS + 1];
        bridge.publish(vec![wide]);
        assert!(bridge.lock().regions.is_empty());
    }

    #[test]
    fn wayland_tab_new_reservation_does_not_inherit_a_prior_drop_target() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        bridge.lock().target = Some(DragTarget {
            window: 2,
            generation: 3,
            point: [12.0, 8.0],
        });
        bridge.lock().completion = Some((91, DragCompletion::Dropped));
        assert!(bridge.reserve(&press));
        assert_eq!(bridge.target(press.identity), None);
        assert!(bridge.lock().completion.is_none());
    }
    #[test]
    fn wayland_tab_ordinary_release_after_sync_restores_ignored_drag_custody() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        assert!(bridge.reserve(&press));
        {
            let mut shared = bridge.lock();
            shared.started = true;
            shared.attached = Some(press.identity);
            assert!(shared.ordinary_button());
        }
        assert_eq!(bridge.ended(press.identity), Some(DragCompletion::Refused));
        assert!(!bridge.attached(press.identity));
        assert!(bridge.was_released(press.identity));
        assert_eq!(bridge.lock().active, Some(press.identity));
        assert!(bridge.lock().rollback);
        assert_eq!(
            bridge.take_completion(press.identity),
            Some((DragCompletion::Refused, None))
        );
    }
    #[test]
    fn wayland_tab_wake_socket_is_nonblocking_and_survives_reader_close() {
        let (read, write) = super::super::socketpair_nonblocking().unwrap();
        let bridge = TabDragBridge::new(write);
        assert!(bridge.can_wake());
        bridge.wake();
        let mut byte = [0_u8; 1];
        // SAFETY: the owned descriptor and one-byte destination remain live.
        assert_eq!(
            unsafe { libc::read(read.as_raw_fd(), byte.as_mut_ptr().cast(), 1) },
            1
        );
        assert_eq!(byte, [1]);
        // SAFETY: the same owned non-blocking descriptor and buffer remain live.
        assert_eq!(
            unsafe { libc::read(read.as_raw_fd(), byte.as_mut_ptr().cast(), 1) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(read);
        bridge.wake();
    }
    #[test]
    fn wayland_tab_withdrawal_then_reannouncement_never_revives_old_custody() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        assert!(bridge.reserve(&press));
        {
            let mut shared = bridge.lock();
            shared.started = true;
            shared.attached = Some(press.identity);
            shared.disable();
            shared.available = true;
        }
        assert!(!bridge.reserve(&press));
        assert!(!bridge.attached(press.identity));
        assert_eq!(bridge.ended(press.identity), None);
        assert_eq!(
            bridge.take_completion(press.identity),
            Some((DragCompletion::Cancelled, None))
        );
        assert_eq!(bridge.lock().active, Some(press.identity));
    }
    #[test]
    fn wayland_tab_drop_target_survives_destination_leave_before_source_completion() {
        let bridge = TabDragBridge::default();
        let press = captured(&bridge);
        assert!(bridge.reserve(&press));
        let target = DragTarget {
            window: 2,
            generation: 3,
            point: [40.0, 8.0],
        };
        let mut shared = bridge.lock();
        shared.target = Some(target);
        shared.note_drop();
        shared.target = None;
        assert_eq!(shared.completion_target(), Some(target));
    }
    #[test]
    fn wayland_tab_motion_wakes_coalesce_until_the_real_event_is_consumed() {
        let bridge = TabDragBridge::default();
        assert!(bridge.queue_wake());
        for x in 0..256 {
            bridge.lock().target = Some(DragTarget {
                window: 2,
                generation: 3,
                point: [f64::from(x), 8.0],
            });
            assert!(!bridge.queue_wake());
        }
        bridge.acknowledge_wake();
        assert!(bridge.queue_wake());
        assert_eq!(bridge.lock().target.unwrap().point, [255.0, 8.0]);
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! Seat-owned pointer observation and outbound toplevel-drag protocol objects.

use std::collections::HashMap;

use wayland_client::protocol::{wl_callback, wl_data_source, wl_pointer};
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols::xdg::toplevel_drag::v1::client::{
    xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1, xdg_toplevel_drag_v1::XdgToplevelDragV1,
};
use winit::platform::wayland::WindowExtWayland;

use super::tab_drag::{DragTarget, TabDragBridge};
use super::tab_drag_state::{DeliveryGate, DragCompletion, ProtocolLife, WirePress};
use super::*;

#[derive(Default)]
struct PointerState {
    pointer: Option<wl_pointer::WlPointer>,
    surface: Option<u64>,
    point: [f64; 2],
}

struct WireDrag {
    press: WirePress,
    source: wl_data_source::WlDataSource,
    extension: Option<XdgToplevelDragV1>,
    life: ProtocolLife,
    mime: String,
    barrier: DeliveryGate<Option<DragTarget>>,
}

pub(super) struct TabProtocol {
    pub(super) bridge: TabDragBridge,
    pub(super) manager: Option<XdgToplevelDragManagerV1>,
    pub(super) global: Option<u32>,
    pointers: HashMap<u32, PointerState>,
    drag: Option<WireDrag>,
}

impl TabProtocol {
    pub(super) fn new(bridge: TabDragBridge) -> Self {
        Self {
            bridge,
            manager: None,
            global: None,
            pointers: HashMap::new(),
            drag: None,
        }
    }

    pub(super) fn wake(&self, proxy: &EventLoopProxy<UserEvent>) {
        if self.bridge.queue_wake() && proxy.send_event(UserEvent::WaylandTabDragWake).is_err() {
            self.bridge.acknowledge_wake();
        }
    }

    pub(super) fn seat_capability(&mut self, seat: u32, pointer: bool) {
        if pointer {
            self.pointers.entry(seat).or_default();
        } else {
            self.remove_seat(seat);
        }
    }

    pub(super) fn create_pointers(
        &mut self,
        seats: &HashMap<u32, wl_seat::WlSeat>,
        qh: &QueueHandle<Listener>,
    ) {
        if self.manager.is_none() || self.global.is_none() {
            return;
        }
        for (name, state) in &mut self.pointers {
            if state.pointer.is_none()
                && let Some(seat) = seats.get(name)
            {
                state.pointer = Some(seat.get_pointer(qh, *name));
            }
        }
        self.bridge.lock().available = self.pointers.values().any(|state| state.pointer.is_some());
    }

    pub(super) fn remove_seat(&mut self, seat: u32) {
        if let Some(state) = self.pointers.remove(&seat)
            && let Some(pointer) = state.pointer
            && pointer.version() >= 3
        {
            pointer.release();
        }
        let mut shared = self.bridge.lock();
        shared.presses.invalidate();
        if shared
            .reserved
            .as_ref()
            .is_some_and(|press| press.seat == seat)
        {
            shared.cancel_active();
        }
        shared.available = self.manager.is_some()
            && self.global.is_some()
            && self.pointers.values().any(|state| state.pointer.is_some());
    }

    pub(super) fn commands(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Listener>,
        proxy: &EventLoopProxy<UserEvent>,
        devices: &HashMap<u32, wl_data_device::WlDataDevice>,
        manager: Option<&wl_data_device_manager::WlDataDeviceManager>,
    ) {
        for _ in 0..2 {
            self.command(conn, qh, proxy, devices, manager);
        }
    }

    fn command(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Listener>,
        proxy: &EventLoopProxy<UserEvent>,
        devices: &HashMap<u32, wl_data_device::WlDataDevice>,
        data_manager: Option<&wl_data_device_manager::WlDataDeviceManager>,
    ) {
        if self.bridge.lock().rollback
            && let Some(drag) = self.drag.as_mut()
        {
            drag.life.rollback();
        }
        let command = {
            let mut shared = self.bridge.lock();
            let Some(command) = shared.commands.pop_front() else {
                return;
            };
            let identity = command.press.identity;
            let accepted = shared.active == Some(identity)
                && shared.cancelled != Some(identity)
                && !shared.rollback
                && if let Some(drag) = self.drag.as_ref() {
                    drag.press == command.press && !drag.life.may_destroy_extension()
                } else {
                    !shared.released && shared.reserved.as_ref() == Some(&command.press)
                };
            if !accepted {
                if let Some((id, outcome)) = shared.ended.filter(|(id, _)| *id == identity) {
                    if outcome == DragCompletion::Dropped {
                        // Drop preceded window preparation. There is no ongoing
                        // role to attach: map the already prepared window via
                        // the ordinary release-time fallback instead.
                        shared.attached = Some(id);
                    } else {
                        shared.completion = Some((id, outcome));
                    }
                    drop(shared);
                    self.wake(proxy);
                    return;
                }
                shared.completion = Some((
                    identity,
                    if self.drag.is_none() {
                        DragCompletion::Refused
                    } else {
                        DragCompletion::Cancelled
                    },
                ));
                drop(shared);
                self.wake(proxy);
                return;
            }
            shared.started = true;
            command
        };
        let identity = command.press.identity;
        if self.drag.is_none() {
            let Some(manager) = self.manager.as_ref() else {
                self.failed(identity, proxy);
                return;
            };
            let Some(device) = devices.get(&command.press.seat) else {
                self.failed(identity, proxy);
                return;
            };
            let Some(data_manager) = data_manager else {
                self.failed(identity, proxy);
                return;
            };
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let source_ptr =
                command
                    .source
                    .window_handle()
                    .ok()
                    .and_then(|handle| match handle.as_raw() {
                        RawWindowHandle::Wayland(handle) => Some(handle.surface.as_ptr() as u64),
                        _ => None,
                    });
            if source_ptr != Some(command.press.region.ptr) {
                self.failed(identity, proxy);
                return;
            }
            // SAFETY: source's Window Arc owns this exact surface incarnation
            // until origin and the requests using it have been dropped. The
            // observer's pointer event selected it; winit owns its destructor.
            let origin = unsafe {
                ObjectId::from_ptr(
                    wl_surface::WlSurface::interface(),
                    command.press.region.ptr as usize as *mut _,
                )
            }
            .ok()
            .and_then(|id| wl_surface::WlSurface::from_id(conn, id).ok());
            let Some(origin) = origin else {
                self.failed(identity, proxy);
                return;
            };
            let source = data_manager.create_data_source(qh, identity);
            let mime = format!(
                "application/x-odytty-tab-{}-{}",
                std::process::id(),
                identity
            );
            source.offer(mime.clone());
            source.set_actions(wl_data_device_manager::DndAction::Move);
            let extension = manager.get_xdg_toplevel_drag(&source, qh, ());
            device.start_drag(Some(&source), &origin, None, command.press.serial);
            let mut life = ProtocolLife::default();
            life.start();
            self.drag = Some(WireDrag {
                press: command.press.clone(),
                source,
                extension: Some(extension),
                life,
                mime,
                barrier: DeliveryGate::default(),
            });
            drop(origin);
        }
        let Some(destination) = command.destination.as_ref() else {
            if let Some(drag) = self.drag.as_mut() {
                drag.barrier.start();
                conn.display().sync(qh, (identity, 0));
                if conn.flush().is_err() {
                    self.failed(identity, proxy);
                }
            }
            return;
        };
        let Some(toplevel_ptr) = destination.xdg_toplevel() else {
            self.failed(identity, proxy);
            return;
        };
        // SAFETY: destination's Window Arc keeps the foreign role alive until
        // this borrowed proxy and its requests have been dropped. Winit owns
        // the role's destructor; this listener never destroys the toplevel.
        let toplevel =
            unsafe { ObjectId::from_ptr(XdgToplevel::interface(), toplevel_ptr.as_ptr().cast()) }
                .ok()
                .and_then(|id| XdgToplevel::from_id(conn, id).ok());
        let Some(toplevel) = toplevel else {
            self.failed(identity, proxy);
            return;
        };
        if let Some(drag) = self.drag.as_mut()
            && let Some(extension) = drag.extension.as_ref()
        {
            let Some(generation) = drag.barrier.attach() else {
                self.failed(identity, proxy);
                return;
            };
            extension.attach(&toplevel, command.offset[0], command.offset[1]);
            conn.display().sync(qh, (identity, generation));
            if conn.flush().is_err() {
                self.failed(identity, proxy);
            }
        }
        drop(toplevel);
        self.wake(proxy);
    }

    fn failed(&mut self, identity: u64, proxy: &EventLoopProxy<UserEvent>) {
        let mut shared = self.bridge.lock();
        shared.completion = Some((
            identity,
            if self.drag.is_none() {
                DragCompletion::Refused
            } else {
                DragCompletion::Cancelled
            },
        ));
        shared.attached = None;
        shared.rollback = true;
        if self.drag.is_none() {
            shared.active = None;
            shared.started = false;
            shared.reserved = None;
        }
        drop(shared);
        self.wake(proxy);
    }

    pub(super) fn owns_offer(&self, mime: &str) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.mime == mime)
    }

    pub(super) fn target(
        &self,
        seat: u32,
        target: Option<DragTarget>,
        proxy: &EventLoopProxy<UserEvent>,
    ) {
        let admitted = self.drag.as_ref().is_some_and(|drag| {
            self.bridge
                .lock()
                .admit_target(drag.press.seat, seat, target)
        });
        if admitted {
            self.wake(proxy);
        }
    }

    pub(super) fn shutdown(&mut self) {
        self.bridge.lock().disable();
        for (_, state) in self.pointers.drain() {
            if let Some(pointer) = state.pointer
                && pointer.version() >= 3
            {
                pointer.release();
            }
        }
        // Proxies do not send protocol destructors on Rust Drop. An unresolved
        // drag remains owned by the display until winit closes the connection.
        self.drag.take();
        if let Some(manager) = self.manager.take() {
            manager.destroy();
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, u32> for Listener {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        seat: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(pointer) = state.tab_drag.pointers.get_mut(seat) else {
            return;
        };
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                pointer.surface = Some(surface.id().as_ptr() as u64);
                pointer.point = [surface_x, surface_y];
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => pointer.point = [surface_x, surface_y],
            wl_pointer::Event::Leave { .. } => pointer.surface = None,
            wl_pointer::Event::Button {
                serial,
                button,
                state: button_state,
                ..
            } => {
                let mut shared = state.tab_drag.bridge.lock();
                let region = shared
                    .regions
                    .iter()
                    .find(|region| Some(region.ptr) == pointer.surface)
                    .cloned();
                let mut changed = shared.ordinary_button();
                if button == 0x110
                    && button_state.into_result().ok() == Some(wl_pointer::ButtonState::Pressed)
                    && let Some(region) = region
                {
                    changed |= shared
                        .presses
                        .press(serial, *seat, &region, pointer.point)
                        .is_some();
                }
                drop(shared);
                if changed {
                    state.tab_drag.wake(&state.proxy);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, u64> for Listener {
    fn event(
        state: &mut Self,
        _: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        identity: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(drag) = state
            .tab_drag
            .drag
            .as_mut()
            .filter(|drag| drag.press.identity == *identity)
        else {
            return;
        };
        if state.tab_drag.bridge.lock().rollback {
            drag.life.rollback();
        }
        let completion = match event {
            wl_data_source::Event::DndDropPerformed => drag.life.event(DragCompletion::Dropped),
            wl_data_source::Event::Cancelled => drag.life.event(DragCompletion::Cancelled),
            wl_data_source::Event::DndFinished => {
                drag.life.finished();
                None
            }
            wl_data_source::Event::Send { fd, .. } => {
                // The private MIME carries the bounded transaction identity.
                // Its payload is empty; EOF services every requested FD without
                // blocking, terminal text, or writes that could raise SIGPIPE.
                drop(fd);
                None
            }
            _ => None,
        };
        if drag.life.may_destroy_extension()
            && let Some(extension) = drag.extension.take()
        {
            extension.destroy();
        }
        let release_source = drag.life.may_destroy_source() && !drag.barrier.pending();
        if let Some(completion) = completion {
            let mut shared = state.tab_drag.bridge.lock();
            shared.ended = Some((*identity, completion));
            if let Some((completion, target)) = drag
                .barrier
                .complete(completion, shared.completion_target())
            {
                shared.ended_target = target;
                shared.completion = Some((*identity, completion));
            }
        }
        if release_source {
            drag.source.destroy();
            state.tab_drag.drag = None;
            let mut shared = state.tab_drag.bridge.lock();
            shared.active = None;
            shared.started = false;
            shared.reserved = None;
            shared.rollback = false;
        }
        state.tab_drag.wake(&state.proxy);
    }
}

delegate_noop!(Listener: ignore XdgToplevelDragManagerV1);
delegate_noop!(Listener: ignore XdgToplevelDragV1);

impl Dispatch<wl_callback::WlCallback, (u64, u64)> for Listener {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        identity: &(u64, u64),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(drag) =
            state.tab_drag.drag.as_mut().filter(|drag| {
                drag.press.identity == identity.0 && drag.barrier.matches(identity.1)
            })
        else {
            return;
        };
        let completed = drag
            .barrier
            .synchronize(identity.1)
            .expect("matched ordered callback");
        let mut shared = state.tab_drag.bridge.lock();
        // A released ordinary implicit grab followed by Sync means start_drag
        // did not take ownership. Keep its unresolved objects until a legal end
        // event or display close, but roll terminal custody back immediately.
        if shared.released && !drag.life.may_destroy_extension() {
            drag.life.rollback();
            shared.rollback = true;
            shared.ended = Some((identity.0, DragCompletion::Refused));
            shared.completion = Some((identity.0, DragCompletion::Refused));
        } else if identity.1 != 0 && !shared.rollback && shared.cancelled != Some(identity.0) {
            shared.attached = Some(identity.0);
        }
        if let Some((completion, target)) = completed {
            shared.ended_target = target;
            shared.completion = Some((identity.0, completion));
        }
        if drag.life.may_destroy_source() {
            drag.source.destroy();
            state.tab_drag.drag = None;
            shared.active = None;
            shared.started = false;
            shared.reserved = None;
            shared.rollback = false;
        }
        drop(shared);
        state.tab_drag.wake(&state.proxy);
    }
}

#[cfg(test)]
mod tests {
    use super::super::state::SurfaceIdent;
    use super::super::tab_drag_state::{DragRect, DragRegion, DragSlot};
    use super::*;

    #[test]
    fn wayland_tab_pointer_capability_loss_invalidates_press_without_ending_protocol() {
        let bridge = TabDragBridge::default();
        let region = DragRegion {
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
        };
        bridge.publish(vec![region.clone()]);
        let press = {
            let mut shared = bridge.lock();
            shared.available = true;
            shared
                .presses
                .press(123, 4, &region, [12.0, 8.0])
                .unwrap()
                .clone()
        };
        assert!(bridge.reserve(&press));
        bridge.lock().started = true;
        let mut protocol = TabProtocol::new(bridge.clone());
        protocol.seat_capability(4, true);
        protocol.seat_capability(4, false);
        assert!(!bridge.available());
        assert!(bridge.captured().is_none());
        assert_eq!(bridge.lock().active, Some(press.identity));
        assert_eq!(bridge.ended(press.identity), None);
        assert_eq!(
            bridge.take_completion(press.identity),
            Some((DragCompletion::Cancelled, None))
        );
        protocol.shutdown();
        assert!(!bridge.available());
        assert_eq!(bridge.ended(press.identity), None);
    }
}

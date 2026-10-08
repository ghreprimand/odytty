// SPDX-License-Identifier: GPL-3.0-only
//! Advertised Wayland transport, sharing the reversible terminal transaction.

use super::*;
use crate::native::wayland_file_drop::{
    DragCompletion, DragRect, DragRegion, DragSlot, TabDragBridge, WaylandSurfaceIdent, WirePress,
};

pub(in crate::native::app::multi_window_host) struct Docked {
    source: ProcessWindowId,
    press: WirePress,
    gesture: super::super::super::pointer::TopTabDrag,
}

impl MultiWindowHost {
    pub(super) fn wayland_docked_contains(&self, id: ProcessWindowId) -> bool {
        self.wayland_docked_drag
            .as_ref()
            .is_some_and(|dock| dock.source == id)
    }

    pub(super) fn route_wayland_docked_event(
        &mut self,
        index: usize,
        event: &WindowEvent,
    ) -> Option<bool> {
        if !self.wayland_docked_contains(self.windows[index].process_window_id()) {
            return None;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && self.live_tab_key(index, &key.logical_key, key.state)
        {
            return Some(true);
        }
        if ordinary_input(event)
            || matches!(
                event,
                WindowEvent::CloseRequested
                    | WindowEvent::Destroyed
                    | WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        ..
                    }
            )
        {
            self.cancel_live_tab();
            return Some(false);
        }
        Some(matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: WinitMouseButton::Left,
                    ..
                }
        ))
    }

    pub(in crate::native::app::multi_window_host) fn acknowledge_wayland_tab_wake(&self) {
        if let Some(bridge) = self.wayland_bridge() {
            bridge.acknowledge_wake();
        }
    }

    pub(super) fn wayland_bridge(&self) -> Option<TabDragBridge> {
        self.wayland_drop
            .as_ref()
            .map(|listener| listener.tab_drag.clone())
    }

    pub(in crate::native::app::multi_window_host) fn publish_wayland_tab_regions(&mut self) {
        let Some(bridge) = self.wayland_bridge() else {
            return;
        };
        let available = bridge.available();
        for app in &mut self.windows {
            app.wayland_live_available = available
                && app.is_wayland_client()
                && app.settings.live_tab_drag
                && !app.hyprland_live_requested();
        }
        let regions = self
            .windows
            .iter()
            .filter(|app| {
                app.wayland_live_available
                    && !app.live_drag_destination
                    && !app.live_drag_source
                    && !self.is_quick_window(app.process_window_id())
            })
            .filter_map(region_for)
            .take(65)
            .collect();
        bridge.publish(regions);
    }

    pub(super) fn wayland_press_for(&self, index: usize, token: SessionToken) -> Option<WirePress> {
        let app = &self.windows[index];
        let gesture = app.top_tab_drag?;
        let region = region_for(app)?;
        let press = self
            .wayland_docked_drag
            .as_ref()
            .filter(|dock| dock.source == app.process_window_id())
            .map(|dock| dock.press.clone())
            .or_else(|| self.wayland_bridge()?.captured())?;
        let scale = app.window.as_ref()?.scale_factor();
        let point = gesture.press_position();
        if press.token != token.0
            || press.region.ident != region.ident
            || (press.point[0] - point[0] / scale).abs() > 0.001
            || (press.point[1] - point[1] / scale).abs() > 0.001
        {
            return None;
        }
        Some(press)
    }

    pub(super) fn queue_wayland_tab_start(&self) -> bool {
        let Some(bridge) = self.wayland_bridge() else {
            return false;
        };
        let Some(drag) = self.live_drag.as_ref() else {
            return false;
        };
        let Some(press) = drag.wayland.clone() else {
            return false;
        };
        let Some(source) = self
            .index_of(drag.source)
            .and_then(|index| self.windows[index].window.clone())
        else {
            return false;
        };
        bridge.start(press, source)
    }

    pub(super) fn queue_wayland_tab_attach(&mut self) -> bool {
        let Some(bridge) = self.wayland_bridge() else {
            return false;
        };
        let Some(drag) = self.live_drag.as_ref() else {
            return false;
        };
        let Some(press) = drag.wayland.clone() else {
            return false;
        };
        if let Some(outcome) = bridge.ended(press.identity) {
            if outcome == DragCompletion::Dropped {
                if let Some(index) = self.index_of(drag.destination) {
                    self.windows[index].live_drag_map_blocked = false;
                }
                self.wayland_docked_drag = None;
                return true;
            }
            return outcome == DragCompletion::Refused;
        }
        let Some(source) = self
            .index_of(drag.source)
            .and_then(|index| self.windows[index].window.clone())
        else {
            return false;
        };
        let Some(index) = self.index_of(drag.destination) else {
            return false;
        };
        let app = &self.windows[index];
        let Some(destination) = app.window.clone() else {
            return false;
        };
        let Some(geometry) = app
            .resolved_cell()
            .and_then(|cell| app.top_strip_geom(cell))
        else {
            return false;
        };
        let Some(offset) =
            attachment_offset(drag.offset, geometry.band, destination.scale_factor())
        else {
            return false;
        };
        let queued = bridge.attach(press, source, destination, offset);
        if queued {
            self.wayland_docked_drag = None;
        }
        queued
    }

    pub(super) fn cancel_wayland_tab_protocol(&mut self) -> bool {
        let dock = self.wayland_docked_drag.take();
        let press = self
            .live_drag
            .as_ref()
            .and_then(|drag| drag.wayland.as_ref())
            .or_else(|| dock.as_ref().map(|dock| &dock.press));
        if let Some(press) = press
            && let Some(bridge) = self.wayland_bridge()
        {
            bridge.rollback(press.identity);
        }
        if let Some(dock) = dock {
            if let Some(index) = self.index_of(dock.source) {
                self.windows[index].cancel_top_tab_drag();
                self.windows[index].pointer_left_held = false;
            }
            true
        } else {
            false
        }
    }

    pub(in crate::native::app::multi_window_host) fn service_wayland_tab_drag(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        let Some(bridge) = self.wayland_bridge() else {
            return;
        };
        let press = self
            .live_drag
            .as_ref()
            .and_then(|drag| drag.wayland.clone())
            .or_else(|| {
                self.wayland_docked_drag
                    .as_ref()
                    .map(|dock| dock.press.clone())
            });
        let Some(press) = press else {
            // The independent observer and winit queues may arrive in either
            // order. Retry only the already armed, exact original host gesture.
            if let Some(captured) = bridge.captured()
                && let Some(index) = self.windows.iter().position(|app| {
                    app.process_window_id().0 == captured.region.ident.window
                        && app.surface_generation == captured.region.ident.generation
                })
                && self.windows[index].top_tab_drag.is_some_and(|gesture| {
                    gesture.tear_out && gesture.origin_token == Some(SessionToken(captured.token))
                })
            {
                let source = self.windows[index].process_window_id();
                self.begin_live_tab(source, SessionToken(captured.token), |app| {
                    app.try_resume_presentation(event_loop)
                        .map_err(|error| error.to_string())
                });
            }
            return;
        };
        if bridge.attached(press.identity)
            && let Some(destination) = self.live_drag.as_ref().map(|drag| drag.destination)
            && let Some(index) = self.index_of(destination)
            && self.windows[index].live_drag_map_blocked
        {
            self.windows[index].live_drag_map_blocked = false;
            self.windows[index].request_redraw_now();
        }
        let released = bridge.was_released(press.identity);
        if let Some((outcome, target)) = bridge.take_completion(press.identity) {
            match outcome {
                DragCompletion::Cancelled => {
                    self.cancel_live_tab();
                }
                DragCompletion::Refused => {
                    let source = self
                        .live_drag
                        .as_ref()
                        .map(|drag| (drag.source, released || drag.release_pending));
                    self.cancel_live_tab();
                    if let Some((source, true)) = source
                        && let Some(index) = self.index_of(source)
                    {
                        self.windows[index].request_tab_tear_out(SessionToken(press.token));
                    }
                }
                DragCompletion::Dropped => {
                    if self.wayland_docked_drag.is_some() {
                        self.finish_wayland_docked(target);
                    } else if self.live_drag.as_ref().is_some_and(|drag| {
                        self.target_in_source_strip(target, drag.source, drag.strip)
                    }) {
                        self.dock_wayland_tab(target);
                        self.finish_wayland_docked(target);
                    } else if self
                        .live_drag
                        .as_ref()
                        .and_then(|drag| self.index_of(drag.destination))
                        .is_some_and(|index| !self.windows[index].live_drag_map_blocked)
                    {
                        self.commit_live_tab();
                    } else {
                        self.cancel_live_tab();
                    }
                }
            }
            return;
        }
        if !bridge.available() {
            self.cancel_live_tab();
            return;
        }
        let target = bridge.target(press.identity);
        if self
            .live_drag
            .as_ref()
            .is_some_and(|drag| self.target_in_source_strip(target, drag.source, drag.strip))
        {
            self.dock_wayland_tab(target);
        } else if let Some(dock) = self.wayland_docked_drag.as_ref() {
            let source = dock.source;
            let mut gesture = dock.gesture;
            if let Some(index) = self.index_of(source) {
                if target.is_some_and(|target| {
                    target.window == source.0
                        && target.generation == self.windows[index].surface_generation
                }) {
                    if let Some(target) = target {
                        let scale = self.windows[index]
                            .window
                            .as_ref()
                            .map_or(1.0, |window| window.scale_factor());
                        gesture.set_press_position(press.point.map(|value| value * scale));
                        self.windows[index].pointer_left_held = true;
                        self.windows[index].top_tab_drag = Some(gesture);
                        self.windows[index]
                            .update_pointer_cell(target.point[0] * scale, target.point[1] * scale);
                        if let Some(gesture) = self.windows[index].top_tab_drag {
                            self.wayland_docked_drag
                                .as_mut()
                                .expect("docked custody")
                                .gesture = gesture;
                        }
                    }
                } else {
                    // During DnD a Leave is the compositor's authoritative
                    // outside-surface event; normal implicit-grab motion stops.
                    let scale = self.windows[index]
                        .window
                        .as_ref()
                        .map_or(1.0, |window| window.scale_factor());
                    gesture.set_press_position(press.point.map(|value| value * scale));
                    gesture.tear_out = true;
                    self.windows[index].top_tab_drag = Some(gesture);
                    self.begin_live_tab(source, SessionToken(press.token), |app| {
                        app.try_resume_presentation(event_loop)
                            .map_err(|error| error.to_string())
                    });
                }
            } else {
                self.cancel_live_tab();
            }
        }
    }

    fn target_in_source_strip(
        &self,
        target: Option<crate::native::wayland_file_drop::DragTarget>,
        source: ProcessWindowId,
        strip: PxRect,
    ) -> bool {
        let Some(index) = self.index_of(source) else {
            return false;
        };
        let app = &self.windows[index];
        let Some(target) = target.filter(|target| {
            target.window == source.0 && target.generation == app.surface_generation
        }) else {
            return false;
        };
        let scale = app
            .window
            .as_ref()
            .map_or(1.0, |window| window.scale_factor());
        DragRect([
            strip.x / scale,
            strip.y / scale,
            strip.width / scale,
            strip.height / scale,
        ])
        .contains(target.point)
    }

    fn dock_wayland_tab(&mut self, target: Option<crate::native::wayland_file_drop::DragTarget>) {
        let Some(drag) = self.live_drag.as_mut() else {
            return;
        };
        let Some(press) = drag.wayland.take() else {
            return;
        };
        let source = drag.source;
        let mut gesture = drag.gesture;
        gesture.tear_out = false;
        // Clear transport ownership only on the temporary terminal transaction:
        // protocol custody stays alive for a later undock or completion.
        self.cancel_live_tab();
        self.wayland_docked_drag = Some(Docked {
            source,
            press,
            gesture,
        });
        if let Some(index) = self.index_of(source) {
            let app = &mut self.windows[index];
            let scale = app
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor());
            let point = self
                .wayland_docked_drag
                .as_ref()
                .expect("docked custody")
                .press
                .point;
            gesture.set_press_position(point.map(|value| value * scale));
            self.wayland_docked_drag
                .as_mut()
                .expect("docked custody")
                .gesture = gesture;
            app.top_tab_drag = Some(gesture);
            app.pointer_left_held = true;
            app.pending_move = None;
            if let Some(target) = target {
                let scale = app
                    .window
                    .as_ref()
                    .map_or(1.0, |window| window.scale_factor());
                app.update_pointer_cell(target.point[0] * scale, target.point[1] * scale);
                if let Some(gesture) = app.top_tab_drag {
                    self.wayland_docked_drag
                        .as_mut()
                        .expect("docked custody")
                        .gesture = gesture;
                }
            }
        }
    }

    fn finish_wayland_docked(
        &mut self,
        target: Option<crate::native::wayland_file_drop::DragTarget>,
    ) {
        let Some(dock) = self.wayland_docked_drag.take() else {
            return;
        };
        if let Some(index) = self.index_of(dock.source) {
            let app = &mut self.windows[index];
            app.top_tab_drag = Some(dock.gesture);
            app.pointer_left_held = true;
            if let Some(target) = target.filter(|target| {
                target.window == dock.source.0 && target.generation == app.surface_generation
            }) {
                let scale = app
                    .window
                    .as_ref()
                    .map_or(1.0, |window| window.scale_factor());
                app.update_pointer_cell(target.point[0] * scale, target.point[1] * scale);
                app.pointer_left_held = false;
                app.finish_top_tab_drag();
            } else {
                app.pointer_left_held = false;
                app.cancel_top_tab_drag();
                app.request_tab_tear_out(SessionToken(dock.press.token));
            }
        }
    }
}

fn region_for(app: &App) -> Option<DragRegion> {
    let scale = app.window.as_ref()?.scale_factor();
    if !scale.is_finite()
        || !(0.25..=8.0).contains(&scale)
        || app.sessions.workspaces.is_empty()
        || app.overlay.is_open()
    {
        return None;
    }
    let geometry = app.top_strip_geom(app.resolved_cell()?)?;
    let rect = |rect: PxRect| {
        DragRect([
            rect.x / scale,
            rect.y / scale,
            rect.width / scale,
            rect.height / scale,
        ])
    };
    let slots = geometry
        .slots
        .iter()
        .filter_map(|slot| {
            Some(DragSlot {
                token: app.sessions.token_at_position(slot.idx)?.0,
                rect: rect(slot.rect),
                close: slot.close.map(rect),
            })
        })
        .collect();
    Some(DragRegion {
        ptr: app.wayland_surface_ptr()?,
        ident: WaylandSurfaceIdent {
            window: app.process_window_id().0,
            generation: app.surface_generation,
        },
        band: rect(geometry.band),
        slots,
        new_slot: geometry.new_slot.map(rect),
    })
}

// The grab is tab-local in logical pixels. Attach uses destination window
// geometry, so its physical strip origin is divided by the destination scale.
fn attachment_offset(offset: [f64; 2], band: PxRect, scale: f64) -> Option<[i32; 2]> {
    if !scale.is_finite() || !(0.25..=8.0).contains(&scale) {
        return None;
    }
    logical_offset([offset[0] + band.x / scale, offset[1] + band.y / scale])
}

fn logical_offset(offset: [f64; 2]) -> Option<[i32; 2]> {
    offset
        .into_iter()
        .all(|value| value.is_finite() && (0.0..=65536.0).contains(&value))
        .then(|| offset.map(|value| value.round() as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol_host(two_tabs: bool) -> (MultiWindowHost, SessionToken) {
        let (mut host, tab, _) = super::super::tests::armed(two_tabs);
        let source = host.windows[0].process_window_id();
        let gesture = host.windows[0].top_tab_drag.expect("held gesture");
        let generation = host.windows[0].surface_generation;
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        let strip = host.live_drag.as_ref().expect("custody").strip;
        host.live_drag.as_mut().expect("custody").wayland = Some(WirePress {
            identity: 1,
            serial: 123,
            seat: 4,
            region: DragRegion {
                ptr: 10,
                ident: WaylandSurfaceIdent {
                    window: source.0,
                    generation,
                },
                band: DragRect([strip.x, strip.y, strip.width, strip.height]),
                slots: Vec::new(),
                new_slot: None,
            },
            token: tab.0,
            point: gesture.press_position(),
        });
        host.windows[1].live_drag_map_blocked = true;
        (host, tab)
    }

    #[test]
    fn wayland_tab_raw_release_and_dnd_focus_do_not_commit_or_expire_custody() {
        let (mut host, tab) = protocol_host(false);
        assert!(host.route_live_tab_event(0, &WindowEvent::Focused(false)));
        host.expire_live_focus(Instant::now() + Duration::from_millis(200));
        assert!(host.live_drag.is_some());
        assert!(host.route_live_tab_event(
            0,
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: WinitMouseButton::Left,
            }
        ));
        assert!(
            host.live_drag
                .as_ref()
                .expect("protocol completion required")
                .release_pending
        );
        assert!(host.windows[0].sessions.workspaces.is_empty());
        assert!(host.windows[1].sessions.owns_session(tab));
        assert!(host.cancel_live_tab());
        assert!(!host.cancel_live_tab());
        assert!(host.windows[0].sessions.owns_session(tab));
    }

    #[test]
    fn wayland_tab_return_to_strip_keeps_logical_text_and_held_reorder() {
        let (mut host, tab) = protocol_host(true);
        let source = host.live_drag.as_ref().expect("custody").source;
        let terminal = Arc::clone(
            &host.windows[1]
                .sessions
                .get(tab)
                .expect("moved tab")
                .terminal,
        );
        let before = terminal.lock().unwrap().snapshot();
        let target = crate::native::wayland_file_drop::DragTarget {
            window: source.0,
            generation: host.windows[0].surface_generation,
            point: [20.0, 8.0],
        };
        host.dock_wayland_tab(Some(target));
        assert!(host.live_drag.is_none());
        assert!(host.wayland_docked_drag.is_some());
        assert!(host.windows[0].pointer_left_held);
        assert!(!host.windows[0].top_tab_drag.expect("held reorder").tear_out);
        assert!(host.windows[0].pending_move.is_none());
        assert!(Arc::ptr_eq(
            &terminal,
            &host.windows[0]
                .sessions
                .get(tab)
                .expect("restored tab")
                .terminal
        ));
        assert_eq!(terminal.lock().unwrap().snapshot(), before);
        assert!(host.cancel_live_tab());
        assert!(host.wayland_docked_drag.is_none());
        assert!(host.windows[0].top_tab_drag.is_none());
    }

    #[test]
    fn wayland_tab_docked_drop_finishes_reorder_once_at_the_protocol_target() {
        let (mut host, tab) = protocol_host(true);
        let source = host.live_drag.as_ref().expect("custody").source;
        let target = crate::native::wayland_file_drop::DragTarget {
            window: source.0,
            generation: host.windows[0].surface_generation,
            point: [20.0, 8.0],
        };
        host.dock_wayland_tab(Some(target));
        let before = host.windows[0].tab_tokens_for_test();
        let target = crate::native::wayland_file_drop::DragTarget {
            point: [500.0, 8.0],
            ..target
        };
        host.finish_wayland_docked(Some(target));
        let after = host.windows[0].tab_tokens_for_test();
        assert_eq!(after.last(), Some(&tab));
        assert_ne!(after, before);
        assert!(host.windows[0].pending_move.is_none());
        assert!(host.windows[0].top_tab_drag.is_none());
        host.finish_wayland_docked(Some(target));
        assert_eq!(host.windows[0].tab_tokens_for_test(), after);
    }

    #[test]
    fn wayland_tab_stale_incarnation_never_docks_the_provisional_window() {
        let (mut host, _) = protocol_host(false);
        let drag = host.live_drag.as_ref().expect("custody");
        let target = crate::native::wayland_file_drop::DragTarget {
            window: drag.source.0,
            generation: host.windows[0].surface_generation + 1,
            point: [20.0, 8.0],
        };
        assert!(!host.target_in_source_strip(Some(target), drag.source, drag.strip));
        assert!(host.live_drag.is_some());
        host.cancel_live_tab();
    }

    #[test]
    fn wayland_tab_attachment_preserves_destination_padding_and_rail_origin() {
        let band = PxRect {
            x: 96.0,
            y: 12.0,
            width: 800.0,
            height: 32.0,
        };
        // The grab offset has already been divided by the source scale.
        // Only the destination strip origin uses the destination scale.
        assert_eq!(attachment_offset([9.5, 4.25], band, 1.0), Some([106, 16]));
        assert_eq!(attachment_offset([9.5, 4.25], band, 2.0), Some([58, 10]));
        assert_eq!(attachment_offset([9.5, 4.25], band, 1.5), Some([74, 12]));
        let zero = PxRect {
            x: 0.0,
            y: 0.0,
            ..band
        };
        assert_eq!(attachment_offset([9.5, 4.25], zero, 2.0), Some([10, 4]));
    }

    #[test]
    fn wayland_tab_attachment_rejects_invalid_destination_geometry() {
        let band = PxRect {
            x: 96.0,
            y: 12.0,
            width: 800.0,
            height: 32.0,
        };
        for scale in [0.0, 0.24, 8.01, f64::NAN, f64::INFINITY] {
            assert_eq!(attachment_offset([1.0, 2.0], band, scale), None);
        }
        for x in [f64::NAN, f64::INFINITY, -100.0, 65536.0] {
            assert_eq!(
                attachment_offset([1.0, 2.0], PxRect { x, ..band }, 1.0),
                None
            );
        }
    }

    #[test]
    fn wayland_tab_logical_offsets_are_checked_before_protocol_requests() {
        assert_eq!(logical_offset([0.49, 0.51]), Some([0, 1]));
        assert_eq!(logical_offset([65536.0, 65536.0]), Some([65536, 65536]));
        for offset in [
            [f64::NAN, 0.0],
            [0.0, f64::INFINITY],
            [-0.01, 0.0],
            [65536.1, 0.0],
        ] {
            assert_eq!(logical_offset(offset), None);
        }
    }
    fn docked_host() -> MultiWindowHost {
        let (mut host, _) = protocol_host(true);
        let source = host.live_drag.as_ref().unwrap().source;
        let target = crate::native::wayland_file_drop::DragTarget {
            window: source.0,
            generation: host.windows[0].surface_generation,
            point: [20.0, 8.0],
        };
        host.dock_wayland_tab(Some(target));
        host
    }

    #[test]
    fn wayland_tab_docked_raw_release_waits_for_compositor_completion() {
        let mut host = docked_host();
        assert!(host.route_live_tab_event(
            0,
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state: ElementState::Released,
                button: WinitMouseButton::Left,
            }
        ));
        assert!(host.wayland_docked_drag.is_some());
        assert!(host.windows[0].top_tab_drag.is_some());
    }

    #[test]
    fn wayland_tab_docked_escape_cancels_through_decoded_host_input() {
        let mut host = docked_host();
        assert!(host.live_tab_key(0, &WinitKey::Named(NamedKey::Escape), ElementState::Pressed));
        assert!(host.wayland_docked_drag.is_none());
        assert!(host.windows[0].top_tab_drag.is_none());
    }

    #[test]
    fn wayland_tab_docked_source_retirement_cancels_before_arena_removal() {
        let mut host = docked_host();
        host.remove_closed_window(0);
        assert!(host.wayland_docked_drag.is_none());
    }
}

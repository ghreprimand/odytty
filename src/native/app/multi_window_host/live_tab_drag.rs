// SPDX-License-Identifier: GPL-3.0-only
//! Reversible tab custody. A provisional surface never owns autosave and never
//! resizes a live terminal. The source surface survives until commit, including
//! the sole-tab source, whose normal maintenance is suspended while empty.
use super::*;
use crate::native::app::chrome_geometry::{PxPoint, PxRect};
use crate::native::app::reparent::MovedHold;
use crate::native::session::{MoveScope, MovedRestore, WorkspaceSet};

const FOCUS_SETTLE_BOUND: Duration = Duration::from_millis(150);

const FOLLOW_INTERVAL: Duration = Duration::from_millis(16);

pub(super) struct ProvisionalTab {
    source: ProcessWindowId,
    destination: ProcessWindowId,
    restore: MovedRestore,
    hold: MovedHold,
    active: SessionToken,
    restore_focus: bool,
    strip: PxRect,
    gesture: super::super::pointer::TopTabDrag,
    source_scale: f64,
    strip_right_inset: f64,
    source_configured: bool,
    #[cfg(test)]
    source_size: Option<PhysicalSize<u32>>,
    /// Surface-logical grab offset survives a move between scale factors.
    offset: [f64; 2],
    point: Option<[i32; 2]>,
    next_move: Instant,
    last_placed: Option<[i32; 2]>,
    focus_deadline: Option<Instant>,
    release_pending: bool,
}

impl ProvisionalTab {
    pub(super) fn contains(&self, id: ProcessWindowId) -> bool {
        self.source == id || self.destination == id
    }
}

impl MultiWindowHost {
    pub(super) fn begin_live_tab(
        &mut self,
        origin: ProcessWindowId,
        tab: SessionToken,
        open: impl FnOnce(&mut App) -> Result<(), String>,
    ) -> bool {
        if self.live_drag.is_some() || self.is_quick_window(origin) {
            return false;
        }
        let Some(index) = self.index_of(origin) else {
            return false;
        };
        let source = &mut self.windows[index];
        if !source.settings.live_tab_drag || !source.sessions.owns_session(tab) {
            return false;
        }
        let Some(drag) = source
            .top_tab_drag
            .filter(|drag| drag.tear_out && drag.origin_token == Some(tab))
        else {
            return false;
        };
        let Some(strip) = source
            .resolved_cell()
            .and_then(|cell| source.top_strip_geom(cell))
            .map(|geom| geom.band)
        else {
            return false;
        };
        let Some(base) = crate::native::window_owner::next_window_token_base() else {
            return false;
        };
        let scale = source
            .window
            .as_ref()
            .map_or(1.0, |window| window.scale_factor());
        let point = pointer_global(source);
        if source.window.is_some() && point.is_none() {
            return false;
        }
        let strip_right_inset = source.resolved_surface().map_or(0.0, |(width, _, _)| {
            (f64::from(width) - strip.x - strip.width).max(0.0)
        });
        let active = source.sessions.active_id();
        source.sessions.switch(tab);
        let restore_focus = source.focused && source.active_tab_tokens().contains(&active);
        let moved = source.detach_for_move(MoveScope::ActiveTab);
        source.sessions.switch(active);
        let Ok((content, hold)) = moved else {
            return false;
        };
        let restore = content.restore_template();
        let set = WorkspaceSet::adopting(
            SessionToken(base),
            content,
            source.workspace_set(),
            source.workspace_set().event_proxy(),
        );
        let mut destination = (self.adopt)(set, Some(source.settings.clone()));
        destination.live_drag_destination = true;
        let id = destination.process_window_id();
        self.live_drag = Some(ProvisionalTab {
            source: origin,
            destination: id,
            restore,
            hold,
            active,
            restore_focus,
            strip,
            gesture: drag,
            source_scale: scale,
            strip_right_inset,
            source_configured: false,
            #[cfg(test)]
            source_size: None,
            offset: [drag.grab_offset_x / scale, (drag.press_y - strip.y) / scale],
            point,
            next_move: Instant::now(),
            last_placed: None,
            focus_deadline: None,
            release_pending: false,
        });
        self.windows[index].live_drag_source = true;
        self.windows.push(destination);
        let dest = self.windows.len() - 1;
        if let Err(error) = open(&mut self.windows[dest]) {
            tracing::warn!(%error, "provisional tab window could not open");
            self.cancel_live_tab();
            return false;
        }
        self.tick_live_tab(Instant::now());
        self.sync_sibling_counts();
        true
    }

    /// Taking the transaction is the exactly-once completion boundary. Duplicate
    /// release, cancellation and stale destination callbacks become no-ops.
    pub(in crate::native::app) fn cancel_live_tab(&mut self) -> bool {
        self.cancel_live_tab_with_focus(|source| {
            if !source.pending_exit
                && let Some(window) = source.window.as_ref()
            {
                window.focus_window();
            }
        })
    }

    fn cancel_live_tab_with_focus(&mut self, request_focus: impl FnOnce(&App)) -> bool {
        let return_focus = self.process_has_focus();
        let Some(drag) = self.live_drag.take() else {
            return false;
        };
        let Some(dest) = self.index_of(drag.destination) else {
            // All surface retirement routes must settle custody first.
            panic!("provisional destination removed before tab rollback");
        };
        let mut destination = self.windows.remove(dest);
        if destination.focused {
            destination.on_window_focus_changed(false);
        }
        let content = destination
            .workspace_set_mut()
            .release_adopted(drag.restore);
        destination.release_surface();
        let source = self
            .index_of(drag.source)
            .expect("provisional source retained until completion");
        let source = &mut self.windows[source];
        source.live_drag_source = false;
        source.sessions.restore_moved(content);
        source.sessions.switch(drag.active);
        source.adopt_moved_hold(drag.hold);
        #[cfg(test)]
        restore_source_fixture(source, drag.source_size);
        source.on_active_session_changed();
        if drag.source_configured {
            reconcile_source_surface(source);
        }
        if drag.restore_focus && source.focused {
            source.send_focus_report_to(drag.active, true);
        }
        source.request_redraw_now();
        if return_focus {
            request_focus(source);
        }
        self.sync_sibling_counts();
        true
    }

    fn commit_live_tab(&mut self) -> bool {
        self.commit_live_tab_with_focus(App::focus_quick_window)
    }

    fn commit_live_tab_with_focus(&mut self, request_focus: impl FnOnce(&App)) -> bool {
        let request_destination_focus = self.process_has_focus();
        // Flush the final pointer position, even inside the throttle interval.
        if let Some(drag) = self.live_drag.as_mut() {
            drag.next_move = Instant::now();
        }
        self.place_live_tab(Instant::now());
        let Some(drag) = self.live_drag.take() else {
            return false;
        };
        let source = self.index_of(drag.source).expect("retained source");
        let dest = self
            .index_of(drag.destination)
            .expect("retained destination");
        let (origin, destination) =
            two_mut(&mut self.windows, source, dest).expect("distinct windows");
        origin.live_drag_source = false;
        destination.live_drag_destination = false;
        destination.adopt_moved_hold(drag.hold);
        let tokens: Vec<_> = destination
            .sessions
            .iter()
            .map(|session| session.id)
            .collect();
        destination.arrive_moved_sessions(&tokens);
        if request_destination_focus {
            request_focus(destination);
        }
        let empty = origin.after_move_out(MoveScope::ActiveTab);
        if empty {
            destination.adopt_autosave_ownership_from(origin, Instant::now());
            let mut retired = self.windows.remove(source);
            retired.release_surface();
        } else {
            #[cfg(test)]
            restore_source_fixture(origin, drag.source_size);
            origin.on_active_session_changed();
            if drag.source_configured {
                reconcile_source_surface(origin);
            }
            origin.request_redraw_now();
        }
        self.sync_sibling_counts();
        true
    }

    pub(super) fn live_input_redirect(
        &self,
        index: usize,
        event: &WindowEvent,
    ) -> Option<ProcessWindowId> {
        let drag = self.live_drag.as_ref()?;
        (self.windows[index].process_window_id() == drag.destination && ordinary_input(event)
            && !matches!(event, WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed && key.logical_key == WinitKey::Named(NamedKey::Escape)))
            .then_some(drag.source)
    }

    pub(super) fn expire_live_focus(&mut self, now: Instant) {
        if !self.process_has_focus()
            && self
                .live_drag
                .as_ref()
                .and_then(|drag| drag.focus_deadline)
                .is_some_and(|deadline| now >= deadline)
        {
            self.cancel_live_tab();
        }
    }

    fn process_has_focus(&self) -> bool {
        self.windows.iter().any(|app| app.focused)
    }

    fn settle_live_focus(&mut self, now: Instant) {
        let Some(deadline) = self.live_drag.as_ref().and_then(|drag| drag.focus_deadline) else {
            return;
        };
        if self.process_has_focus() {
            let drag = self.live_drag.as_mut().expect("custody");
            drag.focus_deadline = None;
            if drag.release_pending {
                drag.release_pending = false;
                self.commit_live_tab();
            }
        } else if now >= deadline {
            self.cancel_live_tab();
        }
    }

    fn live_tab_key(&mut self, index: usize, key: &WinitKey, state: ElementState) -> bool {
        if self
            .live_drag
            .as_ref()
            .is_some_and(|drag| drag.contains(self.windows[index].process_window_id()))
            && state == ElementState::Pressed
            && *key == WinitKey::Named(NamedKey::Escape)
        {
            self.cancel_live_tab();
            true
        } else {
            false
        }
    }

    pub(super) fn route_live_tab_event(&mut self, index: usize, event: &WindowEvent) -> bool {
        let Some(drag) = self.live_drag.as_ref() else {
            return false;
        };
        let id = self.windows[index].process_window_id();
        if !drag.contains(id) {
            return false;
        }
        let source = id == drag.source;
        let source_id = drag.source;
        let destination_id = drag.destination;
        let strip = drag.strip;
        if let WindowEvent::ModifiersChanged(state) = event {
            for owner in [source_id, destination_id] {
                if let Some(index) = self.index_of(owner) {
                    if self.windows[index].sessions.workspaces.is_empty() {
                        self.windows[index].cache_modifiers(state.state());
                    } else {
                        self.windows[index].on_modifiers_changed(*state);
                    }
                }
            }
            return true;
        }
        if matches!(
            event,
            WindowEvent::Ime(winit::event::Ime::Enabled | winit::event::Ime::Disabled)
        ) || matches!(event, WindowEvent::Ime(winit::event::Ime::Preedit(text, _)) if text.is_empty())
        {
            if self.windows[index].sessions.workspaces.is_empty() {
                self.windows[index].ime_session = None;
                self.windows[index].ime_preedit.clear();
            } else if let WindowEvent::Ime(ime) = event {
                self.windows[index].handle_ime(ime.clone());
            }
            return true;
        }
        if matches!(event, WindowEvent::CloseRequested | WindowEvent::Destroyed) {
            if matches!(event, WindowEvent::Destroyed) {
                // A destroyed surface cannot count as process focus or receive
                // a focus-return request during rollback.
                if self.windows[index].sessions.workspaces.is_empty() {
                    self.windows[index].focused = false;
                } else {
                    self.windows[index].on_window_focus_changed(false);
                }
                if source {
                    self.windows[index].pending_exit = true;
                }
            }
            self.cancel_live_tab();
            if source && matches!(event, WindowEvent::Destroyed) {
                self.windows[index].pending_exit = true;
                return true;
            }
            return !source;
        }
        if let WindowEvent::Focused(focused) = event {
            if self.windows[index].sessions.workspaces.is_empty() {
                // A retained sole-tab source has no active-session alias.
                self.windows[index].focused = *focused;
            } else {
                self.windows[index].on_window_focus_changed(*focused);
            }
            if !focused {
                let drag = self.live_drag.as_mut().expect("custody");
                drag.focus_deadline
                    .get_or_insert(Instant::now() + FOCUS_SETTLE_BOUND);
            }
            return true;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            // Copy only the decoded identity; native event ownership stays at ingress.
            let key_identity = key.logical_key.clone();
            if self.live_tab_key(index, &key_identity, key.state) {
                return true;
            }
        }
        if ordinary_input(event) {
            self.cancel_live_tab();
            return !source;
        }
        if source
            && matches!(
                event,
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
            )
        {
            self.configure_live_source(index, event);
            return true;
        }
        if source {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    let point = PxPoint::new(position.x, position.y);
                    if point.x >= strip.x
                        && point.x < strip.x + strip.width
                        && point.y >= strip.y
                        && point.y < strip.y + strip.height
                    {
                        let mut gesture = self.live_drag.as_ref().expect("custody").gesture;
                        gesture.tear_out = false;
                        self.cancel_live_tab();
                        let source = &mut self.windows[index];
                        source.top_tab_drag = Some(gesture);
                        source.pointer_left_held = true;
                        source.pending_move = None;
                        source.update_pointer_cell(position.x, position.y);
                    } else {
                        self.windows[index].window_pointer_px = Some((position.x, position.y));
                        let point = pointer_global(&self.windows[index]);
                        self.live_drag.as_mut().expect("live").point = point;
                    }
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: WinitMouseButton::Left,
                    ..
                } => {
                    if self
                        .live_drag
                        .as_ref()
                        .is_some_and(|drag| drag.focus_deadline.is_some())
                        && !self.process_has_focus()
                    {
                        self.live_drag.as_mut().expect("custody").release_pending = true;
                    } else {
                        self.commit_live_tab();
                    }
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } => {
                    self.cancel_live_tab();
                }
                _ => {}
            }
            return true;
        }
        if matches!(
            event,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
        ) {
            self.live_drag.as_mut().expect("live").last_placed = None;
        }
        // A provisional destination paints/configures, but cannot mutate its
        // arena or receive terminal input before the drag has committed.
        !matches!(
            event,
            WindowEvent::RedrawRequested
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Occluded(_)
        )
    }

    /// Configure only the retained source surface. Its arena may be empty;
    /// terminal reconciliation waits until ownership has been restored/committed.
    fn configure_live_source(&mut self, index: usize, event: &WindowEvent) {
        let app = &mut self.windows[index];
        let (size, scale) = match event {
            WindowEvent::Resized(size) => (
                *size,
                self.live_drag.as_ref().expect("custody").source_scale,
            ),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => (
                app.window
                    .as_ref()
                    .map_or(PhysicalSize::new(0, 0), |window| window.inner_size()),
                *scale_factor,
            ),
            _ => return,
        };
        // Drop old configure work while the source is frozen. Completion reads
        // the current surface instead of replaying a stale debounce payload.
        app.resize_debounce = crate::native::resize::ResizeDebouncer::new(
            crate::native::resize::RESIZE_DEBOUNCE_INTERVAL,
        );
        app.window_minimized = size.width == 0 || size.height == 0;
        if !app.window_minimized {
            app.consecutive_skipped_frames = 0;
        }
        if let Some(gpu) = app.gpu.as_mut() {
            gpu.resize(size.width, size.height);
            if matches!(event, WindowEvent::ScaleFactorChanged { .. }) {
                gpu.set_scale(scale as f32);
            }
        }
        #[cfg(test)]
        if !app.sessions.workspaces.is_empty()
            && let Some((_, padding)) = app.test_surface
        {
            app.test_surface = Some(((size.width, size.height), padding));
        }
        let drag = self.live_drag.as_mut().expect("custody");
        let ratio = scale / drag.source_scale;
        if ratio.is_finite() && ratio > 0.0 {
            drag.strip.x *= ratio;
            drag.strip.y *= ratio;
            drag.strip.height *= ratio;
            drag.strip_right_inset *= ratio;
            drag.source_scale = scale;
        }
        drag.strip.width = (f64::from(size.width) - drag.strip.x - drag.strip_right_inset).max(0.0);
        drag.source_configured = true;
        #[cfg(test)]
        {
            drag.source_size = Some(size);
        }
        drag.point = pointer_global(app);
        drag.last_placed = None;
        drag.next_move = Instant::now();
        app.request_redraw_now();
    }

    pub(super) fn tick_live_tab(&mut self, now: Instant) {
        self.settle_live_focus(now);
        self.place_live_tab(now);
    }

    fn place_live_tab(&mut self, now: Instant) {
        let Some(drag) = self
            .live_drag
            .as_ref()
            .filter(|drag| now >= drag.next_move && drag.point != drag.last_placed)
        else {
            return;
        };
        let id = drag.destination;
        let point = drag.point;
        let offset = drag.offset;
        if let Some(index) = self.index_of(id)
            && let Some(window) = self.windows[index].window.as_ref()
            && let Some(point) = point
        {
            let offset = self.windows[index]
                .resolved_cell()
                .and_then(|cell| self.windows[index].top_strip_geom(cell))
                .map(|geometry| {
                    [
                        offset[0] + geometry.band.x / window.scale_factor(),
                        offset[1] + geometry.band.y / window.scale_factor(),
                    ]
                })
                .unwrap_or(offset);
            if super::super::tab_tear_out::follow_native(window, point, offset) {
                self.live_drag.as_mut().expect("live").last_placed = Some(point);
            }
        }
        let interval = self
            .index_of(id)
            .and_then(|index| self.windows[index].window.as_ref())
            .and_then(|window| window.current_monitor())
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .map(|rate| Duration::from_secs_f64(1000.0 / f64::from(rate.clamp(10_000, 240_000))))
            .unwrap_or(FOLLOW_INTERVAL);
        self.live_drag.as_mut().expect("live").next_move = now + interval;
    }

    pub(super) fn live_drag_wake(&self) -> Option<Instant> {
        let drag = self.live_drag.as_ref()?;
        drag.focus_deadline
            .into_iter()
            .chain((drag.point != drag.last_placed).then_some(drag.next_move))
            .min()
    }
}

fn key_changes_custody(key: &WinitKey, state: ElementState, synthetic: bool) -> bool {
    if *key == WinitKey::Named(NamedKey::Escape) && state == ElementState::Pressed {
        return true;
    }
    // Keep real non-modifier releases on the existing input route, including
    // negotiated terminal key-up reports. Focus-generated presses are metadata.
    !(synthetic && state == ElementState::Pressed)
        && !matches!(
            key,
            WinitKey::Named(
                NamedKey::Shift
                    | NamedKey::Control
                    | NamedKey::Alt
                    | NamedKey::Super
                    | NamedKey::Meta
                    | NamedKey::Hyper
                    | NamedKey::AltGraph
                    | NamedKey::CapsLock
                    | NamedKey::Fn
                    | NamedKey::FnLock
                    | NamedKey::NumLock
                    | NamedKey::ScrollLock
                    | NamedKey::Symbol
                    | NamedKey::SymbolLock
            )
        )
}

fn ordinary_input(event: &WindowEvent) -> bool {
    matches!(event, WindowEvent::KeyboardInput { event: key, is_synthetic, .. }
        if key_changes_custody(&key.logical_key, key.state, *is_synthetic))
        || matches!(event, WindowEvent::Ime(winit::event::Ime::Preedit(text, _)) if !text.is_empty())
        || matches!(
            event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::Ime(winit::event::Ime::Commit(_))
                | WindowEvent::Touch(_)
                | WindowEvent::DroppedFile(_)
                | WindowEvent::HoveredFile(_)
        )
}

#[cfg(test)]
fn restore_source_fixture(app: &mut App, size: Option<PhysicalSize<u32>>) {
    if let Some(size) = size
        && let Some((_, padding)) = app.test_surface
    {
        app.test_surface = Some(((size.width, size.height), padding));
    }
}

fn reconcile_source_surface(app: &mut App) {
    if let Some(cell) = app.resolved_cell()
        && let Some((width, height, padding)) = app.resolved_surface()
    {
        app.resize_grid_with_padding(cell, padding, width, height);
    }
}

fn pointer_global(app: &App) -> Option<[i32; 2]> {
    let window = app.window.as_ref()?;
    let origin = window.inner_position().ok()?;
    let (x, y) = app.window_pointer_px?;
    super::super::tab_tear_out::global_release([origin.x, origin.y], [x, y])
}

#[cfg(test)]
mod tests {
    use super::super::tests::{headless, host_of};
    use super::*;
    use crate::core::{Dimensions, Terminal};

    fn armed(two_tabs: bool) -> (MultiWindowHost, SessionToken, Arc<Mutex<Terminal>>) {
        let mut app = headless();
        app.settings.always_show_tab_bar = true;
        app.set_test_cell_for_test(crate::text::CellSize {
            width: 8,
            height: 16,
            baseline: 12,
        });
        app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
        if two_tabs {
            app.push_headless_session_for_test(
                Arc::new(Mutex::new(Terminal::new(80, 24))),
                crate::native::test_support::headless_writer(),
                Dimensions::new(80, 24),
            );
        }
        let token = app.sessions.active_id();
        let terminal = Arc::clone(&app.sessions.get(token).expect("session").terminal);
        let geometry = app
            .top_strip_geom(app.resolved_cell().expect("cell"))
            .expect("strip");
        app.set_pointer_px_for_test(geometry.slots[0].rect.x + 1.0, geometry.band.y + 1.0);
        app.mouse_left_press_for_test();
        app.pointer_move_for_test(-60.0, -80.0);
        let tab = app.top_tab_drag.expect("drag").origin_token.expect("token");
        let host = host_of(vec![app]);
        (host, tab, terminal)
    }

    #[test]
    fn live_tab_custody_keeps_a_sole_source_and_cancels_once_without_respawning() {
        let (mut host, tab, terminal) = armed(false);
        let source = host.windows[0].process_window_id();
        let before = terminal.lock().expect("terminal").snapshot();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        assert_eq!(host.windows.len(), 2);
        assert!(host.windows[0].sessions.workspaces.is_empty());
        assert!(host.windows[0].live_drag_source);
        assert!(Arc::ptr_eq(
            &terminal,
            &host.windows[1].sessions.get(tab).expect("moved").terminal
        ));
        host.refresh();
        assert!(host.cancel_live_tab());
        assert!(!host.cancel_live_tab());
        assert_eq!(host.windows.len(), 1);
        assert_eq!(host.windows[0].sessions.active_id(), tab);
        assert!(Arc::ptr_eq(
            &terminal,
            &host.windows[0]
                .sessions
                .get(tab)
                .expect("restored")
                .terminal
        ));
        assert_eq!(
            terminal.lock().expect("terminal").snapshot().dimensions,
            before.dimensions
        );
    }

    #[test]
    fn live_tab_creation_failure_rolls_back_and_sole_commit_retires_once() {
        for fail in [true, false] {
            let (mut host, tab, _) = armed(false);
            let source = host.windows[0].process_window_id();
            host.windows[0].set_primary_instance_for_test(true);
            let opened = host.begin_live_tab(source, tab, |_| {
                if fail {
                    Err("injected".to_owned())
                } else {
                    Ok(())
                }
            });
            assert_eq!(opened, !fail);
            if fail {
                assert_eq!(host.windows[0].process_window_id(), source);
                assert!(!host.windows[0].live_drag_source);
            } else {
                assert!(host.commit_live_tab());
                assert!(!host.commit_live_tab());
                assert_ne!(host.windows[0].process_window_id(), source);
            }
            assert_eq!(host.windows.len(), 1);
            assert!(host.windows[0].autosave_is_primary);
            assert!(host.windows[0].sessions.owns_session(tab));
        }
    }
    fn motion(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }
    fn release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: WinitMouseButton::Left,
        }
    }

    #[test]
    fn live_tab_callbacks_cancel_before_closing_or_returning_and_release_commits_once() {
        for event in [WindowEvent::CloseRequested, motion(20.0, 8.0)] {
            let (mut host, tab, _) = armed(false);
            let source = host.windows[0].process_window_id();
            assert!(host.begin_live_tab(source, tab, |_| Ok(())));
            host.route_live_tab_event(0, &event);
            assert_eq!(host.windows.len(), 1);
            assert!(host.windows[0].sessions.owns_session(tab));
            assert!(!host.route_live_tab_event(0, &release()));
        }
        let (mut host, tab, _) = armed(false);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        assert!(host.route_live_tab_event(1, &WindowEvent::CloseRequested));
        assert_eq!(host.windows.len(), 1);
        assert!(host.windows[0].sessions.owns_session(tab));
        // Re-arm through real pointer ingress after cancellation.
        host.windows[0].set_pointer_px_for_test(12.0, 8.0);
        host.windows[0].mouse_left_press_for_test();
        host.windows[0].pointer_move_for_test(-60.0, -80.0);
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        assert!(host.route_live_tab_event(0, &release()));
        assert!(!host.cancel_live_tab());
        assert_eq!(host.windows.len(), 1);
        assert_ne!(host.windows[0].process_window_id(), source);
    }

    #[test]
    fn live_tab_inactive_and_split_rollback_restore_order_focus_and_terminal_identity() {
        for split in [false, true] {
            let (mut host, _, _) = armed(true);
            let app = &mut host.windows[0];
            if split {
                app.cancel_chrome_drags();
                app.mouse_left_release_for_test();
                let tab = app.sessions.token_at_position(0).expect("tab");
                app.sessions.switch(tab);
                app.seed_headless_split_pane_for_test(
                    true,
                    Arc::new(Mutex::new(Terminal::new(80, 24))),
                    crate::native::test_support::headless_writer(),
                    Dimensions::new(80, 24),
                );
                app.set_test_cell_for_test(crate::text::CellSize {
                    width: 8,
                    height: 16,
                    baseline: 0,
                });
                app.set_test_surface_for_test(640, 384, WindowPadding::ZERO);
                let geometry = app
                    .top_strip_geom(app.resolved_cell().expect("cell"))
                    .expect("strip");
                app.set_pointer_px_for_test(geometry.slots[0].rect.x + 1.0, geometry.band.y + 1.0);
                app.mouse_left_press_for_test();
                app.pointer_move_for_test(-60.0, -80.0);
            }
            let target = app.top_tab_drag.expect("drag").origin_token.expect("tab");
            let before = app.tab_tokens_for_test();
            let active = app.sessions.active_id();
            let tree = format!("{:?}", app.sessions.active_layout());
            let terminals: Vec<_> = app
                .sessions
                .iter()
                .map(|session| (session.id, Arc::clone(&session.terminal)))
                .collect();
            let source = app.process_window_id();
            assert!(host.begin_live_tab(source, target, |_| Ok(())));
            host.refresh();
            assert!(host.cancel_live_tab());
            let app = &host.windows[0];
            assert_eq!(app.tab_tokens_for_test(), before);
            assert_eq!(app.sessions.active_id(), active);
            assert_eq!(format!("{:?}", app.sessions.active_layout()), tree);
            for (id, terminal) in terminals {
                assert!(Arc::ptr_eq(
                    &terminal,
                    &app.sessions.get(id).expect("restored pane").terminal
                ));
            }
        }
    }

    #[test]
    fn live_tab_provisional_surface_does_not_resize_or_reset_the_recorder() {
        let (mut host, tab, terminal) = armed(false);
        host.windows[0].sessions.set_recording_enabled(true);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |app| {
            assert!(app.sessions.get(tab).expect("pane").recorder.is_enabled());
            assert!(app.live_drag_destination);
            app.set_test_cell_for_test(crate::text::CellSize {
                width: 12,
                height: 24,
                baseline: 0,
            });
            app.set_test_surface_for_test(96, 48, WindowPadding::ZERO);
            app.resize_grid_with_padding(
                crate::text::CellSize {
                    width: 12,
                    height: 24,
                    baseline: 0,
                },
                WindowPadding::ZERO,
                96,
                48,
            );
            Ok(())
        }));
        assert_eq!(
            terminal.lock().expect("terminal").snapshot().dimensions,
            Dimensions::new(80, 24)
        );
        host.cancel_live_tab();
        assert!(
            host.windows[0]
                .sessions
                .get(tab)
                .expect("restored")
                .recorder
                .is_enabled()
        );
    }
    #[test]
    fn live_tab_real_pointer_custody_forces_the_destination_strip_until_commit() {
        let (mut host, tab, _) = armed(true);
        host.windows[0].settings.always_show_tab_bar = false;
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        assert!(host.windows[1].should_show_tab_bar());
        assert!(host.windows[1].live_drag_destination);
        assert!(host.commit_live_tab());
        let destination = host
            .windows
            .iter()
            .find(|app| app.process_window_id() != source)
            .expect("destination");
        assert!(!destination.should_show_tab_bar());
        assert!(!destination.live_drag_destination);
    }
    #[test]
    fn live_tab_source_input_restores_custody_before_ordinary_ime_routing() {
        let (mut host, tab, _) = armed(false);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        assert!(!host.route_live_tab_event(
            0,
            &WindowEvent::Ime(winit::event::Ime::Commit("x".to_owned()))
        ));
        assert!(host.windows[0].sessions.owns_session(tab));
        assert_eq!(host.windows.len(), 1);
        assert_eq!(host.windows[0].process_window_id(), source);
    }
    #[test]
    fn live_tab_focus_loss_then_provisional_gain_in_same_batch_continues() {
        let (mut host, tab, _) = armed(false);
        host.windows[0].on_window_focus_changed_for_test(true);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.route_live_tab_event(0, &WindowEvent::Focused(false));
        assert!(
            host.live_drag.is_some(),
            "focus classification waits for the batch"
        );
        host.route_live_tab_event(1, &WindowEvent::Focused(true));
        host.tick_live_tab(Instant::now());
        assert!(host.live_drag.is_some());
    }

    #[test]
    fn live_tab_focus_gain_in_later_batch_within_bound_continues() {
        let (mut host, tab, _) = armed(false);
        host.windows[0].on_window_focus_changed_for_test(true);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        let now = Instant::now();
        host.route_live_tab_event(0, &WindowEvent::Focused(false));
        assert!(
            host.live_drag.is_some(),
            "a delayed own-process gain is allowed"
        );
        host.tick_live_tab(now + Duration::from_millis(40));
        assert!(host.live_drag.is_some());
        host.route_live_tab_event(1, &WindowEvent::Focused(true));
        host.tick_live_tab(now + Duration::from_millis(80));
        assert!(host.live_drag.is_some());
    }

    #[test]
    fn live_tab_focus_gain_never_arriving_cancels_at_bound() {
        let (mut host, tab, _) = armed(false);
        host.windows[0].on_window_focus_changed_for_test(true);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.route_live_tab_event(0, &WindowEvent::Focused(false));
        assert!(
            host.live_drag.is_some(),
            "loss does not cancel before classification"
        );
        host.tick_live_tab(Instant::now() + Duration::from_millis(200));
        assert!(host.live_drag.is_none());
        assert_eq!(host.windows.len(), 1);
        assert!(host.windows[0].sessions.owns_session(tab));
        assert!(!host.windows[0].focused);
    }

    #[test]
    fn live_tab_focus_leaving_process_after_provisional_gain_cancels() {
        let (mut host, tab, _) = armed(false);
        host.windows[0].on_window_focus_changed_for_test(true);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.route_live_tab_event(0, &WindowEvent::Focused(false));
        assert!(
            host.live_drag.is_some(),
            "preview activation must survive source loss"
        );
        host.route_live_tab_event(1, &WindowEvent::Focused(true));
        host.tick_live_tab(Instant::now());
        host.route_live_tab_event(1, &WindowEvent::Focused(false));
        host.tick_live_tab(Instant::now() + Duration::from_millis(200));
        assert!(host.live_drag.is_none());
        assert!(!host.windows[0].focused);
    }

    #[test]
    fn live_tab_escape_at_provisional_cancels_and_input_redirects_to_source() {
        let (mut host, tab, _) = armed(false);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        assert!(host.live_tab_key(1, &WinitKey::Named(NamedKey::Escape), ElementState::Pressed));
        assert_eq!(host.windows.len(), 1);
        assert!(host.windows[0].sessions.owns_session(tab));
        host.windows[0].set_pointer_px_for_test(12.0, 8.0);
        host.windows[0].mouse_left_press_for_test();
        host.windows[0].pointer_move_for_test(-60.0, -80.0);
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        let input = WindowEvent::Ime(winit::event::Ime::Commit("x".to_owned()));
        assert_eq!(host.live_input_redirect(1, &input), Some(source));
        assert!(host.route_live_tab_event(1, &input));
        assert!(host.live_drag.is_none());
        assert_eq!(host.windows[0].process_window_id(), source);
    }

    #[test]
    fn live_tab_release_during_focus_gap_waits_instead_of_taking_external_focus() {
        for gain in [false, true] {
            let (mut host, tab, _) = armed(false);
            host.windows[0].on_window_focus_changed_for_test(true);
            let source = host.windows[0].process_window_id();
            assert!(host.begin_live_tab(source, tab, |_| Ok(())));
            host.route_live_tab_event(0, &WindowEvent::Focused(false));
            host.route_live_tab_event(0, &release());
            assert!(host.live_drag.is_some());
            if gain {
                host.route_live_tab_event(1, &WindowEvent::Focused(true));
            }
            host.tick_live_tab(Instant::now() + Duration::from_millis(200));
            assert!(host.live_drag.is_none());
            assert_eq!(host.windows.len(), 1);
            assert_eq!(host.windows[0].process_window_id() == source, !gain);
        }
    }
    #[test]
    fn live_tab_cancel_returns_focus_only_when_the_process_held_focus() {
        for focused in [false, true] {
            let (mut host, tab, _) = armed(false);
            host.windows[0].focused = false;
            let source = host.windows[0].process_window_id();
            assert!(host.begin_live_tab(source, tab, |_| Ok(())));
            host.route_live_tab_event(1, &WindowEvent::Focused(focused));
            let mut requested = false;
            assert!(host.cancel_live_tab_with_focus(|app| {
                assert_eq!(app.process_window_id(), source);
                assert!(app.sessions.owns_session(tab));
                requested = true;
            }));
            assert_eq!(requested, focused);
        }
    }

    #[test]
    fn live_tab_other_process_window_focus_continues_and_late_gain_cannot_reopen() {
        let (mut host, tab, _) = armed(false);
        host.windows[0].on_window_focus_changed_for_test(true);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.route_live_tab_event(0, &WindowEvent::Focused(false));
        let mut third = headless();
        third
            .workspace_set_mut()
            .rekey_sole_session_for_test(SessionToken(1 << 60));
        third.on_window_focus_changed_for_test(true);
        host.windows.push(third);
        host.tick_live_tab(Instant::now());
        assert!(host.live_drag.is_some());
        host.windows[2].on_window_focus_changed_for_test(false);
        host.route_live_tab_event(1, &WindowEvent::Focused(false));
        host.expire_live_focus(Instant::now() + Duration::from_millis(200));
        assert!(host.live_drag.is_none());
        assert!(host.windows[0].sessions.owns_session(tab));
    }
    #[test]
    fn live_tab_destroyed_preview_does_not_leave_a_focused_restored_source() {
        let (mut host, tab, _) = armed(false);
        host.windows[0].on_window_focus_changed_for_test(true);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.route_live_tab_event(0, &WindowEvent::Focused(false));
        host.route_live_tab_event(1, &WindowEvent::Focused(true));
        assert!(host.route_live_tab_event(1, &WindowEvent::Destroyed));
        assert!(host.live_drag.is_none());
        assert!(!host.process_has_focus());
        assert_eq!(host.windows.len(), 1);
        assert_eq!(host.windows[0].process_window_id(), source);
    }
    /// Returning to the strip must leave the reorder gesture exactly as
    /// the release-time path does (`returning_to_the_strip_keeps_reorder...`).
    #[test]
    fn review_live_tab_return_to_strip_keeps_the_reorder_gesture() {
        let (mut host, tab, _) = armed(true);
        let tokens = host.windows[0].tab_tokens_for_test();
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.route_live_tab_event(0, &motion(20.0, 8.0));
        assert!(host.live_drag.is_none(), "return to strip cancels custody");
        assert_eq!(host.windows[0].tab_tokens_for_test(), tokens);
        assert!(
            host.windows[0].top_tab_drag.is_some(),
            "the held tab is still being dragged for reorder"
        );
        assert!(!host.windows[0].top_tab_drag.expect("reorder").tear_out);
        assert!(host.windows[0].pending_move.is_none());
        host.windows[0].pointer_move_for_test(-60.0, -80.0);
        assert!(matches!(host.windows[0].pending_move,
            Some(super::super::super::reparent::MoveRequest::LiveTab(token)) if token == tab));
    }

    /// Same check with the live setting off, through the real pointer
    /// path, as the reference behavior.
    #[test]
    fn review_reference_off_path_keeps_the_gesture_after_returning() {
        let (mut host, _, _) = armed(true);
        host.windows[0].settings.live_tab_drag = false;
        host.windows[0].pointer_move_for_test(20.0, 8.0);
        assert!(host.windows[0].top_tab_drag.is_some());
    }

    #[test]
    fn live_tab_source_resize_keeps_custody_and_updates_return_bounds() {
        for two_tabs in [false, true] {
            let (mut host, tab, terminal) = armed(two_tabs);
            let source = host.windows[0].process_window_id();
            let before = terminal.lock().expect("terminal").snapshot().dimensions;
            assert!(host.begin_live_tab(source, tab, |_| Ok(())));
            assert!(
                host.route_live_tab_event(0, &WindowEvent::Resized(PhysicalSize::new(800, 600)))
            );
            assert!(
                host.live_drag.is_some(),
                "source configure must keep custody"
            );
            assert_eq!(
                terminal.lock().expect("terminal").snapshot().dimensions,
                before
            );
            host.route_live_tab_event(0, &motion(700.0, 8.0));
            assert!(host.live_drag.is_none(), "return uses the enlarged strip");
            assert!(host.windows[0].top_tab_drag.is_some());
        }
    }

    #[test]
    fn live_tab_automation_poll_keeps_custody_and_mutation_restores_first() {
        use crate::automation::protocol::{Action, ObjectId, ObjectKind, Request, VERSION};
        let (mut host, tab, _) = armed(false);
        host.windows[0].settings.automation_endpoint = true;
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.dispatch_automation();
        assert!(host.live_drag.is_some(), "an empty poll must not cancel");
        let instance = [0x42; 16];
        let reply = host.apply_automation_request(
            instance,
            Request {
                version: VERSION,
                request_id: 1,
                action: Action::Rename {
                    target: ObjectId {
                        instance,
                        kind: ObjectKind::Tab,
                        serial: tab.0,
                    },
                    name: "restored".to_owned(),
                },
            },
        );
        assert!(
            host.live_drag.is_none(),
            "mutation restores custody before routing"
        );
        assert_eq!(host.windows.len(), 1);
        assert!(matches!(
            reply,
            crate::automation::protocol::Reply::Applied(_)
        ));
    }

    #[test]
    fn live_tab_off_transitions_keep_the_plain_assignment_branch() {
        for transition in 0..3 {
            let (mut host, _, _) = armed(true);
            let (mut reference, _, _) = armed(true);
            reference.windows[0].top_tab_drag = None;
            for candidate in [&mut host, &mut reference] {
                let app = &mut candidate.windows[0];
                app.settings.live_tab_drag = false;
                app.cursor_icon = CursorIcon::Grabbing;
                app.presentation_epoch = 0;
                match transition {
                    0 => app.reset_pointer_state_for_overlay(),
                    1 => app.on_window_focus_changed(false),
                    _ => app.on_active_session_changed(),
                }
                assert!(app.top_tab_drag.is_none());
            }
            assert_eq!(
                host.windows[0].cursor_icon, reference.windows[0].cursor_icon,
                "old branch does not reset the drag cursor"
            );
            assert_eq!(
                host.windows[0].presentation_epoch, reference.windows[0].presentation_epoch,
                "old branch adds no drag-specific repaint"
            );
        }
    }

    #[test]
    fn live_tab_commit_requests_focus_only_while_the_process_has_focus() {
        for focused in [false, true] {
            let (mut host, tab, _) = armed(false);
            let source = host.windows[0].process_window_id();
            assert!(host.begin_live_tab(source, tab, |_| Ok(())));
            host.windows[0].focused = false;
            host.windows[1].focused = focused;
            let mut requests = 0;
            assert!(host.commit_live_tab_with_focus(|_| requests += 1));
            assert_eq!(requests, usize::from(focused));
            assert!(host.live_drag.is_none());
        }
    }

    #[test]
    fn live_tab_key_classification_ignores_modifiers_and_synthetic_presses() {
        for key in [
            NamedKey::Shift,
            NamedKey::Control,
            NamedKey::Alt,
            NamedKey::Super,
            NamedKey::Meta,
            NamedKey::Hyper,
            NamedKey::AltGraph,
            NamedKey::CapsLock,
            NamedKey::Fn,
            NamedKey::FnLock,
            NamedKey::NumLock,
            NamedKey::ScrollLock,
            NamedKey::Symbol,
            NamedKey::SymbolLock,
        ] {
            assert!(!key_changes_custody(
                &WinitKey::Named(key),
                ElementState::Pressed,
                false
            ));
        }
        let real = WinitKey::Character("x".into());
        assert!(!key_changes_custody(&real, ElementState::Pressed, true));
        assert!(key_changes_custody(&real, ElementState::Released, false));
        assert!(key_changes_custody(&real, ElementState::Pressed, false));
        for synthetic in [false, true] {
            assert!(key_changes_custody(
                &WinitKey::Named(NamedKey::Escape),
                ElementState::Pressed,
                synthetic
            ));
        }
    }

    #[test]
    fn live_tab_input_setup_notifications_do_not_cancel_custody() {
        let (mut host, tab, _) = armed(false);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        for index in [0, 1] {
            for event in [
                WindowEvent::Ime(winit::event::Ime::Enabled),
                WindowEvent::Ime(winit::event::Ime::Disabled),
                WindowEvent::Ime(winit::event::Ime::Preedit(String::new(), None)),
                WindowEvent::ModifiersChanged(winit::event::Modifiers::default()),
            ] {
                host.route_live_tab_event(index, &event);
                assert!(
                    host.live_drag.is_some(),
                    "input setup is not terminal input"
                );
                assert_eq!(host.windows.len(), 2);
            }
        }
        let modifiers =
            WindowEvent::ModifiersChanged(winit::keyboard::ModifiersState::CONTROL.into());
        host.route_live_tab_event(1, &modifiers);
        assert!(host.windows[0].modifiers.ctrl);
        assert!(host.windows[1].modifiers.ctrl);
        host.cancel_live_tab();
    }
}

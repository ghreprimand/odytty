// SPDX-License-Identifier: GPL-3.0-only
//! Thin `winit` `ApplicationHandler` forwarding for the native app.
//!
//! The event ingress stays in one place: every arm forwards to the handler that
//! owns the responsibility, and the trailing pending-exit check runs after the
//! window-event match exactly as before.
//!
//! The window-event match body lives in [`App::process_window_event`] so the
//! single-window `ApplicationHandler` impl here and the multi-window
//! [`crate::native::app::multi_window_host::MultiWindowHost`] dispatch the exact
//! same arms to the exact same handlers - the two run paths cannot drift on
//! which event reaches which method. The single-window impl treats a confirmed
//! close as the process exit; the host routes it through
//! [`crate::native::window_owner::resolve_window_close`] so a sibling close
//! removes only that window.

use super::*;

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.on_resumed(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let redraw_early_exit = self.process_window_event(event_loop, event);
        // CLOSE-CONFIRM: an overlay outcome dispatched during this event (the
        // confirmation dialog's Enter/Y) may have requested the window close.
        // The overlay apply path only holds `&mut self`, so it sets this flag
        // and the actual exit happens here where the event loop is in scope.
        // The redraw early-exit paths left `window_event` before this check
        // historically, so honor that by skipping it when the redraw took one.
        if !redraw_early_exit && self.pending_exit {
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        if self.apply_user_event(event) {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        self.run_about_to_wait_maintenance(now);

        if let Some(deadline) = self.deadline
            && now >= deadline
        {
            event_loop.exit();
            return;
        }

        self.update_control_flow_deadline(event_loop);
    }
}

impl App {
    /// Dispatch a single `WindowEvent` to the handler that owns it, WITHOUT the
    /// trailing `pending_exit`/exit decision (which differs between the
    /// single-window and multi-window run paths). Shared verbatim by the
    /// single-window [`ApplicationHandler`] impl above and the multi-window
    /// host, so the two paths cannot drift on which event reaches which handler.
    ///
    /// Returns `true` when the redraw path took one of its early exits - the
    /// caller must NOT run its trailing close check on that path, exactly as the
    /// single-window match returned early from `window_event` before.
    pub(super) fn process_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) -> bool {
        self.reconcile_displaced_pending_paste();
        match event {
            WindowEvent::CloseRequested => {
                self.on_close_requested(event_loop);
            }
            WindowEvent::ThemeChanged(os_theme) => {
                self.on_os_theme_changed(os_theme);
            }
            WindowEvent::Resized(size) => {
                self.on_window_resized(size, event_loop);
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                inner_size_writer,
            } => {
                self.on_scale_factor_changed(scale_factor, inner_size_writer, event_loop);
            }
            WindowEvent::RedrawRequested => {
                // The redraw path has two early exits that left this handler
                // before the trailing pending-exit check; preserve that by
                // returning `true` here on exactly those paths.
                if self.on_redraw_requested() {
                    return true;
                }
            }
            // `winit` reports modifier state separately from key presses; cache
            // it so the next `KeyboardInput` encodes with Ctrl/Alt/Shift held.
            WindowEvent::ModifiersChanged(state) => {
                self.on_modifiers_changed(state);
            }
            WindowEvent::Focused(focused) => {
                self.on_window_focus_changed(focused);
            }
            // BLACK-SCREEN-ON-RESTORE: a Windows restore can surface as
            // `Occluded(false)` without a non-zero `Resized`; recover the paint
            // there. Only the un-occlude direction is handled (see the method
            // doc) — occlusion is not treated as minimize.
            WindowEvent::Occluded(occluded) => {
                let _ = self.on_window_occluded(occluded);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.update_pointer_cell(position.x, position.y);
            }
            WindowEvent::CursorLeft { .. } => {
                self.on_cursor_left();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(state, button);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.on_keyboard_input(event);
            }
            WindowEvent::Ime(ime) => {
                key_event_diagnostics::log_ime_event(&ime);
                self.handle_ime(ime);
            }
            // Hover is intentionally inert: no path is previewed or trusted
            // before the OS reports a completed drop. HoveredFileCancelled is
            // also not a drop-transaction end: winit emits it when a drag leaves
            // without dropping, not after DroppedFile, so it cannot clear an
            // over-cap latch. Overflow stays refused until cancel, focus-loss,
            // or a fresh Wayland uri-list.
            WindowEvent::HoveredFile(_) | WindowEvent::HoveredFileCancelled => {}
            WindowEvent::DroppedFile(path) => self.queue_file_drop(path),
            _ => {}
        }
        false
    }
}

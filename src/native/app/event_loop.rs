// SPDX-License-Identifier: GPL-3.0-only
//! Thin `winit` `ApplicationHandler` forwarding for the native app.
//!
//! The event ingress stays in one place: every arm forwards to the handler that
//! owns the responsibility, and the trailing pending-exit check runs after the
//! window-event match exactly as before.

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
                // returning here on exactly those paths.
                if self.on_redraw_requested() {
                    return;
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
            _ => {}
        }
        // CLOSE-CONFIRM: an overlay outcome dispatched during this event (the
        // confirmation dialog's Enter/Y) may have requested the window close.
        // The overlay apply path only holds `&mut self`, so it sets this flag
        // and the actual exit happens here where the event loop is in scope.
        // Stays `false` on every path that does not confirm a close, so the
        // off/default behavior is unchanged.
        if self.pending_exit {
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

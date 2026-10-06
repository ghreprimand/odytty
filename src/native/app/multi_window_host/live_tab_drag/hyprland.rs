// SPDX-License-Identifier: GPL-3.0-only
//! Hyprland transport around the shared provisional ownership transaction.
use super::*;
use crate::native::app::hyprland_tear_out::{capture_destination, follow::Geometry};

impl MultiWindowHost {
    pub(super) fn start_hyprland_release(&mut self) -> bool {
        let Some(drag) = self.live_drag.as_ref() else {
            return false;
        };
        if drag.hyprland.is_none() {
            return false;
        }
        // Capture now, not when the worker eventually drains its mailbox.
        let target = capture_destination();
        let drag = self.live_drag.as_mut().expect("custody");
        drag.release_pending = true;
        drag.hyprland.as_mut().expect("transport").release(target);
        true
    }

    pub(super) fn send_hyprland_frame(&mut self, now: Instant) -> bool {
        let Some(drag) = self.live_drag.as_ref() else {
            return false;
        };
        let Some(follow) = drag.hyprland.as_ref() else {
            return false;
        };
        if !drag.follow_dirty || now < drag.next_move || follow.released || follow.failed() {
            return true;
        }
        let Some(index) = self.index_of(drag.destination) else {
            return true;
        };
        let app = &self.windows[index];
        let Some(window) = app.window.as_ref() else {
            return true;
        };
        let scale = window.scale_factor();
        let size = window.inner_size();
        let mut offset = drag.offset;
        if let Some(geometry) = app
            .resolved_cell()
            .and_then(|cell| app.top_strip_geom(cell))
        {
            offset[0] += geometry.band.x / scale;
            offset[1] += geometry.band.y / scale;
        }
        follow.frame(Geometry {
            offset,
            size: [
                f64::from(size.width) / scale,
                f64::from(size.height) / scale,
            ],
        });
        let interval = window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .map(|rate| Duration::from_secs_f64(1000.0 / f64::from(rate.clamp(10_000, 240_000))))
            .unwrap_or(FOLLOW_INTERVAL);
        let drag = self.live_drag.as_mut().expect("custody");
        drag.follow_dirty = false;
        drag.next_move = now + interval;
        true
    }

    /// IPC failure may occur after floating took effect but before its reply.
    /// Retire that exact surface rather than relying on broken IPC to retile it.
    pub(in crate::native::app) fn service_hyprland_follow(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_hyprland_follow(|app| {
            app.try_resume_presentation(event_loop)
                .map_err(|error| error.to_string())
        });
    }

    fn poll_hyprland_follow(&mut self, open: impl FnOnce(&mut App) -> Result<(), String>) {
        let Some(drag) = self.live_drag.as_ref() else {
            return;
        };
        let Some(follow) = drag.hyprland.as_ref() else {
            return;
        };
        let id = drag.destination;
        if follow.failed() && !follow.retired {
            follow.stop();
            let Some(index) = self.index_of(id) else {
                return;
            };
            let destination = &mut self.windows[index];
            if destination.focused {
                destination.on_window_focus_changed(false);
            }
            destination.release_surface();
            destination.pending_hyprland_follow_title = None;
            let drag = self.live_drag.as_mut().expect("custody");
            drag.hyprland.as_mut().expect("transport").retired = true;
            drag.focus_deadline
                .get_or_insert(Instant::now() + FOCUS_SETTLE_BOUND);
        }
        let Some(drag) = self.live_drag.as_ref() else {
            return;
        };
        let follow = drag.hyprland.as_ref().expect("transport");
        if follow.released
            && follow.retired
            && !follow.replacement_ready
            && (drag.focus_deadline.is_none() || self.process_has_focus())
        {
            let target = follow.release_target;
            let Some(index) = self.index_of(id) else {
                return;
            };
            let destination = &mut self.windows[index];
            destination.prepare_tear_out_placement(target);
            if let Err(error) = open(destination) {
                tracing::warn!(%error, "release-time tab replacement could not open");
                self.cancel_live_tab();
                return;
            }
            destination.start_tear_out_placement();
            if target.is_none() {
                destination.finish_tear_out_placement(false);
            }
            self.live_drag
                .as_mut()
                .expect("custody")
                .hyprland
                .as_mut()
                .expect("transport")
                .replacement_ready = true;
        }
        let Some(drag) = self.live_drag.as_ref() else {
            return;
        };
        if drag
            .hyprland
            .as_ref()
            .is_some_and(|follow| follow.released && follow.ready())
            && (drag.focus_deadline.is_none() || self.process_has_focus())
        {
            if let Some(index) = self.index_of(drag.destination) {
                self.windows[index].pending_hyprland_follow_title = None;
                self.windows[index].sync_active_window_title();
            }
            self.commit_live_tab();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::armed;
    use super::*;
    use crate::native::app::hyprland_tear_out::follow::Follow;
    fn host() -> (MultiWindowHost, SessionToken) {
        let (mut host, tab, _) = armed(false);
        let source = host.windows[0].process_window_id();
        assert!(host.begin_live_tab(source, tab, |_| Ok(())));
        host.live_drag.as_mut().expect("custody").hyprland = Some(Follow::fake());
        (host, tab)
    }
    #[test]
    fn hyprland_follow_failure_retires_preview_before_release_time_replacement() {
        let (mut host, tab) = host();
        let source = host.windows[0].process_window_id();
        host.live_drag
            .as_ref()
            .expect("custody")
            .hyprland
            .as_ref()
            .expect("transport")
            .fail_for_test();
        host.poll_hyprland_follow(|_| panic!("must not open until release"));
        assert!(
            host.live_drag
                .as_ref()
                .expect("custody")
                .hyprland
                .as_ref()
                .expect("transport")
                .retired
        );
        assert!(host.windows[1].window.is_none());
        assert!(host.windows[1].sessions.owns_session(tab));
        host.live_drag
            .as_mut()
            .expect("custody")
            .hyprland
            .as_mut()
            .expect("transport")
            .release(None);
        host.poll_hyprland_follow(|_| Ok(()));
        assert!(host.live_drag.is_none());
        assert_eq!(host.windows.len(), 1);
        assert_ne!(host.windows[0].process_window_id(), source);
        assert!(host.windows[0].sessions.owns_session(tab));
    }
    #[test]
    fn hyprland_follow_replacement_failure_restores_the_original_source() {
        let (mut host, tab) = host();
        let source = host.windows[0].process_window_id();
        host.live_drag
            .as_mut()
            .expect("custody")
            .hyprland
            .as_mut()
            .expect("transport")
            .release(None);
        host.poll_hyprland_follow(|_| Err("injected creation failure".to_owned()));
        assert!(host.live_drag.is_none());
        assert_eq!(host.windows.len(), 1);
        assert_eq!(host.windows[0].process_window_id(), source);
        assert!(host.windows[0].sessions.owns_session(tab));
    }
    #[test]
    fn hyprland_follow_uses_process_focus_handover_and_never_reopens_outside_process_focus() {
        let (mut host, _) = host();
        host.route_live_tab_event(0, &WindowEvent::Focused(false));
        host.route_live_tab_event(1, &WindowEvent::Focused(true));
        host.tick_live_tab(Instant::now());
        assert!(host.live_drag.is_some());
        host.route_live_tab_event(1, &WindowEvent::Focused(false));
        host.live_drag
            .as_mut()
            .expect("custody")
            .hyprland
            .as_mut()
            .expect("transport")
            .release(None);
        host.poll_hyprland_follow(|_| panic!("must not map a replacement outside process focus"));
        host.tick_live_tab(Instant::now() + Duration::from_millis(200));
        assert!(host.live_drag.is_none());
    }
}

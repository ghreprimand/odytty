// SPDX-License-Identifier: GPL-3.0-only
//! Process-wide broadcast input across sibling windows.
//!
//! Every window of the process shares one receiver set. After each event the
//! host shares the first window's set with every window (a new window adopts
//! it before its first input), drops receivers whose pane no window owns any
//! more (closed, or lost with its window), delivers queued input to the window
//! that owns each receiver, and repaints every window's labels when the set
//! changed. Delivery goes through the owner's read-only gate, so a receiver
//! moved by a window merge still receives exactly once, at its new owner.

use super::MultiWindowHost;

impl MultiWindowHost {
    pub(super) fn service_broadcast(&mut self) {
        let Some(first) = self.windows.first() else {
            return;
        };
        let shared = first.broadcast_handle();
        let peers = self.windows.len() > 1;
        for app in &mut self.windows {
            app.adopt_broadcast(std::sync::Arc::clone(&shared), peers);
        }
        let outbox = {
            let windows = &self.windows;
            let mut set = crate::native::lock_recover(&shared);
            set.retain_live(|token| windows.iter().any(|app| app.owns_session(token)));
            set.take_outbox()
        };
        for (token, payload) in outbox {
            for app in &mut self.windows {
                if app.deliver_broadcast_payload(token, &payload) {
                    break;
                }
            }
        }
        for app in &mut self.windows {
            app.sync_broadcast_labels();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{headless, host_of};
    use crate::core::Terminal;
    use crate::input::Modifiers;
    use crate::native::pty::PtyWriter;
    use crate::native::session::SessionToken;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use winit::keyboard::{Key as WinitKey, KeyCode, PhysicalKey};

    #[derive(Clone, Default)]
    struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("bytes").extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Split `app` with a recording pane and return its token and bytes.
    fn recorded_split(app: &mut crate::native::app::App) -> (SessionToken, Arc<Mutex<Vec<u8>>>) {
        let recorder = RecordingWriter::default();
        let bytes = recorder.0.clone();
        let writer: PtyWriter = Arc::new(Mutex::new(Box::new(recorder)));
        let before = app.active_tab_pane_tokens_for_test();
        let dimensions = crate::native::options::NativeOptions::default().initial_grid;
        let terminal = Arc::new(Mutex::new(Terminal::new(
            dimensions.columns,
            dimensions.rows,
        )));
        app.seed_headless_split_pane_for_test(true, terminal, writer, dimensions);
        let token = app
            .active_tab_pane_tokens_for_test()
            .into_iter()
            .find(|token| !before.contains(token))
            .expect("new pane");
        (token, bytes)
    }

    #[test]
    fn a_receiver_in_another_window_gets_input_once_and_leaves_with_its_window() {
        let origin = headless();
        let origin_focus = origin.active_session_token_for_test();
        let mut sibling = headless();
        // Headless windows number their panes from 0, so give the sibling a
        // pane whose token no other window holds.
        let _ = recorded_split(&mut sibling);
        let (receiver, received) = recorded_split(&mut sibling);
        assert!(!origin.owns_session(receiver));
        let mut host = host_of(vec![origin, sibling]);
        host.service_broadcast();

        host.windows[1].focus_session_token_for_test(receiver);
        host.windows[1].handle_palette_action_for_test("toggle-broadcast");
        host.service_broadcast();
        assert!(
            host.windows[0].is_broadcast_receiver(receiver),
            "one shared set"
        );
        assert_eq!(
            host.windows[0].broadcast_summary().hidden,
            1,
            "a receiver in another window is hidden"
        );

        host.windows[0].focus_session_token_for_test(origin_focus);
        host.windows[0].drive_raw_key_event_for_test(
            WinitKey::Character("q".into()),
            WinitKey::Character("q".into()),
            PhysicalKey::Code(KeyCode::KeyQ),
            Modifiers::NONE,
            crate::input::KeyEventType::Press,
        );
        assert!(
            received.lock().expect("bytes").is_empty(),
            "queued, not lost"
        );
        host.service_broadcast();
        assert_eq!(received.lock().expect("bytes").as_slice(), b"q");
        host.service_broadcast();
        assert_eq!(
            received.lock().expect("bytes").as_slice(),
            b"q",
            "delivered exactly once"
        );

        host.windows.remove(1);
        host.service_broadcast();
        assert!(
            !host.windows[0].broadcast_active(),
            "a receiver lost with its window is dropped"
        );
    }
}

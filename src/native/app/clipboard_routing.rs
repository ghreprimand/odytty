// SPDX-License-Identifier: GPL-3.0-only
//! Clipboard routing for the native app: the paste and copy shortcuts, and the
//! terminal-originated clipboard requests drained each frame.
//!
//! Clipboard failures stay deliberately non-fatal, and the OSC 52 read gate
//! keeps its existing default. Bodies moved unchanged from the parent module.

use super::*;

impl App {
    /// Test-only clipboard source for the production shortcut route. Keeping
    /// this beside the route avoids growing the oversized general App seam
    /// file and never touches the process clipboard.
    #[cfg(test)]
    pub(in crate::native) fn inject_paste_text_for_test(&mut self, text: &str) {
        self.clipboard.injected_clipboard_text = Some(text.to_owned());
        self.clipboard.read_text_calls = 0;
    }

    /// Paste clipboard text into the PTY if the platform clipboard is
    /// reachable. Clipboard failures are deliberately non-fatal: a terminal
    /// should keep running even when the compositor denies clipboard access.
    pub(super) fn handle_paste_shortcut(&mut self) {
        // Every clipboard text paste enters the shared pre-encoding policy.
        if let Some(text) = self.clipboard.read_text() {
            self.route_paste_text(PasteSource::Clipboard, text);
            return;
        }
        // No text: on a remote integrated tab, a clipboard IMAGE may be offered
        // for upload (F6-i7). Everywhere else this is a no-op, byte-identical to
        // the prior "no text, nothing to paste" behavior.
        self.try_begin_image_paste();
    }

    /// Copy the current visible selection to the clipboard. This is kept fully
    /// native-side: the selected text is derived from a snapshot copy and no
    /// terminal state is mutated.
    pub(super) fn handle_copy_shortcut(&mut self) {
        let Some(text) = self.current_selection_text() else {
            return;
        };
        let _ = self.clipboard.write_text(text.as_str());
    }

    pub(super) fn handle_terminal_clipboard_requests(&mut self) {
        // NF21-5: OSC 52 must be drained for EVERY session, not just the focused
        // one through `Deref`. A background tab (or, post-W1, a background
        // workspace's tab) that emitted an OSC 52 write would otherwise queue it
        // until switch-back and then silently replace the system clipboard —
        // minutes-stale, from a program the user is not looking at. Policy: a
        // WRITE authority requires the active session, a confirmed OS-focused
        // window, and the live `osc52_write` policy. A READ requires the same
        // active session AND OS-focused window (C41) plus the `osc52_read` gate,
        // so a background program -- or a foreground one in the active tab while
        // the window itself is unfocused -- cannot exfiltrate clipboard
        // contents. Every
        // session is drained each pass so nothing queues indefinitely and a
        // discarded request is never applied on switch-back.
        let focused = self.sessions.active_id();
        let mut writes = Vec::new();
        let live_sessions: Vec<_> = self.sessions.iter().map(|session| session.id).collect();
        for session in self.sessions.iter() {
            let is_focused = session.id == focused;
            let requests = session
                .terminal
                .lock()
                .map(|mut terminal| terminal.take_clipboard_requests())
                .unwrap_or_default();
            for request in requests {
                match request {
                    ClipboardRequest::Write { selection, text } => {
                        writes.push((session.id, selection, text));
                    }
                    ClipboardRequest::Read { selection } => {
                        // C41: a READ needs the active session AND a confirmed
                        // OS-focused window -- the same authority the WRITE path
                        // demands. Without the window-focus gate a foreground
                        // program in the active tab could read the clipboard
                        // while the user is working in another application.
                        let window_authority = self.focused && self.osc52_write.focus_observed();
                        if !is_focused || !window_authority {
                            // A denied requester must not learn clipboard
                            // contents, but an explicit empty reply lets it
                            // finish immediately instead of hanging to its own
                            // timeout.
                            let host_output = session
                                .terminal
                                .lock()
                                .map(|mut terminal| {
                                    terminal.answer_clipboard_read(selection, "");
                                    terminal.take_host_output()
                                })
                                .unwrap_or_default();
                            #[cfg(test)]
                            {
                                self.osc52_background_empty_replies_for_test += 1;
                            }
                            if !host_output.is_empty()
                                && let Ok(mut writer) = session.writer.lock()
                            {
                                let _ = writer.write_all(&host_output);
                                let _ = writer.flush();
                            }
                            continue;
                        }
                        if !self.settings.osc52_read {
                            continue;
                        }
                        let Some(text) = read_clipboard_selection(&mut self.clipboard, selection)
                        else {
                            continue;
                        };
                        let host_output = session
                            .terminal
                            .lock()
                            .map(|mut terminal| {
                                terminal.answer_clipboard_read(selection, &text);
                                terminal.take_host_output()
                            })
                            .unwrap_or_default();
                        if !host_output.is_empty()
                            && let Ok(mut writer) = session.writer.lock()
                        {
                            let _ = writer.write_all(&host_output);
                            let _ = writer.flush();
                        }
                    }
                }
            }
        }
        self.prune_osc52_session_state(&live_sessions);
        let now = Instant::now();
        for (session, selection, text) in writes {
            self.handle_osc52_write(session, selection, text, now);
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-only
//! App ownership for suspicious text-paste confirmation.
//!
//! Every present and reserved text source enters through `route_paste_text`.
//! Safe text, the advanced opt-out, and child-enabled bracketed paste call the
//! historical encoder directly. Risky plain text is held transiently until the
//! modal returns an explicit action.

use super::*;

impl App {
    pub(super) fn route_paste_text(&mut self, source: PasteSource, text: String) {
        if self.refuse_input_if_read_only() {
            return;
        }
        if self.broadcast_active() {
            self.route_broadcast_paste(source, text);
            return;
        }
        let bracketed = crate::native::lock_recover(&self.terminal).bracketed_paste_enabled();
        if bracketed || !self.settings.warn_on_risky_paste {
            self.return_to_live();
            self.deliver_paste_text(&text);
            return;
        }
        let assessment = assess(&text);
        if !assessment.risky {
            self.return_to_live();
            self.deliver_paste_text(&text);
            return;
        }

        self.cancel_pending_text_paste();
        let session = self.sessions.active_id();
        self.pending_text_paste = Some(PendingTextPaste {
            session,
            source,
            text,
            bracketed: false,
            file_shell: None,
            broadcast: false,
        });
        self.reset_pointer_state_for_overlay();
        self.overlay.open_risky_paste(RiskyPasteDialog {
            line_count: assessment.line_count,
            byte_count: assessment.byte_count,
            escaped_preview: assessment.escaped_preview,
            preview_truncated: assessment.preview_truncated,
            one_line_available: assessment.one_line_available,
            broadcast: None,
        });
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Reserved entry point for external text-drop events. Platforms that add
    /// such an event must call this method rather than writing the PTY directly.
    #[allow(dead_code)]
    pub(super) fn route_external_text_drop(&mut self, text: String) {
        self.route_paste_text(PasteSource::ExternalTextDrop, text);
    }

    /// Reserved entry point for a future authorized local automation request.
    /// Authorization is outside this policy; confirmed text still uses it.
    #[allow(dead_code)]
    pub(super) fn route_automation_paste(&mut self, text: String) {
        self.route_paste_text(PasteSource::Automation, text);
    }

    /// A pending paste or file-drop batch is valid only while its preview
    /// dialog is showing. Every overlay opener replaces the current dialog
    /// through `Overlay::close`, so any opener that does not cancel explicitly
    /// leaves stale pending state behind. Reconcile at the shared event entry
    /// and at the next drop so no later event can extend or commit it.
    pub(super) fn reconcile_displaced_pending_paste(&mut self) {
        if !self.overlay.is_risky_paste() {
            if self.pending_text_paste.is_some() {
                self.cancel_pending_text_paste();
            } else if self
                .pending_file_drop
                .as_ref()
                .is_some_and(|(_, batch)| !batch.is_rejected())
            {
                self.cancel_file_drop();
            }
        }
    }

    pub(super) fn cancel_pending_text_paste(&mut self) {
        self.cancel_file_drop();
        self.pending_text_paste = None;
        if self.overlay.is_risky_paste() {
            self.overlay.close();
        }
    }

    pub(super) fn commit_pending_text_paste(&mut self, one_line: bool) {
        self.cancel_file_drop();
        let Some(pending) = self.pending_text_paste.take() else {
            self.overlay.close();
            return;
        };
        self.overlay.close();

        // Confirmation authority is tied to the exact pane and the child mode
        // observed when the modal opened. A switch or a newly-enabled bracketed
        // mode makes the prompt stale and writes nothing.
        if self.sessions.active_id() != pending.session || !self.pane_accepts_input(pending.session)
        {
            return;
        }
        let same_mode = crate::native::lock_recover(&self.terminal).bracketed_paste_enabled()
            == pending.bracketed;
        if !same_mode {
            return;
        }

        if let Some(shell) = pending.file_shell
            && (one_line || !self.focused || self.file_drop_shell() != Ok(shell))
        {
            return;
        }

        let text = if one_line {
            let Some(encoded) = lossless_one_line(&pending.text) else {
                return;
            };
            encoded
        } else {
            pending.text
        };
        let _source = pending.source;
        self.return_to_live();
        if pending.broadcast {
            self.broadcast_paste(&text);
        }
        self.deliver_paste_text(&text);
    }

    /// Paste while broadcast is on. Text with a line break always opens the
    /// confirmation, which names the receiver, hidden, and remote counts; a
    /// single line goes out at once unless the single-pane policy would have
    /// asked anyway. Cancel sends nothing to any pane, the focused pane
    /// included. Each receiver encodes the text for its own terminal.
    fn route_broadcast_paste(&mut self, source: PasteSource, text: String) {
        let bracketed = crate::native::lock_recover(&self.terminal).bracketed_paste_enabled();
        let assessment = assess(&text);
        let line_break = text.contains(['\n', '\r']);
        let single_pane_would_ask =
            !bracketed && self.settings.warn_on_risky_paste && assessment.risky;
        if !line_break && !single_pane_would_ask {
            self.return_to_live();
            self.broadcast_paste(&text);
            self.deliver_paste_text(&text);
            return;
        }
        self.cancel_pending_text_paste();
        let session = self.sessions.active_id();
        self.pending_text_paste = Some(PendingTextPaste {
            session,
            source,
            text,
            bracketed,
            file_shell: None,
            broadcast: true,
        });
        self.reset_pointer_state_for_overlay();
        self.overlay.open_risky_paste(RiskyPasteDialog {
            line_count: assessment.line_count,
            byte_count: assessment.byte_count,
            escaped_preview: assessment.escaped_preview,
            preview_truncated: assessment.preview_truncated,
            one_line_available: assessment.one_line_available,
            broadcast: Some(self.broadcast_summary()),
        });
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Encode and write one paste through the shared encoder, and show a
    /// failure notice when it is refused or not delivered, so a paste never
    /// vanishes silently.
    fn deliver_paste_text(&mut self, text: &str) {
        match write_paste_text(&self.terminal, &self.writer, text) {
            Ok(()) => self.surface_input_loss(),
            Err(PasteError::TooLarge { len, max }) => self.raise_open_notice(format!(
                "Paste refused: {} exceeds the {} bracketed paste limit",
                format_byte_size(len),
                format_byte_size(max),
            )),
            Err(PasteError::Write(error)) => {
                tracing::warn!("paste not delivered: {error}");
                self.raise_open_notice(
                    "Paste not delivered: the session is not accepting input".to_owned(),
                );
            }
        }
    }

    /// File paths use the same transient confirmation and encoder authority.
    /// Their quoted tokens must not be transformed by Paste as One Line.
    pub(super) fn hold_file_paste(
        &mut self,
        text: String,
        shell: crate::shell_integration::ShellKind,
    ) {
        self.cancel_pending_text_paste();
        let assessment = assess(&text);
        let bracketed = crate::native::lock_recover(&self.terminal).bracketed_paste_enabled();
        self.pending_text_paste = Some(PendingTextPaste {
            session: self.sessions.active_id(),
            source: PasteSource::ExternalTextDrop,
            text,
            bracketed,
            file_shell: Some(shell),
            broadcast: false,
        });
        self.reset_pointer_state_for_overlay();
        self.overlay.open_risky_paste(RiskyPasteDialog {
            line_count: assessment.line_count,
            byte_count: assessment.byte_count,
            escaped_preview: assessment.escaped_preview,
            preview_truncated: assessment.preview_truncated,
            one_line_available: false,
            broadcast: None,
        });
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    #[cfg(test)]
    pub(in crate::native) fn set_warn_on_risky_paste_for_test(&mut self, enabled: bool) {
        self.settings.warn_on_risky_paste = enabled;
    }

    #[cfg(test)]
    pub(in crate::native) fn risky_paste_pending_for_test(&self) -> bool {
        self.pending_text_paste.is_some() && self.overlay.is_risky_paste()
    }

    #[cfg(test)]
    pub(in crate::native) fn confirm_risky_paste_for_test(&mut self, one_line: bool) {
        self.commit_pending_text_paste(one_line);
    }

    #[cfg(test)]
    pub(in crate::native) fn cancel_risky_paste_for_test(&mut self) {
        self.cancel_pending_text_paste();
    }

    #[cfg(test)]
    pub(in crate::native) fn route_external_text_drop_for_test(&mut self, text: &str) {
        self.route_external_text_drop(text.to_owned());
    }

    #[cfg(test)]
    pub(in crate::native) fn route_automation_paste_for_test(&mut self, text: &str) {
        self.route_automation_paste(text.to_owned());
    }

    #[cfg(test)]
    pub(in crate::native) fn route_context_menu_paste_for_test(&mut self) {
        self.apply_overlay_outcome(OverlayOutcome::ContextMenuPaste);
    }
}

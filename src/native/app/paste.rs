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
        let bracketed = self
            .terminal
            .lock()
            .map(|terminal| terminal.bracketed_paste_enabled())
            .unwrap_or(false);
        if bracketed || !self.settings.warn_on_risky_paste {
            self.return_to_live();
            let _ = write_paste_text(&self.terminal, &self.writer, &text);
            return;
        }
        let assessment = assess(&text);
        if !assessment.risky {
            self.return_to_live();
            let _ = write_paste_text(&self.terminal, &self.writer, &text);
            return;
        }

        self.cancel_pending_text_paste();
        let session = self.sessions.active_id();
        self.pending_text_paste = Some(PendingTextPaste {
            session,
            source,
            text,
        });
        self.reset_pointer_state_for_overlay();
        self.overlay.open_risky_paste(RiskyPasteDialog {
            line_count: assessment.line_count,
            byte_count: assessment.byte_count,
            escaped_preview: assessment.escaped_preview,
            preview_truncated: assessment.preview_truncated,
            one_line_available: assessment.one_line_available,
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

    pub(super) fn cancel_pending_text_paste(&mut self) {
        self.pending_text_paste = None;
        if self.overlay.is_risky_paste() {
            self.overlay.close();
        }
    }

    pub(super) fn commit_pending_text_paste(&mut self, one_line: bool) {
        let Some(pending) = self.pending_text_paste.take() else {
            self.overlay.close();
            return;
        };
        self.overlay.close();

        // Confirmation authority is tied to the exact pane and the child mode
        // observed when the modal opened. A switch or a newly-enabled bracketed
        // mode makes the prompt stale and writes nothing.
        if self.sessions.active_id() != pending.session {
            return;
        }
        let still_plain = self
            .terminal
            .lock()
            .map(|terminal| !terminal.bracketed_paste_enabled())
            .unwrap_or(false);
        if !still_plain {
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
        let _ = write_paste_text(&self.terminal, &self.writer, &text);
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
